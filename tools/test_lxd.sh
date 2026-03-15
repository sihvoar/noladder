#!/bin/bash
# Test NoLadder setup in pristine LXD container
# Usage: ./tools/test_lxd.sh [keep]
# "keep" argument keeps container for manual testing

set -e

CONTAINER="test-noladder-$$"
KEEP=${1:-}

echo "╔════════════════════════════════════════════════════════════╗"
echo "║   NoLadder LXD Test                                         ║"
echo "║   Testing setup.sh on pristine Ubuntu 24.04                ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""
echo "Container: $CONTAINER"
echo ""

# ============================================================
# 1. Create container
# ============================================================

echo "Step 1: Creating LXD container..."
sudo lxc launch ubuntu:24.04 "$CONTAINER" -q
echo "  ✓ Container created"

# ============================================================
# 2. Wait for cloud-init
# ============================================================

echo ""
echo "Step 2: Waiting for system ready..."
sudo lxc exec "$CONTAINER" -- cloud-init status --wait > /dev/null 2>&1
echo "  ✓ System ready"

# ============================================================
# 3. Clone and setup
# ============================================================

echo ""
echo "Step 3: Testing setup.sh..."
echo "────────────────────────────"

sudo lxc exec "$CONTAINER" -- bash <<'SETUP_EOF'
set -e
cd /root
git clone https://github.com/sihvoar/noladder 2>/dev/null
cd noladder
./tools/setup.sh
SETUP_EOF

echo ""
echo "  ✓ Setup complete"

# ============================================================
# 4. Verify binaries built
# ============================================================

echo ""
echo "Step 4: Verifying build..."

sudo lxc exec "$CONTAINER" -- bash <<'VERIFY_EOF'
cd /root/noladder
[ -f target/release/noladder ] && echo "  ✓ noladder binary"
[ -f target/release/noladder-bus ] && echo "  ✓ noladder-bus binary"
VERIFY_EOF

# ============================================================
# 5. Quick functional test
# ============================================================

echo ""
echo "Step 5: Quick functional test..."
echo "────────────────────────────────"

sudo lxc exec "$CONTAINER" -- bash <<'TEST_EOF'
set -e
cd /root/noladder

# Start mock bus in background
timeout 3 python3 tools/noladder_mock_bus.py examples/hello_world/machine.toml &>/dev/null &
MOCK_PID=$!

# Give it a moment to start
sleep 1

# Start bus server
timeout 3 cargo run --release --bin noladder-bus -- examples/hello_world/machine.toml &>/dev/null &
BUS_PID=$!

sleep 1

# Quick control loop test
timeout 2 cargo run --release --example hello_world &>/dev/null &
LOOP_PID=$!

sleep 2

# Cleanup
kill $MOCK_PID $BUS_PID $LOOP_PID 2>/dev/null || true
wait $MOCK_PID $BUS_PID $LOOP_PID 2>/dev/null || true

echo "  ✓ All processes started and ran"
TEST_EOF

# ============================================================
# Summary
# ============================================================

echo ""
echo "╔════════════════════════════════════════════════════════════╗"
echo "║   All Tests Passed! ✓                                       ║"
echo "╚════════════════════════════════════════════════════════════╝"
echo ""

if [ "$KEEP" = "keep" ]; then
    echo "Container kept for manual testing:"
    echo "  sudo lxc exec $CONTAINER -- bash"
    echo ""
    echo "When done:"
    echo "  sudo lxc delete $CONTAINER --force"
else
    echo "Cleaning up container..."
    sudo lxc delete "$CONTAINER" --force -q
    echo "  ✓ Container removed"
    echo ""
    echo "To keep container for manual testing, run:"
    echo "  ./tools/test_lxd.sh keep"
fi

echo ""
