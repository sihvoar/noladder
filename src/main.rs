// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/main.rs
//
// NoLadder Control Loop
//
// This binary uses the noladder library modules directly.
// Public items not used by this specific machine are expected.
#![allow(dead_code)]
//
// Opens shared memory created by noladder-bus
// Runs RT control cycle on isolated core
//
// Start noladder-bus FIRST:
//   noladder-bus machine.toml
//
// Then:
//   noladder machine.toml

use anyhow::Result;
use tracing::{info, warn, error};
use std::sync::{Arc, Mutex};

mod core;
mod bus;
mod config;
mod os;

use core::{
    shared_memory::{SharedIOImage, SHM_PATH},
    arena::Arena,
    mailbox::Mailbox,
    os_server::OsServer,
    io_image::{InputIndex, OutputIndex},
};
use config::loader::DeviceMap;
use os::payload::OsPayload;

fn main() -> Result<()> {
    // ------------------------------------
    // Logging — before anything else
    // ------------------------------------
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .init();

    info!("╔═══════════════════════════════════╗");
    info!("║  NoLadder v{}                   ║",
        env!("CARGO_PKG_VERSION")
    );
    info!("║  Industrial control for Linux IPCs║");
    info!("╚═══════════════════════════════════╝");

    // ------------------------------------
    // Config
    // ------------------------------------
    let config_path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| {
            info!(
                "No config path given — \
                 using machine.toml"
            );
            "machine.toml".to_string()
        });

    info!("Loading config: '{}'", config_path);

    let config = config::loader::load(&config_path)
        .map_err(|e| {
            error!("Config error: {}", e);
            e
        })?;

    info!(
        "Config loaded — \
         {} buses  \
         {} devices \
         {}ms cycle",
        config.buses.len(),
        config.devices.len(),
        config.cycle_ms,
    );

    // ------------------------------------
    // Device map
    // resolve all string paths → indices
    // after this — no strings in RT path
    // ------------------------------------
    let device_map = config::loader::DeviceMap::build(
        &config
    );

    info!(
        "Device map built — \
         {} inputs \
         {} outputs",
        config.input_count,
        config.output_count,
    );

    // ------------------------------------
    // Lock memory
    // must happen before RT activity
    // prevents page faults mid-cycle
    // ------------------------------------
    lock_memory()?;

    // ------------------------------------
    // RT core setup
    // isolate and pin to core 1
    // warn if isolcpus not set
    // ------------------------------------
    setup_rt_core()?;

    // ------------------------------------
    // Open shared IO image
    // bus server must already be running
    // waits up to 5 seconds for bus server
    // ------------------------------------
    let mut shm = SharedIOImage::open(SHM_PATH)
        .map_err(|e| {
            error!("{}", e);
            error!(
                "Start bus server first:\n  \
                 noladder-bus {}",
                config_path
            );
            e
        })?;

    info!(
        "Shared IO image opened — \
         bus server connected"
    );

    // ------------------------------------
    // Pre-allocate RT structures
    // no allocation after this point
    // in the RT path
    // ------------------------------------
    let mut arena   = Arena::new();
    let     mailbox = Arc::new(
        Mutex::new(Mailbox::new())
    );

    info!(
        "Memory allocated — \
         IO image {} bytes \
         arena {} slots \
         mailbox {} slots",
        std::mem::size_of::<core::io_image::IOImage>(),
        core::arena::MAX_RUNGS,
        core::mailbox::MAILBOX_SIZE,
    );

    // ------------------------------------
    // Register OS handlers
    // user defines what OS operations do
    // ------------------------------------
    let mut os_server = OsServer::new(
        mailbox.clone()
    )?;

    register_os_handlers(&mut os_server)?;

    // ------------------------------------
    // Register rungs
    // user defines control logic
    // all device paths resolved here
    // panics on bad path — caught at startup
    // ------------------------------------
    register_rungs(
        &mut arena,
        &device_map,
    )?;

    info!(
        "Registered — {} rungs",
        arena.count()
    );
    info!("Arena: {}", arena.stats());

    // ------------------------------------
    // Start OS server thread
    // handles async requests from rungs
    // normal Linux process — blocking ok
    // ------------------------------------
    os_server.start()?;

    info!("OS server started");

    // ------------------------------------
    // Hand off to RT control loop
    // never returns under normal operation
    // ------------------------------------
    info!(
        "Starting RT control loop — \
         {}ms cycle on core 1",
        config.cycle_ms
    );

    core::cycle::run(
        &config,
        shm.get_mut(),
        &mut arena,
        &mut mailbox.lock().unwrap(),
    )?;

    // should never get here
    error!("Control loop exited unexpectedly");
    std::process::exit(1);
}

