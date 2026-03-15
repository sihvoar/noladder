#!/usr/bin/env python3
"""
Mock NoLadder control loop - generates synthetic IO data for testing the monitor.

Creates /dev/shm/noladder_io and writes periodic updates so the monitor
can display live data without running the full bus server + control loop stack.

Usage:
  python3 tools/mock_noladder.py [machine.toml]

In another terminal:
  ./noladder_monitor examples/hello_world/machine.toml
"""

import sys
import os
import mmap
import struct
import time
import math
from pathlib import Path

# IO image layout (must match Rust src/core/io_image.rs)
VALUE_SIZE = 8  # u32 tag + u32 value
MAX_IO = 4096
SEQUENCE_OFF = 0
INPUTS_OFF = 8
OUTPUTS_OFF = INPUTS_OFF + (MAX_IO * VALUE_SIZE)
IO_IMAGE_SIZE = OUTPUTS_OFF + (MAX_IO * VALUE_SIZE)

TAG_UNSET = 0
TAG_BOOL = 1
TAG_INT = 2
TAG_FLOAT = 3

SHM_PATH = "/dev/shm/noladder_io"


class MockIOImage:
    """Simulate the NoLadder IO image in shared memory."""

    def __init__(self, path=SHM_PATH):
        self.path = path
        self.mm = None
        self.start_time = time.time()
        self.create()

    def create(self):
        """Create and initialize the shared memory file."""
        # Clean up any existing file
        if Path(self.path).exists():
            os.remove(self.path)

        # Create file
        with open(self.path, 'wb') as f:
            f.write(b'\x00' * IO_IMAGE_SIZE)

        # Open for read-write
        fd = os.open(self.path, os.O_RDWR)
        self.mm = mmap.mmap(fd, IO_IMAGE_SIZE)
        os.close(fd)

        print(f"✓ Created IO image: {self.path} ({IO_IMAGE_SIZE} bytes)")

    def increment_sequence(self):
        """Increment sequence counter (atomic marker of data consistency)."""
        self.mm.seek(SEQUENCE_OFF)
        seq = struct.unpack("Q", self.mm.read(8))[0]
        self.mm.seek(SEQUENCE_OFF)
        self.mm.write(struct.pack("Q", seq + 1))

    def write_input(self, index, value, tag=TAG_FLOAT):
        """Write an input value."""
        offset = INPUTS_OFF + index * VALUE_SIZE
        self.mm.seek(offset)
        if tag == TAG_FLOAT:
            self.mm.write(struct.pack("If", tag, value))
        elif tag == TAG_INT:
            self.mm.write(struct.pack("Ii", tag, int(value)))
        elif tag == TAG_BOOL:
            self.mm.write(struct.pack("I?xxx", tag, bool(value)))

    def write_output(self, index, value, tag=TAG_FLOAT):
        """Write an output value."""
        offset = OUTPUTS_OFF + index * VALUE_SIZE
        self.mm.seek(offset)
        if tag == TAG_FLOAT:
            self.mm.write(struct.pack("If", tag, value))
        elif tag == TAG_INT:
            self.mm.write(struct.pack("Ii", tag, int(value)))
        elif tag == TAG_BOOL:
            self.mm.write(struct.pack("I?xxx", tag, bool(value)))

    def write_snapshot(self, inputs, outputs):
        """Write a consistent snapshot of inputs and outputs."""
        self.increment_sequence()  # Mark start of write

        for idx, (value, tag) in enumerate(inputs):
            self.write_input(idx, value, tag)

        for idx, (value, tag) in enumerate(outputs):
            self.write_output(idx, value, tag)

        self.increment_sequence()  # Mark end of write

    def close(self):
        """Close the shared memory mapping."""
        if self.mm:
            self.mm.close()
            print(f"Closed IO image")

    def elapsed(self):
        """Seconds since start."""
        return time.time() - self.start_time


def simulate_hello_world(io, duration=60):
    """Simulate the hello_world example with pump + tank sensors."""

    # Device layout from examples/hello_world/machine.toml:
    # demo.pump (vfd): inputs[0-1] = speed, current; outputs[0-1] = setpoint, enable
    # demo.sensors (analog_in): inputs[2-5] = levels 0-3; outputs none

    pump_speed_idx = 0
    pump_current_idx = 1
    pump_setpoint_idx = 0
    pump_enable_idx = 1

    sensor_0_idx = 2  # level
    sensor_1_idx = 3  # pressure
    sensor_2_idx = 4  # temp
    sensor_3_idx = 5  # flow

    pump_speed = 0.0
    tank_level = 50.0

    print(f"\nSimulating hello_world example for {duration} seconds...")
    print("  Inputs:")
    print(f"    {pump_speed_idx}: pump.speed (rpm)")
    print(f"    {pump_current_idx}: pump.current (A)")
    print(f"    {sensor_0_idx}-{sensor_3_idx}: tank sensors (level, pressure, temp, flow)")
    print("  Outputs:")
    print(f"    {pump_setpoint_idx}: pump.setpoint (rpm)")
    print(f"    {pump_enable_idx}: pump.enable (bool)")
    print()

    while io.elapsed() < duration:
        t = io.elapsed()

        # Tank level oscillates
        tank_level = 50.0 + 30.0 * math.sin(t * 0.3)

        # Pump runs when tank level > 30%
        should_enable = tank_level > 30.0
        setpoint = 1500.0 if should_enable else 0.0

        # Pump speed lags behind setpoint (first-order response)
        pump_speed = pump_speed + (setpoint - pump_speed) * 0.05
        pump_current = abs(pump_speed) / 1500.0 * 5.0  # proportional current

        # Simulated tank sensors
        pressure = 3.0 + 1.0 * math.sin(t * 0.2 + 1.0)
        temp = 25.0 + 5.0 * math.sin(t * 0.1 + 2.0)
        flow = 20.0 + 15.0 * math.sin(t * 0.25 + 3.0)

        # Write snapshot
        inputs = [
            (pump_speed, TAG_FLOAT),
            (pump_current, TAG_FLOAT),
            (tank_level, TAG_FLOAT),
            (pressure, TAG_FLOAT),
            (temp, TAG_FLOAT),
            (flow, TAG_FLOAT),
        ]

        outputs = [
            (setpoint, TAG_FLOAT),
            (should_enable, TAG_BOOL),
        ]

        io.write_snapshot(inputs, outputs)

        # Log every 2 seconds (like the real control loop)
        if int(t) % 2 == 0 and int(t * 10) % 20 == 0:
            print(f"[{t:5.1f}s] level {tank_level:5.1f}% pump {pump_speed:6.0f}rpm "
                  f"current {pump_current:4.1f}A")

        time.sleep(0.01)  # 100 Hz update rate


def main():
    """Main entry point."""
    config_path = sys.argv[1] if len(sys.argv) > 1 else "examples/hello_world/machine.toml"

    print("╔══════════════════════════════════════════════════════════╗")
    print("║         NoLadder Mock Control Loop                       ║")
    print("║  Generates synthetic IO data for monitor testing         ║")
    print("╚══════════════════════════════════════════════════════════╝")

    io = MockIOImage()

    try:
        simulate_hello_world(io, duration=3600)  # Run for 1 hour
    except KeyboardInterrupt:
        print("\n✓ Stopped by user")
    finally:
        io.close()
        if Path(io.path).exists():
            os.remove(io.path)
            print(f"✓ Cleaned up {io.path}")


if __name__ == '__main__':
    main()
