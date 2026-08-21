//! What Ironsight depends on in other people's files, pinned down.
//!
//! Everything Ironsight knows about a session comes from artifacts nobody
//! documents: Claude Code's transcript and session registry, Aider's chat
//! history, the shape of a permission prompt on screen. Any of them can change
//! in a release, and when one does the failure is quiet — a status that reads
//! wrong, a prompt nobody is told about, a cost of zero.
//!
//! These fixtures are real records, trimmed and anonymised. They are not here
//! to test the parsers, which have their own tests; they are here so that when
//! an agent changes what it writes, something says so out loud and names the
//! thing that moved.

use std::path::PathBuf;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn read(name: &str) -> String {
    std::fs::read_to_string(fixture(name)).expect("fixture should be readable")
}

#[test]
fn claude_code_still_writes_a_transcript_scope_can_read() {
    let mut session = ironsight_core::session::Session::open(fixture("claude-transcript.jsonl"));
    session.backfill();

    assert_eq!(
        session.cwd, "/home/someone/api",
        "the working directory is read from a user record"
    );
    assert_eq!(session.branch, "main", "as is the branch");
    assert_eq!(
        session.model, "claude-opus-5",
        "the model from an assistant record"
    );
    assert_eq!(
        session.title, "the rate limiter one",
        "a chosen title outranks the derived one"
    );
    assert!(session.titled, "and Ironsight knows a person chose it");
    assert_eq!(
        session.turns, 1,
        "a turn is counted from the record Claude Code writes when one ends"
    );

    // Cost and context come from the usage block, and a rename in what it is
    // called would show up here as a session that costs nothing.
    assert_eq!(session.totals.output, 240);
    assert_eq!(session.totals.cache_read, 48_000);
    assert_eq!(session.totals.cache_write, 6_200);
    assert!(
        session.totals.cost > 0.0,
        "usage was priced, so the model id is still one Ironsight knows"
    );

    // Tool calls, their results, and the plan.
    assert_eq!(session.tools.get("Bash"), Some(&1), "a tool call was seen");
    // The plan comes from the tool *result*, under `newTodos` — not from the
    // call — which is the sort of thing that is obvious only once.
    assert_eq!(session.todos.len(), 2, "the plan was read back");
    assert_eq!(session.todos[1].status, "in_progress");
    assert!(
        session.events.len() >= 4,
        "prompt, reply, call and result all reached the feed"
    );
}

#[test]
fn the_session_registry_still_says_what_is_running() {
    let text = read("claude-registry.json");
    let record: serde_json::Value = serde_json::from_str(&text).expect("still JSON");
    // Each of these is a field Ironsight reads by name. A rename breaks liveness,
    // which is what everything else hangs off.
    for field in [
        "pid",
        "sessionId",
        "cwd",
        "version",
        "procStart",
        "status",
        "kind",
    ] {
        assert!(
            record.get(field).is_some(),
            "the registry no longer has `{field}`, which Ironsight reads to know a \
             session is alive and what it is doing"
        );
    }
    assert_eq!(
        record["status"], "busy",
        "status is still a word, not a code"
    );
}

#[test]
fn a_permission_prompt_is_still_recognisable_on_screen() {
    let screen = read("claude-permission-screen.txt");
    let asking = ironsight_core::control::pending_approval(&screen)
        .expect("Claude Code draws a prompt Ironsight can no longer see");
    assert_eq!(asking.question, "Do you want to proceed?");
    assert_eq!(asking.options.len(), 3);
    assert_eq!(asking.keys, vec!["1", "2", "3"], "answered by number");
}

#[test]
fn aider_still_asks_in_letters_and_answers_to_them() {
    let screen = read("aider-permission-screen.txt");
    let asking = ironsight_core::control::pending_approval(&screen)
        .expect("Aider draws a prompt Ironsight can no longer see");
    assert!(asking.question.contains("gitignore"));
    assert_eq!(asking.options, vec!["Yes", "No"]);
    assert_eq!(
        asking.keys,
        vec!["y", "n"],
        "answered by letter, not position"
    );
}

#[test]
fn aider_still_records_what_it_did_and_what_it_cost() {
    use ironsight_core::agent::aider::{Line, read_line};
    let history = read("aider-history.md");
    let lines: Vec<Line> = history.lines().map(read_line).collect();

    assert!(
        lines
            .iter()
            .any(|l| matches!(l, Line::Asked(a) if a.contains("docstring"))),
        "what a person asked is still written with ####"
    );
    assert!(
        lines
            .iter()
            .any(|l| matches!(l, Line::Model(m) if m.contains("qwen2.5-coder"))),
        "the model is still announced"
    );
    assert!(
        lines.iter().any(|l| *l
            == Line::Tokens {
                sent: 788,
                received: 80
            }),
        "token counts are still written after each exchange"
    );
    assert!(
        lines
            .iter()
            .any(|l| matches!(l, Line::Cost { message, .. } if (*message - 0.0021).abs() < 1e-9)),
        "and so is the cost"
    );
}
