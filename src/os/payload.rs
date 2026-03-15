// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/os/payload.rs
//
// Fixed size payload for OS requests and responses
// Crosses the RT ↔ OS boundary via mailbox
// No allocation — fixed size array with typed accessors
//
// Layout is entirely up to the user
// Framework does not interpret payload contents
// Handler and rung must agree on layout
//
// Convention:
//   slots 0-N:   f32 or i32 values (4 bytes each)
//   bytes 0-63:  first string field
//   bytes 64-127: second string field
//   etc.
//
// Example — recipe result:
//   slot 0 (bytes 0-3):   speed   f32
//   slot 1 (bytes 4-7):   torque  f32
//   slot 2 (bytes 8-11):  temp    f32
//
// Example — mqtt publish:
//   bytes 0-63:   topic string
//   bytes 64-127: message string
//   slot 32+:     optional numeric values

use crate::core::mailbox::PAYLOAD_SIZE;

// special marker bytes for error payloads
// first 4 bytes = ERROR_MAGIC if error
const ERROR_MAGIC: u32 = 0xDEAD_BEEF;

// max string length in payload
pub const MAX_STR_LEN: usize = 63;  // null terminated

// ------------------------------------
// OsPayload
// thin wrapper over [u8; PAYLOAD_SIZE]
// typed read/write at byte offsets
// copy type — cheap to pass around
// ------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct OsPayload {
    data: [u8; PAYLOAD_SIZE],
}

impl OsPayload {

    // ------------------------------------
    // Constructors
    // ------------------------------------

    /// Empty payload — all zeros
    pub fn new() -> Self {
        Self { data: [0u8; PAYLOAD_SIZE] }
    }

    /// Alias for new() — intent is clearer
    /// when returning from a handler that
    /// has nothing to return
    pub fn empty() -> Self {
        Self::new()
    }

    /// Build from raw bytes
    /// used by framework when receiving
    /// from mailbox
    pub fn from(bytes: [u8; PAYLOAD_SIZE]) -> Self {
        Self { data: bytes }
    }

    /// Error payload
    /// sets magic bytes so receiver can detect
    /// handler errors without panicking
    pub fn error(message: &str) -> Self {
        let mut p = Self::new();

        // write error magic
        p.data[0..4].copy_from_slice(
            &ERROR_MAGIC.to_le_bytes()
        );

        // write message after magic
        let msg   = message.as_bytes();
        let space = PAYLOAD_SIZE - 4;
        let len   = msg.len().min(space - 1);
        p.data[4..4 + len]
            .copy_from_slice(&msg[..len]);

        p
    }

    /// Consume into raw bytes
    /// used by framework when posting to mailbox
    pub fn into_bytes(self) -> [u8; PAYLOAD_SIZE] {
        self.data
    }

    /// Raw bytes reference
    pub fn as_bytes(&self) -> &[u8; PAYLOAD_SIZE] {
        &self.data
    }

    /// Mutable raw bytes
    /// for advanced use cases
    pub fn data_mut(
        &mut self
    ) -> &mut [u8; PAYLOAD_SIZE] {
        &mut self.data
    }

    // ------------------------------------
    // Status checks
    // ------------------------------------

    /// True if this is an error payload
    pub fn is_error(&self) -> bool {
        u32::from_le_bytes(
            self.data[0..4].try_into().unwrap()
        ) == ERROR_MAGIC
    }

    /// True if all bytes are zero
    pub fn is_empty(&self) -> bool {
        self.data.iter().all(|&b| b == 0)
    }

    /// Error message if is_error()
    pub fn error_message(&self) -> Option<&str> {
        if !self.is_error() {
            return None;
        }
        let end = self.data[4..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| p + 4)
            .unwrap_or(PAYLOAD_SIZE);

        std::str::from_utf8(
            &self.data[4..end]
        ).ok()
    }

    // ------------------------------------
    // f32 — slot based
    // slot N = bytes N*4 .. N*4+4
    // up to PAYLOAD_SIZE/4 slots
    // ------------------------------------

