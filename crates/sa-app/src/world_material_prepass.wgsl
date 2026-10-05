// The map material's prepass (depth / normals / motion vectors, and the sun's shadow maps):
// Bevy's default prepass with the alpha-mask discard of world_material.wgsl, so foliage and
// fences don't write solid quads into the depth prepass or the shadow maps.
#import bevy_pbr::prepass_io::{VertexOutput, FragmentOutput}
#import bevy_pbr::mesh_view_bindings::view
#import bevy_pbr::prepass_bindings

struct WorldMat {
    color: vec4<f32>,
    params: vec4<f32>,
}

#ifdef BINDLESS
#import bevy_render::bindless::{bindless_samplers_filtering, bindless_textures_2d}
#import bevy_pbr::mesh_bindings::mesh
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
#endif

// The alpha-mask test of world_material.wgsl.
fn masked_out(in: VertexOutput) -> bool {
#ifdef VERTEX_UVS_A
#ifdef BINDLESS
#ifdef VERTEX_OUTPUT_INSTANCE_INDEX
    let slot = mesh[in.instance_index].material_and_lightmap_bind_group_slot & 0xffffu;
#else
    let slot = 0u;
#endif
    let m = material_array[material_indices[slot].material];
    if m.params.y >= 0.0 {
        let a = textureSample(bindless_textures_2d[material_indices[slot].tex], bindless_samplers_filtering[material_indices[slot].tex_sampler], in.uv).a * m.color.a;
        return a < m.params.y;
    }
#else
    if mat_u.params.y >= 0.0 {
        let a = textureSample(tex, tex_sampler, in.uv).a * mat_u.color.a;
        return a < mat_u.params.y;
    }
#endif
#endif
    return false;
}

#ifdef PREPASS_FRAGMENT
@fragment
fn fragment(in: VertexOutput) -> FragmentOutput {
    if masked_out(in) {
        discard;
    }
    var out: FragmentOutput;
#ifdef NORMAL_PREPASS
    var n = in.world_normal;
    if dot(n, n) < 1e-6 {
        n = cross(dpdx(in.world_position.xyz), dpdy(in.world_position.xyz));
    }
    n = normalize(n);
    if dot(n, view.world_position - in.world_position.xyz) < 0.0 {
        n = -n;
    }
    out.normal = vec4(n * 0.5 + vec3(0.5), 1.0);
#endif
#ifdef UNCLIPPED_DEPTH_ORTHO_EMULATION
    out.frag_depth = in.unclipped_depth;
#endif
#ifdef MOTION_VECTOR_PREPASS
    let clip_position_t = view.unjittered_clip_from_world * in.world_position;
    let clip_position = clip_position_t.xy / clip_position_t.w;
    let previous_clip_position_t = prepass_bindings::previous_view_uniforms.clip_from_world * in.previous_world_position;
    let previous_clip_position = previous_clip_position_t.xy / previous_clip_position_t.w;
    out.motion_vector = (clip_position - previous_clip_position) * vec2(0.5, -0.5);
#endif
    return out;
}
#else
// Depth only (shadow maps of masked materials): just the discard.
@fragment
fn fragment(in: VertexOutput) {
    if masked_out(in) {
        discard;
    }
}
#endif
