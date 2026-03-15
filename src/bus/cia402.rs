// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/bus/cia402.rs
//
// CiA402 CANopen over EtherCAT servo drive profile
// Implemented by virtually every servo drive on the market
// Beckhoff, Yaskawa, Bosch, Panasonic, Mitsubishi,
// Kollmorgen, Lenze, SEW, Delta, Inovance...
//
// State machine, PDO mapping, unit conversion
// EtherCAT transport handled by driver.rs

use tracing::{debug, warn, info};

// ------------------------------------
// CiA402 State Machine
//
//                      Automatic
//  ┌─────────────────────────────────────┐
//  │         Not Ready To Switch On      │
//  └──────────────────┬──────────────────┘
//                     │ automatic
//                     ▼
//  ┌─────────────────────────────────────┐
//  │         Switch On Disabled          │◄──────────┐
//  └──────────────────┬──────────────────┘           │
//                     │ Shutdown                     │
//                     ▼                              │
//  ┌─────────────────────────────────────┐           │
//  │         Ready To Switch On          │           │
//  └──────────────────┬──────────────────┘           │
//                     │ Switch On                    │ Disable
//                     ▼                              │ Voltage
//  ┌─────────────────────────────────────┐           │
//  │         Switched On                 │           │
//  └──────────────────┬──────────────────┘           │
//                     │ Enable Operation             │
//                     ▼                              │
//  ┌─────────────────────────────────────┐           │
//  │         Operation Enabled           │───────────┘
//  └─────────────────────────────────────┘
//
//  Any state → Fault Reaction Active → Fault
//  Fault → (Fault Reset) → Switch On Disabled
// ------------------------------------

// ------------------------------------
// Drive state
// decoded from statusword bits
// ------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum DriveState {
    NotReadyToSwitchOn,
    SwitchOnDisabled,
    ReadyToSwitchOn,
    SwitchedOn,
    OperationEnabled,
    QuickStopActive,
    FaultReactionActive,
    Fault,
    Unknown(u16),
}

impl DriveState {

    // decode from CiA402 statusword
    // mask out non-state bits first
    // then match the state pattern
    pub fn from_statusword(sw: u16) -> Self {
        // bits 0,1,2,3,5,6 encode state
        // bit 4 = voltage enabled (not state)
        match sw & 0x006F {
            0x0000 => Self::NotReadyToSwitchOn,
            0x0040 => Self::SwitchOnDisabled,
            0x0021 => Self::ReadyToSwitchOn,
            0x0023 => Self::SwitchedOn,
            0x0027 => Self::OperationEnabled,
            0x0007 => Self::QuickStopActive,
            0x000F => Self::FaultReactionActive,
            0x0008 => Self::Fault,
            other  => Self::Unknown(other),
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Self::NotReadyToSwitchOn  =>
                "NotReadyToSwitchOn",
            Self::SwitchOnDisabled    =>
                "SwitchOnDisabled",
            Self::ReadyToSwitchOn     =>
                "ReadyToSwitchOn",
            Self::SwitchedOn          =>
                "SwitchedOn",
            Self::OperationEnabled    =>
                "OperationEnabled",
            Self::QuickStopActive     =>
                "QuickStopActive",
            Self::FaultReactionActive =>
                "FaultReactionActive",
            Self::Fault               =>
                "Fault",
            Self::Unknown(_)          =>
                "Unknown",
        }
    }

    pub fn is_fault(&self) -> bool {
        matches!(
            self,
            Self::Fault | Self::FaultReactionActive
        )
    }

    pub fn is_enabled(&self) -> bool {
        matches!(self, Self::OperationEnabled)
    }

    pub fn is_ready(&self) -> bool {
        matches!(
            self,
            Self::OperationEnabled |
            Self::SwitchedOn       |
            Self::ReadyToSwitchOn
        )
    }
}

impl std::fmt::Display for DriveState {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        write!(f, "{}", self.name())
    }
}

// ------------------------------------
// Operation modes
// what the drive does with our setpoints
// ------------------------------------

#[derive(Debug, Clone, Copy, PartialEq)]
#[repr(i8)]
pub enum OperationMode {
    // profile modes — drive handles ramp
    ProfilePosition  = 1,
    ProfileVelocity  = 3,
    ProfileTorque    = 4,

