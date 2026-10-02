//! `CColPoint`: one contact produced by the collision tests.

use glam::Vec3;

/// Piece types 13..=16 are the four wheels.
pub const PIECE_WHEEL_FIRST: u8 = 13;
pub const PIECE_WHEEL_LAST: u8 = 16;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct ColPoint {
    pub point: Vec3,
    /// Points from B towards A (A is the entity being processed).
    pub normal: Vec3,
    pub surface_a: u8,
    pub piece_a: u8,
    pub lighting_a: u8,
    pub surface_b: u8,
    pub piece_b: u8,
    pub lighting_b: u8,
    /// Penetration depth.
    pub depth: f32,
}

impl ColPoint {
    pub fn is_wheel_a(&self) -> bool {
        (PIECE_WHEEL_FIRST..=PIECE_WHEEL_LAST).contains(&self.piece_a)
    }

    pub fn is_wheel_b(&self) -> bool {
        (PIECE_WHEEL_FIRST..=PIECE_WHEEL_LAST).contains(&self.piece_b)
    }
}
