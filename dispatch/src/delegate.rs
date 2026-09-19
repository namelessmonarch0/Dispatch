//! `dispatch delegate`: asking Dispatch to run one task in a second agent.
//!
//! Runs inside a pane, as a command the agent there executes. It blocks until
//! the subagent has finished, prints what that subagent printed, and exits with
//! its code — the shape of every other tool an agent runs.
//!
//! Status lines go to stderr and the subagent's output to stdout, so
//! `dispatch delegate "…" > result.md` captures the work and nothing else while
//! a human watching the pane still sees progress.

use std::process::ExitCode;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use dispatch_client::{Client, ClientError};
use dispatch_core::PaneId;
use dispatch_proto::{ClientMessage, DelegateOutcome, Role, ServerMessage};

/// Exit codes, following `sysexits(3)` so an agent can branch without parsing
/// prose. Documented in `--help` because an agent reads that far more often
/// than a README.
mod exit {
    /// No daemon is listening.
    pub const UNAVAILABLE: u8 = 69;
    /// The request was never answered, or the subagent's pane was closed under
    /// it rather than allowed to exit. Both mean the work has no verdict.
    pub const TEMPFAIL: u8 = 75;
    /// The user said no.
    pub const NOPERM: u8 = 77;
    /// A cap, or a harness with no task form.
    pub const CONFIG: u8 = 78;
}

/// Runs one delegation to completion.
pub fn run(harness: Option<String>, size: (u16, u16), task: &str) -> Result<ExitCode> {
    let parent: PaneId = std::env::var("DISPATCH_PANE")
        .context("DISPATCH_PANE is not set: `dispatch delegate` runs inside a Dispatch pane")?
        .parse()
        .context("DISPATCH_PANE does not name a pane")?;

    let client = match Client::attach_as(Role::Delegate, "dispatch delegate") {
        Ok(client) => client,
        Err(ClientError::NotRunning(endpoint)) => {
            eprintln!("[dispatch] no daemon is listening on {endpoint}");
            return Ok(ExitCode::from(exit::UNAVAILABLE));
        }
        Err(error) => return Err(error).context("failed to reach the daemon"),
    };

    // The parent's own harness when none was named: the common case is an agent
    // delegating to another of itself. The daemon resolves an empty string to
    // whichever harness the asking pane is running.
    let harness = harness.unwrap_or_default();

    client.send(ClientMessage::DelegateRequest {
        parent,
        harness,
        task: task.to_string(),
        size,
    });
    eprintln!("[dispatch] waiting for approval (pane {parent})");

    // The daemon owns the approval deadline and refuses a request whose time is
    // up. This one is only a backstop for a daemon that dies without closing its
    // socket cleanly, so it is deliberately longer than any the daemon enforces:
    // the two must never race to answer the same request.
    let backstop = Instant::now() + Duration::from_secs(24 * 60 * 60);

    loop {
        for message in client.poll() {
            match message {
                ServerMessage::DelegateResolved { outcome, .. } => match outcome {
                    DelegateOutcome::Approved { pane } => {
                        eprintln!("[dispatch] approved; subagent pane {pane}");
                    }
                    DelegateOutcome::Denied => {
                        eprintln!("[dispatch] denied");
                        return Ok(ExitCode::from(exit::NOPERM));
                    }
                    DelegateOutcome::Refused { reason } => {
                        eprintln!("[dispatch] refused: {reason}");
                        return Ok(ExitCode::from(exit::CONFIG));
                    }
                },

                ServerMessage::DelegateFinished {
                    exit: code, tail, ..
                } => {
                    use std::io::Write;
                    std::io::stdout()
                        .write_all(&tail)
                        .context("failed to write the subagent's output")?;
                    std::io::stdout().flush().ok();

                    // -1 is the daemon's sentinel for a subagent that was
                    // killed rather than allowed to exit — its pane was closed
                    // out from under it. No real process exit can produce -1,
                    // and reporting it as ordinary failure (1) would hide that
                    // the work has no verdict at all. TEMPFAIL is the same code
                    // used for an unanswered request, since both mean the same
                    // thing to a caller: try again, this did not finish.
                    if code == -1 {
                        eprintln!("[dispatch] the subagent was stopped before it finished");
                        return Ok(ExitCode::from(exit::TEMPFAIL));
                    }

                    eprintln!("[dispatch] subagent exited {code}");
                    return Ok(ExitCode::from(u8::try_from(code).unwrap_or(1)));
                }

                ServerMessage::Error { error } => {
                    eprintln!("[dispatch] {error}");
                    return Ok(ExitCode::from(exit::CONFIG));
                }

                _ => {}
            }
        }

        // A dropped connection ends the wait, and reconnecting cannot rescue it:
        // the daemon abandons a caller's pending request the moment its socket
        // closes, and a client that reconnects arrives with a new id the answer
        // could not be routed to. Resuming a delegation across a reconnect would
        // mean the daemon holding the request and re-addressing its answer —
        // a feature, not a retry.
        if !client.is_connected() {
            eprintln!("[dispatch] the connection dropped; the request was abandoned with it");
            return Ok(ExitCode::from(exit::TEMPFAIL));
        }

        if Instant::now() >= backstop {
            eprintln!("[dispatch] gave up waiting for the daemon");
            return Ok(ExitCode::from(exit::TEMPFAIL));
        }

        std::thread::sleep(Duration::from_millis(20));
    }
}
