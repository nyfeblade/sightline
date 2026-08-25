//! Sightline's tools, offered the way a second vendor can reach them.
//!
//! Claude Code takes an MCP server this process serves in memory, over the same
//! pipe as everything else. Cursor takes one the ordinary way: a command it
//! spawns, speaking JSON-RPC over standard input and output. Same tools, same
//! `kernel::call` behind them — a second door, not a second implementation.
//!
//! What this makes possible is not vendor parity for its own sake. Until now no
//! worker of any kind could `claim`: workers start without kernel tools, so the
//! one tool whose description reads "say the work you were assigned is
//! finished" could be called only by a chief, which is never assigned anything.
//! The ladder had no entrance from the session doing the work. This is that
//! entrance, and Cursor happens to be what forced it into the open.
//!
//! Deliberately small. Three methods of the protocol are implemented — the
//! handshake, the list, and the call — because that is what a client needs to
//! use a tool, and a fuller implementation would be more surface to keep true
//! for no one's benefit.

use serde_json::{Value, json};

/// The protocol version this speaks, echoed back at a client that asks for it.
const PROTOCOL: &str = "2025-06-18";

/// Answer one request. `None` for a notification, which takes no reply.
pub fn respond(line: &str, session: &str, role: crate::kernel::Role) -> Option<String> {
    let request: Value = serde_json::from_str(line).ok()?;
    let id = request.get("id").cloned();
    let method = request.get("method").and_then(Value::as_str)?;
    // A notification has no id and must not be answered. Replying to one is how
    // a client ends up waiting for a message it already had.
    if id.is_none() {
        return None;
    }

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": PROTOCOL,
            "capabilities": { "tools": {} },
            "serverInfo": { "name": "sightline", "version": env!("CARGO_PKG_VERSION") },
        })),
        "tools/list" => Ok(json!({ "tools": crate::kernel::schemas_for(role) })),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            match crate::kernel::call(session, name, &args) {
                // A refusal is a result, not a transport error. The model is
                // meant to read it and do something else — an error at the
                // protocol level is a client's problem to handle, and most
                // handle it by giving up.
                Ok(text) => Ok(json!({
                    "content": [{ "type": "text", "text": text }],
                    "isError": false,
                })),
                Err(why) => Ok(json!({
                    "content": [{ "type": "text", "text": why }],
                    "isError": true,
                })),
            }
        }
        other => Err(format!("no method {other}")),
    };

    Some(
        match result {
            Ok(value) => json!({ "jsonrpc": "2.0", "id": id, "result": value }),
            Err(message) => json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": { "code": -32601, "message": message },
            }),
        }
        .to_string(),
    )
}

/// The file Cursor reads to find this, for one worker.
///
/// The session's name is baked in, because a server spawned by an agent has no
/// other way to know which session it is serving — and a `claim` attributed to
/// the wrong session marks somebody else's work finished.
pub fn config(sightline: &std::path::Path, session: &str) -> String {
    serde_json::to_string_pretty(&json!({
        "mcpServers": {
            "sightline": {
                "command": sightline.to_string_lossy(),
                "args": ["mcp", "--as", session],
            }
        }
    }))
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::Role;

    fn ask(request: Value, role: Role) -> Value {
        serde_json::from_str(&respond(&request.to_string(), "w1", role).expect("a reply")).unwrap()
    }

    #[test]
    fn a_worker_is_offered_the_tools_a_worker_can_use() {
        let out = ask(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            Role::Worker,
        );
        let names: Vec<String> = out["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(names.contains(&"claim".to_string()), "{names:?}");
        assert!(names.contains(&"note".to_string()), "{names:?}");
        // The whole reason the split exists: a worker that could assign would
        // start workers of its own, and a ceiling counting only the sessions it
        // knows about is not a ceiling.
        assert!(!names.contains(&"assign".to_string()), "{names:?}");
        assert!(!names.contains(&"tell".to_string()), "{names:?}");
    }

    #[test]
    fn a_chief_is_offered_the_tools_a_chief_can_use() {
        let out = ask(
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            Role::Chief,
        );
        let names: Vec<String> = out["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap_or("").to_string())
            .collect();
        assert!(names.contains(&"assign".to_string()), "{names:?}");
        assert!(
            !names.contains(&"claim".to_string()),
            "a chief is never assigned anything, so it has nothing to claim: {names:?}"
        );
    }

    #[test]
    fn the_handshake_answers_with_what_it_can_do() {
        let out = ask(
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize"}),
            Role::Worker,
        );
        assert_eq!(out["result"]["serverInfo"]["name"], "sightline");
        assert!(out["result"]["capabilities"]["tools"].is_object());
    }

    #[test]
    fn a_refusal_comes_back_as_a_result_the_model_can_read() {
        // Not a protocol error. A tool that refused is telling the model
        // something it should act on; a transport error is a client's problem,
        // and most clients handle one by giving up.
        let out = ask(
            json!({
                "jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": "claim", "arguments": {}}
            }),
            Role::Worker,
        );
        assert!(out["result"]["isError"].as_bool().unwrap_or(false));
        assert!(
            out.get("error").is_none(),
            "a refusal is not a transport failure"
        );
        assert!(
            !out["result"]["content"][0]["text"]
                .as_str()
                .unwrap_or("")
                .is_empty(),
            "and it says why"
        );
    }

    #[test]
    fn a_notification_is_not_answered() {
        // No id means no reply. Answering one is how a client ends up waiting
        // for a message it has already had.
        assert!(
            respond(
                &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
                "w1",
                Role::Worker
            )
            .is_none()
        );
    }

    #[test]
    fn the_config_names_the_session_it_serves() {
        // A server spawned by an agent has no other way to know which session it
        // is for, and a claim attributed to the wrong one marks somebody else's
        // work finished.
        let text = config(std::path::Path::new("/usr/bin/sightline"), "owned-7");
        assert!(text.contains("owned-7"), "{text}");
        assert!(text.contains("\"mcp\""), "{text}");
    }
}
