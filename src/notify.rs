//! Desktop notifications, best-effort. Missing tooling is not an error — the
//! event still shows in the UI, it just does not leave the terminal.

use std::process::{Command, Stdio};

pub fn send(title: &str, body: &str) {
    let quiet = |mut c: Command| {
        let _ = c.stdout(Stdio::null()).stderr(Stdio::null()).status();
    };
    if cfg!(target_os = "macos") {
        let mut c = Command::new("osascript");
        c.arg("-e").arg(format!(
            "display notification {:?} with title {:?}",
            body, title
        ));
        quiet(c);
        return;
    }
    let mut c = Command::new("notify-send");
    c.args(["-a", "scope", title, body]);
    quiet(c);
}

pub fn available() -> bool {
    let probe = if cfg!(target_os = "macos") { "osascript" } else { "notify-send" };
    Command::new("which")
        .arg(probe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
