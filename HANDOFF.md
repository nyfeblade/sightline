# Sightline — handoff, 2026-08-26

State at handoff: branch `solidify`, `dc68b83`, pushed and clean. 361 tests,
`cargo fmt --check` clean, Windows cross-check clean, all 11 invariants quiet.

`master` is behind; two Cursor branches exist on the remote
(`cursor/grok-bot-vendor-31b4`, already merged; `cursor/sightline-042-rename-e62f`,
a rename cleanup that has not been reviewed here).

---

## What Sightline is, in one paragraph

A control plane for coding agents. It hosts them rather than sitting beside
them, so every tool call stops at a boundary Sightline owns — in Rust, before
the call happens — where it is allowed, refused, amended, or handed to a person.
Nothing leaves the machine and there is no API key: it drives the CLIs you
already pay for. `crates/core` holds everything that is not a way of looking at
it; `crates/tui` is the commands, `crates/gui` is the window, and neither front
end may grow logic the other needs.

---

## The four agents, and what is true of each

| agent | governed | how it is driven |
| --- | --- | --- |
| Claude Code | fully | stream-json over a held pipe; `--permission-prompt-tool` routes decisions to an in-process MCP server |
| Cursor | fully | same wire after one translation; boundary via `.cursor/hooks.json`; spoken to by `--resume <chatId>` because `-p` is one-shot |
| Grok Bot | **not at all** | connected, never spawned; assignments wait in a mailbox and are collected with `inbox` over `sightline mcp` |
| Aider | not at all | watched only; refused as a worker because it is spawnable and ungoverned |

The rule that decides whether an agent may be a worker is **spawnable, not
governed**. If Sightline starts a process on this machine it must be one the
boundary reaches, or Sightline is what put an unrestricted agent here.
Coordinating with something already running elsewhere creates no local process
to restrain, so refusing it makes nobody safer — the work just happens outside
Sightline instead of through it.

Grok Bot was merged in claiming `Governance::Partial` on the reasoning that it
is Cursor's desktop assistant and so reads `.cursor/hooks.json`. That was tested
and is false: asked to write `/tmp/escaped.txt` in a workspace whose hook
refuses exactly that, it reported writing the file, no such file exists on this
machine, and the probe log that records every hook invocation was empty. It runs
on its own cloud computer. It is `Governance::None` and says so wherever it
appears.

---

## What each cost measurement actually showed

Cost is dominated by context re-read, not output. Measured across one real
supervised project (six sessions):

    output          924,643
    cache writes  1,989,466
    cache reads  61,568,115      ← 67x the output

Every turn re-sends the whole conversation, so a session costs roughly its turn
count times its average context: **it grows with the square of session length**
and barely at all with how much it writes. One 158-turn session at 172k peak
cost 16.9M re-read; the same work split in two would cost about half.

Sightline's own instrument was blind to this until 2026-08-25 — `owned.rs` read
`output_tokens` and discarded the rest. `Kind::CostSpent` now carries `cached`
and `written`, `limits::spend_by_session` aggregates per session, and `fleet()`
reports it so a supervisor can see a worker's context running away while
splitting it is still possible.

Levers, in order of measured effect: shorter sessions; quoting what the chief
already read into the assignment rather than sending a worker to rediscover it;
`effort` (reasoning tokens become context, so effort compounds); model choice.

---

## Routing, and why Sightline does not rank models

`.sightline/routing.toml` in a project. A route is named, carries prose saying
what it is for, and settles agent, model and effort together. A supervisor asks
for one by name; anything it names explicitly overrides the route. A project
with no routing file has **no** routes rather than default ones — a default set
would be Sightline ranking somebody else's models, frozen when written, applied
to work it cannot see, and wrong in a way nobody notices because a fleet quietly
using the wrong model still produces plausible output.

`sightline routes` closes the loop: tasks, verified, open, spent, and cost per
verified item. Spent is output plus a tenth of cache reads, which is how it is
billed. A route with nothing verified shows `—` rather than zero, because zero
reads as free and would flatter exactly the route that spent most and delivered
least. A chief's own task is excluded — it is the most expensive session in a
fleet and would swamp the comparison.

**It currently reads 15 tasks, 0 verified.** No route has said anything about
itself yet. That is the single most valuable thing to change.

---

