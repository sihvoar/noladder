// Copyright 2026 AP Sihvonen
// SPDX-License-Identifier: MIT

// src/core/cycle.rs

use std::time::{Duration, Instant};
use anyhow::Result;
use tracing::{info, warn, error, debug};

use crate::core::io_image::IOImage;
use crate::core::arena::Arena;
use crate::core::mailbox::Mailbox;
use crate::config::loader::Config;

// ------------------------------------
// Tuning constants
// ------------------------------------

// consecutive overruns before safe state
const MAX_CONSECUTIVE_OVERRUNS: u32 = 3;

// cycle utilization warning threshold
const WARN_UTILIZATION: f32 = 0.80;

// how often to log cycle stats
const STATS_INTERVAL_SECS: u64 = 10;

// how often to log arena stats
const ARENA_STATS_INTERVAL_SECS: u64 = 30;

// ------------------------------------
// Cycle statistics
// ------------------------------------

#[derive(Debug, Default)]
pub struct CycleStats {
    pub cycle_count:          u64,
    pub overrun_count:        u64,
    pub consecutive_overruns: u32,
    pub max_cycle_time_us:    u64,
    pub min_cycle_time_us:    u64,
    pub avg_cycle_time_us:    f64,
    pub last_cycle_time_us:   u64,
}

impl CycleStats {
    fn update(
        &mut self,
        elapsed_us: u64,
        budget_us:  u64,
    ) {
        self.cycle_count      += 1;
        self.last_cycle_time_us = elapsed_us;

        // running average — Welford's method
        // numerically stable, no history needed
        let delta = elapsed_us as f64
            - self.avg_cycle_time_us;
        self.avg_cycle_time_us +=
            delta / self.cycle_count as f64;

        // max
        if elapsed_us > self.max_cycle_time_us {
            self.max_cycle_time_us = elapsed_us;
        }

        // min — ignore first cycle, always slow
        if self.cycle_count > 1 {
            if self.min_cycle_time_us == 0
            || elapsed_us < self.min_cycle_time_us {
                self.min_cycle_time_us = elapsed_us;
            }
        }

        // overrun tracking
        if elapsed_us > budget_us {
            self.overrun_count        += 1;
            self.consecutive_overruns += 1;
        } else {
            self.consecutive_overruns  = 0;
        }
    }

    fn utilization(&self, budget_us: u64) -> f32 {
        self.last_cycle_time_us as f32
            / budget_us as f32
    }

    fn log(&self, budget_us: u64) {
        info!(
            "Cycle stats — \
             count: {} \
             overruns: {} ({:.2}%) \
             avg: {:.1}µs \
             min: {}µs \
             max: {}µs \
             last: {}µs \
             utilization: {:.1}%",
            self.cycle_count,
            self.overrun_count,
            self.overrun_count as f64
                / self.cycle_count as f64 * 100.0,
            self.avg_cycle_time_us,
            self.min_cycle_time_us,
            self.max_cycle_time_us,
            self.last_cycle_time_us,
            self.utilization(budget_us) * 100.0,
        );
    }
}

// ------------------------------------
// Safe state
// entered on consecutive overruns
// or unrecoverable error
// ------------------------------------

fn enter_safe_state(
    io:     &mut IOImage,
    arena:  &mut Arena,
    reason: &str,
) {
    error!("═══════════════════════════════════");
    error!("SAFE STATE: {}", reason);
    error!("═══════════════════════════════════");

    // zero all outputs
    for i in 0..crate::core::io_image::MAX_IO {
        io.write(i, false);
    }

    // log any faulted rungs
    arena.log_faults();

    error!(
        "All outputs zeroed — \
         operator intervention required"
    );
}

// ------------------------------------
// RT control loop
// never returns under normal operation
// ------------------------------------

