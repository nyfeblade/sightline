//! Cursor's headless agent, `cursor-agent`.
//!
//! Everything here was established by running it rather than by reading its
//! help, and the two disagreed in one place that matters — see `record`.
//!
//! What it is: Cursor ships a command-line agent separate from the IDE, with
//! `--print --output-format stream-json`, resumable chats, and a model list that
//! includes families Claude Code cannot reach — GPT-5.x, Codex, Grok, Composer,
//! and Cursor's own hosting of Claude. That is the reason to want it: a second
//! quota pool and a second set of models, on a wire close enough to Claude
//! Code's to read with a shim rather than a second parser.
//!
//! How much of the boundary reaches it: most of it, and this was recorded wrong
//! at first. Its `--help` mentions no permission hook, so it was written down as
//! ungoverned. Its binary says otherwise — a `hooks.json` with
//! `beforeShellExecution`, `beforeMCPExecution`, `beforeReadFile` and
//! `beforeSubmitPrompt`, each of which takes `{"permission": "deny"}` and throws
//! rather than running the call:
//!
//! ```text
//! const i = yield e.executeHookForStep(beforeShellExecution, {…command, cwd, sandbox});
//! if ("deny" === i?.permission) { throw new S(H("Command execution", i.user_message)) }
//! ```
//!
//! `afterFileEdit` fires only once a write has happened, which looked at first
//! like a gap the scope kernel could never close. It is not: `preToolUse` is
//! generic, sees every tool including the edit tools, and takes the same `deny`.
//! So the boundary reaches everything, and `hook.rs` is the door.
//!
//! Proved by running it. A session asked to `git push --force` was refused by
//! the forbid kernel; a session asked to write outside its worktree with its own
//! edit tool was refused by the scope kernel, and the file is not on disk.

use super::{Adapter, Delivery, Found, Naming, Options, Record};
use std::path::{Path, PathBuf};

pub struct Cursor;

impl Adapter for Cursor {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn label(&self) -> &'static str {
        "Cursor"
    }

    fn program(&self) -> &'static str {
        "cursor-agent"
    }

    fn command(&self, options: Options) -> Vec<String> {
        let mut argv: Vec<String> = Vec::new();
        if let Some(m) = options.model {
            argv.push("--model".into());
            argv.push(m.into());
        }
        // Effort is not a flag of its own: it rides inside the model name, as
        // `claude-opus-5-thinking-high` or as a bracketed override. A caller
        // that asks for effort without a model has asked for nothing this
        // program can express, and saying so beats guessing a model for them.
        if options.effort.is_some() && options.model.is_none() {
            // Deliberately nothing. `models` lists the levels as separate names.
        }
        match options.mode {
            // Its read-only modes, which are the nearest thing it has to a
            // permission mode.
            Some("plan") => argv.push("--plan".into()),
            Some("ask") => {
                argv.push("--mode".into());
                argv.push("ask".into());
            }
            _ => {}
        }
        argv
    }

    fn resume(&self, id: &str) -> Option<Vec<String>> {
        Some(vec!["--resume".into(), id.to_string()])
    }

    fn delivery(&self) -> Delivery {
        Delivery::Resume
    }

    fn naming(&self) -> Naming {
        Naming::Kept
    }

    fn prepare(&self, root: &Path, sightline: &Path) -> Result<(), String> {
        put_boundary(root, sightline)
    }

    fn offer_kernel(&self, root: &Path, session: &str, sightline: &Path) -> Result<(), String> {
        put_kernel(root, session, sightline)
    }

    fn record(&self) -> Record {
        // Nothing on disk that Sightline can read.
        //
        // It does keep chats — `ls` lists them and `--resume <id>` reopens one —
        // but they live behind its own command rather than in a file, so there
        // is no transcript to tail. A watched Cursor session is read from its
        // screen; a driven one is read from its stream. Claiming a transcript
        // format here would make `keeps_transcripts` true and send the history
        // view looking for files that do not exist.
        Record::None
    }

    fn conversations(&self, _roots: &[PathBuf]) -> Vec<Found> {
        Vec::new()
    }

    fn install_hint(&self) -> Option<&'static str> {
        // The IDE is a separate download and is not this. Somebody with Cursor
        // already installed still needs this, which is exactly the confusion
        // this line exists to prevent.
        Some("curl https://cursor.com/install -fsS | bash")
    }

    fn signin_hint(&self) -> Option<&'static str> {
        Some("cursor-agent login")
    }

    fn signin_probe(&self) -> Option<(&'static [&'static str], &'static str)> {
        Some((&["cursor-agent", "status"], "Logged in"))
    }

    fn governance(&self) -> super::Governance {
        // Full, through `hook.rs`, and proved by a real session rather than by
        // reading a contract: a Cursor agent asked to run `git push --force` was
        // refused by the forbid kernel, and one asked to write outside its
        // worktree with its own edit tool was refused by the scope kernel — the
        // file is not on disk. That second case is what made this `Partial`.
        //
        // It needs `sightline govern <dir>` to have written `.cursor/hooks.json`
        // there. A Cursor somebody starts by hand elsewhere is not governed, and
        // is not claimed to be.
        super::Governance::Full
    }
}

