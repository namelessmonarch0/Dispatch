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

1. Opens a transport connection and writes a 13-byte preamble: one role byte
   followed by a 12-byte token.
2. Opens a second transport connection and writes the same token with the other
   role byte.
3. Returns a `Connection` carrying both, reading on the one it marked
   `TO_CLIENT` and writing on the one it marked `TO_SERVER`.

The token identifies which two connections belong together. It is a process
identifier and a counter, not random bytes: it only has to distinguish
connections being paired at the same moment on one machine, and a counter
guarantees that where randomness merely makes a collision unlikely. It is not a
credential — the transport's own access control, mode `0600` on the Unix socket
and the default owner-only DACL on the named pipe, is what keeps other users
out, exactly as before.

The role byte is named from the client's point of view, so the two ends agree
without either having to invert anything implicitly:

| Byte | Meaning |
| --- | --- |
| `0x01` (`TO_SERVER`) | the client writes here; the daemon reads |
| `0x02` (`TO_CLIENT`) | the client reads here; the daemon writes |

A listener accepting connections:

1. Accepts a raw connection and reads its preamble.
2. Holds half-pairs in a table keyed by token, returning a `Connection` as soon
   as a token has both roles.
3. Drops any connection whose preamble could not be read, and keeps accepting.

The preamble is read in the accept loop, not on a thread of its own. A client
that connects and then neither writes nor exits therefore holds up the loop for
every future client. That is accepted rather than defended against: the peer
can only be a process of the same user, since the transport admits no one else,
and a client that *dies* mid-handshake closes its connection instead, which
fails the read at once. Unix bounds the wait further with a two-second read
timeout on the preamble, cleared before the frame loop sees the connection. A
synchronous named pipe read cannot be given a timeout, so Windows has only the
first bound.

A thread per arriving connection was the alternative. It was rejected because
waking a thread parked in `accept` on listener drop needs a platform-specific
poke on both platforms, and without one the threads outlive the listener still
holding its endpoint open — trading a documented stall for a leak in every
test that binds.

Two preambles that disagree — a duplicate role for one token — are both
dropped. That is a client bug, not a state to reconcile.

## What this does not change

The wire protocol is untouched: no new message, no version bump. The preamble
sits below framing, in `dispatch-os`, where the transport already lives. There
is no compatibility story to tell, because a client and a daemon that disagree
about pairing cannot complete a handshake and so never exchange a frame to
misread. `VERSION` stays at 1.0.

`Connection` keeps `connect`, `connect_to`, `split` and its `Read`/`Write`
impls, so both liveness probes keep working. `split` loses its `Result`: with
the two connections already open there is nothing left in it that can fail, and
an always-`Ok` return is a lie the six call sites would have to keep
handling.

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
- **One shared lock for the tests that redirect the configuration directory.**
  `DISPATCH_CONFIG_DIR` belongs to the process, so a test that redirects it
  changes what every other test resolves — including between two calls inside
  one assertion. The transport tests already serialised among themselves; the
  four new ones widened the window enough that `dispatch-os`'s path tests began
  failing, so every test that sets or reads the variable now takes one
  crate-wide lock.
- **The existing suites are the integration test.** `dispatch-client`,
  `dispatchd`, `delegate_shim`, and `end_to_end` all cross the transport; they
  must stay green on Unix and are the evidence that Windows is fixed when CI
  turns green.

## Verification

Windows cannot be tested locally. The fix is verified by the CI matrix job, and
the standard for "done" is the Windows job green — not the Unix suites passing,
which they already do with the defect present.
