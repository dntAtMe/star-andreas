//! Readers for GTA San Andreas (PC 1.0) data formats.
//!
//! Everything here is engine-agnostic: parsers return plain data in the game's
//! native Z-up coordinate space. Conversion to the renderer's space happens in
//! the app crate.

pub mod audio;
pub mod bin;
pub mod col;
pub mod dat;
pub mod decision;
pub mod dff;
pub mod fonts;
pub mod fxp;
pub mod gxt;
pub mod ide;
pub mod ifp;
pub mod img;
pub mod ipl;
pub mod meleedat;
pub mod objdat;
pub mod population;
pub mod rw;
pub mod txd;
pub mod vehicle;
pub mod weapondat;