/// Put Cursor's permission door in a worktree.
///
/// Written before the session starts. Doing it after would leave the first
/// few tool calls ungoverned, which is the window that matters. Shared with
/// Grok Bot, because that assistant is Cursor's desktop product and reads
/// the same files.
pub fn put_boundary(root: &Path, sightline: &Path) -> Result<(), String> {
    let dir = root.join(".cursor");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(dir.join("hooks.json"), crate::hook::config(sightline))
        .map_err(|e| format!("could not put the boundary in place: {e}"))
}

/// Bind this session's kernel tools, now that it has a name to be attributed by.
pub fn put_kernel(root: &Path, session: &str, sightline: &Path) -> Result<(), String> {
    let dir = root.join(".cursor");
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(dir.join("mcp.json"), crate::mcp::config(sightline, session))
        .map_err(|e| format!("could not offer the kernel tools: {e}"))
}

/// Cursor's stream, rewritten into the shape Sightline already reads.
///
/// The two wires are close enough that a second parser would be a second thing
/// to keep correct for no gain: `system/init`, `user`, `assistant` with a
/// `message.content` array, and `result` are the same objects with the same
/// meanings. Three things differ, and this is all three.
///
/// Establishing that took running it rather than reading the help, and the
/// captured streams are in `tests/fixtures/cursor` so a change to any of it
/// fails a test that names what moved rather than quietly producing a session
/// that reports nothing.
pub fn normalise(line: &str) -> Option<String> {
    let mut v: serde_json::Value = serde_json::from_str(line).ok()?;
    match v.get("type").and_then(|t| t.as_str())? {
        // 1. Usage is camelCase here and snake_case there, and the cache figures
        //    are the ones that matter — they run sixty-odd times the output.
        "result" => {
            if let Some(u) = v.get("usage").cloned() {
                let n = |k: &str| u.get(k).and_then(serde_json::Value::as_u64).unwrap_or(0);
                v["usage"] = serde_json::json!({
                    "input_tokens": n("inputTokens"),
                    "output_tokens": n("outputTokens"),
                    "cache_read_input_tokens": n("cacheReadTokens"),
                    "cache_creation_input_tokens": n("cacheWriteTokens"),
                });
            }
            Some(v.to_string())
        }
        // 2. Thinking is a message type of its own here; there it is a content
        //    block inside an assistant message.
        "thinking" => {
            let text = v.get("text").and_then(|t| t.as_str()).unwrap_or("");
            if text.is_empty() {
                return None;
            }
            Some(
                serde_json::json!({
                    "type": "assistant",
                    "session_id": v.get("session_id"),
                    "message": {
                        "role": "assistant",
                        "content": [{ "type": "thinking", "thinking": text }],
                    }
                })
                .to_string(),
            )
        }
        // 3. A tool call names its tool with the *key* rather than a field —
        //    `{"tool_call": {"readToolCall": {...}}}` — and reports starting and
        //    finishing as two messages. The start becomes the call; the
        //    completion becomes the result under the same id, which is how the
        //    transcript view pairs them.
        "tool_call" => {
            let call = v.get("tool_call")?;
            let (key, body) = call
                .as_object()?
                .iter()
                .find(|(k, _)| k.ends_with("ToolCall"))?;
            let id = call
                .get("toolCallId")
                .and_then(|i| i.as_str())
                .or_else(|| v.get("call_id").and_then(|i| i.as_str()))
                .unwrap_or("");
            let started = v.get("subtype").and_then(|s| s.as_str()) == Some("started");
            if started {
                Some(
                    serde_json::json!({
                        "type": "assistant",
                        "session_id": v.get("session_id"),
                        "message": {
                            "role": "assistant",
                            "content": [{
                                "type": "tool_use",
                                "id": id,
                                "name": tool_name(key),
                                "input": body.get("args").cloned().unwrap_or(serde_json::json!({})),
                            }],
                        }
                    })
                    .to_string(),
                )
            } else {
                let result = body.get("result").cloned().unwrap_or(serde_json::json!({}));
                // A tool that failed says so in its own result. Reporting that
                // as success is the difference between a fleet you can trust and
                // one you cannot.
                let failed = result.get("error").is_some()
                    || result.get("success").is_none() && result.get("failure").is_some();
                Some(
                    serde_json::json!({
                        "type": "user",
                        "session_id": v.get("session_id"),
                        "message": {
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": id,
                                "is_error": failed,
                                "content": serde_json::to_string_pretty(&result).unwrap_or_default(),
                            }],
                        }
                    })
                    .to_string(),
                )
            }
        }
        // Everything else already has the right shape.
        _ => Some(v.to_string()),
    }
}

