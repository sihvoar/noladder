// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/core/os_server.rs
//
// OS side request handler
// completely decoupled from RT loop
// normal Linux process — blocking, allocation, async all fine
//
// User registers handlers via closures:
//
//   server.on("recipe.load", |payload| {
//       let name = payload.read_str();
//       let recipe = Recipe::load(name)?;
//       let mut result = OsPayload::new();
//       result.write_f32(0, recipe.speed);
//       Ok(result)
//   });
//
//   server.on_async("db.query", |payload| async move {
//       let id  = payload.read_i32(0);
//       let row = db::fetch(id).await?;
//       let mut result = OsPayload::new();
//       result.write_f32(0, row.speed);
//       Ok(result)
//   });

use std::sync::{Arc, Mutex};
use std::collections::HashMap;
use anyhow::Result;
use tracing::{info, warn, error, debug};

use crate::core::mailbox::Mailbox;
use crate::config::loader::Config;

// ------------------------------------
// OsHandler trait
// internal — user never implements this
// framework wraps closures into this
// ------------------------------------

trait OsHandler: Send + Sync {
    // execute — may block, that is fine
    // OS thread, not RT thread
    fn handle(
        &self,
        payload: OsPayload,
    ) -> OsPayload;
}

// ------------------------------------
// Sync closure handler
// for file IO, simple computation etc
// ------------------------------------

struct SyncHandler<F>
where
    F: Fn(OsPayload) -> Result<OsPayload>
       + Send + Sync,
{
    key:     String,
    handler: F,
}

impl<F> OsHandler for SyncHandler<F>
where
    F: Fn(OsPayload) -> Result<OsPayload>
       + Send + Sync,
{
    fn handle(
        &self,
        payload: OsPayload,
    ) -> OsPayload {
        match (self.handler)(payload) {
            Ok(result) => result,
            Err(e) => {
                error!(
                    "OS handler '{}' error: {}",
                    self.key, e
                );
                OsPayload::error(&e.to_string())
            }
        }
    }
}

// ------------------------------------
// Async closure handler
// for DB queries, HTTP calls, MQTT etc
// spawns on tokio runtime
// blocks OS server thread until done
// ------------------------------------

struct AsyncHandler<F, Fut>
where
    F:   Fn(OsPayload) -> Fut
         + Send + Sync + 'static,
    Fut: std::future::Future<
             Output = Result<OsPayload>
         > + Send + 'static,
{
    key:     String,
    handler: Arc<F>,
    rt:      Arc<tokio::runtime::Runtime>,
}

impl<F, Fut> OsHandler for AsyncHandler<F, Fut>
where
    F:   Fn(OsPayload) -> Fut
         + Send + Sync + 'static,
    Fut: std::future::Future<
             Output = Result<OsPayload>
         > + Send + 'static,
{
    fn handle(
        &self,
        payload: OsPayload,
    ) -> OsPayload {
        let handler = self.handler.clone();

        // block on async future
        // os server thread — blocking is fine
        match self.rt.block_on(handler(payload)) {
            Ok(result) => result,
            Err(e) => {
                error!(
                    "Async OS handler '{}' error: {}",
                    self.key, e
                );
                OsPayload::error(&e.to_string())
            }
        }
    }
}

// ------------------------------------
// OsServer
// owns all handlers
// polls mailbox each tick
// dispatches to correct handler
// posts result back
// ------------------------------------

pub struct OsServer {
    mailbox:  Arc<Mutex<Mailbox>>,
    handlers: HashMap<String, Box<dyn OsHandler>>,
    rt:       Arc<tokio::runtime::Runtime>,
}

impl OsServer {
    pub fn new(
        mailbox: Arc<Mutex<Mailbox>>,
    ) -> Result<Self> {
        // one tokio runtime for all async handlers
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("noladder-os")
            .enable_all()
            .build()?;

        Ok(Self {
            mailbox,
            handlers: HashMap::new(),
            rt:       Arc::new(rt),
        })
    }

    // ------------------------------------
    // Register sync handler
    // closure receives OsPayload
    // returns Result<OsPayload>
    // ------------------------------------

    pub fn on<F>(
        &mut self,
        key:     impl Into<String>,
        handler: F,
    ) -> &mut Self
    where
        F: Fn(OsPayload) -> Result<OsPayload>
           + Send + Sync + 'static,
    {
        let key = key.into();

        info!(
            "OS server: registering handler '{}'",
            key
        );

        self.handlers.insert(
            key.clone(),
            Box::new(SyncHandler { key, handler }),
        );

        self
    }

    // ------------------------------------
    // Register async handler
    // for DB queries, HTTP, MQTT etc
    // ------------------------------------

