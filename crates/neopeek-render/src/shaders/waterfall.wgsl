// Waterfall pipeline (PLAN.md §4.1): a static grid of (slice, bin)
// vertices; the magnitude comes from the history texture ring, the head
// uniform resolves the wrap. Colors are depth-cueued in the vertex stage.
//
// Clean-room: y mapping and color factors follow the verified sndpeek
// constants (linear `gain·freq_scale·1.8·sqrt(25·|X|)`, dB
// `gain·freq_scale·(20·log10(|X|/8)+80)/80`, past greenish `(0.4·f,
// 1.0·f, 0.4·f)`, "now" line cyan `(0.4, 1.0, 1.0)`), implemented fresh.

const BINS: u32 = 1024u;

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
}

@group(0) @binding(0) var<uniform> u: WaterUniforms;
@group(0) @binding(1) var hist: texture_2d<f32>;

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
    let y_db = u.gain * u.freq_scale * ((20.0 * log10(m_db / 8.0) + 80.0) / 80.0);
    let y = select(y_lin, y_db, u.db > 0.5);

    let fval = (depth - s + u.drift) / depth;

    let past = vec3<f32>(0.4 * fval, 1.0 * fval, 0.4 * fval);
    let rainbow = hue_rgb(0.15 + 0.55 * fval);
    var color = select(past, rainbow, u.rainbow > 0.5);
    if (slice == 0u) {
        color = vec3<f32>(0.4, 1.0, 1.0); // the "now" line
    }

    return VsOut {
        clip: u.view_proj * vec4<f32>(x, y, z, 1.0),
        color: color,
    };
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
