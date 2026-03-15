// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/config/loader.rs

use std::collections::HashMap;
use std::path::Path;
use anyhow::{Result, Context};
use serde::Deserialize;
use tracing::{info, warn, debug};

// ------------------------------------
// Raw config structs
// directly mapped from TOML
// ------------------------------------

#[derive(Debug, Deserialize)]
pub struct RawConfig {
    pub general: GeneralConfig,
    pub bus:     HashMap<String, BusConfig>,
    pub device:  HashMap<String, DeviceConfig>,
}

#[derive(Debug, Deserialize)]
pub struct GeneralConfig {
    pub cycle_ms: u32,
}

/// Which physical bus technology this entry represents.
#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum BusType {
    Modbus,
    EtherCat,
}

#[derive(Debug, Deserialize)]
pub struct BusConfig {
    /// Discriminator field: `type = "modbus"` or `type = "ethercat"`.
    /// Defaults to `modbus` when absent so existing configs keep working.
    #[serde(rename = "type", default = "default_bus_type")]
    pub bus_type:  BusType,

    /// Network interface or IP address.
    pub interface: String,

    /// TCP port — Modbus only.
    pub port:      Option<u16>,

    /// EtherCAT master index (0 = first IgH master).  EtherCAT only.
    #[serde(default)]
    pub master:    u32,

    /// Cycle time in milliseconds.
    pub cycle_ms:  u32,
}

fn default_bus_type() -> BusType {
    BusType::Modbus
}

#[derive(Debug, Deserialize)]
pub struct DeviceConfig {
    pub bus:   String,

    /// Modbus: unit ID.  EtherCAT: slave position in the network ring.
    pub node:  u32,

    #[serde(rename = "type")]
    pub kind:  DeviceKind,

    // ------------------------------------
    // EtherCAT slave identification
    // Optional — used to verify the correct slave is wired at `node`.
    // Obtained from the slave's ESI XML or `ethercat slaves` command.
    // ------------------------------------

    /// EtherCAT vendor ID (e.g. 0x00000002 for Beckhoff).
    pub vendor_id:    Option<u32>,
    /// EtherCAT product code.
    pub product_code: Option<u32>,

    // ------------------------------------
    // Servo drive tuning
    // servo_drive and vfd device types only
    // ------------------------------------

    /// Encoder counts per engineering unit.
    /// linear: counts/mm   rotary: counts/rev
    /// Default 1.0 — raw counts passthrough.
    pub counts_per_unit:     Option<f64>,

    /// Maximum following error in engineering units before warning.
    /// Default: no check.
    pub max_following_error: Option<f64>,

    /// CiA402 operation mode: "csp" (default), "csv", "cst",
    /// "profile_velocity", "profile_position", "homing"
    pub operation_mode:      Option<String>,

    // optional human readable comment
    // encouraged for legacy hardware
    pub note:  Option<String>,
}

#[derive(Debug, Deserialize, Clone, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DeviceKind {
    // drives
    ServoDrive,
    Vfd,           // variable frequency drive

    // IO
    DigitalIn,
    DigitalOut,
    AnalogIn,
    AnalogOut,

    // combined
    MixedIo,

    // safety
    SafetyRelay,
    SafetyDoor,

    // virtual — internal coordination flags
    // not connected to hardware
    // 1 input + 1 output at the device path itself
    Flag,
}

impl DeviceKind {
    // how many input values does this device type produce
    pub fn input_count(&self) -> usize {
        match self {
            DeviceKind::ServoDrive  => 10, // position, velocity, torque, following_error, enabled, fault, target_reached, homing_complete, error_code, referenced
            DeviceKind::Vfd         => 2, // speed, current
            DeviceKind::DigitalIn   => 8, // 8 bits
            DeviceKind::DigitalOut  => 0, // output only
            DeviceKind::AnalogIn    => 4, // 4 channels
            DeviceKind::AnalogOut   => 0, // output only
            DeviceKind::MixedIo     => 4,
            DeviceKind::SafetyRelay => 2,
            DeviceKind::SafetyDoor  => 2,
            DeviceKind::Flag        => 1,
        }
    }

