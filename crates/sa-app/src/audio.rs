//! Audio playback (audio.md): SA's SFX bank sounds as PCM sources and the decrypted Ogg
//! stream tracks (cutscenes), played through Bevy's audio.
//!
//! `SA_PLAYTRACK=<id>` plays a stream track at start, `SA_PLAYSOUND=<bank>,<sound>` an SFX
//! sound (debug).

use std::{num::NonZero, sync::Arc};

use bevy::{
    audio::{AddAudioSource, Decodable, PlaybackMode, Volume},
    prelude::*,
};
use sa_formats::audio::SaAudio;

use crate::player::GameRoot;

pub struct AudioPlugin;

impl Plugin for AudioPlugin {
    fn build(&self, app: &mut App) {
        app.add_audio_source::<PcmSound>().add_systems(Startup, setup).add_systems(Update, debug_play);
    }
}

/// The audio CONFIG tables.
#[derive(Resource, Clone)]
pub struct Audio(pub Arc<SaAudio>);

/// One SFX bank sound: mono samples at its rate.
#[derive(Asset, TypePath, Clone)]
pub struct PcmSound {
    samples: Arc<[f32]>,
    rate: u32,
}

impl Decodable for PcmSound {
    type Decoder = rodio::buffer::SamplesBuffer;

    fn decoder(&self) -> Self::Decoder {
        rodio::buffer::SamplesBuffer::new(NonZero::new(1).unwrap(), NonZero::new(self.rate.max(1)).unwrap(), self.samples.to_vec())
    }
}

fn setup(mut commands: Commands, root: Res<GameRoot>) {
    match SaAudio::open(&root.0) {
        Ok(a) => commands.insert_resource(Audio(Arc::new(a))),
        Err(e) => warn!("audio config: {e}"),
    }
}

/// dB → Bevy linear volume.
pub fn db(v: f32) -> Volume {
    Volume::Linear(10f32.powf(v / 20.0))
}

impl Audio {
    /// An SFX bank sound as a PCM asset (headroom applied by the caller).
    pub fn sound(&self, assets: &mut Assets<PcmSound>, bank: u16, idx: u16) -> Option<(Handle<PcmSound>, f32)> {
        let s = self.0.sound(bank, idx)?;
        let samples: Arc<[f32]> = s.samples.iter().map(|&v| v as f32 / 32768.0).collect();
        Some((assets.add(PcmSound { samples, rate: s.rate }), s.headroom_db))
    }

    /// A stream track (Ogg Vorbis) as an audio asset.
    pub fn track(&self, assets: &mut Assets<AudioSource>, id: u16) -> Option<Handle<AudioSource>> {
        let ogg = self.0.track_ogg(id)?;
        Some(assets.add(AudioSource { bytes: ogg.into() }))
    }
}

/// Play a 2D one-shot SFX sound at `volume_db` (minus its headroom).
pub fn play_sound(commands: &mut Commands, audio: &Audio, assets: &mut Assets<PcmSound>, bank: u16, idx: u16, volume_db: f32) -> Option<Entity> {
    let (h, headroom) = audio.sound(assets, bank, idx)?;
    Some(commands.spawn((AudioPlayer::<PcmSound>(h), PlaybackSettings { mode: PlaybackMode::Despawn, volume: db(volume_db - headroom), ..default() })).id())
}

/// Play a stream track (cutscene audio) at `volume_db`.
pub fn play_track(commands: &mut Commands, audio: &Audio, assets: &mut Assets<AudioSource>, id: u16, volume_db: f32) -> Option<Entity> {
    let h = audio.track(assets, id)?;
    Some(commands.spawn((AudioPlayer::<AudioSource>(h), PlaybackSettings { mode: PlaybackMode::Despawn, volume: db(volume_db), ..default() })).id())
}

fn debug_play(
    mut commands: Commands,
    audio: Option<Res<Audio>>,
    mut done: Local<bool>,
    mut pcm: ResMut<Assets<PcmSound>>,
    mut ogg: ResMut<Assets<AudioSource>>,
) {
    let Some(audio) = audio else { return };
    if *done {
        return;
    }
    *done = true;
    if let Some(id) = std::env::var("SA_PLAYTRACK").ok().and_then(|v| v.parse().ok()) {
        let ok = play_track(&mut commands, &audio, &mut ogg, id, -3.0).is_some();
        info!("audio: track {id} {}", if ok { "playing" } else { "missing" });
    }
    if let Some((b, s)) = std::env::var("SA_PLAYSOUND").ok().and_then(|v| {
        let (a, b) = v.split_once(',')?;
        Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
    }) {
        let ok = play_sound(&mut commands, &audio, &mut pcm, b, s, 0.0).is_some();
        info!("audio: sound {b},{s} {}", if ok { "playing" } else { "missing" });
    }
}
