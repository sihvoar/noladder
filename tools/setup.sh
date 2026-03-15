#!/bin/bash
# NoLadder Complete Setup
# Installs everything needed on a fresh Linux installation
#
# Usage:
#   ./tools/setup.sh
#
# This script handles:
# - System dependencies (build tools, libraries)
# - Rust toolchain
# - Python environment
# - Building NoLadder
# - Python dependencies

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
VENV_PATH="$PROJECT_DIR/.venv"

echo "╔════════════════════════════════════════════════════════════╗"
echo "║   NoLadder Complete Setup                                  ║"
echo "║   (Fresh Linux installation → ready to use)                ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""

# ============================================================
# 1. Check and install system dependencies
# ============================================================

echo "Step 1: System Dependencies"
echo "────────────────────────────"

check_command() {
    if ! command -v "$1" &> /dev/null; then
        echo "  ✗ $2 not found"
        return 1
    else
        echo "  ✓ $2 found"
        return 0
    fi
}

# Check if we can use apt (Ubuntu/Debian)
if ! command -v apt &> /dev/null; then
    echo "  ⚠ apt not found. This script is for Ubuntu/Debian."
    echo "  For other distros, install these manually:"
    echo "    - build-essential (gcc, make, etc.)"
    echo "    - python3-dev"
    echo "    - libssl-dev"
    echo "    - pkg-config"
    echo ""
    read -p "Continue anyway? (y/N) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        exit 1
    fi
else
    echo "  Found apt (Ubuntu/Debian)"

    # Update package lists
    echo "  Updating package lists..."
    sudo apt-get update -qq

    # Install build tools
    PACKAGES_NEEDED=""

    check_command gcc "GCC" || PACKAGES_NEEDED="$PACKAGES_NEEDED build-essential"
    check_command python3 "Python 3" || PACKAGES_NEEDED="$PACKAGES_NEEDED python3 python3-dev python3-venv"
    check_command pkg-config "pkg-config" || PACKAGES_NEEDED="$PACKAGES_NEEDED pkg-config"

    # Check for OpenSSL dev
    if [ ! -f /usr/include/openssl/ssl.h ]; then
        echo "  ✗ OpenSSL dev headers not found"
        PACKAGES_NEEDED="$PACKAGES_NEEDED libssl-dev"
    else
        echo "  ✓ OpenSSL dev headers found"
    fi

    # Check for OpenGL (needed by PySide6)
    if [ ! -f /usr/lib/x86_64-linux-gnu/libGL.so.1 ]; then
        echo "  ✗ OpenGL libraries not found"
        PACKAGES_NEEDED="$PACKAGES_NEEDED libgl1-mesa-glx"
    else
        echo "  ✓ OpenGL libraries found"
    fi

    # Check for X11 libraries (needed by PySide6)
    if [ ! -f /usr/lib/x86_64-linux-gnu/libX11.so.6 ]; then
        echo "  ✗ X11 libraries not found"
        PACKAGES_NEEDED="$PACKAGES_NEEDED libx11-6"
    else
        echo "  ✓ X11 libraries found"
    fi

    # Check for DBus (needed by PySide6)
    if [ ! -f /usr/lib/x86_64-linux-gnu/libdbus-1.so.3 ]; then
        echo "  ✗ DBus not found"
        PACKAGES_NEEDED="$PACKAGES_NEEDED libdbus-1-3"
    else
        echo "  ✓ DBus found"
    fi

    if [ -n "$PACKAGES_NEEDED" ]; then
        echo ""
        echo "  Installing missing packages:$PACKAGES_NEEDED"
        sudo apt-get install -y -qq $PACKAGES_NEEDED
        echo "  ✓ Packages installed"
    else
        echo "  ✓ All system dependencies present"
    fi
fi

echo ""

# ============================================================
# 2. Install Rust (if needed)
# ============================================================

echo "Step 2: Rust Toolchain"
echo "──────────────────────"

if ! command -v rustc &> /dev/null; then
    echo "  Rust not found. Installing..."
    echo "  (This takes 1-2 minutes)"
    echo ""

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y -q

    # Source cargo environment
    source "$HOME/.cargo/env"

    echo "  ✓ Rust installed"
else
    RUST_VERSION=$(rustc --version)
    echo "  ✓ $RUST_VERSION"
fi

echo ""

# ============================================================
# 3. Build Rust project
# ============================================================

echo "Step 3: Building NoLadder (Rust)"
echo "────────────────────────────────"

cd "$PROJECT_DIR"

if [ -f "Cargo.toml" ]; then
    echo "  Building project (this takes 1-2 minutes)..."
    cargo build --release -q 2>/dev/null || cargo build --release
    echo "  ✓ Build complete"
else
    echo "  ✗ Cargo.toml not found!"
    exit 1
fi

echo ""

# ============================================================
# 4. Python virtual environment
# ============================================================

echo "Step 4: Python Virtual Environment"
echo "──────────────────────────────────"

if [ -d "$VENV_PATH" ]; then
    echo "  Virtual environment already exists"
    read -p "  Recreate it? (y/N) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo "  Removing old environment..."
        rm -rf "$VENV_PATH"
    else
        SKIP_VENV=1
    fi
fi

if [ -z "$SKIP_VENV" ]; then
    echo "  Creating virtual environment..."
    python3 -m venv "$VENV_PATH"
    echo "  ✓ Virtual environment created"
fi

# Activate venv
source "$VENV_PATH/bin/activate"

echo "  ✓ Activated"
echo ""

# ============================================================
# 5. Python dependencies
# ============================================================

echo "Step 5: Python Dependencies"
echo "──────────────────────────"

echo "  Upgrading pip..."
pip install --quiet --upgrade pip setuptools wheel

echo "  Installing PySide6 (GUI framework)..."
pip install --quiet PySide6 2>&1 | grep -v "already satisfied" || true

echo "  Installing pymodbus (Modbus protocol)..."
pip install --quiet pymodbus

echo "  ✓ Python dependencies installed"
echo ""

# ============================================================
# 6. Verify everything works
# ============================================================

echo "Step 6: Verification"
echo "───────────────────"

python3 << 'EOF'
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
EOF

if [ $? -eq 0 ]; then
    echo "  ✓ All imports successful"
fi

echo ""

# ============================================================
# 7. Setup shell integration
# ============================================================

echo "Step 7: Shell Integration"
echo "────────────────────────"

BASHRC="$HOME/.bashrc"
VENV_ALIAS="source $VENV_PATH/bin/activate"

if grep -q "$VENV_PATH" "$BASHRC" 2>/dev/null; then
    echo "  ✓ Shell already configured"
else
    echo "  Adding venv activation to ~/.bashrc..."
    echo "" >> "$BASHRC"
    echo "# NoLadder Python environment" >> "$BASHRC"
    echo "$VENV_ALIAS" >> "$BASHRC"
    echo "  ✓ Shell configured"
fi

echo ""

# ============================================================
# Success!
# ============================================================

deactivate 2>/dev/null || true
source "$VENV_PATH/bin/activate"

echo "╔════════════════════════════════════════════════════════════╗"
echo "║   Setup Complete! ✓                                         ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""
echo "NoLadder is ready to use!"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Quick Start (5 terminals):"
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
echo "  ./noladder_monitor examples/hello_world/machine.toml"
echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "Or use the launcher:"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo ""
echo "  ./tools/launch_hello_world.sh"
echo ""
echo "The Python environment is active in this terminal."
echo "It will auto-activate in new terminals."
echo ""
echo "Read GETTING_STARTED.md for more information."
echo ""
