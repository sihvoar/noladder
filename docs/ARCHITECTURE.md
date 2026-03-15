# NoLadder - Looking For Co-Developers

## The Problem

Industrial control software has not meaningfully evolved since the 1970s.
Ladder logic was designed to help electricians transition from relay panels.
Structured Text is Pascal with worse tooling and no debugger worth using.
IEC 61131 was standardized in 1993 and has barely changed since.

Meanwhile the hardware has completely transformed.
Modern industrial IPCs are multicore ARM or x86 machines running Linux.
EtherCAT gives you microsecond synchronization over standard Ethernet.
Every servo drive on the planet speaks CiA402.
Cameras, CAN bus, MQTT, cloud connectivity - all standard.

The hardware is ready for a better programming model.
The tooling is thirty years behind.

We are fixing that.

---

## What NoLadder Is

NoLadder is an open source industrial control runtime for Linux IPCs.
Written in Rust. No proprietary runtime. No ladder logic. No ST.

Your control logic is Rust async functions.
Your hardware is described in a TOML file.
Your buses run as independent Linux processes.
The RT core is a single isolated CPU core with a 1ms cycle.

That is the entire architecture.
Everything else is libraries.

---

## Why Now

Several things have converged:

**Rust is ready** - memory safety, zero cost abstractions,
async/await, no GC. Everything you need for hard RT code
and nothing you don't.

**Hardware is ready** - cheap ARM IPCs with 4+ cores,
EtherCAT on standard Ethernet, open source bus stacks,
Linux PREEMPT_RT merged into mainline kernel.

**The market is ready** - ROS2 is frustrating robotics
engineers. CODESYS is expensive and proprietary.
TwinCAT is Windows. PLCnext is promising but vendor locked.
There is a genuine gap for an open, modern, Rust-native
industrial control framework.

**The frustration is real** - we have both taken PLC
courses, looked at ST and ladder, and concluded that
software engineers deserve better tools.

---

## Architecture

### The Core Insight

A Linux IPC is not a microcontroller.
You do not need an RTOS.
You need RT discipline applied to a small part of Linux.
```
Core 0: Linux - OS, networking, databases, MQTT, everything normal
Core 1: NoLadder RT loop - nothing else, ever
```

One isolated core. One 1ms loop. No surprises.
Linux handles everything else with full OS capabilities.
No reinventing networking. No reinventing filesystems.
No reinventing anything Linux already does well.

### The Three Layers
```
┌─────────────────────────────────────────────────┐
│              Control Logic                      │
│           (user Rust async code)                │
│                                                 │
│  rung!(homing, {                                │
│      ctx.write(enable, true);                   │
│      ctx.yield_until(sensor, true).await;       │
│      ctx.write(homed, true);                    │
│  });                                            │
└──────────────────┬──────────────────────────────┘
                   │ read/write by index
                   │ never by name at runtime
                   ▼
┌─────────────────────────────────────────────────┐
│                IO Image                         │
│         (shared memory, mlockall'd)             │
│                                                 │
│   inputs:  [Value; 4096]   ← frozen each cycle │
│   outputs: [Value; 4096]   ← flushed each cycle│
│   sequence: AtomicU32      ← lockless sync      │
└──────────────────┬──────────────────────────────┘
                   │ normalized Values
                   │ bus server owns hardware
                   ▼
┌─────────────────────────────────────────────────┐
│              Bus Server                         │
│         (independent Linux process)             │
│                                                 │
│  EtherCAT    Modbus    CAN    MQTT    Camera    │
│  1ms         10ms      1ms    async   33ms      │
└─────────────────────────────────────────────────┘
```

### The Memory Model

**No dynamic allocation after startup. Ever.**

Everything pre-allocated at init using arena allocators.
Memory locked with `mlockall` - no page faults mid-cycle.
After startup the RT core never touches the allocator.
```rust
// boot time only - cost irrelevant
let io_image   = IOImage::allocate();    // locked, fixed
let rung_arena = Arena::new();           // fixed capacity
let mailbox    = Mailbox::new();         // fixed ring buffer

// after this - no allocation in RT path
// compiler enforces it
// if you try - it won't compile
```

