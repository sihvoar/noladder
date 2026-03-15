// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/core/rung.rs

use std::cell::Cell;
use std::future::Future;
use std::pin::Pin;
use std::task::{
    Context, Poll,
    RawWaker, RawWakerVTable, Waker,
};
use tracing::{debug, warn};

use crate::core::io_image::{
    IOImage,
    InputIndex,
    OutputIndex,
    Value,
};
use crate::core::mailbox::{
    Mailbox,
    PAYLOAD_SIZE,
};

// ------------------------------------
// Constants
// ------------------------------------

const MAX_CONDITIONS: usize = 16;

// ------------------------------------
// Suspend reasons
// ------------------------------------

#[derive(Debug)]
pub enum SuspendReason {
    WaitFor(InputIndex, Value),

    WaitForAny(
        [(InputIndex, Value); MAX_CONDITIONS],
        usize,   // count
    ),

    WaitForAll {
        conditions: [(InputIndex, Value); MAX_CONDITIONS],
        count:      usize,
        met:        [bool; MAX_CONDITIONS],
    },

    WaitCycles(u32),

    WaitOs(u32),  // request ID
}

// ------------------------------------
// Rung state
// ------------------------------------

#[derive(Debug)]
pub enum RungState {
    Ready,
    Waiting(SuspendReason),
    Complete,
    Faulted(RungFault),
}

impl PartialEq for RungState {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (RungState::Ready,      RungState::Ready)      |
            (RungState::Complete,   RungState::Complete)   |
            (RungState::Faulted(_), RungState::Faulted(_)) |
            (RungState::Waiting(_), RungState::Waiting(_))
        )
    }
}

impl RungState {
    // check if waiting condition is now met
    // returns true if rung should wake
    // mutates WaitForAll met flags in place
    pub fn check(&mut self, io: &IOImage) -> bool {
        match self {
            RungState::Waiting(
                SuspendReason::WaitFor(idx, expected)
            ) => {
                io.read_input(*idx) == *expected
            }

            RungState::Waiting(
                SuspendReason::WaitForAny(
                    conditions, count
                )
            ) => {
                conditions[..*count]
                    .iter()
                    .any(|(idx, expected)| {
                        io.read_input(*idx) == *expected
                    })
            }

            RungState::Waiting(
                SuspendReason::WaitForAll {
                    conditions,
                    count,
                    met,
                }
            ) => {
                for i in 0..*count {
                    if !met[i] {
                        let (idx, expected) =
                            conditions[i];
                        if io.read_input(idx) == expected
                        {
                            met[i] = true;
                        }
                    }
                }
                met[..*count].iter().all(|&m| m)
            }

            RungState::Waiting(
                SuspendReason::WaitCycles(remaining)
            ) => {
                if *remaining == 0 {
                    true
                } else {
                    *remaining -= 1;
                    false
                }
            }

            // woken externally by mailbox delivery
            RungState::Waiting(
                SuspendReason::WaitOs(_)
            ) => false,

            RungState::Ready      => true,
            RungState::Complete   => false,
            RungState::Faulted(_) => false,
        }
    }
}

// ------------------------------------
// Fault reasons
// ------------------------------------

#[derive(Debug, PartialEq, Clone)]
pub enum RungFault {
    Panicked,
    Timeout,
    InvalidIO(usize),
    MailboxFull,
}

// ------------------------------------
// Raw context — no lifetime parameters
// stored in thread-local during poll
// futures read IO / write outputs through this
// ------------------------------------

struct RawCtx {
    io_ptr:      *mut IOImage,
    mailbox_ptr: *mut Mailbox,
    _cycle:      u64,
    cycle_ms:    u32,

    // set by yield primitives during poll
    // read by Rung::poll after future returns Pending
    suspend:     Option<SuspendReason>,

    // OS result stored by arena::notify_os_response
    // consumed by OsRequest future after resume
    os_result:   Option<[u8; PAYLOAD_SIZE]>,
}

thread_local! {
    // pointer to current cycle's RawCtx
    // null when not inside Rung::poll
    static RAW_CTX: Cell<*mut RawCtx> =
        Cell::new(std::ptr::null_mut());
}

// ------------------------------------
// RungContextAccessor
// zero-size struct injected as `ctx` by rung! macro
// all methods reach through TLS to current RawCtx
// ------------------------------------

