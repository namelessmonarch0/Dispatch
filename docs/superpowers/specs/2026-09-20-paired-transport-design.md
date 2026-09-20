# Paired Transport Design

**Date:** 2026-09-20
**Status:** approved

## Problem

Every Dispatch suite that crosses the IPC boundary fails on Windows, and only
on Windows. The pure-logic crates pass there, ConPTY passes there, and the raw
transport test `a_client_and_server_exchange_bytes` passes there — that test
alternates a write with a read. The failures all share one shape: a reader
thread parked in a blocking read while a writer thread on the other end of the
same connection tries to write.

The cause is `Connection::split`. It hands out `try_clone()` of a single
handle. On Windows `try_clone` is `DuplicateHandle`, which produces a second
handle onto the *same file object*. A file object opened for synchronous I/O
serialises its operations: while a `ReadFile` is outstanding, a `WriteFile`
through either handle waits for it. The reader parks forever waiting for a
message the writer cannot send.

The cleanest isolation of the defect is `crates/dispatch-client/src/tests.rs`'s
`subscribing is explicit` case: `Hello` and `Welcome` cross the pipe, then
`Subscribe` — the first frame written while the reader thread is parked — never
arrives.

This is not a delegation bug. It predates delegation, which is merely the first
feature that pushes frames in both directions at once.

## Approach

One logical connection becomes two transport connections, one per direction,
paired by a token the client sends on each.

The alternatives considered were overlapped I/O on Windows and adopting
tokio's `named_pipe`. Both were rejected for the same reason: they put the
correctness of the fix on the one platform that cannot be exercised locally.
Pairing is ordinary blocking I/O on both platforms, so the Unix test suite
exercises the identical state machine that runs on Windows.

## The pairing handshake

Both platforms run it. This is the point of the design: there is one code path,
and it is the one CI can only test slowly.

A client that wants a connection:

1. Opens a transport connection and writes a 17-byte preamble: one role byte
   followed by a 16-byte token.
2. Opens a second transport connection and writes the same token with the other
   role byte.
3. Returns a `Connection` carrying both, reading on the one it marked
   `TO_CLIENT` and writing on the one it marked `TO_SERVER`.

The token is 16 random bytes, and it identifies which two connections belong
together. It is not a credential: the transport's own access control — mode
`0600` on the Unix socket, the default owner-only DACL on the named pipe — is
what keeps other users out, exactly as before. Pairing has to survive two
clients connecting at the same instant, and that is all the token is for.

The role byte is named from the client's point of view, so the two ends agree
without either having to invert anything implicitly:

| Byte | Meaning |
| --- | --- |
| `0x01` (`TO_SERVER`) | the client writes here; the daemon reads |
| `0x02` (`TO_CLIENT`) | the client reads here; the daemon writes |

A listener accepting connections:

1. Accepts a raw connection.
2. Reads its preamble **on a short-lived thread**, then sends the
   `(token, role, stream)` to the listener over a channel.
3. Holds half-pairs in a table keyed by token, and returns a `Connection` as
   soon as a token has both roles.

The preamble read happens off the accept loop deliberately. A client that
connects and then neither writes nor exits would otherwise wedge the daemon's
accept loop for every future client. Neither platform can put a timeout on a
blocking read of a named pipe without overlapped I/O, so the bound comes from
the thread being disposable instead. A client that dies mid-handshake closes
its connection, the read fails at once, and the half-pair is dropped.

Two preambles that disagree — a duplicate role for one token — are both
dropped. That is a client bug, not a state to reconcile.

## What this does not change

The wire protocol is untouched: no new message, no version bump. The preamble
sits below framing, in `dispatch-os`, where the transport already lives. There
is no compatibility story to tell, because a client and a daemon that disagree
about pairing cannot complete a handshake and so never exchange a frame to
misread. `VERSION` stays at 1.0.

`Connection` keeps its public shape — `connect`, `connect_to`, `split`, and its
`Read`/`Write` impls — so all six existing `split()` call sites, both liveness
probes, and every test compile unchanged.

## Windows: a busy pipe must be waited for, not refused

A listener keeps exactly one unconnected pipe instance and creates the next
only once `ConnectNamedPipe` returns. Two connects per client makes the gap
between those two events something a client will actually land in, where today
it is a rare race. `connect` currently maps `ERROR_PIPE_BUSY` to a generic I/O
error, which would surface as a spurious "cannot reach the daemon".

`connect` therefore retries on `ERROR_PIPE_BUSY` for up to two seconds, which
matches the client's existing `HANDSHAKE_TIMEOUT`. A busy pipe means a daemon
is there; the only correct response is to wait for it.

## Testing

Every test below runs on Unix, where it can be watched fail. They are the
Windows tests too.

- **The defect itself, at the transport layer.** A server parks a reader thread
  on a connection and writes from another thread; the client receives the write
  while its own reader is parked. This is the test that fails against
  `try_clone` today on Windows, and it must exist at the `dispatch-os` level so
  that the transport carries its own regression test rather than borrowing
  `dispatch-client`'s.
- **Two clients connecting concurrently are paired correctly.** Interleave two
  clients' connects and assert each `Connection` talks to its own client. This
  is what the token exists for, and without this test the token is untested
  decoration.
- **A half-connection does not wedge the listener.** Open one raw connection,
  write a `TO_SERVER` preamble, abandon it; then connect a complete client and
  assert it is served.
- **A client killed mid-handshake is forgotten.** As above, but drop the raw
  connection without writing anything.
- **The existing suites are the integration test.** `dispatch-client`,
  `dispatchd`, `delegate_shim`, and `end_to_end` all cross the transport; they
  must stay green on Unix and are the evidence that Windows is fixed when CI
  turns green.

## Verification

Windows cannot be tested locally. The fix is verified by the CI matrix job, and
the standard for "done" is the Windows job green — not the Unix suites passing,
which they already do with the defect present.