The IO image is shared memory between bus server and
control loop. Synchronized via a sequence counter -
no locks, no syscalls, no blocking. Ever.
```rust
// bus server side - after writing inputs
image.sequence.fetch_add(1, Ordering::Release);

// control loop side - each cycle
if image.is_fresh(last_seq) {
    image.snapshot();  // freeze for this cycle
}
```

Everything the RT core touches is a flat array of Values:
```rust
enum Value {
    Bool(bool),
    Int(i32),
    Float(f32),
    Unset,
}
```

That is the entire wire protocol between bus and logic.
Every device on every bus ultimately produces
and consumes numbers. Booleans are 1-bit integers.
Strings belong on the OS side.

### The Programming Model

A rung is a Rust async function that runs inside
the cycle executor. It can suspend across cycles
without blocking the loop.
```rust
// this spans as many cycles as needed
// but never blocks the 1ms loop
rung!(pick_and_place, {

    // wait for part present
    ctx.yield_until(part_sensor, true).await;

    // trigger vision - result arrives async
    ctx.write(camera_trigger, true);

    // race vision result against timeout
    let result = ctx.race(
        ctx.yield_until(vision_ready, true),
        ctx.yield_ms(200),
    ).await;

    match result {
        RaceResult::First => {
            let offset_x = ctx.read_float(vision_x);
            let offset_y = ctx.read_float(vision_y);
            ctx.write(robot_offset_x, offset_x);
            ctx.write(robot_offset_y, offset_y);
            ctx.write(pick_enable, true);
        }
        RaceResult::Second => {
            ctx.write(vision_fault, true);
        }
    }
});
```

Compare this to the IEC 61131 equivalent -
a state machine with eight flags, nested CASE statements,
and a programmer who has lost track of what state they're in.

The yield primitives cover everything you need:
```rust
ctx.yield_until(index, value)         // wait for condition
ctx.yield_until_any(&[(idx, val)..])  // wait for any
ctx.yield_until_all(&[(idx, val)..])  // wait for all
ctx.yield_ms(n)                       // wait for time
ctx.race(future_a, future_b)          // first wins
ctx.os_request("key")                 // async OS call
```

These compose freely. Race a condition against a timeout.
Wait for all axes to home. React to estop or start,
whichever comes first. All in readable sequential code.

### OS Side - Just Linux

Anything that is not hard RT runs as a normal Linux process.
No special treatment. No RT constraints.
```
Recipe loading      - read from database, write to mailbox
MQTT publishing     - rumqttc, normal async Rust
Vision processing   - unix socket to any vision SW
Alarm logging       - write to filesystem
Remote monitoring   - REST API, normal Axum server
Cloud telemetry     - whatever you want
```

The mailbox is a fixed ring buffer in shared memory.
Control loop posts requests. OS server delivers responses.
Control loop polls each cycle - never blocks.
```rust
// control side - fire and forget
ctx.os_request("recipe.load").await;
// suspended - not blocked
// resumes when OS delivers

// OS side - normal Linux process
// reads request, loads from DB, posts response
// completely decoupled from RT world
```

This means the OS side can use any Rust crate,
any async runtime, any library.
Tokio, async-std, threads - whatever fits the use case.
NoLadder does not care.

---

## Bus Driver Architecture

A bus driver is a single trait implementation.
One file. Nothing else required.
```rust
pub trait BusDriver {
    fn init(config: &BusConfig) -> Result<Self>;
    fn read_inputs(&mut self)   -> Result<Vec<Value>>;
    fn write_outputs(
        &mut self,
        values: Vec<Value>
    )                           -> Result<()>;
    fn cycle_ms(&self)          -> u32;
}
```

The driver runs on its own thread at its own cycle rate.
It reads from hardware, normalizes to Values,
writes to the IO image. That is its entire job.

The control loop never knows what protocol
a device uses. Swap EtherCAT for Profinet -
control code does not change.

### Drivers - Current (V0.1)