pub struct RungContextAccessor;

impl RungContextAccessor {
    pub fn read_bool(&self, idx: InputIndex) -> bool {
        let ptr = RAW_CTX.with(|c| c.get());
        if ptr.is_null() { return false; }
        let io = unsafe { &*(*ptr).io_ptr };
        io.read_input(idx).as_bool().unwrap_or(false)
    }

    pub fn read_float(&self, idx: InputIndex) -> f32 {
        let ptr = RAW_CTX.with(|c| c.get());
        if ptr.is_null() { return 0.0; }
        let io = unsafe { &*(*ptr).io_ptr };
        io.read_input(idx).as_float().unwrap_or(0.0)
    }

    pub fn read_int(&self, idx: InputIndex) -> i32 {
        let ptr = RAW_CTX.with(|c| c.get());
        if ptr.is_null() { return 0; }
        let io = unsafe { &*(*ptr).io_ptr };
        io.read_input(idx).as_int().unwrap_or(0)
    }

    pub fn write(
        &self,
        idx:   OutputIndex,
        value: impl Into<Value>,
    ) {
        let ptr = RAW_CTX.with(|c| c.get());
        if ptr.is_null() { return; }
        let io = unsafe { &mut *(*ptr).io_ptr };
        io.write_output(idx, value);
    }

    pub fn yield_until(
        &self,
        index:    InputIndex,
        expected: impl Into<Value>,
    ) -> WaitFor {
        WaitFor {
            index,
            expected: expected.into(),
        }
    }

    pub fn yield_until_any(
        &self,
        conditions: &[(InputIndex, Value)],
    ) -> WaitForAny {
        WaitForAny::new(conditions)
    }

    pub fn yield_until_all(
        &self,
        conditions: &[(InputIndex, Value)],
    ) -> WaitForAll {
        WaitForAll::new(conditions)
    }

    pub fn yield_cycles(&self, n: u32) -> WaitCycles {
        WaitCycles { remaining: n }
    }

    pub fn yield_ms(&self, ms: u32) -> WaitCycles {
        let ptr = RAW_CTX.with(|c| c.get());
        let cycle_ms = if ptr.is_null() {
            10
        } else {
            unsafe { (*ptr).cycle_ms }
        };
        let cycles = (ms as f32
            / cycle_ms as f32).ceil() as u32;
        WaitCycles { remaining: cycles }
    }

    pub fn race<A, B>(
        &self,
        first:  A,
        second: B,
    ) -> Race<A, B>
    where
        A: Future<Output = ()>,
        B: Future<Output = ()>,
    {
        Race::new(first, second)
    }

    pub fn os_request(
        &self,
        key:     &str,
        payload: &[u8],
    ) -> OsRequest {
        let ptr = RAW_CTX.with(|c| c.get());
        if ptr.is_null() {
            panic!("os_request called outside rung context");
        }
        let mailbox = unsafe { &mut *(*ptr).mailbox_ptr };
        let id = mailbox.post(key, payload)
            .expect("Mailbox full — increase MAILBOX_SIZE");
        OsRequest { request_id: id, suspended: false }
    }
}

// ------------------------------------
// WaitFor — single condition
// ------------------------------------

pub struct WaitFor {
    pub index:    InputIndex,
    pub expected: Value,
}

impl Future for WaitFor {
    type Output = ();

    fn poll(
        self: Pin<&mut Self>,
        _cx:  &mut Context<'_>,
    ) -> Poll<()> {
        let ptr = RAW_CTX.with(|c| c.get());
        if !ptr.is_null() {
            let io = unsafe { &*(*ptr).io_ptr };
            if io.read_input(self.index) == self.expected {
                return Poll::Ready(());
            }
            unsafe {
                (*ptr).suspend = Some(
                    SuspendReason::WaitFor(
                        self.index,
                        self.expected,
                    )
                );
            }
        }
        Poll::Pending
    }
}

// ------------------------------------
// WaitForAny — OR logic
// returns which index fired
// ------------------------------------

pub struct WaitForAny {
    pub conditions: [(InputIndex, Value); MAX_CONDITIONS],
    pub count:      usize,
}

