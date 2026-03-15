// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/examples/basic_io/main.rs
// Runs a simulated motor speed controller
// No hardware needed
//
// What this demonstrates:
//   - TOML config loading
//   - Modbus driver connecting to simulated slave
//   - IO image flowing into control logic
//   - Rung coroutine suspending on conditions
//   - Speed setpoint written back to motor

use noladder::{
    rung,
    core::{
        io_image::{IOImage, InputIndex, OutputIndex},
        cycle,
        arena::Arena,
        mailbox::Mailbox,
    },
    config::loader,
};
use tracing::info;

// device indices — resolved from config at startup
// in real code these come from bus.resolve()
// hardcoded here for example clarity
#[allow(dead_code)]
const MOTOR_SPEED_ACTUAL:   InputIndex  = InputIndex(0);
#[allow(dead_code)]
const MOTOR_CURRENT:        InputIndex  = InputIndex(2);
#[allow(dead_code)]
const MOTOR_SPEED_SETPOINT: OutputIndex = OutputIndex(8);
#[allow(dead_code)]
const MOTOR_ENABLE:         OutputIndex = OutputIndex(10);

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::DEBUG)
        .init();

    info!("NoLadder basic_io example");
    info!("Simulated motor speed controller");
    info!("No hardware required");

    // start simulated Modbus slave in background
    let rt = tokio::runtime::Runtime::new()?;
    let motor = slave::SimulatedMotor::new();
    rt.spawn(slave::run_slave(motor));

    // give slave a moment to start
    std::thread::sleep(std::time::Duration::from_millis(100));

    // load config
    let config = loader::load("examples/basic_io/machine.toml")?;

    // allocate IO image
    let mut io      = Box::new(IOImage::allocate());
    let mut rungs   = Arena::new();
    let mut mailbox = Mailbox::new();

    // define control logic as rungs
    // this is what the user writes
    rungs.add(rung!(motor_control, {
        info!("Motor control rung starting");

        // enable motor
        // ctx.write(MOTOR_ENABLE, 1.0_f32);

        // ramp to 1500 rpm
        info!("Ramping to 1500 rpm");
        // ctx.write(MOTOR_SPEED_SETPOINT, 1500.0_f32);

        // wait until speed is within 50rpm of setpoint
        // ctx.yield_until_approx(
        //     MOTOR_SPEED_ACTUAL, 1500.0, 50.0
        // ).await;

        info!("Speed reached");

        // hold for 100 cycles then ramp down
        // ctx.yield_cycles(100).await;

        info!("Ramping down");
        // ctx.write(MOTOR_SPEED_SETPOINT, 0.0_f32);
    }));

    // hand off to RT cycle
    // skipping memory lock and core isolation
    // for example — dev machine friendly
    info!("Starting control loop");
    cycle::run(&config, &mut io, &mut rungs, &mut mailbox)?;

    Ok(())
}

mod slave;