**Modbus TCP/RTU** ✅ Production ready
- The cockroach of industrial protocols
- Will outlive all of us
- Ships with software slave for testing (run without hardware)
- Fully tested via modbus crate

**EtherCAT** 🟡 Alpha
- Via IgH open source master
- CiA402 servo state machine implemented and tested
- Needs real hardware validation (Beckhoff, Yaskawa, Bosch, Panasonic, Mitsubishi)
- This single driver unlocks every CiA402 servo on market
- Feature-gated: `cargo build --features ethercat`

### Drivers - Planned

**CAN / CANopen** - High priority for v0.2
- `socketcan` crate production ready
- CiA402 profiles well documented
- Enormous installed base: battery systems, robots, automotive

**MQTT Bus Driver** - Medium priority for v0.3
- Bridges async MQTT to IO image
- Topics become inputs, outputs publish to topics
- IoT and building automation use case

**GigE Vision / V4L2** - Medium priority for v0.3
- Camera as a bus driver
- Frame in shared memory (zero copy)
- Vision SW connects via unix socket
- Works with any vision library, any language

**Profinet** - When needed
- Large installed base in European manufacturing
- Complex protocol - needs domain expert
- Q2 2026 if community interest

**OPC-UA** - When needed
- Standard for industrial data exchange
- `opcua` Rust crate exists

Writing a new driver:
- Implement `BusDriver` trait
- Add device types to `config/loader.rs`
- Add detection in `bus/mod.rs`
- Write one example
- Open a PR

The core never changes.
The driver is completely isolated.

---

## Library Ecosystem

Rungs are just Rust async functions.
Libraries are just crates that return rungs.
```rust
// user code with libraries
use noladder_servo::*;
use noladder_safety::*;
use noladder_vision::*;

fn register_rungs(
    arena: &mut Arena,
    map:   &DeviceMap,
) -> Result<()> {

    let axis   = ServoHandles::from_map(&map, "robot.axis1");
    let estop  = map.input("cabinet.estop");
    let camera = CameraHandles::from_map(&map, "station1");

    // safety first
    arena.add(estop_monitor(
        estop,
        &axis.all_outputs()
    ));

    // motion
    arena.add(homing_sequence(
        axis,
        HomingParams::default()
    ));

    // vision
    arena.add(vision_inspection(
        camera,
        InspectionParams::default()
    ));

    Ok(())
}
```

That is a complete robot station controller.
The domain knowledge lives in the libraries.
The user wires it together.

Libraries we plan:
```
noladder_motion   - PID, trapezoidal profiles, homing
noladder_servo    - CiA402, ServoHandles, move_to rung
noladder_safety   - estop, two-hand control, safety doors
noladder_vision   - camera trigger, result handling
noladder_robot    - kinematics, joint control
noladder_conveyor - belt control, divert, jam detection
noladder_hvac     - temperature loops, valve sequences
noladder_batch    - recipe management, sequence logging
```

Each lives on crates.io.
Each follows the same pattern.
Anyone can publish one.

---

## Target Applications

NoLadder sits at the boundary between the
physical world and the digital world:
```
fast physical world        slow digital world
───────────────────        ──────────────────
sensors, actuators    ←→   databases, APIs
deterministic timing       best effort timing
numbers                    rich data structures
EtherCAT, CAN, Modbus      MQTT, REST, SQL
```

**Robotics** - primary target
Cobot arms, delta robots, SCARA, gantries.
EtherCAT servos, force sensors, cameras.
NoLadder replaces the ROS2 hardware abstraction layer
with something deterministic and simple.
ROS2 still handles planning and perception above it.

**Machine building**
Conveyor systems, packaging lines, CNC coordination.
The bread and butter of industrial automation.
Exactly the use case NoLadder was designed for.

**Energy systems**
Solar inverters, battery storage, EV charging.
Modbus everywhere. MQTT to cloud.
Fast response loops with slow supervisory control.
Perfect fit.

**Building automation**
HVAC, elevators, access control.
Modbus sensors, BACnet gateway via OS side.
Recipe driven setpoints from building management system.

