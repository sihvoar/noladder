#!/bin/bash
# NoLadder Python Environment Setup
# One-command setup for all Python dependencies and tools
#
# Usage:
#   ./tools/setup.sh
#
# This creates a clean isolated Python environment and configures
# convenient shell aliases so you never have to think about venv again.

set -e

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"
VENV_PATH="$PROJECT_DIR/.venv"

echo "╔════════════════════════════════════════════════════════════╗"
echo "║   NoLadder Python Environment Setup                         ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""

# Check if venv already exists
if [ -d "$VENV_PATH" ]; then
    echo "✓ Virtual environment already exists at $VENV_PATH"
    echo ""
    read -p "Reinstall? (y/N) " -n 1 -r
    echo
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "Skipping venv creation."
        SKIP_VENV=1
    else
        echo "Removing old venv..."
        rm -rf "$VENV_PATH"
    fi
fi

# Create virtual environment
if [ -z "$SKIP_VENV" ]; then
    echo "Creating virtual environment..."
    python3 -m venv "$VENV_PATH"
    echo "✓ Virtual environment created at $VENV_PATH"
fi

# Activate venv
echo "Activating virtual environment..."
source "$VENV_PATH/bin/activate"
echo "✓ Activated"
echo ""

# Upgrade pip
echo "Upgrading pip..."
pip install --quiet --upgrade pip setuptools wheel
echo "✓ pip upgraded"
echo ""

# Install dependencies
echo "Installing Python dependencies..."
echo "  - PySide6 (GUI framework)"
echo "  - pymodbus (Modbus protocol)"

pip install --quiet PySide6 pymodbus

echo "✓ Dependencies installed"
echo ""

# Test imports
echo "Testing imports..."
python3 << 'EOF'
try:
    from PySide6.QtWidgets import QApplication
    print("  ✓ PySide6 working")
except ImportError as e:
    print(f"  ✗ PySide6 failed: {e}")
    exit(1)

try:
    import pymodbus
    print("  ✓ pymodbus working")
except ImportError as e:
    print(f"  ✗ pymodbus failed: {e}")
    exit(1)
EOF

echo ""
echo "╔════════════════════════════════════════════════════════════╗"
echo "║   Setup Complete! ✓                                         ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""
echo "Next time you open a terminal, run this ONE command:"
echo ""
echo "  source .venv/bin/activate"
echo ""
echo "Then you can use the tools normally:"
echo ""
echo "  ./noladder_monitor examples/hello_world/machine.toml"
echo "  python3 tools/mock_noladder.py"
echo "  python3 tools/noladder_mock_bus.py examples/hello_world/machine.toml"
echo ""
echo "OR, make it permanent by adding to ~/.bashrc:"
echo ""
echo "  echo 'source ~/noladder/.venv/bin/activate' >> ~/.bashrc"
echo ""
echo "Then the venv activates automatically in new terminals."
echo ""