impl WaitForAny {
    pub fn new(
        conditions: &[(InputIndex, Value)]
    ) -> Self {
        assert!(
            conditions.len() <= MAX_CONDITIONS,
            "Too many conditions — max {}",
            MAX_CONDITIONS
        );

        let mut slots = [
            (InputIndex(0), Value::Unset);
            MAX_CONDITIONS
        ];
        for (i, c) in conditions.iter().enumerate() {
            slots[i] = *c;
        }

        Self {
            conditions: slots,
            count:      conditions.len(),
        }
    }

    pub fn check(&self, io: &IOImage) -> Option<usize> {
        for i in 0..self.count {
            let (idx, expected) = self.conditions[i];
            if io.read_input(idx) == expected {
                return Some(i);
            }
        }
        None
    }
}

impl Future for WaitForAny {
    type Output = usize;

    fn poll(
        self: Pin<&mut Self>,
        _cx:  &mut Context<'_>,
    ) -> Poll<usize> {
        let ptr = RAW_CTX.with(|c| c.get());
        if !ptr.is_null() {
            let io = unsafe { &*(*ptr).io_ptr };
            if let Some(which) = self.check(io) {
                return Poll::Ready(which);
            }
            let conds = self.conditions;
            let count = self.count;
            unsafe {
                (*ptr).suspend = Some(
                    SuspendReason::WaitForAny(
                        conds, count
                    )
                );
            }
        }
        Poll::Pending
    }
}

// ------------------------------------
// WaitForAll — AND logic
// ------------------------------------

pub struct WaitForAll {
    pub conditions: [(InputIndex, Value); MAX_CONDITIONS],
    pub count:      usize,
    pub met:        [bool; MAX_CONDITIONS],
}

impl WaitForAll {
    pub fn new(
        conditions: &[(InputIndex, Value)]
    ) -> Self {
        assert!(
            conditions.len() <= MAX_CONDITIONS,
            "Too many conditions — max {}",
            MAX_CONDITIONS
        );

        let mut slots = [
            (InputIndex(0), Value::Unset);
            MAX_CONDITIONS
        ];
        for (i, c) in conditions.iter().enumerate() {
            slots[i] = *c;
        }

        Self {
            conditions: slots,
            count:      conditions.len(),
            met:        [false; MAX_CONDITIONS],
        }
    }

    pub fn check(&mut self, io: &IOImage) -> bool {
        for i in 0..self.count {
            if !self.met[i] {
                let (idx, expected) =
                    self.conditions[i];
                if io.read_input(idx) == expected {
                    self.met[i] = true;
                }
            }
        }
        self.met[..self.count]
            .iter()
            .all(|&m| m)
    }
}

impl Future for WaitForAll {
    type Output = ();

    fn poll(
        mut self: Pin<&mut Self>,
        _cx:      &mut Context<'_>,
    ) -> Poll<()> {
        let ptr = RAW_CTX.with(|c| c.get());
        if !ptr.is_null() {
            let io = unsafe { &*(*ptr).io_ptr };
            let done = {
                // check and update met flags inline
                let s = self.as_mut().get_mut();
                s.check(io)
            };
            if done {
                return Poll::Ready(());
            }
            let conditions = self.conditions;
            let count      = self.count;
            let met        = self.met;
            unsafe {
                (*ptr).suspend = Some(
                    SuspendReason::WaitForAll {
                        conditions, count, met
                    }
                );
            }
        }
        Poll::Pending
    }
}

// ------------------------------------
// WaitCycles — time based
// decrements each poll
// no TLS needed — internal countdown
// ------------------------------------

pub struct WaitCycles {
    pub remaining: u32,
}

impl Future for WaitCycles {
    type Output = ();

    fn poll(
        mut self: Pin<&mut Self>,
        _cx:      &mut Context<'_>,
    ) -> Poll<()> {
        if self.remaining == 0 {
            Poll::Ready(())
        } else {
            self.remaining -= 1;
            Poll::Pending
        }
    }
}

// ------------------------------------
// OsRequest — async OS call
// posts to mailbox on first poll
// suspends until arena delivers result
// returns raw bytes to caller
// ------------------------------------

pub struct OsRequest {
    pub request_id: u32,
    suspended:      bool,
}

impl Future for OsRequest {
    type Output = [u8; PAYLOAD_SIZE];

