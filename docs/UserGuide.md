# NoLadder User Guide

## Who This Is For

You are a software engineer.
You understand concurrency, memory, and systems programming.
You have looked at PLC programming and concluded life is too short.

This guide will have you running control logic against
real hardware in an afternoon.

---

## Concepts

Before writing any code, five concepts to understand.
Everything else follows from these.

### 1. The Control Cycle

NoLadder runs your logic in a fixed time loop - the control cycle.
Every 1ms (or whatever you configure) the following happens,
in exactly this order, every time:
```
1. freeze inputs    - snapshot IO from hardware
2. run your rungs   - execute control logic
3. send outputs     - write results to hardware
4. sleep            - wait until next cycle
```

Your code never sees hardware directly.
It reads from the frozen snapshot and writes to a pending output buffer.
The hardware sees your outputs on the next cycle.

This one-cycle latency is intentional and universal.
Every PLC works this way. At 1ms it is physically irrelevant
for anything except high speed motion control.

### 2. The IO Image

All hardware values live in a flat array of numbers.
Booleans, integers, floats - everything is a `Value`.
Every device on every bus is mapped to a range of indices
in this array at startup.
```
inputs[0]  = motor speed actual    (float, from EtherCAT node 3)
inputs[1]  = motor current         (float, from EtherCAT node 3)
inputs[2]  = home sensor           (bool,  from EtherCAT node 4)
outputs[0] = motor speed setpoint  (float, to EtherCAT node 3)
outputs[1] = motor enable          (bool,  to EtherCAT node 3)
```

You never use raw indices in your code.
You resolve named paths at startup and use typed handles:
```rust
// once at startup - string lookup, cost irrelevant
let motor_speed   = map.input("line1.motor.speed");
let motor_enable  = map.output("line1.motor.enable");

// at runtime - pure array index, zero cost
let speed = ctx.read_float(motor_speed);
ctx.write(motor_enable, true);
```

### 3. Rungs

A rung is an async Rust function that runs inside the cycle executor.
It can suspend across cycles without blocking the control loop.
```rust
rung!(my_rung, {
    // this code runs across as many cycles as needed
    // but never blocks the loop
    ctx.write(motor_enable, true);
    ctx.yield_until(home_sensor, true).await;  // suspend here
    ctx.write(motor_speed, 1500.0_f32);        // resume here
});
```

When a rung suspends, the executor moves on to the next rung.
On the next cycle it checks if the condition is met.
If yes - resume. If no - skip and check again next cycle.

This means sequential machine logic reads as sequential code.
No state machines. No flags. No nested ifs.

### 4. The Bus Server

The bus server is a separate process that owns all hardware.
It runs each bus at its own cycle rate and keeps the IO image
up to date. Your control logic never knows or cares what
protocol a device uses.
```
EtherCAT bus  →  1ms cycle  →  IO image
Modbus bus    →  10ms cycle →  IO image  ←  your rungs
CAN bus       →  1ms cycle  →  IO image
```

Swap a device from Modbus to EtherCAT?
Update machine.toml. Your rung code does not change.

### 5. The OS Server

The OS server handles anything that is not hard real-time -
reading recipes from a database, publishing MQTT messages,
writing alarm logs, loading configuration from files.

Your rung requests something and suspends.
The OS server delivers the response whenever it is ready.
The control loop never waits.
```rust
rung!(recipe_loader, {
    // request - fire and forget
    ctx.os_request("recipe.load").await;
    // suspended - OS server does its thing
    // resumed - data is in IO image
    let speed = ctx.read_float(recipe_speed);
});
```

---

## Your First Project

### Step 1 - Install
```bash
# Rust toolchain
curl --proto '=https' --tlsv1.2 -sSf \
    https://sh.rustup.rs | sh

# clone NoLadder
git clone https://github.com/sihvoar/noladder
cd noladder
```

### Step 2 - Run The Example

No hardware needed. The example runs against a
simulated Modbus motor.
```bash
cargo run --example basic_io
```

Expected output (may vary):
```
INFO NoLadder v0.1.0
INFO Industrial control for Linux IPCs
INFO Config loaded - 1 bus 2 devices
INFO Memory locked
INFO Starting bus 'modbus0' — 2 devices 2 IO points
INFO Modbus server listening on 127.0.0.1:502
... (motor simulation and control rungs execute)
```

The exact output depends on your config, log level, and rungs.
Run with `RUST_LOG=debug` for detailed diagnostics.

### Step 3 - Describe Your Hardware

