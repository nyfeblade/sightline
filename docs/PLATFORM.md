# Where Ironsight is going

Ironsight today is a way to watch and steer the coding agents running on your
machine. This document is about what it could become, why the shape of it
matters, and — more usefully — which parts already exist, which are ordinary
work, and which are guesses that have not been tested yet.

It is written to be argued with. Every claim about what exists is checkable in
the repository; every claim about what would work is marked as a claim.

## The thesis

One person can supervise perhaps three or four coding agents before they become
the bottleneck — not because the agents are bad, but because the human turns
into a message bus, carrying intent between sessions that cannot see each other.

The usual answer is to make a better agent. The other answer is to build the
layer between a person and a fleet: something that knows what every session is
doing, can act on any of them, and gives whatever supervises them a shared view
of reality instead of each one guessing separately.

That layer is not agent-specific. A session is a program in a terminal, and a
terminal can be read and typed into. Everything above that — what a
conversation contains, how permission is asked, how work is resumed — differs by
agent and belongs in an adapter.

## The rule that keeps it honest

Every layer must be useful as the top layer.

Not "useful once assembled" — useful on its own, as a product someone would
choose to run with nothing above it. Layers may depend downward. They may never
depend upward or sideways. The event stream may not assume something is
listening. Verification may not assume a supervisor exists. A supervisor may not
assume a planner exists.

The test for any proposed feature is one question: name the product that exists
if this is the top layer. If there is no answer, it is not a layer — it is a
feature of the layer below, pretending.

This is also the failure plan. Everything here reads artifacts that other
projects do not document, and those change. A well-layered Ironsight that loses its
transcript reader still watches, steers and isolates sessions. A monolith loses
everything at once.

## Layer 0 — the substrate

Status: exists, in use daily.

A session is discovered, watched, and driven through the terminal it runs in:
tmux on Unix, a pseudo-console Ironsight owns on Windows. From that come the things
everything else needs.

- Sessions: discovery, status, selection, an order you choose and that persists
- Terminals: the live screen as cells with colour and a caret, keys forwarded
  back, one key that always returns to Ironsight
- Transcripts: what was said, what was called, what it returned, what failed
- Permissions: prompts read off the screen and answered in the shape each agent
  expects — a number for Claude Code, a letter for Aider
- Git: working tree state, per-session worktrees on their own branch, merge and
  discard
- Cost: tokens, cache behaviour, and an API-equivalent estimate
- Machine: processor and memory per session, measured from its process tree
- Lifecycle: start with options, name, close, and reopen a conversation that has
  stopped

Two front ends sit on this — a terminal view and a desktop app — and neither
holds logic the other lacks.

## Layer 1 — the compatibility contract

Status: exists as of the first fixtures in `crates/core/tests/`.

Everything Ironsight knows comes from files nobody documents. When one changes, the
failure is quiet: a status that reads wrong, a prompt nobody is told about, a
cost of zero.

The contract is a set of real records, trimmed and anonymised, with tests that
name what moved rather than merely failing — the registry no longer carries
`procStart`, the prompt is no longer answered by number, the token line is no
longer written after each exchange.

This comes before anything else builds on Ironsight. Without it, an ecosystem is a
pile of other people's code that breaks in unison and blames the wrong thing.

Product if this is the top layer: a tool that tells you the moment your agent's
own format changes, which is worth having on its own.

## Layer 2 — the event model

Status: built. See `PHASE1.md` for what it is and what it cost.

Ironsight already computes every transition worth naming; it simply keeps them to
itself. Making them a versioned stream turns supervision from screen-scraping
into consumption.

Draft vocabulary, to be versioned from the first release and extended rather
than changed:

    SessionStarted      agent, cwd, branch, parent, assignment
    SessionWorking      tool, since
    SessionWaiting      since
    PermissionAsked     question, options, keys
    PermissionAnswered  option, by (human | policy)
    ToolCalled          tool, summary
    ToolFailed          tool, summary
    FileChanged         path, added, removed
    CommitCreated       sha, message, branch
    ChecksPassed        suite, duration
    ChecksFailed        suite, first failure
    SessionStalled      no output for, no files for, repeated error
    SessionEnded        reason
    CostSpent           tokens, estimate

Every event carries the session it came from, the agent that produced it, and
the assignment that session was given. Consumers subscribe; nothing is assumed
about who they are.

Product if this is the top layer: an activity log and a webhook for a fleet of
agents. Useful with nothing above it.

## Layer 3 — lineage and task records

Status: built. See `PHASE1.md`.

Sessions are currently peers in a flat list. Nothing records that one session
started another to do part of its job, so there is no tree to supervise, no
rollup of cost per project, and no way to ask what a supervisor's workers are
doing.

Two additions:

- Lineage: which session started this one, and why
- Task record: the assignment, its state (assigned, working, blocked, done,
  verified), the checks that must pass, and what has been learned about it

The task record is what lets a session die without taking its work with it.
Context does not transfer between sessions; explicit task state is the thing
that can.

Product if this is the top layer: a fleet view that shows the shape of the work
rather than a list of processes, with cost attributed to projects.

## Layer 4 — verification

Status: not built. One to two working sessions. The most valuable layer here.

An agent reporting completion is worth very little. The only trustworthy signals
are external: the build compiles, the tests pass, continuous integration is
green, the diff applies.

Verification means a per-project notion of "the checks", run on demand and on
events, with a task refused rather than accepted when they fail. It is
deliberately mechanical: no judgement, no review of quality, nothing that
requires another model's opinion.

    Agent:      done
    Ironsight:  build failed, 2 tests failing
    task state: not done

The trap is the other direction, and it is worse:

    Agent:      done
    Ironsight:  everything passed
    task state: still not done

