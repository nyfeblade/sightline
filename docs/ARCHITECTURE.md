# Architecture

How Sightline uses Claude's models, and where its own guarantees live.

Written 2026-08-24, on evidence from `docs/probes/control_protocol.py`, which
runs against a real Claude Code and prints PROVED or FAILED for each property
this design rests on. Nothing below is asserted from the shape of an API.

> On the name: an earlier draft of this document called the thing you type into
> "Sightline", while the product was Ironsight. The product took the better name,
> so what is left is simply the prompt — you type into Sightline.

## The one thing that changed

Sightline watched. It read transcripts, inferred what a session was doing, and
handed work over as a brief — text an agent may follow or ignore. Every
guarantee it had was advice, and the honest summary of that was: an automatic
copy-paster.

Claude Code's control protocol removes that ceiling. A host process can be the
thing that *answers* a permission request, synchronously, before the tool runs.
Proved, from Rust's point of view — plain JSON over two pipes, no SDK, no Node,
no API key, on a Claude Max subscription:

| Property | Meaning |
| --- | --- |
| **allow** | every tool call reaches the host before it happens |
| **deny** | the host's refusal actually stops it — the file was not created |
| **rewrite** | the host can alter the call: asked for `asked.txt`, ran `rewritten.txt` |
| **capabilities** | `initialize` returns 6 models with effort levels, 5 agents, 77 commands, and the account tier |
| **levers** | `interrupt` ended a turn with 87s of `sleep` left; `set_permission_mode` changed posture mid-session |
| **kernel-tools** | the model called `mcp__host__remember`, a tool served in-process by the host |

The consequence is the whole design: **a rule Sightline holds stops being prose
in a constitution and becomes a function called at a boundary the agent cannot
route around.** That is the difference between a brief and a kernel.

## The shape

    ┌──────────────────────────────────────────────────────────────┐
    │  THE PROMPT — the one thing you type into                    │
    │  intent in · and the only place a decision reaches a person   │
    └───────────────────────────┬──────────────────────────────────┘
                                │
    ┌───────────────────────────▼──────────────────────────────────┐
    │  THE KERNEL — one process, one journal, one authority         │
    │                                                               │
    │   Supervisor          owns claude processes over stream-json  │
    │   Capability registry what this subscription can do           │
    │   Permission router   ← every tool call stops here            │
    │     └ policy kernels  ceiling · scope · trust · invariant     │
    │   Scheduler           who runs, on what model, under what cap │
    │   Ladder              Claimed → Checked → Verified            │
    │   Journal             one writer · versioned · redacted       │
    └───────┬───────────────────────────────────────┬──────────────┘
            │ stream-json over pipes                │ events
    ┌───────▼───────────────────────────┐   ┌───────▼──────────────┐
    │  WORKER SESSIONS                  │   │  HUB — the window    │
    │  super chief · chief · foreman    │   │  reads the journal   │
    │  · agents  (each in a worktree)   │   │  never a 2nd truth   │
    └───────────────────────────────────┘   └──────────────────────┘

Four things about this shape are load-bearing and easy to get wrong.

**The permission boundary is the seam, not `can_use_tool`.** The route that
works is an MCP server the host serves in-process: `--permission-prompt-tool
mcp__host__approve` plus `sdkMcpServers` at `initialize`. Requests arrive as
`control_request` / `mcp_message` / `tools/call`, and the host answers with a
`behavior` of `allow`, `deny`, or `allow` with an `updatedInput`. Designing
against a `can_use_tool` callback instead produces something that never fires.

**Supervision is flat; the tree is logical.** A chief does not spawn its own
children. Every session is a child of the kernel, and a supervisor's only way to
create work is to call a kernel tool — which the `kernel-tools` probe shows the
model will actually do. If a chief could spawn directly, the kernel would not be
in the loop for what it spawned, and every guarantee below would have a hole in
exactly the place it matters.

**Project truth and machine state are different things, kept apart.**
`constitution.md` and `checks.toml` live in the *project's* `.sightline/`:
versioned with the code, reviewable in a pull request, editable by the team.
The journal, tasks, ceilings and trust records live in Sightline's data
directory, outside every worktree — because a ceiling a supervised agent can
edit is a suggestion.

**Checks do not reach Verified.** A passing suite says the failures it can
express did not happen. Claimed → Checked is a suite; Checked → Verified needs a
refutation written to show the work wrong, which was run, did not fire, and has
been seen to fire at least once. An instrument nobody has watched catch anything
has proved nothing.

## The permission router

