// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/examples/hello_world/main.rs
//
// hello_world control process
//
// The RT loop — reads IO from shared memory, runs rungs,
// posts OS requests to shared memory.  No hardware.  No
// blocking.  Does not know or care what the OS process does.
//
// Start order:
//   cargo run --example hello_world_bus   # terminal 1
//   cargo run --example hello_world_os    # terminal 2
//   cargo run --example hello_world       # terminal 3

use tracing::info;

use noladder::{
    rung,
    core::{
        io_image::{InputIndex, Value},
        arena::Arena,
        cycle,
        shared_memory::{SharedIOImage, SharedMailbox},
    },
    config::loader,
};

const SHM_IO:  &str = "/dev/shm/noladder_hello_io";
const SHM_MB:  &str = "/dev/shm/noladder_hello_mb";
const COIL:    InputIndex = InputIndex(0);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("─────────────────────────────────");
    info!("  hello_world control process");
    info!("  IO      ← {}", SHM_IO);
    info!("  Mailbox → {}", SHM_MB);
    info!("─────────────────────────────────");

    let config = loader::load("examples/hello_world/machine.toml")?;

    // ------------------------------------
    // Open shared memory created by the
    // bus and OS processes
    // ------------------------------------

    let mut io_shm = SharedIOImage::open(SHM_IO)?;
    let mut mb_shm = SharedMailbox::open(SHM_MB)?;

    // ------------------------------------
    // Rungs
    // ------------------------------------

    let mut arena = Arena::new();

    arena.add(rung!(hello_world, |ctx| {
        loop {
            // suspend until bus process sets coil active
            ctx.yield_until(COIL, Value::Bool(true)).await;

            info!("Coil active — posting to OS process");
            ctx.os_request("hello", b"").await;
            info!("OS response received — waiting for coil reset");

            // wait for coil to go inactive before looping
            ctx.yield_until(COIL, Value::Bool(false)).await;
        }
    }));

    // ------------------------------------
    // RT control loop
    // ------------------------------------

    cycle::run(
        &config,
        io_shm.get_mut(),
        &mut arena,
        mb_shm.get_mut(),
    )?;

    Ok(())
}
