# Orchestrator delegation

Status: design, approved 2026-09-19. Not yet implemented.

Dispatch's README has promised, since the first commit, "an orchestrator that
delegates work to spawned subagents under explicit approval". `PaneRole::Orchestrator`
has existed and never been constructed. This is the design that fills both in.

## What this adds

An agent running in a Dispatch pane can ask for a second agent to be started on a
task, wait for it, and read its output — if the user approves. The subagent is a
pane the daemon owns, listed under its parent, and the approval is a prompt the
user answers by hand.

## The shape of the mechanism

Coding agents are black-box CLIs in a pseudoterminal. Dispatch cannot see their
tool calls, so delegation has to be something an agent can *do* from inside its
own session. It runs a command:

```
dispatch delegate [--harness <id>] [--size <cols>x<rows>] <task>
```

A shell command rather than an MCP tool, because Dispatch's premise is several
different agents — `claude`, `codex`, `agy`, `opencode`. Every one of them can run
a command; only some speak MCP. The shape also matches how agents already use
tools: run something, read its output, check its status. An MCP server can be
added later as a second front-end onto the same daemon messages, so this is not a
one-way door.

Rejected: scanning pane output for a sentinel the agent is prompted to emit. The
title scanner gets away with that because a wrong title is cosmetic; a wrong spawn
runs a process, and anything the agent prints — `cat` of a file, a diff, a test
name — could forge one.

## Protocol

Additive changes to `dispatch-proto`. Every new field carries `#[serde(default)]`
and every new variant is one an older peer can ignore, which is what the module's
existing compatibility rule requires.

```rust
// ClientMessage
Hello { version, client, #[serde(default)] role: Role }
DelegateRequest { parent: PaneId, harness: String, task: String, size: (u16, u16) }
DelegateDecision { request: RequestId, approve: bool, #[serde(default)] blanket: bool }

// ServerMessage
DelegatePending  { request: RequestId, parent: PaneId, project: ProjectId,
                   harness: String, task: String, depth: u8 }
DelegateResolved { request: RequestId, outcome: DelegateOutcome }
DelegateFinished { request: RequestId, exit: i32,
                   #[serde(with = "serde_bytes_compat")] tail: Vec<u8> }

enum Role { Interface, Delegate }                 // Interface is the default
enum DelegateOutcome { Approved { pane: PaneId }, Denied, Refused { reason: String } }
```

`Role` exists because the two audiences want different traffic. An `Interface`
connection subscribes to panes and draws them. A `Delegate` connection wants the
fate of its own request and nothing else, so the daemon never sends it pane
output — which also keeps a delegate call fast on a busy fleet.

`RequestId` is a new id in `dispatch-core`, beside `PaneId` and `ProjectId`. A
request has a life before a pane does: it can be refused or denied and never
become one.

## Core types

```rust
pub struct Pane {
    // …existing fields…
    pub role: PaneRole,           // Orchestrator is finally constructed
    pub parent: Option<PaneId>,   // None for a top-level pane
    pub durable: bool,            // approved with [A]; outlives its caller
    pub closed: bool,             // tombstone: closed, still holding live children
}
```

A pane becomes `Orchestrator` when its first child is approved. Nothing asks the
user to declare a pane an orchestrator: there is no mode to learn and no way to
set it wrongly.

`closed` supports the tombstone row. `AppState::close_pane` marks a parent closed
and keeps it while it has live children, and removes it when the last one ends.

## Daemon

New state:

```rust
pending: HashMap<RequestId, Pending>,   // asked, not yet answered
blanket: HashSet<PaneId>,               // panes approved with [A], this daemon's lifetime
limits: DelegationLimits,               // from config.toml
```

`Pending` records the request and its `caller: ClientId`, so the daemon knows
whom to answer and whose disappearance cancels it. None of this is persisted: a
daemon restart forgets blanket approvals, which is the right default for a
permission that was never written down.

A request is handled in this order:

1. **Resolve the parent.** The request names a `PaneId`; a daemon that does not
   own it refuses. The caller learns the id from `DISPATCH_PANE`, which the daemon
   puts in each pane's environment when it spawns it, together with
   `DISPATCH_CONFIG_DIR` so the shim reaches the same endpoint rather than the
   default one. The daemon also prepends the directory holding the `dispatch`
   binary — a sibling of its own executable — to the pane's `PATH`, so the shim is
   runnable; where there is no sibling, `PATH` is untouched and the agent gets
   "command not found" rather than a pretence.

   This is attribution, not a security boundary. The socket is owner-only, and
   anything that can connect to it can already spawn panes. `DISPATCH_PANE`
   decides which pane a request is *attributed* to; it protects nothing.

2. **Enforce the caps, without prompting.** Depth comes from the parent chain, so
   `max_depth = 1` means a pane that has a parent cannot delegate at all. Live
   children are counted per parent against `max_live_per_parent`. A refusal is
   `Refused { reason }` in words, because an agent reads it and should be able to
   act on it.

