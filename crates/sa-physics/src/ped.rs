//! `CPed` physics: the shared ped collision model, movement from animation
//! root motion (CalculateNewOrientation/Velocity, UpdatePosition), jumping,
//! and the ped's own entity-collision (ground line) state.
//!
//! The ped's origin is 1.0 above its feet. Ground contact comes only from a
//! vertical line probe (see `World::ped_entity_collision`), never from a
//! contact point.
//!
//! Not ported: aiming col model (Ped2 sensors), riding moving ground entities
//! (their velocity is not added), buoyancy, fall damage, steep-surface slide,
//! NPC turn ramping details beyond the documented formula.

use std::f32::consts::{PI, TAU};

use glam::{Quat, Vec2, Vec3};

use crate::{
    Ctx,
    anim::Clump,
    collision::{ColLine, ColModel, ColSphere, Surf},
    effects::{FrameFx, WorldRequest},
    pedtask::{PedCore, PedTasks},
    physical::{EntityType, Matrix, Physical, pf},
    world::{BodyLogic, EntityId, LineHits},
};

pub const PED_MASS: f32 = 70.0;
pub const PED_TURN_MASS: f32 = 100.0;
pub const PED_AIR_RESISTANCE: f32 = 0.4 / 70.0;
pub const PED_ELASTICITY: f32 = 0.05;
/// Surface type of peds (material 62).
pub const SURFACE_PED: u8 = 62;
pub const NO_CEILING: f32 = 99999.99;

/// `ms_colModelPed1` (0x968DF0): three 0.35 spheres; the lines are added per call.
pub fn ped_col_model() -> ColModel {
    let s = |z: f32, piece: u8| ColSphere {
        center: Vec3::new(0.0, 0.0, z),
        radius: 0.35,
        surf: Surf { material: SURFACE_PED, piece, lighting: 0 },
    };
    ColModel {
        bbox_min: Vec3::new(-0.35, -0.35, -1.0),
        bbox_max: Vec3::new(0.35, 0.35, 0.95),
        bound_center: Vec3::ZERO,
        bound_radius: 1.0,
        spheres: vec![s(-0.2, 0), s(0.200_000_05, 1), s(0.6, 2)],
        ..Default::default()
    }
}

/// `CPedModelInfo::ms_pHitColTable` (0x8A630C, stride 0x1C): bone tag, piece type, offset
/// along the bone's X axis, radius.
const HIT_COL: [(i32, u8, f32, f32); 12] = [
    (5, 9, 0.05, 0.15),
    (3, 3, 0.2, 0.2),
    (3, 3, 0.0, 0.2),
    (2, 4, -0.1, 0.2),
    (32, 5, 0.06, 0.14),
    (22, 6, 0.06, 0.14),
    (33, 5, 0.05, 0.14),
    (23, 6, 0.05, 0.14),
    (42, 7, -0.1, 0.18),
    (52, 8, -0.1, 0.18),
    (43, 7, -0.18, 0.16),
    (53, 8, -0.18, 0.16),
];

/// `CPedModelInfo::AnimatePedColModelSkinned` (0x4C6F70): the bullet hit col model posed
/// from the skinned clump (ped model space).
pub fn hit_col_model(clump: &crate::anim::Clump) -> Option<ColModel> {
    let mut spheres = Vec::with_capacity(HIT_COL.len());
    let (mut lo, mut hi) = (Vec3::splat(f32::MAX), Vec3::splat(f32::MIN));
    for (tag, piece, x, r) in HIT_COL {
        let f = clump.frame_of_tag(tag)?;
        let center = clump.ltm(f).transform_point3(Vec3::new(x, 0.0, 0.0));
        lo = lo.min(center - Vec3::splat(r));
        hi = hi.max(center + Vec3::splat(r));
        spheres.push(ColSphere { center, radius: r, surf: Surf { material: SURFACE_PED, piece, lighting: 0 } });
    }
    let c = (lo + hi) * 0.5;
    Some(ColModel { bbox_min: lo, bbox_max: hi, bound_center: c, bound_radius: (hi - c).length(), spheres, ..Default::default() })
}

