//! The brief that turns a session into a supervisor.
//!
//! A chief is not a new runtime. It is a session with `sightline` on its path, a
//! brief, and a ceiling it cannot raise — which is exactly what a worker is,
//! one level up. The recursion is the point: a chief is a session Sightline
//! manages, managing sessions Sightline manages, and everything it does shows
//! up in the same list and the same stream as everything else.
//!
//! What is written here is Sightline's opinion about supervision, so it lives in
//! the binary rather than in your repository. What is yours — mission,
//! architecture, constraints, what done means — comes from the constitution and
//! is quoted into the brief beside it. The chief is told what it may do by this
//! file, told what the project is by that one, and *prevented* from exceeding
//! the fleet's ceilings by `limits`, which is the only one of the three that
//! does not depend on it reading carefully.
//!
//! Three rules here are not advice, and they are stated as prohibitions because
//! a supervisor that breaks them is worse than no supervisor:
//!
//! - It does not answer permission prompts. The moment a supervisor answers
//!   them, its blast radius is everything a permission protects.
//! - It does not restart a stalled session. From outside, thinking and wedged
//!   are identical, and restarting a session that was thinking throws away work
//!   and pays for it twice.
//! - It does not write code, and it cannot: an owned chief is started with the
//!   editing tools denied, so this one is enforced rather than trusted.

use crate::brief::Constitution;
use crate::limits::Limits;
use crate::work;

/// Tools a chief is started without.
///
/// A chief that edits files is a worker with extra privileges and no checks
/// against it: nothing refutes its work, because the work is nobody's task.
/// Denied at the agent rather than asked for in prose, because prose is not a
/// guarantee.
pub const DENIED: &[&str] = &["Write", "Edit", "NotebookEdit"];

/// The person's home directory, if the platform will say where it is.
///
/// Deliberately not the filesystem root. A supervisor is given the reach its
/// person has, not more — and `cannot tell` is an answer, so a platform that
/// will not say leaves the chief with the directory it started in rather than
/// with everything.
fn home() -> Option<String> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(|h| h.to_string_lossy().into_owned())
        .filter(|h| !h.is_empty())
}

/// What a chief is, in one place.
///
/// This function exists because both front ends used to build this themselves,
/// and both built the same three things wrong: no kernel tools, so the chief
/// could not ask for a worker; no policy, so no `--permission-prompt-tool` and
/// therefore no boundary at all; and a grant list, which would have been a hole
/// in that boundary had there been one. A live chief run against a real
/// codebase got as far as reading it, writing the assignments, and reporting
/// that the mechanism to dispatch them did not exist. It was right.
///
/// The configuration below is not new. It is what `examples/chief_live.rs`
/// already used — the one that was run end to end and came back with the right
/// answer — so what ships and what was proved are now the same code rather than
/// two descriptions of it that drifted.
///
/// Three properties, each of which was individually absent:
///
/// - `kernel_tools`, which is the whole of a supervisor. Without it a chief has
///   a brief describing tools it does not have.
/// - a `policy`, which is what routes every call to `gate::decide`. A chief
///   without one is not supervised by anything.
/// - an empty `allow`, because a granted tool does not prompt and what does not
///   prompt never reaches the gate. With a policy attached, `owned::argv` drops
///   grants anyway; leaving them here would only mislead the next reader.
pub fn spec(model: Option<&str>, opening: &str, project: &std::path::Path) -> crate::owned::Spec {
    let mut policy = crate::gate::Policy::confined_to(project);
    // A supervisor is the one session that must not be able to talk its way
    // past the fleet's size and spend, since it is the one asking for more.
    policy.ceilings = true;
    crate::owned::Spec {
        // Always Claude Code. A chief holds the kernel's own tools — assign,
        // fleet, tell — and those are served over the control protocol's
        // in-process MCP server, which is a seam only this agent exposes. A
        // chief on another vendor could still be governed; it could not ask for
        // a worker, which is the whole of what a chief does.
        agent: "claude".into(),
        model: model.map(str::to_string),
        // A chief thinks and does not type. Its whole output is decisions —
        // which work, in what order, split how — and those are the decisions
        // every worker's cost then follows from. This is the one session in a
        // fleet where effort pays for itself many times over, so it is left at
        // whatever the account's default is rather than cheapened.
        effort: None,
        // Nothing pre-approved: every call is a question, and the kernel
        // answers it.
        mode: None,
        allow: Vec::new(),
        // A chief does not write code. Denied at the agent, not asked for.
        deny: DENIED.iter().map(|s| s.to_string()).collect(),
        opening: Some(opening.to_string()),
        policy: Some(policy),
        // The reach a person has when they start a session in their own home
        // directory. A chief starts in the project so that its constitution and
        // ceilings are found there, and the work it supervises is very often
        // somewhere else — which is exactly what happened the first time one ran
        // for real. Widening what Claude Code will let a tool touch does not
        // widen the boundary: every call still arrives at `gate::decide`, and
        // the scope kernel still confines writes to the root above.
        reach: home().into_iter().collect(),
        // The whole point: it can ask for a worker, and cannot start one.
        kernel_tools: true,
    }
}