3. **Require a `[task]` form** on the harness. Without one, refuse, naming the
   harness. This is the check that keeps "block until exit" honest: an interactive
   agent never exits.

4. **Approve or ask.** A parent in `blanket` is approved at once. Otherwise the
   request is queued and `DelegatePending` goes to subscribed `Interface` clients
   only. A request made while no client is subscribed simply waits, which is what
   the deadline below is for.

**On approval** the daemon spawns the pane as `SpawnPane` does, with two
differences: the launch comes from the harness's `[task]` form with the task
substituted, and the new pane records `parent` and `durable`. Interface clients
hear `PaneSpawned` as usual, so every client draws the same tree.

**On the subagent's exit** the caller gets `DelegateFinished { exit, tail }`,
where `tail` is the last 8 KiB of that pane's history — a slice of what replay
already keeps, not new storage. The pane itself stays, marked by outcome.

**When a caller's socket closes** the rule splits by how the subagent was
approved:

- one-off (`a`) — the subagent's tree is terminated and any pending request
  dropped. This covers Ctrl-C on the delegate command, a killed parent and a
  crashed client alike, because all three close the socket.
- blanket (`A`) — it keeps running. If its parent pane has been closed, the parent
  remains as a tombstone.

Closing a parent applies the same split per child, and finished children go with
it: their transcripts were reachable through the parent the user has just closed,
and keeping rows for work that is over under a pane that is gone would leave the
sidebar collecting debris. So closing a parent removes it and every child except
its live durable ones; those keep the parent as a tombstone, and the tombstone
goes when the last of them ends. A pane's blanket approval is forgotten when the
pane is closed, since no further request can come from it.

Daemon shutdown still terminates everything: a durable subagent is durable
against its caller, not against the daemon that owns it.

## Interface

The sidebar gains one level of nesting: children indented under their parent with
an outcome marker — `⋯` running, `✓` clean exit, `!` non-zero, `⊘` tombstone. A
child's first title is the opening words of its task, which the title scanner
replaces as soon as the agent names itself.

Children are not tiled. Expansion is client-local view state on `App`, not daemon
state — a laptop and a desktop can look at one fleet with different rows open — so
the filtering belongs to the client too: `dispatch-core` grows `children_of` and
keeps `visible_panes` meaning "every pane of the selected project", and `App`
subtracts the children it has not been asked to open before handing the list to
`tile()`. Ten subagents therefore do not shrink the grid to nothing, and nothing
about which rows are open reaches the daemon. Enter on a child row
opens it and gives it focus; closing it restores the grid.

The approval prompt is a new `Overlay::Approval` beside the existing pickers, so
it takes the keyboard while open: a keystroke meant for an agent must never land
on an approval, and vice versa. It shows one request at a time, with `2 more
waiting` when the queue is deeper, and the whole task text — wrapped, scrollable
if long, never truncated. Approving something you cannot read is not approval.

Keys: `a` approve, `d` deny, `A` approve everything from this pane. `Esc` defers
rather than denying — a mistaken deny throws away work the agent has already
reasoned about — and the status line then reads `1 delegation waiting — ^a p`,
which reopens it. Nothing is ever decided by inaction.

Several attached clients all see the prompt; the first decision wins and the rest
close on `DelegateResolved`.

## Config

`dispatch-config` gains the first top-level loader: `Config::load` over
`config.toml`, an absent file meaning defaults, every field `#[serde(default)]`.
Unknown keys are kept and logged by name — a newer daemon's key must not stop an
older one starting, and a typo must not be silent.

```toml
[delegation]
max_depth = 1              # a subagent cannot delegate
max_live_per_parent = 4
request_timeout_secs = 600
```

`HarnessDef` gains `task: Option<TaskLaunch>`, following the per-OS override
mechanism `launch` already has. `{task}` is substituted as one argv element and
never interpolated into a shell string, so a task containing quotes, newlines or
`$(…)` arrives as literal text:

```toml
# claude.toml
[task]
args = ["-p", "{task}"]

# codex.toml
[task]
args = ["exec", "{task}"]
```

The built-ins for `claude` and `codex` ship with a `[task]` form. `agy` and
`opencode` ship without one: their non-interactive flags would be a guess, and a
wrong guess runs a process with flags that mean something else. The refusal path
names the harness and tells the user to add one.

Deferral has a floor. The daemon owns that deadline: at `request_timeout_secs`
it drops the pending request and answers the caller `Refused { reason }`, so a
late approval cannot spawn a subagent nobody is waiting for. The shim keeps a
deadline of its own, slightly longer, purely as a backstop for a daemon that dies
mid-request without closing its socket cleanly; reaching it exits 75. One clock is
authoritative and the other only stops a hang.

## The shim

`--harness` defaults to the parent's own harness, so the common case is
`dispatch delegate "write the tests"`. `--size` defaults to 80x24: a subagent is
not on screen when it starts, and it is resized to its rectangle the first time
the user opens it, exactly as any pane is. `DISPATCH_PANE` is required, and its
absence is reported plainly: running the command from an ordinary shell is a
mistake worth a clear message.

