# Passoff — resume the Ironsight build

Written 2026-08-24. Read this, then `docs/STATE.md` (what is true now),
`docs/PLATFORM.md` (the layers and why), and `docs/BUILD.md` (how each layer is
built and proved). This file is the shorter, more perishable "where the work is
and what to do next".

## One line

Layers 0–6 are built, both halves of 6 included, plus the daemon, owned sessions
as a first-class kind, ceilings a supervisor cannot raise, invariants that can
fire, and glue. Everything is green. What is left is not another layer; it is
the experiment, and living with what exists.

## Where the code is

The full, current tip is the local branch **`solidify`**. `master`/`main` are the
published `v0.4.1` base (`aa01afa`) and are untouched. The stack, bottom to top:

    master (v0.4.1)
      └─ review/1-platform     event model, verification, daemon, hardening
          └─ review/2-gui      Talk view, redesign, icon
              └─ review/3-stream-json  owned sessions   (== ironsight-integration)
                  └─ layer-5-intent    constitution + brief, tasks --json
                      └─ solidify      engine panic-resilience, then this work

Three PRs are open on GitHub (`nyfeblade/ironsight`), each reviewing exactly one
slice (base is the slice below):

- PR #1 `review/1-platform → master`
- PR #2 `review/2-gui → review/1-platform`
- PR #3 `review/3-stream-json → review/2-gui`

All three had a cloud ultrareview; 20 findings across them, all real, all fixed
with tests. `ironsight-integration` is stale — it sits at the PR #3 tip.
`solidify` is the one to build from and to base any new branch on.

Layer 5 and everything after it is **not yet in a review PR**, and that is now
the largest untested-by-adversary surface. Worth opening PR #4
(`layer-5-intent → review/3-stream-json`) and PR #5 (`solidify → layer-5-intent`)
before anything else.

Everything described here is committed and pushed on `solidify`. The standing
rule is to commit only when asked, so if you find uncommitted work, ask about it
before building on top of it.

## What is built (layers)

- **0 substrate, 1 compatibility, 2 events, 3 lineage/tasks** — built earlier.
- **4 verification** — checks + fire-once refutations + `ironsight foreman`.
  Checks only ever *refuse*; a task reaches Verified only when a refutation
  written to show the work wrong was run, did not fire, and has been seen to
  fire at least once. Nothing runs from a repo's `.ironsight/checks.toml` until
  `ironsight trust` approves those exact commands.
- **5 intent** — `.ironsight/constitution.md`, parsed into fixed sections;
  `ironsight brief <who>` renders a task-focused packet. `--task` briefs a
  session as its opening message. Both halves are now in the window: a session's
  brief, and the constitution read and edited in place.
- **6 supervision** — both halves. The foreman refuses claimed work that does not
  pass. `ironsight chief` starts a supervisor: Ironsight on its path, a brief,
  and a ceiling it cannot raise. It will not start without ceilings in force,
  because granting something else the power to start sessions is exactly the
  case they exist for.
- **Ceilings** — `limits.rs`. A count of Ironsight's own running sessions and an
  amount of spend, checked at both doors, in a file outside every worktree. A
  project may lower them and never raise them.
- **Invariants** — `[[invariant]]` in `.ironsight/checks.toml`, commands that
  must *fail*. `ironsight invariants` runs them; a broken one refuses work.
- **Glue** — an ability shipped in the binary that teaches a fork's own agent
  upstream's architecture, seams and invariants, plus `ironsight glue` to drive
  the reconciliation in a worktree.
- **Off-roadmap:** the daemon (`ironsight serve`), and owned sessions.

## Owned sessions, which everything above is built on

`ironsight new <path> --owned` starts a session Ironsight holds itself, spoken to
over Claude Code's stream-json with no terminal. It takes the same folder, model,
permission mode, name, task, parent and brief as any other session.

The design turns on one fact, which was checked against the real tool before a
line was written: **a headless stream-json session writes an ordinary
transcript**, at the usual `~/.claude/projects/<slug>/<id>.jsonl`. So every view
already works on it, and Ironsight only supplies the two things a watched session
gets for free — liveness (no registry entry is ever written for one) and a way in
(no pane to type into). See `## A second kind of session` in `STATE.md`.

What it cannot do, and why, is worth reading before you try to fix it:
**Claude Code 2.1.241 has no `--permission-prompt-tool`.** The passoff before
this one assumed that seam existed. It does not; `claude --help` has no such
flag. In this mode a tool the settings do not allow is refused outright — the
stream carries `system/permission_denied` and the call returns an error — so
there is no request to route to a person. What was done instead: the permission
mode is chosen at start and shown in `ironsight owned`, and a refusal is
published as `PermissionAnswered` by a `Policy` named after that mode. If a later
Claude Code adds a permission seam, `owned::Parser` is where it lands.

## What to build next

There is no obvious next layer, which is itself the finding. In rough order of
what would actually be worth the effort:

1. **Get everything above `review/3-stream-json` adversarially reviewed** — PRs
   #4 and #5. Everything below that line had a cloud ultrareview; four layers
   have since landed on tests and one pair of eyes. This is the largest
   unreviewed surface in the repository, and it includes the code that decides
   what an agent is allowed to do.
