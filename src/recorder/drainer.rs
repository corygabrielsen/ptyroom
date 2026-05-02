//! Background reader for the PTY master fd.
//!
//! Two responsibilities, mirroring the Python prototype:
//!  1. Continuously drain the master fd so the slave (`bash` inside the
//!     container) doesn't block on a full PTY buffer.
//!  2. Watch every drained chunk for OSC 11/10 queries and write the
//!     canned replies back through the master — the recorder is the
//!     terminal emulator from tint's POV.
//!
//! Drained bytes accumulate in a thread-safe buffer; the parent atomically
//! swaps it out via `consume`.

use std::os::fd::{BorrowedFd, RawFd};
use std::sync::{Arc, Mutex};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use nix::sys::select::{FdSet, select};
use nix::sys::time::{TimeVal, TimeValLike};
use nix::unistd::{read, write};

use super::osc::{StubColors, replies_for_chunk, setters_in_chunk};

/// Bytes buffered by the drainer between consume calls.
#[derive(Default)]
struct Buffer { bytes: Vec<u8> }

pub struct Drainer {
    inner: Arc<Mutex<Buffer>>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drainer {
    pub fn start(master_fd: RawFd, stubs: StubColors) -> Self {
        let inner: Arc<Mutex<Buffer>> = Arc::default();
        let stop = Arc::new(AtomicBool::new(false));
        let buf = Arc::clone(&inner);
        let stop_flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("tint-recorder-drainer".into())
            .spawn(move || drain_loop(master_fd, stubs, buf, stop_flag))
            .expect("drainer thread spawn");
        Self { inner, stop, thread: Some(thread) }
    }

    pub fn consume(&self) -> Vec<u8> {
        let mut b = self.inner.lock().expect("drainer mutex poisoned");
        std::mem::take(&mut b.bytes)
    }
}

impl Drop for Drainer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

// `Arc` parameters are moved in but only their refs are used inside the
// loop; the closure that spawns the thread is the actual consumer of the
// owned Arcs (it captures them).
//
// The `stubs` value is the *initial* state. The drainer updates it in place
// as it observes OSC 10/11 setter writes from the child, so subsequent
// query responses reflect the most recently set bg/fg — what tint expects
// when it queries the terminal for the "original" color before opening the
// picker.
#[allow(clippy::needless_pass_by_value)]
fn drain_loop(
    fd: RawFd, mut stubs: StubColors,
    buf: Arc<Mutex<Buffer>>, stop: Arc<AtomicBool>,
) {
    let mut chunk = vec![0u8; 64 * 1024];
    while !stop.load(Ordering::SeqCst) {
        let mut set = FdSet::new();
        // Safety: `fd` is the master end of a PTY whose lifetime exceeds
        // this thread (the parent `Recorder` joins us in `Drop` before
        // dropping the OwnedFd).
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        set.insert(borrowed);
        let mut tv = TimeVal::milliseconds(50);
        match select(Some(fd + 1), Some(&mut set), None, None, Some(&mut tv)) {
            Ok(0) | Err(nix::errno::Errno::EINTR) => continue,
            Ok(_) => {}
            Err(_) => break,
        }
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        let n = match read(borrowed, &mut chunk) {
            Ok(0) => break,
            Ok(n) => n,
            Err(nix::errno::Errno::EINTR) => continue,
            Err(_) => break,
        };
        let read_slice = &chunk[..n];

        // Apply setter writes before answering queries — if a single chunk
        // contains both, tint expects the query to reflect the just-set
        // value.
        for (code, color) in setters_in_chunk(read_slice) {
            match code {
                10 => stubs.fg = color,
                11 => stubs.bg = color,
                _ => {}
            }
        }

        for reply in replies_for_chunk(read_slice, stubs) {
            let mut written = 0;
            while written < reply.len() {
                let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
                match write(borrowed, &reply[written..]) {
                    Ok(0) => break,
                    Ok(k) => written += k,
                    Err(nix::errno::Errno::EINTR) => {}
                    Err(_) => break,
                }
            }
        }

        if let Ok(mut b) = buf.lock() {
            b.bytes.extend_from_slice(read_slice);
        }
    }
}
