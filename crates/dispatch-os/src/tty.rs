//! Asking the terminal a question before anything else reads its answers.

use std::time::Duration;

/// Writes `query` to the terminal and collects what it answers, until `done`
/// says the answer is complete or `timeout` passes.
///
/// Reads standard input directly, so it must run before anything else is
/// reading it: an event loop already running would take the answers for
/// keystrokes. Sends nothing and returns nothing when standard input is not a
/// terminal, and on Windows, which has no way to wait on it with a deadline.
pub fn ask(query: &[u8], timeout: Duration, done: impl FnMut(&[u8]) -> bool) -> Vec<u8> {
    imp::ask(query, timeout, done)
}

#[cfg(unix)]
mod imp {
    use std::io::{IsTerminal, Write};
    use std::time::{Duration, Instant};

    pub(super) fn ask(query: &[u8], timeout: Duration, done: impl FnMut(&[u8]) -> bool) -> Vec<u8> {
        if !std::io::stdin().is_terminal() {
            return Vec::new();
        }

        let mut out = std::io::stdout();
        if out.write_all(query).and_then(|()| out.flush()).is_err() {
            return Vec::new();
        }

        collect(timeout, poll_stdin, read_stdin, done)
    }

    /// The read loop itself, with the two operations that touch a real
    /// descriptor taken as closures so a test can stand in for the terminal
    /// that is not there.
    ///
    /// `wait(left)` is one bounded wait for readability: `Ok(true)` when
    /// there is something to read, `Ok(false)` once `left` has passed with
    /// nothing, `Err` on a failure — except an interrupted wait, which is
    /// retried rather than taken as one, since it says nothing about whether
    /// an answer is coming. `read` fills the buffer and returns how much of
    /// it was written; `0` or an `Err` ends the loop, the same as at the real
    /// descriptor.
    fn collect(
        timeout: Duration,
        mut wait: impl FnMut(Duration) -> std::io::Result<bool>,
        mut read: impl FnMut(&mut [u8]) -> std::io::Result<usize>,
        mut done: impl FnMut(&[u8]) -> bool,
    ) -> Vec<u8> {
        let mut answer = Vec::new();
        let deadline = Instant::now() + timeout;
        let mut chunk = [0_u8; 256];

        while !done(&answer) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }

            match wait(left) {
                Ok(true) => {}
                Ok(false) => break,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => break,
            }

            let Ok(read) = read(&mut chunk) else {
                break;
            };
            if read == 0 {
                break;
            }
            answer.extend_from_slice(&chunk[..read]);
        }

        answer
    }

    /// Waits for standard input to be readable, or for `left` to pass with
    /// nothing.
    fn poll_stdin(left: Duration) -> std::io::Result<bool> {
        let mut ready = libc::pollfd {
            fd: libc::STDIN_FILENO,
            events: libc::POLLIN,
            revents: 0,
        };
        let wait = libc::c_int::try_from(left.as_millis())
            .unwrap_or(libc::c_int::MAX)
            .max(1);

        // SAFETY: one pollfd, owned by this frame, and a count of one.
        let polled = unsafe { libc::poll(&raw mut ready, 1, wait) };
        if polled < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(polled > 0)
    }

    /// Reads from the descriptor rather than through `Stdin`, whose buffer
    /// would keep whatever it read past the answer from the event loop that
    /// reads next.
    fn read_stdin(chunk: &mut [u8]) -> std::io::Result<usize> {
        // SAFETY: the buffer is `chunk`, owned by the caller, and the length
        // passed is its own.
        let read =
            unsafe { libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), chunk.len()) };
        usize::try_from(read).map_err(|_| std::io::Error::last_os_error())
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::cell::Cell;

        /// Whether `bytes` holds a complete primary-device-attributes reply:
        /// `ESC [ ? … c`. Standing in for the real parser, which lives in
        /// `dispatch-tui` and so cannot be a dependency here.
        fn has_device_attributes_reply(bytes: &[u8]) -> bool {
            let Some(start) = bytes.windows(3).position(|w| w == b"\x1b[?") else {
                return false;
            };
            bytes[start + 3..].contains(&b'c')
        }

        #[test]
        fn the_loop_stops_at_the_device_attributes_reply_without_waiting_again() {
            let chunks: [&[u8]; 2] = [b"\x1b]11;rgb:1616/1616/1e1e\x07", b"\x1b[?62c"];
            let mut next = 0usize;
            let wait_calls = Cell::new(0usize);

            let wait = |_left: Duration| {
                wait_calls.set(wait_calls.get() + 1);
                Ok(true)
            };
            let read = move |buf: &mut [u8]| {
                let chunk = chunks[next];
                buf[..chunk.len()].copy_from_slice(chunk);
                next += 1;
                Ok(chunk.len())
            };

            let answer = collect(
                Duration::from_secs(1),
                wait,
                read,
                has_device_attributes_reply,
            );

            let mut expected = Vec::new();
            expected.extend_from_slice(b"\x1b]11;rgb:1616/1616/1e1e\x07");
            expected.extend_from_slice(b"\x1b[?62c");
            assert_eq!(answer, expected);
            assert_eq!(
                wait_calls.get(),
                2,
                "one wait before each chunk, and none after the reply completed it"
            );
        }

        #[test]
        fn the_loop_stops_at_the_deadline_when_nothing_is_ever_readable() {
            // A real `poll` blocks for up to its timeout before saying
            // nothing arrived; the fake mirrors that by sleeping the wait it
            // was given rather than returning at once, so the loop cannot
            // pass this assertion by finishing instantly.
            let wait = |left: Duration| {
                std::thread::sleep(left);
                Ok(false)
            };
            let read = |_chunk: &mut [u8]| -> std::io::Result<usize> {
                panic!("nothing was ever readable, so nothing should be read")
            };

            let start = Instant::now();
            let answer = collect(Duration::from_millis(50), wait, read, |_| false);
            let elapsed = start.elapsed();

            assert!(answer.is_empty());
            assert!(
                elapsed >= Duration::from_millis(50),
                "the deadline should be waited out: {elapsed:?}"
            );
            assert!(
                elapsed < Duration::from_secs(1),
                "and not overrun it: {elapsed:?}"
            );
        }

        #[test]
        fn an_interrupted_wait_is_retried_rather_than_ending_the_answer() {
            let wait_calls = Cell::new(0usize);
            let interrupted = Cell::new(false);

            let wait = |_left: Duration| {
                wait_calls.set(wait_calls.get() + 1);
                if interrupted.get() {
                    Ok(true)
                } else {
                    interrupted.set(true);
                    Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
                }
            };
            let read = |buf: &mut [u8]| {
                buf[..3].copy_from_slice(b"abc");
                Ok(3)
            };

            let answer = collect(Duration::from_secs(1), wait, read, |bytes| {
                !bytes.is_empty()
            });

            assert_eq!(answer, b"abc");
            assert_eq!(
                wait_calls.get(),
                2,
                "the interrupted wait was retried rather than taken as a failure"
            );
        }
    }
}

#[cfg(windows)]
mod imp {
    use std::time::Duration;

    pub(super) fn ask(
        _query: &[u8],
        _timeout: Duration,
        _done: impl FnMut(&[u8]) -> bool,
    ) -> Vec<u8> {
        Vec::new()
    }
}
