//! Every conversation on the machine, however old.
//!
//! The session list is deliberately a window on now: what is running, and what
//! ran recently enough to still matter. Resuming is the opposite question —
//! anything you have ever talked to Claude Code about, whether that was an hour
//! ago or in March — so this reads the transcript directory whole.
//!
//! Only the head of each file is read. A conversation's title, where it was
//! held and how it opened are all written near the start, and a transcript can
//! be tens of megabytes, so reading it all to draw one line would make the
//! browser useless on exactly the machines that need it most.

use serde_json::Value;
use std::path::Path;
use std::time::SystemTime;

/// How much of a transcript is read to describe it.
const HEAD: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct Past {
    pub id: String,
    /// the transcript itself
    pub path: std::path::PathBuf,
    /// where the conversation was held
    pub cwd: String,
    /// the title Claude Code gave it, else how it opened
    pub title: String,
    pub modified: SystemTime,
    pub bytes: u64,
}

impl Past {
    /// Whatever names it best, for the list and for searching.
    pub fn label(&self) -> String {
        if !self.title.is_empty() {
            return self.title.clone();
        }
        format!("(untitled) {}", &self.id[..self.id.len().min(8)])
    }

    pub fn age_secs(&self) -> i64 {
        SystemTime::now()
            .duration_since(self.modified)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

fn head_of(path: &Path) -> Option<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).ok()?;
    let mut buf = vec![0u8; HEAD];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    Some(String::from_utf8_lossy(&buf).into_owned())
}

/// The first thing a person typed, which is what an untitled conversation is
/// remembered by. Meta lines and the harness's own preamble are not it.
fn opening_prompt(rec: &Value) -> Option<String> {
    if rec.get("type").and_then(Value::as_str)? != "user" {
        return None;
    }
    if rec.get("isMeta").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let content = rec.get("message")?.get("content")?;
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .find(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .and_then(|b| b.get("text").and_then(Value::as_str))
            .unwrap_or_default()
            .to_string(),
        _ => return None,
    };
    let t = text.trim();
    if t.is_empty() || t.starts_with("<system-reminder>") || t.starts_with("<command-") {
        return None;
    }
    Some(crate::event::clip(t, 90))
}

fn describe(path: &Path, modified: SystemTime, bytes: u64) -> Option<Past> {
    let id = path.file_stem()?.to_str()?.to_string();
    let mut cwd = String::new();
    let mut title = String::new();
    let mut chosen = String::new();
    let mut opening = String::new();
    for line in head_of(path)?.lines() {
        let Ok(rec) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if cwd.is_empty() {
            if let Some(c) = rec.get("cwd").and_then(Value::as_str) {
                cwd = c.to_string();
            }
        }
        if title.is_empty() {
            if let Some(t) = rec.get("aiTitle").and_then(Value::as_str) {
                title = t.to_string();
            }
        }
        // A name someone chose outranks the derived one, and can be written at
        // any point in the conversation, so this one keeps looking.
        if let Some(t) = rec.get("customTitle").and_then(Value::as_str) {
            chosen = t.to_string();
        }
        if opening.is_empty() {
            if let Some(p) = opening_prompt(&rec) {
                opening = p;
            }
        }
        // A chosen name can be written at any point, and is the one that
        // matters, so reading stops only once there is nothing better to find.
        if !cwd.is_empty() && !chosen.is_empty() {
            break;
        }
    }
    Some(Past {
        id,
        path: path.to_path_buf(),
        cwd,
        title: match (chosen.is_empty(), title.is_empty()) {
            (false, _) => chosen,
            (true, false) => title,
            (true, true) => opening,
        },
        modified,
        bytes,
    })
}

/// Every conversation under `root`, newest first.
pub fn scan(root: &Path) -> Vec<Past> {
    let mut out = Vec::new();
    let Ok(projects) = std::fs::read_dir(root) else {
        return out;
    };
    for project in projects.flatten() {
        let Ok(files) = std::fs::read_dir(project.path()) else {
            continue;
        };
        for f in files.flatten() {
            let path = f.path();
            if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(md) = f.metadata() else { continue };
            // An empty transcript is a session that never said anything; there
            // is nothing to resume.
            if md.len() == 0 {
                continue;
            }
            let modified = md.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if let Some(p) = describe(&path, modified, md.len()) {
                out.push(p);
            }
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// Conversations matching every word typed, in title, folder or id. Words
/// rather than a substring, so "adaudit report" finds it whichever order it
/// was written in.
pub fn matching<'a>(all: &'a [Past], filter: &str) -> Vec<&'a Past> {
    let needles: Vec<String> = filter
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .collect();
    all.iter()
        .filter(|p| {
            if needles.is_empty() {
                return true;
            }
            let hay = format!("{} {} {}", p.title, p.cwd, p.id).to_lowercase();
            needles.iter().all(|n| hay.contains(n))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn fixture(dir: &Path, name: &str, lines: &[&str]) {
        let project = dir.join("-home-someone");
        std::fs::create_dir_all(&project).unwrap();
        let mut f = std::fs::File::create(project.join(format!("{name}.jsonl"))).unwrap();
        for l in lines {
            writeln!(f, "{l}").unwrap();
        }
    }

    fn root(case: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("scope-history-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn describes_a_conversation_by_its_title_or_how_it_opened() {
        let dir = root("described");
        fixture(
            &dir,
            "aaaa1111-0000-0000-0000-000000000000",
            &[
                r#"{"type":"mode","mode":"normal"}"#,
                r#"{"type":"user","cwd":"/home/someone/api","message":{"role":"user","content":"fix the failing auth test"}}"#,
                r#"{"type":"ai-title","aiTitle":"Fix failing auth test"}"#,
            ],
        );
        fixture(
            &dir,
            "bbbb2222-0000-0000-0000-000000000000",
            &[
                r#"{"type":"user","isMeta":true,"cwd":"/home/someone/web","message":{"role":"user","content":"<command-name>/init</command-name>"}}"#,
                r#"{"type":"user","cwd":"/home/someone/web","message":{"role":"user","content":[{"type":"text","text":"draft the landing page"}]}}"#,
            ],
        );
        let all = scan(&dir);
        assert_eq!(all.len(), 2, "both conversations should be found");

        let titled = all.iter().find(|p| p.id.starts_with("aaaa")).unwrap();
        assert_eq!(titled.title, "Fix failing auth test");
        assert_eq!(titled.cwd, "/home/someone/api");

        // No title, so the first thing actually typed names it — not the meta
        // line the harness wrote first.
        let untitled = all.iter().find(|p| p.id.starts_with("bbbb")).unwrap();
        assert_eq!(untitled.title, "draft the landing page");

        let hits = matching(&all, "landing");
        assert_eq!(hits.len(), 1);
        assert!(hits[0].id.starts_with("bbbb"));
        // Words, in any order, across title and folder.
        assert_eq!(matching(&all, "api auth").len(), 1);
        assert_eq!(matching(&all, "nothing here").len(), 0);
        assert_eq!(matching(&all, "").len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn skips_transcripts_with_nothing_in_them() {
        let dir = root("empty");
        std::fs::create_dir_all(&dir).unwrap();
        fixture(&dir, "cccc3333-0000-0000-0000-000000000000", &[]);
        assert!(
            scan(&dir).is_empty(),
            "an empty transcript is not resumable"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
