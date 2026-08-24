# Where things stand

`PLATFORM.md` is the direction and `BUILD.md` is the spec. This is the shorter,
more perishable document: what is true right now, what is half-done, and what
would trip someone up if nobody said so.

Last updated 24 August 2026, at v0.4.1. Phase 2 is built, and so is the rest of
Layer 6: the chief, the ceilings under it, invariants that can fire, and glue.

## What works

The desktop app and the commands, over one engine, on Linux. Watching every
session on the machine; the feed, reading view, files, git tree, plan,
subagents, stats and errors; answering permission prompts from one place;
sending, queueing, broadcasting, interrupting; typing straight into a session's
own screen; starting sessions with an agent, model, effort, mode, name and first
message; worktree isolation with merge and discard; resuming any conversation on
the machine; renaming; closing; an order you choose that persists; cost and
machine usage per session.

Everything above is exercised against real sessions rather than only unit tests.

And, since Phase 1, the layer everything else is meant to consume. Every
transition Ironsight detects is published as a versioned JSON event: to a journal
that survives a restart, to a Unix socket anything on the machine can read, and
to the window, which is pushed to rather than polling. `ironsight events` follows
it — attaching to a running Ironsight when there is one, and watching the machine
itself when there is not, which is the property that makes the layer worth
having on its own.

Beside it, what each session was asked to do. Assignments and lineage persist,
a session started by another is its child, cost rolls up the tree, and a task
outlives the session it was given to. The window shows all of it: a fleet-wide
Stream view, a Work view, the assignment on each row of the session list.

`docs/PHASE1.md` is the account of it — including four things that were wrong
until a live check said so.

The window talks to a session rather than mirroring one. It used to draw the
session's terminal cell by cell, which meant resizing that session to the shape
of a panel — and a session someone is also sitting in, in their own terminal,
is then pulled between two widths and wraps every line in the wrong place in
both. The Talk view is the conversation itself: what was said as prose, what
was run as one dense line that opens, reasoning folded away, and the question
it is stuck on answered where the conversation is. `frame`, `release_frame` and
raw key forwarding went with the mirror; `Read` folded into Talk.

The substrate still offers a screen — `control::frame` is Layer 0's and is
unchanged apart from one fix: it now only reshapes a session when nobody else
is attached to it, which is the bug above at its root.

The window also reads code. Everything it shows that is code is coloured —
tool calls and their output, diffs, fenced blocks inside a reply, and whole
files — by a tokeniser written into `app.js`, because the window is served
under a policy that allows nothing from anywhere else. Clicking a file opens
it: from the Files view, from a path in the feed or the stream, or from the
Tree, where a row shows what changed in it. Output that does not fit is
expanded where it sits rather than only in a dialog.

The tokeniser has its own checks, which `cargo test` does not run:

    node crates/gui/ui/tokenize.test.mjs

The load-bearing one is the round trip. A highlighter that loses or reorders a
character is worse than none, because the code being read is then not the code
that is there.

## A second kind of session

A session Ironsight holds itself, driven over Claude Code's structured JSON with
no terminal in the way. `ironsight new <path> --owned` starts one; it takes the
same folder, model, permission mode, name, task, parent and brief as any other
session, and only the way in is different.

The thing that makes it a session rather than a second sort of object is that
Claude Code in this mode writes an *ordinary* transcript — the same
`~/.claude/projects/<slug>/<id>.jsonl` every watched session writes. So the feed,
the files, the plan, the tree, the cost, the Talk view and the stream all work
for it already: Ironsight only has to say which transcript is which session, and
that it is alive. The conversation id comes off the wire on the agent's first
line, and everything downstream keys on it.

Two things it needs that a watched session does not:

- **Liveness.** A session driven over pipes never writes a Claude Code registry
  entry, so nothing else on the machine knows it is running. Ironsight says so
  itself, in the registry's own shape (`registry::Live::owned`), which is why
  every judgement downstream — working, waiting, ended — is made by the same
  code for both kinds.
- **A way in.** There is no pane to type into. `App::deliver` routes a message
  either to a terminal or down the pipe, and every path that sends one —
  typing, queueing, broadcasting, a foreman — goes through it, so what "sent"
  means cannot differ between the two.