## The ladder, and the gap that just closed

Claimed → Checked → Verified. Verified requires something written to show the
work wrong that was run, did not fire, and **has been seen to fire at some
point**. A refutation nobody has watched catch anything has proved nothing.

`claim` existed for months and could be called by nobody: workers started
without kernel tools, so the only sessions that could reach it were chiefs,
which are never assigned anything. `kernel::Role` now splits the tool sets — a
worker gets `claim`, `note` and `inbox`; a chief gets `assign`, `fleet` and
`tell`. A worker that could assign would start workers of its own, and a ceiling
counting only the sessions it knows about is not a ceiling.

---

## Live state

Eleven sessions, one working, 4.1M output tokens. The adaudit fleet (a chief and
five workers, in worktrees under `~/adaudit-wt-*`) is idle — the sessions
finished their turns and are waiting, not spending. **Five assignments did
substantial work and none reached Verified**, because nothing has run
`sightline check`. Workers can `claim` now, which they could not when those five
started.

`~/adaudit-sandbox` has a constitution (`​.sightline/constitution.md`, committed
locally, remote push is deliberately DISABLED there), a checks file, and six
routes including two Grok ones.

---

## Open items, in the order they are worth doing

1. **Get the five adaudit tasks to Verified.** Costs nothing new and produces the
   first real data in `sightline routes`. Everything else about routing is
   speculation until this exists.
2. **Escalation.** The boundary allows, refuses and rewrites; it will not hold a
   call open while a person decides. Both Claude Code and Cursor can support it
   — Cursor's hook takes `"ask"` — so what is missing is a pending-decision
   store, a surface in both front ends, a timeout policy, and a rule for what
   happens if Sightline exits holding calls open.
3. **A capacity ceiling.** `limits::refuse` runs at the door and asks about count
   and spend. It should also ask whether the machine can take another session.
   Note: this laptop runs zram, so swap reads 100% full permanently and is not a
   signal — `MemAvailable` is. Sessions cost 450–970 MB each.
4. **Grok Build over ACP.** xAI ships a local coding agent speaking the Agent
   Client Protocol, which has a permission flow in the specification. It would be
   the first vendor seam published deliberately rather than reverse-engineered,
   and every future ACP agent would then be nearly free.
5. **Session-length enforcement.** The chief is told in prose that cost is
   quadratic in session length and left to comply. A rule that splits a worker
   when its context passes a threshold, handing over compacted state, would halve
   a long task's cost without anyone deciding anything.
6. **Three stale task records.** t16, t17 and t18 were written against adaudit
   worker sessions during testing, before the handle collision was fixed. Harmless
   but wrong; remove when convenient.

---

## Working method that has been right every time

**Run the thing.** Every significant finding today came from executing something,
and every significant error came from reading documentation and reporting what
it said. The control protocol was established by probes; Raycast's glass by
sampling its pixels; the cost figures by summing real transcripts; Cursor's hook
contract by reading its minified bundle when its `--help` denied hooks existed;
Grok Bot's ungovernability by one prompt after a careful argument said otherwise.

**Watch a test fail before believing it.** A refutation nobody has seen fire has
proved nothing. Break the code on purpose, watch it catch, restore, watch it go
quiet.

**Assert every string replacement.** Two edits silently matched nothing today and
reported success. Anything scripted must fail loudly when its target moved.

**Prove it in the artifact that ships.** `crates/gui/build.rs` did not declare the
frontend as a build input, so `cargo build` could hand back a binary carrying the
previous stylesheet — a change in the tree, a green build, and the old thing on
screen. Fixed, but the lesson generalises.

---

## Standing constraints

- Never `pkill`/`killall` by name. Kill only recorded pids; a killed TUI strands
  the terminal in mouse-reporting mode.
- Run tests and experiments under a scratch `SIGHTLINE_DATA_DIR`.
- The user runs the release binaries. The GUI's interface is compiled in, so
  editing `crates/gui/ui/*` requires rebuilding the gui crate.
- Commit and push only when asked.
- No bold in replies; plainer prose.
- Finished shareable artifacts go to `~/Downloads`.
- Do not install or download without checking first.
- `docs/tiers.html` renders one of every information tier against the real
  stylesheet — the hierarchy cannot be checked from the running app, because the
  things that matter most are rarely on screen.
