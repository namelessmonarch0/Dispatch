# What Dispatch's boundaries are, and are not

## The boundary is the operating-system user

A daemon runs as one user and serves that user alone. On Unix its socket is
mode 0600; on Windows its named pipe carries a DACL admitting that user and
nobody else, and refuses clients on other machines. Reaching another
machine goes through `ssh`, whose authentication is the boundary there.

Anything that can open the socket can do anything a client can: open
projects, start panes running any registered harness, type into any pane,
approve or deny any delegation. There is no second tier of client.

On Windows a client also refuses a pipe whose owner is not the current
user — defence against another user pre-creating the pipe name before the
daemon does — and opens it with identification-only impersonation, so
whoever serves the pipe can learn who is asking but never act as them. An
elevated (administrator) daemon's pipe is owned by that same user's own
SID, not the Administrators group, so an unelevated process of the user's
can open it just as well: elevating the daemon buys no extra boundary, and
running it unelevated avoids the false sense that it does.

## Approval is a workflow, not a sandbox

When an agent asks for a subagent, the request is put to the user, and the
daemon enforces the caps (`max_depth`, `max_live_per_parent`) and the
deadline. That keeps a well-meaning agent from fanning out without anyone
noticing. It does not stop a *malicious* agent:

- `DISPATCH_PANE` says which pane a request comes from. It is attribution,
  not a credential: any process of the user's can set it, or connect to the
  socket directly and act as an interface, approving its own requests.
- A client's role (`interface` or `delegate`) decides what it is sent, not
  what it may do.
- Agents run as the user, with the user's files, keys and network.

If the agents themselves are not trusted, run them under an operating-system
boundary — a separate user, a container, a VM — and give that boundary its
own daemon. A role check or a token in the environment would not be one.

## Opening a project means trusting it

Agents run inside the project directory and read its configuration. On
Windows, `cmd.exe` looks in the current directory before `PATH`, so a
repository that ships, say, `claude.bat` at its root would run in place of
the agent Dispatch meant to start. Dispatch does not defend against a
hostile repository: opening a project is a decision to trust what is in it.

## What the daemon does protect against

A slow, stuck or broken client cannot impair the others: each client's
queue has a byte budget, a client that stops reading is hung up on and
replayed on reconnection, a client that never says hello or stops mid-frame
is hung up on, and at most 64 may connect. A pane that stops reading its
input is refused more input rather than stalling the daemon. On Windows a
task handed to an agent through `cmd.exe` travels on standard input, never
on a command line the shell would parse; the file it travels in is written
into a directory only that user can read, and removed when the pane ends.
Panes on Windows also run inside a Job Object, so ending a pane ends
everything it started, not just the one process Dispatch spawned directly.
