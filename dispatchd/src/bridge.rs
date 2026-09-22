//! Carrying one client's frames to a daemon over this process's own pipes.
//!
//! What makes a remote machine reachable: `ssh host dispatchd --stdio` puts
//! this on the far end of an SSH session, and the client on the near end
//! speaks the ordinary protocol into it.
//!
//! Deliberately ignorant of what it carries. A bridge that parsed frames
//! would refuse a version it did not know -- and so break a client the daemon
//! behind it could have served perfectly well.

use std::io::{Read, Write};
use std::path::Path;

use anyhow::{Context, Result};
use dispatch_os::ipc::Connection;

/// Pumps bytes between this process's stdin/stdout and the daemon on
/// `endpoint`, until either side closes.
pub fn run(endpoint: &Path) -> Result<()> {
    let connection = Connection::connect_to(endpoint)
        .with_context(|| format!("cannot reach the daemon on {}", endpoint.display()))?;
    let (from_daemon, to_daemon) = connection.split();

    // One thread each way: a pump that read and wrote on one thread would
    // deadlock the moment both directions had something to say.
    let outward = std::thread::spawn(move || {
        let stdin = std::io::stdin().lock();
        pump(stdin, to_daemon);
        // Closing tells the daemon the client has gone, rather than leaving
        // it holding a connection nothing will ever speak on again.
    });

    let stdout = std::io::stdout().lock();
    pump(from_daemon, stdout);

    // The daemon closed. The client's next write fails and it redials, so
    // waiting on the other pump would only delay this process's exit.
    drop(outward);
    Ok(())
}

/// Copies from `reader` to `writer` a chunk at a time, flushing after each
/// one, until either side ends the stream.
///
/// `std::io::copy` will not do: it only flushes once the reader hits EOF,
/// and `Stdout` is line-buffered, so a frame with no newline in it -- most of
/// them -- would sit in that buffer indefinitely while the client on the
/// other end waited for a reply that had, in fact, already been written.
fn pump(mut reader: impl Read, mut writer: impl Write) {
    let mut buf = [0u8; 8192];
    loop {
        let read = match reader.read(&mut buf) {
            Ok(0) => return,
            // A signal caught mid-syscall, not the other side closing: the
            // pump's whole job is to keep carrying bytes until a side
            // actually ends the stream, and `std::io::copy` -- what this
            // loop replaced -- retried here too. Treating it as EOF would
            // tear down the pump, and with it the whole bridge, on a stray
            // EINTR.
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => return,
            Ok(read) => read,
        };
        if writer.write_all(&buf[..read]).is_err() || writer.flush().is_err() {
            return;
        }
    }
}
