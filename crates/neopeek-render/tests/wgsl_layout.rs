//! Cross-check that every Rust-side uniform struct matches its WGSL
//! counterpart offset-for-offset, using naga (the parser wgpu 30 ships)
//! as the authority for the WGSL layout.
//!
//! The headless gate cannot create a wgpu device, so a layout drift (the
//! 176 B uniform buffer shipped in M7/M8 vs the 192 B struct the shader
//! actually expects) survived `cargo test` and only died at the user's
//! first draw with a fatal validation error. Here the WGSL struct's span
//! and member offsets/sizes are read straight out of naga's IR —
//! offsets per the WGSL uniform address-space layout rules (vec4-like
//! members align to 16, struct end rounds up to max member alignment) —
//! and compared field-by-field against Rust offsets/sizes computed with
//! `offset_of!`/`size_of!`.
//!
//! Conventions:
//! - WGSL member names must equal Rust field names; every WGSL member
//!   must have a same-named, same-offset Rust field whose size exactly
//!   reaches the next WGSL member's offset (or the struct span), so
//!   neither side leaves an unaccounted gap.
//! - Rust helper fields that carry WGSL alignment gaps but have no WGSL
//!   member may exist under names starting with `_`; they must fill a
//!   genuine WGSL gap (inside (member extent, next member offset]).
//! - A mismatch fails with a full field-by-field diff.

use std::path::PathBuf;

use naga::proc::Layouter;
use naga::TypeInner;

/// Parses and validates one shader, then returns (span, members) of the
/// struct named `struct_name`
/// — members as (name, byte offset, size) from naga's layout.
fn wgsl_struct_layout(shader_file: &str, struct_name: &str) -> (u32, Vec<(String, u32, u32)>) {
    let path: PathBuf =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/shaders").join(shader_file);
    let source = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    let name = format!("shaders/{shader_file}");
    let module = naga::front::wgsl::parse_str(&source)
        .unwrap_or_else(|e| panic!("{name}: WGSL parse error: {e}"));
    naga::valid::Validator::new(naga::valid::ValidationFlags::all(), naga::valid::Capabilities::all())
        .validate(&module)
        .unwrap_or_else(|e| panic!("{name}: WGSL validation error: {e}"));

    let ty_handle = module
        .types
        .iter()
        .filter_map(|(h, ty)| (ty.name.as_deref() == Some(struct_name)).then_some(h))
        .next()
        .unwrap_or_else(|| panic!("{name}: no struct `{struct_name}` defined"));
    let ty = &module.types[ty_handle];
    let TypeInner::Struct { members, span } = &ty.inner else {
        panic!("{name}: `{struct_name}` is not a struct");
    };
    let mut layouter = Layouter::default();
    layouter
        .update(module.to_ctx())
        .unwrap_or_else(|e| panic!("{name}: WGSL layout error: {e:?}"));
    let members = members
        .iter()
        .map(|m| {
            let size = layouter[m.ty].size;
            (m.name.clone().unwrap_or_else(|| "<anon>".into()), m.offset, size)
        })
        .collect();
    (*span, members)
}

/// Compares the WGSL table against the Rust table
/// `&[(field_name, offset, field_size)]`. On any difference the failing
/// assertion prints the field-by-field diff.
fn expect_match(
    shader_file: &'static str,
    struct_name: &'static str,
    rust_size: usize,
    rust: &[(&str, usize, usize)],
) {
    let (wgsl_span, wgsl) = wgsl_struct_layout(shader_file, struct_name);
    let mut diff = String::new();

    // Boundary after member i = next member's offset (or the span).
    let next_start = |i: usize, off: u32| -> u64 {
        wgsl.get(i + 1).map_or(wgsl_span as u64, |(_, o, _)| *o as u64).max(off as u64)
    };

    for (i, (wg_name, wg_off, wg_size)) in wgsl.iter().enumerate() {
        let extent = next_start(i, *wg_off);
        match rust.iter().find(|(r_name, _, _)| r_name == wg_name) {
            Some((_, r_off, _)) if r_off != &(*wg_off as usize) => diff.push_str(&format!(
                "  {wg_name}: WGSL offset {wg_off}, Rust offset {r_off}\n"
            )),
            Some((_, _, r_len)) if *r_len < *wg_size as usize
                || *r_len > extent as usize - *wg_off as usize =>
            {
                diff.push_str(&format!(
                    "  {wg_name}: WGSL extent {} B (offset {wg_off}), Rust field {r_len} B\n",
                    extent - *wg_off as u64
                ))
            }
            None => diff.push_str(&format!(
                "  {wg_name}: WGSL offset {wg_off}, MISSING from Rust table\n"
            )),
            _ => {}
        }
    }
    for (r_name, r_off, r_len) in rust {
        let start = *r_off as u64;
        let end = start + *r_len as u64;
        let straddles = |o: u64| start < o && end > o;
        let is_gap_field = wgsl
            .iter()
            .enumerate()
            .any(|(i, (_, wg_off, wg_size))| {
                let prev_extent = (*wg_off + *wg_size) as u64;
                let next_off = next_start(i, *wg_off);
                start >= prev_extent && end <= next_off
            })
            && !wgsl.iter().any(|(_, wg_off, _)| straddles(*wg_off as u64));
        if !wgsl.iter().any(|(n, _, _)| n == r_name) {
            if !r_name.starts_with('_') {
                diff.push_str(&format!(
                    "  {r_name}: Rust-only field at {r_off} (prefix a `_` if it carries a WGSL alignment gap)\n"
                ));
            } else if !is_gap_field {
                diff.push_str(&format!(
                    "  {r_name}: helper field [{start}..{end}] is not a WGSL alignment gap\n"
                ));
            }
        }
    }
    if rust_size != wgsl_span as usize {
        diff.push_str(&format!("  struct size: WGSL {wgsl_span} B, Rust {rust_size} B\n"));
    }
    assert!(
        diff.is_empty(),
        "{shader_file}: `{struct_name}` layout mismatch (Rust must match WGSL):\n{}",
        diff.trim_end()
    );
    println!("{shader_file} `{struct_name}`: span {wgsl_span} B, Rust {rust_size} B — match");
}

