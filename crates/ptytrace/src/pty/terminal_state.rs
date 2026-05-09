//! Terminal state cleanup shared by interactive PTY frontends.

use std::os::fd::{BorrowedFd, RawFd};
use std::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};

use nix::errno::Errno;
#[cfg(not(test))]
use nix::libc;
use nix::unistd::write;

const NO_SEQUENCE: u8 = 0;
const CHILD_OUTPUT_SEQUENCE: u8 = 1;
const VIEWPORT_SEQUENCE: u8 = 2;
#[cfg(not(test))]
const TERMINATION_SIGNALS: [libc::c_int; 4] =
    [libc::SIGINT, libc::SIGTERM, libc::SIGHUP, libc::SIGQUIT];

static TERMINATION_REQUESTED: AtomicBool = AtomicBool::new(false);
static SIGNAL_RESTORE_FD: AtomicI32 = AtomicI32::new(-1);
static SIGNAL_RESTORE_SEQUENCE: AtomicU8 = AtomicU8::new(NO_SEQUENCE);

const GENERAL_RESTORE_SEQUENCE: &[u8] =
    b"\x1b[0m\x1b[?25h\x1b[?1l\x1b>\x1b[?2004l\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l\x1b[?1049l\x1b[?25h";

/// Cleanup for frontends that pass child PTY output directly to the
/// user's terminal.
#[must_use]
pub const fn child_output_restore_sequence() -> &'static [u8] {
    GENERAL_RESTORE_SEQUENCE
}

/// Cleanup for `ptyroom join` viewport mode.
#[must_use]
pub const fn viewport_restore_sequence() -> &'static [u8] {
    GENERAL_RESTORE_SEQUENCE
}

pub struct RestoreGuard {
    fd: RawFd,
    sequence: &'static [u8],
    _signal_handlers: Option<SignalHandlers>,
}

impl RestoreGuard {
    #[must_use]
    pub fn new(fd: RawFd, sequence: &'static [u8]) -> Self {
        clear_termination_request();
        SIGNAL_RESTORE_FD.store(fd, Ordering::SeqCst);
        SIGNAL_RESTORE_SEQUENCE.store(sequence_kind(sequence), Ordering::SeqCst);
        Self {
            fd,
            sequence,
            _signal_handlers: SignalHandlers::install(),
        }
    }

    pub fn set_fd(&mut self, fd: RawFd) {
        self.fd = fd;
        SIGNAL_RESTORE_FD.store(fd, Ordering::SeqCst);
    }
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        restore_fd_best_effort(self.fd, self.sequence);
        SIGNAL_RESTORE_FD.store(-1, Ordering::SeqCst);
        SIGNAL_RESTORE_SEQUENCE.store(NO_SEQUENCE, Ordering::SeqCst);
        clear_termination_request();
    }
}

#[must_use]
pub fn termination_requested() -> bool {
    TERMINATION_REQUESTED.load(Ordering::SeqCst)
}

pub fn clear_termination_request() {
    TERMINATION_REQUESTED.store(false, Ordering::SeqCst);
}

pub fn restore_fd_best_effort(fd: RawFd, sequence: &'static [u8]) {
    let _ = write_all(fd, sequence);
}

fn sequence_kind(sequence: &'static [u8]) -> u8 {
    if sequence.as_ptr() == child_output_restore_sequence().as_ptr()
        && sequence.len() == child_output_restore_sequence().len()
    {
        CHILD_OUTPUT_SEQUENCE
    } else if sequence.as_ptr() == viewport_restore_sequence().as_ptr()
        && sequence.len() == viewport_restore_sequence().len()
    {
        VIEWPORT_SEQUENCE
    } else {
        CHILD_OUTPUT_SEQUENCE
    }
}

#[cfg(not(test))]
extern "C" fn handle_termination_signal(_signal: libc::c_int) {
    TERMINATION_REQUESTED.store(true, Ordering::SeqCst);
    let fd = SIGNAL_RESTORE_FD.load(Ordering::SeqCst);
    if fd < 0 {
        return;
    }
    match SIGNAL_RESTORE_SEQUENCE.load(Ordering::SeqCst) {
        CHILD_OUTPUT_SEQUENCE => write_signal_safe(fd, child_output_restore_sequence()),
        VIEWPORT_SEQUENCE => write_signal_safe(fd, viewport_restore_sequence()),
        _ => {}
    }
}

