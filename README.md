![NoLadder](assets/noladder-logo-dark-final.svg)

**If Linux has a driver for it — NoLadder can use it as a bus device.**

Industrial control runtime for Linux IPCs, written in Rust.
For people who can program.

**Status:** early development — suitable for experimentation and testing.

---

You looked at Structured Text.
You wondered why control software still looks like 1993.

There is a better way.
This is it.

---

## What It Is

NoLadder is an open source industrial control runtime written in Rust.
It runs on any Linux IPC with a standard Ethernet port.

It replaces proprietary PLC runtimes with a clean, modern architecture
that software engineers can actually work with.

Your control logic is Rust.
Your IO is numbers.
Your hardware is described in a TOML file.
The runtime handles the rest.

---

## How It Works

Three processes. One shared memory region. No proprietary runtime.
```
noladder-bus        owns hardware, speaks protocols
        ↕ /dev/shm/noladder_io
noladder            RT control loop, runs your logic
        ↕ /dev/shm/noladder_io
noladder-monitor    live IO inspector, any tool can read it
```

The bus server handles the wire — Modbus, EtherCAT, CAN,
SDL2 joystick, GPS, camera, anything Linux can read.
Your control logic never knows what protocol a device uses.
Swap hardware without changing a line of control code.

---

## Try It In 60 Seconds

No hardware required. Runs a simulated Modbus motor.

Install Rust (1.75 or newer):
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
```

Clone and run:
```bash
git clone https://github.com/sihvoar/noladder
cd noladder

# terminal 1 — bus server with simulated Modbus motor
cargo run --example hello_world_bus

# terminal 2 — OS server
cargo run --example hello_world_os

# terminal 3 — control loop
cargo run --example hello_world
```

Toggle the Modbus coil with any Modbus tool.
Watch NoLadder say hello.
That is the entire stack working end to end.

---

## What You Write
```rust
fn register_rungs(
    arena: &mut Arena,
    map:   &DeviceMap,
) -> Result<()> {

    let coil = map.input("hello.coil");

    arena.add(rung!(hello_world, {
        ctx.yield_until(coil, true).await;

        ctx.os_request(
            "log.message",
            b"Hello World",
        ).await;
    }));

    Ok(())
}
```

A rung wakes when a condition is met.
It can suspend across cycles without blocking the loop.
No state machines. No nested ifs. No flags.

---

## Screenshot

The monitor reads the shared IO image directly.
Inspect every device and signal live, no configuration needed.

![NoLadder Monitor](noladder_monitor.png)

---

## Documentation

- User guide → [docs/UserGuide.md](docs/UserGuide.md)
- Architecture → [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)
- Design notes → [docs/DESIGN.md](docs/DESIGN.md)
- Adding a bus device → [docs/BusDrivers.md](docs/BusDrivers.md)

---

## License

MIT — [Copyright 2025 AP Sihvonen](LICENSE)
