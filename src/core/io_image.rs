// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/core/io_image.rs

use std::sync::atomic::{AtomicU64, Ordering};

// maximum IO points
// conservative for v0.1 — can be config driven later
pub const MAX_IO: usize = 4096;

// ------------------------------------
// Value — the only type on the wire
// ------------------------------------

// Unset is FIRST so that zeroed memory (from mmap.fill(0))
// is interpreted as Unset, not Bool(false)
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Value {
    Unset,  // slot not yet written — distinguishable from 0
    Bool(bool),
    Int(i32),
    Float(f32),
}

impl Default for Value {
    fn default() -> Self {
        Value::Unset
    }
}

// convenience conversions
impl From<bool>  for Value { fn from(v: bool)  -> Self { Value::Bool(v)  } }
impl From<i32>   for Value { fn from(v: i32)   -> Self { Value::Int(v)   } }
impl From<f32>   for Value { fn from(v: f32)   -> Self { Value::Float(v) } }

impl Value {
    pub fn as_bool(&self) -> Option<bool> {
        match self { Value::Bool(v) => Some(*v), _ => None }
    }
    pub fn as_int(&self) -> Option<i32> {
        match self { Value::Int(v)  => Some(*v), _ => None }
    }
    pub fn as_float(&self) -> Option<f32> {
        match self { Value::Float(v) => Some(*v), _ => None }
    }
    pub fn is_set(&self) -> bool {
        !matches!(self, Value::Unset)
    }
}

// ------------------------------------
// IO Image
// ------------------------------------
// Shared between bus server and control loop via mmap.
// Must be entirely flat — NO heap pointers (Box, Vec, etc.)
// because the struct lives in a shared memory file that
// is mapped into two different process address spaces.
// Box pointers from one process are invalid in the other.
//
// Never locked — sequence counter pattern.
// Bus server writes inputs + increments sequence.
// Control loop snapshots inputs, runs logic, writes outputs.

pub struct IOImage {
    // incremented by bus server each cycle
    // control loop checks to detect fresh data
    pub sequence: AtomicU64,

    // written by bus server each bus cycle
    // control loop snapshots this at cycle start
    inputs:  [Value; MAX_IO],

    // written during rung execution
    // read by bus server to drive outputs
    outputs: [Value; MAX_IO],

    // frozen snapshot of inputs at cycle start
    // rungs read from this — consistent view for whole cycle
    snapshot: [Value; MAX_IO],
}

impl IOImage {
    // Heap-allocate an IOImage, avoiding stack overflow from
    // the ~300KB struct size.  Returns Box so caller owns it.
    // Only used in tests and bus-server (not shared memory path).
    pub fn allocate() -> Box<Self> {
        use std::alloc::{alloc, Layout};

        let layout = Layout::new::<Self>();
        // SAFETY: layout is non-zero (IOImage is non-empty)
        let ptr = unsafe { alloc(layout) as *mut Self };
        assert!(!ptr.is_null(), "IOImage allocation failed");

        // SAFETY: ptr is valid and properly aligned.
        // Write each field individually to avoid a large stack temp.
        unsafe {
            std::ptr::addr_of_mut!((*ptr).sequence)
                .write(AtomicU64::new(0));
            for i in 0..MAX_IO {
                std::ptr::addr_of_mut!((*ptr).inputs[i])
                    .write(Value::Unset);
                std::ptr::addr_of_mut!((*ptr).outputs[i])
                    .write(Value::Unset);
                std::ptr::addr_of_mut!((*ptr).snapshot[i])
                    .write(Value::Unset);
            }
            Box::from_raw(ptr)
        }
    }

    // ------------------------------------
    // Bus server side
    // ------------------------------------

    // bus server calls this after writing fresh input data
    pub fn publish_inputs(&mut self, index: usize, value: Value) {
        debug_assert!(index < MAX_IO, "IO index out of range");
        self.inputs[index] = value;
    }

    // bus server calls this to signal fresh cycle
    pub fn signal_ready(&self) {
        self.sequence.fetch_add(1, Ordering::Release);
    }

    // bus server reads pending outputs each cycle
    pub fn read_output(&self, index: usize) -> Value {
        debug_assert!(index < MAX_IO, "IO index out of range");
        self.outputs[index]
    }

    // ------------------------------------
    // Control loop side
    // ------------------------------------

