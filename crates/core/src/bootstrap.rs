//! What has to be true before scope can do anything, and putting it right
//! where that is possible.
//!
//! In a terminal the answer to a missing tool is an error message and a person
//! who knows what to do with it. An app launched from a dock has neither: the
//! window opens, nothing works, and there is nowhere for the reason to go. So
//! the state of the world is a thing scope can be asked about, in the same
//! shape whichever front end is asking — is this present, what does it mean if
//! it is not, and what exactly should be typed to fix it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// How much a missing piece costs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weight {
    /// Nothing works without it.
    Required,
    /// Something works less well, and it is worth saying which.
    Optional,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Check {
    pub name: &'static str,
    pub ok: bool,
    pub weight: Weight,
    /// what its presence or absence means, in a sentence
    pub detail: String,
    /// exactly what to run, when running something would fix it
    pub fix: Option<String>,
}

/// What the world looks like, gathered once so the reasoning about it is pure.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Probes {
    pub claude: Option<PathBuf>,
    pub multiplexer: bool,
    /// scope hosts its own sessions and needs no multiplexer
    pub hosts_own_sessions: bool,
    pub transcripts: bool,
    pub terminal: Option<String>,
    pub package_manager: Option<&'static str>,
}

/// The verdict on a set of probes. Pure, so every branch is testable without
/// uninstalling anything.
pub fn assess(p: &Probes) -> Vec<Check> {
    let mut out = Vec::new();

    out.push(match &p.claude {
        Some(path) => Check {
            name: "Claude Code",
            ok: true,
            weight: Weight::Required,
            detail: path.display().to_string(),
            fix: None,
        },
        None => Check {
            name: "Claude Code",
            ok: false,
            weight: Weight::Required,
            detail: "not on PATH — scope watches and steers it, so there is \
                     nothing to do without it"
                .into(),
            fix: Some("curl -fsSL https://claude.ai/install.sh | bash".into()),
        },
    });

    if !p.hosts_own_sessions {
        out.push(Check {
            name: "tmux",
            ok: p.multiplexer,
            weight: Weight::Required,
            detail: if p.multiplexer {
                "sessions are held by tmux, so they outlive scope".into()
            } else {
                "sessions can be watched but not started or typed into".into()
            },
            fix: if p.multiplexer {
                None
            } else {
                Some(install_line(p.package_manager, "tmux"))
            },
        });
    }

    out.push(Check {
        name: "transcripts",
        ok: p.transcripts,
        weight: Weight::Optional,
        detail: if p.transcripts {
            "found".into()
        } else {
            "none yet — they appear the first time a session says anything".into()
        },
        fix: None,
    });

    if !p.hosts_own_sessions {
        out.push(Check {
            name: "terminal",
            ok: p.terminal.is_some(),
            weight: Weight::Optional,
            detail: match &p.terminal {
                Some(t) => format!("{t} — sessions can be opened in their own window"),
                None => "none found — sessions open inside scope instead".into(),
            },
            fix: None,
        });
    }

    out
}

/// The command that installs something here, as far as can be told from what
/// package manager exists. Guessing wrong is worse than saying less, so an
/// unknown system gets the name and no invented incantation.
fn install_line(manager: Option<&'static str>, package: &str) -> String {
    match manager {
        Some("brew") => format!("brew install {package}"),
        Some("dnf") => format!("sudo dnf install {package}"),
        Some("apt") => format!("sudo apt install {package}"),
        Some("pacman") => format!("sudo pacman -S {package}"),
        Some("zypper") => format!("sudo zypper install {package}"),
        Some(other) => format!("{other} install {package}"),
        None => format!("install {package} with your package manager"),
    }
}

/// Whether everything required is present.
pub fn ready(checks: &[Check]) -> bool {
    checks.iter().all(|c| c.ok || c.weight == Weight::Optional)
}

fn on_path(name: &str) -> Option<PathBuf> {
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".COM;.EXE;.BAT;.CMD".into())
            .split(';')
            .filter(|e| !e.is_empty())
            .map(str::to_lowercase)
            .collect()
    } else {
        Vec::new()
    };
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let plain = dir.join(name);
        if plain.is_file() {
            return Some(plain);
        }
        for ext in &exts {
            let with_ext = dir.join(format!("{name}{ext}"));
            if with_ext.is_file() {
                return Some(with_ext);
            }
        }
    }
    None
}

