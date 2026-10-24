//! Regression guard for WGSL parseability.
//!
//! wgpu only parses shaders at `Device::create_shader_module` (runtime,
//! fatal), so Rust-side gates never see WGSL syntax errors (e.g. Rust-style
//! struct-literal returns, which WGSL rejects). This test runs the same
//! frontend naga ships (the parser wgpu 30 uses) over every `.wgsl` file in
//! `src/shaders/` so such bugs die in `cargo test`.
//!
//! Files are discovered via `std::fs` relative to `CARGO_MANIFEST_DIR` so a
//! new shader added to the directory is covered automatically without
//! touching a list; `include_str!`-style lists go stale silently.

use std::path::PathBuf;

const CAPS: naga::valid::Capabilities = naga::valid::Capabilities::all();

#[test]
fn all_shaders_parse_and_validate() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/shaders");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
        .expect("src/shaders directory to exist")
        .map(|e| e.expect("readable shader dir entry").path())
        .filter(|p| p.extension().is_some_and(|e| e == "wgsl"))
        .collect();
    files.sort();
    assert!(
        !files.is_empty(),
        "expected at least one .wgsl file in src/shaders/"
    );

    for path in files {
        let source = std::fs::read_to_string(&path).expect("readable shader file");
        let name = format!("shaders/{}", path.file_name().unwrap().to_string_lossy());
        let module = match naga::front::wgsl::parse_str(&source) {
            Ok(m) => m,
            Err(e) => panic!("{name}: WGSL parse error: {e}"),
        };
        if let Err(e) = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            CAPS,
        )
        .validate(&module)
        {
            panic!("{name}: WGSL validation error: {e}");
        }
        // Every shader in the crate supplies both a @vertex and a @fragment
        // entry point; require them so a rename can't silently drop coverage.
        for ep in ["vs_main", "fs_main"] {
            assert!(
                module.entry_points.iter().any(|e| e.name == ep),
                "{name}: missing entry point `{ep}`"
            );
        }
    }
}
