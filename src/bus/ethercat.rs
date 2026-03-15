// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/bus/ethercat.rs
//
// EtherCAT bus driver using the IgH EtherCAT master (ethercat crate).
//
// Requirements at runtime:
//   - IgH EtherCAT master kernel module loaded
//   - Master configured for the interface in machine.toml
//   - Run as root or with /dev/EtherCAT* permissions
//
// Quick start on a real machine:
//   modprobe ec_master
//   modprobe ec_generic          # or ec_e1000 / ec_igb for your NIC
//   ethercat config -m 0 -a eth1 # tell master which interface to use
//   cargo build --features ethercat
//   sudo noladder-bus machine.toml
//
// Servo drives follow CiA 402 — state machine and unit conversion
// are handled by cia402::CiA402Drive.  The driver only does PDO
// mapping and IO image bridging.

#![allow(dead_code)]

use anyhow::{Result, Context};
use tracing::{info, warn, debug};

use ethercat::{
    Master, MasterAccess,
    DomainIdx,
    SlaveAddr, SlaveId,
    AlState,
    PdoCfg, PdoEntryIdx, PdoEntryInfo, PdoEntryPos, PdoIdx,
    Offset, SmCfg,
    Idx, SubIdx, SmIdx,
};

use crate::config::loader::{BusConfig, ResolvedDevice, DeviceKind};
use crate::core::io_image::{IOImage, Value};
use super::cia402::{self, CiA402Drive, OperationMode};

// ------------------------------------
// PDO entry index helper
// ------------------------------------

fn oi(idx: u16, sub: u8) -> PdoEntryIdx {
    PdoEntryIdx {
        idx:     Idx::from(idx),
        sub_idx: SubIdx::from(sub),
    }
}

// ------------------------------------
// CiA 402 — servo / VFD cyclic data
// Verify against your slave's ESI XML.
// ------------------------------------

// Outputs (master → slave / RxPDO)
const CONTROL_WORD:       PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6040), sub_idx: SubIdx::new(0x00) };
const OPERATION_MODE_SET: PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6060), sub_idx: SubIdx::new(0x00) };
const TARGET_POSITION:    PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x607A), sub_idx: SubIdx::new(0x00) };
const TARGET_VELOCITY:    PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x60FF), sub_idx: SubIdx::new(0x00) };
const TARGET_TORQUE:      PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6071), sub_idx: SubIdx::new(0x00) };
const TORQUE_LIMIT:       PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6072), sub_idx: SubIdx::new(0x00) };
const MAX_CURRENT_LIMIT:  PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6073), sub_idx: SubIdx::new(0x00) };

// Inputs (slave → master / TxPDO)
const STATUS_WORD:            PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6041), sub_idx: SubIdx::new(0x00) };
const OPERATION_MODE_DISPLAY: PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6061), sub_idx: SubIdx::new(0x00) };
const ACTUAL_POSITION:        PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6064), sub_idx: SubIdx::new(0x00) };
const ACTUAL_VELOCITY:        PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x606C), sub_idx: SubIdx::new(0x00) };
const ACTUAL_TORQUE:          PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6077), sub_idx: SubIdx::new(0x00) };
const FOLLOWING_ERROR_ACT:    PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x60F4), sub_idx: SubIdx::new(0x00) };
const ERROR_CODE_OBJ:         PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x603F), sub_idx: SubIdx::new(0x00) };

// VFD-only (velocity + current without full position feedback)
const ACTUAL_CURRENT:     PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6078), sub_idx: SubIdx::new(0x00) };

// CiA 404 — digital / analog IO
const DIGITAL_INPUTS:    PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x6000), sub_idx: SubIdx::new(0x01) };
const DIGITAL_OUTPUTS:   PdoEntryIdx = PdoEntryIdx { idx: Idx::new(0x7000), sub_idx: SubIdx::new(0x01) };
const ANALOG_IN_BASE:    u16 = 0x6401;   // sub_idx 0x01..0x04 per channel
const ANALOG_OUT_BASE:   u16 = 0x7401;   // sub_idx 0x01..0x04 per channel

// ------------------------------------
// Per-slave resolved PDO offsets
// ------------------------------------

struct SlaveMap {
    device_path: String,
    kind:        DeviceKind,
    io_in_base:  usize,   // first input  slot in IOImage
    io_out_base: usize,   // first output slot in IOImage
    in_offsets:  Vec<Offset>,   // TxPDO byte offsets in domain data
    out_offsets: Vec<Offset>,   // RxPDO byte offsets in domain data

