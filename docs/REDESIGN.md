# Sightline redesign — for review

Written 2026-08-24, to be argued with. This is a proposal, not a plan of record.
`PLATFORM.md` is the current direction and `STATE.md` is what is actually built;
this document says what should change and why, and is explicit about which
parts are opinion and which are established fact.

## The one-line version

Sightline stops being a thing that watches other people's agents and becomes the
thing that runs them: its own session type, its own supervision loop, its own
permission model, driven from a surface you type into. Monitoring stops being
the product and becomes a kernel service.

## What Sightline is today

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
to a model. Nothing in Sightline knows what step a chief is on, notices when it
skips one, or can say it has been forty minutes with nothing checked. Two of the
three prohibitions in that brief are unenforceable.

**Observed, not theorised.** A chief was run live against a real project. The
best of three runs read the constitution and the checks, diagnosed the bug, and
wrote an assignment more precise than the one it was given — including an edge
case the tests did not cover. Then everything else it did was run shell commands
a person could have run faster: `sightline new`, poll, `sightline check`,
`sightline trust`. Its entire contribution was rewording the task. At one worker
and one task, that is an indirection, not a supervisor.

**The inversion.** Everything mechanical — start a worker, wait, run the checks,
run the refutations, refuse, escalate — is done by a language model through a
shell, sometimes blocked by permissions it does not have. Everything that
genuinely needs a model happens once, in the first thirty seconds. It is exactly
the wrong way round.

**Borrowed processes.** Every limitation that hurt in practice came from the
agent not being Sightline's: a permissions field silently dropped by a daemon
built before it existed; a grant list discovered only by watching a chief fail
and report itself blocked; no way to ask a person anything mid-run.

## The finding that changes the options

Claude Code's shipped binary speaks an undocumented control protocol over the
same stream-json pipes Sightline already uses. Extracted from
`~/.local/share/claude/versions/2.1.241`:

    type: "control_request" | "control_response" | "control_cancel_request"

    subtypes seen: initialize, can_use_tool, interrupt,
                   set_permission_mode, hook_callback, mcp_message

    can_use_tool carries: tool_name, display_name, input, tool_use_id,
                          description, permission_suggestions
    responses are: { subtype: "success" | "error", request_id, response }
    interrupt carries: cancel_queued

**This has been driven, and it works.** A permission request was routed out of a
headless session, answered by the host, and the tool call went through — on a
subscription, from plain JSON over the pipes Sightline already uses, with no SDK,
no Node and no API key.

### What was actually exercised

    client → {"type":"control_request", request_id, request:{
                 subtype:"initialize", hooks:{}, sdkMcpServers:["sightline"]}}
    CLI    → control_response success, carrying the session's command list

    CLI    → control_request mcp_message: JSON-RPC "initialize"
    client → control_response with the MCP server's capabilities
    CLI    → control_request mcp_message: "notifications/initialized"
    client → control_response (empty)
    CLI    → control_request mcp_message: "tools/list"
    client → control_response listing one tool, `approve`

    ... the model decides to call Write ...

    CLI    → control_request mcp_message: "tools/call" → approve
             arguments: { tool_name: "Write",
                          input: { file_path: ..., content: "yes\n" } }
    client → control_response: { content:[{ type:"text",
                 text:"{\"behavior\":\"allow\",\"updatedInput\":{...}}" }] }

    ... the write succeeds, the file exists, the turn completes ...

The session was started with:

    claude -p --verbose --input-format stream-json --output-format stream-json \
           --permission-prompt-tool mcp__sightline__approve

### Two traps, each of which cost a run

**`--permission-prompt-tool` is not in `--help`.** It exists in the binary and
works. An earlier conclusion in this project that the flag "does not exist in
2.1.241" was drawn from the help text and was wrong.

**Every `control_request` needs a `control_response`, including the ones wrapping
JSON-RPC notifications.** A notification needs no JSON-RPC reply, but the control
envelope around it still does. Miss that and the session stalls silently after
the handshake — no error, no output, nothing in stderr.

### What it means

- Permissions can be Sightline's, routed to Sightline, on a subscription
- Turns can be interrupted (`interrupt`, with `cancel_queued`)
- Permission mode can be changed mid-session (`set_permission_mode`)
- **Sightline can host MCP tools inside every session it starts.** This is the
  syscall interface from the changes list below, already available. An agent
  asking Sightline for something stops being "shell out to `sightline` and hope
  it was granted permission" and becomes a tool call over a channel Sightline
  owns — which removes the grant-list problem rather than working around it.

This is most of what "own the loop" was going to buy, without writing a model
client. It does **not** give model choice, and it does **not** give a scheduler.
Those are the remaining reasons to go native, and they are weaker reasons than
permissions were.

Note the mechanism: `canUseTool` in the Node SDK and `--permission-prompt-tool`
are alternatives — the binary refuses both at once — and the SDK almost certainly
implements the former using the latter plus an in-process MCP server. Sightline
does not need the SDK to reach it.

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

