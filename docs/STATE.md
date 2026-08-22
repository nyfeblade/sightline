# Where things stand

`PLATFORM.md` is the direction and `BUILD.md` is the spec. This is the shorter,
more perishable document: what is true right now, what is half-done, and what
would trip someone up if nobody said so.

Last updated 21 August 2026, at v0.4.1.

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

## Things that have bitten, so they are worth knowing

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

## Immediate loose ends

The v0.4.0 release published artifacts named `scope-*`, built before the rename,
which the installers no longer match. v0.4.1 exists to correct that; check its
assets are named `ironsight-*` before pointing anyone at the install line.

The default branch is `master`, and the installers fetch from it. If the branch
is ever renamed to `main`, both installers and the README need the same change.

## Next

In the order given in `BUILD.md`, and for the reasons given there: wire the Aider
adapter, then the event model, then lineage and task records, then verification.
Each one is useful on its own; none of them requires the layer above it to exist.
