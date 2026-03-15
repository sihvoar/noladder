// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/bin/noladder_bus.rs
//
// NoLadder Bus Server
// Separate process from control loop
// Creates shared memory IO image
// Runs all configured bus drivers
// Each driver on its own thread at its own cycle rate
//
// Start this BEFORE noladder control loop
//
// Usage:
//   noladder-bus machine.toml

use anyhow::Result;
use tracing::{info, warn, error};

use noladder::config::loader;
use noladder::core::shared_memory::{
    SharedIOImage,
    SharedSymbolTable,
    Symbol,
    SHM_PATH,
    SYMBOLS_PATH,
    MAX_SYMBOLS,
};
use noladder::bus;

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("NoLadder Bus Server v{}",
        env!("CARGO_PKG_VERSION")
    );

    // config path from args or default
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "machine.toml".to_string());

    info!("Loading config from '{}'", config_path);

    let config = loader::load(&config_path)?;

    info!(
        "Config loaded — {} buses {} devices",
        config.buses.len(),
        config.devices.len(),
    );

    // clean up any stale shared memory
    // from previous run
    cleanup_shm(SHM_PATH);
    cleanup_shm(SYMBOLS_PATH);

    // create shared memory region
    // control loop will open this
    let mut shm = SharedIOImage::create(SHM_PATH)?;

    info!(
        "Shared memory ready at {} — \
         waiting for control loop",
        SHM_PATH
    );

    // write symbol table so monitor / tools
    // are self-describing without machine.toml
    write_symbol_table(&config)?;

    // register signal handler
    // clean up shared memory on exit
    setup_signal_handler(SHM_PATH);

    // start all bus drivers
    // each on its own thread
    // all write to shared IO image
    //
    // SAFETY: shm is forgotten below and lives for
    // the entire process (bus server never exits).
    // Bus threads access non-overlapping index ranges.
    let io: &'static mut noladder::core::io_image::IOImage =
        unsafe { &mut *(shm.get_mut() as *mut _) };

    let handles = bus::start_all(
        &config,
        io,
    )?;

    // shm must not be dropped — bus threads hold
    // a 'static reference into its mmap
    std::mem::forget(shm);

    info!(
        "All bus drivers started — \
         {} buses running",
        handles.len(),
    );

    // bus server runs forever
    // bus driver threads own their loops
    // this thread monitors driver health
    monitor_loop(handles);
}

fn monitor_loop(
    handles: Vec<(String, std::thread::JoinHandle<()>)>,
) -> ! {
    use std::time::Duration;

    // check every 10 seconds
    let check_interval = Duration::from_secs(10);

    loop {
        std::thread::sleep(check_interval);

        let mut dead = 0usize;
        for (name, handle) in &handles {
            if handle.is_finished() {
                dead += 1;
                error!(
                    "Bus thread '{}' has exited — \
                     restart noladder-bus to recover",
                    name
                );
            }
        }

        if dead == 0 {
            info!(
                "Bus server healthy — \
                 {}/{} threads running",
                handles.len(),
                handles.len(),
            );
        } else {
            error!(
                "{} of {} bus threads dead — \
                 bus server needs restart",
                dead,
                handles.len(),
            );
        }
    }
}

fn cleanup_shm(path: &str) {
    if std::path::Path::new(path).exists() {
        match std::fs::remove_file(path) {
            Ok(_)  => {
                tracing::debug!(
                    "Cleaned up stale shared memory: {}",
                    path
                )
            }
            Err(e) => {
                warn!(
                    "Could not clean up {}: {}",
                    path, e
                )
            }
        }
    }
}

fn setup_signal_handler(_path: &'static str) {
    // clean up shared memory on SIGTERM / SIGINT
    // so control loop gets clear error on restart
    unsafe {
        libc::signal(
            libc::SIGTERM,
            handle_signal as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGINT,
            handle_signal as *const () as libc::sighandler_t,
        );
    }
}