    fn poll(
        mut self: Pin<&mut Self>,
        _cx:      &mut Context<'_>,
    ) -> Poll<[u8; PAYLOAD_SIZE]> {
        let ptr = RAW_CTX.with(|c| c.get());
        if ptr.is_null() {
            return Poll::Pending;
        }
        let raw = unsafe { &mut *ptr };

        if self.suspended {
            // already posted — check if result arrived
            if let Some(result) = raw.os_result.take() {
                return Poll::Ready(result);
            }
        }

        // set suspend reason — arena will wake us
        raw.suspend = Some(
            SuspendReason::WaitOs(self.request_id)
        );
        self.suspended = true;
        Poll::Pending
    }
}

// ------------------------------------
// Race — first future wins
// ------------------------------------

#[derive(Debug, PartialEq)]
pub enum RaceResult {
    First,
    Second,
}

pub struct Race<A, B>
where
    A: Future<Output = ()>,
    B: Future<Output = ()>,
{
    first:  A,
    second: B,
}

impl<A, B> Race<A, B>
where
    A: Future<Output = ()>,
    B: Future<Output = ()>,
{
    pub fn new(first: A, second: B) -> Self {
        Self { first, second }
    }
}

impl<A, B> Future for Race<A, B>
where
    A: Future<Output = ()>,
    B: Future<Output = ()>,
{
    type Output = RaceResult;

    fn poll(
        mut self: Pin<&mut Self>,
        cx:       &mut Context<'_>,
    ) -> Poll<RaceResult> {
        // SAFETY: fields are pinned because self is pinned
        unsafe {
            let this = self.as_mut().get_unchecked_mut();
            if let Poll::Ready(()) =
                Pin::new_unchecked(&mut this.first).poll(cx)
            {
                return Poll::Ready(RaceResult::First);
            }
            if let Poll::Ready(()) =
                Pin::new_unchecked(&mut this.second).poll(cx)
            {
                return Poll::Ready(RaceResult::Second);
            }
        }
        Poll::Pending
    }
}

// ------------------------------------
// Rung
// owns its future
// executor polls each cycle
// ------------------------------------

pub struct Rung {
    pub name:           &'static str,
    pub state:          RungState,
    pub os_result:      Option<[u8; PAYLOAD_SIZE]>,
    future:             Pin<Box<
                            dyn Future<Output = ()>
                            + Send
                        >>,
    last_active:        u64,
    timeout_cycles:     Option<u64>,
}

impl Rung {
    pub fn new(
        name:           &'static str,
        future:         impl Future<Output = ()>
                        + Send + 'static,
        timeout_cycles: Option<u64>,
    ) -> Self {
        Self {
            name,
            state:          RungState::Ready,
            os_result:      None,
            future:         Box::pin(future),
            last_active:    0,
            timeout_cycles,
        }
    }

    pub fn poll(
        &mut self,
        io:       &mut IOImage,
        mailbox:  &mut Mailbox,
        cycle:    u64,
        cycle_ms: u32,
    ) -> &RungState {

        // check timeout
        if let Some(timeout) = self.timeout_cycles {
            if matches!(
                self.state,
                RungState::Waiting(_)
            ) {
                if cycle - self.last_active > timeout {
                    warn!(
                        "Rung '{}' timed out \
                         after {} cycles",
                        self.name, timeout
                    );
                    self.state = RungState::Faulted(
                        RungFault::Timeout
                    );
                    return &self.state;
                }
            }
        }

        // fast-path condition check
        let should_wake = self.state.check(io);
        if !should_wake {
            return &self.state;
        }

        self.state       = RungState::Ready;
        self.last_active = cycle;

        // build raw context for this poll
        // SAFETY: RawCtx lives on the stack for
        // the duration of future.poll below
        // TLS cleared before returning
        let mut raw = RawCtx {
            io_ptr:      io  as *mut IOImage,
            mailbox_ptr: mailbox as *mut Mailbox,
            _cycle:      cycle,
            cycle_ms,
            suspend:     None,
            os_result:   self.os_result.take(),
        };

        RAW_CTX.with(|c| c.set(
            &mut raw as *mut RawCtx
        ));

        let waker  = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let result = self.future.as_mut().poll(&mut cx);

        // clear TLS before any early return
        RAW_CTX.with(|c| c.set(
            std::ptr::null_mut()
        ));

        match result {
            Poll::Ready(()) => {
                debug!(
                    "Rung '{}' complete",
                    self.name
                );
                self.state = RungState::Complete;
            }
            Poll::Pending => {
                if let Some(reason) =
                    raw.suspend.take()
                {
                    self.state =
                        RungState::Waiting(reason);
                } else if matches!(
                    self.state,
                    RungState::Ready
                ) {
                    // no explicit suspend —
                    // poll again next cycle
                    self.state = RungState::Waiting(
                        SuspendReason::WaitCycles(1)
                    );
                }
            }
        }

        &self.state
    }

