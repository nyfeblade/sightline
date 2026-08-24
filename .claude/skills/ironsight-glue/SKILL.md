---
name: ironsight-glue
description: Reconcile a customised fork of Ironsight onto a new upstream release. Use when running `ironsight glue <version>`, when a fork has diverged from upstream and needs merging, when an upstream change breaks a local customisation, or when asked to update/rebase/reconcile an Ironsight fork. Encodes upstream's architecture, seams, invariants and testing procedure so the merge can be done by adapters rather than by hand.
---

# Reconciling an Ironsight fork

You are updating someone's customised fork of Ironsight onto a newer upstream
release. You know their fork; this document is what upstream knows. Read it
before you touch the diff.

The job is not "resolve the conflicts". It is: keep every upstream change, keep
every local customisation, and where the two genuinely disagree, write a small
piece of translation so both survive. A merge that drops a local feature to make
the build pass has failed, and so has one that pins the fork to an old upstream.

## The one rule about confidence

You do not get to decide the merge worked.

Upstream's whole design says an agent reporting success is worth very little.
The merge is good when the project's checks pass **and** the refutations written
to show it wrong were run and did not fire. Until then it is unmerged work in a
worktree, however sure you are. Never commit to the fork's main branch on the
strength of your own reading.

If the checks pass but the fork has no refutations, say so in your report: the
work is *checked*, not *verified*, and the difference is the whole point of the
verification layer you are working inside.

## The architecture you are merging into

Three crates, one engine.

    crates/core   everything that is not a way of looking at it
    crates/tui    the terminal view
    crates/gui    the desktop app (Tauri; the UI is compiled into the binary)

The load-bearing rule: **neither front end may grow logic the other needs.**
Sending a message, answering a prompt, starting a session, reopening a
conversation — one implementation in `core`, two callers. If an upstream change
moves logic from a front end into `core`, that is upstream removing a
duplication, and a fork that customised the front-end copy must move its
customisation to the `core` seam rather than keeping its copy.

Inside `core`, roughly bottom to top:

    control.rs      what can be done to a session, whichever backend holds it
    tmux/host/daemon  the three backends, chosen at startup by `control::chosen_from`
    session.rs      one session: transcript, totals, status
    registry.rs     Claude Code's live-session registry, and liveness generally
    event.rs        a transcript line becomes a feed entry
    bus.rs          the versioned event stream, the journal, the publisher lock
    gateway.rs      the Unix socket that serves the stream
    stream.rs       transitions detected from session snapshots
    work.rs         assignments, lineage, task state
    checks.rs       the project's definition of done, and refutations
    brief.rs        the constitution, and the packet a worker is briefed with
    limits.rs       ceilings a supervisor cannot raise
    owned.rs        sessions Ironsight holds by pipe rather than by terminal
    chief.rs        the brief that turns a session into a supervisor
    app.rs          the engine: discovery, refresh, steering, everything wired

## Where a fork is meant to plug in

These are the seams. A local change that lives in one of them will almost never
conflict with upstream, and a local change that lives anywhere else is the
first thing to look at when something breaks.

**A new agent** — `crates/core/src/agent/`. Implement `Adapter`, add it to
`agent::all()`. `Record` says how its transcript is written; `Naming` says
whether it renames itself. If the fork added an agent, upstream changes to the
`Adapter` trait are the likely break, and the fix is to implement the new method
rather than to revert the trait.

**A new backend for holding sessions** — `crates/core/src/control.rs`, the
`on_backend!` macro and the `Backend` enum. Every backend offers the same
function names. Upstream adding a function to that set breaks a fork's custom
backend loudly, at compile time, which is the good case: implement it.

**Project configuration** — everything under a project's `.ironsight/`:
`checks.toml`, `constitution.md`, `limits.toml`. These are data, never code. A
fork that wants new project-level configuration should add a file here rather
than a flag.

**A new view or pane** — `crates/tui/src/ui.rs` and `crates/gui/ui/app.js`. Both
are presentation. Adding one is safe; the thing to watch is that the data it
needs comes from `core` and not from a computation the front end does itself.

**A new command for the window** — a `#[tauri::command]` in
`crates/gui/src/main.rs` plus its `invoke_handler` entry. Adding one is safe.
Forgetting the handler entry is the classic fork bug: the UI calls it and gets
"command not found" at runtime, not at compile time.

**A new event kind** — `bus::Kind`. See the invariant below before you touch it.

## Invariants: what must never break

Each of these is load-bearing. If a reconciliation breaks one, the merge is
wrong even if everything compiles and the tests are green. Where a way to prove
it exists, use it.

1. **One writer journals.** `bus::PublisherLock` decides who writes
   `events.jsonl`, on every platform. Two processes numbering events from
   separate counters makes `--since` meaningless. Never make journalling
   conditional on the backend or the platform.

2. **The event vocabulary is a promise.** `bus::VERSION` is 1. Fields are added,
   never removed or repurposed; kinds are added, never renamed. Anything else
   bumps the version, with both emitted for one release. A fork that renames a
   kind has broken every consumer, including `ironsight events`.

3. **Redaction is on the way out, never on the way in.** Command lines are
   masked before they reach the journal or the socket. The interface still shows
   the real command — it is the user's machine. If a fork adds a new thing that
   leaves the process, it goes through `redact::text`. If it adds a new thing
   that only gets displayed, it must not.