    pub fn on_async<F, Fut>(
        &mut self,
        key:     impl Into<String>,
        handler: F,
    ) -> &mut Self
    where
        F:   Fn(OsPayload) -> Fut
             + Send + Sync + 'static,
        Fut: std::future::Future<
                 Output = Result<OsPayload>
             > + Send + 'static,
    {
        let key = key.into();

        info!(
            "OS server: registering async handler '{}'",
            key
        );

        self.handlers.insert(
            key.clone(),
            Box::new(AsyncHandler {
                key,
                handler: Arc::new(handler),
                rt:      self.rt.clone(),
            }),
        );

        self
    }

    // ------------------------------------
    // Start on dedicated thread
    // called from main after handler registration
    // ------------------------------------

    pub fn start(self) -> anyhow::Result<()> {
        std::thread::Builder::new()
            .name("os-server".to_string())
            .spawn(move || self.run())?;
        Ok(())
    }

    // ------------------------------------
    // Run the OS server loop
    // called from dedicated thread
    // never returns
    // ------------------------------------

    pub fn run(self) -> ! {
        info!(
            "OS server running — {} handlers registered",
            self.handlers.len()
        );

        for key in self.handlers.keys() {
            debug!("  handler: '{}'", key);
        }

        loop {
            // poll mailbox for pending request
            let request = {
                let mut mb = self.mailbox
                    .lock()
                    .unwrap();
                mb.poll_pending()
            };

            match request {
                None => {
                    // nothing pending
                    // sleep briefly to avoid busy loop
                    // 1ms sleep — fine for OS side
                    // RT side never waits for us
                    std::thread::sleep(
                        std::time::Duration::from_millis(1)
                    );
                }

                Some((id, key, raw_payload)) => {
                    debug!(
                        "OS server: request {} \
                         key '{}'",
                        id, key
                    );

                    let payload = OsPayload::from(
                        raw_payload
                    );

                    // find handler
                    let result = match self.handlers
                        .get(&key)
                    {
                        Some(handler) => {
                            // execute — may block
                            handler.handle(payload)
                        }

                        None => {
                            warn!(
                                "OS server: no handler \
                                 for key '{}' — \
                                 registered handlers: {}",
                                key,
                                self.handlers
                                    .keys()
                                    .cloned()
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                            OsPayload::error(
                                &format!(
                                    "no handler for '{}'",
                                    key
                                )
                            )
                        }
                    };

                    // post result back to mailbox
                    {
                        let mut mb = self.mailbox
                            .lock()
                            .unwrap();
                        mb.post_result(
                            id,
                            &result.into_bytes()
                        );
                    }

                    debug!(
                        "OS server: request {} complete",
                        id
                    );
                }
            }
        }
    }
}

// ------------------------------------
// Startup
// called from main.rs
// user provides registration function
// ------------------------------------

pub fn start(
    config:           &Config,
    mailbox:          Arc<Mutex<Mailbox>>,
    register_handlers: impl FnOnce(
        &mut OsServer
    ) -> Result<()>,
) -> Result<()> {

    let mut server = OsServer::new(
        mailbox.clone()
    )?;

    // built-in handlers
    register_builtin_handlers(
        &mut server,
        config,
    );

    // user handlers
    register_handlers(&mut server)?;

    info!(
        "OS server: {} handlers total",
        server.handlers.len()
    );

    // run on dedicated thread
    // completely separate from RT loop
    std::thread::Builder::new()
        .name("os-server".to_string())
        .spawn(move || server.run())?;

    Ok(())
}

// ------------------------------------
// Built-in handlers
// always registered
// user can override by registering
// same key after this call
// ------------------------------------

fn register_builtin_handlers(
    server: &mut OsServer,
    _config: &Config,
) {
    // ping — useful for testing OS bridge
    server.on("ping", |_payload| {
        let mut result = OsPayload::new();
        result.write_str("pong");
        Ok(result)
    });

    // echo — returns payload unchanged
    // useful for testing round trip
    server.on("echo", |payload| {
        Ok(payload)
    });

    // status — returns server uptime and stats
    let start_time = std::time::Instant::now();
    server.on("status", move |_payload| {
        let uptime_secs = start_time
            .elapsed()
            .as_secs();

        let mut result = OsPayload::new();
        result.write_i32(0, uptime_secs as i32);
        Ok(result)
    });
}

// ------------------------------------
// OsPayload
// moved here so os_server.rs is self-contained
// also exported from src/os/payload.rs
// ------------------------------------

// re-export for convenience
pub use crate::os::payload::OsPayload;