# Changelog

All notable changes to NoLadder will be documented in this file.

## [0.1.0] - 2026-03-15

### Added
- **Core Runtime**
  - RT control loop with isolated CPU core support
  - Rung coroutine executor with async/await suspension
  - IO image with lockless sequence counter synchronization
  - Pre-allocated arena allocators for deterministic memory usage
  - Shared memory IPC between bus server and control logic

- **Bus Drivers**
  - Modbus TCP/RTU driver (fully featured)
  - EtherCAT driver via IgH master (feature-gated, needs validation)
  - CiA402 servo drive state machine (tested, hardware validation pending)

- **Configuration**
  - TOML-based machine topology definition
  - Device path resolution at startup
  - Configuration validator binary
  - Support for multiple bus types in single machine

- **Programming Model**
  - Yield primitives: `yield_until`, `yield_until_any`, `yield_until_all`, `yield_ms`, `race`
  - Deterministic rung execution without dynamic allocation
  - OS async bridge via fixed-size mailbox
  - Type-safe IO access via IndexInput/OutputIndex

- **Development Tools**
  - Example code: hello_world (three-process), basic_io (simulated conveyor)
  - Built-in software Modbus slave for testing without hardware
  - Config validator tool
  - Comprehensive logging via tracing

- **Documentation**
  - README with value proposition and quick start
  - Architecture guide explaining design decisions
  - User guide with patterns and common mistakes
  - Design document for co-developers
  - API documentation in code

### Known Limitations
- **Not production ready yet** — v0.1 is proof of concept
- **EtherCAT driver** — compiled but needs real hardware validation
- **No hot reload** — planned for v0.2
- **No remote monitoring** — planned for v0.3
- **No SIL certification** — targeted for v1.0
- **Linux only** — tested on x86 industrial IPCs
- **No IEC 61131 compatibility** — by design (this is a feature)

### Dependencies
- tokio: async runtime
- serde/toml: configuration
- tracing: structured logging
- tokio-modbus: Modbus driver
- crossbeam: lock-free primitives
- memmap2: shared memory
- libc: Linux RT syscalls

## Roadmap

### v0.2
- Profinet driver
- Hot reload of control logic
- Cycle statistics dashboard
- Hardware watchdog integration

### v0.3
- CANopen driver
- Remote monitoring over MQTT
- Structured logging to InfluxDB
- PLCnext IO module support

### v1.0
- IEC 62443 security baseline
- SIL2 pathway investigation
- Production hardened

---

For more information see:
- `docs/DESIGN.md` - architecture and design rationale
- `docs/ARCHITECTURE.md` - detailed technical architecture
- `docs/UserGuide.md` - how to use NoLadder