    // CiA402 state machine — Some only for ServoDrive
    drive:       Option<CiA402Drive>,
}

// ------------------------------------
// EtherCatDriver
// ------------------------------------

pub struct EtherCatDriver {
    name:       String,
    master_idx: u32,
    cycle_ms:   u32,
    devices:    Vec<ResolvedDevice>,
    io_base:    usize,
}

impl EtherCatDriver {
    pub fn new(
        name:    &str,
        config:  &BusConfig,
        devices: &[ResolvedDevice],
        io_base: usize,
    ) -> Result<Self> {
        Ok(Self {
            name:       name.to_string(),
            master_idx: config.master,
            cycle_ms:   config.cycle_ms,
            devices:    devices.to_vec(),
            io_base,
        })
    }

    pub fn run(self, io: &'static mut IOImage) -> Result<()> {
        use std::time::{Duration, Instant};

        info!(
            "EtherCAT driver '{}' starting — master {} — {}ms cycle",
            self.name, self.master_idx, self.cycle_ms
        );

        // ------------------------------------
        // Open and reserve master
        // ------------------------------------

        let mut master = Master::open(self.master_idx, MasterAccess::ReadWrite)
            .with_context(|| format!(
                "Could not open EtherCAT master {} — \
                 is the ec_master kernel module loaded \
                 and /dev/EtherCAT{} accessible?",
                self.master_idx, self.master_idx
            ))?;

        master.reserve()
            .context("Could not reserve EtherCAT master")?;

        // ------------------------------------
        // Create domain
        // All slaves share one domain so the master
        // fills all process data in one exchange cycle.
        // ------------------------------------

        let domain_idx: DomainIdx = master.create_domain()
            .context("Could not create EtherCAT domain")?;

        // ------------------------------------
        // Configure slaves, register PDO entries
        // ------------------------------------

        let mut io_in_cursor = self.io_base;
        let mut slave_maps: Vec<SlaveMap> = Vec::new();

        for device in &self.devices {
            let io_in_base  = io_in_cursor;
            let io_out_base = device.output_base;

            let pos = SlaveAddr::ByPos(device.node as u16);
            master.request_state(
                ethercat::SlavePos::from(device.node as u16),
                AlState::PreOp,
            ).unwrap_or_else(|e| {
                warn!(
                    "Could not request PreOp for '{}': {}",
                    device.path, e
                );
            });

            let slave_id = SlaveId {
                vendor_id:    device.vendor_id.unwrap_or(0),
                product_code: device.product_code.unwrap_or(0),
            };

            let mut sc = master.configure_slave(pos, slave_id)
                .with_context(|| format!(
                    "Could not configure slave '{}' at position {}",
                    device.path, device.node
                ))?;

            let map = configure_pdos(
                &mut sc,
                domain_idx,
                device,
                io_in_base,
                io_out_base,
            )?;

            io_in_cursor += device.kind.input_count();

            if let Some(m) = map {
                slave_maps.push(m);
            }
        }

        // ------------------------------------
        // Activate
        // ------------------------------------

        master.activate()
            .context("Could not activate EtherCAT master")?;

        info!(
            "EtherCAT driver '{}' active — {} slaves mapped",
            self.name,
            slave_maps.len()
        );

        // ------------------------------------
        // Cyclic PDO exchange loop
        // ------------------------------------

        let cycle    = Duration::from_millis(self.cycle_ms as u64);
        let mut next = Instant::now() + cycle;

        loop {
            // 1. Receive frames → update domain data
            master.receive().unwrap_or_else(|e| {
                warn!(
                    "EtherCAT receive error on '{}': {}",
                    self.name, e
                );
            });
            master.domain(domain_idx).process().unwrap_or_else(|e| {
                warn!("EtherCAT domain process error: {}", e);
            });

            // 2. For each slave:
            //    - servo drives: read TxPDO + run state machine + write RxPDO + publish feedback
            //    - IO devices:   read TxPDO → IOImage AND IOImage → write RxPDO
            if let Ok(data) = master.domain_data(domain_idx) {
                for sm in &mut slave_maps {
                    process_slave(io, sm, data);
                }
                io.signal_ready();
            }

            // 3. Queue and send
            master.domain(domain_idx).queue().unwrap_or_else(|e| {
                warn!("EtherCAT domain queue error: {}", e);
            });
            master.send().unwrap_or_else(|e| {
                warn!(
                    "EtherCAT send error on '{}': {}",
                    self.name, e
                );
                0
            });

            debug!(
                "EtherCAT '{}' domain state: {:?}",
                self.name,
                master.domain(domain_idx).state()
            );

            // 4. Cycle timing
            let now = Instant::now();
            if now < next {
                std::thread::sleep(next - now);
            } else {
                debug!(
                    "EtherCAT '{}' cycle late by {}µs",
                    self.name,
                    (now - next).as_micros()
                );
            }
            next += cycle;
        }
    }
}

