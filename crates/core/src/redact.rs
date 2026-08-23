//! Keeping secrets out of the things Ironsight writes down.
//!
//! The event stream records what every session ran, and it does two things with
//! that: writes it to a file that stays on disk, and offers it on a socket to
//! anything running as you. A command line is a good place to find a token —
//! `curl -H "Authorization: Bearer …"`, `GITHUB_TOKEN=… ./deploy`, an API key
//! passed as a flag — and none of that should end up in a durable artifact
//! because a monitor happened to be watching.
//!
//! The split is deliberate: the interface still shows the real command, because
//! it is your machine and your terminal and you can already see it. What gets
//! redacted is what *leaves* — the journal, and the socket. Reading a screen is
//! not the same as writing to a file that outlives the session and is served to
//! whatever else is running.
//!
//! It errs towards redacting. A summary with `‹redacted›` in it is a small loss;
//! a token in a log that gets pasted into an issue is not. Where it cannot
//! tell, it leaves the text alone rather than mangling every command into
//! unreadability — the aim is to catch the shapes secrets actually come in, not
//! to guarantee something no heuristic can.

/// What replaces anything that looks like a secret.
pub const MASK: &str = "‹redacted›";

/// Prefixes that are only ever the beginning of a credential.
const MINTED: [&str; 12] = [
    "sk-",
    "sk_live_",
    "sk_test_",
    "ghp_",
    "gho_",
    "ghu_",
    "ghs_",
    "github_pat_",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "AIza",
];

/// Words in a name that mean whatever follows it is not for writing down.
const TELLING: [&str; 9] = [
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "credential",
    "auth",
    "session_key",
];

/// Flags and words whose *next* word is the secret.
///
/// Short flags are deliberately absent. `-p` means password to `psql` and
/// package to `cargo`, port to `docker` and parents to `mkdir`; including it
/// redacted half the commands in this repository, which a test caught.
const CARRIES: [&str; 7] = [
    "--token",
    "--password",
    "--api-key",
    "--apikey",
    "--secret",
    "--auth",
    "bearer",
];

fn looks_minted(word: &str) -> bool {
    MINTED
        .iter()
        .any(|p| word.starts_with(p) && word.len() > p.len() + 8)
}

/// A long opaque run of characters that is mixed enough not to be a hash.
///
/// Deliberately not "any long hex string": a git object name is forty hex
/// characters, is in half the commands here, and is not a secret. Requiring a
/// mix of cases and digits catches the base64-ish shapes keys actually take.
fn looks_opaque(word: &str) -> bool {
    let body = word.trim_matches(|c| matches!(c, '"' | '\'' | ',' | ';' | ')' | '('));
    if body.len() < 32 || body.contains('/') || body.contains(' ') {
        return false;
    }
    let usable = body
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '+' | '=' | '.'));
    let upper = body.chars().any(|c| c.is_ascii_uppercase());
    let lower = body.chars().any(|c| c.is_ascii_lowercase());
    let digit = body.chars().any(|c| c.is_ascii_digit());
    usable && upper && lower && digit
}

/// Whether a name says that what it holds is a secret.
///
/// A whole-token match, not a bare substring: `--author` and `GIT_AUTHOR_NAME`
/// contain "auth" but hold nothing sensitive, and mangling them was exactly the
/// over-redaction the module warns against. A telling word counts only when the
/// character after it is not another letter — so "auth", "auth_token" and "AUTH"
/// match, "author" does not.
fn telling(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    TELLING.iter().any(|w| {
        let mut from = 0;
        while let Some(at) = lowered[from..].find(w) {
            let end = from + at + w.len();
            let next_is_letter = lowered[end..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic());
            if !next_is_letter {
                return true;
            }
            from = from + at + 1;
        }
        false
    })
}

