# How each layer gets built, and how it gets proved

`PLATFORM.md` says what the layers are and why they are in that order. This is
the working document: what each one actually is, where it lives, what it does,
and what would have to be true for it to count as finished.

## How work is done here

Five rules, all of them learned the hard way in this codebase.

Prove it against something real. Unit tests are necessary and never sufficient.
Every layer here has a live check — a session actually started, a key actually
typed, a screen actually read back. Three rounds of continuous integration
failures on other platforms came from tests that asserted something about one
laptop rather than about the code.

Pin other people's formats. Anything read from a file Ironsight does not write gets
a fixture in `crates/core/tests/fixtures/` and an assertion that names what
moved. See `crates/core/tests/compatibility.rs`.

Keep the failure honest. When something cannot be determined, say so rather than
guessing: no reading rather than a made-up number, "cannot tell" rather than a
confident wrong status. Nothing is closed, killed or answered on a guess.

One implementation, two callers. The terminal view and the desktop app are front
ends over `ironsight-core`. Any behaviour that both need lives in core; anything a
front end knows that the other does not is a bug waiting to be reported twice.

Every layer standalone. Dependencies point downward only. If a layer cannot be
described as a product with nothing above it, it is not a layer.

## Layer 2 — the event model

### What it does

Turns state changes Ironsight already detects into a stream anything can consume,
so supervision never has to scrape a screen.

### Where it lives

    crates/core/src/event_stream.rs      the types, the bus, the subscribers
    crates/core/src/app.rs               emission points (already the detector)
    crates/tui/src/main.rs               `ironsight events` subcommand
    crates/gui/src/main.rs               a Tauri channel for the window

### The shape

```rust
pub struct Event {
    /// bumped only for a breaking change; consumers check it
    pub version: u32,
    pub at: DateTime<Utc>,
    pub session: String,
    pub agent: String,
    /// the session that started this one, when Ironsight started it
    pub parent: Option<String>,
    /// the assignment that session was given, when it has one
    pub task: Option<String>,
    pub kind: Kind,
}

pub enum Kind {
    SessionStarted { cwd: String, branch: String },
    SessionWorking { tool: Option<String> },
    SessionWaiting,
    PermissionAsked { question: String, options: Vec<String> },
    PermissionAnswered { option: String, by: By },
    ToolCalled { tool: String, summary: String },
    ToolFailed { tool: String, summary: String },
    FileChanged { path: String, added: usize, removed: usize },
    CommitCreated { sha: String, message: String, branch: String },
    ChecksPassed { suite: String, ms: u64 },
    ChecksFailed { suite: String, first: String },
    SessionStalled { quiet_for: u64, no_files_for: u64, repeated: Option<String> },
    SessionEnded { reason: Ended },
    CostSpent { output: u64, estimate: f64 },
}

pub enum By { Human, Policy(String) }
```

### How it is emitted

Nothing new is detected. Every event has an existing detection point:

- `refresh()` compares each session's status against the previous tick — that
  comparison already exists for notifications and becomes `SessionWorking`,
  `SessionWaiting`, `SessionEnded`
- `probe()` reads panes and already produces `Approval` — becomes
  `PermissionAsked`; `App::answer` becomes `PermissionAnswered`
- the transcript reader already produces tool calls, results, file touches and
  usage — becomes `ToolCalled`, `ToolFailed`, `FileChanged`, `CostSpent`
- `git::status` already reports commits and tree state — becomes
  `CommitCreated`
- stalls are new but mechanical: no transcript growth, no file changes, and the
  same error text repeating

### How it is consumed

Three transports, one format — JSON, one event per line:

    ironsight events                     follow the stream in a terminal
    ironsight events --since <id>        replay from a point, for a consumer restart
    ~/.local/share/ironsight/events.sock   a socket for anything else
    Tauri channel                    the window, which stops polling for status

Consumers are never assumed. The bus drops events for a subscriber that has
stopped reading rather than blocking the engine.

### Versioning

`version` starts at 1. Fields are added, never removed or repurposed. A kind is
added, never renamed. A breaking change bumps the number and both are emitted
for one release. This is the promise that makes it safe to build on.

### How it is tested