// ------------------------------------
// Configure PDOs for one slave
// returns None for unsupported DeviceKind
// ------------------------------------

fn configure_pdos(
    sc:          &mut ethercat::SlaveConfig,
    domain:      DomainIdx,
    device:      &ResolvedDevice,
    io_in_base:  usize,
    io_out_base: usize,
) -> Result<Option<SlaveMap>> {

    // (rx_entries, tx_entries) as (PDO index, bit_len) pairs
    // rx = RxPDO: master → slave (our outputs)
    // tx = TxPDO: slave → master (our inputs)

    match &device.kind {

        DeviceKind::ServoDrive => {
            // RxPDO 0x1600 — master sends to drive each cycle
            let rx_entries: &[(PdoEntryIdx, u8)] = &[
                (CONTROL_WORD,       16),
                (OPERATION_MODE_SET,  8),
                (TARGET_POSITION,    32),
                (TARGET_VELOCITY,    32),
                (TARGET_TORQUE,      16),
                (TORQUE_LIMIT,       16),
                (MAX_CURRENT_LIMIT,  16),
            ];
            // TxPDO 0x1A00 — drive sends back each cycle
            let tx_entries: &[(PdoEntryIdx, u8)] = &[
                (STATUS_WORD,            16),
                (OPERATION_MODE_DISPLAY,  8),
                (ACTUAL_POSITION,        32),
                (ACTUAL_VELOCITY,        32),
                (ACTUAL_TORQUE,          16),
                (FOLLOWING_ERROR_ACT,    32),
                (ERROR_CODE_OBJ,         16),
            ];

            configure_sm(sc, domain, rx_entries, tx_entries, 0x1600, 0x1A00, &device.path)?;

            let in_offsets  = register_entries(sc, domain, tx_entries);
            let out_offsets = register_entries(sc, domain, rx_entries);

            let drive = CiA402Drive::new(
                &device.path,
                device.node as u16,
                parse_operation_mode(device.operation_mode.as_deref()),
                device.counts_per_unit.unwrap_or(1.0),
                device.max_following_error.unwrap_or(f64::MAX),
            );

            Ok(Some(SlaveMap {
                device_path: device.path.clone(),
                kind:        device.kind.clone(),
                io_in_base,
                io_out_base,
                in_offsets,
                out_offsets,
                drive:       Some(drive),
            }))
        }

        DeviceKind::Vfd => {
            let rx_entries: &[(PdoEntryIdx, u8)] = &[
                (TARGET_VELOCITY, 32),
                (CONTROL_WORD,    16),
            ];
            let tx_entries: &[(PdoEntryIdx, u8)] = &[
                (ACTUAL_VELOCITY, 32),
                (ACTUAL_CURRENT,  16),
            ];

            configure_sm(sc, domain, rx_entries, tx_entries, 0x1600, 0x1A00, &device.path)?;

            Ok(Some(SlaveMap {
                device_path: device.path.clone(),
                kind:        device.kind.clone(),
                io_in_base,
                io_out_base,
                in_offsets:  register_entries(sc, domain, tx_entries),
                out_offsets: register_entries(sc, domain, rx_entries),
                drive:       None,
            }))
        }

        DeviceKind::DigitalIn => {
            let tx_entries: &[(PdoEntryIdx, u8)] = &[(DIGITAL_INPUTS, 8)];
            configure_sm(sc, domain, &[], tx_entries, 0x1600, 0x1A00, &device.path)?;

            Ok(Some(SlaveMap {
                device_path: device.path.clone(),
                kind:        device.kind.clone(),
                io_in_base,
                io_out_base,
                in_offsets:  register_entries(sc, domain, tx_entries),
                out_offsets: vec![],
                drive:       None,
            }))
        }

        DeviceKind::DigitalOut => {
            let rx_entries: &[(PdoEntryIdx, u8)] = &[(DIGITAL_OUTPUTS, 8)];
            configure_sm(sc, domain, rx_entries, &[], 0x1600, 0x1A00, &device.path)?;

            Ok(Some(SlaveMap {
                device_path: device.path.clone(),
                kind:        device.kind.clone(),
                io_in_base,
                io_out_base,
                in_offsets:  vec![],
                out_offsets: register_entries(sc, domain, rx_entries),
                drive:       None,
            }))
        }

        DeviceKind::AnalogIn => {
            let idxs: Vec<(PdoEntryIdx, u8)> = (1u8..=4)
                .map(|ch| (oi(ANALOG_IN_BASE, ch), 32u8))
                .collect();
            configure_sm(sc, domain, &[], &idxs, 0x1600, 0x1A00, &device.path)?;

            Ok(Some(SlaveMap {
                device_path: device.path.clone(),
                kind:        device.kind.clone(),
                io_in_base,
                io_out_base,
                in_offsets:  register_entries(sc, domain, &idxs),
                out_offsets: vec![],
                drive:       None,
            }))
        }

        DeviceKind::AnalogOut => {
            let idxs: Vec<(PdoEntryIdx, u8)> = (1u8..=4)
                .map(|ch| (oi(ANALOG_OUT_BASE, ch), 32u8))
                .collect();
            configure_sm(sc, domain, &idxs, &[], 0x1600, 0x1A00, &device.path)?;

            Ok(Some(SlaveMap {
                device_path: device.path.clone(),
                kind:        device.kind.clone(),
                io_in_base,
                io_out_base,
                in_offsets:  vec![],
                out_offsets: register_entries(sc, domain, &idxs),
                drive:       None,
            }))
        }

        other => {
            warn!(
                "EtherCAT: device kind {:?} not supported — \
                 skipping slave '{}'",
                other, device.path
            );
            Ok(None)
        }
    }
}

