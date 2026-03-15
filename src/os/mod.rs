// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/os/mod.rs
//
// OS server side of NoLadder
// Everything here runs as a normal Linux process
// No RT constraints — blocking, allocation, async all fine
//
// User registers handlers via:
//   server.on("key", |payload| { ... })
//   server.on_async("key", |payload| async { ... })
//
// Handlers receive an OsPayload and return an OsPayload
// Framework handles mailbox polling and delivery

pub mod payload;

use std::sync::{Arc, Mutex};
use anyhow::Result;
use tracing::{info, warn, error, debug};

use crate::core::mailbox::Mailbox;
use payload::OsPayload;

// ------------------------------------
// OsHandler trait
// implemented by closure wrappers
// user never implements this directly
// ------------------------------------

pub trait OsHandler: Send + Sync {
    // key or prefix this handler responds to
    // "recipe.load" matches exactly "recipe.load"
    // "recipe" matches "recipe.load", "recipe.save" etc
    fn key(&self) -> &str;

    // execute the request synchronously
    // called from OS server thread
    // blocking is fine here
    fn handle(
        &self,
        key:     &str,
        payload: OsPayload,
    ) -> OsPayload;
}

// ------------------------------------
// Sync closure handler
// wraps a Fn closure as an OsHandler
// ------------------------------------

struct SyncHandler<F>
where
    F: Fn(OsPayload) -> Result<OsPayload>
       + Send + Sync,
{
    key:     &'static str,
    handler: F,
}

impl<F> OsHandler for SyncHandler<F>
where
    F: Fn(OsPayload) -> Result<OsPayload>
       + Send + Sync,
{
    fn key(&self) -> &str {
        self.key
    }

    fn handle(
        &self,
        key:     &str,
        payload: OsPayload,
    ) -> OsPayload {
        match (self.handler)(payload) {
            Ok(result) => result,
            Err(e) => {
                error!(
                    "OS handler '{}' error: {}",
                    key, e
                );
                OsPayload::error(&e.to_string())
            }
        }
    }
}

// ------------------------------------
// Async closure handler
// wraps an async Fn closure as OsHandler
// blocks the OS thread on the future
// OS thread is not RT — blocking is fine
// ------------------------------------

