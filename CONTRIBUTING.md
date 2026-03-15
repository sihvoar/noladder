# Contributing to NoLadder

Thank you for your interest in NoLadder! We welcome contributions from the community.

## Getting Started

1. **Read the documentation first**
   - `README.md` - overview and quick start
   - `docs/DESIGN.md` - architecture and design decisions
   - `docs/UserGuide.md` - how to use NoLadder
   - `docs/ARCHITECTURE.md` - detailed technical architecture

2. **Clone and build**
   ```bash
   git clone https://github.com/sihvoar/noladder
   cd noladder
   cargo build
   cargo test
   ```

3. **Run the examples**
   ```bash
   cargo run --example hello_world_bus &
   cargo run --example hello_world_os &
   cargo run --example hello_world
   ```

## What We Need Most (v0.1 → v0.2)

### Bus Drivers
- **EtherCAT** — IgH bindings exist; needs hardware validation
- **CAN / CANopen** — socketcan crate ready; device profiles documented
- **MQTT** — bridge async MQTT topics to IO image
- **GigE Vision / V4L2** — camera as a bus driver

See `src/bus/modbus.rs` for the driver pattern. One file, one trait implementation.

### Device Types
New variants in `config/loader.rs`:
- Encoders, safety devices, vision triggers
- Vendor-specific modules (Beckhoff, Yaskawa, etc.)

### Documentation
- Better error messages
- More real-world examples
- Machine builder guides (for non-programmers)

### Testing on Real Hardware
- Raspberry Pi CM4 + any Modbus device works
- Report what you find — we'll fix it

## Before Opening a PR

1. **Run tests and linting**
   ```bash
   cargo test
   cargo clippy
   cargo fmt
   ```

2. **Keep it focused**
   - One feature or fix per PR
   - Bus drivers can be in their own PR

3. **Add tests**
   - Unit tests for new modules
   - Examples for user-facing features

4. **Update docs if needed**
   - DESIGN.md for architectural changes
   - UserGuide.md for new patterns
   - Code comments for non-obvious logic

## Code Style

- Follow rustfmt formatting: `cargo fmt`
- Use clippy guidelines: `cargo clippy -- -D warnings`
- Add SPDX-License-Identifier headers to new files: `// SPDX-License-Identifier: MIT`
- Comments explain *why*, not *what* (code is the what)

## Roadmap

### v0.1 (current)
- ✅ Core RT loop, rung coroutines, modbus
- ✅ TOML config, examples

### v0.2
- EtherCAT driver validation
- Hot reload of control logic
- Cycle statistics dashboard
- Hardware watchdog integration

### v0.3
- CANopen driver
- MQTT bus driver
- Remote monitoring via MQTT
- InfluxDB structured logging

### v1.0
- IEC 62443 security baseline
- SIL2 pathway investigation
- Production hardened

## Questions?

- Open an issue for bugs or feature requests
- Start a discussion for architecture questions
- Check existing issues before opening new ones

---

*NoLadder exists because industrial automation deserves better tooling.*
*If you agree — contribute, star, or tell a colleague.*