/// What the chief is told when it starts.
///
/// `intent` is the paragraph from the person: the thing to be done, in their
/// words, unparaphrased. Everything else is context it would otherwise have to
/// go and find, and a supervisor whose first three turns are `ls` is a
/// supervisor you are paying to orient itself.
pub fn brief(
    intent: &str,
    cwd: &str,
    constitution: Option<&Constitution>,
    limits: &Limits,
    store: &work::Store,
) -> String {
    let mut out = String::new();

    out.push_str(
        "You are the chief of a fleet of coding agents, driven through a tool called\n\
         Sightline. You decide what work is to be done and by whom, you check that it\n\
         was actually done, and you report back. You do not write the code.\n\n",
    );

    out.push_str("WHAT IS WANTED\n");
    for line in intent.trim().lines() {
        out.push_str(&format!("  {line}\n"));
    }
    out.push('\n');

    if let Some(c) = constitution {
        if !c.mission.is_empty() {
            out.push_str(&format!("MISSION\n  {}\n\n", one_line(&c.mission)));
        }
        if !c.architecture.is_empty() {
            out.push_str(&format!(
                "ARCHITECTURE\n  {}\n\n",
                one_line(&c.architecture)
            ));
        }
        if !c.constraints.is_empty() {
            out.push_str("STANDING CONSTRAINTS\n");
            for line in &c.constraints {
                out.push_str(&format!("  {line}\n"));
            }
            out.push('\n');
        }
        if !c.done.is_empty() {
            out.push_str("DONE MEANS, IN THIS PROJECT\n");
            for line in &c.done {
                out.push_str(&format!("  {line}\n"));
            }
            out.push('\n');
        }
        if !c.rejected.is_empty() {
            out.push_str("ALREADY REJECTED — do not propose these again\n");
            for line in &c.rejected {
                out.push_str(&format!("  {line}\n"));
            }
            out.push('\n');
        }
    } else {
        out.push_str(
            "There is no constitution for this project. Nothing has been written down\n\
             about its mission, its constraints, or what done means, so do not assume\n\
             any. If a decision needs one, ask rather than inventing it.\n\n",
        );
    }

    let routes = crate::routing::load(std::path::Path::new(cwd));
    if !routes.routes.is_empty() {
        out.push_str(
            "HOW WORK IS ROUTED HERE\n\
             \x20 Somebody wrote these down for this project. Ask for one by name when\n\
             \x20 you assign — `route: \"mechanical\"` — and it settles the agent, the\n\
             \x20 model and the effort together. Naming any of those yourself overrides\n\
             \x20 the route, which is worth doing deliberately and not by habit.\n\n",
        );
        out.push_str(&routes.describe());
        out.push('\n');
    }

    out.push_str(&format!("WHERE\n  {cwd}\n"));
    out.push_str(
        "\x20 Where you start, not where you are confined. You can read and run\n\
         \x20 commands anywhere under the home directory, and you can assign a worker\n\
         \x20 to any directory on the machine — a worker is confined to the one it is\n\
         \x20 given, which is the point of giving it one. If the work is somewhere\n\
         \x20 else, go there; do not report yourself stuck in this folder.\n\n",
    );

    out.push_str("CEILINGS\n");
    out.push_str(&format!("  {}\n", limits.describe()));
    out.push_str(
        "\x20 Sightline enforces these. A start that would exceed one fails and tells\n\
         \x20 you so; there is nothing you can do about it from here, and nothing you\n\
         \x20 need to do about it except plan within them. If they are too tight for\n\
         \x20 the work, say so in your report — do not work around them.\n\n",
    );

    out.push_str(&fleet(store));

    out.push_str(
        "HOW TO WORK\n\
         \x20 You do not start processes. Sightline does, when you ask it to, and it\n\
         \x20 gives you three tools for the purpose:\n\
         \n\
         \x20   assign(path, task)   start a worker on one assignment\n\
         \x20   fleet()              every worker, whether it is busy, what it is doing\n\
         \x20   tell(who, text)      say something to a worker you started\n\
         \n\
         \x20 This is not a formality. A worker Sightline starts is confined to its\n\
         \x20 directory, counted against the ceilings, and stopped when the fleet is\n\
         \x20 stopped. One you started yourself would be none of those things, so\n\
         \x20 there is no way to start one and no reason to look for one.\n\
         \n\
         \x20 A worker cannot start workers, but it is a Claude Code session and its\n\
         \x20 own Agent tool is not restricted — it can launch subagents freely, and\n\
         \x20 those do not count against the ceiling. So say in the assignment when\n\
         \x20 work parallelises: a sweep of the same question across many files, or\n\
         \x20 several independent checks of one claim. Not when it is sequential.\n\
         \n\
         \x20 WHAT A FLEET COSTS, BECAUSE IT IS NOT WHAT ANYONE EXPECTS\n\
         \x20 Every turn re-sends the whole conversation so far. So a session costs\n\
         \x20 roughly its number of turns times its average context — it grows with\n\
         \x20 the SQUARE of how long the session runs, not with how much it writes.\n\
         \x20 Measured on one real supervised project: 924,000 tokens written against\n\
         \x20 61,500,000 re-read. Sixty-seven to one.\n\
         \n\
         \x20 Three things follow, and they are the whole of running a fleet cheaply:\n\
         \n\
         \x20 One long session is far more expensive than two short ones doing the\n\
         \x20 same work. Splitting a hundred-turn task in half costs about half as\n\
         \x20 much. Size an assignment to something finishable in forty turns or so.\n\
         \n\
         \x20 What a worker has to discover, it then carries for the rest of its life.\n\
         \x20 If you have already read the thing it needs, quote it into the\n\
         \x20 assignment. A paragraph you paste costs a few hundred tokens once; the\n\
         \x20 same paragraph found by a worker reading a large file costs thousands,\n\
         \x20 on every turn it takes afterwards.\n\
         \n\
         \x20 Set effort deliberately. `assign` takes it. Applying a change somebody\n\
         \x20 has already decided is low; working out what the change should be is\n\
         \x20 high. Reasoning tokens become context, and context is re-read, so effort\n\
         \x20 compounds rather than costing once.\n\
         \n\
         \x20 fleet() tells you what each worker has spent and how fast its context is\n\
         \x20 growing. A worker past about 120k with a long way to go should be told\n\
         \x20 to finish and hand over rather than left to run.\n\
         \n\
         \x20 An assignment is a sentence a stranger could act on. \"Fix the tests\" is\n\
         \x20 not one. Say what is to be true afterwards. The worker sees the task and\n\
         \x20 nothing else — not this brief, not the constitution, not what you were\n\
         \x20 asked. Everything it needs has to be in the sentence you write.\n\n",
    );

    out.push_str(
        "WHAT COUNTS AS DONE\n\
         \x20 A worker saying it has finished is worth nothing on its own. Work reaches\n\
         \x20 Claimed when the worker says so, Checked when the project's checks pass,\n\
         \x20 and Verified only when something written to show the work wrong was run,\n\
         \x20 did not fire, and has been seen to fire at some point. Do not report work\n\
         \x20 as done on a worker's word. Run `sightline check`.\n\n",
    );

    out.push_str(
        "WHAT YOU MUST NOT DO\n\
         \x20 - Do not answer a permission prompt on anyone's behalf. If a session is\n\
         \x20   blocked on one, say so in your report and leave it blocked.\n\
         \x20 - Do not restart a stalled session. From outside, thinking and wedged look\n\
         \x20   identical, and a restart throws away work and pays for it twice. Report\n\
         \x20   the stall.\n\
         \x20 - Do not write or edit code. Those tools are denied to you, so attempting\n\
         \x20   it wastes a turn.\n\
         \x20 - Do not decide anything the constitution says is not yours to decide.\n\n",
    );

    out.push_str(
        "ESCALATE, RATHER THAN DECIDING, WHEN\n\
         \x20 - the work needs a decision the constitution does not cover\n\
         \x20 - a worker is blocked on a permission, or stalled\n\
         \x20 - checks fail twice on the same task for the same reason\n\
         \x20 - what was asked for turns out to be ambiguous enough that two readings\n\
         \x20   lead to materially different work\n\
         \x20 Escalating means saying so plainly in your reply, naming the choice and\n\
         \x20 what you would do. It is not a failure; it is the job.\n\n",
    );

    out.push_str(
        "NOW\n\
         \x20 Read the fleet, decide what work is needed, and say what you intend to do\n\
         \x20 before you do it. Then do it, and report: what was assigned, what was\n\
         \x20 verified, what failed, what needs a person.\n",
    );

    out
}

