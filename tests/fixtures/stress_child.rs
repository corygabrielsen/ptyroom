//! Synthetic child for the recorder stress tests.
//!
//! Emits pattern + trailing in one `print!` (one write syscall on
//! Linux), so the PTY delivers them as a single chunk and the drainer
//! buffer is guaranteed to hold both bytes by the time the recorder's
//! `consume` runs. That makes the `wait_for` cutoff contract
//! deterministically observable: a primitive that returns "everything
//! in buffer" puts trailing bytes in the `wait_for` event; the corrected
//! "consume up to `pattern_end`" puts them in the next event.
//!
//! Pure-Rust replacement for `tests/fixtures/stress_child.sh`.

use std::io::Write as _;

fn main() {
    let mut out = std::io::stdout().lock();
    out.write_all(b"PROMPT$ payload-extra-bytes-here")
        .expect("write");
    out.flush().expect("flush");
}