    // called once at start of each control cycle
    // freezes current inputs into snapshot
    // all rungs read from snapshot — never live inputs
    pub fn snapshot(&mut self) {
        // Copy inputs → snapshot via raw pointers to avoid
        // borrow-checker confusion with self-referential slice copy
        unsafe {
            std::ptr::copy_nonoverlapping(
                self.inputs.as_ptr(),
                self.snapshot.as_mut_ptr(),
                MAX_IO,
            );
        }
    }

    // check if bus server has published fresh data
    pub fn is_fresh(&self, last_sequence: u64) -> bool {
        self.sequence.load(Ordering::Acquire) != last_sequence
    }

    pub fn current_sequence(&self) -> u64 {
        self.sequence.load(Ordering::Acquire)
    }

    // ------------------------------------
    // Rung side — called during execution
    // ------------------------------------

    // rungs read from snapshot — frozen at cycle start
    pub fn read(&self, index: usize) -> Value {
        debug_assert!(index < MAX_IO, "IO index out of range");
        self.snapshot[index]
    }

    // convenience typed reads — panic on wrong type in debug
    // in release just return default — never crash a machine
    pub fn read_bool(&self, index: usize) -> bool {
        match self.read(index) {
            Value::Bool(v)  => v,
            Value::Unset    => false,
            other => {
                #[cfg(debug_assertions)]
                panic!("Expected Bool at index {}, got {:?}", index, other);
                #[cfg(not(debug_assertions))]
                false
            }
        }
    }

    pub fn read_float(&self, index: usize) -> f32 {
        match self.read(index) {
            Value::Float(v) => v,
            Value::Int(v)   => v as f32,  // int → float is safe
            Value::Unset    => 0.0,
            other => {
                #[cfg(debug_assertions)]
                panic!("Expected Float at index {}, got {:?}", index, other);
                #[cfg(not(debug_assertions))]
                0.0
            }
        }
    }

    pub fn read_int(&self, index: usize) -> i32 {
        match self.read(index) {
            Value::Int(v)   => v,
            Value::Unset    => 0,
            other => {
                #[cfg(debug_assertions)]
                panic!("Expected Int at index {}, got {:?}", index, other);
                #[cfg(not(debug_assertions))]
                0
            }
        }
    }

    // rungs write to output image
    pub fn write(&mut self, index: usize, value: impl Into<Value>) {
        debug_assert!(index < MAX_IO, "IO index out of range");
        self.outputs[index] = value.into();
    }

    // ------------------------------------
    // Diagnostics
    // ------------------------------------

    pub fn input_count(&self) -> usize {
        self.inputs.iter().filter(|v| v.is_set()).count()
    }

    pub fn output_count(&self) -> usize {
        self.outputs.iter().filter(|v| v.is_set()).count()
    }
}

// ------------------------------------
// Device index — resolved at startup
// just a newtype over usize
// prevents accidentally using raw integers as indices
// ------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InputIndex(pub usize);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputIndex(pub usize);

// so you can't accidentally write to an input index
// or read from an output index
// compiler catches it
impl IOImage {
    pub fn read_input(&self, idx: InputIndex) -> Value {
        self.read(idx.0)
    }
    pub fn write_output(&mut self, idx: OutputIndex, value: impl Into<Value>) {
        self.write(idx.0, value)
    }
}

// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_io() -> Box<IOImage> {
        IOImage::allocate()
    }

    #[test]
    fn test_snapshot_freezes_inputs() {
        let mut io = make_io();

        // bus server writes input
        io.publish_inputs(0, Value::Float(42.0));
        io.snapshot();

        // bus server updates — should not affect snapshot
        io.publish_inputs(0, Value::Float(99.0));

        // rung still sees frozen value
        assert_eq!(io.read_float(0), 42.0);
    }

    #[test]
    fn test_sequence_counter() {
        let io  = make_io();
        let seq = io.current_sequence();

        io.signal_ready();

        assert!(io.is_fresh(seq));
        assert!(!io.is_fresh(io.current_sequence()));
    }

    #[test]
    fn test_typed_index() {
        let mut io       = make_io();
        let motor_speed  = InputIndex(42);
        let motor_enable = OutputIndex(42);

        io.publish_inputs(42, Value::Float(100.0));
        io.snapshot();

        assert_eq!(io.read_input(motor_speed).as_float(), Some(100.0));

        io.write_output(motor_enable, true);
        assert_eq!(io.read_output(42), Value::Bool(true));
    }

    #[test]
    fn test_zeroed_is_unset() {
        // verify Unset is discriminant 0 so mmap.fill(0) → Unset
        let io = make_io();
        assert_eq!(io.read(0), Value::Unset);
        assert_eq!(io.read(MAX_IO - 1), Value::Unset);
    }
}