Unit: a fixed sequence of transcript records and pane captures produces an exact
sequence of events, asserted whole rather than by sampling.

Fixture: `compatibility.rs` gains a case that reads the Claude transcript
fixture and asserts the events it yields, so a format change fails here first.

Live: start a real session under Ironsight, run `ironsight events` beside it, send a
message, watch `SessionWorking`, `ToolCalled`, `ChecksFailed` and `SessionWaiting`
arrive in order; kill the subscriber mid-stream and confirm the engine is
unaffected.

### Done when

`ironsight events` prints a correct stream for a real session, the window uses it
instead of polling for status, and the compatibility suite covers it.

## Layer 3 — lineage and task records

### What it does

Records that one session started another and what it was asked to do, so a flat
list becomes a tree with work attached to it.

### Where it lives

    crates/core/src/work.rs                       the types and the store
    ~/.local/share/ironsight/work.json           the store on disk
    crates/core/src/app.rs                        set on start, read on refresh

### The shape

```rust
pub struct Task {
    pub id: String,
    pub session: String,
    pub parent: Option<String>,      // the session that assigned it
    pub assignment: String,          // what was asked, in words
    pub constraints: Vec<String>,
    pub success: Vec<String>,        // what must be true to be finished
    pub escalate_if: Vec<String>,
    pub state: State,
    pub checks: Vec<String>,         // named suites that must pass
    pub notes: Vec<Note>,            // what has been learned, appended
}

pub enum State { Assigned, Working, Blocked(String), Claimed, Verified, Abandoned }
```

`Claimed` is deliberately distinct from `Verified`: an agent may reach the first
on its own and never the second.

### How it is wired

`App::start_session` records lineage when a session starts another; a session
started by a person has no parent. The task is written at assignment time, moves
to `Working` on the first `SessionWorking` event, to `Claimed` when the agent
says so, and to `Verified` only by layer 4.

Front ends: the session list indents children under their parent and shows the
assignment beneath the name; cost rolls up the tree.

    ironsight tasks                  what exists, and in what state
    ironsight assign <session> ...   give a session an assignment
    Ironsight task <id> --note ...   append what was learned

### How it is tested

Unit: a tree built from assignments produces the expected parents, states and
cost rollup; an abandoned session leaves its task readable.

Live: start a session, assign it, start a second from within it, and confirm the
tree appears in both front ends with cost attributed upward.

### Done when

A session started by another shows as its child in both front ends, tasks
survive a restart, and cost rolls up.

## Layer 4 — verification

### What it does

Refuses to accept "done" without evidence. The single highest-value layer.

### Where it lives

    crates/core/src/checks.rs                     the runner
    <project>/.ironsight/checks.toml                  what the checks are, per project

### The shape

```toml
# .ironsight/checks.toml — committed with the project, so it is the project's
# definition of done rather than Ironsight's
[[check]]
name    = "build"
run     = "cargo build --release"
timeout = "5m"

[[check]]
name    = "tests"
run     = "cargo test"
timeout = "10m"

[[check]]
name     = "ci"
run      = "gh run list --branch $BRANCH --limit 1 --json conclusion"
expect   = "success"
optional = true      # missing tooling reports unknown, never failure
```

### How it behaves

Checks run in the session's own worktree, so a failing check never blocks
another session. They run on demand (`ironsight check <session>`), when a task is
`Claimed`, and on a commit event. Results become `ChecksPassed` or
`ChecksFailed`, and a claimed task with a failing check returns to `Working`
with the first failure appended as a note — which is the message the agent
receives.

What it will not do: judge quality, review style, or ask a model whether the
work is good. Mechanical signals only, so that a pass means something.

### How it is tested

Unit: a check configuration parses; a failing command produces `ChecksFailed`
carrying the first failure line; a missing optional tool reports unknown rather
than failure; a timeout is a failure with a reason.

Live: a fixture repository with one passing and one failing suite. Assign a
task, mark it claimed, confirm it is refused with the failure attached, fix the
code, confirm it verifies.

### Done when

A task cannot reach `Verified` while any required check fails, in both front
ends and from the command line, with the failure visible to the agent that
claimed it.

## Layer 5 — intent

### What it does

