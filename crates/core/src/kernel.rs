//! What a supervisor may ask the kernel to do.
//!
//! A chief that starts its own processes is outside everything: no ceiling is
//! consulted, no policy is attached, and the thing it started is not in the
//! fleet, so stopping the fleet does not stop it. The supervision would be a
//! story told in a prompt.
//!
//! So a supervisor does not start processes. It calls these, and the kernel
//! starts them — which means every worker that exists came through the same
//! door as every other, wearing a policy the kernel chose. `docs/probes/`
//! shows the model will actually reach for a tool served this way.
//!
//! The tools are deliberately few. Each one is a thing a supervisor genuinely
//! cannot do for itself, and nothing here is a convenience.

use crate::gate::Policy;
use crate::owned::{self, Spec};
use serde_json::{Value, json};

/// How long to wait for a new worker to name its transcript before answering.
///
/// Short: the answer is useful without it, and the reader thread fills the id in
/// moments later either way.
const SETTLE: std::time::Duration = std::time::Duration::from_millis(1500);

/// The tools, as the session is told about them.
pub fn schemas() -> Vec<Value> {
    vec![
        json!({
            "name": "assign",
            "description": "Start a worker on one task. The worker is confined to \
                            `path`, cannot start workers of its own, and is subject \
                            to the same ceilings you are. Returns the name to watch.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string",
                             "description": "the directory the worker owns and may write in"},
                    "task": {"type": "string",
                             "description": "what it is to do, in full — it sees nothing else"},
                    "model": {"type": "string",
                              "description": "optional; the kernel picks a sensible default"},
                },
                "required": ["path", "task"],
            },
        }),
        json!({
            "name": "fleet",
            "description": "What Sightline is running right now: every worker, whether \
                            it is busy, and what it is doing.",
            "inputSchema": {"type": "object", "properties": {}},
        }),
        json!({
            "name": "claim",
            "description": "Say the work you were assigned is finished. Sightline runs                             the project's checks and its refutations and tells you what                             they actually showed. Saying you are done does not make it                             so, and this is how you find out.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "summary": {"type": "string",
                                "description": "what you did, in one or two sentences"},
                },
                "required": ["summary"],
            },
        }),
        json!({
            "name": "tell",
            "description": "Say something to a worker you started — an answer, a \
                            correction, or more of the task.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "who": {"type": "string"},
                    "text": {"type": "string"},
                },
                "required": ["who", "text"],
            },
        }),
    ]
}

/// Run one, by the name the session used.
///
/// The name arrives bare — the server prefix is stripped before it gets here —
/// but not always, and a supervisor that guessed the long form should not be
/// told the tool does not exist.
pub fn call(session: &str, name: &str, args: &Value) -> Result<String, String> {
    let bare = name
        .rsplit("__")
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(name);
    match bare {
        "assign" => assign(session, args),
        "fleet" => Ok(fleet()),
        "tell" => tell(args),
        "claim" => claim(session, args),
        other => Err(format!("{other} is not one of Sightline's tools")),
    }
}