// ------------------------------------
// Configure SyncManager PDO assignments
// ------------------------------------

fn configure_sm(
    sc:      &mut ethercat::SlaveConfig,
    _domain: DomainIdx,
    rx:      &[(PdoEntryIdx, u8)],
    tx:      &[(PdoEntryIdx, u8)],
    rx_pdo:  u16,
    tx_pdo:  u16,
    path:    &str,
) -> Result<()> {
    if !rx.is_empty() {
        sc.config_sm_pdos(SmCfg::output(SmIdx::from(2u8)), &[
            PdoCfg {
                idx:     PdoIdx::from(rx_pdo),
                entries: make_entries(rx),
            }
        ]).with_context(|| format!(
            "RxPDO config failed for '{}'", path
        ))?;
    }
    if !tx.is_empty() {
        sc.config_sm_pdos(SmCfg::input(SmIdx::from(3u8)), &[
            PdoCfg {
                idx:     PdoIdx::from(tx_pdo),
                entries: make_entries(tx),
            }
        ]).with_context(|| format!(
            "TxPDO config failed for '{}'", path
        ))?;
    }
    Ok(())
}

fn make_entries(entries: &[(PdoEntryIdx, u8)]) -> Vec<PdoEntryInfo> {
    entries.iter().enumerate().map(|(pos, &(entry_idx, bit_len))| {
        PdoEntryInfo {
            pos:       PdoEntryPos::from(pos as u8),
            entry_idx,
            bit_len,
            name:      String::new(),
        }
    }).collect()
}

fn register_entries(
    sc:      &mut ethercat::SlaveConfig,
    domain:  DomainIdx,
    entries: &[(PdoEntryIdx, u8)],
) -> Vec<Offset> {
    entries.iter()
        .filter_map(|(idx, _)| sc.register_pdo_entry(*idx, domain).ok())
        .collect()
}

// ------------------------------------
// Parse operation mode from config string
// ------------------------------------

fn parse_operation_mode(mode: Option<&str>) -> OperationMode {
    match mode {
        Some("csv") | Some("cyclic_sync_velocity")  => OperationMode::CyclicSyncVelocity,
        Some("cst") | Some("cyclic_sync_torque")    => OperationMode::CyclicSyncTorque,
        Some("profile_velocity")                    => OperationMode::ProfileVelocity,
        Some("profile_position")                    => OperationMode::ProfilePosition,
        Some("profile_torque")                      => OperationMode::ProfileTorque,
        Some("homing")                              => OperationMode::Homing,
        _                                           => OperationMode::CyclicSyncPosition,
    }
}

// ------------------------------------
// Per-slave process function
// called every EtherCAT cycle
// ------------------------------------

