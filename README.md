# sa-rs

GTA San Andreas world renderer in Rust + Bevy, reading assets from an existing
PC 1.0 install (nothing is modified or redistributed).

- `crates/sa-formats` — IMG, DFF, TXD, IDE, IPL (text + binary stream), gta.dat
- `crates/sa-formats` also reads COL collision, IFP (ANP3) animations, DFF skin/HAnim
- `crates/sa-app` — Bevy viewer: distance streaming, HD/LOD via `VisibilityRange`,
  BC textures uploaded as-is, Rapier collision from COL, skinned ped with IFP
  animation blending, kinematic character controller, orbit camera

```
cargo run -p sa-app -- "<game dir>"        # or SA_DIR env var
cargo run -p sa-formats --example validate --release -- "<game dir>"
```

Controls: `F` toggles walk / fly.
- Walk: click to grab the mouse (Esc releases), WASD, Shift sprint, Alt walk, Space jump, wheel = zoom.
- Fly: hold RMB to look, WASD/QE move, Shift fast, wheel = speed. Switching back to
  walk drops the player below the camera.

Debug env vars: `SA_POS=x,y,z,yaw,pitch` (GTA coords) start camera, `SA_PLAYER=x,y,z`
player spawn, `SA_FLY=1` start in fly mode, `SA_AUTOWALK=1` hold forward,
`SA_SHOT=out.png` save a screenshot once streaming settles, then exit.

## Build times

Dev builds use `rust-lld` (`.cargo/config.toml`), Bevy `dynamic_linking` (sa-app's
default `dev` feature) and no debuginfo for dependencies. A clean build is ~15 min;
incremental rebuilds of sa-app take about a minute. Because Bevy is a DLL, run through
`cargo run` (it sets PATH). For a standalone binary:
`cargo build --release -p sa-app --no-default-features`.
