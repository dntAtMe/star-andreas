//! `CVehicleRecording` playback (carrec.md §3): 16 slots; each frame, after the collision and
//! shift passes, a playing car gets the pose, speed and pedals of its recording interpolated at
//! the running time.

use std::sync::Arc;

use glam::Vec3;
use sa_formats::carrec::Record;

use crate::{
    automobile::Automobile,
    physical::{Matrix, pf},
    world::{EntityId, World},
};

const SLOTS: usize = 16;

/// One playback slot.
#[derive(Clone)]
pub struct Playback {
    pub veh: EntityId,
    pub data: Arc<Vec<Record>>,
    /// Byte offset / 32 of the current record.
    pub index: usize,
    pub running_time: f32,
    pub speed: f32,
    pub looped: bool,
    pub paused: bool,
}

/// The playback slots and the last four frame times (`CTimer` time history).
#[derive(Default)]
pub struct Recordings {
    pub slots: Vec<Option<Playback>>,
    time_history: [u32; 4],
}

fn rec_matrix(r: &Record) -> Matrix {
    let right = Vec3::new(r.right[0] as f32, r.right[1] as f32, r.right[2] as f32) / 127.0;
    let fwd = Vec3::new(r.fwd[0] as f32, r.fwd[1] as f32, r.fwd[2] as f32) / 127.0;
    Matrix { right, fwd, up: right.cross(fwd), pos: Vec3::from(r.pos) }
}

fn rec_vel(r: &Record) -> Vec3 {
    Vec3::new(r.vel[0] as f32, r.vel[1] as f32, r.vel[2] as f32) / 16383.5
}

impl World {
    /// `StartPlaybackRecordedCar(veh, number, useCarAI = false, looped)` (non-AI mode).
    pub fn start_playback(&mut self, veh: EntityId, data: Arc<Vec<Record>>, looped: bool) {
        let rec = &mut self.recordings;
        if rec.slots.len() < SLOTS {
            rec.slots.resize(SLOTS, None);
        }
        let Some(s) = rec.slots.iter().position(|s| s.is_none()) else { return };
        rec.slots[s] = Some(Playback { veh, data, index: 0, running_time: 0.0, speed: 1.0, looped, paused: false });
        if let Some(b) = self.body_mut(veh) {
            // bDisableCollisionForce on, collide-as-static off.
            b.phys.flags = (b.phys.flags | pf::DISABLE_COLLISION_FORCE) & !pf::COLLIDE_AS_STATIC;
        }
        if let Some(c) = self.body_mut(veh).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) {
            c.rec_slot = Some(s as u8);
        }
    }

    /// `StopPlaybackRecordedCar`: flag 0x4 cleared, the car keeps its last pose and speed.
    pub fn stop_playback(&mut self, veh: EntityId) {
        for s in self.recordings.slots.iter_mut() {
            if s.as_ref().is_some_and(|p| p.veh == veh) {
                *s = None;
            }
        }
        if let Some(b) = self.body_mut(veh) {
            b.phys.flags &= !pf::DISABLE_COLLISION_FORCE;
        }
        if let Some(c) = self.body_mut(veh).and_then(|b| b.logic.as_any_mut().downcast_mut::<Automobile>()) {
            c.rec_slot = None;
        }
    }

    /// `IsPlaybackGoingOnForCar`.
    pub fn is_playback_going_on(&self, veh: EntityId) -> bool {
        self.recordings.slots.iter().flatten().any(|p| p.veh == veh)
    }

    /// `CVehicleRecording::Update` (0x45A610).
    pub(crate) fn update_recordings(&mut self) {
        let now = self.now_ms;
        let oldest = self.recordings.time_history[0];
        // CTimer::Update shifts the history: the last four frame times.
        let h = &mut self.recordings.time_history;
        h.rotate_left(1);
        h[3] = now;
        if self.recordings.slots.iter().all(|s| s.is_none()) {
            return;
        }
        for s in 0..self.recordings.slots.len() {
            let Some(mut p) = self.recordings.slots[s].clone() else { continue };
            if self.body(p.veh).is_none() {
                self.recordings.slots[s] = None;
                continue;
            }
            if !p.paused {
                p.running_time += now.wrapping_sub(oldest) as f32 * p.speed * 0.25;
            }
            let t = p.running_time;
            let d = &p.data;
            let n = d.len();
            let mut cur = p.index.min(n.saturating_sub(1));
            while cur + 1 < n && (d[cur + 1].time as f32) < t {
                cur += 1;
            }
            while cur > 0 && (d[cur].time as f32) > t {
                cur -= 1;
            }
            p.index = cur;
            if cur + 1 >= n {
                if p.looped {
                    p.running_time = 0.0;
                    p.index = 0;
                    self.recordings.slots[s] = Some(p);
                } else {
                    self.stop_playback(p.veh);
                }
                continue;
            }
            let (a, b) = (&d[cur], &d[cur + 1]);
            let f = (t - a.time as f32) / b.time.wrapping_sub(a.time) as f32;
            let (ma, mb) = (rec_matrix(a), rec_matrix(b));
            let m = Matrix {
                right: ma.right * (1.0 - f) + mb.right * f,
                fwd: ma.fwd * (1.0 - f) + mb.fwd * f,
                up: ma.up * (1.0 - f) + mb.up * f,
                pos: ma.pos * (1.0 - f) + mb.pos * f,
            };
            let vel = rec_vel(a) * (1.0 - f) + rec_vel(b) * f;
            if let Some(body) = self.body_mut(p.veh) {
                body.phys.matrix = m;
                body.phys.move_speed = vel;
                body.phys.turn_speed = Vec3::ZERO;
                if let Some(c) = body.logic.as_any_mut().downcast_mut::<Automobile>() {
                    c.steer_angle = a.steer as f32 * 0.05;
                    c.gas = a.gas as f32 * 0.01;
                    c.brake = a.brake as f32 * 0.01;
                    c.handbrake = a.handbrake;
                }
            }
            self.recordings.slots[s] = Some(p);
        }
    }
}
