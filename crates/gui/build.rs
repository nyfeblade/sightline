fn main() {
    // `tauri_build::build()` registers tauri.conf.json and the icons, but not
    // the frontend the config points at — so editing app.css alone left the
    // crate fresh and `cargo build` handed back a binary carrying the previous
    // build's stylesheet. That is the worst possible failure mode for UI work:
    // the change is in the tree, the build is green, and what you screenshot is
    // the old one. It cost a wrong diagnosis in this session — a rule was
    // declared to be losing a cascade fight it had never been in.
    for entry in std::fs::read_dir("ui").expect("crates/gui/ui") {
        let path = entry.expect("ui entry").path();
        println!("cargo:rerun-if-changed={}", path.display());
    }
    // And the directory itself, so a new or deleted file is noticed too.
    println!("cargo:rerun-if-changed=ui");

    tauri_build::build()
}