// ============================================================
// User control logic
//
// This is the ONLY section the machine builder writes.
// Everything above is infrastructure — never touch it.
// ============================================================

// ------------------------------------
// Rung registration
// Define control logic here
// Runs on RT core — no allocation,
// no blocking, no OS calls directly
// ------------------------------------
fn register_rungs(
    arena: &mut Arena,
    map:   &DeviceMap,
) -> Result<()> {
    use crate::rung;
    use core::io_image::Value;
    use os::payload::OsPayload;

    // ------------------------------------
    // Resolve device paths → indices
    // panics here on bad path
    // never at runtime
    // ------------------------------------

    // motor — CiA402 signal names
    let motor_speed    = InputIndex(
        map.input("line1.conveyor.motor.velocity")
    );
    let motor_current  = InputIndex(
        map.input("line1.conveyor.motor.torque")
    );
    let motor_enable   = OutputIndex(
        map.output("line1.conveyor.motor.quick_stop")
    );
    let motor_setpoint = OutputIndex(
        map.output("line1.conveyor.motor.target_velocity")
    );

    // sensor
    let home_sensor    = InputIndex(
        map.input("line1.conveyor.sensor.0")
    );

    // internal coordination flags
    // these are IO image slots used as flags
    // between rungs — not connected to hardware
    // assigned indices above hardware device range
    let homed_flag     = InputIndex(
        map.input("flags.homed")
    );
    let homed_output   = OutputIndex(
        map.output("flags.homed")
    );
    let recipe_speed   = InputIndex(
        map.input("flags.recipe_speed")
    );
    let recipe_speed_output = OutputIndex(
        map.output("flags.recipe_speed")
    );

    // ------------------------------------
    // Safety monitor
    // registered FIRST — runs FIRST each cycle
    // always watching — never completes
    // ------------------------------------
    arena.add(rung!(safety_monitor, |ctx| {
        loop {
            // read current
            let current = ctx.read_float(motor_current);

            // overcurrent — disable immediately
            if current > 15.0 {
                ctx.write(motor_enable,   false);
                ctx.write(motor_setpoint, 0.0_f32);
                tracing::error!(
                    "Overcurrent {:.1}A — \
                     motor disabled",
                    current
                );

                // wait for current to clear
                // operator must reset
                ctx.yield_until(
                    motor_current,
                    Value::Float(0.0)
                ).await;
            }

            ctx.yield_cycles(1).await;
        }
    }));

    // ------------------------------------
    // Recipe loader
    // runs once at startup
    // requests recipe from OS side
    // stores result as flag for other rungs
    // ------------------------------------
    arena.add(rung!(recipe_loader, |ctx| {

        // request recipe from OS server
        let result = ctx.os_request(
            "recipe.load",
            b"default",
        ).await;

        // unpack result
        let speed = OsPayload::from(result)
            .read_f32(0);

        tracing::info!(
            "Recipe loaded — target speed {:.0} rpm",
            speed
        );

        // store in flags for speed_control rung
        ctx.write(recipe_speed_output, speed);
    }));

    // ------------------------------------
    // Homing sequence
    // runs once after recipe loaded
    // ------------------------------------
    arena.add(rung!(homing_sequence, |ctx| {

        // wait for recipe to be loaded first
        ctx.yield_until(
            recipe_speed,
            Value::Unset,
        ).await;

        tracing::info!("Homing sequence started");

        // enable motor at slow speed
        ctx.write(motor_enable,   true);
        ctx.write(motor_setpoint, 50.0_f32);

        // race home sensor against timeout
        // 10 second timeout
        let result = ctx.race(
            ctx.yield_until(home_sensor, true),
            ctx.yield_ms(10_000),
        ).await;

        match result {
            RaceResult::First => {
                tracing::info!("Home sensor triggered");

                // stop motor
                ctx.write(motor_setpoint, 0.0_f32);

                // settle time
                ctx.yield_ms(100).await;

                // signal homed
                ctx.write(homed_output, true);

                tracing::info!("Homing complete");
            }

            RaceResult::Second => {
                // timeout — disable and fault
                ctx.write(motor_enable,   false);
                ctx.write(motor_setpoint, 0.0_f32);

                tracing::error!(
                    "Homing timeout — \
                     check home sensor wiring"
                );

                // publish fault to MQTT
                let mut payload = OsPayload::new();
                payload
                    .write_str_at(0, "machine/fault")
                    .write_str_at(64, "homing_timeout");

                ctx.os_request(
                    "mqtt.publish",
                    &payload.into_bytes(),
                ).await;
            }
        }
    }));

    // ------------------------------------
    // Speed control
    // runs continuously after homing
    // PID loop — runs every cycle
    // ------------------------------------
    arena.add(rung!(speed_control, |ctx| {

        // wait until homed
        ctx.yield_until(homed_flag, true).await;

        tracing::info!("Speed control started");

        // simple proportional controller
        let kp = 0.1_f32;

        loop {
            let actual  = ctx.read_float(motor_speed);
            let target  = ctx.read_float(recipe_speed);
            let error   = target - actual;
            let output  = (actual + kp * error)
                .clamp(0.0, 3000.0);

            ctx.write(motor_setpoint, output);

            ctx.yield_cycles(1).await;
        }
    }));

    // ------------------------------------
    // Telemetry
    // publishes speed to MQTT every second
    // 100 cycles at 10ms
    // ------------------------------------
    arena.add(rung!(telemetry, |ctx| {

        // wait until running
        ctx.yield_until(homed_flag, true).await;

        loop {
            ctx.yield_ms(1000).await;

            let speed   = ctx.read_float(motor_speed);
            let current = ctx.read_float(motor_current);

            let mut payload = OsPayload::new();
            payload
                .write_str_at(0,   "machine/telemetry")
                .write_f32(16,     speed)
                .write_f32(17,     current);

            ctx.os_request(
                "mqtt.publish",
                &payload.into_bytes(),
            ).await;
        }
    }));

    info!(
        "Registered {} rungs",
        arena.count()
    );

    Ok(())
}