fn process_slave(io: &mut IOImage, sm: &mut SlaveMap, data: &mut [u8]) {
    match &sm.kind {

        DeviceKind::ServoDrive => {
            if let Some(drive) = &mut sm.drive {
                // 1. Read TxPDO (status, feedback) from domain data
                //    Order must match tx_entries in configure_pdos
                let tx = cia402::TxPDO {
                    statusword:             get_u16(data, &sm.in_offsets, 0),
                    operation_mode_display: get_u8( data, &sm.in_offsets, 1) as i8,
                    actual_position:        get_i32(data, &sm.in_offsets, 2),
                    actual_velocity:        get_i32(data, &sm.in_offsets, 3),
                    actual_torque:          get_i16(data, &sm.in_offsets, 4),
                    following_error:        get_i32(data, &sm.in_offsets, 5),
                    error_code:             get_u16(data, &sm.in_offsets, 6),
                };

                // 2. Apply setpoints and commands from IOImage
                //    (written by control loop in previous cycle)
                let b = sm.io_out_base;
                drive.set_position(
                    io.read_output(b + cia402::OUT_TARGET_POSITION)
                        .as_float().unwrap_or(0.0) as f64
                );
                drive.set_velocity(
                    io.read_output(b + cia402::OUT_TARGET_VELOCITY)
                        .as_float().unwrap_or(0.0) as f64
                );
                drive.set_torque(
                    io.read_output(b + cia402::OUT_TARGET_TORQUE)
                        .as_float().unwrap_or(0.0) as f64
                );
                drive.set_max_torque(
                    io.read_output(b + cia402::OUT_MAX_TORQUE)
                        .as_float().unwrap_or(100.0) as f64
                );
                if io.read_output(b + cia402::OUT_FAULT_RESET)
                        .as_bool().unwrap_or(false) {
                    drive.reset_fault();
                }
                if io.read_output(b + cia402::OUT_QUICK_STOP)
                        .as_bool().unwrap_or(false) {
                    drive.quick_stop();
                }

                // 3. Run CiA402 state machine
                //    drive.update() generates the next controlword
                let rx = drive.update(tx);

                // 4. Write RxPDO to domain data
                //    Order must match rx_entries in configure_pdos
                set_u16(data, &sm.out_offsets, 0, rx.controlword);
                set_i8( data, &sm.out_offsets, 1, rx.operation_mode);
                set_i32(data, &sm.out_offsets, 2, rx.target_position);
                set_i32(data, &sm.out_offsets, 3, rx.target_velocity);
                set_i16(data, &sm.out_offsets, 4, rx.target_torque);
                set_u16(data, &sm.out_offsets, 5, rx.max_torque);
                set_u16(data, &sm.out_offsets, 6, rx.max_current);

                // 5. Publish feedback to IOImage inputs
                let b = sm.io_in_base;
                io.publish_inputs(
                    b + cia402::IN_ACTUAL_POSITION,
                    Value::Float(drive.actual_position() as f32),
                );
                io.publish_inputs(
                    b + cia402::IN_ACTUAL_VELOCITY,
                    Value::Float(drive.actual_velocity() as f32),
                );
                io.publish_inputs(
                    b + cia402::IN_ACTUAL_TORQUE,
                    Value::Float(drive.actual_torque() as f32),
                );
                io.publish_inputs(
                    b + cia402::IN_FOLLOWING_ERROR,
                    Value::Float(drive.following_error() as f32),
                );
                io.publish_inputs(
                    b + cia402::IN_ENABLED,
                    Value::Bool(drive.is_enabled()),
                );
                io.publish_inputs(
                    b + cia402::IN_FAULT,
                    Value::Bool(drive.is_fault()),
                );
                io.publish_inputs(
                    b + cia402::IN_TARGET_REACHED,
                    Value::Bool(drive.is_target_reached()),
                );
                io.publish_inputs(
                    b + cia402::IN_HOMING_COMPLETE,
                    Value::Bool(drive.is_homing_complete()),
                );
                io.publish_inputs(
                    b + cia402::IN_ERROR_CODE,
                    Value::Int(drive.error_code() as i32),
                );
                io.publish_inputs(
                    b + cia402::IN_REFERENCED,
                    Value::Bool(drive.is_referenced()),
                );
            }
        }

        DeviceKind::Vfd => {
            // VFD: simplified — no full state machine
            // velocity + current feedback
            let b = sm.io_in_base;
            io.publish_inputs(b,     Value::Float(get_i32(data, &sm.in_offsets, 0) as f32));
            io.publish_inputs(b + 1, Value::Float(get_i16(data, &sm.in_offsets, 1) as f32 / 100.0));

            let b = sm.io_out_base;
            let vel = io.read_output(b    ).as_float().unwrap_or(0.0);
            let ena = io.read_output(b + 1).as_bool().unwrap_or(false);
            set_i32(data, &sm.out_offsets, 0, vel as i32);
            set_u16(data, &sm.out_offsets, 1, if ena { 0x000F } else { 0x0006 });
        }

        DeviceKind::DigitalIn => {
            let bits = get_u8(data, &sm.in_offsets, 0);
            let b    = sm.io_in_base;
            for i in 0..8usize {
                io.publish_inputs(b + i, Value::Bool((bits >> i) & 1 != 0));
            }
        }

        DeviceKind::DigitalOut => {
            let b    = sm.io_out_base;
            let mut bits = 0u8;
            for i in 0..8usize {
                if io.read_output(b + i).as_bool().unwrap_or(false) {
                    bits |= 1 << i;
                }
            }
            set_u8(data, &sm.out_offsets, 0, bits);
        }

        DeviceKind::AnalogIn => {
            let b = sm.io_in_base;
            for (i, offset) in sm.in_offsets.iter().enumerate() {
                let raw = get_i32_at(data, offset);
                io.publish_inputs(b + i, Value::Float(raw as f32));
            }
        }

        DeviceKind::AnalogOut => {
            let b = sm.io_out_base;
            for (i, offset) in sm.out_offsets.iter().enumerate() {
                let v = io.read_output(b + i).as_float().unwrap_or(0.0);
                set_i32_at(data, offset, v as i32);
            }
        }

        _ => {
            debug!(
                "EtherCAT: unhandled device '{}' kind {:?}",
                sm.device_path, sm.kind
            );
        }
    }
}

