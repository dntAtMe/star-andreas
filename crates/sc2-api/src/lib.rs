//! StarCraft II as a simulation backend.
//!
//! [`client`] speaks the raw protocol to a retail `SC2_x64.exe` started with
//! `-listen`; [`sim`] runs that client on its own thread in lockstep and
//! exposes engine-agnostic unit snapshots and commands.

pub mod client;
pub mod sim;

#[allow(clippy::all)]
pub mod proto {
    include!(concat!(env!("OUT_DIR"), "/sc2api_protocol.rs"));
}
