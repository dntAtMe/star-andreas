// CPostEffects::HeatHazeFX (0x701780): 180 tiles of the screen copy, each a srcW x srcH
// patch drawn dstW x dstH about the same centre (x50/47), blended at `alpha` in tile order.
// Mode 2 limits it to the heat-haze sprites' footprint (the stencil mask of the original;
// sprite_CJ's alpha > 0 region is the disc |uv - 0.5| < 0.477).
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

struct HeatHaze {
    // alpha (0..1), mode (0 off, 1 full screen, 2 masked), mask quad count, unused
    params: vec4<f32>,
    // even: dst rect (x, y, w, h) in pixels; odd: src rect
    tiles: array<vec4<f32>, 360>,
    // per quad: (origin.xy, edgeU.xy), (edgeV.xy, alpha, unused) in pixels
    masks: array<vec4<f32>, 128>,
}

@group(0) @binding(2) var<uniform> hh: HeatHaze;

fn in_mask(p: vec2<f32>) -> bool {
    let n = u32(hh.params.z);
    for (var i = 0u; i < n; i++) {
        let a = hh.masks[2u * i];
        let b = hh.masks[2u * i + 1u];
        let e1 = a.zw;
        let e2 = b.xy;
        let det = e1.x * e2.y - e1.y * e2.x;
        if abs(det) < 1e-6 {
            continue;
        }
        let d = p - a.xy;
        let s = (d.x * e2.y - d.y * e2.x) / det;
        let t = (e1.x * d.y - e1.y * d.x) / det;
        if s >= 0.0 && s <= 1.0 && t >= 0.0 && t <= 1.0 && length(vec2(s, t) - 0.5) < 0.477 {
            return true;
        }
    }
    return false;
}

@fragment
fn fragment(in: FullscreenVertexOutput) -> @location(0) vec4<f32> {
    let base = textureSampleLevel(screen_texture, texture_sampler, in.uv, 0.0);
    let mode = hh.params.y;
    if mode < 0.5 {
        return base;
    }
    let p = in.position.xy;
    if mode > 1.5 && !in_mask(p) {
        return base;
    }
    let size = vec2<f32>(textureDimensions(screen_texture));
    var c = base.rgb;
    let a = hh.params.x;
    for (var i = 0u; i < 180u; i++) {
        let d = hh.tiles[2u * i];
        if p.x >= d.x && p.x < d.x + d.z && p.y >= d.y && p.y < d.y + d.w {
            let s = hh.tiles[2u * i + 1u];
            let src = s.xy + (p - d.xy) / d.zw * s.zw;
            let col = textureSampleLevel(screen_texture, texture_sampler, src / size, 0.0).rgb;
            c = mix(c, col, a);
        }
    }
    return vec4(c, base.a);
}
