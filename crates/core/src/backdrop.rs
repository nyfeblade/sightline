//! The light behind the glass, and where it comes from.
//!
//! The window's surfaces are translucent, so what is behind them is most of
//! what they look like. That light was drawn in CSS for a while — blooms and
//! hard-edged diagonal rakes meant to imitate a reference — and the rakes never
//! worked. A gradient can make a soft bloom convincingly and cannot make
//! geometry: the edges come out mathematically clean in a way real light is
//! not, and the result reads as a pattern laid over the window rather than as
//! something behind it.
//!
//! So the geometry is a picture now, and the person chooses it. A soft bloom is
//! what ships, because it is the thing CSS is actually good at and it needs no
//! file to exist.
//!
//! The image is read here rather than in either front end, and handed over as a
//! data URI. Three reasons, in order of how much they matter: the window's
//! content security policy allows `img-src 'self' data:` and nothing else, so a
//! file path on disk is not something the page can load; both front ends need
//! the same answer about what the backdrop currently is; and reading a file the
//! person named is a decision with a size limit and a format list attached,
//! which is logic, and logic does not live in a front end.

use std::path::{Path, PathBuf};

/// What the window paints behind everything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Choice {
    /// The bloom that ships. Costs nothing and needs no file.
    Bloom,
    /// Nothing at all: flat black, for anyone who finds light distracting.
    None,
    /// A picture the person chose.
    Image(PathBuf),
}

/// What the webview can actually decode. Anything else is refused by name
/// rather than encoded into a data URI the page will silently fail to paint.
const DECODABLE: [(&str, &str); 6] = [
    ("png", "image/png"),
    ("jpg", "image/jpeg"),
    ("jpeg", "image/jpeg"),
    ("webp", "image/webp"),
    ("gif", "image/gif"),
    ("avif", "image/avif"),
];

/// The ceiling on a backdrop, before base64.
///
/// A data URI is a string held in the page, and base64 adds a third again on
/// top of the file. Twelve megabytes of photograph becomes sixteen megabytes of
/// string in the webview, which is already more than a backdrop is worth. The
/// limit exists so that choosing a RAW export by mistake is an error message
/// rather than a window that stops responding.
pub const LARGEST: u64 = 12 * 1024 * 1024;

fn path_in(dir: &Path) -> PathBuf {
    dir.join("backdrop.json")
}

/// Read the choice. A missing or unreadable file means "no preference yet",
/// which is the bloom.
pub fn load() -> Choice {
    load_from(&crate::app::data_dir())
}

pub fn load_from(dir: &Path) -> Choice {
    let text = match std::fs::read_to_string(path_in(dir)) {
        Ok(t) => t,
        Err(_) => return Choice::Bloom,
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return Choice::Bloom,
    };
    match value.get("kind").and_then(serde_json::Value::as_str) {
        Some("none") => Choice::None,
        Some("image") => value
            .get("path")
            .and_then(serde_json::Value::as_str)
            .map(|p| Choice::Image(PathBuf::from(p)))
            .unwrap_or(Choice::Bloom),
        _ => Choice::Bloom,
    }
}

/// Remember the choice. Validated first: a path that is not a readable,
/// decodable image of a sane size is refused here rather than saved and then
/// found to be broken every time the window opens.
pub fn save(choice: &Choice) -> Result<(), String> {
    save_in(&crate::app::data_dir(), choice)
}

pub fn save_in(dir: &Path, choice: &Choice) -> Result<(), String> {
    // Absolute, always. A relative path is resolved against the working
    // directory of whoever is asking, and the two askers here have different
    // ones: `sightline backdrop wallpaper.png` typed in the folder holding the
    // file, and a window started from somewhere else entirely. Stored as typed,
    // it works once and then silently falls back to the bloom forever after,
    // with no error anywhere — which is exactly what it did.
    let choice = &match choice {
        Choice::Image(path) => Choice::Image(
            std::fs::canonicalize(path).map_err(|e| format!("{}: {e}", path.display()))?,
        ),
        other => other.clone(),
    };
    if let Choice::Image(path) = choice {
        // Reads it once, and throws the bytes away. The point is that the
        // failure happens now, while somebody is looking at the thing they
        // chose it in.
        read_image(path)?;
    }
    let value = match choice {
        Choice::Bloom => serde_json::json!({ "kind": "bloom" }),
        Choice::None => serde_json::json!({ "kind": "none" }),
        Choice::Image(p) => serde_json::json!({ "kind": "image", "path": p.to_string_lossy() }),
    };
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    std::fs::write(
        path_in(dir),
        serde_json::to_string_pretty(&value).map_err(|e| e.to_string())?,
    )
    .map_err(|e| format!("could not remember the backdrop: {e}"))
}