/// A Physical configured like the CPed constructor (0x5E8030).
pub fn ped_physical(matrix: Matrix) -> Physical {
    let mut p = Physical::new(EntityType::Ped, matrix);
    p.flags |= pf::DISABLE_TURN_FORCE | pf::KEEP_COLLISION_RECORDS;
    p.mass = PED_MASS;
    p.turn_mass = PED_TURN_MASS;
    p.air_resistance = PED_AIR_RESISTANCE;
    p.elasticity = PED_ELASTICITY;
    p
}

/// `CGeneral::LimitRadianAngle` (0x53CB50).
pub fn limit_radian_angle(mut a: f32) -> f32 {
    a = a.clamp(-25.0, 25.0);
    while a > PI {
        a -= TAU;
    }
    while a < -PI {
        a += TAU;
    }
    a
}

/// Ped state the physics reads/writes (the CPed fields of the notes).
pub struct PedLogic {
    pub is_player: bool,
    /// m_fCurrentRotation / m_fAimedRotation (heading h faces (-sin h, cos h)).
    pub cur_rot: f32,
    pub aim_rot: f32,
    /// Heading change rate, degrees per frame (pedstats; default 15).
    pub turn_rate: f32,
    turn_factor: f32,
    /// Root-motion velocity of the playing animations, ped-local
    /// (x right, y forward), units per 1/50 s frame.
    pub anim_velocity: Vec2,
    pub standing: bool,
    pub was_standing: bool,
    pub ground_normal: Vec3,
    pub ground_surface: u8,
    /// `m_fContactSurfaceBrightness` (+0x12C, default 1.0): the ground's collision lighting.
    pub lighting: f32,
    /// Ground entity (vehicle/object) the ped stands on, if any.
    pub ground_entity: Option<EntityId>,
    /// The ground entity is a vehicle.
    pub ground_is_car: bool,
    pub ceiling_z: f32,
    /// Player ceiling probe enable (ped+0x478 & 0x100).
    pub ceiling_probe: bool,
    /// Set by the app to request a jump; consumed by ProcessControl.
    pub jump_request: Option<JumpKind>,
    /// Frames since knocked down by a vehicle (0 = not knocked down).
    pub knocked_down: f32,
    /// Ground z found by the world's vertical line test (pos.z - 4.0), if any.
    pub ground_below: Option<f32>,
    /// The ped's anim blend clump (skinned skeleton); drives root motion when present.
    pub clump: Option<Box<Clump>>,
    /// Pose before the last anim update, for render interpolation.
    pub prev_pose: Vec<(Quat, Vec3)>,
    /// Player tasks (on foot, weapons); used when the clump and anims are set.
    pub tasks: PedTasks,
    /// Damage generated by the world this frame (falls, explosions, cars, bullets).
    pub pending_damage: Vec<crate::peddamage::DamageIn>,
    /// Souls combat mode (the player only; None = GTA's own controls).
    pub souls: Option<Box<crate::souls::Souls>>,
    /// Souls mode's hit reactions and poise on a ped the player fights.
    pub souls_react: Option<Box<crate::souls::Reaction>>,
    /// Souls mode's ER melee brain on a ped fighting the player.
    pub souls_enemy: Option<Box<crate::souls::Enemy>>,
    /// Random NPC state (wander task, population bookkeeping); None for the player.
    pub npc: Option<crate::npc::NpcState>,
    /// ped+0x58C with ped+0x46C & 0x100: seated in a vehicle (CTaskSimpleCarDrive).
    pub vehicle: Option<crate::incar::InVehicle>,
    /// Knocked down by the player's car this frame (the run-over crime).
    pub run_over_by_player: bool,
    /// CTaskComplexEnterCarAsDriver (the player getting into a car).
    pub enter: Option<crate::entercar::EnterCar>,
    /// CTaskComplexLeaveCar (the player getting out).
    pub leave: Option<crate::entercar::LeaveCar>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum JumpKind {
    /// Horizontal launch speed: 0.1 walk, 0.17 run, 0.22 sprint (units/frame).
    Speed(f32),
}

impl PedLogic {
    pub fn new(is_player: bool, heading: f32) -> Self {
        Self {
            is_player,
            cur_rot: heading,
            aim_rot: heading,
            // pedstats+0x20: STAT_PLAYER 9.0 (SetModelIndex overwrites the ctor default 15).
            turn_rate: if is_player { 9.0 } else { 15.0 },
            turn_factor: 0.1,
            anim_velocity: Vec2::ZERO,
            standing: false,
            was_standing: false,
            ground_normal: Vec3::Z,
            ground_surface: 0,
            lighting: 1.0,
            ground_entity: None,
            ground_is_car: false,
            ceiling_z: NO_CEILING,
            ceiling_probe: false,
            jump_request: None,
            knocked_down: 0.0,
            ground_below: None,
            clump: None,
            prev_pose: Vec::new(),
            tasks: {
                let mut t = PedTasks::default();
                t.is_player = is_player;
                t
            },
            pending_damage: Vec::new(),
            souls: None,
            souls_react: None,
            souls_enemy: None,
            npc: None,
            vehicle: None,
            run_over_by_player: false,
            enter: None,
            leave: None,
        }
    }