They are held by the daemon wherever there can be one, so they outlive every
window: that is the whole reason for owning a session rather than shelling out.
`control::owned_home` is the rule, and it is separate from where
pseudo-terminals live, because tmux cannot hold a pipe.

    ironsight new <path> --owned [--task WHAT] [--permission-mode P]
    ironsight owned                 what is held, and what each is doing
    ironsight send <who> <text>     the same command for both kinds
    ironsight stop <who>            and the same for stopping one

Proved end to end: started with a task, briefed from the project's constitution
as its opening message, answered, spoken to again from a *different* process
minutes later, both turns in one transcript, listed by `ironsight` and
talked to in the window — then stopped by name.

### What it cannot do, and why

Nobody can be asked anything mid-run. Claude Code 2.1.241 has no
`--permission-prompt-tool`, and in `--input-format stream-json` a tool the
session's settings do not allow is refused outright: the stream carries a
`system/permission_denied` and the call comes back as an error. There is no
request to answer, so there is nothing for Ironsight to route to a person.

What there is instead is honesty about it. The permission mode is chosen when
the session starts (`--permission-mode`, the same flag a terminal session takes)
and shown in `ironsight owned`, because it decides every tool call for the life
of the session. And a refusal is published as `PermissionAnswered` by a
`Policy` named after that mode — the first thing to produce that event, and the
truthful reading of it: a decision was made on your behalf, by settings, and you
were not asked. Without it a session getting nothing done looked like a session
with bad luck.

Interrupting is the other absence: there is no Escape to press and no interrupt
in the input format, so the window says so rather than sending a key
nowhere.

## Aider, read rather than watched

The Aider adapter reads `.aider.chat.history.md` — what was asked, what came
back, the model, tokens and cost — and is tested against a real Aider run. It is
now called: a pane running `aider` in a folder that has a record becomes a
session identified by that folder, which is how Aider itself resumes one, and
`Session::pump` reads markdown rather than JSON for it.

The two things markdown needs that JSON does not: an answer runs over as many
lines as it takes, so consecutive lines are joined into one thing that was said
rather than a dozen feed entries; and only the "chat started" line carries a
time, so how recently the session was active is taken from the record's own
mtime — otherwise a session working now reports its age from when it opened.

Cost is added up per exchange rather than read from the running session total
aider prints beside it, because a second run of aider in the same folder starts
that total again from zero.

## Known rough edges

Windows has never been run on Windows. It compiles, its logic is unit-tested,
and the pty backend's own test runs on Unix — but nobody has started a session
there.

The desktop app has never been run on macOS. It compiles and can be bundled.

Memory per session is proportional set size on Linux and the agent's own
resident size elsewhere, because only Linux keeps the shared figure. The second
number undercounts; it does not overcount, which is the failure that matters.

Sessions created before the rename are named `scope-N`. They are still
recognised, and new ones are `ironsight-N`. Sessions Ironsight holds by pipe are
`owned-N` — a name space of their own on purpose, so that typing the name of a
terminal session cannot reach one of these.

A session is matched to the pane it runs in partly by looking for its id in the
pane's command line, which is how a conversation adopted seconds ago is found
before it has registered itself. A path that happens to contain a session id
therefore matches too. Nothing real does that — it takes a 36-character uuid in
a command line — but a test that puts a fixture under a directory named after
the running session will watch it claim the wrong pane, which cost an hour once.

Cross-checking the app crate for Windows from Linux needs `llvm-rc`. Everything
else checks from here:

    cargo check --target x86_64-pc-windows-msvc -p ironsight-core -p ironsight

The event socket is Unix-only — the standard library has no Windows equivalent —
so there the stream is reached in-process and through `ironsight events`, and
`gateway::serve` says so rather than pretending. The window's UI is compiled
into the binary, so editing anything under `crates/gui/ui/` needs a rebuild
before it has any effect; an hour can go into wondering why a change did not
take.

## Intent: the constitution and the brief