Checks can only refuse. A passing suite says the failures it can express did not
happen, which is not the same as the work being right, and writing "verified" on
the strength of it produces confident sign-off on work nobody has tried to
break. What carries a task past `Checked` is a refutation — something written to
succeed only if the work is wrong, that was run and did not. Work with nothing
named that would refute it can be checked and never verified.

And a refutation counts only once it has been seen to fire. One that cannot fire
stands for ever and would verify anything, which is the same error again, one
level down — it was in this codebase until a test went looking for it.

Product if this is the top layer: nothing is marked finished until it is
demonstrably finished, for a human running agents by hand. That is arguably the
most useful single feature on this list and needs no organisation above it.

## Layer 5 — intent

Status: not built. Roughly half a working session.

Two artifacts, both plain files, both readable and editable by a person.

A project constitution: the mission, architectural decisions and why, standing
constraints, preferences, rejected approaches and the reasons, and what done
means here. It is written once and amended as decisions are made, so a decision
survives the session that made it.

An intent packet: what a specific assignment needs and nothing more — the task,
the constraints that apply, the criteria for success, and the conditions under
which the worker must stop and escalate rather than decide.

    TASK          implement the OAuth callback
    CONSTRAINTS   do not change the public API; preserve existing sessions
    SUCCESS       callback works; tests pass; edge cases covered
    ESCALATE IF   the API must change; the schema must change; a security
                  trade-off is unclear

Product if this is the top layer: consistent briefing for sessions you start
yourself, and decisions that stop being re-litigated every morning.

## Layer 6 — supervision and planning

Status: not built, and a hypothesis rather than a plan.

Two roles, deliberately separate because they fail differently.

A foreman answers whether the work is actually happening: progress, stalls,
repeated errors, claimed completion against real checks. It consumes events and
acts on facts. It is the role that must never be another model saying "looks
good".

A chief answers what the person actually wants: intent, architecture, priority,
decomposition, and which decisions are too consequential to make alone. It
converts a paragraph from a human into assignments, and converts results back
into a report a human can act on.

Neither needs a new runtime. A chief is a session with Ironsight on its path and a
brief; the recursion falls out — a chief is a session Ironsight manages, managing
sessions Ironsight manages.

Constraints that are not optional:

- Autonomous workers run in their own worktree. Containment is the price of
  autonomy, and it already exists.
- Permission answering stays human by default. The moment a supervisor answers
  prompts, its blast radius includes anything a permission protects.
- Stalls are escalated, never restarted automatically. From outside, thinking
  and wedged are identical.
- Every supervisory action is an event, so the human can read what was done on
  their behalf.

Product if this is the top layer: one agent supervising several, with the human
consulted on decisions above a threshold they set.

## Layer 7 — organisations

Status: speculative.

A project may contain projects. A chief may create a chief. The hierarchy comes
from the work rather than from a fixed number of levels. Assignments move
between projects as work finishes, which in practice means ending a session and
starting another with the accumulated task state rather than migrating anything.

Limits are part of the design, not an afterthought: a ceiling on agents, on
depth, on concurrent projects, and on spend, with a supervisor required to
justify exceeding any of them.

The machine is a real constraint before the architecture is. A Claude Code
session costs roughly 450MB of resident memory, so twenty is nine gigabytes
before compilers, test runners and databases. A hundred agents is a
fleet-of-machines problem and a different product.

## What Ironsight does not become

- An agent. Ironsight runs other people's agents and does not compete with them.
- A model provider. Whatever you are already authenticated as is what runs;
  Ironsight never handles a key.
- A monolith. Capability grows through layers and adapters, not by adding
  features to the core.
- A judge of quality. It can tell you the tests passed. It cannot tell you the
  work is good, and should not pretend to.

## The open question

Everything above layer 4 rests on one claim that nobody has tested honestly:

> A supervised organisation of agents produces better software, for less human
> attention, than the same person directing the same agents by hand.

It is falsifiable and cheap to test. Build layers 2 through 5, brief one chief
with three verified workers on real work, and compare against doing it yourself.
Judge it on the third day rather than the first demo, because the first demo of
an agent hierarchy always looks good.

Evidence that would count: fewer decisions escalated twice, fewer contradictory
implementations, less time spent re-explaining the same context, and work that
passes its checks the first time it is reported as done. Evidence that would
count against it: more notifications rather than fewer, intent arriving at the
worker in a form the human would not recognise, and cost rising faster than
output.

If the answer is no, layers 0 through 5 remain useful on their own — which is
the point of building them in that order.

## Risks worth stating plainly

The substrate moves. Everything depends on undocumented files, and the
compatibility contract is the only reason a change becomes a failing test
instead of a confused user.

Verification is the ceiling. If done cannot be distinguished from claimed, no
hierarchy helps. This is why layer 4 comes before layer 6.

Intent decays through layers. Each level is another chance to paraphrase a
person into something adjacent to what they meant.

Attention is the budget. The promise is to spend tokens to save human attention.
An organisation that produces twenty notifications where there were five has
failed, however good its code is.

Terms of use are real. Model subscriptions are for a person working
interactively. A fleet running continuously belongs on API keys or local models,
with the cost visibility Ironsight already provides.

## Sequence

    1  compatibility contract        done
    2  event model                   done
    3  lineage and task records      done
    4  verification                  ~1–2 sessions
    5  intent artifacts              ~½ session
    6  one chief, three workers      ~½ session to stand up
    7  measure, honestly             a fortnight of real use
    8  organisations                 only with evidence from 7

Every step before 6 improves Ironsight for someone who never builds an organisation
at all. That is the property to protect: add a layer and it gets better, remove
one and what remains is still a thing worth running.