    /// First half of 0x5E4C50: turn `cur_rot` towards `aim_rot`.
    fn calculate_new_orientation(&mut self, ts: f32) {
        let lim = self.turn_rate * 0.017_453_292 * ts;
        self.cur_rot = limit_radian_angle(self.cur_rot);
        let mut a = limit_radian_angle(self.aim_rot);
        if self.cur_rot + PI < a {
            a -= TAU;
        } else if self.cur_rot - PI > a {
            a += TAU;
        }
        let d = a - self.cur_rot;
        if self.is_player {
            self.turn_factor = 1.0;
        } else if d >= 0.0 && self.turn_factor < 0.0 {
            self.turn_factor = 0.1;
        } else if d < 0.0 && self.turn_factor > 0.0 {
            self.turn_factor = -0.1;
        }
        let step = self.turn_factor.abs() * lim;
        if d > step {
            self.cur_rot += step;
            self.turn_factor = (self.turn_factor + ts * 0.1).min(1.0);
        } else if d < -step {
            self.cur_rot -= step;
            self.turn_factor = (self.turn_factor - ts * 0.1).max(-1.0);
        } else if !self.is_player && d.abs() > 0.1 * lim {
            self.cur_rot += 0.5 * d;
            self.turn_factor *= 0.5;
        } else {
            self.cur_rot += d;
            self.turn_factor = (d.abs() / lim.max(1e-9)).max(0.1);
        }
    }

    /// Second half of 0x5E4C50: slope-corrected anim velocity in world xy.
    fn anim_world_velocity(&self, m: &Matrix) -> Vec2 {
        let nf = self.ground_normal.dot(m.fwd);
        let nr = self.ground_normal.dot(m.right);
        let sf = (1.0 - nf * nf).max(0.0).sqrt();
        let sr = (1.0 - nr * nr).max(0.0).sqrt();
        let f = Vec2::new(m.fwd.x, m.fwd.y);
        let r = Vec2::new(m.right.x, m.right.y);
        f * (sf * self.anim_velocity.y) + r * (sr * self.anim_velocity.x)
    }

    /// `CTaskSimpleJump::Launch` (0x679B80).
    fn launch_jump(&mut self, p: &mut Physical, hs: f32) {
        let up = if self.is_player { 8.5 } else { 4.5 };
        p.apply_move_force(Vec3::new(0.0, 0.0, up));
        let mv = Vec2::new(p.move_speed.x, p.move_speed.y);
        if mv.length_squared() < hs * hs || self.ground_entity.is_some() {
            p.move_speed.x = -self.cur_rot.sin() * hs;
            p.move_speed.y = self.cur_rot.cos() * hs;
        }
        self.standing = false;
    }
}

impl PedLogic {
    /// `CPlayerPed::HandlePlayerBreath(underWater, 1.0)` (0x60A8D0): breath runs out, then
    /// `ftol(3·ts)` drowning damage per frame (type 53).
    pub fn handle_breath(&mut self, under_water: bool, ts: f32) {
        let pd = &mut self.tasks.pd;
        if under_water {
            if pd.breath > 0.0 {
                pd.breath = (pd.breath - ts).max(0.0);
            } else {
                self.pending_damage.push(crate::peddamage::DamageIn {
                    src: None,
                    src_pos: None,
                    ty: 53,
                    damage: (ts * 3.0) as i32 as f32,
                    piece: 3,
                    dir: 0,
                    fight: None,
                    force_death: false,
                });
            }
        } else if pd.breath < BREATH_MAX {
            pd.breath += 2.0 * ts;
        }
    }
}

/// GetFatAndMuscleModifier(8): the player's lung capacity [I: stat-driven, default stats].
pub const BREATH_MAX: f32 = crate::swim::MAX_BREATH;

/// Rotate the matrix to heading `h` about Z, keeping position (`CMatrix::SetRotateZOnly`-style).
pub fn set_heading(m: &mut Matrix, h: f32) {
    let (s, c) = h.sin_cos();
    m.right = Vec3::new(c, s, 0.0);
    m.fwd = Vec3::new(-s, c, 0.0);
    m.up = Vec3::Z;
}

impl BodyLogic for PedLogic {
    fn hit_col_model(&self) -> Option<ColModel> {
        hit_col_model(self.clump.as_deref()?)
    }