    // how many output values does this device type consume
    pub fn output_count(&self) -> usize {
        match self {
            DeviceKind::ServoDrive  => 6, // target_position, target_velocity, target_torque, max_torque, fault_reset, quick_stop
            DeviceKind::Vfd         => 2, // setpoint, enable
            DeviceKind::DigitalIn   => 0, // input only
            DeviceKind::DigitalOut  => 8, // 8 bits
            DeviceKind::AnalogIn    => 0, // input only
            DeviceKind::AnalogOut   => 4, // 4 channels
            DeviceKind::MixedIo     => 4,
            DeviceKind::SafetyRelay => 1,
            DeviceKind::SafetyDoor  => 0, // input only
            DeviceKind::Flag        => 1,
        }
    }
}

// ------------------------------------
// Resolved config
// after validation and index assignment
// this is what the runtime uses
// ------------------------------------

#[derive(Debug)]
pub struct Config {
    pub cycle_ms: u32,
    pub buses:    HashMap<String, BusConfig>,
    pub devices:  Vec<ResolvedDevice>,

    // total IO points allocated
    pub input_count:  usize,
    pub output_count: usize,
}

#[derive(Debug, Clone)]
pub struct ResolvedDevice {
    /// Dotted path from config, e.g. `"line1.conveyor.motor"`.
    pub path:         String,

    pub bus:          String,
    /// Modbus unit ID or EtherCAT slave position.
    pub node:         u32,
    pub kind:         DeviceKind,
    pub note:         Option<String>,

    /// EtherCAT slave identification (optional, used for wiring verification).
    pub vendor_id:    Option<u32>,
    pub product_code: Option<u32>,

    /// Servo drive tuning (servo_drive / vfd only).
    pub counts_per_unit:     Option<f64>,
    pub max_following_error: Option<f64>,
    pub operation_mode:      Option<String>,

    /// Assigned indices into IOImage.
    pub input_base:   usize,
    pub output_base:  usize,
}

impl ResolvedDevice {
    // resolve a signal name to an IOImage input index
    // "speed" → input_base + signal_offset
    pub fn input_index(&self, signal: &str) -> Option<usize> {
        let offset = match (&self.kind, signal) {
            (DeviceKind::ServoDrive, "position")       => Some(0),
            (DeviceKind::ServoDrive, "velocity")       => Some(1),
            (DeviceKind::ServoDrive, "torque")         => Some(2),
            (DeviceKind::ServoDrive, "following_error")=> Some(3),
            (DeviceKind::ServoDrive, "enabled")        => Some(4),
            (DeviceKind::ServoDrive, "fault")          => Some(5),
            (DeviceKind::ServoDrive, "target_reached") => Some(6),
            (DeviceKind::ServoDrive, "homing_complete")=> Some(7),
            (DeviceKind::ServoDrive, "error_code")     => Some(8),
            (DeviceKind::ServoDrive, "referenced")     => Some(9),

            (DeviceKind::Vfd, "speed")           => Some(0),
            (DeviceKind::Vfd, "current")         => Some(1),

            (DeviceKind::DigitalIn, signal) => {
                // digital_in.0 through digital_in.7
                signal.parse::<usize>().ok()
                    .filter(|&i| i < 8)
            }

            (DeviceKind::AnalogIn, signal) => {
                signal.parse::<usize>().ok()
                    .filter(|&i| i < 4)
            }

            (DeviceKind::SafetyRelay, "ok")      => Some(0),
            (DeviceKind::SafetyRelay, "fault")   => Some(1),
            (DeviceKind::SafetyDoor,  "closed")  => Some(0),
            (DeviceKind::SafetyDoor,  "locked")  => Some(1),

            // flag: empty signal "" resolves to slot 0
            (DeviceKind::Flag, "")               => Some(0),

            _ => None,
        }?;

        Some(self.input_base + offset)
    }