Streams are split by audience — the subagent's tail on stdout, every status line
on stderr:

```
$ dispatch delegate "write the tests for the http client"
[dispatch] waiting for approval (pane 7f3a…, harness claude)     # stderr
[dispatch] approved; subagent pane 9c21…                          # stderr
…the subagent's own output…                                       # stdout
[dispatch] pane 9c21… exited 0                                    # stderr
```

So `dispatch delegate "…" > result.md` captures the work and nothing else, which
is what an agent doing fan-out will write, while a human watching the parent pane
still sees progress.

Fan-out needs no feature of its own: the agent's shell already has one.

```sh
dispatch delegate "write the tests" > tests.md &
dispatch delegate "write the docs"  > docs.md  &
wait
```

Exit codes follow `sysexits(3)` so an agent can branch without parsing prose:

| Code | Meaning |
|---|---|
| 0–125 | the subagent's own exit code |
| 69 | no daemon is listening |
| 75 | timed out waiting for approval |
| 77 | denied by the user |
| 78 | refused: caps, or the harness has no `[task]` form |

These are documented in `--help`, which an agent reads far more often than a
README.

## Testing

A test harness whose task form is the shell makes the whole path testable with no
model in the loop:

```toml
command = "sh"
[task]
args = ["-c", "{task}"]
```

A subagent is then any shell command, and `dispatch delegate "echo delegated-42"`
exercises the real spawn, the real caps, the real approval and the real exit
plumbing.

- **`dispatch-config`** — defaults with no `config.toml`; unknown keys logged, not
  rejected; `{task}` substituted as one argv element, proven with a task
  containing a quote, a newline and `$(…)`.
- **`dispatch-core`** — the tombstone state machine as pure state: a parent with
  live children is marked closed and kept, the last child ending removes it, a
  child's parent link survives being re-adopted.
- **`dispatch-proto`** — round-trips for the new messages; an older peer's `Hello`
  without `role` defaulting to `Interface`.
- **`dispatch-daemon`** — refusal by depth, by live count and by missing `[task]`,
  each naming its reason; approval recording `parent` and `durable`; blanket
  approval skipping the prompt; a caller disconnecting killing a one-off child and
  sparing a durable one; `DelegateFinished` carrying the exit code and a tail from
  history; a timed-out request dropped so a late approval spawns nothing.
- **`dispatchd`** — over the real socket, an `Interface` client and a `Delegate`
  caller at once. This is where the role split is proved: the delegate caller
  receives no pane output.
- **`dispatch`** — the real binaries in a real pseudoterminal. Type
  `dispatch delegate "echo delegated-42"` into the parent pane, see the prompt,
  press `a`, watch the child row appear, and watch `delegated-42` arrive back in
  the parent pane where the shim printed it. A second test presses `d` and checks
  the denial and exit 77.

Manual, because it needs credentials and a model: one real `claude -p` subagent,
approved, read back by its parent.

### Manual check, performed once per release

With a real `claude` harness and credentials:

1. `dispatchd <project>` and `dispatch --attach`.
2. Spawn a `claude` pane. Ask it to run `dispatch delegate "summarise this repo"`.
3. The prompt appears; approve with `a`.
4. The subagent appears nested, runs, and exits; its summary arrives in the
   parent pane, and the parent agent can quote it back.

## Out of scope

- An MCP front-end onto the same messages.
- Remembered approvals on disk. Blanket approval lives and dies with the daemon.
- Depth beyond 1 as a default. The cap is configurable; the default is not moved.
- A result protocol richer than the output tail — no `$DISPATCH_RESULT` file, no
  structured summary. Add one when an agent's tail proves insufficient in use.
- Detached delegation (`--no-wait`, `dispatch wait <id>`). The shell covers
  fan-out.

## Decisions, and what they rule out

| Decision | Rejected alternative | Why |
|---|---|---|
| Shell shim | MCP server; output sentinels | Works for every harness; sentinels are forgeable by anything the agent prints |
| Nested pane, opened on demand | Tiled immediately; fully headless | A handful of subagents would make every pane unreadable; a screen nobody can open hides a running process |
| Blocking call, exit code plus tail | Handle plus `wait`; result file | One command, one result; the agent's shell already does fan-out |
| Always prompt, plus caps that refuse | Remembered approvals; a budget that auto-approves | "Explicit approval" is the promise; caps stop a runaway agent queueing hundreds of prompts |
| Per-harness `[task]` form | Typing into an interactive pane | An interactive agent never exits, and "block until exit" would never return |
| Kept after exit, marked | Closed on success | The transcript is the only record of what a subagent did |
| Durable only when approved with `A` | Always dies with its caller | Deliberate: `A` is how the user says "let this pane's work run" |
| Tombstone parent rows | Promote survivors to top level | Keeps provenance visible for a pane that outlived its parent |
