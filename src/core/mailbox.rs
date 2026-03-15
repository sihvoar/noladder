// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/core/mailbox.rs

use std::sync::atomic::{AtomicU32, Ordering};

pub const MAILBOX_SIZE:    usize = 64;
pub const KEY_SIZE:        usize = 64;
pub const PAYLOAD_SIZE:    usize = 256;

// ------------------------------------
// A single mailbox slot
// fixed size — no allocation
// ------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct MailboxSlot {
    // monotonic request ID
    // rung stores this to match response
    pub id:      u32,

    // request key — what OS server should do
    // "recipe.load", "mqtt.publish", "file.read"
    pub key:     [u8; KEY_SIZE],

    // request payload — fixed size
    // serialized by rung before posting
    pub payload: [u8; PAYLOAD_SIZE],

    // response payload
    // written by OS server
    pub result:  [u8; PAYLOAD_SIZE],

    // slot state
    pub state:   SlotState,
}

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SlotState {
    Empty    = 0,
    Pending  = 1,   // RT posted, OS not seen yet
    Running  = 2,   // OS picked up, working
    Ready    = 3,   // OS finished, result waiting
}

impl MailboxSlot {
    pub fn key_str(&self) -> &str {
        let len = self.key
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(KEY_SIZE);
        std::str::from_utf8(&self.key[..len])
            .unwrap_or("invalid")
    }

    pub fn set_key(&mut self, key: &str) {
        self.key.fill(0);
        let bytes = key.as_bytes();
        let len   = bytes.len().min(KEY_SIZE - 1);
        self.key[..len].copy_from_slice(&bytes[..len]);
    }
}

// ------------------------------------
// Mailbox
// shared between RT loop and OS server
// lives in shared memory alongside IOImage
// or as separate shared memory region
// ------------------------------------

#[repr(C)]
pub struct Mailbox {
    // next request ID to assign
    next_id:  AtomicU32,

    // ring buffer of slots
    slots:    [MailboxSlot; MAILBOX_SIZE],
}

impl Mailbox {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
            slots:   [MailboxSlot {
                id:      0,
                key:     [0; KEY_SIZE],
                payload: [0; PAYLOAD_SIZE],
                result:  [0; PAYLOAD_SIZE],
                state:   SlotState::Empty,
            }; MAILBOX_SIZE],
        }
    }

    // ------------------------------------
    // RT side
    // post a request — never blocks
    // returns request ID or None if full
    // ------------------------------------

    pub fn post(
        &mut self,
        key:     &str,
        payload: &[u8],
    ) -> Option<u32> {

        // find empty slot
        let slot = self.slots
            .iter_mut()
            .find(|s| s.state == SlotState::Empty)?;

        let id = self.next_id
            .fetch_add(1, Ordering::Relaxed);

        slot.id = id;
        slot.set_key(key);

        // copy payload — truncate if too large
        slot.payload.fill(0);
        let len = payload.len().min(PAYLOAD_SIZE);
        slot.payload[..len]
            .copy_from_slice(&payload[..len]);

        slot.result.fill(0);

        // mark pending — OS server will see this
        // Release ordering — payload visible before state
        std::sync::atomic::fence(Ordering::Release);
        slot.state = SlotState::Pending;

        Some(id)
    }

    // check if request is complete
    // called each cycle by arena before poll_all
    // returns result payload if ready
    pub fn check(
        &mut self,
        id: u32,
    ) -> Option<[u8; PAYLOAD_SIZE]> {

        let slot = self.slots
            .iter_mut()
            .find(|s| s.id == id)?;

        if slot.state == SlotState::Ready {
            let result = slot.result;

            // free the slot
            slot.state = SlotState::Empty;

            Some(result)
        } else {
            None
        }
    }

    // drain all ready responses
    // called by arena each cycle
    // wakes waiting rungs
    pub fn drain_responses(
        &mut self,
        arena: &mut crate::core::arena::Arena,
    ) {
        for slot in self.slots.iter_mut() {
            if slot.state == SlotState::Ready {
                // copy result before freeing slot
                let id     = slot.id;
                let result = slot.result;

                // free slot
                slot.state = SlotState::Empty;

                // wake waiting rung
                arena.notify_os_response(id, result);
            }
        }
    }

    // ------------------------------------
    // OS server side
    // poll for pending requests
    // called in a normal async loop
    // ------------------------------------

    pub fn poll_pending(
        &mut self,
    ) -> Option<(u32, String, [u8; PAYLOAD_SIZE])> {

        let slot = self.slots
            .iter_mut()
            .find(|s| s.state == SlotState::Pending)?;

        // mark running so we don't pick it up twice
        slot.state = SlotState::Running;

        std::sync::atomic::fence(Ordering::Acquire);

        Some((
            slot.id,
            slot.key_str().to_string(),
            slot.payload,
        ))
    }

    // OS server posts result back
    pub fn post_result(
        &mut self,
        id:     u32,
        result: &[u8],
    ) -> bool {
        let slot = self.slots
            .iter_mut()
            .find(|s| s.id == id);

        let Some(slot) = slot else {
            tracing::warn!(
                "Mailbox: result for unknown id {}",
                id
            );
            return false;
        };

        slot.result.fill(0);
        let len = result.len().min(PAYLOAD_SIZE);
        slot.result[..len]
            .copy_from_slice(&result[..len]);

        // Release ordering — result visible before state
        std::sync::atomic::fence(Ordering::Release);
        slot.state = SlotState::Ready;

        true
    }
}