    pub fn is_done(&self) -> bool {
        matches!(
            self.state,
            RungState::Complete | RungState::Faulted(_)
        )
    }

    pub fn reset(&mut self) {
        if matches!(
            self.state,
            RungState::Faulted(_)
        ) {
            warn!(
                "Rung '{}' reset from faulted state",
                self.name
            );
        }
        self.state     = RungState::Ready;
        self.os_result = None;
    }
}

// ------------------------------------
// Convenience macro
// injects `ctx: RungContextAccessor`
// and `use RaceResult` into async body
// ------------------------------------

#[macro_export]
macro_rules! rung {
    // with explicit ctx identifier: rung!(name, |ctx| { body })
    ($name:ident, |$ctx:ident| $body:block) => {
        $crate::core::rung::Rung::new(
            stringify!($name),
            async move {
                #[allow(unused_imports)]
                use $crate::core::rung::RaceResult;
                let $ctx = $crate::core::rung::RungContextAccessor;
                $body
            },
            None,
        )
    };
    // with explicit ctx + timeout
    ($name:ident, timeout: $t:expr, |$ctx:ident| $body:block) => {
        $crate::core::rung::Rung::new(
            stringify!($name),
            async move {
                #[allow(unused_imports)]
                use $crate::core::rung::RaceResult;
                let $ctx = $crate::core::rung::RungContextAccessor;
                $body
            },
            Some($t),
        )
    };
    // simple form — no ctx needed
    ($name:ident, $body:expr) => {
        $crate::core::rung::Rung::new(
            stringify!($name),
            async move { $body },
            None,
        )
    };
    ($name:ident, timeout: $t:expr, $body:expr) => {
        $crate::core::rung::Rung::new(
            stringify!($name),
            async move { $body },
            Some($t),
        )
    };
}

// ------------------------------------
// Noop waker
// cycle timer is the wakeup signal
// not the waker mechanism
// ------------------------------------

fn noop_waker() -> Waker {
    const VTABLE: RawWakerVTable = RawWakerVTable::new(
        |_| RawWaker::new(
            std::ptr::null(), &VTABLE
        ),
        |_| {},
        |_| {},
        |_| {},
    );
    unsafe {
        Waker::from_raw(
            RawWaker::new(
                std::ptr::null(), &VTABLE
            )
        )
    }
}

// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::io_image::IOImage;
    use crate::core::mailbox::Mailbox;

    fn make_io() -> Box<IOImage> {
        IOImage::allocate()
    }

    fn make_mailbox() -> Mailbox {
        Mailbox::new()
    }

    #[test]
    fn test_rung_completes() {
        let mut io      = make_io();
        let mut mailbox = make_mailbox();
        let ran         = std::sync::Arc::new(
            std::sync::atomic::AtomicBool::new(false)
        );
        let ran2 = ran.clone();

        let mut rung = rung!(test_rung, {
            ran2.store(
                true,
                std::sync::atomic::Ordering::SeqCst
            );
        });

        rung.poll(&mut io, &mut mailbox, 1, 10);

        assert!(
            ran.load(std::sync::atomic::Ordering::SeqCst)
        );
        assert!(matches!(
            rung.state,
            RungState::Complete
        ));
    }

    #[test]
    fn test_wait_for_condition() {
        let mut io      = make_io();
        let mut mailbox = make_mailbox();
        let sensor      = InputIndex(0);
        let mut rung    = rung!(wait_test, {});

        rung.state = RungState::Waiting(
            SuspendReason::WaitFor(
                sensor,
                Value::Bool(true)
            )
        );

        // not met
        io.publish_inputs(0, Value::Bool(false));
        io.snapshot();
        rung.poll(&mut io, &mut mailbox, 1, 10);
        assert!(matches!(
            rung.state,
            RungState::Waiting(_)
        ));

        // met
        io.publish_inputs(0, Value::Bool(true));
        io.snapshot();
        rung.poll(&mut io, &mut mailbox, 2, 10);
        assert!(matches!(
            rung.state,
            RungState::Complete
        ));
    }

    #[test]
    fn test_wait_for_any() {
        let mut io = make_io();
        let a      = InputIndex(0);
        let b      = InputIndex(1);

        let waiter = WaitForAny::new(&[
            (a, Value::Bool(true)),
            (b, Value::Bool(true)),
        ]);

        io.publish_inputs(0, Value::Bool(false));
        io.publish_inputs(1, Value::Bool(false));
        io.snapshot();
        assert_eq!(waiter.check(&io), None);

        io.publish_inputs(1, Value::Bool(true));
        io.snapshot();
        assert_eq!(waiter.check(&io), Some(1));
    }

    #[test]
    fn test_wait_for_all() {
        let mut io     = make_io();
        let a          = InputIndex(0);
        let b          = InputIndex(1);
        let mut waiter = WaitForAll::new(&[
            (a, Value::Bool(true)),
            (b, Value::Bool(true)),
        ]);

        io.publish_inputs(0, Value::Bool(false));
        io.publish_inputs(1, Value::Bool(false));
        io.snapshot();
        assert!(!waiter.check(&io));

        io.publish_inputs(0, Value::Bool(true));
        io.snapshot();
        assert!(!waiter.check(&io));

        io.publish_inputs(1, Value::Bool(true));
        io.snapshot();
        assert!(waiter.check(&io));
    }

    #[test]
    fn test_wait_cycles_countdown() {
        let waker      = noop_waker();
        let mut cx     = Context::from_waker(&waker);
        let mut future = WaitCycles { remaining: 3 };
        let mut pinned = Pin::new(&mut future);

        assert_eq!(
            pinned.as_mut().poll(&mut cx),
            Poll::Pending
        );
        assert_eq!(
            pinned.as_mut().poll(&mut cx),
            Poll::Pending
        );
        assert_eq!(
            pinned.as_mut().poll(&mut cx),
            Poll::Pending
        );
        assert_eq!(
            pinned.as_mut().poll(&mut cx),
            Poll::Ready(())
        );
    }

    #[test]
    fn test_race_first_wins() {
        let waker  = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut race = Race::new(
            WaitCycles { remaining: 0 },
            WaitCycles { remaining: 100 },
        );

        assert_eq!(
            Pin::new(&mut race).poll(&mut cx),
            Poll::Ready(RaceResult::First)
        );
    }

    #[test]
    fn test_race_second_wins() {
        let waker  = noop_waker();
        let mut cx = Context::from_waker(&waker);

        let mut race = Race::new(
            WaitCycles { remaining: 100 },
            WaitCycles { remaining: 0 },
        );

        assert_eq!(
            Pin::new(&mut race).poll(&mut cx),
            Poll::Ready(RaceResult::Second)
        );
    }

    #[test]
    fn test_rung_timeout() {
        let mut io      = make_io();
        let mut mailbox = make_mailbox();
        let mut rung    = Rung::new(
            "timeout_test",
            async move {},
            Some(5),
        );

        rung.state       = RungState::Waiting(
            SuspendReason::WaitFor(
                InputIndex(0),
                Value::Bool(true)
            )
        );
        rung.last_active = 0;

        rung.poll(&mut io, &mut mailbox, 10, 10);
        assert!(matches!(
            rung.state,
            RungState::Faulted(RungFault::Timeout)
        ));
    }

    #[test]
    fn test_os_result_stored_and_consumed() {
        let mut rung = Rung::new(
            "os_test",
            async move {},
            None,
        );

        // simulate arena delivering result
        let mut result = [0u8; PAYLOAD_SIZE];
        result[0] = 99;
        rung.os_result = Some(result);

        // result accessible
        let r = rung.os_result.take().unwrap();
        assert_eq!(r[0], 99);

        // consumed
        assert!(rung.os_result.is_none());
    }

    #[test]
    fn test_reset_clears_os_result() {
        let mut rung = Rung::new(
            "reset_test",
            async move {},
            None,
        );

        rung.os_result = Some([1u8; PAYLOAD_SIZE]);
        rung.state     = RungState::Complete;

        rung.reset();

        assert!(rung.os_result.is_none());
        assert!(matches!(
            rung.state,
            RungState::Ready
        ));
    }
}