// ------------------------------------
// OS handler registration
// Define what OS operations do here
// Runs on OS thread — blocking fine
// ------------------------------------
fn register_os_handlers(
    server: &mut OsServer,
) -> Result<()> {

    // recipe TOML shape:
    //   [recipe]
    //   speed = 1500.0
    #[derive(serde::Deserialize)]
    struct RecipeFile { recipe: RecipeBody }
    #[derive(serde::Deserialize)]
    struct RecipeBody {
        #[serde(default = "default_recipe_speed")]
        speed: f32,
    }
    fn default_recipe_speed() -> f32 { 1500.0 }

    // ------------------------------------
    // Recipe loader
    // loads TOML recipe file
    // returns speed as f32 at slot 0
    // ------------------------------------
    server.on("recipe.load", |payload| {
        let name = payload.read_str();
        let path = format!("recipes/{}.toml", name);

        tracing::info!("Loading recipe: {}", path);

        let content = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| {
                tracing::warn!(
                    "Recipe '{}' not found — \
                     using defaults",
                    path
                );
                "[recipe]\nspeed = 1500.0\n".to_string()
            });

        let parsed: RecipeFile = toml::from_str(&content)
            .unwrap_or_else(|e| {
                tracing::warn!(
                    "Recipe parse error — \
                     using defaults: {}",
                    e
                );
                RecipeFile {
                    recipe: RecipeBody { speed: 1500.0 }
                }
            });

        let mut result = OsPayload::new();
        result.write_f32(0, parsed.recipe.speed);

        Ok(result)
    });

    // ------------------------------------
    // MQTT publish
    // payload layout:
    //   bytes 0-63:   topic string
    //   bytes 64-127: message string
    //   slots 16+:    optional f32 values
    // ------------------------------------
    server.on("mqtt.publish", |payload| {
        let topic   = payload.read_str_at(0);
        let message = payload.read_str_at(64);

        // log the publish — wire in rumqttc here
        // for real MQTT support
        tracing::info!(
            "MQTT → {} : {}",
            topic,
            message
        );

        // also log any float values
        // at slots 16+ if present
        // (telemetry values packed here)
        let speed   = payload.read_f32(16);
        let current = payload.read_f32(17);
        if speed > 0.0 {
            tracing::info!(
                "  speed: {:.1} rpm \
                 current: {:.2} A",
                speed, current
            );
        }

        Ok(OsPayload::empty())
    });

    // ------------------------------------
    // File read
    // returns file contents in payload
    // ------------------------------------
    server.on("file.read", |payload| {
        let filename = payload.read_str();
        let path     = format!("data/{}", filename);

        match std::fs::read(&path) {
            Ok(bytes) => {
                let mut result = OsPayload::new();
                // copy up to PAYLOAD_SIZE bytes
                let data = result.data_mut();
                let len  = bytes.len()
                    .min(data.len());
                data[..len]
                    .copy_from_slice(&bytes[..len]);
                Ok(result)
            }
            Err(e) => {
                tracing::warn!(
                    "file.read '{}': {}",
                    path, e
                );
                Ok(OsPayload::empty())
            }
        }
    });

    // ------------------------------------
    // File write
    // payload layout:
    //   bytes 0-63:  filename
    //   bytes 64+:   content
    // ------------------------------------
    server.on("file.write", |payload| {
        let filename = payload.read_str_at(0);
        let path     = format!("data/{}", filename);
        let content  = payload.read_str_at(64);

        std::fs::write(&path, content)
            .map_err(|e| anyhow::anyhow!(e))?;

        tracing::debug!(
            "file.write: {}",
            path
        );

        Ok(OsPayload::empty())
    });

    info!("OS handlers registered");

    Ok(())
}

