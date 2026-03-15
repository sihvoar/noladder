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
    SHM_PATH,
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

    // create shared memory region
    // control loop will open this
    let mut shm = SharedIOImage::create(SHM_PATH)?;

    info!(
        "Shared memory ready at {} — \
         waiting for control loop",
        SHM_PATH
    );

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
    // remove shared memory file
    let _ = std::fs::remove_file(SHM_PATH);
    std::process::exit(0);
}