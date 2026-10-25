// Waterfall pipeline (PLAN.md §4.1): a static grid of (slice, bin)
// vertices; the magnitude comes from the history texture ring, the head
// uniform resolves the wrap. Colors are depth-cueued in the vertex stage.
//
// Clean-room: y mapping and color factors follow the verified sndpeek
// constants (linear `gain·freq_scale·1.8·sqrt(25·|X|)`, dB
// `gain·freq_scale·(20·log10(|X|/8)+80)/80`, past greenish `(0.4·f,
// 1.0·f, 0.4·f)`, "now" line cyan `(0.4, 1.0, 1.0)`), implemented fresh.
//
// Bin geometry: `bin` arrives as the vertex attribute (the CPU grid
// covers exactly the current FFT's valid bins), so no compile-time bin
// constant is needed — the history texture is MAX_BINS wide and only
// columns < the FFT's bin_count are written.

struct WaterUniforms {
    view_proj: mat4x4<f32>,
    head: u32,
    depth: u32,
    spacing: f32,
    z_offset: f32,
    db: f32,      // 1.0 = dB magnitude mode
    rainbow: f32, // 1.0 = rainbow depth colors
    gain: f32,
    freq_scale: f32,
    drift: f32, // slice-fade animation factor in 0…1
    pad: vec3<f32>,
    past_color: vec4<f32>,  // theme (PLAN.md §6.1)
    future_color: vec4<f32>,
    now_color: vec4<f32>,
    db_floor: f32,
    db_ceil: f32,
    surface: f32, // 1.0 = filled-surface mode (M8)
    pad_end: f32,
}

@group(0) @binding(0) var<uniform> u: WaterUniforms;
@group(0) @binding(1) var hist: texture_2d<f32>;
// Filled-surface ramp, 256 samples in a 1-row texture (theme-driven).
@group(0) @binding(2) var ramp: texture_2d<f32>;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
    // Normalized curve height, `clamp(y_lin, 0, 1)`: the heatmap
    // coordinate the fragment stage re-maps when `rainbow` is on.
    // Negative marks vertices whose color is final in the vertex stage
    // (theme paths, arriving-plane blends) — the fragment pass keeps
    // the vertex color and never runs the hue ramp on them.
    @location(1) height: f32,
}

// Standard HSV→RGB (hue in turns).
fn hue_rgb(h: f32) -> vec3<f32> {
    let s = 6.0 * fract(h);
    return clamp(
        vec3(
            abs(s - 3.0) - 1.0,
            2.0 - abs(s - 2.0),
            2.0 - abs(s - 4.0),
        ),
        vec3(0.0),
        vec3(1.0),
    );
}

// Rainbow = height heatmap, not depth: hue sweeps ~0.72 → 0 turns as the
// normalized curve height t rises 0 → 1 — violet-blue at the floor,
// cyan/green through the middle, yellow → red at the crest (classic heat
// ramp). Height is the MAGNITUDE mapping (`y_lin`), not the drawn y, so
// linear and dB display modes color the same curve identically.
fn heat_hue(t: f32) -> f32 {
    return (1.0 - t) * 0.72;
}

// Surface-only dimming floor. The raw depth fade
// `fval = (depth − slice + drift) / depth` bottoms out at ≈ 1/depth, which
// for a FILLED surface multiplies whole slices into the background — the
// structure vanishes. Thin depth-cued lines read fine with the full fade,
// so the floor is applied only on the filled-surface path: fval is remapped
// so the oldest slice still shows SURFACE_FLOOR of its ramp color and the
// fade stays monotonic up to full brightness at the newest slices.
const SURFACE_FLOOR: f32 = 0.35;
fn surface_fade(fval: f32) -> f32 {
    return mix(SURFACE_FLOOR, 1.0, clamp(fval, 0.0, 1.0));
}

@vertex
fn vs_main(
    @location(0) x: f32,
    @location(1) bin: u32,
    @location(2) slice: u32,
) -> VsOut {
    let s = f32(slice);
    let depth = f32(u.depth);
    let z = u.z_offset - s * u.spacing;
    // Ring row: slice 0 = newest = just-written row (no CPU shifting).
    let row = i32((u.head + u.depth - 1u - slice) % u.depth);
    let mag = textureLoad(hist, vec2<u32>(bin, u32(row)), 0).r;

    let y_lin = u.gain * u.freq_scale * 1.8 * sqrt(25.0 * mag);
    let m_db = max(mag, 1e-30);
    // WGSL has no `log10` builtin; log10(x) == log2(x) / 3.32192809486.
    let y_db = u.gain * u.freq_scale * ((20.0 * log2(m_db / 8.0) / 3.32192809486 - u.db_floor) / (u.db_ceil - u.db_floor));
    let y = select(y_lin, y_db, u.db > 0.5);

    let fval = (depth - s + u.drift) / depth;

    // Filled-surface mode (M8). Rainbow on: the theme surface ramp is
    // REPLACED by the same per-vertex height heatmap the line mode uses
    // (the fragment stage refines it per-pixel) — a surface vertex whose
    // neighbor was the theme ramp keeps a height ≥ 0 handoff. Rainbow
    // off: the theme ramp with the floored depth dimming
    // (`surface_fade`; the un-floored `fval` stays line-mode-only).
    if u.surface > 0.5 {
        let t = clamp(y_lin, 0.0, 1.0);
        var c = select(
            hue_rgb(heat_hue(t)),
            textureLoad(ramp, vec2<u32>(u32(t * 255.0 + 0.5), 0u), 0).rgb * surface_fade(fval),
            u.rainbow <= 0.5,
        );
        var heat_t = select(t, -1.0, u.rainbow <= 0.5);
        if (slice == 0u) {
            // The arriving plane blends now_line → future (theme),
            // rainbow or not: a live marker, not a depth cue.
            c = mix(u.now_color.rgb, u.future_color.rgb, u.drift);
            heat_t = -1.0;
        }
        return VsOut(
            u.view_proj * vec4<f32>(x, y, z, 1.0),
            c,
            heat_t,
        );
    }

    let past = u.past_color.rgb * fval;
    let heat = hue_rgb(heat_hue(clamp(y_lin, 0.0, 1.0)));
    var color = select(past, heat, u.rainbow > 0.5);
    var heat_t = select(-1.0, clamp(y_lin, 0.0, 1.0), u.rainbow > 0.5);
    if (slice == 0u) {
        color = u.now_color.rgb; // the "now" line
        heat_t = -1.0;
    }

    return VsOut(
        u.view_proj * vec4<f32>(x, y, z, 1.0),
        color,
        heat_t,
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    // Height heatmap (rainbow on): per-pixel hue from the interpolated
    // curve height for a smooth gradient along every segment/triangle.
    // Negative height = the vertex color is already final.
    let c = select(in.color, hue_rgb(heat_hue(in.height)), in.height >= 0.0);
    return vec4<f32>(c, 1.0);
}
