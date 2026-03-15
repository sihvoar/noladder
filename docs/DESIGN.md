# NoLadder Architecture

## Why This Exists

Industrial control programming has not meaningfully evolved since the 1970s.
Ladder logic was designed to help electricians transition from relay panels.
Structured Text is Pascal with worse tooling.
Both force modern software engineers into an alien paradigm that offers
no advantages over contemporary programming practices.

NoLadder is a modern industrial control runtime for Linux IPCs.
It is opinionated. It will not try to be compatible with IEC 61131.
It is for people who can program.

---

## Core Principles

- **No dynamic allocation after init** - everything pre-allocated at startup
- **No blocking in control loop** - ever
- **No strings at runtime** - resolved to indices at startup
- **No hardware knowledge in control logic** - ever
- **No proprietary anything** - TOML config, Rust, open protocols

---

## Architecture Overview
```
┌─────────────────────────────────────────┐
│           Control Logic                 │
│         (your Rust code)                │
│                                         │
│  rung!(conveyor, |ctx| {                │
│      ctx.wait_for(home_sensor).await;   │
│  });                                    │
└─────────────────┬───────────────────────┘
                  │ read/write by index
                  ▼
┌─────────────────────────────────────────┐
│            IO Image                     │
│      (shared memory, locked)            │
│                                         │
│   inputs:  [Value; 4096]                │
│   outputs: [Value; 4096]                │
└─────────────────┬───────────────────────┘
                  │ normalized Values
                  ▼
┌─────────────────────────────────────────┐
│            Bus Server                   │
│      (independent process)              │
│                                         │
│   EtherCAT  │  Profinet  │  Modbus      │
│   1ms       │  4ms       │  10ms        │
└─────────────────────────────────────────┘
```

---

## Memory Model

### RT Side - No Surprises Ever

All memory allocated at startup, never during operation:
```rust
// Boot time only
let io_image     = IOImage::allocate();    // fixed, locked with mlockall
let rung_arena   = Arena::new(MAX_RUNGS);  // one slot per rung, fixed
let device_arena = Arena::new(MAX_DEVICES);// one slot per device, fixed
let mailbox      = Mailbox::new();         // fixed ring buffer
```

### OS Side - Normal Rules Apply

OS request handling uses normal stack allocation.
Short lived, throwaway, no RT constraints.

---

## The IO Image

The central data structure. Shared memory between bus server and
control loop. Never accessed with locks - sequence counter pattern:
```rust
struct IOImage {
    sequence: AtomicU32,
    inputs:   [Value; MAX_IO],
    outputs:  [Value; MAX_IO],
}

enum Value {
    Bool(bool),
    Int(i32),
    Float(f32),
}
```

### Why Only Numbers

Every device on every bus ultimately produces and consumes numbers.
Strings, structs, enums belong in control logic, not on the wire.
The mapping between rich types and wire values is explicit and
owned by the control engineer.

---

## The Cycle

One isolated CPU core. One loop. No surprises:
```
┌─────────────────────────────────────────┐
│  loop {                                 │
│      cycle_timer.wait();  // 1ms        │
│                                         │
│      io.snapshot_inputs();              │
│                                         │
│      for rung in rungs {                │
│          rung.poll();    // never blocks│
│      }                                  │
│                                         │
│      io.flush_outputs();                │
│      mailbox.check_os();                │
│                                         │
│      if overrun { log + safe_state }    │
│  }                                      │
└─────────────────────────────────────────┘
```

### Cycle Failure

A cycle that does not complete within its deadline is a hard failure.
Consecutive failures trigger safe state. This is not configurable.
A system that cannot meet its timing guarantees is not safe.

---

## Rungs