    /// Write f32 at slot N
    /// slot 0 = bytes 0-3
    /// slot 1 = bytes 4-7
    /// etc
    pub fn write_f32(
        &mut self,
        slot:  usize,
        value: f32,
    ) -> &mut Self {
        let offset = slot * 4;
        debug_assert!(
            offset + 4 <= PAYLOAD_SIZE,
            "f32 slot {} out of range", slot
        );
        self.data[offset..offset + 4]
            .copy_from_slice(&value.to_le_bytes());
        self
    }

    /// Read f32 at slot N
    pub fn read_f32(&self, slot: usize) -> f32 {
        let offset = slot * 4;
        debug_assert!(
            offset + 4 <= PAYLOAD_SIZE,
            "f32 slot {} out of range", slot
        );
        f32::from_le_bytes(
            self.data[offset..offset + 4]
                .try_into()
                .unwrap()
        )
    }

    // ------------------------------------
    // i32 — slot based
    // same layout as f32 — same slots
    // do not mix f32 and i32 at same slot
    // ------------------------------------

    pub fn write_i32(
        &mut self,
        slot:  usize,
        value: i32,
    ) -> &mut Self {
        let offset = slot * 4;
        debug_assert!(
            offset + 4 <= PAYLOAD_SIZE,
            "i32 slot {} out of range", slot
        );
        self.data[offset..offset + 4]
            .copy_from_slice(&value.to_le_bytes());
        self
    }

    pub fn read_i32(&self, slot: usize) -> i32 {
        let offset = slot * 4;
        debug_assert!(
            offset + 4 <= PAYLOAD_SIZE,
            "i32 slot {} out of range", slot
        );
        i32::from_le_bytes(
            self.data[offset..offset + 4]
                .try_into()
                .unwrap()
        )
    }

    // ------------------------------------
    // u32 — slot based
    // ------------------------------------

    pub fn write_u32(
        &mut self,
        slot:  usize,
        value: u32,
    ) -> &mut Self {
        let offset = slot * 4;
        debug_assert!(
            offset + 4 <= PAYLOAD_SIZE,
            "u32 slot {} out of range", slot
        );
        self.data[offset..offset + 4]
            .copy_from_slice(&value.to_le_bytes());
        self
    }

    pub fn read_u32(&self, slot: usize) -> u32 {
        let offset = slot * 4;
        debug_assert!(
            offset + 4 <= PAYLOAD_SIZE,
            "u32 slot {} out of range", slot
        );
        u32::from_le_bytes(
            self.data[offset..offset + 4]
                .try_into()
                .unwrap()
        )
    }

    // ------------------------------------
    // bool — byte based
    // one bool per byte
    // use byte_offset not slot
    // ------------------------------------

    pub fn write_bool(
        &mut self,
        byte_offset: usize,
        value:       bool,
    ) -> &mut Self {
        debug_assert!(
            byte_offset < PAYLOAD_SIZE,
            "bool offset {} out of range",
            byte_offset
        );
        self.data[byte_offset] = value as u8;
        self
    }

    pub fn read_bool(
        &self,
        byte_offset: usize,
    ) -> bool {
        debug_assert!(
            byte_offset < PAYLOAD_SIZE,
            "bool offset {} out of range",
            byte_offset
        );
        self.data[byte_offset] != 0
    }

    // ------------------------------------
    // Strings — byte offset based
    // null terminated
    // max MAX_STR_LEN bytes + null
    // ------------------------------------

    /// Write string at byte offset
    /// null terminated
    /// truncated if too long
    pub fn write_str_at(
        &mut self,
        byte_offset: usize,
        value:       &str,
    ) -> &mut Self {
        debug_assert!(
            byte_offset < PAYLOAD_SIZE,
            "str offset {} out of range",
            byte_offset
        );

        let bytes = value.as_bytes();
        let space = PAYLOAD_SIZE - byte_offset;
        let len   = bytes.len().min(space - 1);

        // zero the string area first
        let end = (byte_offset + len + 1)
            .min(PAYLOAD_SIZE);
        self.data[byte_offset..end].fill(0);

        // write string bytes
        self.data[byte_offset..byte_offset + len]
            .copy_from_slice(&bytes[..len]);

        // null terminator
        if byte_offset + len < PAYLOAD_SIZE {
            self.data[byte_offset + len] = 0;
        }

        self
    }

    /// Write string at byte offset 0
    pub fn write_str(
        &mut self,
        value: &str,
    ) -> &mut Self {
        self.write_str_at(0, value)
    }

