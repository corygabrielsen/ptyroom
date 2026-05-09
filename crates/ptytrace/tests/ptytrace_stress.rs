//! PtyTracer-layer stress tests.
//!
//! These tests exercise the recorder's timing-sensitive primitives
//! against a synthetic host child compiled from
//! `tests/fixtures/stress_child.rs` and assert library-level correctness
//! contracts directly — not through any application-layer script.
//!
//! Architectural rule: this file imports `ptytrace::*` only —
//! it must not depend on any consumer crate. The recorder library is
//! meant to be domain-generic, and these tests guard that seam.
//! Consumer-layer integration coverage belongs in the consumer crate.

use std::collections::HashSet;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use ptytrace::pty::{PtyTracer, PtyTracerConfig};
use ptytrace::trace::{EventKind, Trace};

const PATTERN: &[u8] = b"PROMPT$ ";
const TRAILING: &str = "payload-extra-bytes-here";

const PARALLEL_THREADS: usize = 4;
const PARALLEL_RUNS_PER_THREAD: usize = 12;
const CONTENTION_RUNS: usize = 30;
const CONTENTION_BURNERS: usize = 4;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn fixture_path() -> String {
    static PATH: OnceLock<PathBuf> = OnceLock::new();
    PATH.get_or_init(build_stress_child)
        .to_string_lossy()
        .into_owned()
}

fn build_stress_child() -> PathBuf {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src = manifest_dir.join("tests/fixtures/stress_child.rs");
    let out_dir = std::env::var_os("CARGO_TARGET_TMPDIR")
        .map_or_else(|| manifest_dir.join("target/test-fixtures"), PathBuf::from);
    std::fs::create_dir_all(&out_dir).expect("create stress fixture output dir");
    let exe = out_dir.join("stress-child");
    let rustc = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let status = Command::new(rustc)
        .arg("--edition=2024")
        .arg(src)
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("compile stress fixture");
    assert!(status.success(), "stress fixture compilation failed");
    exe
}

fn run_trace() -> anyhow::Result<Trace> {
    let cfg = PtyTracerConfig {
        container: None,
        max_runtime: Duration::from_secs(10),
        ..PtyTracerConfig::default()
    };
    let mut r = PtyTracer::spawn(cfg, &[fixture_path().as_str()])?;
    r.send_raw_wait_for(&[], ms(0), PATTERN, ms(2000), "wait_pattern")?;
    // Capture any leftover bytes that the wait_for event correctly
    // declined to absorb.
    r.dwell(ms(0), ms(150))?;
    r.stop()
}

fn output_event_data(trace: &Trace) -> Vec<&str> {
    trace
        .events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Output))
        .map(|e| e.data.as_str())
        .collect()
}

fn trace_string(trace: &Trace) -> String {
    trace.to_string()
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
        "{label}: {} distinct traces across {} runs.\n\
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
    let trace = run_trace().expect("run trace");
    let events = output_event_data(&trace);
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

/// Stability under parallel load: same fixture, same trace path, run across
/// multiple threads. Asserts the trace is byte-identical across all runs.
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
                    let trace = run_trace().expect("run trace");
                    local.push(trace_string(&trace));
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
        let trace = run_trace().expect("run trace");
        runs.push(trace_string(&trace));
    }

    stop.store(true, Ordering::Relaxed);
    for b in burners {
        let _ = b.join();
    }

    if distinct_count(&runs) != 1 {
        fail_with_variants("cpu-burn", CONTENTION_RUNS, &runs);
    }
}
