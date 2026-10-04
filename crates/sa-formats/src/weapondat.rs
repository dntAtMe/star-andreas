//! `data/weapon.dat`: weapon definitions, raw rows as written in the file.
//!
//! Lines start with a marker: `0xA3` (pound sign, latin-1) melee weapons, `$` guns,
//! `%` gun aiming offsets, `ENDWEAPONDATA` ends the file. Conversion into the
//! game's `CWeaponInfo` (frames to seconds, skill table slots, flags) is done by
//! `sa_physics::weapon`.

use anyhow::{Context, Result};

#[derive(Debug, Clone)]
pub struct MeleeRow {
    pub name: String,
    pub fire_type: String,
    pub target_range: f32,
    pub weapon_range: f32,
    pub model1: i32,
    pub model2: i32,
    pub slot: i32,
    pub base_combo: String,
    pub num_combos: i32,
    /// Hex.
    pub flags: u32,
    pub stealth_anim_group: String,
}

#[derive(Debug, Clone)]
pub struct GunRow {
    pub name: String,
    pub fire_type: String,
    pub target_range: f32,
    pub weapon_range: f32,
    pub model1: i32,
    pub model2: i32,
    pub slot: i32,
    pub anim_group: String,
    pub ammo_clip: i32,
    pub damage: i32,
    pub fire_offset: [f32; 3],
    /// 0 poor, 1 std, 2 pro, 3 cop (pistol only).
    pub skill: i32,
    pub req_stat: i32,
    pub accuracy: f32,
    pub move_speed: f32,
    /// Anim loop start, end, fire (frames at 30 fps).
    pub anim_loop: [i32; 3],
    pub anim_loop2: [i32; 3],
    pub breakout_time: i32,
    /// Hex.
    pub flags: u32,
    /// Projectile / area effect only: speed, radius, lifespan, spread.
    pub speed: Option<f32>,
    pub radius: Option<f32>,
    pub lifespan: Option<f32>,
    pub spread: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct AimRow {
    pub anim_group: String,
    pub aim_x: f32,
    pub aim_z: f32,
    pub duck_x: f32,
    pub duck_z: f32,
    /// Reload sample times (ms): standing A/B, crouching A/B.
    pub reload: [i32; 2],
    pub crouch_reload: [i32; 2],
}

#[derive(Debug, Clone, Default)]
pub struct WeaponDat {
    pub melee: Vec<MeleeRow>,
    pub guns: Vec<GunRow>,
    pub aim: Vec<AimRow>,
}

pub fn parse(data: &[u8]) -> Result<WeaponDat> {
    let mut out = WeaponDat::default();
    for (ln, raw) in data.split(|&b| b == b'\n').enumerate() {
        let Some(&marker) = raw.iter().find(|b| !b.is_ascii_whitespace()) else { continue };
        let text: String = raw.iter().map(|&b| if b.is_ascii() { b as char } else { ' ' }).collect();
        let f: Vec<&str> = text.split_ascii_whitespace().collect();
        let ctx = || format!("weapon.dat line {}", ln + 1);
        match marker {
            0xA3 => {
                let g = Fields(&f[..]);
                out.melee.push(MeleeRow {
                    name: g.s(0),
                    fire_type: g.s(1),
                    target_range: g.f(2).with_context(ctx)?,
                    weapon_range: g.f(3).with_context(ctx)?,
                    model1: g.i(4).with_context(ctx)?,
                    model2: g.i(5).with_context(ctx)?,
                    slot: g.i(6).with_context(ctx)?,
                    base_combo: g.s(7),
                    num_combos: g.i(8).with_context(ctx)?,
                    flags: g.hex(9).with_context(ctx)?,
                    stealth_anim_group: g.s(10),
                });
            }
            b'$' => {
                let g = Fields(&f[1..]);
                let opt = |i: usize| g.f(i).ok();
                out.guns.push(GunRow {
                    name: g.s(0),
                    fire_type: g.s(1),
                    target_range: g.f(2).with_context(ctx)?,
                    weapon_range: g.f(3).with_context(ctx)?,
                    model1: g.i(4).with_context(ctx)?,
                    model2: g.i(5).with_context(ctx)?,
                    slot: g.i(6).with_context(ctx)?,
                    anim_group: g.s(7),
                    ammo_clip: g.i(8).with_context(ctx)?,
                    damage: g.i(9).with_context(ctx)?,
                    fire_offset: [g.f(10).with_context(ctx)?, g.f(11).with_context(ctx)?, g.f(12).with_context(ctx)?],
                    skill: g.i(13).with_context(ctx)?,
                    req_stat: g.i(14).with_context(ctx)?,
                    accuracy: g.f(15).with_context(ctx)?,
                    move_speed: g.f(16).with_context(ctx)?,
                    anim_loop: [g.i(17).with_context(ctx)?, g.i(18).with_context(ctx)?, g.i(19).with_context(ctx)?],
                    anim_loop2: [g.i(20).with_context(ctx)?, g.i(21).with_context(ctx)?, g.i(22).with_context(ctx)?],
                    breakout_time: g.i(23).with_context(ctx)?,
                    flags: g.hex(24).with_context(ctx)?,
                    speed: opt(25),
                    radius: opt(26),
                    lifespan: opt(27),
                    spread: opt(28),
                });
            }
            b'%' => {
                let g = Fields(&f[1..]);
                out.aim.push(AimRow {
                    anim_group: g.s(0),
                    aim_x: g.f(1).with_context(ctx)?,
                    aim_z: g.f(2).with_context(ctx)?,
                    duck_x: g.f(3).with_context(ctx)?,
                    duck_z: g.f(4).with_context(ctx)?,
                    reload: [g.i(5).with_context(ctx)?, g.i(6).with_context(ctx)?],
                    crouch_reload: [g.i(7).with_context(ctx)?, g.i(8).with_context(ctx)?],
                });
            }
            _ if text.trim_start().starts_with("ENDWEAPONDATA") => break,
            _ => {}
        }
    }
    Ok(out)
}

struct Fields<'a>(&'a [&'a str]);

impl Fields<'_> {
    fn get(&self, i: usize) -> Result<&str> {
        self.0.get(i).copied().with_context(|| format!("missing column {i}"))
    }
    fn s(&self, i: usize) -> String {
        self.0.get(i).copied().unwrap_or("").to_string()
    }
    fn f(&self, i: usize) -> Result<f32> {
        Ok(self.get(i)?.parse()?)
    }
    fn i(&self, i: usize) -> Result<i32> {
        Ok(self.get(i)?.parse()?)
    }
    fn hex(&self, i: usize) -> Result<u32> {
        Ok(u32::from_str_radix(self.get(i)?, 16)?)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn parses_rows() {
        let src = b"# c\n\xa3 UNARMED\t\tMELEE\t10.0  1.6\t-1\t-1\t\t0\t\tUNARMED\t\t4\t\t\t1\t\tnull\r\n\
$ PISTOL\t\t\tINSTANT_HIT\t25.0 30.0\t346\t-1\t\t2\tcolt45\t\t17\t 25\t\t0.25  0.05  0.09    0  0\t0.75 1.0 \t 6 17  6     6 16  6  99\t3033\n\
$ FTHROWER\t\t\tAREA_EFFECT\t4.0  5.1\t361\t-1\t\t7\tflame\t\t500\t 25\t\t0.98  0.0   0.40    1  0\t1.0\t 1.0 \t11 12 11\t11 12 11  35\t30238\t\t0.5  0.0075 1000.0\t 2.0\n\
% colt45\t\t\t0.2\t\t0.6\t\t\t0.1\t\t0.1\t\t254\t633\t\t254\t633\nENDWEAPONDATA\n$ X";
        let d = super::parse(src).unwrap();
        assert_eq!(d.melee.len(), 1);
        assert_eq!(d.melee[0].name, "UNARMED");
        assert_eq!(d.guns.len(), 2);
        assert_eq!(d.guns[0].flags, 0x3033);
        assert_eq!(d.guns[0].anim_loop2, [6, 16, 6]);
        assert_eq!(d.guns[1].flags, 0x30238);
        assert_eq!(d.guns[1].radius, Some(0.0075));
        assert_eq!(d.aim[0].crouch_reload, [254, 633]);
    }
}