#[test]
fn waterfall_uniforms_match_wgsl() {
    use neopeek_render::waterfall::WaterUniforms;
    let rust: &[(&str, usize, usize)] = &[
        ("view_proj", std::mem::offset_of!(WaterUniforms, view_proj), std::mem::size_of::<[f32; 16]>()),
        ("head", std::mem::offset_of!(WaterUniforms, head), std::mem::size_of::<u32>()),
        ("depth", std::mem::offset_of!(WaterUniforms, depth), std::mem::size_of::<u32>()),
        ("spacing", std::mem::offset_of!(WaterUniforms, spacing), std::mem::size_of::<f32>()),
        ("z_offset", std::mem::offset_of!(WaterUniforms, z_offset), std::mem::size_of::<f32>()),
        ("db", std::mem::offset_of!(WaterUniforms, db), std::mem::size_of::<f32>()),
        ("rainbow", std::mem::offset_of!(WaterUniforms, rainbow), std::mem::size_of::<f32>()),
        ("gain", std::mem::offset_of!(WaterUniforms, gain), std::mem::size_of::<f32>()),
        ("freq_scale", std::mem::offset_of!(WaterUniforms, freq_scale), std::mem::size_of::<f32>()),
        ("drift", std::mem::offset_of!(WaterUniforms, drift), std::mem::size_of::<f32>()),
        ("_align_hole", std::mem::offset_of!(WaterUniforms, _align_hole), std::mem::size_of::<[f32; 3]>()),
        ("pad", std::mem::offset_of!(WaterUniforms, pad), std::mem::size_of::<[f32; 4]>()),
        ("past_color", std::mem::offset_of!(WaterUniforms, past_color), std::mem::size_of::<[f32; 4]>()),
        ("future_color", std::mem::offset_of!(WaterUniforms, future_color), std::mem::size_of::<[f32; 4]>()),
        ("now_color", std::mem::offset_of!(WaterUniforms, now_color), std::mem::size_of::<[f32; 4]>()),
        ("db_floor", std::mem::offset_of!(WaterUniforms, db_floor), std::mem::size_of::<f32>()),
        ("db_ceil", std::mem::offset_of!(WaterUniforms, db_ceil), std::mem::size_of::<f32>()),
        ("surface", std::mem::offset_of!(WaterUniforms, surface), std::mem::size_of::<f32>()),
        ("pad_end", std::mem::offset_of!(WaterUniforms, pad_end), std::mem::size_of::<[f32; 1]>()),
    ];
    expect_match(
        "waterfall.wgsl",
        "WaterUniforms",
        std::mem::size_of::<WaterUniforms>(),
        rust,
    );
}

#[test]
fn line_uniforms_match_wgsl() {
    use neopeek_render::lines::LineUniforms;
    let rust: &[(&str, usize, usize)] = &[
        ("view_proj", std::mem::offset_of!(LineUniforms, view_proj), std::mem::size_of::<[f32; 16]>()),
        ("color", std::mem::offset_of!(LineUniforms, color), std::mem::size_of::<[f32; 4]>()),
    ];
    expect_match(
        "line.wgsl",
        "LineUniforms",
        std::mem::size_of::<LineUniforms>(),
        rust,
    );
}

#[test]
fn hud_uniforms_match_wgsl() {
    // The HUD uniform is a bare mat4 (`ortho_pixels`), no named Rust
    // struct; pin span == size_of::<[f32; 16]>() and the single member.
    expect_match("hud.wgsl", "HudUniforms", std::mem::size_of::<[f32; 16]>(), &[("vp", 0, 64)]);
}
