// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/examples/hello_world/slave.rs
//
// Simulated Modbus TCP slave — one holding register
//
// Register 0: coil state  (0 = off, 1 = on)
//
// After COIL_DELAY_SECS seconds the coil activates.
// The bus thread in main.rs polls this every cycle.

use std::{
    future,
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use tokio::net::TcpListener;
use tokio_modbus::{
    prelude::*,
    server::{
        Service,
        tcp::{Server, accept_tcp_connection},
    },
};
use tracing::info;

// port above 1024 — no root required for the example
pub const SLAVE_ADDR: &str = "127.0.0.1:5502";

const COIL_DELAY_SECS: u64 = 2;
const COIL_ON_SECS:    u64 = 3;
const COIL_OFF_SECS:   u64 = 2;

// ------------------------------------
// Simulated coil
// ------------------------------------

#[derive(Clone)]
pub struct SimulatedCoil {
    pub active: Arc<AtomicBool>,
}

impl SimulatedCoil {
    pub fn new() -> Self {
        Self {
            active: Arc::new(AtomicBool::new(false)),
        }
    }
}

// Respond to any ReadHoldingRegisters request with [coil_state]
// All other function codes return zeros — not needed for this example
impl Service for SimulatedCoil {
    type Request = Request<'static>;
    type Response = Response;
    type Error    = io::Error;
    type Future   = future::Ready<Result<Response, io::Error>>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let val = if self.active.load(Ordering::Relaxed) { 1u16 } else { 0u16 };

        let response = match req {
            Request::ReadHoldingRegisters(_addr, count) => {
                Response::ReadHoldingRegisters(vec![val; count as usize])
            }
            _ => {
                // return zeros for anything else
                Response::ReadHoldingRegisters(vec![0])
            }
        };

        future::ready(Ok(response))
    }
}

// ------------------------------------
// Run the slave
// ------------------------------------

pub async fn run(coil: SimulatedCoil) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SLAVE_ADDR).await?;
    info!("Modbus slave listening on {} — coil activates in {}s",
        SLAVE_ADDR,
        COIL_DELAY_SECS,
    );

    // toggle coil: wait COIL_DELAY_SECS, then on/off forever
    let trigger = coil.active.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(COIL_DELAY_SECS)).await;
        loop {
            trigger.store(true, Ordering::Relaxed);
            info!("Coil ON");
            tokio::time::sleep(
                std::time::Duration::from_secs(COIL_ON_SECS)
            ).await;
            trigger.store(false, Ordering::Relaxed);
            info!("Coil OFF");
            tokio::time::sleep(
                std::time::Duration::from_secs(COIL_OFF_SECS)
            ).await;
        }
    });

    let coil = Arc::new(coil);

    let on_connected = |stream, socket_addr| {
        let coil = Arc::clone(&coil);
        async move {
            accept_tcp_connection(stream, socket_addr, move |_addr| {
                Ok(Some(Arc::clone(&coil)))
            })
        }
    };

    Server::new(listener)
        .serve(&on_connected, |_err| {})
        .await?;

    Ok(())
}