    pub fn output_index(&self, signal: &str) -> Option<usize> {
        let offset = match (&self.kind, signal) {
            (DeviceKind::ServoDrive, "target_position") => Some(0),
            (DeviceKind::ServoDrive, "target_velocity") => Some(1),
            (DeviceKind::ServoDrive, "target_torque")   => Some(2),
            (DeviceKind::ServoDrive, "max_torque")      => Some(3),
            (DeviceKind::ServoDrive, "fault_reset")     => Some(4),
            (DeviceKind::ServoDrive, "quick_stop")      => Some(5),

            (DeviceKind::Vfd, "setpoint")             => Some(0),
            (DeviceKind::Vfd, "enable")               => Some(1),

            (DeviceKind::DigitalOut, signal) => {
                signal.parse::<usize>().ok()
                    .filter(|&i| i < 8)
            }

            (DeviceKind::AnalogOut, signal) => {
                signal.parse::<usize>().ok()
                    .filter(|&i| i < 4)
            }

            (DeviceKind::SafetyRelay, "reset")        => Some(0),

            // flag: empty signal "" resolves to slot 0
            (DeviceKind::Flag, "")                    => Some(0),

            _ => None,
        }?;

        Some(self.output_base + offset)
    }
}

// ------------------------------------
// Device map
// built at startup from resolved config
// used by control logic to resolve
// "line1.conveyor.motor.speed" → IOImage index
// after this — no more string lookups
// ------------------------------------

pub struct DeviceMap {
    // "line1.conveyor.motor.speed" → input index
    inputs:  HashMap<String, usize>,

    // "line1.conveyor.motor.setpoint" → output index
    outputs: HashMap<String, usize>,
}

impl DeviceMap {
    pub fn build(config: &Config) -> Self {
        let mut inputs  = HashMap::new();
        let mut outputs = HashMap::new();

        for device in &config.devices {
            // build all known signal paths for this device type
            let input_signals  = device_input_signals(&device.kind);
            let output_signals = device_output_signals(&device.kind);

            for signal in input_signals {
                if let Some(idx) = device.input_index(signal) {
                    // empty signal (Flag kind) → register
                    // device path itself without a suffix
                    let path = if signal.is_empty() {
                        device.path.clone()
                    } else {
                        format!("{}.{}", device.path, signal)
                    };
                    debug!("Input  '{}' → index {}", path, idx);
                    inputs.insert(path, idx);
                }
            }

            for signal in output_signals {
                if let Some(idx) = device.output_index(signal) {
                    let path = if signal.is_empty() {
                        device.path.clone()
                    } else {
                        format!("{}.{}", device.path, signal)
                    };
                    debug!("Output '{}' → index {}", path, idx);
                    outputs.insert(path, idx);
                }
            }
        }

        info!(
            "Device map built — {} inputs {} outputs",
            inputs.len(), outputs.len()
        );

        Self { inputs, outputs }
    }

    // called at init time — cost irrelevant
    pub fn resolve_input(&self, path: &str) -> Option<usize> {
        self.inputs.get(path).copied()
    }

    pub fn resolve_output(&self, path: &str) -> Option<usize> {
        self.outputs.get(path).copied()
    }

    // panics if path not found
    // correct behavior — bad path is a programming error
    // caught at startup, never at runtime
    pub fn input(&self, path: &str) -> usize {
        self.resolve_input(path)
            .unwrap_or_else(|| panic!(
                "Unknown input path '{}' — check machine.toml",
                path
            ))
    }

    pub fn output(&self, path: &str) -> usize {
        self.resolve_output(path)
            .unwrap_or_else(|| panic!(
                "Unknown output path '{}' — check machine.toml",
                path
            ))
    }
}

