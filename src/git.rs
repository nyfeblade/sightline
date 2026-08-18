//! What a session has actually changed on disk, as opposed to what its
//! transcript says it did. Nothing here assumes the directory is a repository.

use std::path::Path;
use std::process::{Command, Stdio};
use std::time::Instant;

#[derive(Clone)]
pub struct Entry {
    /// porcelain status code, e.g. " M", "??", "A "
    pub code: String,
    pub path: String,
}

#[derive(Clone)]
pub struct Tree {
    pub branch: String,
    pub entries: Vec<Entry>,
    pub insertions: usize,
    pub deletions: usize,
    pub fetched: Instant,
}

fn git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(cwd)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn status(cwd: &Path) -> Option<Tree> {
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?.trim().to_string();
    let porcelain = git(cwd, &["status", "--porcelain=v1"])?;
    let entries = porcelain
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| Entry { code: l[..2].to_string(), path: l[3..].to_string() })
        .collect();
    let (mut insertions, mut deletions) = (0, 0);
    if let Some(stat) = git(cwd, &["diff", "--shortstat"]) {
        for part in stat.split(',') {
            let n: usize = part
                .trim()
                .split_whitespace()
                .next()
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            if part.contains("insertion") {
                insertions = n;
            } else if part.contains("deletion") {
                deletions = n;
            }
        }
    }
    Some(Tree { branch, entries, insertions, deletions, fetched: Instant::now() })
}

/// The working-tree diff for one path, for the detail view.
pub fn diff(cwd: &Path, path: &str) -> Option<String> {
    let tracked = git(cwd, &["diff", "--", path]).filter(|d| !d.trim().is_empty());
    tracked.or_else(|| {
        // Untracked: show the file itself, capped so a huge blob cannot wedge
        // the view.
        let full = cwd.join(path);
        let text = std::fs::read_to_string(full).ok()?;
        Some(text.chars().take(200_000).collect())
    })
}
