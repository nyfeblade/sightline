# Ironsight redesign — for review

Written 2026-08-24, to be argued with. This is a proposal, not a plan of record.
`PLATFORM.md` is the current direction and `STATE.md` is what is actually built;
this document says what should change and why, and is explicit about which
parts are opinion and which are established fact.

## The one-line version

Ironsight stops being a thing that watches other people's agents and becomes the
thing that runs them: its own session type, its own supervision loop, its own
permission model, driven from a surface you type into. Monitoring stops being
the product and becomes a kernel service.

## What Ironsight is today

Three crates, one engine, two front ends. It watches Claude Code sessions by
reading the transcripts and registry Claude Code writes, steers them by typing
into their terminals, and — since recently — can hold sessions itself by driving
`claude -p --input-format stream-json` over pipes.

On top of that sit layers that are genuinely built and tested:

- an event stream, journalled and served, with a compatibility contract
- assignments, lineage and task state, with cost rolled up the tree
- verification: project checks, and refutations that must have been *seen to
  fire* before their standing counts as evidence
- ceilings on session count and spend, enforced at the two doors a session can
  come through, in a file outside every worktree
- a trust gate: nothing runs from a repository until those exact commands are
  approved
- worktree isolation
- a Hub with two faces: monitoring, and directing work

The verification and limits half is the part that took the thinking, and it
should survive this redesign untouched.

## What is wrong with it

**The supervisory loop is prose.** `chief::brief` renders a document and hands it
to a model. Nothing in Ironsight knows what step a chief is on, notices when it
skips one, or can say it has been forty minutes with nothing checked. Two of the
three prohibitions in that brief are unenforceable.

**Observed, not theorised.** A chief was run live against a real project. The
best of three runs read the constitution and the checks, diagnosed the bug, and
wrote an assignment more precise than the one it was given — including an edge
case the tests did not cover. Then everything else it did was run shell commands
a person could have run faster: `ironsight new`, poll, `ironsight check`,
`ironsight trust`. Its entire contribution was rewording the task. At one worker
and one task, that is an indirection, not a supervisor.

**The inversion.** Everything mechanical — start a worker, wait, run the checks,
run the refutations, refuse, escalate — is done by a language model through a
shell, sometimes blocked by permissions it does not have. Everything that
genuinely needs a model happens once, in the first thirty seconds. It is exactly
the wrong way round.

**Borrowed processes.** Every limitation that hurt in practice came from the
agent not being Ironsight's: a permissions field silently dropped by a daemon
built before it existed; a grant list discovered only by watching a chief fail
and report itself blocked; no way to ask a person anything mid-run.

## The finding that changes the options

Claude Code's shipped binary speaks an undocumented control protocol over the
same stream-json pipes Ironsight already uses. Extracted from
`~/.local/share/claude/versions/2.1.241`:

    type: "control_request" | "control_response" | "control_cancel_request"

    subtypes seen: initialize, can_use_tool, interrupt,
                   set_permission_mode, hook_callback, mcp_message

    can_use_tool carries: tool_name, display_name, input, tool_use_id,
                          description, permission_suggestions
    responses are: { subtype: "success" | "error", request_id, response }
    interrupt carries: cancel_queued

**This has not been driven yet.** It was read out of the binary, not exercised.
Verifying it is the first task in any plan below, because two prior conclusions
in this project were wrong for exactly the reason that this one might be: they
were drawn from `--help` and from observed behaviour rather than from trying the
thing.

If it works, it means:

- a permission can be routed to a human watching a fleet, on a subscription,
  from Rust, with no Node SDK and no API key
- a turn can be interrupted mid-flight
- permission mode can be changed while a session runs
- hooks and MCP can be driven from the host

That is most of what "own the loop" was going to buy, without writing a model
client. It does not give model choice, and it does not give a scheduler — those
still have to be built.

## The proposed shape

### Sightline

The surface you type into. You give it what you want in your own words; it is a
continuing conversation over the life of a project, not a one-shot prompt. This
is where a Super Chief lives.

The Super Chief has **no hands**. It emits intent — decompositions, priorities,
escalations, reports — and the kernel acts. It never runs a command. The moment
it can start things directly, the current failure returns.

### The Hub

Monitoring, as deep as can be managed. Not the front door any more. It is what
you turn to when something looks wrong, and it is a kernel service rather than
the product: an OS whose processes are black boxes is a broken OS, but nobody
adopts an OS for its process viewer.

### The kernel

What Ironsight becomes. It already has processes with identity and lifecycle,
isolation, resource limits, accounting, persistence and a shell. Three things
are missing and they are the OS parts:

**A scheduler.** Today a model decides what runs next by typing shell commands.
The kernel should decide, from policy: a task is assigned and has no worker, so
start one; a worker claimed, so run the checks; checks passed, so run the
refutations; failed twice the same way, so ask the model to re-plan; needs a
human, so say so in Sightline. The model is consulted where judgement is
irreducible and nowhere else. Everything previously described as "the workflow
belongs in the heart" is this.

