// CPostEffects::HeatHazeFX (0x701780): 180 tiles of the screen copy, each a srcW x srcH
// patch drawn dstW x dstH about the same centre (x50/47), blended at `alpha` in tile order.
// Mode 2 limits it to the heat-haze sprites' footprint (the stencil mask of the original;
// sprite_CJ's alpha > 0 region is the disc |uv - 0.5| < 0.477).
#import bevy_core_pipeline::fullscreen_vertex_shader::FullscreenVertexOutput

@group(0) @binding(0) var screen_texture: texture_2d<f32>;
@group(0) @binding(1) var texture_sampler: sampler;

struct HeatHaze {
    // colour filter: rgb * a / 255, k1.w = enabled
    k1: vec4<f32>,
    k2: vec4<f32>,
    // alpha (0..1), mode (0 off, 1 full screen, 2 masked), mask quad count, unused
    params: vec4<f32>,
    // even: dst rect (x, y, w, h) in pixels; odd: src rect
    tiles: array<vec4<f32>, 360>,
    // per quad: (origin.xy, edgeU.xy), (edgeV.xy, alpha, unused) in pixels
    masks: array<vec4<f32>, 128>,
    // enhanced: sun shafts (sun uv xy, intensity, keep HDR > 1 in the colour filter) and colour
    rays: vec4<f32>,
    rays_col: vec4<f32>,
}

@group(0) @binding(2) var<uniform> hh: HeatHaze;

fn to_gamma(c: vec3<f32>) -> vec3<f32> {
    let lo = c * 12.92;
    let hi = 1.055 * pow(c, vec3(1.0 / 2.4)) - 0.055;
    return select(hi, lo, c <= vec3(0.0031308));
}

fn to_linear(c: vec3<f32>) -> vec3<f32> {
    let lo = c / 12.92;
    let hi = pow((c + 0.055) / 1.055, vec3(2.4));
    return select(hi, lo, c <= vec3(0.04045));
}

// CPostEffects::ColourFilter (0x703650), 8-bit gamma: two additive passes of one copy.
fn colour_filter(c: vec3<f32>) -> vec3<f32> {
    if hh.k1.w < 0.5 {
        return c;
    }
    let s = to_gamma(clamp(c, vec3(0.0), vec3(1.0)));
    let p1 = min(vec3(1.0), s + s * hh.k1.rgb);
    let f = to_linear(min(vec3(1.0), p1 + s * hh.k2.rgb));
    // Enhanced (HDR): what is above 1.0 passes through for the bloom.
    return f + select(vec3(0.0), max(c - vec3(1.0), vec3(0.0)), hh.rays.w > 0.5);
}

// Enhanced: screen-space sun shafts. March from the pixel toward the sun, gathering the
// bright (sky) pixels with decay; the dark skyline blocks them.
fn sun_shafts(uv: vec2<f32>) -> vec3<f32> {
    let k = hh.rays.z;
    if k <= 0.0 {
        return vec3(0.0);
    }
    let n = 40;
    let delta = (hh.rays.xy - uv) / f32(n) * 0.9;
    var p = uv;
    var w = 1.0;
    var acc = 0.0;
    for (var i = 0; i < n; i++) {
        p += delta;
        let c = textureSampleLevel(screen_texture, texture_sampler, clamp(p, vec2(0.0), vec2(1.0)), 0.0).rgb;
        let l = dot(c, vec3(0.2126, 0.7152, 0.0722));
        acc += smoothstep(0.45, 0.9, l) * w;
        w *= 0.955;
    }
    // Fade with the distance to the sun on screen.
    let d = length((uv - hh.rays.xy) * vec2(1.0, 0.6));
    return hh.rays_col.rgb * (acc / f32(n)) * k * (1.0 - smoothstep(0.2, 1.1, d));
}

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
    let raw = textureSampleLevel(screen_texture, texture_sampler, in.uv, 0.0);
    let base = vec4(colour_filter(raw.rgb) + sun_shafts(in.uv), raw.a);
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
            let col = colour_filter(textureSampleLevel(screen_texture, texture_sampler, src / size, 0.0).rgb);
            c = mix(c, col, a);
        }
    }
    return vec4(c, base.a);
}
