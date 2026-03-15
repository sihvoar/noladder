// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/core/arena.rs

use tracing::{debug, warn, error};

use crate::core::io_image::IOImage;
use crate::core::mailbox::{Mailbox, PAYLOAD_SIZE};
use crate::core::rung::{
    Rung,
    RungState,
    SuspendReason,
};

pub const MAX_RUNGS: usize = 256;

// ------------------------------------
// Arena
// owns all rungs
// executor calls poll_all each cycle
// ------------------------------------

pub struct Arena {
    slots: [Option<Rung>; MAX_RUNGS],
    count: usize,
}

impl Arena {
    pub fn new() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            count: 0,
        }
    }

    // add rung at startup only
    // panics if full — programming error
    pub fn add(&mut self, rung: Rung) {
        assert!(
            self.count < MAX_RUNGS,
            "Arena full — increase MAX_RUNGS or \
             reduce rung count"
        );
        debug!("Arena: adding rung '{}'", rung.name);
        self.slots[self.count] = Some(rung);
        self.count += 1;
    }

    // ------------------------------------
    // Main executor tick
    // called once per control cycle
    // order matters — rungs execute in
    // registration order
    // ------------------------------------

    pub fn poll_all(
        &mut self,
        io:       &mut IOImage,
        mailbox:  &mut Mailbox,
        cycle:    u64,
        cycle_ms: u32,
    ) {
        for slot in self.slots[..self.count].iter_mut() {
            if let Some(rung) = slot {
                match &rung.state {

                    // skip silently
                    RungState::Complete => {}

                    // skip — already logged at fault time
                    RungState::Faulted(_) => {}

                    // waiting — check condition first
                    // only poll if condition met
                    RungState::Waiting(_) => {
                        let should_wake =
                            rung.state.check(io);
                        if should_wake {
                            rung.poll(
                                io, mailbox,
                                cycle, cycle_ms,
                            );
                        }
                    }

                    // ready — poll immediately
                    RungState::Ready => {
                        rung.poll(
                            io, mailbox,
                            cycle, cycle_ms,
                        );
                    }
                }
            }
        }
    }

    // ------------------------------------
    // OS mailbox delivery
    // called before poll_all each cycle
    // marks waiting rung ready when
    // OS response arrives
    // result payload stored in rung
    // for retrieval after resume
    // ------------------------------------

    pub fn notify_os_response(
        &mut self,
        request_id: u32,
        result:     [u8; PAYLOAD_SIZE],
    ) {
        for slot in self.slots[..self.count].iter_mut() {
            if let Some(rung) = slot {
                if let RungState::Waiting(
                    SuspendReason::WaitOs(id)
                ) = &rung.state {
                    if *id == request_id {
                        debug!(
                            "Arena: OS response {} \
                             delivered to rung '{}'",
                            request_id,
                            rung.name
                        );
                        // store result — rung reads
                        // it after resuming
                        rung.os_result = Some(result);
                        rung.state     = RungState::Ready;
                        return;
                    }
                }
            }
        }

        warn!(
            "Arena: OS response {} has no waiting \
             rung — arrived after timeout?",
            request_id
        );
    }

    // ------------------------------------
    // Fault handling
    // ------------------------------------

    pub fn has_faults(&self) -> bool {
        self.slots[..self.count]
            .iter()
            .filter_map(|s| s.as_ref())
            .any(|r| matches!(
                r.state,
                RungState::Faulted(_)
            ))
    }

    pub fn log_faults(&self) {
        for slot in self.slots[..self.count].iter() {
            if let Some(rung) = slot {
                if let RungState::Faulted(
                    ref fault
                ) = rung.state {
                    error!(
                        "Rung '{}' faulted: {:?}",
                        rung.name,
                        fault
                    );
                }
            }
        }
    }

    // ------------------------------------
    // Reset operations
    // ------------------------------------

    pub fn reset_complete(&mut self) {
        for slot in self.slots[..self.count].iter_mut() {
            if let Some(rung) = slot {
                if matches!(
                    rung.state,
                    RungState::Complete
                ) {
                    rung.reset();
                }
            }
        }
    }

    pub fn reset_rung(&mut self, name: &str) -> bool {
        for slot in self.slots[..self.count].iter_mut() {
            if let Some(rung) = slot {
                if rung.name == name {
                    rung.reset();
                    return true;
                }
            }
        }
        warn!(
            "Arena: reset_rung '{}' not found",
            name
        );
        false
    }

    pub fn reset_all_faults(&mut self) {
        for slot in self.slots[..self.count].iter_mut() {
            if let Some(rung) = slot {
                if matches!(
                    rung.state,
                    RungState::Faulted(_)
                ) {
                    warn!(
                        "Arena: resetting faulted \
                         rung '{}'",
                        rung.name
                    );
                    rung.reset();
                }
            }
        }
    }

    // ------------------------------------
    // Diagnostics
    // ------------------------------------

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn stats(&self) -> ArenaStats {
        let mut stats = ArenaStats {
            total: self.count,
            ..Default::default()
        };

        for slot in self.slots[..self.count].iter() {
            if let Some(rung) = slot {
                match &rung.state {
                    RungState::Ready =>
                        stats.ready    += 1,
                    RungState::Complete =>
                        stats.complete += 1,
                    RungState::Faulted(_) =>
                        stats.faulted  += 1,
                    RungState::Waiting(reason) => {
                        stats.waiting += 1;
                        match reason {
                            SuspendReason::WaitFor(..) =>
                                stats.waiting_io   += 1,
                            SuspendReason::WaitForAny(..) =>
                                stats.waiting_io   += 1,
                            SuspendReason::WaitForAll { .. } =>
                                stats.waiting_io   += 1,
                            SuspendReason::WaitCycles(_) =>
                                stats.waiting_time += 1,
                            SuspendReason::WaitOs(_) =>
                                stats.waiting_os   += 1,
                        }
                    }
                }
            }
        }
        stats
    }
}