What Sightline becomes. It already has processes with identity and lifecycle,
isolation, resource limits, accounting, persistence and a shell. Three things
are missing and they are the OS parts:

**A scheduler.** Today a model decides what runs next by typing shell commands.
The kernel should decide, from policy: a task is assigned and has no worker, so
start one; a worker claimed, so run the checks; checks passed, so run the
refutations; failed twice the same way, so ask the model to re-plan; needs a
human, so say so in Sightline. The model is consulted where judgement is
irreducible and nowhere else. Everything previously described as "the workflow
belongs in the heart" is this.

**A syscall interface.** An agent that wants something from Sightline currently
shells out to `sightline` and hopes it has been granted permission. That is not
an interface. It should be MCP tools that Sightline hosts inside each session it
starts — verified as working above — so the request arrives over a channel
Sightline owns, with the session's identity attached, and no grant list to get
wrong.

**A permission model of Sightline's own.** Currently delegated to whichever
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

1. ~~**Verify the control protocol.**~~ Done — see above. A permission was
   routed to the host and answered, and the tool call went through. What remains
   unexercised is `interrupt` and `set_permission_mode`; both are present in the
   binary and ride the same envelope, so the risk is low but not zero.

2. **Promote `agent::Adapter` from a launcher to an engine interface.** Today it
   says what to run and how to read a transcript. It should say: start a session,
   take a turn, surface a permission request, report a tool call, interrupt,
   stop. One implementation for now — Claude on a subscription. Other vendors
   are deliberately deferred; the seam exists so they can be added.

3. **Route permissions to Sightline.** A tool call needing approval appears where
   you are, and your answer goes back over the control protocol. This is the
   single capability that makes Sightline something the vendor CLIs are not.

4. **Build the scheduler.** Task state already exists and is the state machine;
   what is missing is the thing that advances it without asking a model.

5. **Make the Super Chief a conversation without hands.** It talks to you and
   emits structured intent — assignments, priorities, escalations — validated
   against a schema. The kernel starts the workers, under the ceilings, in
   worktrees.

6. **Give the chief the accumulated record.** Today it reads the same repository
   you can read, which is why it has nothing to add. Sightline holds every task
   this project has run, what failed and how, which refutations have ever fired,
   what notes were left, what it all cost — and hands the chief a task list. A
   supervisor whose edge is memory across sessions is doing something you
   genuinely cannot; one that rewords your sentence is not.

7. **Foreman becomes kernel.** No session, no model, no tokens.

8. **Sightline becomes the front door**, with the Hub one turn away. Both in one
   program: two programs means two copies of session state and a synchronisation
   problem.

9. **Demote the compatibility layer.** Reading other people's transcripts,
   scraping panes, the Aider adapter — park rather than maintain. If Sightline
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
   under Sightline's scheduler, with permissions routed to Sightline, on a
   subscription" is roughly three weeks. "An agent OS with crazy amounts of
   ability" is unbounded. The difference between those two is whether this ships.

2. **Does a native API engine exist in the plan at all**, or is delegating the
   turn to a vendor binary the permanent architecture? This is the most important
   open question, and the control-protocol result moved it: permissions and
   interrupt no longer require going native, so the remaining reasons are model
   choice and owning the scheduler. Owning the model turn costs an edit tool and
   context management — years of accumulated tuning, and most of the quality gap.
   Owning the session costs neither.

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
end-to-end under Sightline's control, then generalise the control into a kernel.

**The premise is still untested.** Nothing has shown that supervised
orchestration beats one person driving the same agents by hand. That is Layer 7
in `PLATFORM.md` — a fortnight of measured use — and it has never been run.
Everything above Layer 4 rests on it. Building a kernel on the assumption raises
the stakes on finding out.

**Quota is the binding constraint, not capability or money.** Subscription usage
is not a separate pool: a fleet and the session you are typing in draw from the
same allowance. This is observed, not predicted — the one real chief run in this
project died mid-flight with "You've hit your session limit · resets 11:50pm".
It was not a design failure, it was the plan's ceiling.

That promotes two items above. Ceilings stop being a safety feature and become
the thing that stops one project eating a day's allowance. And model-per-role
stops being an optimisation: a foreman reading exit codes does not need a
frontier model, and the difference between running a whole fleet on the largest
model and running only the judgement on it is the difference between a few hours
and a day.

**Nothing here requires API keys.** The subscription already powers Sightline
today — `--owned` sessions run `claude -p` as you. What was missing was never
model access; it was control over the session, which the protocol above
provides.

**Three prior conclusions in this project were confidently wrong** and were only
caught by driving the real thing: that `--allowedTools` restricts (it grants),
that `--permission-prompt-tool` does not exist (it does, hidden from `--help`),
and that stream-json has no permission seam (it has a whole control protocol).
All three came from reading help text and watching behaviour rather than
exercising the interface. Any claim in this document that has not been exercised
should be treated the same way — including the parts of the control protocol
that have only been read out of the binary.