A project writes its standing decisions once, in `.ironsight/constitution.md` —
mission, architecture, constraints, preferences, rejected approaches, what done
means, open questions — and a decision recorded there outlives the session that
made it. `ironsight brief <who>` renders a worker's brief from it: the standing
constraints that bear on the task, its success criteria, and its escalation
conditions, and nothing else, because a brief is not a transcript. A `[tag]`
prefix scopes a constraint to tasks that mention it, so a database worker is not
handed the front-end rules. Starting a session with `--task` briefs it this way
as its opening message. Nothing here asks a model anything — intent paraphrased
on the way in is intent you can no longer trust.

Both halves are in the window now. A session with a task offers its brief, and
it is rendered when asked rather than stored — the constitution as it stands
plus the task as it stands, which answers "what would this session be told
today", the question worth asking when its work has drifted. Any session offers
its project's constitution, read and edited in place; a project without one is
handed the empty document, with the headings the parser actually looks for, so
what someone writes reaches a brief instead of sitting in a file nothing reads.
It is the only thing in the window that writes into your repository, and it
writes exactly the path it showed you.

## Supervision, and the ceilings under it

A chief is a session with Ironsight on its path, a brief, and a ceiling it cannot
raise. `ironsight chief <path> <what you want done>` starts one. It is not a new
runtime — that is the point, and the recursion falls out: a chief is a session
Ironsight manages, managing sessions Ironsight manages.

Its brief carries the intent unparaphrased, the project's constitution, the fleet
as it stands, and three prohibitions. It does not answer permission prompts,
because the moment a supervisor answers them its blast radius is everything a
permission protects. It does not restart a stalled session, because from outside
thinking and wedged are identical and a restart throws away work and pays for it
twice. And it does not write code — that one is enforced rather than asked for,
since an owned chief starts with the editing tools denied.

`limits.rs` is the part that does not depend on the chief reading carefully. A
count of sessions and an amount of spend, checked at both doors a session can
come through, refusing with the reason. The real file lives in Ironsight's data
directory, outside every worktree, because a ceiling a supervised agent can edit
is a suggestion in a file it has write access to; a project's
`.ironsight/limits.toml` may lower it and never raise it, and `effective` is a
pure function with a test that a greedy repository gets what the machine allows.

The count is of sessions *Ironsight started*, not of every session on the
machine. It counted everything at first, which meant a dozen of your own open
sessions ate the whole allowance and no worker could start — and since a chief
refuses to run without a ceiling, that made the chief unusable on exactly the
machines busy enough to want one. A supervisor cannot start a session by any
route other than Ironsight, so this still bounds everything it can do.
`ironsight limits` shows where you stand, because picking a number without
knowing the current one is guessing.

Spend is counted from the event journal rather than from the sessions currently
open, since spend you can reset by closing a window is not a ceiling. That has a
real limit and the command says so: the journal is written while an Ironsight is
running, so a spend ceiling on a machine that only ever runs the commands is
measuring nothing yet.

Nothing is on by default. A ceiling nobody asked for that refuses a ninth session
is a surprise, and surprises are how a tool gets turned off. What is not optional
is supervision.

A live chief was run against a real project with a real failing check. It read
the fleet, the constitution and the checks, diagnosed the bug, wrote an
assignment more precise than the one it was given — including an edge case the
tests did not cover — started a worker on a worktree, polled its state, and ran
`ironsight check` and `ironsight trust` against it. A session rate limit stopped
it, not a design fault. Two things were learned only by running it: a headless
supervisor needs an explicit grant for the commands its job is made of, because
`--allowedTools` grants rather than restricts and nothing can be asked mid-run;
and a daemon built before `owned::Spec` had an `allow` field silently dropped it,
which is why `Spec` is now `deny_unknown_fields` and any change to it bumps the
wire version.

## The Hub has two faces

Watching a fleet and directing one are different jobs with different questions,
and mixing them into one screen made the second invisible. Every layer built on
top of the fleet — a chief, ceilings, what a project says done means, what must
never stop being true — arrived as a terminal command, in a program whose whole
purpose is that you should not need one.

