#!/usr/bin/env python3
# noladder_mock_bus.py
#
# Modbus TCP mock server for noladder development.
#
# Reads machine.toml, serves synthetic data matching the device layout
# used by noladder-bus.  Replace the IP in machine.toml to commission
# against real hardware — no other change needed.
#
# Usage:
#   python3 tools/noladder_mock_bus.py examples/hello_world/machine.toml
#
# Requires: pip install pymodbus  (3.x)

import sys
import struct
import asyncio
import math
import time
import tomllib

from pymodbus.datastore import (
    ModbusSequentialDataBlock,
    ModbusDeviceContext,
    ModbusServerContext,
)
from pymodbus.server import StartAsyncTcpServer

# ------------------------------------
# Register layout — must match modbus.rs
# ------------------------------------

REGISTERS_PER_DEVICE = 16  # per node: 0-7 inputs, 8-15 outputs

# ------------------------------------
# Float encoding — must match modbus.rs float_to_regs / regs_to_float
# Two u16 registers represent one f32 (hi word first)
# ------------------------------------

def float_to_regs(f):
    bs      = struct.pack('>f', f)
    hi, lo  = struct.unpack('>HH', bs)
    return hi, lo


def regs_to_float(hi, lo):
    bs = struct.pack('>HH', hi, lo)
    return struct.unpack('>f', bs)[0]


# ------------------------------------
# Config helpers
# ------------------------------------

def find_modbus_bus(config):
    for name, bus in config.get('bus', {}).items():
        if bus.get('type', 'modbus') == 'modbus':
            return name, bus
    raise ValueError("No modbus bus found in config")


def collect_devices(config, bus_name):
    """Devices on this bus, sorted by path (matches Rust index assignment)."""
    devices = []
    for path, dev in config.get('device', {}).items():
        if dev.get('bus') == bus_name:
            devices.append((path, dev))
    devices.sort(key=lambda x: x[0])
    return devices


# ------------------------------------
# Mock simulation loop
# ------------------------------------

class MockBus:
    def __init__(self, devices, slave_ctx, cycle_ms):
        self.devices   = devices
        self.slave_ctx = slave_ctx
        self.cycle_ms  = cycle_ms
        self._t0       = time.time()
        self._speeds   = {}         # node -> current speed rpm

    def _t(self):
        return time.time() - self._t0

    async def run(self):
        while True:
            await asyncio.sleep(self.cycle_ms / 1000.0)
            self._tick()

    def _tick(self):
        t = self._t()
        for path, dev in self.devices:
            node    = dev['node']
            kind    = dev.get('type', 'unknown')
            ir_base = node * REGISTERS_PER_DEVICE
            hr_base = node * REGISTERS_PER_DEVICE + 8

            if kind == 'vfd':
                # read holding regs: setpoint (2 regs), enable (2 regs)
                hr       = self.slave_ctx.getValues(3, hr_base, 4)
                setpoint = regs_to_float(hr[0], hr[1])
                enable   = regs_to_float(hr[2], hr[3]) >= 0.5

                # first-order speed response toward setpoint
                speed  = self._speeds.get(node, 0.0)
                target = setpoint if enable else 0.0
                speed  = speed + (target - speed) * 0.1   # τ ≈ 10 cycles
                self._speeds[node] = speed

                current = abs(speed) / 1500.0 * 5.0       # A proportional to rpm

                regs = (
                    list(float_to_regs(speed))   +
                    list(float_to_regs(current)) +
                    [0, 0, 0, 0]                           # padding to 8 regs
                )
                self.slave_ctx.setValues(4, ir_base, regs)

            elif kind == 'analog_in':
                # tank sensors: slow sine waves
                level    = 50.0 + 40.0 * math.sin(t * 0.10)
                pressure =  3.0 +  1.0 * math.sin(t * 0.07 + 1.0)
                temp     = 25.0 +  5.0 * math.sin(t * 0.05 + 2.0)
                flow     = 20.0 + 15.0 * math.sin(t * 0.13 + 3.0)

                regs = (
                    list(float_to_regs(level))    +
                    list(float_to_regs(pressure)) +
                    list(float_to_regs(temp))     +
                    list(float_to_regs(flow))
                )
                self.slave_ctx.setValues(4, ir_base, regs)
            # other kinds → no-op (zeros already in block)


# ------------------------------------
# Entry point
# ------------------------------------

async def main(config_path):
    with open(config_path, 'rb') as f:
        config = tomllib.load(f)

    bus_name, bus_cfg = find_modbus_bus(config)
    host     = bus_cfg.get('interface', '127.0.0.1')
    port     = bus_cfg.get('port', 502)
    cycle_ms = bus_cfg.get('cycle_ms', 10)

    devices = collect_devices(config, bus_name)
    if not devices:
        print("No devices found on modbus bus", file=sys.stderr)
        sys.exit(1)

    print(f"Mock bus — {bus_name} at {host}:{port}  cycle {cycle_ms}ms")
    print("Simulating {} device(s): {}".format(
        len(devices),
        ", ".join(
            f"node {d['node']} {p} {d.get('type','?')}"
            for p, d in devices
        )
    ))

    ir_block   = ModbusSequentialDataBlock(0, [0] * 256)
    hr_block   = ModbusSequentialDataBlock(0, [0] * 256)
    device_ctx = ModbusDeviceContext(ir=ir_block, hr=hr_block)
    server_ctx = ModbusServerContext(device_ctx, single=True)

    mock = MockBus(devices, device_ctx, cycle_ms)
    asyncio.create_task(mock.run())

    await StartAsyncTcpServer(context=server_ctx, address=(host, port))


if __name__ == '__main__':
    if len(sys.argv) != 2:
        print(f"Usage: {sys.argv[0]} <machine.toml>", file=sys.stderr)
        sys.exit(1)
    asyncio.run(main(sys.argv[1]))