/// Start a worker under the kernel's terms rather than the caller's.
///
/// Three of those terms are not negotiable and none of them are in the schema,
/// because a supervisor asking for a worker should not be able to ask for a less
/// constrained one:
///
/// - it is recorded as this session's, so the shape of a project survives the
///   sessions that made it;
/// - it is confined to `path`, with the ceilings in force there;
/// - it gets no kernel tools, so it cannot start workers of its own — the tree
///   stays one deep until somebody decides otherwise;
/// - the ceiling is consulted *before* it starts, not only once it is running,
///   because a session that starts and is then refused every call has already
///   spent the thing the ceiling was protecting;
/// - it is given no permission mode, so every call prompts and every prompt is
///   this kernel. `acceptEdits` would be the obvious kindness and it is the one
///   thing that must not be done: it approves writes before Sightline is asked,
///   which blinds the scope kernel to exactly the calls it exists for.
fn assign(asked_by: &str, args: &Value) -> Result<String, String> {
    let path = args
        .get("path")
        .and_then(Value::as_str)
        .ok_or("assign needs a path")?;
    let task = args
        .get("task")
        .and_then(Value::as_str)
        .ok_or("assign needs a task")?;
    let root = std::path::PathBuf::from(crate::app::expand(path));
    if !root.is_dir() {
        return Err(format!("{} is not a directory", root.display()));
    }

    let policy = Policy::confined_to(&root);
    // The door, which is where the count ceiling belongs: this is the one moment
    // the question "may another session exist" is actually being asked. The gate
    // asks about spend on every call afterwards; it does not ask this one.
    let limits = crate::limits::in_force(&root).map_err(|e| format!("the ceilings: {e}"))?;
    let spent = match limits.spend {
        Some(_) => crate::limits::spent_since(
            &crate::app::data_dir().join("events.jsonl"),
            limits.window_hours(),
        ),
        None => 0.0,
    };
    if let Some(why) = crate::limits::refuse(&limits, owned::running(), spent) {
        return Err(why);
    }

    let spec = Spec {
        model: args
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
        mode: None,
        allow: Vec::new(),
        deny: Vec::new(),
        // Started silent on purpose. The `task` kernel refuses a write from a
        // session with no assignment on record, and the record cannot be written
        // until the session has a name — so the task is handed over *after* it is
        // written down. Given as the opening message instead, the worker could
        // reach its first write before its own assignment existed and be refused
        // for something Sightline had not finished doing.
        opening: None,
        policy: Some(policy.on_assigned_work()),
        // No reach beyond `path`, and this is the difference between a worker
        // and the chief that asked for it. A supervisor needs to see across the
        // machine to decide anything; a worker has one assignment in one
        // directory, and everything Sightline claims about confinement is this
        // line.
        reach: Vec::new(),
        kernel_tools: false,
    };
    let started = owned::start("claude", &root, &spec, SETTLE)?;
    let mut store = crate::work::Store::load(crate::work::path_in(&crate::app::data_dir()));
    let id = store.assign(&started.name, task);
    // Who asked. Written here and nowhere else, because here is the only place
    // that knows: a worker's task records what it was told to do, and without
    // this it does not record that anybody told it. The fleet then has no tree
    // — every task an unrelated root — and nothing downstream can say how a
    // project was distributed, because the fact was never kept.
    store.record_lineage(&started.name, asked_by);
    store.save().map_err(|e| {
        format!(
            "{} started but its task could not be recorded: {e}",
            started.name
        )
    })?;
    // Only now.
    owned::say(&started.name, task)?;
    Ok(format!(
        "started {} in {} on task {id}. It is confined to that directory and \
         cannot start workers of its own. Watch it with the fleet tool; speak to \
         it with tell.",
        started.name,
        root.display()
    ))
}

fn fleet() -> String {
    let all = owned::list();
    if all.is_empty() {
        return "nothing is running".into();
    }
    let mut out = String::new();
    for o in all {
        out.push_str(&format!(
            "{} · {} · {} · {}\n",
            o.name,
            if o.alive { "running" } else { "ended" },
            if o.busy {
                if o.tool.is_empty() {
                    "thinking".to_string()
                } else {
                    o.tool.clone()
                }
            } else {
                "idle".into()
            },
            o.cwd,
        ));
    }
    out
}

/// The ladder, reached by the session that did the work.
///
/// A worker used to have to be *asked* whether it was done, and the answer was
/// its own opinion. This is the same verdict the commands reach — one
/// implementation in `ladder`, because two definitions of finished is two
/// definitions of finished — delivered to the one party with a reason to argue
/// with it.
fn claim(session: &str, args: &Value) -> Result<String, String> {
    let summary = args
        .get("summary")
        .and_then(Value::as_str)
        .ok_or("claim needs a summary of what you did")?;
    let here = owned::get(session)
        .map(|o| std::path::PathBuf::from(o.cwd))
        .ok_or("this session is not one Sightline is holding")?;

    let path = crate::work::path_in(&crate::app::data_dir());
    let mut store = crate::work::Store::load(path);
    if let Some(task) = store.task_for(session).map(|t| t.id.clone()) {
        let _ = store.note(&task, &format!("claimed: {summary}"));
        let _ = store.set_state(&task, crate::work::State::Claimed);
        store.flush();
    }

    let report = crate::ladder::adjudicate(&mut store, session, &here)?;
    let mut out = report.reached.say();
    if !report.tried.is_empty() {
        out.push_str("\n\nwhat was tried against it:\n");
        for line in &report.tried {
            out.push_str(&format!("  {line}\n"));
        }
    }
    if !report.reached.good() {
        out.push_str(
            "\nThe task is back to working. Fix what this found rather than claiming \
             again, and do not argue with it — the commands are the project's, not mine.",
        );
    }
    Ok(out)
}