The fundamental unit of control logic. A rung is a Rust async function
that runs inside the cycle executor. It can suspend across cycles
without blocking the loop:
```rust
rung!(homing_sequence, |ctx| {
    // request something from OS side
    ctx.os_request("recipe.load", machine_id);

    // suspend - comes back next cycle(s) when OS delivers
    ctx.yield_until("recipe.ready").await;

    // rich types in logic - numbers only at boundary
    let recipe = Recipe::from_io(&ctx.inputs);
    motor.apply(recipe);

    // wait for physical condition
    ctx.yield_until(home_sensor).await;

    ctx.outputs.write(motor_enable, true);
});
```

### What A Rung Cannot Do

- Block
- Allocate
- Access hardware directly
- Know what bus its devices are on
- Perform IO outside the image

The compiler enforces most of this.

---

## Device Addressing

Devices are named in config, resolved to indices at startup.
After init, only indices exist in RT code:
```rust
// Init time - string resolution, cost irrelevant
let motor_speed  = bus.resolve("line1.conveyor.motor.speed");
let home_sensor  = bus.resolve("line1.conveyor.sensor.home");

// Runtime - pure array indexing
let speed = io.read(motor_speed);
io.write(motor_enable, true);
```

---

## Configuration

Machine topology defined in TOML. This file IS the documentation
of what hardware is connected and where:
```toml
[bus.ethercat0]
interface = "eth1"
cycle_ms  = 1

[bus.modbus0]
interface = "eth2"
cycle_ms  = 10
# legacy hardware - do not use for time critical logic

[device.line1.conveyor.motor]
bus  = "ethercat0"
node = 3
type = "servo_drive"

[device.line1.conveyor.sensor.home]
bus  = "ethercat0"
node = 4
type = "digital_in"
```

Config is validated at startup. Invalid config is a hard failure.
A config validator tool is provided for pre-deployment checking.

---

## Bus Drivers

Each protocol is a plugin implementing BusDriver:
```rust
trait BusDriver {
    fn init(config: &BusConfig)  -> Result<Self>;
    fn read_inputs(&mut self)    -> Result<BusFrame>;
    fn write_outputs(&mut self,
        frame: BusFrame)         -> Result<()>;
    fn cycle_ms(&self)           -> u32;
}
```

### V0.1 Supports

- Modbus TCP/RTU - because everyone has it

### Planned

- EtherCAT - via IgH open source master
- Profinet - when someone needs it enough to write it

---

## OS Communication

Control logic can request services from the Linux side
asynchronously. The RT loop never waits for OS responses:
```rust
// Control side - fire and forget
ctx.os_request("recipe.load", id);
ctx.yield_until("recipe.ready").await;  // suspends, not blocks

// OS side - normal Linux process, no RT constraints
// reads request from mailbox
// does whatever it needs (DB, file, REST call)
// writes response back to mailbox
```

### OS Server Is Also A Plugin

File IO, database, REST, MQTT - all just implementations
of the same simple interface. Ships with basic file IO.
Everything else is community plugins.

---

## What NoLadder Is Not

- Not IEC 61131 compatible - by design
- Not a SCADA system
- Not trying to replace TwinCAT on its own turf - yet
- Not safe for SIL applications - yet
- Not production ready - yet

---

## Status (V0.1)

**Complete and working:**
- Core IO image with lockless sequence counter
- RT control loop (1ms, configurable)
- Rung coroutine model with suspend/resume
- Modbus TCP/RTU driver (production ready)
- EtherCAT driver (alpha, feature-gated)
- TOML config loader with validation
- Config validator tool
- Two complete examples (hello_world, basic_io)
- 78 unit tests passing
- CiA402 servo state machine

**Ready for:**
- Development and testing
- Small-scale production (Modbus)
- Hardware vendor integration

**Not ready for:**
- SIL-rated safety applications (yet)
- Deployments without testing (evaluate on your hardware)

**Call to action:**
- Test on real hardware (especially EtherCAT)
- Contribute bus drivers (CAN, Profinet, GigE Vision)
- Report issues and feedback on GitHub


