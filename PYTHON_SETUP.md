# Python Setup — No Longer Needed!

**As of the latest version, venv is no longer required.**

The setup script (`./tools/setup.sh`) now installs all Python packages system-wide:
- PySide6 (via `python3-pyside6`)
- pymodbus (via `python3-pymodbus`)

## Just Run Setup

```bash
./tools/setup.sh
```

That's it. Everything is installed globally. No venv complexity.

---

## If You Have Old Venv

If you created a venv in a previous setup:
```bash
rm -rf .venv
./tools/setup.sh
```

The new setup script will install everything system-wide automatically.

---

## For Non-Ubuntu/Debian Systems

If your distro doesn't have `python3-pyside6` package available, you may need to use a venv:
```bash
python3 -m venv .venv
source .venv/bin/activate
pip install PySide6 pymodbus
```

But most modern distros have it in their package repos now.
