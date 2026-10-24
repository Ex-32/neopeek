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

    // Filled-surface mode (M8): color from the theme ramp over the linear
    // height fraction; depth-cued like the line mode.
    if u.surface > 0.5 {
        let t = clamp(y_lin, 0.0, 1.0);
        var c = textureLoad(ramp, vec2<u32>(u32(t * 255.0 + 0.5), 0u), 0).rgb;
        c = c * fval;
        if (slice == 0u) {
            // The arriving plane blends now_line → future (theme).
            c = mix(u.now_color.rgb, u.future_color.rgb, u.drift);
        }
        return VsOut(
            u.view_proj * vec4<f32>(x, y, z, 1.0),
            c,
        );
    }

    let past = u.past_color.rgb * fval;
    let rainbow = hue_rgb(0.15 + 0.55 * fval);
    var color = select(past, rainbow, u.rainbow > 0.5);
    if (slice == 0u) {
        color = u.now_color.rgb; // the "now" line
    }

    return VsOut(
        u.view_proj * vec4<f32>(x, y, z, 1.0),
        color,
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
