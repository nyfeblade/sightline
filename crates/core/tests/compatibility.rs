//! What Sightline depends on in other people's files, pinned down.
//!
//! Everything Sightline knows about a session comes from artifacts nobody
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
    let mut session = sightline_core::session::Session::open(fixture("claude-transcript.jsonl"));
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
    assert!(session.titled, "and Sightline knows a person chose it");
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
        "usage was priced, so the model id is still one Sightline knows"
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
    // Each of these is a field Sightline reads by name. A rename breaks liveness,
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
            "the registry no longer has `{field}`, which Sightline reads to know a \
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
    let asking = sightline_core::control::pending_approval(&screen)
        .expect("Claude Code draws a prompt Sightline can no longer see");
    assert_eq!(asking.question, "Do you want to proceed?");
    assert_eq!(asking.options.len(), 3);
    assert_eq!(asking.keys, vec!["1", "2", "3"], "answered by number");
}

/// Detecting a prompt is half the job; the other half is that choosing an
/// option sends the keystroke that option is answered by. If the mapping from
/// "the second choice" to what gets typed ever broke, detection would still
/// pass and a person answering from one place would silently pick the wrong
/// thing — which is the failure that matters most, because it acts.
#[test]
fn answering_a_prompt_sends_what_that_option_is_answered_by() {
    use sightline_core::control::{keystroke_for, pending_approval};

    // Claude Code: a numbered list, answered by the number.
    let numbered = read("claude-permission-screen.txt");
    let claude = pending_approval(&numbered).expect("a numbered prompt");
    assert_eq!(
        keystroke_for(&claude, 1),
        "1",
        "the first option is answered by typing 1"
    );
    assert_eq!(
        keystroke_for(&claude, 2),
        "2",
        "and the second by 2 — not by its position drifting"
    );

    // Aider: letters in brackets, answered by the letter, not its position.
    let letters = read("aider-permission-screen.txt");
    let aider = pending_approval(&letters).expect("a letter prompt");
    let first_key = aider.keys.first().cloned().unwrap_or_default();
    assert_eq!(
        keystroke_for(&aider, 1),
        first_key,
        "the first option sends its own letter, whatever position it is in"
    );
    assert!(
        first_key.chars().all(|c| c.is_ascii_lowercase()),
        "and a letter prompt is answered by a letter, not a number: {first_key:?}"
    );
}

#[test]
fn aider_still_asks_in_letters_and_answers_to_them() {
    let screen = read("aider-permission-screen.txt");
    let asking = sightline_core::control::pending_approval(&screen)
        .expect("Aider draws a prompt Sightline can no longer see");
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
    use sightline_core::agent::aider::{Line, read_line};
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

/// The event stream is a promise to whatever consumes it, and everything it
/// carries is derived from records Claude Code writes. A change to those
/// records would otherwise reach a consumer as an event that silently stopped
/// arriving — a foreman that never hears about a failed tool call, a cost that
/// stays at zero — so the stream is pinned to the same fixture the reader is.
#[test]
fn the_stream_still_carries_what_a_transcript_says() {
    use sightline_core::bus::Kind;
    use sightline_core::stream::{Snapshot, Watcher};
    use std::time::Instant;

    let mut session = sightline_core::session::Session::open(fixture("claude-transcript.jsonl"));
    let mut watcher = Watcher::new();
    let now = Instant::now();

    // First look: the session is known, and nothing it has already done is
    // replayed. This is the property that stops a consumer connecting to a busy
    // machine and being handed a day of history.
    watcher.poll(now, &[Snapshot::of(&session)]);

    // Now it does everything the transcript records.
    session.backfill();
    let events = watcher.poll(now, &[Snapshot::of(&session)]);
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.name()).collect();

    assert!(
        kinds.contains(&"toolCalled"),
        "a tool call no longer reaches the stream — the transcript's tool_use \
         block has moved, and a foreman would see a session doing nothing: {kinds:?}"
    );
    assert!(
        kinds.contains(&"costSpent"),
        "spend no longer reaches the stream — the usage block has moved, and \
         every cost ceiling built on this would read zero: {kinds:?}"
    );

    let tool = events
        .iter()
        .find_map(|e| match &e.kind {
            Kind::ToolCalled { tool, summary } => Some((tool.clone(), summary.clone())),
            _ => None,
        })
        .expect("the call is in the stream");
    assert_eq!(tool.0, "Bash", "the tool is named, not merely counted");
    assert!(
        tool.1.contains("cargo test"),
        "and the command it ran survives into the summary rather than being \
         reduced to the tool's name: {}",
        tool.1
    );

    let spent = events
        .iter()
        .find_map(|e| match &e.kind {
            Kind::CostSpent { output, estimate } => Some((*output, *estimate)),
            _ => None,
        })
        .expect("spend is in the stream");
    assert_eq!(
        spent.0, 240,
        "output tokens are reported as the amount spent since the last look"
    );
    assert!(spent.1 > 0.0, "and priced, so the model id is still known");

    // Every event is legible to something that was not compiled against this
    // version, which is the entire point of publishing them.
    for ev in &events {
        assert_eq!(ev.version, 1, "the stream is still speaking version 1");
        let line = ev.line();
        let back: sightline_core::bus::Event =
            serde_json::from_str(&line).expect("an event on the wire parses back");
        assert_eq!(&back, ev, "what a consumer reads is what was published");
        assert!(
            line.contains("\"session\":") && line.contains("\"agent\":"),
            "every event says which session and which agent it came from: {line}"
        );
    }
}

/// The stream-json protocol is a documented, versioned interface — but it is
/// still one Sightline depends on, and the point of pinning it is that a change
/// fails here rather than as an owned session that silently produces no events.
#[test]
fn claude_code_still_speaks_the_stream_json_sightline_parses() {
    use sightline_core::owned::parse_line;

    let fixture = read("claude-stream-json.jsonl");
    let events: Vec<_> = fixture
        .lines()
        .flat_map(|l| parse_line(l, "sess", "claude"))
        .collect();
    let kinds: Vec<&str> = events.iter().map(|e| e.kind.name()).collect();

    assert!(
        kinds.contains(&"sessionStarted"),
        "the init line still announces the session — its shape or subtype moved: {kinds:?}"
    );
    assert!(
        kinds.contains(&"toolCalled"),
        "a tool_use block is still a call — the assistant message shape moved: {kinds:?}"
    );
    assert!(
        kinds.contains(&"costSpent"),
        "the result line still carries usage — the field moved or was renamed: {kinds:?}"
    );
}