    /// Read string at byte offset
    pub fn read_str_at(
        &self,
        byte_offset: usize,
    ) -> &str {
        debug_assert!(
            byte_offset < PAYLOAD_SIZE,
            "str offset {} out of range",
            byte_offset
        );

        let end = self.data[byte_offset..]
            .iter()
            .position(|&b| b == 0)
            .map(|p| byte_offset + p)
            .unwrap_or(PAYLOAD_SIZE);

        std::str::from_utf8(
            &self.data[byte_offset..end]
        ).unwrap_or("")
    }

    /// Read string at byte offset 0
    pub fn read_str(&self) -> &str {
        self.read_str_at(0)
    }

    // ------------------------------------
    // Raw bytes — for custom serialization
    // ------------------------------------

    /// Write raw bytes at byte offset
    pub fn write_bytes(
        &mut self,
        byte_offset: usize,
        bytes:       &[u8],
    ) -> &mut Self {
        let space = PAYLOAD_SIZE - byte_offset;
        let len   = bytes.len().min(space);

        self.data[byte_offset..byte_offset + len]
            .copy_from_slice(&bytes[..len]);
        self
    }

    /// Read raw bytes at byte offset
    pub fn read_bytes(
        &self,
        byte_offset: usize,
        len:         usize,
    ) -> &[u8] {
        let end = (byte_offset + len)
            .min(PAYLOAD_SIZE);
        &self.data[byte_offset..end]
    }
}

impl Default for OsPayload {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for OsPayload {
    fn fmt(
        &self,
        f: &mut std::fmt::Formatter<'_>,
    ) -> std::fmt::Result {
        if self.is_error() {
            write!(
                f,
                "OsPayload::Error({})",
                self.error_message()
                    .unwrap_or("unknown")
            )
        } else if self.is_empty() {
            write!(f, "OsPayload::Empty")
        } else {
            write!(
                f,
                "OsPayload([{} bytes])",
                PAYLOAD_SIZE
            )
        }
    }
}

// ------------------------------------
// Convenience builder pattern
// for complex payloads
// ------------------------------------

/// Builder for payloads with mixed content
/// chaining syntax
///
/// ```
/// # use noladder::os::payload::OsPayload;
/// let payload = OsPayload::new()
///     .with_str(0, "product_A")
///     .with_f32(16, 1500.0)
///     .with_f32(17, 10.0);
/// ```
impl OsPayload {
    pub fn with_str(
        mut self,
        byte_offset: usize,
        value:       &str,
    ) -> Self {
        self.write_str_at(byte_offset, value);
        self
    }

    pub fn with_f32(
        mut self,
        slot:  usize,
        value: f32,
    ) -> Self {
        self.write_f32(slot, value);
        self
    }

    pub fn with_i32(
        mut self,
        slot:  usize,
        value: i32,
    ) -> Self {
        self.write_i32(slot, value);
        self
    }

    pub fn with_bool(
        mut self,
        byte_offset: usize,
        value:       bool,
    ) -> Self {
        self.write_bool(byte_offset, value);
        self
    }
}

// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_f32_roundtrip() {
        let mut p = OsPayload::new();
        p.write_f32(0, 1500.0);
        p.write_f32(1, 10.0);
        p.write_f32(2, -42.5);