const MANAGERS: [&str; 5] = ["brew", "dnf", "apt", "pacman", "zypper"];

const TERMINALS: [&str; 8] = [
    "kitty",
    "wezterm",
    "alacritty",
    "ghostty",
    "gnome-terminal",
    "konsole",
    "xfce4-terminal",
    "xterm",
];

/// Look at the actual machine.
pub fn probe(transcript_root: &Path) -> Probes {
    let transcripts = std::fs::read_dir(transcript_root)
        .map(|mut d| d.any(|e| e.is_ok()))
        .unwrap_or(false);
    Probes {
        claude: on_path("claude"),
        multiplexer: crate::control::available(),
        hosts_own_sessions: !crate::control::OUTLIVES_SCOPE,
        transcripts,
        terminal: std::env::var("TERMINAL")
            .ok()
            .filter(|t| !t.is_empty() && on_path(t).is_some())
            .or_else(|| {
                TERMINALS
                    .iter()
                    .find(|t| on_path(t).is_some())
                    .map(|t| (*t).to_string())
            }),
        package_manager: MANAGERS.into_iter().find(|m| on_path(m).is_some()),
    }
}

/// Everything the app needs running before it can start a session, done for
/// the person rather than asked of them. Starting the server up front means
/// the first session does not pay for it, and that a dock launch with nothing
/// else running still has somewhere to put a session.
pub fn ensure_backend() -> Result<(), String> {
    if !crate::control::OUTLIVES_SCOPE {
        return Ok(());
    }
    if !crate::control::available() {
        return Err("tmux is not installed".into());
    }
    let started = Command::new("tmux")
        .arg("start-server")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|e| e.to_string())?;
    if started.success() {
        Ok(())
    } else {
        Err("tmux would not start a server".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unix_ready() -> Probes {
        Probes {
            claude: Some(PathBuf::from("/usr/local/bin/claude")),
            multiplexer: true,
            hosts_own_sessions: false,
            transcripts: true,
            terminal: Some("kitty".into()),
            package_manager: Some("dnf"),
        }
    }

    #[test]
    fn a_ready_machine_reports_ready() {
        let checks = assess(&unix_ready());
        assert!(ready(&checks));
        assert!(checks.iter().all(|c| c.ok));
        assert!(
            checks.iter().all(|c| c.fix.is_none()),
            "nothing to fix, so nothing should be suggested"
        );
    }

    #[test]
    fn missing_claude_is_fatal_and_says_how_to_get_it() {
        let mut p = unix_ready();
        p.claude = None;
        let checks = assess(&p);
        assert!(!ready(&checks));
        let c = checks.iter().find(|c| c.name == "Claude Code").unwrap();
        assert_eq!(c.weight, Weight::Required);
        assert!(c.fix.as_deref().unwrap().contains("install.sh"));
    }

    #[test]
    fn missing_tmux_suggests_the_right_package_manager() {
        let mut p = unix_ready();
        p.multiplexer = false;
        let fix = |mgr| {
            let mut p = p.clone();
            p.package_manager = mgr;
            assess(&p)
                .into_iter()
                .find(|c| c.name == "tmux")
                .unwrap()
                .fix
                .unwrap()
        };
        assert_eq!(fix(Some("dnf")), "sudo dnf install tmux");
        assert_eq!(fix(Some("brew")), "brew install tmux");
        assert_eq!(fix(Some("apt")), "sudo apt install tmux");
        // Nothing recognised: say what is needed, do not invent a command.
        assert_eq!(fix(None), "install tmux with your package manager");
    }

    #[test]
    fn hosting_its_own_sessions_needs_no_multiplexer() {
        let mut p = unix_ready();
        p.hosts_own_sessions = true;
        p.multiplexer = false;
        p.terminal = None;
        let checks = assess(&p);
        assert!(ready(&checks), "Windows needs neither tmux nor a terminal");
        assert!(!checks.iter().any(|c| c.name == "tmux"));
        assert!(!checks.iter().any(|c| c.name == "terminal"));
    }

    #[test]
    fn what_is_missing_but_survivable_does_not_block() {
        let mut p = unix_ready();
        p.transcripts = false;
        p.terminal = None;
        let checks = assess(&p);
        assert!(ready(&checks), "a first run has no transcripts yet");
        let t = checks.iter().find(|c| c.name == "transcripts").unwrap();
        assert!(!t.ok && t.weight == Weight::Optional);
        assert!(t.detail.contains("first time"));
    }
}
