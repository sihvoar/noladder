// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/examples/hello_world/bus_main.rs
//
// hello_world bus process
//
// Owns the simulated Modbus slave and the IO image.
// Writes coil state to shared memory every cycle so the
// control process can read it without touching hardware.
//
// Start this first:
//   cargo run --example hello_world_bus

use std::time::Duration;

use tokio_modbus::prelude::{tcp, Reader as _};
use tracing::info;

use noladder::core::{
    io_image::Value,
    shared_memory::SharedMailbox,
};
use noladder::core::shared_memory::SharedIOImage;

mod slave;

const SHM_IO: &str = "/dev/shm/noladder_hello_io";
const SHM_MB: &str = "/dev/shm/noladder_hello_mb";

// cycle time — matches control process
const CYCLE_MS: u64 = 10;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("─────────────────────────────────");
    info!("  hello_world bus process");
    info!("  IO → {}", SHM_IO);
    info!("─────────────────────────────────");

    // ------------------------------------
    // Simulated Modbus slave
    // runs in its own tokio runtime
    // toggles coil ON/OFF after initial delay
    // ------------------------------------

    let slave_rt = tokio::runtime::Runtime::new()?;
    slave_rt.spawn(slave::run(slave::SimulatedCoil::new()));

    // give slave a moment to bind the port
    std::thread::sleep(Duration::from_millis(100));

    // ------------------------------------
    // Create shared memory IO image
    // control process will open this
    // ------------------------------------

    let mut shm = SharedIOImage::create(SHM_IO)?;

    // wait for OS process too (optional — bus can run standalone)
    // just log if mailbox isn't there yet
    match SharedMailbox::open(SHM_MB) {
        Ok(_)  => info!("OS process shared mailbox found at {}", SHM_MB),
        Err(_) => info!("OS process not running yet — that's fine, start it separately"),
    }

    info!("Bus ready — polling slave every {}ms", CYCLE_MS);

    // ------------------------------------
    // Bus polling loop
    // reads coil from Modbus slave
    // publishes Bool to IO image each cycle
    // ------------------------------------

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    let mut ctx = loop {
        match rt.block_on(tcp::connect(
            slave::SLAVE_ADDR.parse().unwrap()
        )) {
            Ok(c)  => break c,
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    };

    info!("Bus connected to Modbus slave at {}", slave::SLAVE_ADDR);

    loop {
        match rt.block_on(ctx.read_holding_registers(0, 1)) {
            Ok(regs) => {
                let coil = regs.first().map(|&v| v != 0).unwrap_or(false);
                let io = shm.get_mut();
                io.publish_inputs(0, Value::Bool(coil));
                io.signal_ready();
            }
            Err(e) => {
                tracing::warn!("Modbus read error: {e}");
            }
        }
        std::thread::sleep(Duration::from_millis(CYCLE_MS));
    }
}