fn device_input_signals(kind: &DeviceKind) -> &'static [&'static str] {
    match kind {
        DeviceKind::ServoDrive  => &["position", "velocity", "torque", "following_error", "enabled", "fault", "target_reached", "homing_complete", "error_code", "referenced"],
        DeviceKind::Vfd         => &["speed", "current"],
        DeviceKind::DigitalIn   => &["0","1","2","3","4","5","6","7"],
        DeviceKind::DigitalOut  => &[],
        DeviceKind::AnalogIn    => &["0","1","2","3"],
        DeviceKind::AnalogOut   => &[],
        DeviceKind::MixedIo     => &["0","1","2","3"],
        DeviceKind::SafetyRelay => &["ok", "fault"],
        DeviceKind::SafetyDoor  => &["closed", "locked"],
        DeviceKind::Flag        => &[""],  // empty signal → device path itself
    }
}

fn device_output_signals(kind: &DeviceKind) -> &'static [&'static str] {
    match kind {
        DeviceKind::ServoDrive  => &["target_position", "target_velocity", "target_torque", "max_torque", "fault_reset", "quick_stop"],
        DeviceKind::Vfd         => &["setpoint", "enable"],
        DeviceKind::DigitalIn   => &[],
        DeviceKind::DigitalOut  => &["0","1","2","3","4","5","6","7"],
        DeviceKind::AnalogIn    => &[],
        DeviceKind::AnalogOut   => &["0","1","2","3"],
        DeviceKind::MixedIo     => &["0","1","2","3"],
        DeviceKind::SafetyRelay => &["reset"],
        DeviceKind::SafetyDoor  => &[],
        DeviceKind::Flag        => &[""],  // empty signal → device path itself
    }
}

// ------------------------------------
// Loader
// ------------------------------------

pub fn load(path: impl AsRef<Path>) -> Result<Config> {
    let path = path.as_ref();

    let text = std::fs::read_to_string(path)
        .with_context(|| format!(
            "Could not read config file '{}'",
            path.display()
        ))?;

    let raw: RawConfig = toml::from_str(&text)
        .with_context(|| format!(
            "Could not parse config file '{}'",
            path.display()
        ))?;

    validate_and_resolve(raw)
}

fn validate_and_resolve(raw: RawConfig) -> Result<Config> {
    let mut errors = Vec::new();

    // validate each device references a known bus
    for (path, device) in &raw.device {
        if !raw.bus.contains_key(&device.bus) {
            errors.push(format!(
                "Device '{}' references unknown bus '{}'",
                path, device.bus
            ));
        }

        // warn about legacy hardware
        if device.kind == DeviceKind::Vfd {
            if let Some(note) = &device.note {
                warn!("Device '{}': {}", path, note);
            }
        }
    }

    // fail hard on any validation error
    // a machine that starts with bad config is dangerous
    if !errors.is_empty() {
        let msg = errors.join("\n");
        anyhow::bail!("Config validation failed:\n{}", msg);
    }

    // assign IO indices
    // deterministic ordering — sort by path
    // so indices are stable across restarts
    let mut sorted_devices: Vec<(&String, &DeviceConfig)> =
        raw.device.iter().collect();
    sorted_devices.sort_by_key(|(path, _)| path.as_str());

    let mut input_cursor  = 0usize;
    let mut output_cursor = 0usize;
    let mut resolved      = Vec::new();

    for (path, device) in sorted_devices {
        let input_base  = input_cursor;
        let output_base = output_cursor;

        input_cursor  += device.kind.input_count();
        output_cursor += device.kind.output_count();

        resolved.push(ResolvedDevice {
            path:                path.clone(),
            bus:                 device.bus.clone(),
            node:                device.node,
            kind:                device.kind.clone(),
            note:                device.note.clone(),
            vendor_id:           device.vendor_id,
            product_code:        device.product_code,
            counts_per_unit:     device.counts_per_unit,
            max_following_error: device.max_following_error,
            operation_mode:      device.operation_mode.clone(),
            input_base,
            output_base,
        });

        debug!(
            "Device '{}' — inputs {}..{} outputs {}..{}",
            path,
            input_base,  input_base  + device.kind.input_count(),
            output_base, output_base + device.kind.output_count(),
        );
    }

    info!(
        "Config loaded — {} buses {} devices \
         {} input slots {} output slots",
        raw.bus.len(),
        resolved.len(),
        input_cursor,
        output_cursor,
    );

    Ok(Config {
        cycle_ms:     raw.general.cycle_ms,
        buses:        raw.bus,
        devices:      resolved,
        input_count:  input_cursor,
        output_count: output_cursor,
    })
}