    // homing — drive executes homing procedure
    Homing           = 6,

    // cyclic sync — WE send setpoint every cycle
    // drive follows directly — best for RT control
    CyclicSyncPosition = 8,   // CSP ← use this
    CyclicSyncVelocity = 9,   // CSV
    CyclicSyncTorque   = 10,  // CST
}

impl OperationMode {
    pub fn name(&self) -> &'static str {
        match self {
            Self::ProfilePosition    => "ProfilePosition",
            Self::ProfileVelocity    => "ProfileVelocity",
            Self::ProfileTorque      => "ProfileTorque",
            Self::Homing             => "Homing",
            Self::CyclicSyncPosition => "CSP",
            Self::CyclicSyncVelocity => "CSV",
            Self::CyclicSyncTorque   => "CST",
        }
    }
}

// ------------------------------------
// Controlword
// what we send to drive each cycle
// bits have specific meanings per state
// ------------------------------------

pub struct Controlword;

impl Controlword {
    // state machine transitions
    pub const SHUTDOWN:         u16 = 0x0006;
    pub const SWITCH_ON:        u16 = 0x0007;
    pub const ENABLE_OPERATION: u16 = 0x000F;
    pub const DISABLE_VOLTAGE:  u16 = 0x0000;
    pub const QUICK_STOP:       u16 = 0x0002;
    pub const FAULT_RESET:      u16 = 0x0080;

    // operation mode specific
    // CSP normal operation
    pub const ENABLE_OP_CSP:    u16 = 0x001F;

    // homing — start homing procedure
    pub const HOMING_START:     u16 = 0x001F;
}

// ------------------------------------
// Statusword bits
// additional status beyond state machine
// ------------------------------------

pub struct Statusword;

impl Statusword {
    pub const TARGET_REACHED:    u16 = 0x0400;
    pub const INTERNAL_LIMIT:    u16 = 0x0800;
    pub const FOLLOWING_ERROR:   u16 = 0x2000;
    pub const HOMING_COMPLETE:   u16 = 0x1000;
    pub const HOMING_ERROR:      u16 = 0x2000;
    pub const DRIVE_REFERENCED:  u16 = 0x1000;
}

// ------------------------------------
// RxPDO — we send to drive each cycle
// layout must match drive ESI/XML config
// ------------------------------------

#[derive(Debug, Default, Clone, Copy)]
pub struct RxPDO {
    // controlword — state machine command
    pub controlword:     u16,

    // operation mode
    pub operation_mode:  i8,

    // setpoints — which is active depends on mode
    pub target_position: i32,   // counts (CSP)
    pub target_velocity: i32,   // counts/s (CSV)
    pub target_torque:   i16,   // 0.1% rated (CST)

    // limits
    pub max_torque:      u16,   // 0.1% rated
    pub max_current:     u16,   // 0.1% rated
}

// ------------------------------------
// TxPDO — drive sends to us each cycle
// ------------------------------------

#[derive(Debug, Default, Clone, Copy)]
pub struct TxPDO {
    // statusword — current state + status bits
    pub statusword:             u16,

    // actual operation mode
    pub operation_mode_display: i8,

    // feedback
    pub actual_position:        i32,   // counts
    pub actual_velocity:        i32,   // counts/s
    pub actual_torque:          i16,   // 0.1% rated

    // error tracking
    pub following_error:        i32,   // counts
    pub error_code:             u16,   // manufacturer specific
}

// ------------------------------------
// Homing methods
// CiA402 defines many homing methods
// most common listed here
// ------------------------------------

#[derive(Debug, Clone, Copy)]
#[repr(i8)]
pub enum HomingMethod {
    // negative limit switch
    NegativeLimitSwitch     = 1,
    // positive limit switch
    PositiveLimitSwitch     = 2,
    // home switch negative
    HomeSwitchNegative      = 3,
    // home switch positive
    HomeSwitchPositive      = 4,
    // current position is home
    CurrentPosition         = 35,
    // index pulse
    IndexPulseNegative      = 33,
    IndexPulsePositive      = 34,
}

