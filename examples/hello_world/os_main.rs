// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/examples/hello_world/os_main.rs
//
// hello_world OS process
//
// Owns the mailbox shared memory.  The control process posts
// requests here; this process handles them and writes results
// back.  Blocking, heavy computation, external calls — all fine
// here because this is completely isolated from the RT loop.
//
// Start this before the control process:
//   cargo run --example hello_world_os

use std::time::Duration;

use tracing::info;

use noladder::{
    SHM_MB_PATH,
    core::{
        mailbox::PAYLOAD_SIZE,
        shared_memory::SharedMailbox,
    },
};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("─────────────────────────────────");
    info!("  hello_world OS process");
    info!("  Mailbox → {}", SHM_MB_PATH);
    info!("─────────────────────────────────");

    // ------------------------------------
    // Create and own the mailbox
    // control process will open this
    // ------------------------------------

    let mut shm = SharedMailbox::create(SHM_MB_PATH)?;

    info!("OS process ready — waiting for requests");

    // ------------------------------------
    // Request loop
    // Add handlers here as the program grows.
    // Blocking is fine — this is not the RT thread.
    // ------------------------------------

    loop {
        let mailbox = shm.get_mut();

        if let Some((id, key, _payload)) = mailbox.poll_pending() {
            handle(mailbox, id, &key);
        }

        std::thread::sleep(Duration::from_millis(1));
    }
}

fn handle(
    mailbox: &mut noladder::core::mailbox::Mailbox,
    id:      u32,
    key:     &str,
) {
    match key {
        "hello" => {
            // In a real OS process this could be:
            //   - ML inference
            //   - pattern recognition
            //   - database query
            //   - HTTP request
            //   - recipe loading
            // All safe here — completely isolated from RT loop.
            println!("Hello World!");
            mailbox.post_result(id, &[0u8; PAYLOAD_SIZE]);
        }
        other => {
            tracing::warn!("OS process: unknown key '{}'", other);
            mailbox.post_result(id, &[0u8; PAYLOAD_SIZE]);
        }
    }
}