extern "C" fn handle_signal(_: libc::c_int) {
    // remove shared memory files
    let _ = std::fs::remove_file(SHM_PATH);
    let _ = std::fs::remove_file(SYMBOLS_PATH);
    std::process::exit(0);
}

// ------------------------------------
// Symbol table
// ------------------------------------

fn write_symbol_table(
    config: &loader::Config,
) -> Result<()> {
    let mut shm = SharedSymbolTable::create(SYMBOLS_PATH)?;
    let table   = shm.get_mut();

    let mut count = 0usize;

    for device in &config.devices {
        for (offset, sig) in
            kind_input_signals(&device.kind).iter().enumerate()
        {
            if count >= MAX_SYMBOLS { break; }
            let name = if sig.is_empty() {
                device.path.clone()
            } else {
                format!("{}.{}", device.path, sig)
            };
            fill_symbol(
                &mut table.symbols[count],
                (device.input_base + offset) as u32,
                0, // kind: input
                &name,
            );
            count += 1;
        }

        for (offset, sig) in
            kind_output_signals(&device.kind).iter().enumerate()
        {
            if count >= MAX_SYMBOLS { break; }
            let name = if sig.is_empty() {
                device.path.clone()
            } else {
                format!("{}.{}", device.path, sig)
            };
            fill_symbol(
                &mut table.symbols[count],
                (device.output_base + offset) as u32,
                1, // kind: output
                &name,
            );
            count += 1;
        }
    }

    table.count = count as u32;

    // keep mmap alive for process lifetime —
    // same pattern as SharedIOImage
    std::mem::forget(shm);

    info!(
        "Symbol table written — {} symbols at {}",
        count, SYMBOLS_PATH
    );

    Ok(())
}

fn fill_symbol(
    sym:   &mut Symbol,
    index: u32,
    kind:  u8,
    name:  &str,
) {
    sym.index      = index;
    sym.kind       = kind;
    sym.value_type = 0; // determined at runtime
    let bytes      = name.as_bytes();
    let len        = bytes.len().min(64);
    sym.name_len   = len as u8;
    sym._pad       = 0;
    sym.name[..len].copy_from_slice(&bytes[..len]);
}

fn kind_input_signals(
    kind: &loader::DeviceKind,
) -> &'static [&'static str] {
    match kind {
        loader::DeviceKind::ServoDrive  => &[
            "position", "velocity", "torque",
            "following_error", "enabled", "fault",
            "target_reached", "homing_complete",
            "error_code", "referenced",
        ],
        loader::DeviceKind::Vfd         => &["speed", "current"],
        loader::DeviceKind::DigitalIn   => &["0","1","2","3","4","5","6","7"],
        loader::DeviceKind::DigitalOut  => &[],
        loader::DeviceKind::AnalogIn    => &["0","1","2","3"],
        loader::DeviceKind::AnalogOut   => &[],
        loader::DeviceKind::MixedIo     => &["0","1","2","3"],
        loader::DeviceKind::SafetyRelay => &["ok", "fault"],
        loader::DeviceKind::SafetyDoor  => &["closed", "locked"],
        loader::DeviceKind::Flag        => &[""],
    }
}

fn kind_output_signals(
    kind: &loader::DeviceKind,
) -> &'static [&'static str] {
    match kind {
        loader::DeviceKind::ServoDrive  => &[
            "target_position", "target_velocity",
            "target_torque", "max_torque",
            "fault_reset", "quick_stop",
        ],
        loader::DeviceKind::Vfd         => &["setpoint", "enable"],
        loader::DeviceKind::DigitalIn   => &[],
        loader::DeviceKind::DigitalOut  => &["0","1","2","3","4","5","6","7"],
        loader::DeviceKind::AnalogIn    => &[],
        loader::DeviceKind::AnalogOut   => &["0","1","2","3"],
        loader::DeviceKind::MixedIo     => &["0","1","2","3"],
        loader::DeviceKind::SafetyRelay => &["reset"],
        loader::DeviceKind::SafetyDoor  => &[],
        loader::DeviceKind::Flag        => &[""],
    }
}