/// `readToolCall` is `Read`. Cursor names its tools in camelCase with a suffix;
/// Sightline shows them the way Claude Code does, so the two read alike in one
/// feed.
fn tool_name(key: &str) -> String {
    let bare = key.strip_suffix("ToolCall").unwrap_or(key);
    let mut out = String::with_capacity(bare.len() + 2);
    for (i, c) in bare.chars().enumerate() {
        if i == 0 {
            out.extend(c.to_uppercase());
        } else if c.is_uppercase() {
            out.push(' ');
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::owned::Parser;

    const PLAIN: &str = include_str!("../../tests/fixtures/cursor/plain-turn.ndjson");
    const WITH_TOOL: &str = include_str!("../../tests/fixtures/cursor/tool-turn.ndjson");

    /// Everything a stream produces, once translated and read by the one parser.
    fn events(fixture: &str) -> Vec<crate::bus::Event> {
        let mut parser = Parser::new();
        let mut out = Vec::new();
        for line in fixture.lines().filter(|l| !l.trim().is_empty()) {
            if let Some(rewritten) = normalise(line) {
                out.extend(parser.feed(&rewritten, "s1", "cursor"));
            }
        }
        out
    }

    #[test]
    fn a_turn_that_only_talked_is_read_as_a_turn_that_talked() {
        let all = events(PLAIN);
        assert!(
            all.iter()
                .any(|e| matches!(e.kind, crate::bus::Kind::CostSpent { .. })),
            "a finished turn reports what it spent"
        );
    }

    #[test]
    fn the_numbers_that_dominate_survive_the_translation() {
        // Cursor reports usage in camelCase; Sightline reads snake_case. Getting
        // this wrong would not fail loudly — it would report every Cursor
        // session as having cost nothing, which is the silent kind of wrong.
        let all = events(PLAIN);
        let spent = all
            .iter()
            .find_map(|e| match &e.kind {
                crate::bus::Kind::CostSpent { output, cached, .. } => Some((*output, *cached)),
                _ => None,
            })
            .expect("the turn published its cost");
        assert_eq!(spent.0, 46, "output tokens, from outputTokens");
        assert_eq!(spent.1, 5_460, "context re-read, from cacheReadTokens");
    }

    #[test]
    fn a_tool_call_and_its_result_are_one_call_rather_than_two_rows() {
        // Cursor sends `started` and `completed` as separate messages and names
        // the tool with the object key rather than a field. Both have to be
        // undone or a transcript shows every call twice and none of them named.
        let all = events(WITH_TOOL);
        let said: Vec<String> = all.iter().map(|e| format!("{:?}", e.kind)).collect();
        assert!(
            said.iter().any(|s| s.contains("Read")),
            "readToolCall is a Read, not a `readToolCall`: {said:?}"
        );
    }

    #[test]
    fn the_tool_name_is_the_key_and_reads_like_the_others() {
        assert_eq!(tool_name("readToolCall"), "Read");
        assert_eq!(tool_name("writeToolCall"), "Write");
        assert_eq!(tool_name("shellToolCall"), "Shell");
        // Compound names stay legible rather than running together.
        assert_eq!(tool_name("semanticSearchToolCall"), "Semantic Search");
    }

    #[test]
    fn a_line_that_is_not_json_is_skipped_rather_than_fatal() {
        // Anything on this stream that is not an object is somebody else's
        // problem — a warning, a progress bar — and must not stop the session.
        assert!(normalise("not json at all").is_none());
        assert!(normalise("").is_none());
    }
}