// ------------------------------------
// CiA402Drive
// manages one servo axis
// state machine + unit conversion
// called every EtherCAT cycle
// ------------------------------------

pub struct CiA402Drive {
    pub name:            String,
    pub node_id:         u16,

    // current state — decoded from statusword
    pub state:           DriveState,

    // operation mode
    pub mode:            OperationMode,

    // unit conversion
    // drive works in encoder counts
    // user works in engineering units
    //
    // linear:  counts per mm
    // rotary:  counts per degree
    //          or counts per revolution
    counts_per_unit:     f64,

    // fault tracking
    pub error_code:      u16,
    fault_count:         u32,
    consecutive_faults:  u32,

    // following error threshold
    // if exceeded — fault the drive
    // in engineering units
    max_following_error: f64,

    // current PDO values
    pub rx:              RxPDO,
    pub tx:              TxPDO,

    // state machine timing
    // some drives need settling time
    // between state transitions
    state_timer:         u32,
}

impl CiA402Drive {
    pub fn new(
        name:                &str,
        node_id:             u16,
        mode:                OperationMode,
        counts_per_unit:     f64,
        max_following_error: f64,
    ) -> Self {
        info!(
            "CiA402 drive '{}' — \
             node {} mode {} \
             {:.0} counts/unit \
             max error {:.3} units",
            name,
            node_id,
            mode.name(),
            counts_per_unit,
            max_following_error,
        );

        Self {
            name:                name.to_string(),
            node_id,
            state:               DriveState::Unknown(0),
            mode,
            counts_per_unit,
            error_code:          0,
            fault_count:         0,
            consecutive_faults:  0,
            max_following_error,
            rx:                  RxPDO::default(),
            tx:                  TxPDO::default(),
            state_timer:         0,
        }
    }

    // ------------------------------------
    // Main update
    // called every EtherCAT cycle
    // takes fresh TxPDO from drive
    // returns RxPDO to send to drive
    // ------------------------------------

    pub fn update(
        &mut self,
        tx: TxPDO,
    ) -> RxPDO {
        self.tx = tx;

        // decode current state
        let new_state = DriveState::from_statusword(
            tx.statusword
        );

        // log state transitions
        if new_state != self.state {
            info!(
                "Drive '{}': {} → {}",
                self.name,
                self.state,
                new_state,
            );
            self.state       = new_state;
            self.state_timer = 0;
        }

        self.state_timer += 1;

        // track error code
        if tx.error_code != 0
        && tx.error_code != self.error_code {
            warn!(
                "Drive '{}' error code: \
                 0x{:04X}",
                self.name,
                tx.error_code,
            );
            self.error_code = tx.error_code;
        }

        // check following error
        self.check_following_error();

        // build controlword for next cycle
        self.rx.controlword    =
            self.next_controlword();
        self.rx.operation_mode =
            self.mode as i8;

        self.rx
    }

    // ------------------------------------
    // State machine
    // returns controlword to advance
    // toward OperationEnabled
    // ------------------------------------

    fn next_controlword(&mut self) -> u16 {
        match self.state {

            DriveState::NotReadyToSwitchOn => {
                // drive initializing
                // just wait — nothing to do
                Controlword::DISABLE_VOLTAGE
            }

            DriveState::SwitchOnDisabled => {
                // transition → ReadyToSwitchOn
                Controlword::SHUTDOWN
            }

            DriveState::ReadyToSwitchOn => {
                // transition → SwitchedOn
                // some drives need a settling cycle
                if self.state_timer > 2 {
                    Controlword::SWITCH_ON
                } else {
                    Controlword::SHUTDOWN
                }
            }

            DriveState::SwitchedOn => {
                // transition → OperationEnabled
                if self.state_timer > 2 {
                    Controlword::ENABLE_OPERATION
                } else {
                    Controlword::SWITCH_ON
                }
            }

            DriveState::OperationEnabled => {
                // running — normal operation
                // CSP mode — send setpoint each cycle
                self.consecutive_faults = 0;
                Controlword::ENABLE_OP_CSP
            }

            DriveState::QuickStopActive => {
                // motor stopping — wait
                // then go back to disabled
                if self.state_timer > 10 {
                    Controlword::DISABLE_VOLTAGE
                } else {
                    Controlword::QUICK_STOP
                }
            }

            DriveState::FaultReactionActive => {
                // drive handling fault
                // wait for it to complete
                Controlword::DISABLE_VOLTAGE
            }

            DriveState::Fault => {
                self.fault_count         += 1;
                self.consecutive_faults  += 1;

                if self.consecutive_faults == 1 {
                    warn!(
                        "Drive '{}' FAULT — \
                         error code 0x{:04X} \
                         total faults: {}",
                        self.name,
                        self.error_code,
                        self.fault_count,
                    );
                }

                // attempt fault reset
                // drive goes back to
                // SwitchOnDisabled if successful
                if self.state_timer > 5 {
                    debug!(
                        "Drive '{}' attempting \
                         fault reset",
                        self.name
                    );
                    Controlword::FAULT_RESET
                } else {
                    Controlword::DISABLE_VOLTAGE
                }
            }

            DriveState::Unknown(sw) => {
                warn!(
                    "Drive '{}' unknown \
                     statusword 0x{:04X}",
                    self.name,
                    sw,
                );
                Controlword::DISABLE_VOLTAGE
            }
        }
    }

