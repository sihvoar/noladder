// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/examples/basic_io/slave.rs
// A simple Modbus TCP slave
// simulates a motor with speed feedback
// runs locally so the example needs no hardware

use std::{future, io, sync::{Arc, Mutex}};

use tokio::net::TcpListener;
use tokio_modbus::{
    prelude::*,
    server::{
        Service,
        tcp::{Server, accept_tcp_connection},
    },
};
use tracing::info;

pub const SLAVE_ADDR: &str = "127.0.0.1:502";

// Register layout matches ModbusDriver convention:
//   regs 0-1:   actual speed    (f32, big-endian two u16)
//   regs 2-3:   actual current  (f32, big-endian two u16)
//   regs 8-9:   speed setpoint  (f32, big-endian two u16)
//   regs 10-11: enable          (f32, 0.0 or 1.0)

#[derive(Default, Clone)]
pub struct SimulatedMotor {
    registers: Arc<Mutex<[u16; 32]>>,
}

impl SimulatedMotor {
    pub fn new() -> Self {
        Self::default()
    }

    // simulate motor physics each tick
    // actual speed ramps toward setpoint at 10 rpm/tick
    pub fn tick(&self) {
        let mut regs = self.registers.lock().unwrap();

        let setpoint  = regs_to_float(regs[8],  regs[9]);
        let enable    = regs[10] > 0;
        let actual    = regs_to_float(regs[0],  regs[1]);

        let target    = if enable { setpoint } else { 0.0 };
        let new_speed = if (target - actual).abs() < 10.0 {
            target
        } else if target > actual {
            actual + 10.0
        } else {
            actual - 10.0
        };

        let (hi, lo) = float_to_regs(new_speed);
        regs[0] = hi;
        regs[1] = lo;

        let current  = new_speed * 0.1;
        let (hi, lo) = float_to_regs(current);
        regs[2] = hi;
        regs[3] = lo;
    }
}

// ------------------------------------
// Modbus service — handles register reads and writes
// ------------------------------------

impl Service for SimulatedMotor {
    type Request = Request<'static>;
    type Response = Response;
    type Error    = io::Error;
    type Future   = future::Ready<Result<Response, io::Error>>;

    fn call(&self, req: Self::Request) -> Self::Future {
        let regs = self.registers.lock().unwrap();

        let response = match req {
            Request::ReadHoldingRegisters(addr, count) => {
                let addr  = addr as usize;
                let count = count as usize;
                let end   = (addr + count).min(regs.len());
                let data  = regs[addr..end].to_vec();
                Response::ReadHoldingRegisters(data)
            }
            Request::WriteMultipleRegisters(addr, values) => {
                drop(regs);
                let mut regs = self.registers.lock().unwrap();
                let addr = addr as usize;
                for (i, &v) in values.iter().enumerate() {
                    if addr + i < regs.len() {
                        regs[addr + i] = v;
                    }
                }
                Response::WriteMultipleRegisters(addr as u16, values.len() as u16)
            }
            _ => Response::ReadHoldingRegisters(vec![0]),
        };

        future::ready(Ok(response))
    }
}

// ------------------------------------
// Run the slave server
// ------------------------------------

pub async fn run_slave(motor: SimulatedMotor) -> anyhow::Result<()> {
    let listener = TcpListener::bind(SLAVE_ADDR).await?;
    info!("Simulated Modbus slave on {} — motor physics at 100ms ticks", SLAVE_ADDR);

    // tick motor physics every 100ms
    let tick_motor = motor.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(
            std::time::Duration::from_millis(100)
        );
        loop {
            interval.tick().await;
            tick_motor.tick();
        }
    });

    let motor = Arc::new(motor);

    let on_connected = |stream, socket_addr| {
        let motor = Arc::clone(&motor);
        async move {
            accept_tcp_connection(stream, socket_addr, move |_addr| {
                Ok(Some(Arc::clone(&motor)))
            })
        }
    };

    Server::new(listener)
        .serve(&on_connected, |_err| {})
        .await?;

    Ok(())
}

// ------------------------------------
// Register encoding helpers
// ------------------------------------

pub fn float_to_regs(value: f32) -> (u16, u16) {
    let bits = value.to_bits();
    ((bits >> 16) as u16, (bits & 0xFFFF) as u16)
}

pub fn regs_to_float(hi: u16, lo: u16) -> f32 {
    f32::from_bits(((hi as u32) << 16) | lo as u32)
}
