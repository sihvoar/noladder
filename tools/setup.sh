#!/bin/bash
# NoLadder Setup — System-Wide Installation
# Simpler than venv. Just install everything globally.

set -e

echo "╔════════════════════════════════════════════════════════════╗"
echo "║   NoLadder Simple Setup (System-Wide)                      ║"
echo "║   No venv. No complexity. Everything in one script.        ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# ============================================================
# 1. System Dependencies
# ============================================================

echo "Step 1: System Dependencies"
echo "────────────────────────────"

if ! command -v apt &> /dev/null; then
    echo "  ⚠ apt not found. This script is for Ubuntu/Debian."
    echo "  For other distros, install manually:"
    echo "    - build-essential"
    echo "    - python3-dev"
    echo "    - libssl-dev"
    echo "    - python3-pyside6"
    echo ""
    exit 1
fi

echo "  Found apt. Updating package lists..."
sudo apt-get update -qq

# Check what we need
PACKAGES_NEEDED=""

[ ! -f /usr/include/openssl/ssl.h ] && \
    PACKAGES_NEEDED="$PACKAGES_NEEDED build-essential python3-dev libssl-dev pkg-config"

[ ! -f /usr/lib/x86_64-linux-gnu/libGL.so.1 ] && \
    PACKAGES_NEEDED="$PACKAGES_NEEDED libgl1-mesa-glx libx11-6 libdbus-1-3"

# Try to install PySide6 system-wide
if ! python3 -c "from PySide6.QtWidgets import QApplication" 2>/dev/null; then
    PACKAGES_NEEDED="$PACKAGES_NEEDED python3-pyside6"
fi

# Try to install pymodbus system-wide
if ! python3 -c "import pymodbus" 2>/dev/null; then
    PACKAGES_NEEDED="$PACKAGES_NEEDED python3-pymodbus"
fi

if [ -n "$PACKAGES_NEEDED" ]; then
    echo "  Installing:$PACKAGES_NEEDED"
    sudo apt-get install -y -qq $PACKAGES_NEEDED
    echo "  ✓ Packages installed"
else
    echo "  ✓ All dependencies present"
fi

echo ""

# ============================================================
# 2. Rust Toolchain
# ============================================================

echo "Step 2: Rust Toolchain"
echo "──────────────────────"

if ! command -v rustc &> /dev/null; then
    echo "  Installing Rust..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y -q
    source "$HOME/.cargo/env"
    echo "  ✓ Rust installed"
else
    echo "  ✓ $(rustc --version)"
fi

echo ""

# ============================================================
# 3. Build Rust Project
# ============================================================

echo "Step 3: Building NoLadder"
echo "─────────────────────────"

cd "$PROJECT_DIR"
cargo build --release -q 2>/dev/null || cargo build --release
echo "  ✓ Build complete"

echo ""

# ============================================================
# 4. Verify Everything Works
# ============================================================

echo "Step 4: Verification"
echo "───────────────────"

python3 << 'PYEOF'
import sys

checks = [
    ("PySide6", "from PySide6.QtWidgets import QApplication"),
    ("pymodbus", "import pymodbus"),
]

all_ok = True
for name, import_stmt in checks:
    try:
        exec(import_stmt)
        print(f"  ✓ {name} working")
    except ImportError as e:
        print(f"  ✗ {name} failed: {e}")
        all_ok = False

if not all_ok:
    sys.exit(1)
PYEOF

if [ $? -eq 0 ]; then
    echo "  ✓ All imports successful"
fi

echo ""

# ============================================================
# Success!
# ============================================================

echo "╔════════════════════════════════════════════════════════════╗"
echo "║   Setup Complete! ✓                                         ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""
echo "NoLadder is ready to use."
echo ""
echo "Quick start (5 terminals):"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "Terminal 1 (Mock Modbus):"
echo "  python3 tools/noladder_mock_bus.py examples/hello_world/machine.toml"
echo ""
echo "Terminal 2 (Bus Server):"
echo "  cargo run --bin noladder-bus -- examples/hello_world/machine.toml"
echo ""
echo "Terminal 3 (OS Handler):"
echo "  cargo run --example hello_world_os"
echo ""
echo "Terminal 4 (Control Loop):"
echo "  cargo run --example hello_world"
echo ""
echo "Terminal 5 (Monitor):"
echo "  python3 tools/noladder_monitor.py examples/hello_world/machine.toml"
echo ""
echo "Or use the launcher:"
echo "  ./tools/launch_hello_world.sh"
echo ""