4. **Checks only ever refuse.** A passing suite says the failures it can express
   did not happen; it never says the work is good. `Claimed` → `Checked` →
   `Verified`, and only a refutation that *has been seen to fire* can carry a
   task to `Verified`. Never let a passing check write "verified".

5. **Nothing runs from a repository until it is read.** `.ironsight/checks.toml`
   is shell arriving with someone else's code; `ironsight trust` approves those
   exact commands, and it asks again if the file changes. Never add a path that
   executes project configuration without the trust gate.

6. **A ceiling lives outside the worktree.** `limits.toml` in Ironsight's data
   directory is the real ceiling; a project's `.ironsight/limits.toml` may only
   lower it. A fork that moves ceilings into the repository has made them
   editable by the thing they constrain.

7. **A session is only reshaped when nobody else is attached.** `control::frame`
   resizes a pane to draw it; doing that to a session a person is also sitting
   in wraps every line in the wrong place in both. Keep the attached check.

8. **The daemon owns file descriptors and makes no judgements.** Status, cost,
   transcripts and tasks stay in the front ends, which read the same files they
   always read. A daemon that starts deciding things has to be restarted to
   change one.

9. **`owned::Spec` changes bump `daemon::WIRE`.** That struct carries what an
   agent is allowed to do. A daemon built before a field existed will silently
   ignore it and start a session with the wrong permissions — this has actually
   happened. `Spec` is `deny_unknown_fields` for the same reason.

10. **Core is synchronous and embeddable.** No async runtime. Both front ends
    reach `core` directly; making it async makes both async.

## Classifying an upstream change

Work through the diff and put every change in one of these buckets. Say which
bucket in your report — it is how a human checks your judgement quickly.

**Additive, no fork impact.** A new module, a new event kind, a new command, a
new test. Take it as-is.

**Additive to a seam the fork implements.** A new trait method, a new backend
function, a new field on a struct the fork constructs. The fork must implement
or supply it. This is the case adapters are for, and it fails at compile time,
which is the good case.

**A signature or shape change in `core` that the fork calls.** The reconciliation
is a small adapter: keep upstream's new signature, and give the fork's call
sites a wrapper with the old shape where that is cheaper than updating them.
Prefer updating call sites when there are few; prefer an adapter when the fork
calls it from many places or when the fork's version has extra behaviour.

**A change to something the fork also changed.** The genuinely hard case. Read
both intents before writing anything: what was upstream trying to fix, what was
the fork trying to add. Almost always both can be kept, and the merge is upstream's
structure with the fork's behaviour reapplied inside it — not a choice between
two texts.

**A change to an invariant above.** Stop. Do not reconcile this automatically.
Report it to the user with the invariant named and what upstream did to it.

**A rename or a move.** Follow it. Upstream renamed `scope` to `ironsight` once
and left the old names recognised so running sessions were not orphaned; expect
that pattern and preserve both sides of it if the fork depends on either.

## The testing procedure

This is the gate. Run all of it from the worktree, not from the fork's main
checkout.

    cargo fmt --check
    cargo test
    node crates/gui/ui/tokenize.test.mjs
    cargo check --target x86_64-pc-windows-msvc -p ironsight-core -p ironsight

Notes that will cost you time if nobody says them:

- `cargo test` includes doctests. An indented block in a `//!` comment is a Rust
  doctest and will try to compile; fence it as ```text.
- The window's UI is compiled into the binary. Editing `crates/gui/ui/*` does
  nothing until the gui crate is rebuilt. The assets are compressed in the
  binary, so `strings` will not find your CSS — that is not evidence the rebuild
  failed.
- The Windows cross-check needs no Windows. The app crate needs `llvm-rc` and is
  excluded from it deliberately.
- Tests that touch the world must say what they mean, not what they looked like
  on one machine. Upstream has had three rounds of red CI from exactly this.

And the rule upstream applies to every fix: **a fix gets a test that would have
failed before it.** If you write an adapter, write the test that fails without
it. A reconciliation with no new tests has not been shown to reconcile anything.

## The protocol

1. **Read before diffing.** The fork's `README`, its `.ironsight/constitution.md`
   if it has one, and `git log` on the fork's own commits. You are about to make
   decisions about what the fork was for; find out.

2. **Establish the three sets.** What upstream changed since the merge base,
   what the fork changed since the merge base, and the intersection. The
   intersection is the work; the rest is mechanical.

3. **Classify** every change in the intersection using the buckets above.

4. **Reconcile in the worktree**, smallest changes first, keeping upstream's
   structure and reapplying the fork's behaviour inside it.

5. **Write the tests** that would have failed before each adapter.

6. **Run the gate.** All four commands. If anything is red, fix it or stop — do
   not weaken a check to make it pass, and do not delete a fork test you do not
   understand.

7. **Report**, whether it worked or not:
   - what upstream changed, in one line per bucket
   - every adapter you wrote and why
   - anything you could not reconcile, named, with what you would need
   - any invariant upstream touched
   - the exact output of the gate
   - whether the result is *checked* or *verified*, and which refutations ran

8. **Do not merge to the fork's main branch.** Leave it on the worktree branch
   and let the human take it, unless they have explicitly said otherwise. The
   worktree is the containment; using it and then merging anyway wastes it.

## When to stop and ask

- An upstream change to any invariant in the list above.
- A local customisation that upstream has since implemented differently: the
  fork may want to drop theirs, or may not, and that is not your call.
- Checks failing twice for the same reason.
- Anything where two readings of the fork's intent lead to materially different
  merges.

Stopping is not a failure here. A reconciliation nobody understands is worse
than one that is not finished.
