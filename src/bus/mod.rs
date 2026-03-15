// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/bus/mod.rs

pub mod modbus;
pub mod cia402;
#[cfg(feature = "ethercat")]
pub mod ethercat;

use anyhow::Result;
use tracing::{info, warn};

use crate::config::loader::{Config, BusType};
use crate::core::io_image::IOImage;

// start all bus server threads
// one thread per configured bus
// each runs independently at its own cycle rate
// returns a handle per started thread for health monitoring
pub fn start_all(
    config: &Config,
    io:     &'static mut IOImage,
) -> Result<Vec<(String, std::thread::JoinHandle<()>)>> {
    let mut handles = Vec::new();

    // track IO base offset per bus
    // each bus gets a slice of the IO image
    let mut io_base = 0usize;

    // store as usize (Send) so each bus thread can recover the pointer
    // SAFETY: bus threads access non-overlapping index ranges of IOImage
    let io_addr: usize = io as *mut IOImage as usize;

    for (name, bus_config) in &config.buses {

        // collect devices on this bus
        let bus_devices: Vec<_> = config.devices
            .iter()
            .filter(|d| d.bus == *name)
            .collect();

        if bus_devices.is_empty() {
            warn!(
                "Bus '{}' has no devices — \
                 skipping",
                name
            );
            continue;
        }

        // count IO points for this bus
        let bus_input_count: usize = bus_devices
            .iter()
            .map(|d| d.kind.input_count())
            .sum();

        info!(
            "Starting bus '{}' — {} devices \
             {} IO points at base {}",
            name,
            bus_devices.len(),
            bus_input_count,
            io_base,
        );

        // dispatch to correct driver
        let bus_name = name.clone();
        let bus_cfg  = bus_config;
        let devices: Vec<_> = bus_devices
            .iter()
            .map(|d| (**d).clone())
            .collect();
        let base = io_base;

        match bus_cfg.bus_type {

            BusType::Modbus => {
                let Some(_port) = bus_cfg.port else {
                    warn!(
                        "Bus '{}' type=modbus but no port configured — skipping",
                        bus_name
                    );
                    io_base += bus_input_count;
                    continue;
                };

                let driver = modbus::ModbusDriver::new(
                    &bus_name,
                    bus_cfg,
                    &devices,
                    base,
                )?;

                let label = bus_name.clone();
                let handle = std::thread::Builder::new()
                    .name(format!("bus-{}", bus_name))
                    .spawn(move || {
                        let io_ref = unsafe {
                            &mut *(io_addr as *mut IOImage)
                        };
                        if let Err(e) = driver.run(io_ref) {
                            tracing::error!(
                                "Bus '{}' thread failed: {}",
                                bus_name, e
                            );
                        }
                    })?;

                handles.push((label, handle));
            }

            BusType::EtherCat => {
                #[cfg(feature = "ethercat")]
                {
                    let driver = ethercat::EtherCatDriver::new(
                        &bus_name,
                        bus_cfg,
                        &devices,
                        base,
                    )?;

                    let label = bus_name.clone();
                    let handle = std::thread::Builder::new()
                        .name(format!("bus-{}", bus_name))
                        .spawn(move || {
                            let io_ref = unsafe {
                                &mut *(io_addr as *mut IOImage)
                            };
                            if let Err(e) = driver.run(io_ref) {
                                tracing::error!(
                                    "Bus '{}' thread failed: {}",
                                    bus_name, e
                                );
                            }
                        })?;

                    handles.push((label, handle));
                }
                #[cfg(not(feature = "ethercat"))]
                {
                    warn!(
                        "Bus '{}' type=ethercat but noladder was compiled \
                         without the 'ethercat' feature — skipping. \
                         Recompile with: cargo build --features ethercat",
                        bus_name
                    );
                    io_base += bus_input_count;
                    continue;
                }
            }
        }

        io_base += bus_input_count;
    }

    Ok(handles)
}
