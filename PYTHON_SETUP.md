# Python Environment Setup

## Qt6 Library Issue

The system has **Qt6 6.4.2** but **PySide6 6.10.2** requires a newer version, causing this error:

```
ImportError: /usr/lib/x86_64-linux-gnu/libQt6DBus.so.6: undefined symbol: _ZN14QObjectPrivateC2Ei
```

## Solutions

### Option 1: Use Virtual Environment (Recommended)
```bash
python3 -m venv .venv
source .venv/bin/activate
pip install PySide6 pymodbus
./noladder_monitor examples/hello_world/machine.toml
```

### Option 2: System-Wide (Requires sudo)
```bash
sudo apt install python3-pyside6 python3-pymodbus
./noladder_monitor examples/hello_world/machine.toml
```

### Option 3: Work Without Monitor GUI

The **mock tools work without PySide6**:
```bash
# Terminal 1: Generate synthetic IO data
python3 tools/mock_noladder.py

# Terminal 2: Serve Modbus simulation
python3 tools/noladder_mock_bus.py examples/hello_world/machine.toml

# Terminal 3: Run actual NoLadder bus + control loop
cargo run --bin noladder-bus -- examples/hello_world/machine.toml
cargo run --example hello_world_os
cargo run --example hello_world
```

## Current System State

✅ pymodbus — **system-wide ready**
❌ PySide6 — **requires venv due to Qt6 mismatch**

## Why This Happens

- Ubuntu ships Qt6 6.4.2 (library)
- Python 3.12 requires PySide6 6.6.0+
- Earlier PySide6 versions don't support Python 3.12
- Creating a venv avoids the system Qt6 entirely

It's annoying, but this is a common Python packaging issue across many projects.