    /// CPed::ProcessControl (0x5E8CD0), the physics-relevant steps.
    fn process_control(&mut self, p: &mut Physical, _col: &mut ColModel, ctx: &Ctx, _lines: &LineHits) {
        let ts = ctx.ts;
        self.was_standing = false;
        self.ceiling_z = NO_CEILING;
        if self.knocked_down > 0.0 {
            self.knocked_down += ts;
        }

        // CEntity::UpdateAnim (CWorld::Process step a): the anims advance; the root shift
        // becomes the anim velocity (CalculateNewVelocity: shift / ts).
        if let Some(clump) = &mut self.clump {
            self.prev_pose.clone_from(&clump.pose);
            clump.update(ts * 0.02);
            let v = clump.velocity;
            let k = if ts < 0.01 { 0.01 } else { 1.0 / ts };
            self.anim_velocity = Vec2::new(v.x, v.y) * k;
        }

        // CPed::ProcessControl: gun flash decay.
        for (alpha, rate) in &mut self.tasks.gun_flash {
            if *alpha > 0 {
                let step = (ts * 0.02 * 1000.0) as i32;
                if (*alpha as i32) > *rate as i32 * step {
                    *alpha -= (step * *rate as i32) as i16;
                } else {
                    *alpha = 0;
                }
            }
        }

        // Souls mode: i-frames and the guard soak hits up before GTA sees them.
        let souls_on = self.souls.is_some() && self.tasks.health.alive() && self.vehicle.is_none() && self.enter.is_none() && self.leave.is_none();
        if souls_on {
            let pos = p.matrix.pos;
            let s = self.souls.as_deref_mut().unwrap();
            self.pending_damage.retain(|d| {
                // Souls falls hurt by its own rule; GTA's fall damage is dropped.
                if d.ty == 54 {
                    return d.piece == crate::souls::player::OWN_FALL_PIECE;
                }
                s.receive_hit(pos, d.src_pos.unwrap_or(pos), d.damage)
            });
        }
        // Damage events of the last frame (health changes at once, reactions are blended).
        if let (Some(clump), Some(m)) = (self.clump.as_deref_mut(), self.tasks.anims.clone()) {
            for d in std::mem::take(&mut self.pending_damage) {
                if let Some(n) = self.npc.as_mut() {
                    n.damaged_by = Some(d.src);
                }
                if let Some(n) = self.npc.as_ref() {
                    self.tasks.move_state = n.move_state;
                }
                if let Some((src, force)) = self.tasks.take_damage(d, clump, &m, ctx.now_ms) {
                    let dd = (src - p.matrix.pos).truncate().normalize_or_zero();
                    self.standing = false;
                    p.apply_move_force(Vec3::new(dd.x * force * -5.0, dd.y * force * -5.0, 5.0));
                }
            }
        } else {
            self.pending_damage.clear();
        }
        let busy = match (self.clump.as_deref_mut(), self.tasks.anims.clone()) {
            (Some(clump), Some(m)) => self.tasks.process_health(clump, &m, self.standing, ctx.now_ms),
            _ => false,
        };
        if busy {
            self.tasks.pad.clear_just_down();
            self.tasks.cam_request = 0;
        }

        // CTaskComplexEnterCar 800 GoToCarDoorAndStandStill: run to the door point.
        if let (Some(e), Some(clump), Some(m)) = (self.enter.as_mut(), self.clump.as_deref_mut(), self.tasks.anims.clone()) {
            if e.stage == crate::entercar::Stage::GoTo && !busy {
                let d = (e.target - p.matrix.pos).truncate();
                if self.tasks.pad.enter_exit_just_down && ctx.now_ms > e.started_ms() + 100 || ctx.now_ms > e.started_ms() + e.timeout_ms {
                    e.cancel = true;
                } else if d.length_squared() < 0.5 * 0.5 {
                    e.reached = true;
                } else {
                    self.aim_rot = limit_radian_angle(crate::pedtask::radian_angle_between_points(e.target.x, e.target.y, p.matrix.pos.x, p.matrix.pos.y));
                    if clump.get(crate::anim::anim_id::RUN).is_none_or(|a| a.blend < 1.0 && a.blend_delta <= 0.0) {
                        clump.blend_animation(&m, self.tasks.anim_group, crate::anim::anim_id::RUN, 4.0);
                    }
                    self.tasks.move_state = 6;
                }
                self.tasks.pad.clear_just_down();
            }
        }
        // Souls mode replaces the player's tasks: its state machine moves and turns the ped
        // and poses the clump; GTA's physics keeps collision, ground and gravity.
        if souls_on {
            if let (Some(s), Some(clump)) = (self.souls.as_deref_mut(), self.clump.as_deref_mut()) {
                let dt = ts * 0.02;
                // Q / E still switch between the fists and the melee weapon.
                let pad = &self.tasks.pad;
                if pad.next_weapon_just_down || pad.prev_weapon_just_down {
                    let slot = if self.tasks.pd.chosen_slot == 0 && self.tasks.weapons.get(1).is_some_and(|w| w.ty != 0) { 1 } else { 0 };
                    self.tasks.pd.chosen_slot = slot;
                }
                let chosen = self.tasks.pd.chosen_slot;
                if chosen <= 1 {
                    self.tasks.active_slot = chosen;
                    s.set_gta_weapon(self.tasks.weapons.get(chosen).map_or(0, |w| w.ty));
                }
                s.face_target(p.matrix.pos, dt);
                let out = s.step(p.matrix.pos, self.standing, dt);
                s.animate(clump, dt);
                self.tasks.weapon_model = s.gta_model;
                let h = s.heading;
                self.cur_rot = h;
                self.aim_rot = h;
                let (f, r) = (Vec2::new(-h.sin(), h.cos()), Vec2::new(h.cos(), h.sin()));
                self.anim_velocity = Vec2::new(out.vel.dot(r), out.vel.dot(f)) * 0.02;
                if !self.standing {
                    // In the air the carried velocity stands in for the anim velocity.
                    p.move_speed.x = out.vel.x * 0.02;
                    p.move_speed.y = out.vel.y * 0.02;
                }
                if let Some(vz) = out.vz {
                    p.move_speed.z = vz * 0.02;
                    if vz > 0.0 {
                        self.standing = false;
                    }
                }
                if out.fall_damage > 0.0 {
                    self.pending_damage.push(crate::peddamage::DamageIn {
                        src: None,
                        src_pos: None,
                        ty: 54,
                        damage: out.fall_damage,
                        piece: crate::souls::player::OWN_FALL_PIECE,
                        dir: 0,
                        fight: None,
                        force_death: out.fall_damage >= 1000.0,
                    });
                }
            }
            self.tasks.pad.clear_just_down();
        }
        // Step 11: CPedIntelligence::Process (the player's tasks).
        if self.is_player && !souls_on && self.tasks.anims.is_some() && !busy && self.enter.is_none() && !self.tasks.arrested {
            if let Some(clump) = self.clump.as_deref_mut() {
                let mut core = PedCore {
                    p,
                    clump,
                    cur_rot: &mut self.cur_rot,
                    aim_rot: &mut self.aim_rot,
                    turn_rate: &mut self.turn_rate,
                    standing: self.standing,
                    ground_below: self.ground_below,
                    ground_entity: self.ground_entity.is_some(),
                    ground_car: self.ground_entity.is_some() && self.ground_is_car,
                };
                self.tasks.process(&mut core, ctx);
                self.tasks.post_process(core.clump, ctx);
            }
            // HandlePlayerBreath from the swim task (rate scales the drain and the refill).
            if let Some((under, rate)) = self.tasks.breath_request.take() {
                self.handle_breath(under, ts * rate);
            }
            self.tasks.pad.clear_just_down();
        }

        if self.is_player {
            self.tasks.regen_stamina(self.vehicle.is_some(), ts);
        }

        // NPCs: CPedIntelligence::Process (the wander task) and SetMoveAnim (step 17).
        let alive = self.tasks.health.alive();
        if let (Some(npc), Some(clump), Some(m)) = (self.npc.as_mut(), self.clump.as_deref_mut(), self.tasks.anims.clone()) {
            if std::mem::take(&mut self.tasks.health.anim_reset) {
                npc.last_move_state = 0;
            }
            if alive && !busy && self.knocked_down <= 0.0 && self.vehicle.is_none() && self.enter.is_none() {
                if let Some(paths) = npc.paths.clone() {
                    let i = crate::npc::NpcIn { paths: &paths, anims: &m, now_ms: ctx.now_ms, frame: ctx.frame, ts };
                    // HandleEvents: respond to the highest-priority event.
                    if let Some(e) = npc.pick_event() {
                        if let Some(r) = npc.compute_response(&e, ctx.now_ms) {
                            npc.start_response(e, r, clump);
                        }
                    }
                    let ri = npc.resp_in;
                    let mut me = crate::pedevents::PedNow { pos: p.matrix.pos, move_speed: p.move_speed, aim_rot: &mut self.aim_rot, cur_rot: self.cur_rot };
                    // Souls mode enemies fight with the ER brain instead (below).
                    if self.souls_enemy.is_some() {
                    } else if !npc.process_response(&mut me, clump, &m, &mut self.tasks, &ri, &i)
                        && !npc.process_pursuit(&mut me, clump, &m, &mut self.tasks, &ri, &i)
                    {
                        npc.process(p.matrix.pos, p.move_speed, &mut self.aim_rot, self.cur_rot, &i);
                    }
                    // Secondary slot 0: an NPC's CTaskSimpleFight.
                    if let Some(mut f) = self.tasks.fight.take() {
                        let mut core = PedCore {
                            p,
                            clump,
                            cur_rot: &mut self.cur_rot,
                            aim_rot: &mut self.aim_rot,
                            turn_rate: &mut self.turn_rate,
                            standing: self.standing,
                            ground_below: self.ground_below,
                            ground_entity: self.ground_entity.is_some(),
                            ground_car: self.ground_entity.is_some() && self.ground_is_car,
                        };
                        if !f.process_ped(&mut self.tasks, &mut core, ctx, &m) {
                            self.tasks.fight = Some(f);
                        }
                    }
                    // Secondary slot 0: the gun control's CTaskSimpleUseGun.
                    if let Some(mut g) = self.tasks.gun.take() {
                        let mut core = PedCore {
                            p,
                            clump,
                            cur_rot: &mut self.cur_rot,
                            aim_rot: &mut self.aim_rot,
                            turn_rate: &mut self.turn_rate,
                            standing: self.standing,
                            ground_below: self.ground_below,
                            ground_entity: self.ground_entity.is_some(),
                            ground_car: self.ground_entity.is_some() && self.ground_is_car,
                        };
                        if !g.process_ped(&mut self.tasks, &mut core, ctx, &m) {
                            self.tasks.gun = Some(g);
                        }
                    }
                    self.tasks.ikm.process(clump, ctx.now_ms as i64, ctx.ts);
                    self.tasks.update_weapon(clump, ctx);
                }
                npc.set_move_anim(clump, &m);
            }
            // The clump alpha fade (per frame, not ×ts).
            npc.alpha = if npc.fading_out { npc.alpha.saturating_sub(8) } else { npc.alpha.saturating_add(16) };
        }

        // Souls mode: the ER enemy brain moves, turns and poses a ped fighting the player…
        if let (Some(e), Some(clump)) = (self.souls_enemy.as_deref_mut(), self.clump.as_deref_mut()) {
            if !self.tasks.health.alive() || self.vehicle.is_some() {
                self.souls_enemy = None;
            } else if self.souls_react.as_deref().is_some_and(|r| r.active()) {
                e.interrupt();
            } else {
                let (h, v) = e.step(p.matrix.pos, clump, ts);
                self.cur_rot = h;
                self.aim_rot = h;
                self.anim_velocity = v;
                self.tasks.fight = None;
            }
        }
        // …and an ER hurt reaction overrides the ped's pose, turn and anim velocity.
        if let (Some(r), Some(clump)) = (self.souls_react.as_deref_mut(), self.clump.as_deref_mut()) {
            if !self.tasks.health.alive() {
                self.souls_react = None;
            } else if let Some((h, v)) = r.step(clump, ts) {
                self.cur_rot = h;
                self.aim_rot = h;
                self.anim_velocity = v;
            }
        }
        // Jump task (from the intelligence step).
        if let Some(JumpKind::Speed(hs)) = self.jump_request.take() {
            if self.standing {
                self.launch_jump(p, hs);
            }
        }

        // Step 12: rising cap.
        if !self.standing && p.move_speed.z > 0.25 {
            if self.is_player {
                p.move_speed.z = 0.25;
            } else {
                p.move_speed *= 0.95f32.powf(ts);
            }
        }

        // Step 13: CPhysical::ProcessControl (NPC zero-speed skip not needed for the player).
        p.process_control(ctx);

        // Step 15: orientation and anim velocity.
        self.calculate_new_orientation(ts);
        let anim_vel = self.anim_world_velocity(&p.matrix);

        // Step 16: UpdatePosition (0x5E1B10), static-ground path.
        if self.standing || self.tasks.swim.is_some() {
            set_heading(&mut p.matrix, self.cur_rot);
        }
        if self.standing {
            // On static ground the horizontal velocity is exactly the anim velocity.
            p.move_speed.x = anim_vel.x;
            p.move_speed.y = anim_vel.y;
        }
        // CPlayerPed::ProcessControl enables the ceiling probe afterwards.
        self.ceiling_probe = self.is_player;
    }