/// The fleet as it stands, so the chief does not have to ask for it first.
fn fleet(store: &work::Store) -> String {
    let mut out = String::from("THE FLEET AS IT STANDS\n");
    let mut any = false;
    for task in store.tasks() {
        any = true;
        let short = &task.session[..task.session.len().min(8)];
        out.push_str(&format!(
            "  {} · {} · {} · {}\n",
            task.id,
            task.state.label(),
            short,
            one_line(&task.assignment)
        ));
    }
    if !any {
        out.push_str("  Nothing is assigned. This is a fresh start.\n");
    }
    out.push('\n');
    out
}

/// Collapse a paragraph to one line, for a place that has room for one.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::work::State;

    fn a_constitution() -> Constitution {
        Constitution::parse(
            "# Constitution\n\
             ## Mission\n\
             Ship a small, correct thing.\n\
             ## Constraints\n\
             - Never break the public API.\n\
             ## Rejected\n\
             - An ORM, because the queries are the point.\n\
             ## Done means\n\
             - The suite passes and someone else has read it.\n",
        )
    }

    #[test]
    fn the_brief_carries_the_intent_unparaphrased() {
        // The whole reason this is assembled rather than asked for: intent
        // paraphrased on the way in is intent you can no longer trust.
        let intent = "Make the importer handle files with no trailing newline.";
        let out = brief(
            intent,
            "/w",
            None,
            &Limits::default(),
            &work::Store::default(),
        );
        assert!(out.contains(intent), "the words are the person's: {out}");
    }

    #[test]
    fn the_brief_carries_what_the_project_has_already_decided() {
        let c = a_constitution();
        let out = brief(
            "Add an importer.",
            "/w",
            Some(&c),
            &Limits::default(),
            &work::Store::default(),
        );
        assert!(out.contains("Ship a small, correct thing."), "the mission");
        assert!(
            out.contains("Never break the public API."),
            "the constraints"
        );
        assert!(
            out.contains("An ORM, because the queries are the point."),
            "and what has already been ruled out, so it is not proposed again"
        );
        assert!(
            out.contains("The suite passes and someone else has read it."),
            "and what done means here"
        );
    }

    #[test]
    fn a_project_with_nothing_written_down_is_said_so_rather_than_guessed() {
        let out = brief(
            "Do the thing.",
            "/w",
            None,
            &Limits::default(),
            &work::Store::default(),
        );
        assert!(
            out.contains("no constitution"),
            "an absent constitution is stated, not filled in with assumptions: {out}"
        );
    }

    #[test]
    fn the_ceilings_are_stated_as_something_it_cannot_change() {
        let limits = Limits {
            sessions: Some(3),
            spend: Some(10.0),
            window: Some(24),
        };
        let out = brief(
            "Do the thing.",
            "/w",
            None,
            &limits,
            &work::Store::default(),
        );
        assert!(out.contains("at most 3 sessions of its own running"));
        assert!(out.contains("at most $10.00"));
        assert!(
            out.contains("Sightline enforces these"),
            "and that it is not being asked to respect them: {out}"
        );
    }

    #[test]
    fn the_three_prohibitions_are_all_there() {
        // Each of these is a way a supervisor makes things worse rather than
        // better, and each has to be said: none of them is obvious from the
        // outside, and two of them look helpful.
        let out = brief(
            "Do the thing.",
            "/w",
            None,
            &Limits::default(),
            &work::Store::default(),
        );
        assert!(out.contains("Do not answer a permission prompt"));
        assert!(out.contains("Do not restart a stalled session"));
        assert!(out.contains("Do not write or edit code"));
    }

    #[test]
    fn a_chief_is_born_able_to_act_and_supervised_while_it_does() {
        // Each assertion here is a thing that was actually false in the shipped
        // binary while this module's prose described it as true. A live chief
        // read its brief, went looking for the tools the brief promised, found
        // none, and reported the mission undispatchable. Assert the properties
        // that failed, not the shape of the struct.
        let dir = std::env::temp_dir();
        let spec = spec(None, "the brief", &dir);

        assert!(
            spec.kernel_tools,
            "a supervisor that cannot ask for a worker is not a supervisor"
        );
        let policy = spec
            .policy
            .as_ref()
            .expect("without a policy no call reaches gate::decide, so nothing is supervised");
        assert!(
            policy.ceilings,
            "the one session that asks for more sessions is the one that must not raise the ceiling"
        );
        assert!(
            spec.allow.is_empty(),
            "a granted tool does not prompt, and what does not prompt never reaches the gate"
        );
        for denied in DENIED {
            assert!(
                spec.deny.iter().any(|d| d == denied),
                "{denied} must be refused at the agent, not asked for in prose"
            );
        }
        assert!(
            spec.mode.is_none(),
            "a permission mode approves calls before Sightline is asked about them"
        );
        // The reach a person has. A chief pinned to the folder the window
        // happened to be pointed at reported the mission undispatchable while
        // the codebase it had been given sat one directory away.
        if let Some(home) = std::env::var_os("HOME") {
            let home = home.to_string_lossy().into_owned();
            assert!(
                spec.reach.contains(&home),
                "a supervisor is given its person's reach, not a narrower one"
            );
        }

        // And the flags that carry all of it. The struct being right is worth
        // nothing if the command line does not say so.
        let argv = crate::owned::argv(&spec);
        assert!(
            argv.iter().any(|a| a == "--permission-prompt-tool"),
            "the boundary is this flag; without it the policy above is decoration"
        );
        assert!(
            argv.iter().any(|a| a == "--mcp-config"),
            "the kernel's own tools are callable only when the server is declared"
        );
        assert!(
            !argv.iter().any(|a| a == "--allowedTools"),
            "a grant on the command line is a hole in the boundary, silently"
        );
        if std::env::var_os("HOME").is_some() {
            assert!(
                argv.iter().any(|a| a == "--add-dir"),
                "reach is this flag; without it the field above is decoration"
            );
        }
    }

    #[test]
    fn the_chief_is_told_to_ask_rather_than_to_run_something() {
        let brief = brief(
            "get the thing done",
            "/tmp/x",
            None,
            &Limits::default(),
            &work::Store::default(),
        );
        assert!(
            brief.contains("assign(path, task)"),
            "the way to create work has to be in the brief"
        );
        assert!(
            !brief.contains("sightline new"),
            "a chief told to shell out is the copy-paster this replaced"
        );
        assert!(
            brief.contains("cannot start workers"),
            "the depth of the tree is a fact it should not have to discover"
        );
    }

    #[test]
    fn editing_is_denied_at_the_agent_not_only_asked_for() {
        // The prohibition above is prose, and prose is not a guarantee. This is
        // the half that is.
        assert!(DENIED.contains(&"Write"));
        assert!(DENIED.contains(&"Edit"));
    }

    #[test]
    fn the_fleet_is_handed_over_rather_than_gone_looking_for() {
        let mut store = work::Store::default();
        let id = store.assign("abcdef1234", "make the importer handle empty files");
        let _ = store.set_state(&id, State::Claimed);
        let out = brief("Carry on.", "/w", None, &Limits::default(), &store);
        assert!(
            out.contains("make the importer handle empty files"),
            "the assignment is in the brief: {out}"
        );
        assert!(
            out.contains("claimed"),
            "and where it got to, so the first turn is not spent asking"
        );
        assert!(
            out.contains("abcdef12"),
            "and which session, shortened the way everything else shortens it"
        );
    }

    #[test]
    fn an_empty_fleet_says_so() {
        let out = brief(
            "Start something.",
            "/w",
            None,
            &Limits::default(),
            &work::Store::default(),
        );
        assert!(out.contains("Nothing is assigned"));
    }
}