/// Everything that looks like a credential, replaced.
pub fn text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut carried = false;

    for word in input.split_inclusive(char::is_whitespace) {
        let trimmed = word.trim_end();
        let space = &word[trimmed.len()..];
        let bare = trimmed.trim_matches(|c| matches!(c, '"' | '\''));

        // The word after `--token`, or after a header that names one.
        if carried {
            carried = false;
            if !bare.is_empty() && !bare.starts_with('-') {
                out.push_str(MASK);
                out.push_str(space);
                continue;
            }
        }

        // NAME=value, where the name gives it away.
        if let Some((name, value)) = bare.split_once('=') {
            if !value.is_empty() && telling(name) && !name.contains('/') {
                out.push_str(name);
                out.push('=');
                out.push_str(MASK);
                out.push_str(space);
                continue;
            }
        }

        // `Authorization: Bearer …` and its neighbours.
        if let Some((name, value)) = bare.split_once(':') {
            if telling(name) || name.eq_ignore_ascii_case("authorization") {
                if !value.trim().is_empty() {
                    out.push_str(name);
                    out.push(':');
                    out.push_str(MASK);
                    out.push_str(space);
                    continue;
                }
            }
        }

        if looks_minted(bare) || looks_opaque(bare) {
            out.push_str(MASK);
            out.push_str(space);
            continue;
        }

        // `Authorization: Bearer <token>` arrives as three words once a shell
        // has been through it, so the word before the token is what arms this.
        if CARRIES.iter().any(|f| bare.eq_ignore_ascii_case(f)) {
            carried = true;
        }
        out.push_str(word);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The secret must be absent, not merely something masked somewhere. An
    /// earlier version of this test passed while redacting the header *name*
    /// and leaving the token beside it.
    fn hides(command: &str, secret: &str) {
        let out = text(command);
        assert!(!out.contains(secret), "the secret survived: {out}");
        assert!(out.contains(MASK), "nothing was masked: {out}");
    }

    #[test]
    fn hides_the_shapes_credentials_come_in() {
        for (command, secret) in [
            (
                "curl -H Authorization: Bearer sk-ant-api03-Zm9vYmFyYmF6cXV4Q2c https://api.example.com",
                "sk-ant-api03-Zm9vYmFyYmF6cXV4Q2c",
            ),
            (
                "GITHUB_TOKEN=ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8 gh pr create",
                "ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8",
            ),
            (
                "export ANTHROPIC_API_KEY=sk-ant-api03-Zm9vYmFyYmF6cXV4Q2c",
                "sk-ant-api03-Zm9vYmFyYmF6cXV4Q2c",
            ),
            (
                "aws configure set aws_access_key_id AKIAIOSFODNN7EXAMPLE",
                "AKIAIOSFODNN7EXAMPLE",
            ),
            (
                "deploy --token gho_16C7e42F292c6912E7710c838347Ae178B4a",
                "gho_16C7e42F292c6912E7710c838347Ae178B4a",
            ),
            (
                "echo AIzaSyD9tSrke72PouQMnMXa7eZSW0jkFMBWY",
                "AIzaSyD9tSrke72PouQMnMXa7eZSW0jkFMBWY",
            ),
            (
                "run --password Tr0ub4dor3AndAVeryLongOneIndeed42x",
                "Tr0ub4dor3AndAVeryLongOneIndeed42x",
            ),
        ] {
            hides(command, secret);
        }
    }

    #[test]
    fn leaves_ordinary_commands_alone() {
        for plain in [
            "cargo test --release",
            "git commit -m \"fix the parser\"",
            "grep -rn TODO crates/core/src",
            "python3 scripts/make-icon.py",
            "ls -la /home/nyfe/ironsight/target/debug",
            "cargo check --target x86_64-pc-windows-msvc -p ironsight-core",
        ] {
            assert_eq!(text(plain), plain, "mangled an ordinary command");
        }
    }

    #[test]
    fn author_is_not_treated_as_auth() {
        // "auth" inside "author" is not a credential name; the value must survive.
        let out = text("git commit --author=\"John Doe\" -m fix");
        assert!(
            out.contains("John") || out.contains("--author"),
            "an --author was mangled as if it were a secret: {out}"
        );
        assert!(
            !out.contains(MASK),
            "nothing should have been redacted: {out}"
        );
        // But a real auth token name is still caught.
        assert!(
            text("AUTH_TOKEN=Zm9vYmFyMTIzNDU2Nzg5MFFXRVJUWQ").contains(MASK),
            "a genuine AUTH_TOKEN is still redacted"
        );
    }

    #[test]
    fn a_commit_name_is_not_a_secret() {
        // Forty hex characters, in half the commands in this repository.
        let sha = "git show aa01afaf1c4e8b2d3f5a6c7e8b9d0a1c2e3f4a5b";
        assert_eq!(text(sha), sha, "a git object name was taken for a key");
        let branch = "git checkout -b feature/some-long-branch-name-here";
        assert_eq!(text(branch), branch);
    }

    #[test]
    fn a_path_is_not_a_secret_however_long() {
        let path =
            "cat /tmp/claude-1000/-home-nyfe/1f71557e-a5e0-41bd-9644-b1744ca70605/scratchpad/x";
        assert_eq!(text(path), path, "a long path was taken for a key");
    }

    #[test]
    fn what_is_left_is_still_readable() {
        let out = text("GITHUB_TOKEN=ghp_A1b2C3d4E5f6G7h8I9j0K1l2M3n4O5p6Q7r8 gh pr create --fill");
        assert!(out.starts_with("GITHUB_TOKEN="), "the name survives: {out}");
        assert!(
            out.ends_with("gh pr create --fill"),
            "and so does the command: {out}"
        );
        assert!(!out.contains("ghp_"), "but not the key: {out}");
    }

    #[test]
    fn spacing_survives_a_redaction() {
        // The summary is read by people; collapsing whitespace would make a
        // redacted command harder to recognise than the secret was worth.
        let out = text("run  --token  abc123def456GHI789jkl012MNO345pqr  --verbose");
        assert!(out.contains("--verbose"));
        assert!(out.contains(MASK));
    }

    #[test]
    fn nothing_at_all_is_fine() {
        assert_eq!(text(""), "");
        assert_eq!(text("   "), "   ");
    }
}