Create `machine.toml` in your project root.
This file describes every device on every bus.
```toml
[general]
cycle_ms = 10       # 10ms control cycle

[bus.modbus0]
interface = "127.0.0.1"  # or IP address for real hardware
port      = 502
cycle_ms  = 10

[device."line1.conveyor.motor"]
bus  = "modbus0"    # start with Modbus for testing
node = 0
type = "vfd"

[device."line1.conveyor.sensor"]
bus  = "modbus0"
node = 1
type = "digital_in"

# For production with faster determinism, add EtherCAT:
# [bus.ethercat0]
# interface = "eth1"
# cycle_ms  = 1
# note: EtherCAT driver is alpha - needs real hardware validation
```

Validate it before running:
```bash
cargo run --bin validate -- machine.toml

✓ bus ethercat0 - OK
✓ bus modbus0 - OK
✓ device line1.conveyor.motor - servo_drive on ethercat0 node 3
✓ device line1.conveyor.sensor.home - digital_in on ethercat0 node 4
✓ device line1.tank.pressure - analog_in on modbus0 node 12
3 devices OK
```

### Step 4 - Resolve Your Device Paths

In `main.rs`, resolve all device paths to indices at startup.
After this point no strings exist in your control code.
```rust
fn register_rungs(
    arena: &mut Arena,
    map:   &DeviceMap,
) -> Result<()> {

    // resolve at startup - panics on bad path
    // caught here, never at runtime
    let motor_speed    = InputIndex(
        map.input("line1.conveyor.motor.speed")
    );
    let motor_enable   = OutputIndex(
        map.output("line1.conveyor.motor.enable")
    );
    let motor_setpoint = OutputIndex(
        map.output("line1.conveyor.motor.setpoint")
    );
    let home_sensor    = InputIndex(
        map.input("line1.conveyor.sensor.home.0")
    );

    // register your rungs here
    // ...

    Ok(())
}
```

### Step 5 - Write Your Rungs

Rungs are async functions registered at startup.
They run in registration order every cycle.
```rust
// simple rung - runs once and completes
arena.add(rung!(startup_check, {
    let pressure = ctx.read_float(tank_pressure);
    if pressure > 10.0 {
        ctx.write(alarm_high_pressure, true);
    }
}));

// sequential rung - spans multiple cycles
arena.add(rung!(homing_sequence, {
    ctx.write(motor_enable,   true);
    ctx.write(motor_setpoint, 100.0_f32);   // slow speed

    // suspend until sensor triggers
    ctx.yield_until(home_sensor, true).await;

    ctx.write(motor_setpoint, 0.0_f32);
    ctx.yield_ms(50).await;                 // settle

    ctx.write(homed_flag, true);
}));

// cyclic rung - resets each scan after completing
arena.add(rung!(speed_monitor, {
    let speed = ctx.read_float(motor_speed);
    if speed > MAX_SPEED {
        ctx.write(motor_enable, false);
        ctx.write(alarm_overspeed, true);
    }
}));
```

---

## Rung Patterns

### Wait For Condition
```rust
rung!(wait_example, {
    // suspend until input matches value
    ctx.yield_until(sensor, true).await;
    ctx.yield_until(pressure, Value::Float(10.0)).await;
});
```

### Wait For Time
```rust
rung!(timing_example, {
    ctx.write(valve_open, true);
    ctx.yield_ms(500).await;        // wait 500ms
    ctx.write(valve_open, false);
});
```

### Timeout Pattern
```rust
rung!(homing_with_timeout, {
    ctx.write(motor_enable, true);

    let result = ctx.race(
        ctx.yield_until(home_sensor, true),
        ctx.yield_ms(5000),             // 5 second timeout
    ).await;

    match result {
        RaceResult::First  => {
            // homed OK
            ctx.write(homed_flag, true);
        }
        RaceResult::Second => {
            // timeout - fault
            ctx.write(motor_enable, false);
            ctx.write(alarm_homing_timeout, true);
        }
    }
});
```

### Wait For Any Condition
```rust
rung!(operator_input, {
    // wait for start button OR estop OR fault
    let which = ctx.yield_until_any(&[
        (start_button, Value::Bool(true)),
        (estop,        Value::Bool(true)),
        (fault_input,  Value::Bool(true)),
    ]).await;

    match which {
        0 => ctx.write(cycle_running, true),
        1 => ctx.write(safe_state,    true),
        2 => ctx.write(fault_active,  true),
        _ => unreachable!()
    }
});
```

### Wait For All Conditions
```rust
rung!(multi_axis_home, {
    // all axes must home - order irrelevant
    // each axis has its own homing rung running
    // in parallel via IO flags
    ctx.yield_until_all(&[
        (x_axis_homed, Value::Bool(true)),
        (y_axis_homed, Value::Bool(true)),
        (z_axis_homed, Value::Bool(true)),
    ]).await;

    ctx.write(all_axes_ready, true);
});
```