The Work tab in the window is the other face. The session list stays either
way, because what is running is still what you are directing. What changes is the question the rest of the screen answers:

    THIS PROJECT   checks, whether they are approved, invariants, a constitution
                   — that is, whether anything here can tell a worker it is wrong
    CEILINGS       what a fleet here may do, and how much is in use
    WORK           every assignment, and how far it got

with `c` to hand work to a chief, `s` to set the project up, `l` for ceilings
and `v` to run the invariants. All four exist as commands too; the commands now
call the same engine functions rather than holding the logic themselves, because
neither front end may grow what the other needs.

Setting a project up is the one that mattered most. Four files and a trust
ceremony before anything happens is a good reason not to start, and it was the
reason none of this was being used. `set_up_project` reads what is lying in the
folder, guesses the build system, writes a first draft of the checks and a
constitution with the headings the parser looks for, and says it is a guess. It
never overwrites, and it approves only what it wrote: the trust gate exists
because a checks file arrives with a clone, and one written at your asking a
second ago is not that.

## Invariants: guarantees that can fire

`.ironsight/checks.toml` takes `[[invariant]]` beside `[[check]]`, and they point
in opposite directions. A check must pass, and a passing check says only that the
failures it can express did not happen. An invariant is stated as the thing that
must not be found, so its command must *fail* — one that succeeds has
demonstrated the very defect it was written to look for.

That direction is what makes them survive a merge. "The tests pass" survives an
adapter that quietly broke something load-bearing; a command looking for the
breakage does not. `ironsight invariants` runs them and a quiet run is the good
one. A broken invariant refuses work: `ironsight check` runs them before the
suite and sends the task back to Working, because they answer a different
question — not "is this finished" but "did it break something that was never its
business".

Nine are written for this repository. Each was proved the way the fire-once rule
demands, by breaking the thing on purpose in a scratch tree and watching the
command catch it — which mattered, because three of them fired against an intact
repository on the first draft. All three were bad shell rather than broken
guarantees, and they were only found by running them.

`Unrunnable` is neither held nor broken. An invariant nobody can test vouches for
nothing, which is the same mistake the fire-once rule exists to prevent one level
down. The trust gate counts them: they are shell from the same file arriving with
the same someone else's code, and approving nine commands without naming them
would be the gate failing at its only job.

## Glue: reconciling a fork by teaching its agent

People fork this, customise it, and every release pulls them further out of step.
`git merge` matches line numbers, knows nothing about what a module is for, and
hands back conflict markers — so nobody reconciles and the fork stops updating.

The observation glue is built on is that whoever forked it already has an agent,
and that agent already knows their fork. What it does not know is upstream: the
layers, the seams a customisation is meant to live in, the invariants that must
survive, and how upstream tests. That is the same for every fork, so it is
written once and shipped in the binary as an ability.

    ironsight glue --install     teach this fork's agent, and stop there
    ironsight glue <version>     compute the divergence, cut a worktree, and
                                 brief an owned session to do the work

`--install` matters on its own: after it, the fork's own agent can be asked to
reconcile without going through Ironsight at all, which is the point of shipping
an ability rather than a tool. Maintenance then scales with forks rather than
with the author's time.

It does not get to decide the merge worked. The bar is the same as everywhere
else here — the checks pass and the refutations do not fire — and a fork with no
checks file is told plainly that its result can only ever be unverified. It runs
inside somebody else's repository, so every step that can fail says what it
wanted: not a repo, no remotes, an unknown version, upstream unchanged, the fork
unchanged, uncommitted work in the tree.

## Taking rows off the list

A machine that has run agents for a week has a list mostly made of sessions that
ended days ago. `x` ends a process; `-` ends a row. `=` takes every finished row
off, `+` puts everything back, and `ironsight hidden --ended` does it from a
shell. Both front ends have it.

Hidden, never deleted: the transcript stays where Claude Code wrote it, `R` still
finds the conversation and `A` still reopens it.

Any row comes off, running or not, whoever started it. The first version refused
a running one and said to close it first with `x` — but `x` needs a session
Ironsight can steer, so for anything it merely watches that was a dead end with
no way out. A live one says so as it goes.