// ------------------------------------
// Validator binary
// cargo run --bin validate -- machine.toml
// ------------------------------------

pub fn validate_and_report(path: impl AsRef<Path>) {
    let path = path.as_ref();
    match load(path) {
        Ok(config) => {
            println!("✓ Config valid — {} buses {} devices",
                config.buses.len(),
                config.devices.len(),
            );
            for device in &config.devices {
                println!(
                    "  ✓ {} — {} on bus {} node {}",
                    device.path,
                    format!("{:?}", device.kind).to_lowercase(),
                    device.bus,
                    device.node,
                );
            }
        }
        Err(e) => {
            println!("✗ Config invalid:\n{}", e);
            std::process::exit(1);
        }
    }
}

// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_CONFIG: &str = r#"
        [general]
        cycle_ms = 1

        [bus.ethercat0]
        interface = "eth1"
        cycle_ms  = 1

        [bus.modbus0]
        interface = "127.0.0.1"
        port      = 502
        cycle_ms  = 10
        note      = "legacy sensor array"

        [device."line1.conveyor.motor"]
        bus  = "ethercat0"
        node = 3
        type = "servo_drive"

        [device."line1.conveyor.sensor"]
        bus  = "ethercat0"
        node = 4
        type = "digital_in"

        [device."cabinet1.pump"]
        bus  = "modbus0"
        node = 0
        type = "vfd"
        note = "legacy — replace Q3 2026"
    "#;

    #[test]
    fn test_valid_config_loads() {
        let raw: RawConfig = toml::from_str(VALID_CONFIG).unwrap();
        let config = validate_and_resolve(raw).unwrap();

        assert_eq!(config.buses.len(),   2);
        assert_eq!(config.devices.len(), 3);
    }

    #[test]
    fn test_indices_are_stable() {
        // same config loaded twice should give same indices
        let raw1: RawConfig = toml::from_str(VALID_CONFIG).unwrap();
        let raw2: RawConfig = toml::from_str(VALID_CONFIG).unwrap();

        let c1 = validate_and_resolve(raw1).unwrap();
        let c2 = validate_and_resolve(raw2).unwrap();

        for (d1, d2) in c1.devices.iter().zip(c2.devices.iter()) {
            assert_eq!(d1.input_base,  d2.input_base);
            assert_eq!(d1.output_base, d2.output_base);
        }
    }

    #[test]
    fn test_unknown_bus_fails() {
        let bad_config = r#"
            [general]
            cycle_ms = 1

            [bus.ethercat0]
            interface = "eth1"
            cycle_ms  = 1

            [device."line1.motor"]
            bus  = "doesnotexist"
            node = 1
            type = "vfd"
        "#;

        let raw: RawConfig = toml::from_str(bad_config).unwrap();
        assert!(validate_and_resolve(raw).is_err());
    }

    #[test]
    fn test_device_map_resolution() {
        let raw: RawConfig = toml::from_str(VALID_CONFIG).unwrap();
        let config = validate_and_resolve(raw).unwrap();
        let map    = DeviceMap::build(&config);

        // should resolve known signals (CiA402 names)
        assert!(map.resolve_input(
            "line1.conveyor.motor.velocity").is_some()
        );
        assert!(map.resolve_output(
            "line1.conveyor.motor.target_velocity").is_some()
        );

        // should not resolve unknown signals
        assert!(map.resolve_input(
            "line1.conveyor.motor.banana").is_none()
        );
    }

    #[test]
    fn test_device_map_panics_on_bad_path() {
        let raw: RawConfig = toml::from_str(VALID_CONFIG).unwrap();
        let config = validate_and_resolve(raw).unwrap();
        let map    = DeviceMap::build(&config);

        let result = std::panic::catch_unwind(|| {
            map.input("this.does.not.exist.speed")
        });
        assert!(result.is_err());
    }
}
