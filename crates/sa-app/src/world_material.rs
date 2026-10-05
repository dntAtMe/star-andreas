//! Material for map geometry: SA's day/night prelit blend, timecyc ambient and fog
//! (see world_material.wgsl). The material is **bindless** (one bind group for all the map's
//! materials, so Bevy batches the draws); the per-frame globals live in one small shared
//! texture (every material points at the same one).

use bevy::{
    asset::embedded_asset,
    mesh::{MeshVertexAttribute, MeshVertexBufferLayoutRef},
    pbr::{MaterialPipeline, MaterialPipelineKey},
    prelude::*,
    asset::RenderAssetUsages,
    render::render_resource::{
        AsBindGroup, AsBindGroupShaderType, Extent3d, RenderPipelineDescriptor, ShaderType, SpecializedMeshPipelineError, TextureDimension, TextureFormat,
        VertexFormat,
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

#[derive(Clone, Copy, Default, ShaderType, Debug)]
pub struct WorldMatUniform {
    /// Material colour, gamma 0..1.
    pub color: Vec4,
    /// x: lit, y: alpha cutoff (< 0 = none).
    pub params: Vec4,
}

#[derive(Asset, TypePath, AsBindGroup, Debug, Clone)]
#[data(0, WorldMatUniform, binding_array(10))]
#[bindless(index_table(range(0..4)))]
pub struct WorldMaterial {
    pub uniform: WorldMatUniform,
    #[texture(1)]
    #[sampler(2)]
    pub texture: Option<Handle<Image>>,
    /// The shared per-frame globals (GLOBALS_W × 1 RGBA16F).
    #[texture(3)]
    pub globals: Handle<Image>,
    pub alpha_mode: AlphaMode,
}

impl AsBindGroupShaderType<WorldMatUniform> for WorldMaterial {
    fn as_bind_group_shader_type(&self, _images: &bevy::render::render_asset::RenderAssets<bevy::render::texture::GpuImage>) -> WorldMatUniform {
        self.uniform
    }
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

/// The shared per-frame globals texture.
#[derive(Resource, Clone)]
pub struct WorldGlobals(pub Handle<Image>);

/// Texels in the globals texture.
pub const GLOBALS_W: u32 = 8;

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
    /// Enhanced graphics: the fog's sun in-scattering colour (gamma) and strength.
    pub haze: Vec4,
}

impl GlobalsData {
    pub fn pack(&self) -> Vec<[f32; 4]> {
        let (fs, fe, fc) = self.fog.unwrap_or((0.0, 1.0, Vec3::ZERO));
        vec![
            [self.dn, 0.0, fs, fe],
            [self.ambient.x, self.ambient.y, self.ambient.z, if self.fog.is_some() { 1.0 } else { 0.0 }],
            [fc.x, fc.y, fc.z, if self.point_lights { 1.0 } else { 0.0 }],
            [self.sun.x, self.sun.y, self.sun.z, self.shadow],
            self.haze.to_array(),
        ]
    }
}

impl GlobalsData {
    /// The texture bytes (RGBA16F texels, zero-padded to GLOBALS_W).
    pub fn texels(&self) -> Vec<u8> {
        let mut v = self.pack();
        v.resize(GLOBALS_W as usize, [0.0; 4]);
        v.iter().flatten().flat_map(|&x| f32_to_f16(x).to_le_bytes()).collect()
    }
}

/// IEEE half from f32 (round to nearest, no NaN payloads needed here).
fn f32_to_f16(x: f32) -> u16 {
    let b = x.to_bits();
    let sign = ((b >> 16) & 0x8000) as u16;
    let exp = ((b >> 23) & 0xFF) as i32 - 127 + 15;
    let mant = b & 0x7F_FFFF;
    if exp >= 31 {
        return sign | 0x7C00;
    }
    if exp <= 0 {
        if exp < -10 {
            return sign;
        }
        let m = (mant | 0x80_0000) >> (1 - exp);
        return sign | ((m + 0x1000) >> 13) as u16;
    }
    let h = sign as u32 | ((exp as u32) << 10) | (mant >> 13);
    (h + ((mant >> 12) & 1)) as u16
}

fn init(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    let data = GlobalsData { dn: 0.0, ambient: Vec3::ZERO, fog: None, sun: Vec3::Y, shadow: 0.0, point_lights: false, haze: Vec4::ZERO };
    let img = Image::new(
        Extent3d { width: GLOBALS_W, height: 1, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data.texels(),
        TextureFormat::Rgba16Float,
        RenderAssetUsages::RENDER_WORLD | RenderAssetUsages::MAIN_WORLD,
    );
    commands.insert_resource(WorldGlobals(images.add(img)));
}
