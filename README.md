```
╔════════════════════════════════════════════════════════╗
║                   NoLadder                             ║
║    Industrial control runtime for Linux IPCs           ║
║                 Written in Rust                        ║
╚════════════════════════════════════════════════════════╝
```


> Industrial control runtime for Linux IPCs.
> For people who can program.

**Status:** early development — suitable for experimentation and testing.

Documentation:

* User guide → docs/UserGuide.md
* Architecture → docs/ARCHITECTURE.md
* Design notes → docs/DESIGN.md

---

You took the Beckhoff course.
You looked at Structured Text.
You wondered why control software still looks like 1993.

There might be a better way.

---

## What It Is

NoLadder is an open source industrial control runtime written in Rust.
It runs on any Linux IPC with a standard Ethernet port.

It replaces proprietary PLC runtimes with a clean, modern architecture that software engineers can actually work with.

Your control logic is Rust.
Your IO is numbers.
Your hardware is described in a TOML file.
The runtime handles the rest.

---

## Try it in 60 seconds

Install the Rust toolchain (1.75 or newer):

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

No hardware required. This runs a simulated Modbus motor.

```bash
git clone https://github.com/sihvoar/noladder
cd noladder

# terminal 1
cargo run --example hello_world_bus

# terminal 2
cargo run --example hello_world_os

# terminal 3
cargo run --example hello_world
```

---

## Screenshot

The runtime exposes its IO image through shared memory.
The monitor tool can inspect devices and signals live:

![NoLadder Monitor](noladder_monitor.png)

---

## What It Is Not

* Not IEC 61131 compatible — by design
* Not a SCADA system
* Not trying to replace TwinCAT on day one
* Not production certified — yet
* Not magic — you still need to understand your machine

---

## What The Code Looks Like

```rust
// resolve device paths at startup - strings gone after this
let motor_speed    = map.input("line1.conveyor.motor.speed");
let motor_setpoint = map.output("line1.conveyor.motor.setpoint");
let motor_enable   = map.output("line1.conveyor.motor.enable");
let home_sensor    = map.input("line1.conveyor.sensor.0");

// homing sequence - suspends across cycles without blocking
arena.add(rung!(homing_sequence, |ctx| {
    ctx.write(motor_enable,   true);
    ctx.write(motor_setpoint, 100.0_f32);  // slow speed

    // suspend here - resumes when sensor triggers
    // no nested ifs, no state machine, no flags
    ctx.yield_until(home_sensor, Value::Bool(true)).await;

    ctx.write(motor_setpoint, 0.0_f32);
    ctx.yield_cycles(10).await;              // settle time
}));

// speed controller - runs every cycle after homing
arena.add(rung!(speed_control, |ctx| {
    ctx.yield_until(homed_flag, Value::Bool(true)).await;

    loop {
        let actual  = ctx.read_float(motor_speed);
        let target  = ctx.read_float(recipe_speed);

        let output = (actual + 0.1 * (target - actual))
            .clamp(0.0, 3000.0);

        ctx.write(motor_setpoint, output);
        ctx.yield_cycles(1).await;
    }
}));
```

Compared to the equivalent Structured Text implementation, this avoids manually written state machines, nested CASE statements, and flag variables.

---

## Why Rust

* No garbage collector — deterministic timing
* Memory safety enforced at compile time
* No runtime surprises — everything known at startup
* Ownership model prevents data races
* Compiler checks prevent many IO misuse errors
* `async/await` enables coroutine suspension across control cycles
* Zero-cost abstractions — compiled code is comparable to hand-written C

---

## Why Not TwinCAT, CODESYS, or PLCnext?

Those systems are excellent and widely used.

However they assume:

* Vendor IDEs and proprietary tooling
* IEC 61131 programming models
* Closed runtime environments

NoLadder targets a different space:

* Linux industrial PCs
* open tooling
* modern systems programming
* integration with standard Linux software stacks

It is intended for engineers who are already comfortable building software systems.

---

## Architecture

Three processes, each responsible for a single job:

```
┌──────────────────────┐   ┌──────────────────────┐
│     noladder-bus     │   │   your OS handlers   │
│  (framework binary)  │   │  (in your binary or  │
│                      │   │  a separate process) │
│  Modbus / EtherCAT   │   │                      │
│  → IOImage in shm    │   │  file IO, recipes,   │
└──────────┬───────────┘   │  MQTT, ML inference  │
           │ /dev/shm      └─────────┬────────────┘
           │ noladder_io             │ /dev/shm
           ▼                         │ noladder_mb
┌──────────────────────┐             │
│    your control      │◄────────────┘
│      binary          │
│                      │
│  RT loop on core 1   │
│  rungs (coroutines)  │
│  arena + mailbox     │
└──────────────────────┘
```

**Your logic never touches hardware.**
It reads from a frozen input snapshot and writes to an output image.

The bus server handles the wire protocol. If a device moves from Modbus to EtherCAT, the control code does not change.

**No dynamic allocation after startup.**
Everything is pre-allocated at initialization.

**One isolated CPU core.**
The RT control loop runs with `SCHED_FIFO` on a dedicated core.

**Rungs are coroutines.**
A rung can suspend across cycles without blocking the control loop.

---

## Hardware

NoLadder runs on any Linux system with:

* 2+ CPU cores (one dedicated to RT)
* ~512 MB RAM minimum
* Standard Ethernet port for fieldbus

### Tested On

* x86 industrial IPCs

### Bus Support

| Protocol | Status  | Notes                                      |
| -------- | ------- | ------------------------------------------ |
| Modbus   | v0.1    | TCP and RTU fully working                  |
| EtherCAT | alpha   | via IgH master, hardware validation needed |
| Profinet | planned | Q2 2026 if community interest              |
| CANopen  | planned | socketcan ready, device profiles needed    |

---

## Getting Started

### Prerequisites

Linux PREEMPT_RT is recommended for production but not required for development.

---

### Hello World (three terminals)

No hardware required. Uses a simulated Modbus slave.

See the **Try it in 60 seconds** section above for the quickest setup.

---

### Validate Your Config

```bash
cargo run --bin validate -- machine.toml
```

Example output:

```
✓ bus modbus0 - OK
✓ device line1.conveyor.motor - vfd on modbus0 node 0
✓ device line1.conveyor.sensor - digital_in on modbus0 node 1
2 devices OK
```

---

### Production Setup

```bash
# isolate CPU core
GRUB_CMDLINE_LINUX="isolcpus=1 nohz_full=1 rcu_nocbs=1"
sudo update-grub

# allow RT scheduling without root
sudo setcap cap_sys_nice,cap_ipc_lock+ep ./target/release/noladder
```

Run:

```
noladder-bus machine.toml
noladder machine.toml
```

---

## Machine Configuration

Machine topology is defined in a single TOML file:

```toml
[general]
cycle_ms = 1

[bus.ethercat0]
type      = "ethercat"
interface = "eth1"
cycle_ms  = 1

[device."line1.conveyor.motor"]
bus  = "ethercat0"
node = 3
type = "servo_drive"

[device."line1.conveyor.sensor.home"]
bus  = "ethercat0"
node = 4
type = "digital_in"
```

---

## Contributing

NoLadder is currently **v0.1**.

Areas where contributions are especially useful:

**Bus drivers**

* CANopen
* Profinet
* additional EtherCAT devices

**Device types**

* encoders
* safety relays
* vision triggers

**Hardware testing**

Real industrial hardware validation is extremely valuable.

**Documentation**

Better examples and diagnostics help machine builders adopt the platform.

---

## Roadmap

### v0.1 - Complete

* RT cycle loop
* coroutine rungs
* Modbus driver
* EtherCAT driver (alpha)
* shared memory IO image
* configuration validator
* CiA402 servo support

### v0.2

* CANopen driver
* hot reload of control logic
* cycle statistics dashboard
* hardware watchdog integration

### v0.3

* MQTT monitoring
* camera bus driver (GigE Vision / V4L2)
* structured logging to InfluxDB
* vision examples

### v1.0

* security baseline review
* investigation of functional safety pathways
* production hardening from field feedback

---

## License

MIT — use it however you like.

---

## Why The Name

Because nobody misses the ladder.

---

Built out of frustration with ST and a belief that industrial automation deserves better tooling.

If you share that frustration — open an issue, send a PR, or star the project.