    // ------------------------------------
    // Following error check
    // if actual position deviates too far
    // from commanded position — something
    // is mechanically wrong
    // ------------------------------------

    fn check_following_error(&self) {
        let error_units = self.tx.following_error
            as f64
            / self.counts_per_unit;

        if error_units.abs() >
            self.max_following_error
        {
            warn!(
                "Drive '{}' following error \
                 {:.3} units \
                 (max {:.3})",
                self.name,
                error_units,
                self.max_following_error,
            );
        }
    }

    // ------------------------------------
    // Setpoint commands
    // called by bus driver from IO image values
    // all in engineering units
    // conversion to counts happens here
    // rung never sees counts
    // ------------------------------------

    pub fn set_position(
        &mut self,
        units: f64,
    ) {
        self.rx.target_position =
            (units * self.counts_per_unit)
            as i32;
    }

    pub fn set_velocity(
        &mut self,
        units_per_sec: f64,
    ) {
        self.rx.target_velocity =
            (units_per_sec * self.counts_per_unit)
            as i32;
    }

    pub fn set_torque(
        &mut self,
        percent: f64,
    ) {
        // 0.1% units — multiply by 10
        self.rx.target_torque =
            (percent * 10.0) as i16;
    }

    pub fn set_max_torque(
        &mut self,
        percent: f64,
    ) {
        self.rx.max_torque =
            (percent * 10.0) as u16;
    }

    pub fn set_max_current(
        &mut self,
        percent: f64,
    ) {
        self.rx.max_current =
            (percent * 10.0) as u16;
    }

    // quick stop — stops motor fast
    // does not disable drive
    pub fn quick_stop(&mut self) {
        self.rx.controlword = Controlword::QUICK_STOP;
    }

    // disable voltage — drops drive immediately
    // use for emergency only
    pub fn disable(&mut self) {
        self.rx.controlword =
            Controlword::DISABLE_VOLTAGE;
    }

    // manually reset fault
    // called by rung via IO image fault_reset output
    pub fn reset_fault(&mut self) {
        if self.state.is_fault() {
            info!(
                "Drive '{}' fault reset \
                 requested",
                self.name
            );
            self.consecutive_faults = 0;
            self.error_code         = 0;
            self.state_timer        = 0;
        }
    }

    // ------------------------------------
    // Feedback — all in engineering units
    // called by bus driver to publish
    // to IO image each cycle
    // ------------------------------------

    pub fn actual_position(&self) -> f64 {
        self.tx.actual_position as f64
            / self.counts_per_unit
    }

    pub fn actual_velocity(&self) -> f64 {
        self.tx.actual_velocity as f64
            / self.counts_per_unit
    }

    pub fn actual_torque(&self) -> f64 {
        // 0.1% units — divide by 10
        self.tx.actual_torque as f64 / 10.0
    }

    pub fn following_error(&self) -> f64 {
        self.tx.following_error as f64
            / self.counts_per_unit
    }

    // ------------------------------------
    // Status flags
    // decoded from statusword
    // ------------------------------------

    pub fn is_enabled(&self) -> bool {
        self.state.is_enabled()
    }

