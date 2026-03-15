#!/bin/bash
# Launch hello_world example with all processes in separate xterm windows
# Usage: ./tools/launch_hello_world.sh [machine.toml]

set -e

# Get script directory
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

# Configuration
MACHINE_CONFIG="${1:-$PROJECT_DIR/examples/hello_world/machine.toml}"

if [ ! -f "$MACHINE_CONFIG" ]; then
    echo "Error: Machine config not found: $MACHINE_CONFIG"
    echo "Usage: $0 [machine.toml]"
    exit 1
fi

echo "Starting NoLadder hello_world example..."
echo "Machine config: $MACHINE_CONFIG"
echo ""
echo "Opening 5 xterm windows:"
echo "  1. Mock Modbus bus (Python)"
echo "  2. NoLadder bus server"
echo "  3. OS event handler"
echo "  4. Control loop"
echo "  5. Live monitor"
echo ""

# Function to launch a command in xterm
launch_xterm() {
    local title="$1"
    local cmd="$2"

    xterm -T "$title" -e "bash -c 'cd \"$PROJECT_DIR\" && echo \"$title\" && $cmd; echo \"Press Enter to close...\"; read'" &
}

# Terminal 1: Mock Modbus bus
launch_xterm "NoLadder — Mock Modbus Bus" \
    "./noladder_mock_bus \"$MACHINE_CONFIG\""

sleep 1

# Terminal 2: Bus server
launch_xterm "NoLadder — Bus Server" \
    "cargo run --bin noladder-bus -- \"$MACHINE_CONFIG\""

sleep 2

# Terminal 3: OS handler
launch_xterm "NoLadder — OS Handler" \
    "cargo run --example hello_world_os"

# Terminal 4: Control loop
launch_xterm "NoLadder — Control Loop" \
    "cargo run --example hello_world"

sleep 1

# Terminal 5: Monitor
launch_xterm "NoLadder — Monitor" \
    "./noladder_monitor \"$MACHINE_CONFIG\""

echo "✓ All processes started in xterm windows"
echo ""
echo "Close any xterm window to terminate that process."
echo "All processes will exit when you close the main terminal."
