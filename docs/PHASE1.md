# Phase 1 — foundations and infrastructure

Phase 1 of the roadmap builds the engine everything else consumes: process
control, isolation, an event bus, a socket other programs can read, and a store
that remembers what each session was asked to do.

Much of it already exists. This document says which parts, what is genuinely
missing, what is being built instead of what the roadmap named, and why —
because a plan written from zero and a project at v0.4.1 disagree about the
starting position, and the disagreements are worth recording rather than
quietly resolving.

## What the roadmap asks for, against what is here

| Phase 1 component | Status before this work |
|---|---|
| PTY stream and process multiplexer | Exists. `control.rs` fronts tmux on Unix and a self-hosted pseudo-console on Windows (`host.rs`). |
| Git worktree orchestrator | Exists and is tested — create, merge, discard, detect (`git.rs`). |
| Event bus | Missing. |
| IPC socket gateway | Missing. |
| State and task lineage store | Missing. |

So Phase 1 is three new pieces on top of two that work, not five from scratch.

## Deviations, and the reason for each

Four library choices in the roadmap are not taken. Each is a deliberate
decision rather than an oversight.

No tokio. The roadmap names `tokio::sync::broadcast`. Core is synchronous and
embeddable: the terminal view and the desktop app each construct `App` and
drive it from their own loop. An async runtime inside core would make both front
ends async to reach it, and the workload does not justify it — a handful of
local subscribers, not a network service. `std::sync::mpsc::SyncSender` with
`try_send` gives the property that actually matters, which is that a subscriber
who stops reading is dropped rather than allowed to block the engine. The
gateway gets one thread per client, which is affordable at this count.

No git2-rs. The worktree layer works, is covered by tests that survive three
platforms, and spends its time waiting on git rather than on process startup.
Rewriting it to change which library issues `worktree add` produces no
observable difference and re-opens settled behaviour.

No SQLite. The roadmap names SQLite or DuckDB in `.scope/db`. Two stores are
built instead, each shaped by what it holds. Events are an append-only log of
JSON lines, which is what makes replay-from-a-point trivial and is the format
the stream already promises its consumers. Task and lineage records are a JSON
document, because they are tens to hundreds of records that are held in memory
anyway and are read as a whole every time. A database earns its place when
there is a single writing daemon and history outgrows memory; neither is true
while core is embeddable. The store's interface is narrow enough that swapping
it later touches one file.

Not a daemon, yet. The roadmap describes a headless runtime that front ends
connect to. This phase builds the socket as a publisher — anything may subscribe
to the event stream, nothing may issue commands through it. That covers what
Phases 2 and 3 need, which is to observe. A command channel, and with it the
question of who owns state, is Phase 2's to answer when the Foreman first needs
to act rather than watch.

## Two things the running system decided

Neither was in the plan. Both came from watching it work and finding it wrong.

One publisher at a time, and the socket settles it. Sequence numbers are what
make `--since` mean anything, and two processes journalling from their own
counters would produce two events numbered 12. So publishing is exclusive:
whoever binds the socket owns the journal, and a second Ironsight keeps its own
state, publishes only to itself, and says so. This is also why `assign` and
`tasks` load the store without touching the stream — a short command has no
business taking the stream from the window you have open.

The store is shared, so it is re-read. `ironsight assign` is a separate process.
The Ironsight holding the stream would otherwise stamp every event with lineage
it read at startup, and an assignment made a minute ago would never appear. The
store notices the file changing underneath it, writes its own pending work
first, and otherwise takes what is there. Two processes editing the same task in
the same second is a race this does not resolve; a lock file would, and nothing
yet justifies one.

A third thing, which was in the plan but only became concrete in use: a session
Ironsight starts has no id of its own until it writes its first transcript
record, so an assignment given at `ironsight new --task` is filed against its
pane. When the session becomes itself, its records move with it. Without that,
every assignment given at the moment of starting would be orphaned minutes
later — which is precisely when assignments are most likely to be given.

## What gets built

### The event bus — `crates/core/src/bus.rs`

Every transition Ironsight already detects becomes a record anything can consume,
so supervision never has to scrape a screen.

    Event { version, seq, at, session, agent, parent, task, kind }

`seq` is monotonic per run and is what `--since` replays from. `version` starts
at 1; fields are added, never removed or repurposed, and a breaking change bumps
it with both emitted for one release.

