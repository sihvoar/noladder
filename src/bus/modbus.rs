// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/bus/modbus.rs

use anyhow::Result;
use tokio_modbus::prelude::*;
use tokio::runtime::Runtime;
use tracing::{info, warn, debug};

use crate::core::io_image::{IOImage, Value};
use crate::config::loader::{BusConfig, ResolvedDevice};

// ------------------------------------
// Modbus register map
// each device occupies a range of registers
// determined by its type and node address
// ------------------------------------

const REGISTERS_PER_DEVICE: u16 = 16;

// register layout per device:
// 0-7  : input registers  (read from device)
// 8-15 : output registers (write to device)

struct DeviceRegisters {
    input_base:  u16,
    output_base: u16,
    io_index:    usize,  // base index into IOImage
}

// ------------------------------------
// Modbus bus driver
// ------------------------------------

pub struct ModbusDriver {
    name:        String,
    address:     std::net::SocketAddr,
    cycle_ms:    u32,
    devices:     Vec<DeviceRegisters>,
    rt:          Runtime,  // tokio runtime for async modbus calls
}

impl ModbusDriver {
    pub fn new(
        name:    &str,
        config:  &BusConfig,
        devices: &[ResolvedDevice],
        io_base: usize,  // starting index in IOImage for this bus
    ) -> Result<Self> {

        let address = format!(
            "{}:{}",
            config.interface,
            config.port.unwrap_or(502)
        ).parse()?;

        // build register map for each device
        let mut device_registers = Vec::new();
        for (i, device) in devices.iter().enumerate() {
            let node_offset = device.node as u16
                * REGISTERS_PER_DEVICE;

            device_registers.push(DeviceRegisters {
                input_base:  node_offset,
                output_base: node_offset + 8,
                io_index:    io_base + (i * 8),
            });

            debug!(
                "Device '{}' mapped — \
                 input regs {}..{} output regs {}..{} \
                 io index {}",
                device.path,
                node_offset, node_offset + 7,
                node_offset + 8, node_offset + 15,
                io_base + (i * 8),
            );
        }

        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;

        info!(
            "Modbus driver '{}' — {} devices at {}",
            name,
            device_registers.len(),
            address,
        );

        Ok(Self {
            name:    name.to_string(),
            address,
            cycle_ms: config.cycle_ms,
            devices: device_registers,
            rt,
        })
    }

    // ------------------------------------
    // Main bus loop
    // runs on its own thread
    // reads inputs, writes outputs each cycle
    // ------------------------------------

    pub fn run(self, io: &'static mut IOImage) -> Result<()> {
        use std::time::{Duration, Instant};

        info!("Modbus driver '{}' starting", self.name);

        let cycle = Duration::from_millis(self.cycle_ms as u64);

        // connect — retry until success
        // bus server should not fail on startup
        // if hardware is not ready yet
        let mut ctx = self.connect_with_retry()?;

        let mut next_cycle = Instant::now() + cycle;

        loop {
            let cycle_start = Instant::now();

            // read all device inputs
            for device in &self.devices {
                match self.rt.block_on(
                    ctx.read_input_registers(
                        device.input_base, 8
                    )
                ) {
                    Ok(regs) => {
                        // map registers to Values in IOImage
                        // registers are u16 — interpret as float pairs
                        // two u16 registers = one f32
                        for (i, chunk) in regs.chunks(2).enumerate() {
                            if chunk.len() == 2 {
                                let value = regs_to_float(chunk[0], chunk[1]);
                                io.publish_inputs(
                                    device.io_index + i,
                                    Value::Float(value)
                                );
                            }
                        }
                    }
                    Err(e) => {
                        warn!(
                            "Modbus read failed on '{}' device {}: {}",
                            self.name, device.io_index, e
                        );
                        // mark device inputs as unset
                        // control logic will see Unset and handle it
                        for i in 0..4 {
                            io.publish_inputs(
                                device.io_index + i,
                                Value::Unset
                            );
                        }
                        // attempt reconnect
                        if let Ok(new_ctx) = self.connect_with_retry() {
                            ctx = new_ctx;
                        }
                    }
                }
            }

            // signal fresh data to control loop
            io.signal_ready();

            // write all device outputs
            for device in &self.devices {
                let mut regs = Vec::with_capacity(8);

                for i in 0..4 {
                    let value = io.read_output(device.io_index + i);
                    let (hi, lo) = float_to_regs(match value {
                        Value::Float(f) => f,
                        Value::Int(i)   => i as f32,
                        Value::Bool(b)  => if b { 1.0 } else { 0.0 },
                        Value::Unset    => 0.0,
                    });
                    regs.push(hi);
                    regs.push(lo);
                }

                if let Err(e) = self.rt.block_on(
                    ctx.write_multiple_registers(
                        device.output_base,
                        &regs
                    )
                ) {
                    warn!(
                        "Modbus write failed on '{}': {}",
                        self.name, e
                    );
                }
            }

            // cycle timing
            let elapsed = cycle_start.elapsed();
            if elapsed > cycle {
                warn!(
                    "Modbus driver '{}' cycle overrun — \
                     took {}ms budget {}ms",
                    self.name,
                    elapsed.as_millis(),
                    self.cycle_ms,
                );
            }

            let now = Instant::now();
            if now < next_cycle {
                std::thread::sleep(next_cycle - now);
            }
            next_cycle += cycle;
        }
    }

    fn connect_with_retry(
        &self
    ) -> Result<tokio_modbus::client::Context> {
        let mut attempts = 0;

        loop {
            attempts += 1;
            match self.rt.block_on(
                tcp::connect(self.address)
            ) {
                Ok(ctx) => {
                    info!(
                        "Modbus '{}' connected to {} \
                         (attempt {})",
                        self.name, self.address, attempts
                    );
                    return Ok(ctx);
                }
                Err(e) => {
                    warn!(
                        "Modbus '{}' connection failed \
                         (attempt {}): {}",
                        self.name, attempts, e
                    );
                    std::thread::sleep(
                        std::time::Duration::from_secs(1)
                    );
                }
            }
        }
    }
}

// ------------------------------------
// Register encoding
// f32 packed into two u16 registers
// big endian — standard Modbus convention
// ------------------------------------

fn float_to_regs(value: f32) -> (u16, u16) {
    let bits = value.to_bits();
    let hi   = (bits >> 16) as u16;
    let lo   = (bits & 0xFFFF) as u16;
    (hi, lo)
}

fn regs_to_float(hi: u16, lo: u16) -> f32 {
    let bits = ((hi as u32) << 16) | (lo as u32);
    f32::from_bits(bits)
}

// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_float_register_roundtrip() {
        let values = [0.0_f32, 1.0, -1.0, 42.5, 1000.123, f32::MAX];

        for value in values {
            let (hi, lo)  = float_to_regs(value);
            let recovered = regs_to_float(hi, lo);
            assert!(
                (value - recovered).abs() < f32::EPSILON,
                "Roundtrip failed for {}", value
            );
        }
    }
}
