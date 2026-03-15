# Getting Started with NoLadder

## Quick Start (5 minutes)

### 1. One-time setup
```bash
./tools/setup.sh
```

This installs everything:
- System dependencies (build tools, graphics libraries)
- Rust toolchain (if needed)
- Python packages (PySide6, pymodbus)
- Builds the entire project

### 2. Run the hello_world example

**Terminal 1 — Mock Modbus server (synthetic data):**
```bash
python3 tools/noladder_mock_bus.py examples/hello_world/machine.toml
```

**Terminal 2 — Bus server (reads Modbus, writes to shared memory):**
```bash
cargo run --bin noladder-bus -- examples/hello_world/machine.toml
```

**Terminal 3 — OS event handler:**
```bash
cargo run --example hello_world_os
```

**Terminal 4 — Control loop (your code):**
```bash
cargo run --example hello_world
```

**Terminal 5 — Live monitor (watch IO signals):**
```bash
./noladder_monitor examples/hello_world/machine.toml
```

Watch the monitor display pump speed ramping up and down. "Hello World!" prints every 2 seconds.

---

## Or Use the Launch Script

Start everything in separate xterm windows:
```bash
./tools/launch_hello_world.sh
```

(Requires xterm installed)

---

## Testing Without the Full Stack

### Mock NoLadder (generates synthetic data)
```bash
python3 tools/mock_noladder.py
```

### Run tests
```bash
python3 tools/tests/test_monitor.py
```

---

## FAQ

**Q: What if `./tools/setup.sh` fails?**
A: Run it with verbose output to see what went wrong:
```bash
bash -x ./tools/setup.sh
```

**Q: Can I use the monitor without the full stack?**
A: Yes! Use `noladder_mock_bus.py` to generate data, then run the monitor in another terminal.

**Q: What Python version do I need?**
A: Python 3.8+. Tested with 3.12.

**Q: Can I use this on non-Ubuntu/Debian systems?**
A: The setup script uses `apt`. For other distros, install manually: build-essential, python3-dev, libssl-dev, python3-pyside6, python3-pymodbus, then run `cargo build --release`.

---

## Next Steps

- Read [README.md](README.md) for architecture overview
- Check [docs/UserGuide.md](docs/UserGuide.md) for how to write rungs
- Look at `examples/hello_world/` to understand the control loop

---

## Architecture

```
Terminal 1: Mock Modbus (generates data)
    ↓
Terminal 2: Bus Server (reads Modbus → writes /dev/shm/noladder_io)
    ↓
Terminal 3: OS Handler (processes OS requests via /dev/shm/noladder_mb)
Terminal 4: Control Loop (reads IO, runs your Rust code, writes outputs)
    ↓
Terminal 5: Monitor (reads /dev/shm/noladder_io, displays live)
```

No magic. Just Rust binaries and shared memory. 100% deterministic.

---

## Troubleshooting

**Monitor won't start:**
```bash
python3 tools/noladder_monitor.py examples/hello_world/machine.toml
```
Check that setup.sh completed successfully (especially Step 4: Verification).

**Bus server won't connect to Modbus:**
- Make sure mock_bus is running in another terminal
- Check the port in `examples/hello_world/machine.toml` (default: 5502)

**Control loop crashes:**
- Check that bus server is running
- Verify OS handler is running
- Look at the error message — usually a device config issue

**Tests:**
```bash
python3 tools/tests/test_monitor.py -v
```

---

Made with ❤️ for people who can program.