pub fn run(
    config:  &Config,
    io:      &mut IOImage,
    arena:   &mut Arena,
    mailbox: &mut Mailbox,
) -> Result<()> {

    let cycle_duration = Duration::from_millis(
        config.cycle_ms as u64
    );
    let budget_us = cycle_duration.as_micros() as u64;

    info!(
        "Control loop starting — \
         {}ms cycle \
         {}µs budget \
         {} rungs",
        config.cycle_ms,
        budget_us,
        arena.count(),
    );

    // set RT priority before loop
    set_rt_priority()?;

    let mut stats      = CycleStats::default();
    let mut last_seq   = io.current_sequence();
    let mut next_cycle = Instant::now() + cycle_duration;

    // stat logging timers
    let mut last_cycle_stats = Instant::now();
    let mut last_arena_stats = Instant::now();

    let cycle_stats_interval = Duration::from_secs(
        STATS_INTERVAL_SECS
    );
    let arena_stats_interval = Duration::from_secs(
        ARENA_STATS_INTERVAL_SECS
    );

    // cycle counter — passed to arena
    // used for timeout detection
    let mut cycle: u64 = 0;

    loop {
        let cycle_start = Instant::now();
        cycle += 1;

        // ----------------------------------------
        // 1. check for fresh IO data
        //    warn if bus server is lagging
        //    do not fail — bus may be slower than
        //    control cycle by design (e.g. Modbus)
        // ----------------------------------------
        if !io.is_fresh(last_seq) {
            if cycle > 10 {
                // suppress during startup
                debug!(
                    "Cycle {}: stale IO data — \
                     bus server lagging or slower cycle",
                    cycle
                );
            }
        }
        last_seq = io.current_sequence();

        // ----------------------------------------
        // 2. freeze inputs
        //    all rungs see consistent snapshot
        //    bus server may continue writing
        //    inputs during this cycle — irrelevant
        // ----------------------------------------
        io.snapshot();

        // ----------------------------------------
        // 3. deliver OS responses
        //    before poll_all so rungs can act
        //    on OS data this cycle
        // ----------------------------------------
        mailbox.drain_responses(arena);

        // ----------------------------------------
        // 4. execute all rungs
        //    in registration order
        //    each either advances or stays suspended
        //    never blocks
        // ----------------------------------------
        arena.poll_all(io, mailbox, cycle, config.cycle_ms);

        // ----------------------------------------
        // 5. measure cycle execution time
        //    before sleep — pure logic time
        // ----------------------------------------
        let elapsed    = cycle_start.elapsed();
        let elapsed_us = elapsed.as_micros() as u64;

        stats.update(elapsed_us, budget_us);

        // ----------------------------------------
        // 6. overrun detection
        // ----------------------------------------
        if elapsed > cycle_duration {
            warn!(
                "Cycle {} OVERRUN — \
                 took {}µs budget {}µs \
                 ({} consecutive)",
                cycle,
                elapsed_us,
                budget_us,
                stats.consecutive_overruns,
            );

            // log arena state on first overrun
            // helps diagnose what is taking too long
            if stats.consecutive_overruns == 1 {
                let arena_stats = arena.stats();
                warn!("Arena state: {}", arena_stats);
                arena.log_faults();
            }

            if stats.consecutive_overruns
                >= MAX_CONSECUTIVE_OVERRUNS
            {
                enter_safe_state(
                    io,
                    arena,
                    &format!(
                        "{} consecutive cycle overruns",
                        MAX_CONSECUTIVE_OVERRUNS
                    ),
                );
                anyhow::bail!(
                    "Safe state — \
                     operator intervention required"
                );
            }

        } else if stats.utilization(budget_us)
            > WARN_UTILIZATION
        {
            warn!(
                "Cycle {} high utilization {:.0}% — \
                 {}µs of {}µs budget used",
                cycle,
                stats.utilization(budget_us) * 100.0,
                elapsed_us,
                budget_us,
            );
        }

        // ----------------------------------------
        // 7. periodic stats logging
        // ----------------------------------------
        if last_cycle_stats.elapsed() > cycle_stats_interval {
            stats.log(budget_us);
            last_cycle_stats = Instant::now();
        }

        if last_arena_stats.elapsed() > arena_stats_interval {
            info!("Arena: {}", arena.stats());
            if arena.has_faults() {
                arena.log_faults();
            }
            last_arena_stats = Instant::now();
        }

        // ----------------------------------------
        // 8. sleep until next cycle boundary
        //    absolute deadline — no drift
        // ----------------------------------------
        let now = Instant::now();
        if now < next_cycle {
            std::thread::sleep(next_cycle - now);
        } else {
            // already past deadline
            // skip sleep — catch up next cycle
            debug!(
                "Cycle {} late start — \
                 missed deadline by {}µs",
                cycle,
                (now - next_cycle).as_micros(),
            );
        }

        // advance absolute deadline
        // never drifts regardless of sleep accuracy
        next_cycle += cycle_duration;
    }
}

// ------------------------------------
// RT thread priority
// ------------------------------------

fn set_rt_priority() -> Result<()> {
    #[cfg(target_os = "linux")]
    unsafe {
        let param = libc::sched_param {
            sched_priority: 80,
        };
        let ret = libc::sched_setscheduler(
            0,
            libc::SCHED_FIFO,
            &param,
        );
        if ret != 0 {
            warn!(
                "Could not set SCHED_FIFO — \
                 RT performance degraded.\n  \
                 Production: sudo setcap \
                 cap_sys_nice+ep ./noladder"
            );
        } else {
            info!("RT priority set — SCHED_FIFO 80");
        }
    }

    #[cfg(not(target_os = "linux"))]
    warn!(
        "RT scheduling not available — \
         dev platform"
    );

    Ok(())
}

// ------------------------------------
// Tests
// ------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stats_welford_average() {
        let mut stats = CycleStats::default();

        // known values — average should be 500
        stats.update(400, 1000);
        stats.update(500, 1000);
        stats.update(600, 1000);

        assert!(
            (stats.avg_cycle_time_us - 500.0).abs() < 1.0,
            "Expected avg ~500, got {}",
            stats.avg_cycle_time_us
        );
    }

    #[test]
    fn test_stats_min_max() {
        let mut stats = CycleStats::default();

        stats.update(800,  1000);
        stats.update(1200, 1000);
        stats.update(400,  1000);

        assert_eq!(stats.max_cycle_time_us, 1200);
        // min ignores first cycle
        assert_eq!(stats.min_cycle_time_us, 400);
    }

    #[test]
    fn test_stats_consecutive_overruns() {
        let mut stats = CycleStats::default();

        stats.update(1100, 1000);
        stats.update(1200, 1000);
        assert_eq!(stats.consecutive_overruns, 2);
        assert_eq!(stats.overrun_count,        2);

        // recovery resets consecutive
        stats.update(800, 1000);
        assert_eq!(stats.consecutive_overruns, 0);
        assert_eq!(stats.overrun_count,        2);
    }

    #[test]
    fn test_stats_utilization() {
        let mut stats = CycleStats::default();
        stats.update(750, 1000);

        assert!(
            (stats.utilization(1000) - 0.75).abs() < 0.001
        );
    }

    #[test]
    fn test_overrun_percentage() {
        let mut stats = CycleStats::default();

        // 2 normal, 1 overrun
        stats.update(800,  1000);
        stats.update(900,  1000);
        stats.update(1100, 1000);

        let pct = stats.overrun_count as f64
            / stats.cycle_count as f64 * 100.0;

        assert!(
            (pct - 33.33).abs() < 0.1,
            "Expected ~33.3%, got {:.2}%", pct
        );
    }
}