/// The bytes of a chosen image, with the mime type the page will need.
///
/// Every refusal here names the file and says what is wrong with it, because
/// the alternative — a window that opens with no backdrop and no explanation —
/// is indistinguishable from the feature not working.
fn read_image(path: &Path) -> Result<(String, Vec<u8>), String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mime = DECODABLE
        .iter()
        .find(|(e, _)| *e == ext)
        .map(|(_, m)| *m)
        .ok_or_else(|| {
            let known: Vec<&str> = DECODABLE.iter().map(|(e, _)| *e).collect();
            format!(
                "{} is not an image this window can paint. It reads: {}.",
                path.display(),
                known.join(", ")
            )
        })?;
    let size = std::fs::metadata(path)
        .map_err(|e| format!("{}: {e}", path.display()))?
        .len();
    if size > LARGEST {
        return Err(format!(
            "{} is {:.1} MB, and a backdrop may be at most {} MB. It is held in \
             the window as text, which is a third larger again than the file.",
            path.display(),
            size as f64 / 1_048_576.0,
            LARGEST / 1_048_576,
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok((mime.to_string(), bytes))
}

/// What the window should paint, ready to be handed to the page.
///
/// `Image` comes back as a data URI because the content security policy permits
/// `data:` and no other source. The other two carry no payload — the stylesheet
/// already knows what a bloom looks like.
pub fn painted() -> (String, Option<String>) {
    match load() {
        Choice::Bloom => ("bloom".into(), None),
        Choice::None => ("none".into(), None),
        Choice::Image(path) => match read_image(&path) {
            Ok((mime, bytes)) => {
                let encoded = base64(&bytes);
                (
                    "image".into(),
                    Some(format!("data:{mime};base64,{encoded}")),
                )
            }
            // The file was there when it was chosen and is not now. Falling back
            // to the bloom rather than to nothing: a window that opens looking
            // wrong is a better failure than one that opens looking broken.
            Err(_) => ("bloom".into(), None),
        },
    }
}

/// Base64, by hand, because this crate has no encoder and one function is a
/// smaller thing to own than a dependency.
fn base64(bytes: &[u8]) -> String {
    const SET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(SET[(n >> 18) as usize & 63] as char);
        out.push(SET[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            SET[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            SET[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The smallest thing that passes validation.
    const ONE_PIXEL: [u8; 33] = [
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F,
        0x15, 0xC4, 0x89,
    ];

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sightline-backdrop-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn no_preference_yet_is_the_bloom_not_an_error() {
        // The commonest state by far, and it must not be a failure: a machine
        // that has never chosen one still has to open a window.
        assert_eq!(load_from(&scratch("empty")), Choice::Bloom);
    }

    #[test]
    fn a_file_the_window_cannot_paint_is_refused_by_name() {
        let dir = scratch("wrong-kind");
        let bad = dir.join("notes.txt");
        std::fs::write(&bad, "not an image").unwrap();
        let why = save_in(&dir, &Choice::Image(bad)).expect_err("a .txt is not a backdrop");
        assert!(
            why.contains("png"),
            "the refusal has to say what it does read: {why}"
        );
        // And nothing was remembered, so the next window open is unaffected.
        assert_eq!(load_from(&dir), Choice::Bloom);
    }

    #[test]
    fn something_far_too_large_is_refused_before_it_is_remembered() {
        let dir = scratch("too-big");
        let huge = dir.join("wallpaper.png");
        std::fs::write(&huge, vec![0u8; (LARGEST + 1) as usize]).unwrap();
        let why = save_in(&dir, &Choice::Image(huge)).expect_err("over the ceiling");
        assert!(
            why.contains("MB"),
            "say how big it is and how big it may be: {why}"
        );
        assert_eq!(load_from(&dir), Choice::Bloom);
    }

    #[test]
    fn a_choice_survives_being_written_and_read_back() {
        let dir = scratch("round-trip");
        // A one-pixel PNG, so the validation has something real to accept.
        let png = dir.join("one.png");
        std::fs::write(
            &png,
            [
                0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48,
                0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
                0x00, 0x1F, 0x15, 0xC4, 0x89,
            ],
        )
        .unwrap();
        save_in(&dir, &Choice::Image(png.clone())).unwrap();
        assert_eq!(load_from(&dir), Choice::Image(png));
        save_in(&dir, &Choice::None).unwrap();
        assert_eq!(load_from(&dir), Choice::None);
    }

    #[test]
    fn a_relative_path_is_made_absolute_before_it_is_remembered() {
        // The bug this was written for: `sightline backdrop wallpaper.png`,
        // typed in the folder holding the file, stored the name verbatim. The
        // window has a different working directory, could not find it, and fell
        // back to the bloom with no error anywhere — the feature simply did
        // nothing, and looked like a CSS problem.
        let dir = scratch("relative");
        let png = dir.join("one.png");
        std::fs::write(&png, ONE_PIXEL).unwrap();

        let here = std::env::current_dir().unwrap();
        std::env::set_current_dir(&dir).unwrap();
        let saved = save_in(&dir, &Choice::Image(PathBuf::from("one.png")));
        std::env::set_current_dir(here).unwrap();
        saved.unwrap();

        match load_from(&dir) {
            Choice::Image(p) => assert!(
                p.is_absolute(),
                "a path only its author's shell can resolve is not a stored preference: {}",
                p.display()
            ),
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn the_encoder_agrees_with_the_specification() {
        // Hand-written, so it is checked against the RFC's own examples rather
        // than against itself. The padding cases are where these go wrong.
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foob"), "Zm9vYg==");
        assert_eq!(base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
        // A byte range that exercises the high bits of every sextet.
        let all: Vec<u8> = (0u8..=255).collect();
        assert_eq!(base64(&all).len(), 344);
        assert!(base64(&all).starts_with("AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8g"));
    }
}
