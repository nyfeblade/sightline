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
    if cfg!(windows) {
        // A toast through PowerShell: no module to install, and it survives
        // Sightline being in the background, which is the whole point.
        let esc = |s: &str| s.replace('\'', "''");
        let script = format!(
            "$ErrorActionPreference='Stop';\
             [void][Windows.UI.Notifications.ToastNotificationManager,Windows.UI.Notifications,ContentType=WindowsRuntime];\
             $x=[Windows.UI.Notifications.ToastNotificationManager]::GetTemplateContent(2);\
             $t=$x.GetElementsByTagName('text');\
             $t[0].AppendChild($x.CreateTextNode('{}'))|Out-Null;\
             $t[1].AppendChild($x.CreateTextNode('{}'))|Out-Null;\
             [Windows.UI.Notifications.ToastNotificationManager]::CreateToastNotifier('Microsoft.Windows.Powershell').Show([Windows.UI.Notifications.ToastNotification]::new($x))",
            esc(title),
            esc(body)
        );
        let mut c = Command::new("powershell");
        c.args(["-NoProfile", "-NonInteractive", "-Command", &script]);
        quiet(c);
        return;
    }
    let mut c = Command::new("notify-send");
    c.args(["-a", "sightline", title, body]);
    quiet(c);
}

pub fn available() -> bool {
    // Windows looks things up on PATH with `where`, not `which`.
    let (finder, probe) = if cfg!(windows) {
        ("where", "powershell")
    } else if cfg!(target_os = "macos") {
        ("which", "osascript")
    } else {
        ("which", "notify-send")
    };
    Command::new(finder)
        .arg(probe)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}