struct AsyncHandler<F, Fut>
where
    F:   Fn(OsPayload) -> Fut
         + Send + Sync + 'static,
    Fut: std::future::Future<
             Output = Result<OsPayload>
         > + Send + 'static,
{
    key:     &'static str,
    handler: Arc<F>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl<F, Fut> OsHandler for AsyncHandler<F, Fut>
where
    F:   Fn(OsPayload) -> Fut
         + Send + Sync + 'static,
    Fut: std::future::Future<
             Output = Result<OsPayload>
         > + Send + 'static,
{
    fn key(&self) -> &str {
        self.key
    }

    fn handle(
        &self,
        key:     &str,
        payload: OsPayload,
    ) -> OsPayload {
        let handler = self.handler.clone();

        match self.runtime.block_on(
            async move { handler(payload).await }
        ) {
            Ok(result) => result,
            Err(e) => {
                error!(
                    "Async OS handler '{}' error: {}",
                    key, e
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
    handlers: Vec<Box<dyn OsHandler>>,
    runtime:  Arc<tokio::runtime::Runtime>,
}

impl OsServer {
    pub fn new(
        mailbox: Arc<Mutex<Mailbox>>,
    ) -> Self {
        // single tokio runtime shared by all
        // async handlers
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("noladder-os")
            .enable_all()
            .build()
            .expect("Could not build OS runtime");

        Self {
            mailbox,
            handlers: Vec::new(),
            runtime:  Arc::new(runtime),
        }
    }

    // ------------------------------------
    // Register sync handler
    //
    // server.on("recipe.load", |payload| {
    //     let name = payload.read_str();
    //     let mut result = OsPayload::new();
    //     result.write_f32(0, load_speed(name));
    //     Ok(result)
    // });
    // ------------------------------------

    pub fn on<F>(
        &mut self,
        key:     &'static str,
        handler: F,
    ) -> &mut Self
    where
        F: Fn(OsPayload) -> Result<OsPayload>
           + Send + Sync + 'static,
    {
        info!(
            "OS server: registered handler '{}'",
            key
        );
        self.handlers.push(Box::new(
            SyncHandler { key, handler }
        ));
        self
    }

    // ------------------------------------
    // Register async handler
    //
    // server.on_async("db.query", |payload| async move {
    //     let id  = payload.read_i32(0);
    //     let row = db::fetch(id).await?;
    //     let mut result = OsPayload::new();
    //     result.write_f32(0, row.speed);
    //     Ok(result)
    // });
    // ------------------------------------

    pub fn on_async<F, Fut>(
        &mut self,
        key:     &'static str,
        handler: F,
    ) -> &mut Self
    where
        F:   Fn(OsPayload) -> Fut
             + Send + Sync + 'static,
        Fut: std::future::Future<
                 Output = Result<OsPayload>
             > + Send + 'static,
    {
        info!(
            "OS server: registered async handler '{}'",
            key
        );
        self.handlers.push(Box::new(
            AsyncHandler {
                key,
                handler: Arc::new(handler),
                runtime: self.runtime.clone(),
            }
        ));
        self
    }

    // ------------------------------------
    // Start OS server on its own thread
    // never returns
    // ------------------------------------

    pub fn start(self) -> Result<()> {
        let handler_count = self.handlers.len();

        std::thread::Builder::new()
            .name("noladder-os-server".to_string())
            .spawn(move || {
                info!(
                    "OS server started — \
                     {} handlers",
                    handler_count
                );
                self.run_loop();
            })?;

        Ok(())
    }

    // ------------------------------------
    // Main poll loop
    // runs on OS server thread
    // polls mailbox for pending requests
    // dispatches to registered handlers
    // posts results back
    // ------------------------------------

    fn run_loop(self) -> ! {
        use std::time::Duration;

        let poll_interval = Duration::from_millis(1);

        loop {
            // poll for pending request
            let request = {
                let mut mb = self.mailbox
                    .lock()
                    .unwrap();
                mb.poll_pending()
            };

            match request {
                None => {
                    // nothing pending
                    // sleep briefly to avoid
                    // burning CPU in tight loop
                    std::thread::sleep(poll_interval);
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

                    // find matching handler
                    // exact match first
                    // then prefix match
                    let handler = self.handlers
                        .iter()
                        .find(|h| h.key() == key)
                        .or_else(|| {
                            self.handlers
                                .iter()
                                .find(|h| {
                                    key.starts_with(
                                        h.key()
                                    )
                                })
                        });

                    let result = match handler {
                        Some(h) => {
                            h.handle(&key, payload)
                        }
                        None => {
                            warn!(
                                "OS server: no handler \
                                 for key '{}' — \
                                 returning empty result",
                                key
                            );
                            OsPayload::empty()
                        }
                    };

                    // post result back to mailbox
                    {
                        let mut mb = self.mailbox
                            .lock()
                            .unwrap();
                        mb.post_result(
                            id,
                            &result.into_bytes(),
                        );
                    }

                    debug!(
                        "OS server: request {} \
                         complete",
                        id
                    );
                }
            }
        }
    }
}


// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::mailbox::Mailbox;

    fn make_mailbox() -> Arc<Mutex<Mailbox>> {
        Arc::new(Mutex::new(Mailbox::new()))
    }

    #[test]
    fn test_sync_handler_registered() {
        let mailbox = make_mailbox();
        let mut server = OsServer::new(
            mailbox.clone()
        );

        server.on("test.key", |payload| {
            let mut result = OsPayload::new();
            result.write_f32(0, 42.0);
            Ok(result)
        });

        assert_eq!(server.handlers.len(), 1);
        assert_eq!(
            server.handlers[0].key(),
            "test.key"
        );
    }

    #[test]
    fn test_handler_dispatch() {
        let mailbox    = make_mailbox();
        let mut server = OsServer::new(
            mailbox.clone()
        );

        server.on("echo", |payload| {
            // echo back first float
            let val = payload.read_f32(0);
            let mut result = OsPayload::new();
            result.write_f32(0, val * 2.0);
            Ok(result)
        });

        // build a payload
        let mut payload = OsPayload::new();
        payload.write_f32(0, 21.0);

        // dispatch directly
        let result = server.handlers[0].handle(
            "echo",
            payload,
        );

        assert!(
            (result.read_f32(0) - 42.0).abs() < 0.001
        );
    }

    #[test]
    fn test_handler_error_returns_error_payload() {
        let mailbox    = make_mailbox();
        let mut server = OsServer::new(
            mailbox.clone()
        );

        server.on("will_fail", |_| {
            Err(anyhow::anyhow!("expected error"))
        });

        let result = server.handlers[0].handle(
            "will_fail",
            OsPayload::new(),
        );

        // error flag set in result
        assert!(result.is_error());
    }

    #[test]
    fn test_prefix_match() {
        let mailbox    = make_mailbox();
        let mut server = OsServer::new(
            mailbox.clone()
        );

        server.on("recipe", |payload| {
            Ok(OsPayload::new())
        });

        // "recipe.load" should match "recipe" prefix
        let handler = server.handlers
            .iter()
            .find(|h| {
                "recipe.load".starts_with(h.key())
            });

        assert!(handler.is_some());
    }

    #[test]
    fn test_no_handler_returns_empty() {
        // simulate dispatch with no match
        let handlers: Vec<Box<dyn OsHandler>> = vec![];

        let result = handlers
            .iter()
            .find(|h| h.key() == "unknown.key");

        assert!(result.is_none());
        // in real server — returns OsPayload::empty()
    }

    #[test]
    fn test_async_handler_registered() {
        let mailbox    = make_mailbox();
        let mut server = OsServer::new(
            mailbox.clone()
        );

        server.on_async("async.test", |payload| {
            async move {
                Ok(OsPayload::new())
            }
        });

        assert_eq!(server.handlers.len(), 1);
        assert_eq!(
            server.handlers[0].key(),
            "async.test"
        );
    }
}