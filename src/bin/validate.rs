// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/bin/validate.rs
//
// Config validator
// Run before deploying to catch mistakes
//
// Usage:
//   cargo run --bin validate -- machine.toml
//
// Exit code 0 = valid
// Exit code 1 = invalid

fn main() {
    let path = std::env::args()
        .nth(1)
        .unwrap_or_else(|| {
            eprintln!(
                "Usage: validate <machine.toml>"
            );
            std::process::exit(1);
        });

    noladder::config::loader::validate_and_report(path);
}
