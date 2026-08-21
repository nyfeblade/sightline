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
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn status(cwd: &Path) -> Option<Tree> {
    let branch = git(cwd, &["rev-parse", "--abbrev-ref", "HEAD"])?
        .trim()
        .to_string();
    let porcelain = git(cwd, &["status", "--porcelain=v1"])?;
    let entries = porcelain
        .lines()
        .filter(|l| l.len() > 3)
        .map(|l| Entry {
            code: l[..2].to_string(),
            path: l[3..].to_string(),
        })
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
    Some(Tree {
        branch,
        entries,
        insertions,
        deletions,
        fetched: Instant::now(),
    })
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

pub fn repo_root(cwd: &Path) -> Option<std::path::PathBuf> {
    let out = git(cwd, &["rev-parse", "--show-toplevel"])?;
    let trimmed = out.trim();
    (!trimmed.is_empty()).then(|| std::path::PathBuf::from(trimmed))
}

/// Where isolated checkouts live. Kept out of the repository so a worktree
/// never shows up as untracked noise in the original.
pub fn worktree_root(repo: &Path) -> std::path::PathBuf {
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| crate::app::home().join(".local").join("share"));
    let name = repo
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    base.join("nyfe-scope").join("worktrees").join(name)
}

/// Branch names are used as directory names, so keep them to safe characters.
fn slug(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// Create a worktree on a new branch cut from the repository's current HEAD.
pub fn create_worktree(repo: &Path, branch: &str) -> Result<std::path::PathBuf, String> {
    let branch = slug(branch);
    let dir = worktree_root(repo).join(&branch);
    if dir.exists() {
        return Err(format!("{} already exists", dir.display()));
    }
    std::fs::create_dir_all(dir.parent().unwrap_or(&dir)).map_err(|e| e.to_string())?;
    let dir_str = dir.to_string_lossy().to_string();
    git(repo, &["worktree", "add", "-b", &branch, &dir_str])
        .ok_or_else(|| format!("git worktree add failed for {branch} (does the branch exist?)"))?;
    Ok(dir)
}

/// Commits on this branch that the base does not have, and vice versa.
pub fn ahead_behind(cwd: &Path, base: &str) -> Option<(usize, usize)> {
    let out = git(
        cwd,
        &[
            "rev-list",
            "--left-right",
            "--count",
            &format!("{base}...HEAD"),
        ],
    )?;
    let mut parts = out.split_whitespace();
    let behind = parts.next()?.parse().ok()?;
    let ahead = parts.next()?.parse().ok()?;
    Some((ahead, behind))
}

/// The branch a worktree was cut from, best effort: the repository's default.
pub fn base_branch(repo: &Path) -> String {
    for candidate in ["main", "master"] {
        if git(repo, &["rev-parse", "--verify", candidate]).is_some() {
            return candidate.to_string();
        }
    }
    git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "HEAD".into())
}

/// Merge a session's branch back into the base, without fast-forwarding, so
/// the work stays visible as its own set of commits.
pub fn merge(repo: &Path, branch: &str, base: &str) -> Result<String, String> {
    let current = git(repo, &["rev-parse", "--abbrev-ref", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();
    if current != base {
        return Err(format!(
            "the repository is on {current}, not {base} — switch it first"
        ));
    }
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["merge", "--no-ff", branch])
        .output()
        .map_err(|e| e.to_string())?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    if out.status.success() {
        Ok(text.trim().to_string())
    } else {
        Err(text.lines().next().unwrap_or("merge failed").to_string())
    }
}

pub fn remove_worktree(repo: &Path, path: &str) -> Result<(), String> {
    git(repo, &["worktree", "remove", path])
        .map(|_| ())
        .ok_or_else(|| "git worktree remove failed (uncommitted changes? use --force)".into())
}

/// A linked worktree has a git dir separate from the repository's common dir.
pub fn is_worktree(cwd: &Path) -> bool {
    let (dir, common) = (
        git(cwd, &["rev-parse", "--absolute-git-dir"]),
        git(
            cwd,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        ),
    );
    match (dir, common) {
        (Some(d), Some(c)) => d.trim() != c.trim(),
        _ => false,
    }
}

/// The repository a linked worktree belongs to.
pub fn main_repo(cwd: &Path) -> Option<std::path::PathBuf> {
    let common = git(
        cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?;
    std::path::Path::new(common.trim())
        .parent()
        .map(|p| p.to_path_buf())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn run(cwd: &Path, args: &[&str]) {
        let out = Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(["-c", "user.email=t@example.com", "-c", "user.name=test"])
            .args(args)
            .output()
            .expect("git should run");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository with one commit on main, in a temp directory of its own.
    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("scope-git-test-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        run(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        run(&dir, &["add", "."]);
        commit(&dir, "first");
        dir
    }

    /// Commit with an identity of the test's own. A machine that has never had
    /// git configured — every fresh CI runner — refuses to commit otherwise,
    /// and then every assertion after it is about an empty repository.
    fn commit(dir: &std::path::Path, message: &str) {
        run(
            dir,
            &[
                "-c",
                "user.email=test@scope.invalid",
                "-c",
                "user.name=scope tests",
                "commit",
                "-qm",
                message,
            ],
        );
    }

    #[test]
    fn isolates_commits_then_merges_them_back() {
        let repo = scratch("merge");
        let tree = create_worktree(&repo, "feature/one").expect("worktree should be created");
        assert!(
            tree.join("a.txt").exists(),
            "the worktree is a full checkout"
        );
        assert!(is_worktree(&tree), "and it reads as a linked worktree");
        assert_eq!(main_repo(&tree).as_deref(), Some(repo.as_path()));

        std::fs::write(tree.join("b.txt"), "two\n").unwrap();
        run(&tree, &["add", "."]);
        commit(&tree, "work from the session");

        assert_eq!(
            ahead_behind(&tree, "main"),
            Some((1, 0)),
            "one commit ahead of main"
        );
        assert!(
            !repo.join("b.txt").exists(),
            "the original checkout is untouched"
        );

        merge(&repo, "feature-one", "main").expect("merge should succeed");
        assert!(repo.join("b.txt").exists(), "the work is now on main");

        remove_worktree(&repo, &tree.to_string_lossy()).expect("worktree should be removable");
        assert!(!tree.exists());
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn reports_a_plain_checkout_as_not_isolated() {
        let repo = scratch("plain");
        assert!(!is_worktree(&repo));
        assert_eq!(base_branch(&repo), "main");
        let _ = std::fs::remove_dir_all(&repo);
    }

    #[test]
    fn sees_uncommitted_work() {
        let repo = scratch("status");
        std::fs::write(repo.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(repo.join("new.txt"), "fresh\n").unwrap();
        let t = status(&repo).expect("status should read");
        assert_eq!(t.branch, "main");
        assert!(
            t.entries
                .iter()
                .any(|e| e.path == "new.txt" && e.code.trim() == "??")
        );
        assert_eq!(t.insertions, 1, "one line added to the tracked file");
        let _ = std::fs::remove_dir_all(&repo);
    }
}