fn tell(args: &Value) -> Result<String, String> {
    let who = args
        .get("who")
        .and_then(Value::as_str)
        .ok_or("tell needs a worker")?;
    let text = args
        .get("text")
        .and_then(Value::as_str)
        .ok_or("tell needs something to say")?;
    owned::say(who, text)?;
    Ok(format!("said to {who}"))
}

#[cfg(test)]
mod tests {

    #[test]
    fn an_assignment_records_who_asked_for_it() {
        // The defect this was written for: `assign` started a worker, wrote its
        // task, and never recorded the chief that asked. Every task in the store
        // was a root, the fleet had no tree, and nothing downstream could say
        // how a project had been distributed — because the fact had never been
        // kept. It is unrecoverable after the fact: the only moment anybody
        // knows who asked is the moment they ask.
        //
        // Asserted on the source rather than by running `assign`, which starts
        // a process and spends quota. The thing being protected is that the
        // call exists on that path at all.
        let source = include_str!("kernel.rs");
        let body = &source[source.find("fn assign(").unwrap()..];
        let body = &body[..body.find("\n}").unwrap()];
        assert!(
            body.contains("record_lineage"),
            "a worker with no recorded parent is a project with no shape"
        );
        assert!(
            body.contains("asked_by"),
            "the parent recorded has to be the session that asked, not a guess"
        );
    }

    #[test]
    fn a_worker_is_confined_and_a_chief_is_not() {
        // The two halves of the same decision, asserted together so that
        // widening one cannot quietly widen the other. A supervisor needs to
        // see across the machine to decide anything; a worker has one
        // assignment in one directory.
        let dir = std::env::temp_dir();
        let chief = crate::chief::spec(None, "the brief", &dir);
        assert!(
            !chief.reach.is_empty() || std::env::var_os("HOME").is_none(),
            "a chief with no reach is the bug this was written for"
        );

        // The worker's spec is built inside `assign`, which starts a process,
        // so assert the rule the way the rest of this file states it: nothing
        // may hand a worker `reach`.
        let source = include_str!("kernel.rs");
        let assign = &source[source.find("fn assign(").unwrap()..];
        let assign = &assign[..assign.find("\n}").unwrap()];
        assert!(
            assign.contains("reach: Vec::new()"),
            "everything Sightline claims about confining a worker is that line"
        );
    }
    use super::*;

    #[test]
    fn the_long_name_and_the_short_one_both_arrive() {
        // Which of the two the tool sees is not documented, and guessing wrong
        // means a supervisor is told its tool does not exist.
        assert!(call("owned-1", "fleet", &json!({})).is_ok());
        assert!(call("owned-1", "mcp__sightline__fleet", &json!({})).is_ok());
        assert!(call("owned-1", "nonsense", &json!({})).is_err());
    }

    #[test]
    fn a_worker_is_never_given_a_mode_that_answers_for_us() {
        // `acceptEdits` reads as a convenience and is a hole: an approved write
        // never prompts, and what never prompts never reaches the gate — so the
        // scope kernel would go blind to precisely the calls it exists to judge.
        let root = std::env::temp_dir();
        let policy = Policy::confined_to(&root);
        let spec = Spec {
            mode: None,
            policy: Some(policy),
            ..Spec::default()
        };
        let args = crate::owned::argv(&spec);
        assert!(
            !args.iter().any(|a| a == "--permission-mode"),
            "a worker that answers its own prompts is not supervised: {args:?}"
        );
        assert!(args.iter().any(|a| a == "--permission-prompt-tool"));
    }

    #[test]
    fn a_worker_cannot_be_asked_for_outside_a_directory() {
        let e = assign(
            "chief",
            &json!({"path": "/no/such/place/here", "task": "x"}),
        )
        .unwrap_err();
        assert!(e.contains("not a directory"), "{e}");
    }

    #[test]
    fn assign_will_not_take_a_task_it_was_not_given() {
        assert!(assign("chief", &json!({"path": "/tmp"})).is_err());
        assert!(assign("chief", &json!({"task": "something"})).is_err());
    }

    #[test]
    fn every_tool_offered_is_a_tool_that_answers() {
        // The refutation for the schema drifting away from the match arm: a tool
        // in the list that nothing implements is a supervisor calling into a
        // hole, and it would look exactly like the model being wrong.
        for tool in schemas() {
            let name = tool["name"].as_str().unwrap();
            let answer = call("owned-1", name, &json!({}));
            assert!(
                !matches!(&answer, Err(e) if e.contains("not one of Sightline's tools")),
                "{name} is offered but not implemented"
            );
        }
    }
}