Every tool call an owned session makes stops here, with the tool's name and its
input. The router asks the policy kernels in turn, and each returns one of four
things:

    allow      it is within what this session may do
    rewrite    it may proceed in an altered form
    deny       with a reason the model sees and can act on
    abstain    no kernel is confident — escalate

Only `abstain` reaches a person, at the prompt. That ratio is the product: a
supervisor that asks about everything is a copy-paster with extra steps, and one
that asks about nothing is a robot with your credentials.

The kernels are ordinary Rust, deterministic, and testable without a model:

- **Ceiling** — how many of Sightline's sessions are running and what they have
  spent. Lives outside every worktree; a project may lower it, never raise it.
- **Scope** — which paths, commands and hosts this session may reach. A write
  outside its worktree is denied; a path that should be inside it is a candidate
  for `rewrite` rather than refusal.
- **Trust** — nothing runs from a repository's `checks.toml` until `sightline
  trust` has approved those exact commands. At the boundary this becomes: a
  command claiming to be the project's checks must match an approved one.
- **Invariant** — what must never stop being true here. These run at claim time;
  at the boundary they contribute the list of files that are load-bearing, so an
  edit to one is escalated rather than waved through.

`rewrite` deserves its own note, because it is the outcome with no equivalent in
a hook or a settings file, and the probe shows it works: the kernel can pin a
command to its trusted form, redirect a stray path back inside the worktree, or
add `--dry-run` to something it is not willing to allow outright. A gate that
can only say yes or no forces every ambiguous call to a person; one that can
amend answers most of them itself.

## Using the models well

`initialize` hands back what this subscription can do, so none of it is
hardcoded: the model list with each one's supported effort levels, whether it
supports fast mode and adaptive thinking, the available subagents, the
installed commands, and the account tier. New models appear without a release.

That makes role assignment data rather than code — a super chief on a large
model at high effort because it is deciding what happens; mechanical passes on
a small one at low effort because they are not.

The stream also carries `rate_limit_event`, with the window type (`five_hour`),
whether the request was allowed, when the window resets, and whether overage is
available. Quota is the binding constraint on this product — a live chief has
already been stopped by it — so the scheduler treats it as a first-class input:
shed the foremen before the chief, downshift effort before stalling, and hold
queued work until the reset rather than failing it. A run that degrades is worth
more than a run that dies.

Two levers make that enforceable rather than advisory: `interrupt` ends a turn
immediately, and `set_permission_mode` changes what a session may do without
restarting it. Both are proved.

## The prompt and the Hub

The prompt is where intent goes in and where escalations come out. One place, so
the answer to "what is it waiting on" is never "look through the panes".

The Hub reads the journal and shows what is happening. It is deliberately not a
second source of truth: one writer holds a lock and numbers the events, because
two processes numbering from separate counters makes every "since" meaningless.
Monitoring matters less than it used to — the point is no longer to watch
everything, it is to be asked about the few things worth asking about.

## What exists, and what is new

Most of this is built. The delta is smaller than the ambition suggests.

| | State |
| --- | --- |
| Ceilings, trust, invariants, checks + refutations | built |
| Constitution, briefs, tasks, lineage | built |
| Worktree isolation, glue | built |
| Event journal, one-writer lock, redaction, gateway | built |
| Owned sessions over stream-json, the daemon | built |
| **Permission router** — the MCP server and the decision loop | **new** |
| **Policy kernels at the boundary** — the same rules, called per tool call | **new** |
| **Capability registry** — models, efforts, agents, from `initialize` | **new** |
| **Quota-aware scheduler** — roles, degradation, `rate_limit_event` | **new** |
| **Kernel tools** — how a supervisor creates work | **new** |
| **The prompt** — one place to say what is wanted, and be asked | **new** |

The existing kernels do not get rewritten. They get called from a place where
the agent cannot decline.

## Open questions

- **How long may the host hold a permission request?** The CLI blocks on the
  response, which is what makes escalation possible, but no timeout has been
  measured. If one exists, an escalation to a person who has gone for lunch
  needs a defined answer rather than a hang.
- **What is the smallest version that counts as working?** One super chief, one
  worker, one real task, end to end, under a ceiling — before any of the tree.
- **One super chief per project, or one across all of them?** The user's
  framing is one session = one project, which argues per project; the ceiling is
  per machine, which argues the scheduler is not.
- **Concurrency against one allowance.** Several sessions on one subscription
  share a five-hour window. How many is useful rather than merely parallel is a
  measurement nobody has taken.
- **Is a native API engine ever in the plan?** The subscription path is proved
  and is the scope now. Everything above is transport-agnostic except the
  supervisor, which is the point at which a second vendor would be added.