// ------------------------------------
// Process data byte accessors
// EtherCAT process data is little-endian
// ------------------------------------

fn get_u8(data: &[u8], offsets: &[Offset], i: usize) -> u8 {
    offsets.get(i).map(|o| get_u8_at(data, o)).unwrap_or(0)
}
fn get_u16(data: &[u8], offsets: &[Offset], i: usize) -> u16 {
    offsets.get(i).map(|o| get_u16_at(data, o)).unwrap_or(0)
}
fn get_i16(data: &[u8], offsets: &[Offset], i: usize) -> i16 {
    get_u16(data, offsets, i) as i16
}
fn get_i32(data: &[u8], offsets: &[Offset], i: usize) -> i32 {
    offsets.get(i).map(|o| get_i32_at(data, o)).unwrap_or(0)
}

fn get_u8_at(data: &[u8], o: &Offset) -> u8 {
    *data.get(o.byte).unwrap_or(&0)
}
fn get_u16_at(data: &[u8], o: &Offset) -> u16 {
    let b = o.byte;
    if b + 1 < data.len() {
        u16::from_le_bytes([data[b], data[b + 1]])
    } else { 0 }
}
fn get_i32_at(data: &[u8], o: &Offset) -> i32 {
    let b = o.byte;
    if b + 3 < data.len() {
        i32::from_le_bytes(data[b..b+4].try_into().unwrap_or([0;4]))
    } else { 0 }
}

fn set_u8(data: &mut [u8], offsets: &[Offset], i: usize, v: u8) {
    if let Some(o) = offsets.get(i) {
        if o.byte < data.len() { data[o.byte] = v; }
    }
}
fn set_u16(data: &mut [u8], offsets: &[Offset], i: usize, v: u16) {
    if let Some(o) = offsets.get(i) { set_u16_at(data, o, v); }
}
fn set_i8(data: &mut [u8], offsets: &[Offset], i: usize, v: i8) {
    set_u8(data, offsets, i, v as u8);
}
fn set_i16(data: &mut [u8], offsets: &[Offset], i: usize, v: i16) {
    set_u16(data, offsets, i, v as u16);
}
fn set_i32(data: &mut [u8], offsets: &[Offset], i: usize, v: i32) {
    if let Some(o) = offsets.get(i) { set_i32_at(data, o, v); }
}

fn set_u16_at(data: &mut [u8], o: &Offset, v: u16) {
    let b = o.byte;
    if b + 1 < data.len() {
        let bytes = v.to_le_bytes();
        data[b]     = bytes[0];
        data[b + 1] = bytes[1];
    }
}
fn set_i32_at(data: &mut [u8], o: &Offset, v: i32) {
    let b = o.byte;
    if b + 3 < data.len() {
        let bytes = v.to_le_bytes();
        data[b..b+4].copy_from_slice(&bytes);
    }
}
