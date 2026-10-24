// HUD meters quad shader (PLAN.md §4.5): screen-space axis-aligned rects,
// one instance per rect; the vertex buffer holds rect xywh + rgba and the
// four strip corners are derived from the vertex index.

struct HudUniforms {
    vp: mat4x4<f32>,
}

@group(0) @binding(0) var<uniform> u: HudUniforms;

struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
}

@vertex
fn vs_main(
    @builtin(vertex_index) vi: u32,
    @location(0) rect: vec4<f32>,
    @location(1) color: vec4<f32>,
) -> VsOut {
    // Strip corner order: (0,0) → (1,0) → (0,1) → (1,1).
    let corner = vec2<f32>(f32(vi & 1u), f32(vi >> 1u));
    let pos = vec2<f32>(rect.x, rect.y) + corner * vec2<f32>(rect.z, rect.w);
    return VsOut(
        u.vp * vec4<f32>(pos, 0.0, 1.0),
        color,
    );
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