        assert!(
            (p.read_f32(0) - 1500.0).abs() < f32::EPSILON
        );
        assert!(
            (p.read_f32(1) - 10.0).abs() < f32::EPSILON
        );
        assert!(
            (p.read_f32(2) - (-42.5)).abs() < f32::EPSILON
        );
    }

    #[test]
    fn test_i32_roundtrip() {
        let mut p = OsPayload::new();
        p.write_i32(0, 42);
        p.write_i32(1, -100);
        p.write_i32(2, i32::MAX);

        assert_eq!(p.read_i32(0), 42);
        assert_eq!(p.read_i32(1), -100);
        assert_eq!(p.read_i32(2), i32::MAX);
    }

    #[test]
    fn test_bool_roundtrip() {
        let mut p = OsPayload::new();
        p.write_bool(0, true);
        p.write_bool(1, false);
        p.write_bool(2, true);

        assert_eq!(p.read_bool(0), true);
        assert_eq!(p.read_bool(1), false);
        assert_eq!(p.read_bool(2), true);
    }

    #[test]
    fn test_str_roundtrip() {
        let mut p = OsPayload::new();
        p.write_str("hello world");

        assert_eq!(p.read_str(), "hello world");
    }

    #[test]
    fn test_str_at_offset() {
        let mut p = OsPayload::new();
        p.write_str_at(0,  "topic/name");
        p.write_str_at(64, "message body");

        assert_eq!(p.read_str_at(0),  "topic/name");
        assert_eq!(p.read_str_at(64), "message body");
    }

    #[test]
    fn test_str_truncation() {
        let mut p   = OsPayload::new();
        let long_str = "x".repeat(PAYLOAD_SIZE * 2);
        p.write_str(&long_str);

        // should not panic
        // string truncated to fit
        let result = p.read_str();
        assert!(result.len() < PAYLOAD_SIZE);
    }

    #[test]
    fn test_str_does_not_overlap_next_field() {
        let mut p = OsPayload::new();
        p.write_str_at(0,  "first");
        p.write_str_at(64, "second");

        // first string should not bleed into second
        assert_eq!(p.read_str_at(0),  "first");
        assert_eq!(p.read_str_at(64), "second");
    }

    #[test]
    fn test_error_payload() {
        let p = OsPayload::error("something failed");

        assert!(p.is_error());
        assert_eq!(
            p.error_message(),
            Some("something failed")
        );
    }

    #[test]
    fn test_non_error_payload() {
        let mut p = OsPayload::new();
        p.write_f32(0, 42.0);

        assert!(!p.is_error());
        assert!(p.error_message().is_none());
    }

    #[test]
    fn test_empty_detection() {
        let p = OsPayload::new();
        assert!(p.is_empty());

        let mut p2 = OsPayload::new();
        p2.write_f32(0, 1.0);
        assert!(!p2.is_empty());
    }

    #[test]
    fn test_builder_pattern() {
        let p = OsPayload::new()
            .with_str(0, "machine/status")
            .with_f32(16, 1500.0)
            .with_f32(17, 10.5)
            .with_bool(128, true);

        assert_eq!(p.read_str_at(0), "machine/status");
        assert!(
            (p.read_f32(16) - 1500.0).abs()
            < f32::EPSILON
        );
        assert!(
            (p.read_f32(17) - 10.5).abs()
            < f32::EPSILON
        );
        assert_eq!(p.read_bool(128), true);
    }

    #[test]
    fn test_into_bytes_roundtrip() {
        let mut original = OsPayload::new();
        original.write_f32(0, 99.9);
        original.write_str_at(64, "test");

        let bytes     = original.into_bytes();
        let recovered = OsPayload::from(bytes);

        assert!(
            (recovered.read_f32(0) - 99.9).abs()
            < f32::EPSILON
        );
        assert_eq!(
            recovered.read_str_at(64),
            "test"
        );
    }

    #[test]
    fn test_mixed_types_no_overlap() {
        // f32 at slots 0,1,2 = bytes 0-11
        // str at byte 64
        // bool at byte 128
        let mut p = OsPayload::new();
        p.write_f32(0,  1.0);
        p.write_f32(1,  2.0);
        p.write_f32(2,  3.0);
        p.write_str_at(64, "hello");
        p.write_bool(128, true);

        // all independent
        assert!((p.read_f32(0) - 1.0).abs() < f32::EPSILON);
        assert!((p.read_f32(1) - 2.0).abs() < f32::EPSILON);
        assert!((p.read_f32(2) - 3.0).abs() < f32::EPSILON);
        assert_eq!(p.read_str_at(64), "hello");
        assert_eq!(p.read_bool(128), true);
    }

    #[test]
    fn test_raw_bytes_roundtrip() {
        let mut p    = OsPayload::new();
        let original = [1u8, 2, 3, 4, 5];

        p.write_bytes(10, &original);

        let recovered = p.read_bytes(10, 5);
        assert_eq!(recovered, &original);
    }

    #[test]
    fn test_display_empty() {
        let p = OsPayload::empty();
        assert_eq!(
            format!("{}", p),
            "OsPayload::Empty"
        );
    }

    #[test]
    fn test_display_error() {
        let p = OsPayload::error("oops");
        assert!(
            format!("{}", p).contains("oops")
        );
    }
}