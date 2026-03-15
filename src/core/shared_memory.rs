// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/core/shared_memory.rs
//
// Shared memory backed IO image
// Two separate processes map the same physical memory
// Bus server creates, control loop opens
// No sockets, no serialization, no copying
// Synchronized via sequence counter in IOImage

use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use memmap2::MmapMut;
use anyhow::{Result, Context};
use tracing::{info, debug};

use crate::core::io_image::IOImage;
use crate::core::mailbox::Mailbox;

// shared memory paths
pub const SHM_IO_PATH:  &str = "/dev/shm/noladder_io";
pub const SHM_MB_PATH:  &str = "/dev/shm/noladder_mb";

// keep old name for compatibility
pub const SHM_PATH: &str = SHM_IO_PATH;

// ------------------------------------
// SharedIOImage
// wraps a memory mapped file
// containing an IOImage
// ------------------------------------

pub struct SharedIOImage {
    // keeps mapping alive
    // dropped when SharedIOImage drops
    _mmap: MmapMut,

    // raw pointer into mmap
    // valid as long as _mmap is alive
    inner: *mut IOImage,

    // path for diagnostics
    path: PathBuf,
}

// SharedIOImage is Send — we manage
// synchronization via AtomicU32 in IOImage
unsafe impl Send for SharedIOImage {}
unsafe impl Sync for SharedIOImage {}

impl SharedIOImage {

    // ------------------------------------
    // Bus server side
    // creates and owns the shared memory region
    // must be called before control loop starts
    // ------------------------------------

    pub fn create(path: &str) -> Result<Self> {
        let path_buf = PathBuf::from(path);

        debug!(
            "Creating shared memory at {}",
            path
        );

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!(
                "Could not create shared memory \
                 file '{}' — check permissions \
                 on /dev/shm",
                path
            ))?;

        // size the file to hold one IOImage
        let size = std::mem::size_of::<IOImage>();
        file.set_len(size as u64)
            .context("Could not size shared memory")?;

        let mut mmap = unsafe {
            MmapMut::map_mut(&file)
                .context("Could not mmap shared memory")?
        };

        // zero initialize
        // ensures no garbage in unset slots
        mmap.fill(0);

        // lock into RAM
        // same guarantee as mlockall but
        // specifically for this region
        mmap.lock()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "Could not lock shared memory: {} — \
                     page faults possible",
                    e
                )
            });

        let inner = mmap.as_mut_ptr() as *mut IOImage;

        info!(
            "Shared memory created — {} bytes at {}",
            size, path
        );

        Ok(Self {
            _mmap: mmap,
            inner,
            path: path_buf,
        })
    }

    // ------------------------------------
    // Control loop side
    // opens existing shared memory region
    // bus server must be running first
    // ------------------------------------

    pub fn open(path: &str) -> Result<Self> {
        let path_buf = PathBuf::from(path);

        // wait for bus server to create the file
        // with timeout
        Self::wait_for_file(path, 5000)?;

        debug!("Opening shared memory at {}", path);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!(
                "Could not open shared memory '{}' — \
                 is noladder-bus running?",
                path
            ))?;

        // verify size matches our IOImage
        let expected = std::mem::size_of::<IOImage>() as u64;
        let actual   = file.metadata()?.len();

        if actual != expected {
            anyhow::bail!(
                "Shared memory size mismatch — \
                 expected {} bytes got {} bytes — \
                 bus server version mismatch?",
                expected, actual
            );
        }

        let mmap = unsafe {
            MmapMut::map_mut(&file)
                .context("Could not mmap shared memory")?
        };

        mmap.lock()
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "Could not lock shared memory: {}",
                    e
                )
            });

        let inner = mmap.as_ptr() as *mut IOImage;

        info!(
            "Shared memory opened — {} bytes at {}",
            actual, path
        );

        Ok(Self {
            _mmap: mmap,
            inner,
            path: path_buf,
        })
    }

    // ------------------------------------
    // Access
    // ------------------------------------

    pub fn get(&self) -> &IOImage {
        unsafe { &*self.inner }
    }

    pub fn get_mut(&mut self) -> &mut IOImage {
        unsafe { &mut *self.inner }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    // ------------------------------------
    // Wait for bus server to create
    // shared memory file
    // ------------------------------------

    fn wait_for_file(
        path:       &str,
        timeout_ms: u64,
    ) -> Result<()> {
        use std::time::{Duration, Instant};

        let deadline = Instant::now()
            + Duration::from_millis(timeout_ms);

        while Instant::now() < deadline {
            if Path::new(path).exists() {
                return Ok(());
            }
            std::thread::sleep(
                Duration::from_millis(100)
            );
            debug!(
                "Waiting for shared memory at {}...",
                path
            );
        }

        anyhow::bail!(
            "Timeout waiting for shared memory '{}' \
             after {}ms — is noladder-bus running?",
            path, timeout_ms
        )
    }
}