Keeps decisions and preferences somewhere durable, and gives each assignment
only the context it needs.

### Where it lives

    <project>/.ironsight/constitution.md      the project's standing decisions
    crates/core/src/brief.rs              rendering a packet from it

### The constitution

Plain markdown, written by a person, amended when decisions are made. Sections
are fixed so they can be read programmatically: mission, architecture,
constraints, preferences, rejected approaches with reasons, definition of done,
and open questions.

It is the answer to "why does this project do it that way" surviving longer than
the session that decided it.

### The packet

```
ironsight brief <session> --task "implement the OAuth callback"
```

Renders the assignment, the constraints that apply, the success criteria, and
the escalation conditions — and nothing else. The point is that a worker is
briefed rather than handed a transcript: full context is expensive and mostly
irrelevant to one task.

### How it is tested

Unit: a constitution parses into sections; a packet contains the constraints
that match the task and omits the rest; an amended decision appears in the next
packet.

Live: brief two sessions in the same project on different tasks and confirm each
one's opening message contains its own constraints and neither contains the
other's.

### Done when

Starting a session with `--task` briefs it from the constitution, and a decision
recorded once is visible to every session started afterwards.

## Layer 6 — supervision and planning

### What they are

Not new runtimes. A foreman and a chief are sessions with `ironsight` on their path,
a brief, and permission to run a specific set of commands.

    Foreman: reads `ironsight events`, runs `ironsight check`, appends notes, returns
             claimed work that fails, escalates stalls. Never writes code.
    Chief:   reads the constitution and `ironsight tasks`, writes assignments, starts
             workers with `ironsight new --task`, reports to the human, and asks
             when a decision exceeds its threshold.

### Safety, which is not optional

- A supervised worker runs in its own worktree. Containment is the price of
  autonomy and already exists.
- Permission answering stays human by default. A policy that answers on your
  behalf is opt-in, per project, per prompt shape, and every answer is an event
  marked `By::Policy`.
- Stalls escalate, never restart. From outside, thinking and wedged look the
  same.
- Spend and agent-count ceilings are enforced by Ironsight, not by the supervisor's
  good intentions: a start that would exceed them fails with the reason.

### How it is tested

Unit: a policy that would answer a prompt outside its allowed shape is refused;
a start that would exceed a ceiling fails with the ceiling named.

Simulated: a fleet of stand-in agents — scripts that claim completion without
doing the work, stall, or fail their checks — driven through a full cycle, so
the supervisor's behaviour is tested without paying for inference. This is how
the failure paths get covered.

Live: one chief, three workers, one real project, real checks.

### Done when

A chief can take a paragraph of intent, produce assignments, start workers, have
their work refused when checks fail, and report back — with every action it took
readable as events afterwards.

## The experiment

Layer 6 exists to answer one question:

> Does a supervised organisation produce better software, for less human
> attention, than the same person directing the same agents by hand?

### Protocol

Take one real project. Choose two comparable bodies of work — similar size,
similar risk, ideally two halves of the same feature set. Run one by hand across
three sessions, the way you work now. Run the other through a chief with three
workers and verification on. Alternate which comes first across trials, because
whichever is second benefits from what was learned in the first.

### What to measure

Human attention: minutes spent typing, deciding, or re-explaining. This is the
number that matters most, and it is measurable from the event stream —
`PermissionAnswered { by: Human }` and messages sent per unit of work.

Rework: tasks that reached `Claimed` and were refused; contradictory
implementations found later; decisions re-litigated after being recorded.

Throughput: work verified per hour of wall clock.

Cost: tokens and estimate, from data Ironsight already has, per unit of verified
work rather than per session.

### What counts as a result

For: fewer human interruptions per unit of verified work, less rework, and
decisions that stay decided.

Against: more notifications rather than fewer, intent arriving at the worker in
a form you would not recognise, or cost per verified unit rising faster than
throughput.

Judge on the third day, not the first demo. The first demo of an agent
hierarchy always looks good.

### If the answer is no

Layers 0 through 5 stay. Events, lineage, verification and briefing improve
Ironsight for one person running agents by hand, which is what it is for today. That
is the reason for this order: the experiment is cheap because everything under
it was worth building anyway.
