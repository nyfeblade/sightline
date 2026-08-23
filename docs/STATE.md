# Where things stand

`PLATFORM.md` is the direction and `BUILD.md` is the spec. This is the shorter,
more perishable document: what is true right now, what is half-done, and what
would trip someone up if nobody said so.

Last updated 22 August 2026, at v0.4.1, after Phase 1.

## What works

The terminal view and the desktop app, over one engine, on Linux. Watching every
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

## What is built but not yet wired

The Aider adapter reads `.aider.chat.history.md` — what was asked, what came
back, the model, tokens and cost — and every part of it is tested against a real
Aider run. Nothing calls `conversations()` yet, and the session reader has no
branch for a markdown record, so an Aider session currently shows as a screen
and nothing more. Wiring it is the next obvious piece of work and the first
demonstration that the adapter layer means something.

## Known rough edges

Windows has never been run on Windows. It compiles, its logic is unit-tested,
and the pty backend's own test runs on Unix — but nobody has started a session
there.

The desktop app has never been run on macOS. It compiles and can be bundled.

Memory per session is proportional set size on Linux and the agent's own
resident size elsewhere, because only Linux keeps the shared figure. The second
number undercounts; it does not overcount, which is the failure that matters.

Sessions created before the rename are named `scope-N`. They are still
recognised, and new ones are `ironsight-N`.

Cross-checking the app crate for Windows from Linux needs `llvm-rc`. Everything
else checks from here:

    cargo check --target x86_64-pc-windows-msvc -p ironsight-core -p ironsight

The event socket is Unix-only — the standard library has no Windows equivalent —
so there the stream is reached in-process and through `ironsight events`, and
`gateway::serve` says so rather than pretending. The window's UI is compiled
into the binary, so editing anything under `crates/gui/ui/` needs a rebuild
before it has any effect; an hour can go into wondering why a change did not
take.

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

Still outstanding from before, and still small: wiring the Aider adapter.