#[cfg(not(test))]
fn write_signal_safe(fd: RawFd, mut bytes: &[u8]) {
    while !bytes.is_empty() {
        let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
        if written <= 0 {
            return;
        }
        let Ok(written) = usize::try_from(written) else {
            return;
        };
        bytes = &bytes[written..];
    }
}

fn write_all(fd: RawFd, mut bytes: &[u8]) -> anyhow::Result<()> {
    while !bytes.is_empty() {
        let borrowed = unsafe { BorrowedFd::borrow_raw(fd) };
        match write(borrowed, bytes) {
            Ok(0) => anyhow::bail!("terminal restore write returned 0"),
            Ok(n) => bytes = &bytes[n..],
            Err(Errno::EINTR) => {}
            Err(err) => return Err(anyhow::anyhow!("terminal restore write failed: {err}")),
        }
    }
    Ok(())
}

#[cfg(not(test))]
struct SignalHandlers {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

#[cfg(not(test))]
impl SignalHandlers {
    fn install() -> Option<Self> {
        let mut previous = Vec::with_capacity(TERMINATION_SIGNALS.len());
        for signal in TERMINATION_SIGNALS {
            let mut action = empty_sigaction();
            action.sa_sigaction = handle_termination_signal as *const () as libc::sighandler_t;
            action.sa_flags = 0;
            unsafe {
                libc::sigemptyset(&raw mut action.sa_mask);
            }
            let mut old = empty_sigaction();
            let rc = unsafe { libc::sigaction(signal, &raw const action, &raw mut old) };
            if rc != 0 {
                restore_previous_handlers(&previous);
                return None;
            }
            previous.push((signal, old));
        }
        Some(Self { previous })
    }
}

#[cfg(not(test))]
impl Drop for SignalHandlers {
    fn drop(&mut self) {
        restore_previous_handlers(&self.previous);
    }
}

#[cfg(not(test))]
fn restore_previous_handlers(previous: &[(libc::c_int, libc::sigaction)]) {
    for &(signal, action) in previous.iter().rev() {
        unsafe {
            libc::sigaction(signal, &raw const action, std::ptr::null_mut());
        }
    }
}

#[cfg(not(test))]
fn empty_sigaction() -> libc::sigaction {
    unsafe { std::mem::zeroed() }
}

#[cfg(test)]
struct SignalHandlers;

#[cfg(test)]
impl SignalHandlers {
    fn install() -> Option<Self> {
        None
    }
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;

    use nix::unistd::{pipe, read};

    use super::{RestoreGuard, child_output_restore_sequence, viewport_restore_sequence};

    #[test]
    fn child_output_restore_shows_cursor_after_leaving_alt_screen() {
        assert_cursor_visible_after_alt_screen_exit(child_output_restore_sequence());
    }

    #[test]
    fn viewport_restore_shows_cursor_after_leaving_alt_screen() {
        assert_cursor_visible_after_alt_screen_exit(viewport_restore_sequence());
    }

    #[test]
    fn restore_guard_writes_sequence_on_drop() {
        let (read_fd, write_fd) = pipe().unwrap();

        {
            let _guard = RestoreGuard::new(write_fd.as_raw_fd(), child_output_restore_sequence());
        }
        drop(write_fd);

        let mut output = Vec::new();
        let mut buf = [0_u8; 128];
        loop {
            match read(&read_fd, &mut buf).unwrap() {
                0 => break,
                n => output.extend_from_slice(&buf[..n]),
            }
        }

        assert_eq!(output, child_output_restore_sequence());
    }

    fn assert_cursor_visible_after_alt_screen_exit(sequence: &[u8]) {
        let alt_screen_exit = b"\x1b[?1049l";
        let show_cursor = b"\x1b[?25h";
        let alt_pos = find_subslice(sequence, alt_screen_exit).unwrap();
        let final_show_pos = sequence
            .windows(show_cursor.len())
            .rposition(|window| window == show_cursor)
            .unwrap();

        assert!(alt_pos < final_show_pos);
        assert!(sequence.ends_with(show_cursor));
    }

    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack
            .windows(needle.len())
            .position(|window| window == needle)
    }
}