impl Drop for SharedIOImage {
    fn drop(&mut self) {
        debug!(
            "Shared memory unmapped: {}",
            self.path.display()
        );
        // _mmap drops here — unmaps automatically
        // file remains on disk until explicitly deleted
        // bus server should clean up on exit
    }
}

// ------------------------------------
// SharedMailbox
// same mmap pattern as SharedIOImage
// OS server creates, control loop opens
// Mailbox is #[repr(C)] and lock-free —
// safe to share across process boundaries
// ------------------------------------

pub struct SharedMailbox {
    // keeps mapping alive
    _mmap: MmapMut,

    // raw pointer into mmap
    inner: *mut Mailbox,

    path: PathBuf,
}

unsafe impl Send for SharedMailbox {}
unsafe impl Sync for SharedMailbox {}

impl SharedMailbox {

    // ------------------------------------
    // OS server side — creates and owns
    // ------------------------------------

    pub fn create(path: &str) -> Result<Self> {
        let path_buf = PathBuf::from(path);

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!(
                "Could not create shared mailbox \
                 '{}' — check permissions on /dev/shm",
                path
            ))?;

        let size = std::mem::size_of::<Mailbox>();
        file.set_len(size as u64)
            .context("Could not size shared mailbox")?;

        let mut mmap = unsafe {
            MmapMut::map_mut(&file)
                .context("Could not mmap shared mailbox")?
        };

        mmap.fill(0);

        mmap.lock().unwrap_or_else(|e| {
            tracing::warn!(
                "Could not lock shared mailbox: {}",
                e
            )
        });

        // initialize Mailbox in place
        // Mailbox::new() sets next_id = 1 and zeros slots
        // fill(0) above is not enough because next_id starts at 1
        let inner = mmap.as_mut_ptr() as *mut Mailbox;
        unsafe { inner.write(Mailbox::new()); }

        info!(
            "Shared mailbox created — {} bytes at {}",
            size, path
        );

        Ok(Self { _mmap: mmap, inner, path: path_buf })
    }

    // ------------------------------------
    // Control loop side — opens existing
    // ------------------------------------

    pub fn open(path: &str) -> Result<Self> {
        let path_buf = PathBuf::from(path);

        Self::wait_for_file(path, 5000)?;

        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)
            .with_context(|| format!(
                "Could not open shared mailbox '{}' — \
                 is noladder-os running?",
                path
            ))?;

        let expected = std::mem::size_of::<Mailbox>() as u64;
        let actual   = file.metadata()?.len();

        if actual != expected {
            anyhow::bail!(
                "Shared mailbox size mismatch — \
                 expected {} bytes got {} — \
                 version mismatch?",
                expected, actual
            );
        }

        let mmap = unsafe {
            MmapMut::map_mut(&file)
                .context("Could not mmap shared mailbox")?
        };

        mmap.lock().unwrap_or_else(|e| {
            tracing::warn!(
                "Could not lock shared mailbox: {}",
                e
            )
        });

        let inner = mmap.as_ptr() as *mut Mailbox;

        info!(
            "Shared mailbox opened — {} bytes at {}",
            actual, path
        );

        Ok(Self { _mmap: mmap, inner, path: path_buf })
    }

    pub fn get(&self) -> &Mailbox {
        unsafe { &*self.inner }
    }

    pub fn get_mut(&mut self) -> &mut Mailbox {
        unsafe { &mut *self.inner }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn wait_for_file(path: &str, timeout_ms: u64) -> Result<()> {
        use std::time::{Duration, Instant};

        let deadline = Instant::now()
            + Duration::from_millis(timeout_ms);

        while Instant::now() < deadline {
            if Path::new(path).exists() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
            debug!("Waiting for shared mailbox at {}...", path);
        }

        anyhow::bail!(
            "Timeout waiting for shared mailbox '{}' \
             after {}ms — is noladder-os running?",
            path, timeout_ms
        )
    }
}

impl Drop for SharedMailbox {
    fn drop(&mut self) {
        debug!(
            "Shared mailbox unmapped: {}",
            self.path.display()
        );
    }
}