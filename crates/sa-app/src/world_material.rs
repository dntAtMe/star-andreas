//! Material for map geometry: SA's day/night prelit blend, timecyc ambient and fog
//! (see world_material.wgsl). One shared storage buffer carries the per-frame globals.

use bevy::{
    asset::embedded_asset,
    mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef},
    pbr::{MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    render::{
        render_resource::{AsBindGroup, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError, VertexFormat},
        storage::ShaderBuffer,
    },
    shader::ShaderRef,
};

/// Night prelit colours (the Extra Vert Colour plugin), gamma 0..1.
pub const ATTRIBUTE_NIGHT_COLOR: MeshVertexAttribute =
    MeshVertexAttribute::new("SaNightColor", 718_254_031, VertexFormat::Float32x4);

pub struct WorldMaterialPlugin;

impl Plugin for WorldMaterialPlugin {
    fn build(&self, app: &mut App) {
        embedded_asset!(app, "world_material.wgsl");
        embedded_asset!(app, "world_material_prepass.wgsl");
        app.add_plugins(MaterialPlugin::<WorldMaterial>::default()).add_systems(PreStartup, init);
    }
}

#[derive(Clone, Copy, ShaderType, Debug)]
pub struct WorldMatUniform {
    /// Material colour, gamma 0..1.
    pub color: Vec4,
    /// x: lit, y: alpha cutoff (< 0 = none).
    pub params: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
pub struct WorldMaterial {
    #[uniform(0)]
    pub uniform: WorldMatUniform,
    #[texture(1)]
    #[sampler(2)]
    pub texture: Option<Handle<Image>>,
    #[storage(3, read_only)]
    pub globals: Handle<ShaderBuffer>,
    pub alpha_mode: AlphaMode,
}

impl Material for WorldMaterial {
    fn vertex_shader() -> ShaderRef {
        "embedded://sa_app/world_material.wgsl".into()
    }

    fn fragment_shader() -> ShaderRef {
        "embedded://sa_app/world_material.wgsl".into()
    }

    fn prepass_fragment_shader() -> ShaderRef {
        "embedded://sa_app/world_material_prepass.wgsl".into()
    }

    fn alpha_mode(&self) -> AlphaMode {
        self.alpha_mode
    }

    fn specialize(
        _pipeline: &MaterialPipeline,
        descriptor: &mut RenderPipelineDescriptor,
        layout: &MeshVertexBufferLayoutRef,
        _key: MaterialPipelineKey<Self>,
    ) -> Result<(), SpecializedMeshPipelineError> {
        descriptor.primitive.cull_mode = None;
        // Prepass / shadow pipelines keep Bevy's standard attribute locations.
        let prepass = descriptor.vertex.shader_defs.iter().any(|d| matches!(d, bevy::shader::ShaderDefVal::Bool(n, true) if n == "PREPASS_PIPELINE"));
        if prepass {
            return Ok(());
        }
        let vertex_layout = layout.0.get_layout(&[
            Mesh::ATTRIBUTE_POSITION.at_shader_location(0),
            Mesh::ATTRIBUTE_UV_0.at_shader_location(1),
            Mesh::ATTRIBUTE_COLOR.at_shader_location(2),
            ATTRIBUTE_NIGHT_COLOR.at_shader_location(3),
        ])?;
        descriptor.vertex.buffers = vec![vertex_layout];
        descriptor.primitive.cull_mode = None;
        Ok(())
    }
}

/// The shared per-frame globals buffer.
#[derive(Resource, Clone)]
pub struct WorldGlobals(pub Handle<ShaderBuffer>);

/// Per-frame values (gamma space).
#[derive(Debug, Clone, Copy)]
pub struct GlobalsData {
    pub dn: f32,
    pub ambient: Vec3,
    pub fog: Option<(f32, f32, Vec3)>,
    /// Enhanced graphics: toward the sun (Bevy world space) and the shadow strength (0 = off).
    pub sun: Vec3,
    pub shadow: f32,
    /// Enhanced graphics: point lights light the map.
    pub point_lights: bool,
}

impl GlobalsData {
    pub fn pack(&self) -> Vec<[f32; 4]> {
        let (fs, fe, fc) = self.fog.unwrap_or((0.0, 1.0, Vec3::ZERO));
        vec![
            [self.dn, 0.0, fs, fe],
            [self.ambient.x, self.ambient.y, self.ambient.z, if self.fog.is_some() { 1.0 } else { 0.0 }],
            [fc.x, fc.y, fc.z, if self.point_lights { 1.0 } else { 0.0 }],
            [self.sun.x, self.sun.y, self.sun.z, self.shadow],
        ]
    }
}

fn init(mut commands: Commands, mut buffers: ResMut<Assets<ShaderBuffer>>) {
    let data = GlobalsData { dn: 0.0, ambient: Vec3::ZERO, fog: None, sun: Vec3::Y, shadow: 0.0, point_lights: false };
    commands.insert_resource(WorldGlobals(buffers.add(ShaderBuffer::from(data.pack()))));
}