    /// 0x5E3E90 SpecialEntityCalcCollisionSteps.
    fn collision_steps(&self, p: &Physical, ts: f32) -> (u8, bool) {
        let d = p.move_speed.length() * ts;
        if !self.is_player {
            if d * d < 0.09 {
                return (1, false);
            }
            return ((d * 5.0).ceil().clamp(1.0, 255.0) as u8, false);
        }
        let steps = if self.ground_entity.is_some() {
            (d * 6.666_666_5).ceil().max(4.0)
        } else {
            (d * 3.333_333_3).ceil().max(2.0)
        };
        (steps.min(255.0) as u8, false)
    }

    /// `CTaskSimpleUseGun::SetPedPosition` (after the physics): FireGun → CWeapon::Fire.
    fn process_effects(&mut self, id: EntityId, phys: &mut Physical, _col: &ColModel, fx: &mut FrameFx) {
        // IKChainManager_c::Update (after the collision), then SetPedPosition / FireGun.
        if let Some(clump) = self.clump.as_deref_mut() {
            let inv = phys.matrix.inverse();
            self.tasks.ikm.update_chains(clump, |w| inv.transform(w));
        }
        if let Some(clump) = self.clump.as_deref() {
            let reqs = crate::gun::fire_guns(&mut self.tasks, clump, phys, id, fx.now_ms);
            fx.requests.extend(reqs);
        }
        if let Some(n) = self.npc.as_mut() {
            for k in n.raised.drain(..) {
                let k = match k {
                    crate::pedevents::EventKind::SeenPanickedPed { threat, .. } => {
                        crate::pedevents::EventKind::SeenPanickedPed { fleer: id, threat }
                    }
                    k => k,
                };
                fx.requests.push(WorldRequest::PedEvent(k));
            }
            // CTaskSimpleDead: the DEAD_PED event, once.
            if matches!(self.tasks.health.life, crate::peddamage::Life::Wasted { .. }) && !n.dead_reported {
                n.dead_reported = true;
                fx.requests.push(WorldRequest::PedEvent(crate::pedevents::EventKind::DeadPed { dead: id }));
            }
        }
        for mut r in self.tasks.requests.drain(..) {
            if let WorldRequest::FireProjectile { owner, .. } = &mut r {
                *owner = id;
            }
            if let WorldRequest::MeleeStrike(s) = &mut r {
                s.owner = id;
            }
            fx.requests.push(r);
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
        self
    }
}

/// The two probe lines for one entity-collision call (model space).
pub fn ped_lines(was_standing: bool, ceiling_probe: bool, ts: f32) -> Vec<ColLine> {
    let k = ts * -0.15;
    let mut l0 = ColLine { start: Vec3::ZERO, end: Vec3::new(0.0, 0.0, -1.0) };
    if was_standing {
        l0.end.z += k;
    }
    let mut out = vec![l0];
    if ceiling_probe {
        let top = 0.6 + 0.35;
        let t = -0.2 - 0.35 + 1.0;
        out.push(ColLine { start: Vec3::new(0.0, 0.0, top - t), end: Vec3::new(0.0, 0.0, top + t) });
    }
    out
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::{
        collision::{ColBox, ColTriangle, TrianglePlane},
        world::World,
    };

    fn ground() -> Arc<ColModel> {
        let s = 100.0;
        let verts = vec![Vec3::new(-s, -s, 0.0), Vec3::new(s, -s, 0.0), Vec3::new(s, s, 0.0), Vec3::new(-s, s, 0.0)];
        let tris = vec![
            ColTriangle { v: [0, 2, 1], material: 1, light: 0 },
            ColTriangle { v: [0, 3, 2], material: 1, light: 0 },
        ];
        let planes = tris
            .iter()
            .map(|t| TrianglePlane::new(verts[t.v[0] as usize], verts[t.v[1] as usize], verts[t.v[2] as usize]))
            .collect();
        Arc::new(ColModel {
            bbox_min: Vec3::new(-s, -s, -0.1),
            bbox_max: Vec3::new(s, s, 0.1),
            bound_radius: s * 1.5,
            verts,
            tris,
            planes,
            ..Default::default()
        })
    }

    /// An axis-aligned block from y = y0 forward, `h` high.
    fn block(y0: f32, h: f32) -> Arc<ColModel> {
        let b = ColBox { min: Vec3::new(-10.0, y0, 0.0), max: Vec3::new(10.0, y0 + 10.0, h), surf: Surf::default() };
        let c = (b.min + b.max) * 0.5;
        Arc::new(ColModel {
            bbox_min: b.min,
            bbox_max: b.max,
            bound_center: c,
            bound_radius: (b.max - c).length(),
            boxes: vec![b],
            ..Default::default()
        })
    }

    fn world_with_ped(z: f32) -> (World, crate::world::EntityId) {
        let mut w = World::default();
        w.add_building(Matrix::IDENTITY, ground());
        let m = Matrix { pos: Vec3::new(0.0, 0.0, z), ..Matrix::IDENTITY };
        let id = w.add_body(ped_physical(m), ped_col_model(), Box::new(PedLogic::new(true, 0.0)));
        (w, id)
    }

    fn ped(w: &World, id: crate::world::EntityId) -> (&Physical, &PedLogic) {
        let b = w.body(id).unwrap();
        (&b.phys, b.logic.as_any().downcast_ref::<PedLogic>().unwrap())
    }

    #[test]
    fn falls_and_stands_one_unit_above_ground() {
        let (mut w, id) = world_with_ped(3.0);
        for _ in 0..60 {
            w.process(1.0);
        }
        let (p, s) = ped(&w, id);
        assert!(s.standing);
        assert!((p.matrix.pos.z - 1.0).abs() < 0.02, "z = {}", p.matrix.pos.z);
    }

    #[test]
    fn walks_at_anim_speed_and_climbs_a_curb_but_not_a_wall() {
        let (mut w, id) = world_with_ped(1.0);
        w.add_building(Matrix::IDENTITY, block(3.0, 0.2)); // curb 3 m ahead
        let set_anim = |w: &mut World| {
            w.body_mut(id).unwrap().logic.as_any_mut().downcast_mut::<PedLogic>().unwrap().anim_velocity = Vec2::new(0.0, 0.1);
        };
        for _ in 0..10 {
            w.process(1.0);
        }
        set_anim(&mut w);
        for _ in 0..50 {
            w.process(1.0);
        }
        let (p, s) = ped(&w, id);
        // 0.1 units/frame forward (+Y) for 50 frames, up onto the 0.2 curb.
        assert!(p.matrix.pos.y > 4.0, "y = {}", p.matrix.pos.y);
        assert!(s.standing);
        assert!((p.matrix.pos.z - 1.2).abs() < 0.05, "z = {}", p.matrix.pos.z);

        let (mut w, id) = world_with_ped(1.0);
        w.add_building(Matrix::IDENTITY, block(3.0, 2.0)); // wall 3 m ahead
        for _ in 0..10 {
            w.process(1.0);
        }
        set_anim(&mut w);
        for _ in 0..80 {
            w.process(1.0);
        }
        let (p, _) = ped(&w, id);
        assert!(p.matrix.pos.y < 3.0 && p.matrix.pos.y > 2.3, "stopped at y = {}", p.matrix.pos.y);
        assert!((p.matrix.pos.z - 1.0).abs() < 0.05);
    }
}
