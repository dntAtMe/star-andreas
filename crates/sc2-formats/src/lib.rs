//! Readers for StarCraft II (retail, 5.x) data.
//!
//! Like `sa-formats`, nothing here touches the installation: files are read
//! straight out of the local CASC storage under `SC2Data/`.

pub mod blte;
pub mod casc;
pub mod dds;
pub mod m3;
pub mod root;