**Machine vision stations**
Any inspection, measurement or guidance application.
Camera bus driver with unix socket to any vision SW.
OpenCV, Halcon, custom ML model - all the same interface.

---

## What We Are Looking For

### Bus Driver Authors
If you have hardware we do not support yet -
EtherCAT, CAN, Profinet, GigE Vision -
and you want to write the driver, we want to talk.
The trait is small. The isolation is complete.
You will not need to understand the rest of the codebase.

### Robotics Engineers
If you are fighting ROS2 for deterministic servo control
and you want a cleaner alternative for the RT layer -
the CiA402 driver is working and needs validation
on real hardware. Beckhoff, Yaskawa, any CiA402 drive.

### Library Authors
If you have domain knowledge in motion control,
safety systems, vision, HVAC, batch processing -
and you want to package it as a noladder library crate -
the pattern is simple and the need is real.

### Rust Embedded Engineers
The async executor and arena allocator are interesting
problems. If you enjoy RT systems programming in Rust
and want something more purposeful than benchmarks -
the core has interesting problems left to solve.

### Industrial Automation Engineers
If you have worked with TwinCAT, CODESYS, or PLCnext
and have opinions about what the programming model
should look like - we need your domain knowledge.
The Rust engineers can write the code.
We need people who know what the code needs to do.

---

## Current Status (v0.1)

**Fully Working:**
- ✅ RT cycle loop (1ms, configurable)
- ✅ IO image with lockless sequence counter sync
- ✅ Rung coroutine executor with suspend/resume
- ✅ All yield primitives - until, any, all, race, ms, cycles
- ✅ Arena allocator - fixed, pre-allocated, no malloc
- ✅ Modbus TCP/RTU driver (production ready)
- ✅ TOML config loader with full validation
- ✅ CiA402 servo state machine (code complete)
- ✅ Config validator binary tool
- ✅ Software Modbus slave for testing (no hardware needed)
- ✅ OS request/response mailbox bridge
- ✅ 78 unit tests passing

**Alpha (needs validation):**
- 🟡 EtherCAT driver via IgH master (compiles, needs real hardware)

**Planned for v0.2:**
- CAN / CANopen driver
- Hot reload of control logic
- Cycle statistics dashboard
- Hardware watchdog integration

**Planned for v0.3:**
- Remote monitoring over MQTT
- Structured logging to InfluxDB
- GigE Vision / V4L2 camera bus driver

**Future (v1.0):**
- IEC 62443 security baseline
- SIL2 pathway investigation
- Production hardened certification

---

## The Stack
```toml
[dependencies]
# config
serde     = { version = "1", features = ["derive"] }
toml      = "0.8"

# async executor for rungs
tokio     = { version = "1", features = ["rt", "macros"] }

# logging
tracing   = "0.1"

# error handling
anyhow    = "1"
thiserror = "1"

# modbus
tokio-modbus = "0.9"

# lock-free data structures
crossbeam = "0.8"

# linux RT (optional - dev without it)
libc      = "0.2"
```

Minimal dependencies.
No magic. No frameworks within frameworks.
The codebase is small enough to read in an afternoon.

---

## Get Involved
```
GitHub:     github.com/sihvoar/noladder
Issues:     bug reports, hardware compatibility, feature requests
Discussions: architecture, use cases, integration questions
```

If you want to contribute:

1. Read `docs/DESIGN.md` - architecture decisions and rationale
2. Run `cargo run --example basic_io` - works without hardware
3. Pick something from the roadmap that matches your skills
4. Open an issue to discuss before writing code

If you just want to follow along:
star the repo - it helps others find it.

If you work for a hardware vendor and want
your devices supported:
open an issue - bus drivers are isolated,
your engineers can write one without touching the core.

---

## Why This Matters

Industrial automation runs everything.
The factory that makes your phone.
The conveyor that delivers your package.
The robot that welds your car.
The HVAC system in the building you are sitting in.

All of it running on software that was designed
when the best programming language available
was Pascal.

Engineers who could build better tools
have been looking at ladder logic and ST
and concluding that life is too short.

NoLadder exists because the tools should be
as good as the engineers using them.

If you agree - come build it with us.
