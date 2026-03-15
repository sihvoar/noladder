// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// examples/hello_world/main.rs
//
// hello_world control process
//
// The RT loop — reads IO from shared memory, runs rungs,
// posts OS requests to shared memory.  No hardware.  No
// blocking.  Does not know or care what the OS process does.
//
// Start order:
//   Terminal 1: python3 tools/noladder_mock_bus.py examples/hello_world/machine.toml
//   Terminal 2: cargo run --bin noladder-bus  -- examples/hello_world/machine.toml
//   Terminal 3: cargo run --example hello_world_os
//   Terminal 4: cargo run --example hello_world
//   Terminal 5: python3 tools/noladder_monitor.py examples/hello_world/machine.toml

use tracing::info;

use noladder::{
    rung,
    core::{
        io_image::{InputIndex, OutputIndex, Value},
        arena::Arena,
        cycle,
        shared_memory::{SharedIOImage, SharedMailbox, SHM_IO_PATH, SHM_MB_PATH},
    },
    config::loader,
    DeviceMap,
};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("─────────────────────────────────");
    info!("  hello_world control process");
    info!("  IO      ← {}", SHM_IO_PATH);
    info!("  Mailbox → {}", SHM_MB_PATH);
    info!("─────────────────────────────────");

    let config = loader::load("examples/hello_world/machine.toml")?;

    // ------------------------------------
    // Resolve signal paths to IO indices
    // Panics at startup if machine.toml is wrong —
    // never panics at runtime
    // ------------------------------------

    let map = DeviceMap::build(&config);

    let level_idx    = InputIndex(map.input("demo.sensors.0"));
    let speed_idx    = InputIndex(map.input("demo.pump.speed"));
    let current_idx  = InputIndex(map.input("demo.pump.current"));
    let setpoint_idx = OutputIndex(map.output("demo.pump.setpoint"));
    let enable_idx   = OutputIndex(map.output("demo.pump.enable"));

    // ------------------------------------
    // Open shared memory created by the
    // bus and OS processes
    // ------------------------------------

    let mut io_shm = SharedIOImage::open(SHM_IO_PATH)?;
    let mut mb_shm = SharedMailbox::open(SHM_MB_PATH)?;

    // ------------------------------------
    // Rungs
    // ------------------------------------

    let mut arena = Arena::new();

    // Pump control: reads level + speed, writes setpoint + enable, logs every second
    arena.add(rung!(pump_control, |ctx| {
        loop {
            let level   = ctx.read_float(level_idx);
            let speed   = ctx.read_float(speed_idx);
            let current = ctx.read_float(current_idx);

            // simple level control: run pump when tank level > 30 %
            let setpoint = if level > 30.0 { 1500.0_f32 } else { 0.0_f32 };
            ctx.write(setpoint_idx, Value::Float(setpoint));
            ctx.write(enable_idx,   Value::Bool(level > 30.0));

            info!(
                "level {:.0}%  speed {:.0} rpm  current {:.1} A",
                level, speed, current
            );

            ctx.yield_ms(1000).await;
        }
    }));

    // Hello World: continuous demonstration of OS requests
    arena.add(rung!(hello_world, |ctx| {
        loop {
            ctx.os_request("hello", b"pump_controller").await;
            ctx.yield_ms(2000).await;  // repeat every 2 seconds
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
