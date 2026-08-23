# Passoff — resume the Ironsight build

Written 2026-08-23. Read this, then `docs/STATE.md` (what is true now),
`docs/PLATFORM.md` (the layers and why), and `docs/BUILD.md` (how each layer is
built and proved). This file is the shorter, more perishable "where the work is
and what to do next".

## One line

The buildable roadmap is complete through Layer 5. Layers 0–5 are built, the
foreman half of Layer 6 is built, plus two things off the original roadmap (a
self-contained daemon backend and owned sessions over stream-json). Everything
is green; the next real work is making owned sessions first-class.

## Where the code is

The full, current tip is the local branch **`solidify`** (everything stacks up
to it). `master`/`main` are the published `v0.4.1` base (`aa01afa`) and are
untouched. The stack, bottom to top:

    master (v0.4.1)
      └─ review/1-platform     event model, verification, daemon, hardening
          └─ review/2-gui      Talk view, redesign, icon
              └─ review/3-stream-json  owned sessions   (== ironsight-integration)
                  └─ layer-5-intent    constitution + brief, tasks --json
                      └─ solidify      engine panic-resilience   ← full tip

Three PRs are open on GitHub (`nyfeblade/ironsight`), each reviewing exactly one
slice (base is the slice below):

- PR #1 `review/1-platform → master`
- PR #2 `review/2-gui → review/1-platform`
- PR #3 `review/3-stream-json → review/2-gui`

All three had a cloud ultrareview; **20 findings across them, all real, all
fixed with tests** (the fix commits are the branch tips). `ironsight-integration`
is stale — it sits at the PR #3 tip and does NOT include Layer 5 / tasks --json /
solidify. `solidify` is the one to build from and to base any new branch on.

Layer 5 and later are not yet in a review PR. Consider opening a PR #4
(`layer-5-intent → review/3-stream-json`) and PR #5 (`solidify → layer-5-intent`)
if you want them to get the same adversarial pass.

## What is built (layers)

- **0 substrate, 1 compatibility, 2 events, 3 lineage/tasks** — built earlier.
- **4 verification** — checks + fire-once refutations + `ironsight foreman`.
  Checks only ever *refuse*; a task reaches Verified only when a refutation
  written to show the work wrong was run, did not fire, and has been seen to
  fire at least once. Nothing runs from a repo's `.ironsight/checks.toml` until
  `ironsight trust` approves those exact commands.
- **5 intent** — `.ironsight/checks.toml`'s sibling `.ironsight/constitution.md`,
  parsed into fixed sections; `ironsight brief <who>` renders a task-focused
  packet (constraints that bear on the task via `[tag]` scoping, success,
  escalation). `ironsight new --task` briefs the session as its opening message.
- **6 supervision** — foreman built; **chief not built** (it is a methodology/
  skill — "a session with ironsight on its path and a brief" — not new runtime;
  the CLI it needs is done).
- **Off-roadmap:** a daemon (`ironsight serve`, control socket, chosen via
  `IRONSIGHT_BACKEND=daemon`) so sessions outlive the window without tmux; and
  owned sessions (`ironsight run`) that drive Claude Code over
  `--input-format stream-json` with no terminal.

176 test functions; `cargo test`, `node crates/gui/ui/tokenize.test.mjs`,
`cargo fmt --check`, and `cargo check --target x86_64-pc-windows-msvc -p
ironsight-core -p ironsight` are all clean on the tip.

## What to build next (recommended order)

1. **Owned sessions as first-class.** They exist only in the one-shot `ironsight
   run` today (`crates/core/src/owned.rs`, `OwnedSession`). Make them a real
   session type: started, persistent, in the session list, watchable and
   talkable in the GUI Talk view. This is what makes the daemon + stream-json
   investment pay off, and it is the substrate a chief drives.
2. **Interactive permissions for owned sessions.** The documented gap — route
   each permission through Claude Code's `--permission-prompt-tool` so an owned
   session can be answered from one place instead of running under fixed
   settings. Pairs with #1.
3. **Wire the Aider adapter.** Oldest loose end: `agent/aider.rs::conversations()`
   is built and tested but nothing calls it, so an Aider session shows as a bare
   screen. Small; proves the adapter layer means something.
4. **Surface the brief/constitution in the GUI** — a session's brief in its
   panel; the constitution read/edited in the window.

Then the research tier (not "build"): the chief, organisations (Layer 7), and
the experiment — does supervised orchestration beat one person driving the same
agents by hand (Layer 7/8 of `PLATFORM.md`). That is a bet to run, not code.

## The honest gaps (cannot be closed from this Linux box)

- Windows has never been *run* on Windows; the macOS app has never been *run* on
  macOS. Both compile, cross-check clean, and are unit-tested — not the same as
  working there. Needs a real Windows box and a Mac.
- The daemon survived crashes and 400 concurrent requests, but not a week of
  real use. Sustained daily use on the daemon backend is unproven.

## Gotchas that will cost you time

- **NEVER `pkill`/`killall` by process name** (`ironsight`, `ironsight-gui`).
  It kills the user's own running TUI/app, and a killed TUI strands their
  terminal in mouse-reporting mode (every mouse move types garbage). This
  happened three times in one session. Kill only specific pids you recorded,
  and run test processes in an isolated `IRONSIGHT_DATA_DIR`. `pkill -f` also
  matches the Bash-tool's own shell → bare `Exit code 144`. To rescue a stranded
  terminal: `printf '\033[?1003l\033[?1002l\033[?1000l\033[?1006l\033[?25h' >
  /dev/pts/N`, or have the user Ctrl+C then `reset`.
- **The user runs the RELEASE binaries** (`target/release/`), launched from the
  desktop entry. Rebuilding only `debug` means your changes never reach them.
  After meaningful changes: `cargo build --release`, and the app is current on
  next launch. This bit twice.
- **The GUI UI is compiled into the binary** (Tauri `frontendDist`). Editing
  `crates/gui/ui/*` does nothing until you rebuild the gui crate. Bit once.
- **The stack is a real dependency chain.** review/1-platform builds as
  `-p ironsight-core -p ironsight` only (the GUI crate needs a `control::WHERE`
  rename that lands in review/2-gui). Fixes flow bottom-up: fix a lower branch,
  then `git rebase` the upper branches onto it, then force-push with
  `--force-with-lease`.
- **Screenshotting the GUI:** use the `screenshot-gui-app` skill; can only
  capture an app launched under XWayland, never the user's existing Wayland
  windows. Rebuild the gui crate before capturing or you screenshot the old UI.
- **Verify against the artifact that ships, assert the truth not the shape.**
  This codebase has repeatedly had tests pass while a field carried wrong data,
  a redaction masked the wrong thing, or a refutation could never fire. Every
  fix here got a test that would have failed before it.

## Working style the user asked for

- The **keep-alive** skill (`~/.claude/skills/keep-alive/`): work autonomously
  through a list, self-verify each item with evidence, continue to the next in
  the same turn rather than stopping to ask "what's next?". Stop only for a
  genuine fork, a destructive/outward-facing action, or a real block.
- No bold in replies (the user finds it reads as AI). Plainer prose.
- Commit/push only when asked. Finished shareable artifacts go to `~/Downloads`.