// ------------------------------------
// Stats
// ------------------------------------

#[derive(Debug, Default)]
pub struct ArenaStats {
    pub total:        usize,
    pub ready:        usize,
    pub waiting:      usize,
    pub waiting_io:   usize,
    pub waiting_time: usize,
    pub waiting_os:   usize,
    pub complete:     usize,
    pub faulted:      usize,
}

impl std::fmt::Display for ArenaStats {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        write!(
            f,
            "rungs: {} total \
             {} ready \
             {} waiting ({} io / {} time / {} os) \
             {} complete \
             {} faulted",
            self.total,
            self.ready,
            self.waiting,
            self.waiting_io,
            self.waiting_time,
            self.waiting_os,
            self.complete,
            self.faulted,
        )
    }
}

// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::io_image::{
        IOImage,
        InputIndex,
        Value,
    };
    use crate::core::mailbox::Mailbox;
    use crate::core::rung::{SuspendReason, RungFault};
    use crate::rung;

    fn make_io() -> Box<IOImage> {
        IOImage::allocate()
    }

    fn make_mailbox() -> Mailbox {
        Mailbox::new()
    }

    #[test]
    fn test_add_and_poll() {
        let mut arena = Arena::new();
        let mut io    = make_io();

        let ran  = std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(false)
        );
        let ran2 = ran.clone();

        arena.add(rung!(test, {
            ran2.store(
                true,
                std::sync::atomic::Ordering::SeqCst
            );
        }));

        let mut mb = make_mailbox();
        arena.poll_all(&mut io, &mut mb, 1, 10);

        assert!(
            ran.load(std::sync::atomic::Ordering::SeqCst)
        );
    }

    #[test]
    fn test_complete_not_polled_again() {
        let mut arena = Arena::new();
        let mut io    = make_io();
        let mut mb    = make_mailbox();

        let count  = std::sync::Arc::new(
            std::sync::atomic::AtomicU32::new(0)
        );
        let count2 = count.clone();

        arena.add(rung!(counter, {
            count2.fetch_add(
                1,
                std::sync::atomic::Ordering::SeqCst
            );
        }));

        arena.poll_all(&mut io, &mut mb, 1, 10);
        arena.poll_all(&mut io, &mut mb, 2, 10);
        arena.poll_all(&mut io, &mut mb, 3, 10);

        assert_eq!(
            count.load(std::sync::atomic::Ordering::SeqCst),
            1
        );
    }

    #[test]
    fn test_waiting_rung_wakes_on_condition() {
        let mut arena = Arena::new();
        let mut io    = make_io();
        let sensor    = InputIndex(0);

        arena.add(rung!(waiter, {}));

        if let Some(rung) = &mut arena.slots[0] {
            rung.state = RungState::Waiting(
                SuspendReason::WaitFor(
                    sensor,
                    Value::Bool(true)
                )
            );
        }

        let mut mb = make_mailbox();

        // not met — stays waiting
        io.publish_inputs(0, Value::Bool(false));
        io.snapshot();
        arena.poll_all(&mut io, &mut mb, 1, 10);
        assert!(matches!(
            arena.slots[0].as_ref().unwrap().state,
            RungState::Waiting(_)
        ));

        // met — wakes and completes
        io.publish_inputs(0, Value::Bool(true));
        io.snapshot();
        arena.poll_all(&mut io, &mut mb, 2, 10);
        assert!(matches!(
            arena.slots[0].as_ref().unwrap().state,
            RungState::Complete
        ));
    }

    #[test]
    fn test_os_response_delivery() {
        let mut arena = Arena::new();

        arena.add(rung!(os_waiter, {}));

        if let Some(rung) = &mut arena.slots[0] {
            rung.state = RungState::Waiting(
                SuspendReason::WaitOs(42)
            );
        }

        assert_eq!(arena.stats().waiting_os, 1);

        let result = [0u8; PAYLOAD_SIZE];
        arena.notify_os_response(42, result);

        assert_eq!(arena.stats().ready,      1);
        assert_eq!(arena.stats().waiting_os, 0);

        // result stored in rung
        assert!(
            arena.slots[0]
                .as_ref()
                .unwrap()
                .os_result
                .is_some()
        );
    }

    #[test]
    fn test_os_result_cleared_after_read() {
        let mut arena = Arena::new();
        let mut io    = make_io();

        arena.add(rung!(os_waiter, {}));

        if let Some(rung) = &mut arena.slots[0] {
            rung.state = RungState::Waiting(
                SuspendReason::WaitOs(1)
            );
        }

        let mut result = [0u8; PAYLOAD_SIZE];
        result[0] = 42;

        arena.notify_os_response(1, result);
        let mut mb = make_mailbox();
        arena.poll_all(&mut io, &mut mb, 1, 10);

        // after poll os_result consumed
        assert!(
            arena.slots[0]
                .as_ref()
                .unwrap()
                .os_result
                .is_none()
        );
    }

    #[test]
    fn test_stats_breakdown() {
        let mut arena = Arena::new();
        let mut io    = make_io();

        arena.add(rung!(completes,   {}));
        arena.add(rung!(waits_io,    {}));
        arena.add(rung!(waits_time,  {}));
        arena.add(rung!(waits_os,    {}));

        if let Some(r) = &mut arena.slots[1] {
            r.state = RungState::Waiting(
                SuspendReason::WaitFor(
                    InputIndex(0),
                    Value::Bool(true)
                )
            );
        }
        if let Some(r) = &mut arena.slots[2] {
            r.state = RungState::Waiting(
                SuspendReason::WaitCycles(100)
            );
        }
        if let Some(r) = &mut arena.slots[3] {
            r.state = RungState::Waiting(
                SuspendReason::WaitOs(1)
            );
        }

        let mut mb = make_mailbox();
        arena.poll_all(&mut io, &mut mb, 1, 10);

        let stats = arena.stats();
        assert_eq!(stats.total,        4);
        assert_eq!(stats.complete,     1);
        assert_eq!(stats.waiting,      3);
        assert_eq!(stats.waiting_io,   1);
        assert_eq!(stats.waiting_time, 1);
        assert_eq!(stats.waiting_os,   1);
    }

    #[test]
    fn test_reset_complete() {
        let mut arena = Arena::new();
        let mut io    = make_io();

        arena.add(rung!(r1, {}));
        let mut mb = make_mailbox();
        arena.poll_all(&mut io, &mut mb, 1, 10);

        assert_eq!(arena.stats().complete, 1);
        arena.reset_complete();
        assert_eq!(arena.stats().ready,    1);
        assert_eq!(arena.stats().complete, 0);
    }

    #[test]
    fn test_reset_named_rung() {
        let mut arena = Arena::new();
        let mut io    = make_io();

        arena.add(rung!(named_rung, {}));
        let mut mb = make_mailbox();
        arena.poll_all(&mut io, &mut mb, 1, 10);

        assert!(arena.reset_rung("named_rung"));
        assert_eq!(arena.stats().ready, 1);
    }

    #[test]
    fn test_reset_unknown_returns_false() {
        let mut arena = Arena::new();
        assert!(!arena.reset_rung("does_not_exist"));
    }

    #[test]
    fn test_has_faults() {
        let mut arena = Arena::new();

        arena.add(rung!(faulted, {}));
        if let Some(rung) = &mut arena.slots[0] {
            rung.state = RungState::Faulted(
                RungFault::Timeout
            );
        }

        assert!(arena.has_faults());
        arena.reset_all_faults();
        assert!(!arena.has_faults());
    }

    #[test]
    #[should_panic]
    fn test_overflow_panics() {
        let mut arena = Arena::new();
        for _ in 0..=MAX_RUNGS {
            arena.add(rung!(overflow, {}));
        }
    }
}