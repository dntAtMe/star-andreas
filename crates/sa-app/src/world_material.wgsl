// Map geometry the way CCustomBuildingDNPipeline draws it (timecycle.md §5.2), in gamma space:
//   prelit = ftol(night * DN + day * (1 - DN))
//   colour = saturate(prelit + ambient * lit) * materialColour * texture
// then SA's linear fog (start = timecyc FogSt, end = far clip, colour = sky bottom), view-z based.
#import bevy_pbr::mesh_functions::{get_world_from_local, mesh_position_local_to_world}
#import bevy_pbr::view_transformations::position_world_to_clip
#import bevy_pbr::mesh_view_bindings::{view, lights}
#import bevy_pbr::mesh_view_bindings as view_bindings
#import bevy_pbr::shadows::fetch_directional_shadow
#import bevy_pbr::clustered_forward as clustering
#import bevy_pbr::lighting::getDistanceAttenuation

struct WorldMat {
    color: vec4<f32>,
    // x: lit (geometry has rpGEOMETRYLIGHT), y: alpha cutoff (mask mode, else < 0)
    params: vec4<f32>,
}

#ifdef BINDLESS
#import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d}
#import bevy_pbr::mesh_bindings::mesh
// The bindless index table: binding 0 = the material data, 1 = texture, 2 = sampler, 3 = globals.
struct WorldMatBindings {
    material: u32,
    tex: u32,
    tex_sampler: u32,
    globals: u32,
}
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<storage> material_indices: array<WorldMatBindings>;
@group(#{MATERIAL_BIND_GROUP}) @binding(10) var<storage> material_array: array<WorldMat>;
#else
@group(#{MATERIAL_BIND_GROUP}) @binding(0) var<uniform> mat_u: WorldMat;
@group(#{MATERIAL_BIND_GROUP}) @binding(1) var tex: texture_2d<f32>;
@group(#{MATERIAL_BIND_GROUP}) @binding(2) var tex_sampler: sampler;
@group(#{MATERIAL_BIND_GROUP}) @binding(3) var globals_tex: texture_2d<f32>;
#endif
// The globals texels: [0] = (DN, unused, fog start, fog end), [1] = (ambient rgb, fog on),
// [2] = (fog colour rgb, point lights on), [3] = (toward the sun, shadow strength) for the
// enhanced graphics (strength 0 = classic), [4] = (sun haze colour, strength)

fn material_slot(instance: u32) -> u32 {
#ifdef BINDLESS
    return mesh[instance].material_and_lightmap_bind_group_slot & 0xffffu;
#else
    return 0u;
#endif
}

fn load_mat(slot: u32) -> WorldMat {
#ifdef BINDLESS
    return material_array[material_indices[slot].material];
#else
    return mat_u;
#endif
}

fn sample_tex(slot: u32, uv: vec2<f32>) -> vec4<f32> {
#ifdef BINDLESS
    return textureSample(bindless_textures_2d[material_indices[slot].tex], bindless_samplers_filtering[material_indices[slot].tex_sampler], uv);
#else
    return textureSample(tex, tex_sampler, uv);
#endif
}

fn global(slot: u32, k: i32) -> vec4<f32> {
#ifdef BINDLESS
    return textureLoad(bindless_textures_2d[material_indices[slot].globals], vec2<i32>(k, 0), 0);
#else
    return textureLoad(globals_tex, vec2<i32>(k, 0), 0);
#endif
}

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
    @location(4) @interpolate(flat) instance: u32,
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
    o.instance = v.instance_index;
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
    let slot = material_slot(i.instance);
    let mat = load_mat(slot);
    let g0 = global(slot, 0);
    let g1 = global(slot, 1);
    let g2 = global(slot, 2);
    let dn = clamp(g0.x, 0.0, 1.0);
    // Per channel truncation of the 0..255 blend.
    let prelit = floor((i.night * dn + i.day * (1.0 - dn)) * 255.0) / 255.0;
    let lit = vec4(prelit.rgb + g1.rgb * mat.params.x, prelit.a);
    let col = clamp(lit, vec4(0.0), vec4(1.0)) * mat.color;
    let t = sample_tex(slot, i.uv);
    var rgb = to_gamma(t.rgb) * col.rgb;
    let a = t.a * col.a;
    if mat.params.y >= 0.0 && a < mat.params.y {
        discard;
    }
    // Enhanced: the sun's shadow map darkens the baked light where the sun is blocked (on
    // surfaces facing it), and sunlit faces get a little warmth.
    let g3 = global(slot, 3);
    if g3.w > 0.0 && lights.n_directional_lights > 0u
        && (lights.directional_lights[0].flags & 1u) != 0u {
        var n = normalize(cross(dpdx(i.world), dpdy(i.world)));
        if dot(n, view.world_position - i.world) < 0.0 {
            n = -n;
        }
        let ndl = dot(n, g3.xyz);
        let view_z = (view.view_from_world * vec4<f32>(i.world, 1.0)).z;
        var sh = 1.0;
        if ndl > 0.0 {
            sh = fetch_directional_shadow(0u, vec4<f32>(i.world, 1.0), n, view_z, i.clip.xy);
        }
        let facing = smoothstep(0.0, 0.25, ndl);
        let k = g3.w;
        let shade = 1.0 - k * 0.5 * facing * (1.0 - sh);
        let warm = 1.0 + k * 0.18 * facing * sh;
        rgb = rgb * shade * warm;
    }
    if g1.w > 0.5 {
        let z = -(view.view_from_world * vec4<f32>(i.world, 1.0)).z;
        let f = clamp((g0.w - z) / max(g0.w - g0.z, 0.001), 0.0, 1.0);
        // Enhanced: the haze glows toward the sun (in-scattering lobe).
        var fog_col = g2.rgb;
        let g4 = global(slot, 4);
        if g4.w > 0.0 {
            let vdir = normalize(i.world - view.world_position);
            let sdot = max(dot(vdir, global(slot, 3).xyz), 0.0);
            fog_col = mix(fog_col, g4.rgb, clamp(pow(sdot, 6.0) * g4.w, 0.0, 1.0));
        }
        rgb = mix(fog_col, rgb, f);
    }
    var out = to_linear(clamp(rgb, vec3(0.0), vec3(1.0)));
    // Enhanced: the clustered point lights (street lamps, headlights, fires) light the map.
    if global(slot, 2).w > 0.5 {
        var n = normalize(cross(dpdx(i.world), dpdy(i.world)));
        if dot(n, view.world_position - i.world) < 0.0 {
            n = -n;
        }
        let view_z = (view.view_from_world * vec4<f32>(i.world, 1.0)).z;
        let cluster = clustering::view_fragment_cluster_index(i.clip.xy, view_z, false);
        let ranges = clustering::unpack_clusterable_object_index_ranges(cluster);
        var add = vec3(0.0);
        for (var k: u32 = ranges.first_point_light_index_offset; k < ranges.first_spot_light_index_offset; k = k + 1u) {
            let id = clustering::get_clusterable_object_id(k);
            let l = &view_bindings::clustered_lights.data[id];
            let d = (*l).position_radius.xyz - i.world;
            let d2 = dot(d, d);
            let att = getDistanceAttenuation(d2, (*l).color_inverse_square_range.w);
            let ndl = max(dot(n, d * inverseSqrt(max(d2, 1e-6))), 0.0);
            add += (*l).color_inverse_square_range.rgb * att * ndl;
        }
        var fog_k = 1.0;
        if g1.w > 0.5 {
            let z = -view_z;
            fog_k = clamp((g0.w - z) / max(g0.w - g0.z, 0.001), 0.0, 1.0);
        }
        out += t.rgb * to_linear(mat.color.rgb) * add * view.exposure * fog_k;
    }
    return vec4(out, a);
}