    pub fn is_fault(&self) -> bool {
        self.state.is_fault()
    }

    pub fn is_target_reached(&self) -> bool {
        self.tx.statusword
            & Statusword::TARGET_REACHED != 0
    }

    pub fn is_homing_complete(&self) -> bool {
        self.tx.statusword
            & Statusword::HOMING_COMPLETE != 0
    }

    pub fn is_homing_error(&self) -> bool {
        self.tx.statusword
            & Statusword::HOMING_ERROR != 0
    }

    pub fn has_following_error(&self) -> bool {
        self.tx.statusword
            & Statusword::FOLLOWING_ERROR != 0
    }

    pub fn is_referenced(&self) -> bool {
        self.tx.statusword
            & Statusword::DRIVE_REFERENCED != 0
    }

    pub fn fault_count(&self) -> u32 {
        self.fault_count
    }

    pub fn error_code(&self) -> u16 {
        self.error_code
    }
}

// ------------------------------------
// IO image layout per drive
// indices relative to drive io_base
// ------------------------------------

// inputs (feedback from drive)
pub const IN_ACTUAL_POSITION:  usize = 0;
pub const IN_ACTUAL_VELOCITY:  usize = 1;
pub const IN_ACTUAL_TORQUE:    usize = 2;
pub const IN_FOLLOWING_ERROR:  usize = 3;
pub const IN_ENABLED:          usize = 4;
pub const IN_FAULT:            usize = 5;
pub const IN_TARGET_REACHED:   usize = 6;
pub const IN_HOMING_COMPLETE:  usize = 7;
pub const IN_ERROR_CODE:       usize = 8;
pub const IN_REFERENCED:       usize = 9;
pub const DRIVE_INPUT_COUNT:   usize = 10;

// outputs (commands to drive)
pub const OUT_TARGET_POSITION: usize = 0;
pub const OUT_TARGET_VELOCITY: usize = 1;
pub const OUT_TARGET_TORQUE:   usize = 2;
pub const OUT_MAX_TORQUE:      usize = 3;
pub const OUT_FAULT_RESET:     usize = 4;
pub const OUT_QUICK_STOP:      usize = 5;
pub const DRIVE_OUTPUT_COUNT:  usize = 6;

// ------------------------------------
// TOML config for a servo drive
//
// [device."robot.axis1"]
// bus              = "ethercat0"
// node             = 1
// type             = "servo_drive"
// counts_per_unit  = 10000.0
// max_following_error = 1.0
// mode             = "csp"
// ------------------------------------

// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_drive() -> CiA402Drive {
        CiA402Drive::new(
            "test_drive",
            1,
            OperationMode::CyclicSyncPosition,
            10000.0,  // 10000 counts/mm
            1.0,      // 1mm max following error
        )
    }

    // helper — build TxPDO with given statusword
    fn tx(statusword: u16) -> TxPDO {
        TxPDO {
            statusword,
            ..Default::default()
        }
    }

    // ------------------------------------
    // State machine tests
    // ------------------------------------

    #[test]
    fn test_state_machine_full_progression() {
        let mut drive = make_drive();

        // NotReadyToSwitchOn → nothing useful to send
        let rx = drive.update(tx(0x0000));
        assert_eq!(
            drive.state,
            DriveState::NotReadyToSwitchOn
        );
        assert_eq!(
            rx.controlword,
            Controlword::DISABLE_VOLTAGE
        );

        // SwitchOnDisabled → send Shutdown
        let rx = drive.update(tx(0x0040));
        assert_eq!(
            drive.state,
            DriveState::SwitchOnDisabled
        );
        assert_eq!(
            rx.controlword,
            Controlword::SHUTDOWN
        );

        // ReadyToSwitchOn → send SwitchOn after settling timer
        // state_timer resets on transition, so run 3 cycles
        let _ = drive.update(tx(0x0021)); // timer=1 → SHUTDOWN
        let _ = drive.update(tx(0x0021)); // timer=2 → SHUTDOWN
        let rx = drive.update(tx(0x0021)); // timer=3 > 2 → SWITCH_ON
        assert_eq!(
            drive.state,
            DriveState::ReadyToSwitchOn
        );
        assert_eq!(
            rx.controlword,
            Controlword::SWITCH_ON
        );

        // SwitchedOn → send EnableOperation after settling timer
        let _ = drive.update(tx(0x0023)); // timer=1 → SWITCH_ON
        let _ = drive.update(tx(0x0023)); // timer=2 → SWITCH_ON
        let rx = drive.update(tx(0x0023)); // timer=3 > 2 → ENABLE_OPERATION
        assert_eq!(
            drive.state,
            DriveState::SwitchedOn
        );
        assert_eq!(
            rx.controlword,
            Controlword::ENABLE_OPERATION
        );

        // OperationEnabled → running
        let rx = drive.update(tx(0x0027));
        assert_eq!(
            drive.state,
            DriveState::OperationEnabled
        );
        assert_eq!(
            rx.controlword,
            Controlword::ENABLE_OP_CSP
        );
        assert!(drive.is_enabled());
    }

    #[test]
    fn test_fault_handling() {
        let mut drive = make_drive();

        // go to fault state
        let rx = drive.update(tx(0x0008));
        assert_eq!(drive.state, DriveState::Fault);
        assert!(drive.is_fault());
        assert_eq!(drive.fault_count(), 1);

        // after timer — should attempt reset
        drive.state_timer = 10;
        let rx = drive.update(tx(0x0008));
        assert_eq!(
            rx.controlword,
            Controlword::FAULT_RESET
        );
    }

    #[test]
    fn test_fault_reset() {
        let mut drive = make_drive();

        // fault state
        drive.update(tx(0x0008));
        assert!(drive.is_fault());

        // manual reset — goes back to disabled
        drive.reset_fault();
        assert_eq!(drive.consecutive_faults, 0);
        assert_eq!(drive.error_code, 0);
    }

    #[test]
    fn test_statusword_decoding() {
        assert_eq!(
            DriveState::from_statusword(0x0000),
            DriveState::NotReadyToSwitchOn
        );
        assert_eq!(
            DriveState::from_statusword(0x0040),
            DriveState::SwitchOnDisabled
        );
        assert_eq!(
            DriveState::from_statusword(0x0021),
            DriveState::ReadyToSwitchOn
        );
        assert_eq!(
            DriveState::from_statusword(0x0023),
            DriveState::SwitchedOn
        );
        assert_eq!(
            DriveState::from_statusword(0x0027),
            DriveState::OperationEnabled
        );
        assert_eq!(
            DriveState::from_statusword(0x0007),
            DriveState::QuickStopActive
        );
        assert_eq!(
            DriveState::from_statusword(0x000F),
            DriveState::FaultReactionActive
        );
        assert_eq!(
            DriveState::from_statusword(0x0008),
            DriveState::Fault
        );
    }

    // ------------------------------------
    // Unit conversion tests
    // ------------------------------------

    #[test]
    fn test_position_counts_conversion() {
        let mut drive = make_drive();
        // 10000 counts per mm

        drive.set_position(1.0);
        assert_eq!(drive.rx.target_position, 10000);

        drive.set_position(1.5);
        assert_eq!(drive.rx.target_position, 15000);

        drive.set_position(-2.0);
        assert_eq!(drive.rx.target_position, -20000);

        drive.set_position(0.001);
        assert_eq!(drive.rx.target_position, 10);
    }

    #[test]
    fn test_velocity_counts_conversion() {
        let mut drive = make_drive();

        // 100 mm/s → 1000000 counts/s
        drive.set_velocity(100.0);
        assert_eq!(
            drive.rx.target_velocity,
            1_000_000
        );
    }

    #[test]
    fn test_torque_conversion() {
        let mut drive = make_drive();

        // 50% torque → 500 (0.1% units)
        drive.set_torque(50.0);
        assert_eq!(drive.rx.target_torque, 500);

        // 100% max torque
        drive.set_max_torque(100.0);
        assert_eq!(drive.rx.max_torque, 1000);
    }

    #[test]
    fn test_actual_position_conversion() {
        let mut drive = make_drive();

        let mut feedback = TxPDO::default();
        feedback.actual_position = 15000;
        drive.update(feedback);

        // 15000 counts / 10000 counts_per_mm = 1.5mm
        assert!(
            (drive.actual_position() - 1.5).abs()
            < 0.0001
        );
    }

    #[test]
    fn test_actual_velocity_conversion() {
        let mut drive = make_drive();

        let mut feedback = TxPDO::default();
        feedback.actual_velocity = 1_000_000;
        drive.update(feedback);

        // 1000000 counts/s / 10000 = 100 mm/s
        assert!(
            (drive.actual_velocity() - 100.0).abs()
            < 0.0001
        );
    }

    #[test]
    fn test_actual_torque_conversion() {
        let mut drive = make_drive();

        let mut feedback = TxPDO::default();
        feedback.actual_torque = 500;
        drive.update(feedback);

        // 500 * 0.1% = 50%
        assert!(
            (drive.actual_torque() - 50.0).abs()
            < 0.0001
        );
    }

    // ------------------------------------
    // Status bit tests
    // ------------------------------------

    #[test]
    fn test_target_reached() {
        let mut drive = make_drive();

        let mut feedback = TxPDO::default();
        feedback.statusword =
            0x0027 | Statusword::TARGET_REACHED;
        drive.update(feedback);

        assert!(drive.is_target_reached());
        assert!(drive.is_enabled());
    }

    #[test]
    fn test_homing_complete() {
        let mut drive = make_drive();

        let mut feedback = TxPDO::default();
        feedback.statusword =
            0x0027 | Statusword::HOMING_COMPLETE;
        drive.update(feedback);

        assert!(drive.is_homing_complete());
    }

    #[test]
    fn test_following_error_flag() {
        let mut drive = make_drive();

        let mut feedback = TxPDO::default();
        feedback.statusword =
            0x0027 | Statusword::FOLLOWING_ERROR;
        drive.update(feedback);

        assert!(drive.has_following_error());
    }

    #[test]
    fn test_is_referenced() {
        let mut drive = make_drive();

        let mut feedback = TxPDO::default();
        feedback.statusword =
            0x0027 | Statusword::DRIVE_REFERENCED;
        drive.update(feedback);

        assert!(drive.is_referenced());
    }

    // ------------------------------------
    // Operation mode tests
    // ------------------------------------

    #[test]
    fn test_operation_mode_set_in_pdo() {
        let mut drive = CiA402Drive::new(
            "test",
            1,
            OperationMode::CyclicSyncVelocity,
            10000.0,
            1.0,
        );

        let rx = drive.update(tx(0x0027));
        assert_eq!(
            rx.operation_mode,
            OperationMode::CyclicSyncVelocity as i8
        );
    }

    // ------------------------------------
    // Edge case tests
    // ------------------------------------

    #[test]
    fn test_unknown_statusword() {
        let mut drive = make_drive();

        // unknown statusword
        let rx = drive.update(tx(0xFFFF));
        assert!(matches!(
            drive.state,
            DriveState::Unknown(_)
        ));
        assert_eq!(
            rx.controlword,
            Controlword::DISABLE_VOLTAGE
        );
    }

    #[test]
    fn test_state_display() {
        assert_eq!(
            DriveState::OperationEnabled.to_string(),
            "OperationEnabled"
        );
        assert_eq!(
            DriveState::Fault.to_string(),
            "Fault"
        );
    }

    #[test]
    fn test_zero_position() {
        let mut drive = make_drive();
        drive.set_position(0.0);
        assert_eq!(drive.rx.target_position, 0);
    }

    #[test]
    fn test_consecutive_faults_tracked() {
        let mut drive = make_drive();

        // multiple fault cycles
        for _ in 0..5 {
            drive.update(tx(0x0008));
        }

        assert_eq!(drive.fault_count(), 5);
        assert_eq!(drive.consecutive_faults, 5);

        // recovery resets consecutive
        drive.update(tx(0x0027));
        assert_eq!(drive.consecutive_faults, 0);
        assert_eq!(drive.fault_count(), 5); // total kept
    }

    #[test]
    fn test_quick_stop() {
        let mut drive = make_drive();
        drive.update(tx(0x0027));
        assert!(drive.is_enabled());

        drive.quick_stop();
        assert_eq!(
            drive.rx.controlword,
            Controlword::QUICK_STOP
        );
    }
}