### Branching Logic
```rust
rung!(mode_handler, {
    ctx.yield_until(cycle_start, true).await;

    if ctx.read_bool(manual_mode) {
        // manual path - operator jogs axes
        ctx.yield_until(jog_button, true).await;
        ctx.write(motor_enable, true);
        ctx.yield_until(jog_button, false).await;
        ctx.write(motor_enable, false);
    } else {
        // auto path - run recipe
        ctx.os_request("recipe.load").await;
        let speed = ctx.read_float(recipe_speed);
        ctx.write(motor_setpoint, speed);
        ctx.write(motor_enable,   true);
        ctx.yield_until(position_reached, true).await;
        ctx.write(motor_enable, false);
    }
});
```

### Remote Command Execution (OS Bridge)
```rust
rung!(command_executor, {
    // wait for remote command via OS server
    ctx.os_request("execute_command").await;

    // read the command parameters from shared IO
    let command_id = ctx.read_int(command_index);
    let param1 = ctx.read_float(parameter_1);

    // execute hardware action based on command
    ctx.write(device_command, command_id);
    ctx.write(device_param, param1);
    ctx.write(device_trigger, true);

    // wait for completion with timeout
    let result = ctx.race(
        ctx.yield_until(device_done, true),
        ctx.yield_ms(5000),  // 5 second timeout
    ).await;

    match result {
        RaceResult::First => {
            ctx.os_request("complete").await;
        }
        RaceResult::Second => {
            ctx.write(device_fault, true);
            ctx.os_request("timeout_fault").await;
        }
    }
});
```

**Note:** Advanced patterns like MQTT bridges and CAN communication are planned for v0.2/v0.3.
In v0.1, use the OS request/response bridge (above) for any non-RT communication.

---

## Device Types (V0.1)

| Type            | Inputs (Read)                       | Outputs (Write)                        |
|-----------------|-------------------------------------|--------|
| `servo_drive`   | position, velocity, torque, following_error, enabled, fault, target_reached, homing_complete, error_code, referenced | target_position, target_velocity, target_torque, max_torque, fault_reset, quick_stop |
| `vfd`           | speed, current                      | setpoint, enable                       |
| `digital_in`    | bits 0-7 (8 separate inputs)        | -                                      |
| `digital_out`   | -                                   | bits 0-7 (8 separate outputs)          |
| `analog_in`     | channels 0-3 (4 inputs)             | -                                      |
| `analog_out`    | -                                   | channels 0-3 (4 outputs)               |
| `mixed_io`      | channels 0-3 (input)                | channels 0-3 (output)                  |
| `safety_relay`  | ok, fault                           | reset                                  |
| `safety_door`   | closed, locked                      | -                                      |
| `flag`          | any integer/bool flag (virtual, no hardware) | any integer/bool flag (virtual, no hardware) |

**Note:** `can_rxtx`, `mqtt_rx`, `mqtt_tx` device types are planned for v0.2/v0.3 and not yet implemented.

---

## Production Setup

### Kernel Configuration

For reliable RT performance add to kernel cmdline:
```bash
# /etc/default/grub
GRUB_CMDLINE_LINUX="isolcpus=1 nohz_full=1 rcu_nocbs=1"

sudo update-grub
sudo reboot
```

This dedicates core 1 entirely to NoLadder.
Linux, networking, and your OS server run on core 0.

### Capabilities

Run without root using Linux capabilities:
```bash
sudo setcap cap_sys_nice,cap_ipc_lock+ep \
    ./target/release/noladder
```

`cap_sys_nice`  - allows SCHED_FIFO RT scheduling
`cap_ipc_lock`  - allows mlockall memory locking

### Systemd Service
```ini
# /etc/systemd/system/noladder.service

[Unit]
Description=NoLadder Control Runtime
After=network.target
Wants=network.target

[Service]
Type=simple
User=noladder
ExecStart=/opt/noladder/noladder /etc/noladder/machine.toml
Restart=on-failure
RestartSec=1

# RT permissions
AmbientCapabilities=CAP_SYS_NICE CAP_IPC_LOCK
LimitRTPRIO=80
LimitMEMLOCK=infinity

# if it crashes - restart fast
# hardware watchdog will safe-state outputs
# before restart completes
StartLimitBurst=3
StartLimitIntervalSec=10

[Install]
WantedBy=multi-user.target
```
```bash
sudo systemctl enable noladder
sudo systemctl start noladder
sudo journalctl -u noladder -f
```

### Hardware Watchdog

NoLadder kicks a hardware watchdog each cycle.
If the process crashes or hangs, the watchdog fires
and drives all outputs to safe state (off/zero).

