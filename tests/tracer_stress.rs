//! Tracer-layer stress tests.
//!
//! These tests exercise the recorder's timing-sensitive primitives
//! against a synthetic host child (`target/debug/stress-child` or the
//! release equivalent, built from `src/bin/stress_child.rs`) and
//! assert library-level correctness contracts directly — not through
//! any application-layer scene.
//!
//! Architectural rule: this file imports `tracer::*` only —
//! it must not depend on any consumer crate. The recorder library is
//! meant to be domain-generic, and these tests guard that seam.
//! Consumer-layer integration coverage belongs in the consumer crate.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use tracer::trace::{EventKind, Trace};
use tracer::tracer::{Tracer, TracerConfig};

const PATTERN: &[u8] = b"PROMPT$ ";
const TRAILING: &str = "payload-extra-bytes-here";

const PARALLEL_THREADS: usize = 4;
const PARALLEL_RUNS_PER_THREAD: usize = 12;
const CONTENTION_RUNS: usize = 30;
const CONTENTION_BURNERS: usize = 4;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

/// Path to the `stress-child` binary cargo built alongside this test.
/// Cargo sets `CARGO_BIN_EXE_<name>` for every `[[bin]]` listed in
/// `Cargo.toml` whenever it builds an integration test, so we never
/// need to hard-code a debug/release path.
fn fixture_path() -> String {
    env!("CARGO_BIN_EXE_stress-child").to_string()
}

fn run_scene() -> anyhow::Result<Trace> {
    let cfg = TracerConfig {
        container: None,
        max_runtime: Duration::from_secs(10),
        ..TracerConfig::default()
    };
    let mut r = Tracer::spawn(cfg, &[fixture_path().as_str()])?;
    r.send_raw_wait_for(&[], ms(0), PATTERN, ms(2000), "wait_pattern")?;
    // Capture any leftover bytes that the wait_for event correctly
    // declined to absorb.
    r.dwell(ms(0), ms(150))?;
    r.stop()
}

fn output_event_data(cast: &Trace) -> Vec<&str> {
    cast.events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Output))
        .map(|e| e.data.as_str())
        .collect()
}

fn cast_string(cast: &Trace) -> String {
    cast.to_string()
}

fn distinct_count(runs: &[String]) -> usize {
    let set: HashSet<&String> = runs.iter().collect();
    set.len()
}

fn fail_with_variants(label: &str, total: usize, runs: &[String]) -> ! {
    let set: HashSet<&String> = runs.iter().collect();
    let mut iter = set.into_iter();
    let first = iter.next().map_or("", std::string::String::as_str);
    let second = iter.next().map_or(first, std::string::String::as_str);
    panic!(
        "{label}: {} distinct casts across {} runs.\n\
         \n--- variant A ---\n{}\n\
         \n--- variant B ---\n{}\n",
        runs.iter().collect::<HashSet<_>>().len(),
        total,
        first,
        second,
    );
}

/// Contract: `send_raw_wait_for` must capture bytes UP TO AND INCLUDING
/// the pattern in this event, regardless of what else is in the drainer
/// buffer. Post-pattern bytes belong to the next event.
///
/// The fixture writes `PATTERN+TRAILING` in a single printf call, so by
/// the time `consume()` runs the drainer buffer is guaranteed to hold
/// both. A primitive that returns "everything in buffer" deterministically
/// puts trailing bytes in this event (the bug). A primitive that splits
/// at `pattern_end` deterministically puts them in the next event.
#[test]
fn wait_for_event_contains_only_up_to_pattern() {
    let cast = run_scene().expect("run scene");
    let events = output_event_data(&cast);
    let pattern_str = std::str::from_utf8(PATTERN).expect("pattern utf8");

    assert_eq!(
        events.first().copied(),
        Some(pattern_str),
        "wait_for event must contain ONLY the pattern, not post-pattern bytes.\n\
         got events: {events:#?}\n\
         \nIf event[0] contains pattern + trailing, the wait_for primitive is\n\
         folding post-pattern bytes into the same event \u{2014} the cutoff race.",
    );
    assert_eq!(
        events.get(1).copied(),
        Some(TRAILING),
        "post-pattern bytes must land in the subsequent event.\n\
         got events: {events:#?}",
    );
}

/// Stability under parallel load: same fixture, same scene, run across
/// multiple threads. Asserts the cast is byte-identical across all runs.
/// Primarily a regression net for future races, not the load-bearing
/// demonstration of the `wait_for` cutoff bug (the contract test above
/// is that).
#[test]
fn wait_for_byte_stable_under_parallel_load() {
    let handles: Vec<_> = (0..PARALLEL_THREADS)
        .map(|_| {
            thread::spawn(|| {
                let mut local: Vec<String> = Vec::with_capacity(PARALLEL_RUNS_PER_THREAD);
                for _ in 0..PARALLEL_RUNS_PER_THREAD {
                    let cast = run_scene().expect("run scene");
                    local.push(cast_string(&cast));
                }
                local
            })
        })
        .collect();

    let mut runs: Vec<String> = Vec::new();
    for h in handles {
        runs.extend(h.join().expect("join"));
    }

    if distinct_count(&runs) != 1 {
        fail_with_variants(
            "parallel",
            PARALLEL_THREADS * PARALLEL_RUNS_PER_THREAD,
            &runs,
        );
    }
}

/// Stability under CPU contention. CPU-burning background threads
/// pressure the recorder/drainer scheduling. Same regression-net role
/// as the parallel test.
#[test]
fn wait_for_byte_stable_under_cpu_burn() {
    let stop = Arc::new(AtomicBool::new(false));
    let burners: Vec<_> = (0..CONTENTION_BURNERS)
        .map(|_| {
            let s = Arc::clone(&stop);
            thread::spawn(move || {
                let mut counter: u64 = 0;
                while !s.load(Ordering::Relaxed) {
                    counter = counter.wrapping_add(1);
                }
                counter
            })
        })
        .collect();

    let mut runs: Vec<String> = Vec::with_capacity(CONTENTION_RUNS);
    for _ in 0..CONTENTION_RUNS {
        let cast = run_scene().expect("run scene");
        runs.push(cast_string(&cast));
    }

    stop.store(true, Ordering::Relaxed);
    for b in burners {
        let _ = b.join();
    }

    if distinct_count(&runs) != 1 {
        fail_with_variants("cpu-burn", CONTENTION_RUNS, &runs);
    }
}