// ============================================================
// System setup
// Machine builders do not touch below this line
// ============================================================

fn lock_memory() -> Result<()> {
    #[cfg(target_os = "linux")]
    unsafe {
        let ret = libc::mlockall(
            libc::MCL_CURRENT | libc::MCL_FUTURE
        );
        if ret != 0 {
            anyhow::bail!(
                "mlockall failed — \
                 run with CAP_IPC_LOCK or as root\n  \
                 sudo setcap cap_ipc_lock+ep \
                 ./target/release/noladder"
            );
        }
        info!("Memory locked — no page faults possible");
    }

    #[cfg(not(target_os = "linux"))]
    warn!(
        "Memory locking not available — \
         dev platform, RT performance degraded"
    );

    Ok(())
}

fn setup_rt_core() -> Result<()> {
    #[cfg(target_os = "linux")]
    {
        // check isolcpus
        let cmdline = std::fs::read_to_string(
            "/proc/cmdline"
        )?;

        if cmdline.contains("isolcpus=1") {
            info!(
                "CPU core 1 isolated — \
                 optimal RT performance"
            );
        } else {
            warn!(
                "isolcpus=1 not set — \
                 RT performance degraded\n  \
                 For production add to \
                 /etc/default/grub:\n  \
                 GRUB_CMDLINE_LINUX=\
                 \"isolcpus=1 nohz_full=1 \
                 rcu_nocbs=1\"\n  \
                 Fine for development"
            );
        }

        // pin process to core 1
        unsafe {
            let mut cpuset =
                std::mem::zeroed::<libc::cpu_set_t>();
            libc::CPU_SET(1, &mut cpuset);
            let ret = libc::sched_setaffinity(
                0,
                std::mem::size_of::<libc::cpu_set_t>(),
                &cpuset,
            );
            if ret != 0 {
                warn!(
                    "Could not pin to core 1 — \
                     continuing anyway"
                );
            } else {
                info!("Process pinned to core 1");
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    warn!(
        "Core isolation not available — \
         dev platform"
    );

    Ok(())
}