2. **Run the experiment.** Does supervised orchestration beat one person driving
   the same agents by hand (Layer 7/8 of `PLATFORM.md`)? Everything it needs
   exists now. `BUILD.md` has the protocol; it is a bet to run, not code.
3. **Live with the daemon**, and with a chief. Both work; neither has been used
   for a week. Sustained daily use on `IRONSIGHT_BACKEND=daemon` is the only
   thing that will find what is left.
4. **Glue against a real fork.** It is proved end to end against a fixture with
   a genuine two-sided divergence, and never against a fork someone actually
   cares about. The first real one will find something.

## The honest gaps (cannot be closed from this Linux box)

- Windows has never been *run* on Windows; the macOS app has never been *run* on
  macOS. Both compile, cross-check clean, and are unit-tested — not the same as
  working there. Needs a real Windows box and a Mac. Note that owned sessions
  fall back to being held in-process there, because the daemon needs a Unix
  socket, so they end with the window.
- The daemon has not been lived with for a week.
- Aider is wired but was proved with a stand-in binary and a real recorded
  history, not by driving actual Aider against a model. The reading is tested
  against a real run's output; the *pane discovery* was tested with a shim.
- A chief has run once, well, until a session rate limit stopped it. That is
  evidence it starts and orients, and no evidence about a long run.
- The spend ceiling is measured from the event journal, which is only written
  while an Ironsight window or terminal view is running. On a machine that only
  ever runs the commands it measures nothing, and `ironsight limits` says so.

## Gotchas that will cost you time

- **NEVER `pkill`/`killall` by process name** (`ironsight`, `ironsight-gui`,
  `claude`). It kills the user's own running TUI/app, and a killed TUI strands
  their terminal in mouse-reporting mode. Kill only specific pids you recorded,
  and `tmux kill-session -t <exact name>` for tmux. `pkill -f` also matches the
  Bash-tool's own shell → bare `Exit code 144`. To rescue a stranded terminal:
  `printf '\033[?1003l\033[?1002l\033[?1000l\033[?1006l\033[?25h' > /dev/pts/N`.
- **Run tests and experiments under `IRONSIGHT_DATA_DIR=<scratch>`.** The daemon
  socket, journal and task store all live there, so an experiment cannot then
  touch the user's real fleet — and the daemon you start is one you can kill by
  the pid you recorded.
- **The user runs the RELEASE binaries** (`target/release/`), launched from the
  desktop entry. Rebuilding only `debug` means your changes never reach them.
  After meaningful changes: `cargo build --release`.
- **The GUI UI is compiled into the binary** (Tauri `frontendDist`). Editing
  `crates/gui/ui/*` does nothing until you rebuild the gui crate. It does rebuild
  correctly on a UI-only change — verified — but `strings` on the binary will not
  find your CSS, because the assets are compressed. Do not conclude from that
  that the rebuild failed.
- **`cargo test` runs doctests.** An indented block in a `//!` comment is a Rust
  doctest and will fail to compile; fence it as ```` ```text ````. One had been
  failing on the tip unnoticed, which means somebody had been reading
  `cargo test --lib` and calling it green.
- **The stack is a real dependency chain.** review/1-platform builds as
  `-p ironsight-core -p ironsight` only. Fixes flow bottom-up: fix a lower
  branch, rebase the upper ones, force-push with `--force-with-lease`.
- **Screenshotting the GUI:** use the `screenshot-gui-app` skill; you can only
  capture an app you launched under XWayland, and there is no `xdotool` on this
  box, so you cannot click. To see a dialog, temporarily open it from a
  `setTimeout` at the end of `app.js`, capture, then remove it. To choose which
  session is selected, write `order.json` in the scratch data dir — the window
  selects the first live session in that order.
- **Verify against the artifact that ships, assert the truth not the shape.**
  This codebase has repeatedly had tests pass while a field carried wrong data.
  Every fix here got a test that would have failed before it.
- **Check the tool before designing around it.** Two of the assumptions in the
  previous passoff were wrong in ways ten minutes with `claude --help` and a
  Python driver would have caught. `--allowedTools` grants and does not restrict,
  which is the opposite of what the name suggests and was only found by watching
  a session run `ls /tmp` while allowed nothing but `Bash(echo *)`.
- **A running daemon holds the old code.** It is started once and outlives every
  rebuild, so a change to anything it does — especially `owned::Spec`, which
  decides what an agent may do — does not reach it until it is restarted. An
  afternoon went into a chief that could not run a command because of this.
  `Spec` is `deny_unknown_fields` now and the wire version bumps for it, so the
  next one fails loudly; the habit to keep is to kill the daemon by its recorded
  pid after changing core.
- **An invariant that fires against an intact repository is worse than none.**
  Three of the first nine did, all from sloppy shell: a `grep -A2` window that
  always contains a line without the term, a guard matched on the wrong line, a
  file that was not where the thing being checked actually lives. Break each
  guarantee on purpose in a scratch tree and watch the command catch it before
  believing any of them.

## Working style the user asked for

- The **keep-alive** skill (`~/.claude/skills/keep-alive/`): work autonomously
  through a list, self-verify each item with evidence, continue to the next in
  the same turn rather than stopping to ask "what's next?". Stop only for a
  genuine fork, a destructive/outward-facing action, or a real block.
- No bold in replies (the user finds it reads as AI). Plainer prose.
- Commit/push only when asked. Finished shareable artifacts go to `~/Downloads`.
