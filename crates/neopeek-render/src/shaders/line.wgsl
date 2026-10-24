// Waveform / Lissajous line strips (PLAN.md §4.1): plain world-space
// positions + one uniform color per pipeline instantiation (waveform blue
// (0.4, 0.4, 1.0), Lissajous yellow (1.0, 1.0, 0.5)).

struct LineUniforms {
    view_proj: mat4x4<f32>,
    color: vec4<f32>,
}

@group(0) @binding(0) var<uniform> u: LineUniforms;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec3<f32>,
}

@vertex
fn vs_main(@location(0) pos: vec3<f32>) -> VsOut {
    return VsOut(
        u.view_proj * vec4<f32>(pos, 1.0),
        u.color.rgb,
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return vec4<f32>(in.color, 1.0);
}