Kinds, and where each is detected — every one has an existing detection point
except stalls, so nothing new is being inferred:

| Kind | Detected at |
|---|---|
| `SessionStarted` | a session appearing in the scan |
| `SessionWorking` / `SessionWaiting` / `SessionEnded` | the status comparison in `watch_transitions` |
| `PermissionAsked` | `probe`, which already produces `Approval` |
| `PermissionAnswered` | `App::answer`, carrying whether a human or a policy answered |
| `ToolCalled` / `ToolFailed` | new transcript events, by kind |
| `FileChanged` | the per-file touch counters moving |
| `CommitCreated` | HEAD moving under a session's directory |
| `CostSpent` | the usage totals moving |
| `SessionStalled` | no transcript growth, no file changes, and the same error repeating |
| `ChecksPassed` / `ChecksFailed` | nothing, until Phase 2 — the vocabulary is fixed now so consumers need not change later |

The bus keeps a bounded ring per subscriber. A subscriber that stops reading
loses events and is told how many when it comes back; the engine never waits on
one.

### The journal — `crates/core/src/bus.rs`

Events are appended to `~/.local/share/ironsight/events.jsonl`, one JSON object
per line, capped and rotated so it cannot grow without limit. Replay reads from
a sequence number, which is what lets a consumer restart without a gap.

### The gateway — `crates/core/src/gateway.rs`

A Unix domain socket at `~/.local/share/ironsight/events.sock`. Every connected
client is written the same line the journal receives. Clients are dropped on
write failure and never block the engine. Windows has no equivalent in the
standard library, so there the stream is available in-process and through
`ironsight events`, and the socket is absent rather than faked.

### The work store — `crates/core/src/work.rs`

What a session was asked to do, and which session asked it.

    Task { id, session, parent, assignment, constraints, success,
           escalate_if, state, checks, notes }
    State = Assigned | Working | Blocked(why) | Claimed | Verified | Abandoned

`Claimed` is deliberately distinct from `Verified`: an agent reaches the first on
its own and never the second. Phase 2 owns that transition.

Lineage is recorded when one session starts another, which turns a flat list
into a tree and lets cost roll up it. Records persist to
`~/.local/share/ironsight/work.json` and survive a restart.

### Reaching it from outside

    ironsight events                 follow the stream
    ironsight events --since <seq>   replay from a point
    ironsight events --json          one object per line, for piping
    ironsight tasks                  what exists, and in what state
    ironsight assign <session> ...   give a session an assignment
    ironsight note <id> <text>       append what was learned

## How it is proved

Unit. A fixed sequence of statuses, transcript records and approvals produces an
exact sequence of events, asserted whole. A subscriber that stops reading is
dropped without stalling the publisher, and reports its loss. The journal
replays from a sequence number across a rotation. A task tree produces the
expected parents, states and cost rollup, and survives being written and read
back.

Compatibility. `crates/core/tests/compatibility.rs` gains a case that reads the
Claude transcript fixture and asserts the events it yields, so a format change
fails here first rather than as a wrong number in the interface.

Live. A real session started under Ironsight, with `ironsight events` beside it:
`SessionStarted`, `SessionWorking`, `ToolCalled`, `CostSpent` and
`SessionWaiting` arrive in order. The subscriber is killed mid-stream and the
engine is unaffected.

### What the live checks actually found

Worth recording, because each one is a thing the unit tests could not have said.

The `agent` field carried the session's *name*. `agent_name` in a transcript is
what a person called the session, not what is running — so every event said
`agent: "Adaudit Code Review"`, and anything filtering by agent would have found
nothing. It is now read from the pane's command line, which is the only place
that knows.

Assignments made from the command line never reached the stream. See above: the
store is now re-read.

`ironsight assign` could not run while Ironsight was open. It tried to take the
socket. Publishing and state are now separate things to ask for.

A consumer starting in the same second as the publisher raced it to the socket
and failed outright. It now falls back to following the publisher that won.

## Done when

`ironsight events` prints a correct stream for a real session; a second process
reading the socket sees the same lines; the desktop window takes its status from
the stream rather than from polling; tasks and lineage survive a restart and
roll cost up the tree; and the compatibility suite covers the stream.
