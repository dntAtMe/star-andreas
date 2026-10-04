// Map geometry the way CCustomBuildingDNPipeline draws it (timecycle.md §5.2), in gamma space:
//   prelit = ftol(night * DN + day * (1 - DN))
//   colour = saturate(prelit + ambient * lit) * materialColour * texture
// then SA's linear fog (start = timecyc FogSt, end = far clip, colour = sky bottom), view-z based.
#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_position_local_to_world}
#import bevy_pbr::view_transformations::position_world_to_clip
#import bevy_pbr::mesh_view_bindings::view

struct WorldMat {
    color: vec4<f32>,
    // x: lit (geometry has rpGEOMETRYLIGHT), y: alpha cutoff (mask mode, else < 0)
    params: vec4<f32>,
}

@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> mat: WorldMat;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var tex_sampler: sampler;
// [0] = (DN, unused, fog start, fog end), [1] = (ambient rgb, fog on), [2] = (fog colour rgb, 0)
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var<storage, read> globals: array<vec4<f32>>;

struct Vertex {
    @builtin(instance_index) instance_index: u32,
    @location(0) position: vec3<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) day: vec4<f32>,
    @location(3) night: vec4<f32>,
};

struct VOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) day: vec4<f32>,
    @location(2) night: vec4<f32>,
    @location(3) world: vec3<f32>,
};

@vertex
fn vertex(v: Vertex) -> VOut {
    var o: VOut;
    let w = mesh_position_local_to_world(get_world_from_local(v.instance_index), vec4<f32>(v.position, 1.0));
    o.clip = position_world_to_clip(w.xyz);
    o.uv = v.uv;
    o.day = v.day;
    o.night = v.night;
    o.world = w.xyz;
    return o;
}

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

@fragment
fn fragment(i: VOut) -> @location(0) vec4<f32> {
    let g0 = globals[0];
    let g1 = globals[1];
    let g2 = globals[2];
    let dn = clamp(g0.x, 0.0, 1.0);
    // Per channel truncation of the 0..255 blend.
    let prelit = floor((i.night * dn + i.day * (1.0 - dn)) * 255.0) / 255.0;
    let lit = vec4(prelit.rgb + g1.rgb * mat.params.x, prelit.a);
    let col = clamp(lit, vec4(0.0), vec4(1.0)) * mat.color;
    let t = textureSample(tex, tex_sampler, i.uv);
    var rgb = to_gamma(t.rgb) * col.rgb;
    let a = t.a * col.a;
    if mat.params.y >= 0.0 && a < mat.params.y {
        discard;
    }
    if g1.w > 0.5 {
        let z = -(view.view_from_world * vec4<f32>(i.world, 1.0)).z;
        let f = clamp((g0.w - z) / max(g0.w - g0.z, 0.001), 0.0, 1.0);
        rgb = mix(g2.rgb, rgb, f);
    }
    return vec4(to_linear(clamp(rgb, vec3(0.0), vec3(1.0))), a);
}
