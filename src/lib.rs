// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/lib.rs

pub mod core;
pub mod bus;
pub mod config;
pub mod os;

// ------------------------------------
// Public API re-exports
// Users import from here rather than
// reaching into module paths directly
// ------------------------------------

// rung! macro — exported at crate root via #[macro_export]

pub use core::arena::Arena;
pub use core::cycle;
pub use core::io_image::{IOImage, InputIndex, OutputIndex, Value};
pub use core::mailbox::Mailbox;
pub use core::shared_memory::{
    SharedIOImage,
    SharedMailbox,
    SHM_IO_PATH,
    SHM_MB_PATH,
    SHM_PATH,
};
pub use core::os_server::OsServer;

pub use config::loader::{Config, DeviceMap};

pub use os::payload::OsPayload;