The filter runs on both the fast tick and the slow one. It ran only in `discover`
at first, four times a second slower than the pass in `refresh` that re-adds
owned sessions, so a removed row came back within 250ms and looked exactly like a
key that did nothing.

## Hardening, and what each fix actually guarantees

A pass over the ways the system could lose or leak data, each with a test that
would have failed before it:

- **Secrets do not reach disk or the socket.** Command lines are redacted on the
  way out — minted key shapes, `NAME=value` where the name says secret, the word
  after `--token` or `Bearer`, long opaque strings. Not on the way in: the
  window still shows the real command, because it is your machine. The redactor
  errs towards hiding and is tested both ways, including that `-p` is not treated
  as `--password` (which would have masked half the commands here).

- **Nothing runs from a repository until it is read.** `.ironsight/checks.toml`
  is shell that arrives with someone else's code; `ironsight check` refuses it
  until those exact commands are approved with `ironsight trust`, and asks again
  if the file changes.

- **A refutation counts only once it has fired.** One that cannot fire stood for
  ever and verified anything; now it is evidence only after it has been seen to
  catch something.

- **One writer, on every platform.** A pid-file lock decides who journals, so
  two processes cannot both number events from separate counters where there is
  no socket to stop them. A lock left by a dead process is stolen; one held by a
  live process is honoured.

- **A rotated-away gap is reported, not hidden.** A consumer asking `--since N`
  for events that have rolled off the journal is told how many it missed rather
  than handed a short read it would mistake for having caught up.

- **Spend is not lost when a transcript is re-read lower.** Counters going
  backwards re-baseline instead of swallowing every later increment until they
  pass the old high-water mark.

- **A journal that cannot write says so.** A full disk is a counted loss that is
  surfaced once, not events that quietly never happened.

- **A stale pane record cannot be misadopted.** An assignment filed under a
  pane id while a session is being born is dropped if the session never arrives,
  so a reused pane id does not inherit someone else's task.

## Things that have bitten, so they are worth knowing

A test that assumed how many writes it takes to notice a socket has closed. A
consumer of the event socket is forgotten on the next write that *fails*, and
how many writes that takes depends on when the kernel tears the socket down —
so `a_consumer_that_leaves_costs_the_others_nothing` passed on an idle machine
and failed roughly one run in fifteen on a busy one. It now publishes until the
gateway notices, which asserts the property rather than the timing. The same
mistake in a different costume as the three below.

Tests that assert something about one machine. Three rounds of red CI came from
a fixture repository with no git identity, macOS shipping a bash whose `printf`
does not know `\u`, and paths compared as strings when git answers with the real
one. Every test that touches the world now says what it means rather than what
it looked like here.

Moving the checkout. Absolute paths are baked into build artifacts at compile
time — Tauri caches them and `CARGO_MANIFEST_DIR` is compiled in — so a moved
tree needs `target/` cleared or tests fail looking for fixtures in a directory
that no longer exists.

A single missing element in the window used to throw at load and leave a blank
app with no explanation. Errors now land in the status line.

## Where sessions live

Three answers now, chosen at startup rather than at compile time, and asked for
with `IRONSIGHT_BACKEND`:

    tmux        tmux holds them. Outlives Ironsight; needs tmux installed.
    daemon      a process of Ironsight's own holds them. Outlives every window,
                and needs nothing installed.
    hosted      this process holds them. They end when it does.

tmux is still the default wherever it exists, and deliberately so: any sessions
already running are in it, and switching underneath them would empty the list.
Without tmux, the daemon is chosen. The rule is a pure function with tests
(`control::chosen_from`) rather than something that reads the environment while
it decides.

The daemon owns pseudo-terminals and nothing else. Everything about what a
session *means* — status, cost, transcripts, tasks — stays in the front ends,
reading the same files they always read. A daemon that starts making judgements
is a daemon that has to be restarted to change one. It answers one JSON object
per line on `~/.local/share/ironsight/control.sock`, and it is started for you
the first time something needs it, in a session of its own so that closing the
terminal that happened to start it cannot hang it up.

    ironsight serve          run it yourself, to watch it
    ironsight attach <who>   hand this terminal to a session it holds

