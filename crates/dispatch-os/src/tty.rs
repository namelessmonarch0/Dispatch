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

    pub(super) fn ask(
        query: &[u8],
        timeout: Duration,
        mut done: impl FnMut(&[u8]) -> bool,
    ) -> Vec<u8> {
        let mut answer = Vec::new();
        if !std::io::stdin().is_terminal() {
            return answer;
        }

        let mut out = std::io::stdout();
        if out.write_all(query).and_then(|()| out.flush()).is_err() {
            return answer;
        }

        let deadline = Instant::now() + timeout;
        let mut chunk = [0_u8; 256];

        while !done(&answer) {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }

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
                if std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                break;
            }
            if polled == 0 {
                break;
            }

            // Read from the descriptor rather than through `Stdin`, whose
            // buffer would keep whatever it read past the answer from the
            // event loop that reads next.
            //
            // SAFETY: the buffer is `chunk`, owned by this frame, and the
            // length passed is its own.
            let read =
                unsafe { libc::read(libc::STDIN_FILENO, chunk.as_mut_ptr().cast(), chunk.len()) };
            let Ok(read) = usize::try_from(read) else {
                break;
            };
            if read == 0 {
                break;
            }
            answer.extend_from_slice(&chunk[..read]);
        }

        answer
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