Your hardware must support this.
Configure the watchdog device in machine.toml:
```toml
[watchdog]
device      = "/dev/watchdog"
timeout_ms  = 100     # must be > cycle_ms
safe_output = "off"   # all outputs off on timeout
```

---

## Debugging

### Verbose Logging
```bash
RUST_LOG=debug ./noladder machine.toml
```

Shows every rung state change, every IO value,
every cycle overrun in detail.

### Cycle Statistics

Logged automatically every 10 seconds:
```
INFO cycle stats - count: 10000 overruns: 0 (0.00%)
     avg: 312µs min: 287µs max: 891µs
     utilization: 31.2%
```

High utilization (>80%) means your cycle time
is too short for your logic. Increase `cycle_ms`
or split complex rungs.

### Arena Statistics

Logged every 30 seconds:
```
INFO arena: 12 total 3 ready 8 waiting
     (6 io / 1 time / 1 os) 1 complete 0 faulted
```

If waiting_os is non-zero and not decreasing -
your OS server is not delivering responses.

If waiting_io is non-zero and not decreasing -
a condition your rung is waiting for never fired.
Check your wiring and device config.

### Config Validator

Always run before deploying:
```bash
cargo run --bin validate -- machine.toml
```

Catches unknown bus references, bad device types,
duplicate node addresses, missing required fields.

---

## Common Mistakes

### Rung Never Completes

Symptom: `waiting_io` in arena stats stays high.
Cause: condition never becomes true.
```rust
// wrong - waiting for exact float equality
// float from sensor will never be exactly 1500.0
ctx.yield_until(motor_speed,
    Value::Float(1500.0)).await;

// right - use a threshold rung
// or a separate flag set by speed monitor rung
ctx.yield_until(speed_reached_flag,
    Value::Bool(true)).await;
```

### Writing To An Input

The compiler catches this - `InputIndex` and
`OutputIndex` are different types.
You cannot pass an `InputIndex` to `ctx.write()`.

### Slow Modbus Device In Fast Logic

Symptom: cycle overruns when reading from Modbus device.
Cause: Modbus at 10ms cycle, control at 1ms -
       data is stale 90% of the time.
```rust
// wrong - reading Modbus value in 1ms rung
// value only updates every 10ms
let pressure = ctx.read_float(modbus_pressure);

// right - use the value knowing it may be 10ms old
// for slow processes (temperature, pressure)
// this is usually fine
// for fast processes - move the device to EtherCAT
```

Document slow devices in machine.toml:
```toml
[device."tank.pressure"]
bus  = "modbus0"
type = "analog_in"
note = "10ms latency - not suitable for safety logic"
```

### Too Many Rungs In One

Symptom: high cycle utilization, hard to debug.
Cause: one rung doing too many things.

Split complex sequences into smaller rungs
that coordinate via IO image flags:
```rust
// instead of one giant rung
// split into phases coordinated by flags

rung!(phase1_home,  { /* homing logic   */ });
rung!(phase2_load,  { /* loading logic  */ });
rung!(phase3_run,   { /* running logic  */ });
rung!(safety_watch, { /* always running */ });
```

---

## Writing A Bus Driver

If your hardware is not supported, writing a driver
is straightforward. Implement the `BusDriver` trait:
```rust
use noladder::bus::BusDriver;
use noladder::core::io_image::{IOImage, Value};
use noladder::config::loader::BusConfig;

pub struct MyDriver {
    // your hardware connection here
}

impl BusDriver for MyDriver {

    fn init(config: &BusConfig) -> Result<Self> {
        // connect to hardware
        // return Err if hardware not found
        // NoLadder will retry
    }

    fn read_inputs(&mut self) -> Result<Vec<Value>> {
        // read from hardware
        // return normalized Values
        // return Err on comms failure
    }

    fn write_outputs(
        &mut self,
        values: Vec<Value>
    ) -> Result<()> {
        // write Values to hardware
        // return Err on comms failure
    }

    fn cycle_ms(&self) -> u32 {
        // how fast can your hardware run
    }
}
```

Add your driver to `bus/mod.rs` and open a PR.
If you work for a hardware vendor -
your customers will thank you.

---

## Getting Help

**GitHub Issues** - bug reports, feature requests
**GitHub Discussions** - questions, use cases, ideas

When reporting an issue please include:

- `machine.toml` (anonymized if needed)
- `RUST_LOG=debug` output around the problem
- Cycle stats at time of failure
- Hardware description

---

## License

MIT. Do what you want. Contribute back if you can.

---

*NoLadder exists because industrial automation*
*deserves better than 1970s programming tools.*
*If you agree - star the repo, open an issue,*
*send a PR, or just tell a colleague.*