**A syscall interface.** An agent that wants something from Ironsight currently
shells out to `ironsight` and hopes it has been granted permission. That is not
an interface. A socket with a defined request set and an identity per session
makes the permission question answerable and removes the grant-list problem
entirely.

**A permission model of Ironsight's own.** Currently delegated to whichever
vendor binary is running, which is why a worker can be refused something you
would have approved. In an OS, the kernel decides what a process may do.

### The organisation

One session is one project, containing a Super Chief, project chiefs, and
agents. Proposed change to that list: **the foreman becomes a kernel service,
not a role.** Its entire job — watch for claimed work, run checks, run
refutations, refuse — is mechanical. Keeping it as an agent means paying a
language model to run `cargo test` and read an exit code. Keep the word in the
interface; delete the session.

## The changes, itemised

1. **Verify the control protocol.** Drive `claude -p --input-format stream-json`,
   answer a `can_use_tool` request, and interrupt a turn. Everything else is
   contingent on this. Half a day.

2. **Promote `agent::Adapter` from a launcher to an engine interface.** Today it
   says what to run and how to read a transcript. It should say: start a session,
   take a turn, surface a permission request, report a tool call, interrupt,
   stop. One implementation for now — Claude on a subscription. Other vendors
   are deliberately deferred; the seam exists so they can be added.

3. **Route permissions to Sightline.** A tool call needing approval appears where
   you are, and your answer goes back over the control protocol. This is the
   single capability that makes Ironsight something the vendor CLIs are not.

4. **Build the scheduler.** Task state already exists and is the state machine;
   what is missing is the thing that advances it without asking a model.

5. **Make the Super Chief a conversation without hands.** It talks to you and
   emits structured intent — assignments, priorities, escalations — validated
   against a schema. The kernel starts the workers, under the ceilings, in
   worktrees.

6. **Give the chief the accumulated record.** Today it reads the same repository
   you can read, which is why it has nothing to add. Ironsight holds every task
   this project has run, what failed and how, which refutations have ever fired,
   what notes were left, what it all cost — and hands the chief a task list. A
   supervisor whose edge is memory across sessions is doing something you
   genuinely cannot; one that rewords your sentence is not.

7. **Foreman becomes kernel.** No session, no model, no tokens.

8. **Sightline becomes the front door**, with the Hub one turn away. Both in one
   program: two programs means two copies of session state and a synchronisation
   problem.

9. **Demote the compatibility layer.** Reading other people's transcripts,
   scraping panes, the Aider adapter — park rather than maintain. If Ironsight
   owns its sessions it knows rather than infers, and much of this code deletes
   itself rather than needing to be ported.

10. **Model-per-role, when vendors return.** A foreman that reads exit codes
    does not need a frontier model. Subscription usage shares the same quota as
    interactive use, so a fleet competes with the session you type in — which
    makes this structural rather than an optimisation.

## What survives untouched

The event model and its compatibility contract. Assignments, lineage, task
state, cost rollup. Checks, refutations, the fire-once rule, and the refusal
that `Verified` cannot be reached by anything an agent says. Ceilings and where
they live. The trust gate. Worktree isolation. The Hub's rendering. Glue and its
ability. The invariants.

## Open questions

These are genuinely undecided and are the most useful things to argue about.

1. **Smallest version that counts as working.** "An agent completes a real task
   under Ironsight's scheduler, with permissions routed to Sightline, on a
   subscription" is roughly three weeks. "An agent OS with crazy amounts of
   ability" is unbounded. The difference between those two is whether this ships.

2. **Does a native API engine exist in the plan at all**, or is delegating the
   turn to a vendor binary the permanent architecture? Owning the model turn
   costs an edit tool and context management — years of accumulated tuning, and
   most of the quality gap. Owning the session costs neither.

3. **Tree depth.** Super Chief → project chief → agents is three levels. "A chief
   may create a chief" is unbounded, and unbounded is where cost, ceilings and
   debugging all become hard.

4. **Concurrency.** Several projects at once share one machine-wide allowance. A
   chief planning within a budget it does not control will plan badly.

5. **Is the Super Chief one conversation per project, or one across all of
   them?** Memory and cost pull in opposite directions here.

## Risks

**The unbounded-scope failure.** The version of this that dies builds a
scheduler, a syscall layer and a permission model before a single agent has done
useful work under them. The order that de-risks it is: make one thing run
end-to-end under Ironsight's control, then generalise the control into a kernel.

**The premise is still untested.** Nothing has shown that supervised
orchestration beats one person driving the same agents by hand. That is Layer 7
in `PLATFORM.md` — a fortnight of measured use — and it has never been run.
Everything above Layer 4 rests on it. Building a kernel on the assumption raises
the stakes on finding out.

**Quota.** Subscription usage is not a separate pool. A fleet and the session
you are typing in draw from the same allowance.

**Two prior conclusions in this project were confidently wrong** and were only
caught by driving the real thing: that `--allowedTools` restricts (it grants),
and that stream-json has no permission seam (it has one, undocumented). Any
claim in this document that has not been exercised should be treated the same
way.