Owned sessions are held by the daemon too, and by a rule of their own
(`control::owned_home`): the daemon wherever there can be one, this process
where there cannot — Windows, or a run that asked for `hosted`. Where
pseudo-terminals live says nothing about where a pipe lives, and tmux cannot
hold one.

`attach` exists because tmux gave that for free, and losing it would mean no way
back in when the window is the problem. It polls rather than streams: the daemon
answers questions and does not push, which costs a frame of latency and buys a
protocol nobody has to debug while a fleet is wedged.

Proved end to end: a session started through the daemon, typed into over the
socket by a Python client that knows nothing about Ironsight, answering
correctly, still running after the process that created it had exited — and the
daemon confirmed to be a session leader with no controlling terminal.

## What the stream does not carry

Command lines are a good place to find a token, and the event journal is a file
that stays on disk and a socket served to anything running as you. So what
leaves is redacted: minted key shapes, `NAME=value` where the name says secret,
the word after `--token` or `Bearer`, and long opaque strings that are not
paths or object names.

Only what leaves. The interface still shows the command as it was — it is your
machine and you can already see it. Reading a screen is not the same as writing
to a file that outlives the session.

It errs towards redacting, and its tests run both ways: seven credential shapes
that must not survive, and ordinary commands — `cargo check --target
x86_64-pc-windows-msvc -p ironsight-core`, a forty-character git object name, a
long scratch path — that must come through untouched. The first version of that
test caught `-p` being treated as `--password`, which would have redacted half
the commands in this repository.

## What Phase 1 deliberately did not do

The roadmap named tokio, git2-rs and SQLite. None is used, and `PHASE1.md` gives
the reason for each. The short version: core is synchronous and embeddable and
an async runtime would make both front ends async to reach it; the worktree
layer already works and is tested; and the two stores are shaped by what they
hold — an append-only log for events, a JSON document for tasks — with a
database earning its place when there is a single writing daemon and history
outgrows memory.

Nor is there a daemon. The socket publishes and takes no instructions. That is
enough for a foreman to watch, and the question of who owns state is Phase 2's
to answer when one first needs to act.

## Immediate loose ends

The v0.4.0 release published artifacts named `scope-*`, built before the rename,
which the installers no longer match. v0.4.1 exists to correct that; check its
assets are named `ironsight-*` before pointing anyone at the install line.

The default branch is `master`, and the installers fetch from it. If the branch
is ever renamed to `main`, both installers and the README need the same change.

## Next

Phase 2: verification, and the foreman that uses it. An agent reporting
completion is worth very little, and until `Claimed` can be told from `Verified`
by something other than the agent's own word, nothing built above this helps.
The vocabulary is already reserved — `ChecksPassed` and `ChecksFailed` are in
the stream's version 1 and nothing emits them yet — so a consumer written today
does not change when they start arriving.

Layer 6 is built, both halves. The foreman refuses claimed work that does not
pass; the chief turns a paragraph of intent into assignments and cannot exceed
what it was given. Owned sessions are the substrate underneath both, and glue is
what the whole apparatus turns out to be good for once it exists.

What is left is not more of the same shape.

The experiment is the honest next thing, and it is a bet rather than code: does
supervised orchestration beat one person driving the same agents by hand? Owned
sessions and a chief exist now, so the comparison is finally possible.
`BUILD.md` has the protocol.

Two things want living with rather than building. The daemon has held sessions
through crashes and 400 concurrent requests, but not a week of real use. And a
chief has run once, well, until a rate limit stopped it — once is not evidence
about anything except that it starts.

The glue idea has one weak seam left, and it is worth naming: "auto-merge if
confident" is not this codebase's bar and never will be. It is auto-merge if
*verified*. Where a fork has no refutations, the strongest thing that can be said
is checked, and glue should keep saying so rather than growing a confidence
threshold. The Phase 2 sketch — agents that find and fix bugs in the background
and merge them without telling anyone — has the same problem in a worse place:
undisclosed local fixes generate exactly the divergence glue exists to remove.
The valuable version proposes and never merges.
