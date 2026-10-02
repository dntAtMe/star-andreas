# sa-rs

GTA San Andreas world renderer in Rust + Bevy, reading assets from an existing
PC 1.0 install (nothing is modified or redistributed).

- `crates/sa-formats` — IMG, DFF, TXD, IDE, IPL (text + binary stream), gta.dat
- `crates/sa-formats` also reads COL collision, IFP (ANP3) animations, DFF skin/HAnim,
  vehicles.ide, handling.cfg, carcols.dat and vehicle-embedded COL
- `crates/sa-app` — Bevy viewer: distance streaming, HD/LOD via `VisibilityRange`,
  BC textures uploaded as-is, Rapier collision from COL, skinned ped with IFP
  animation blending, kinematic character controller, orbit camera, drivable cars
  (raycast suspension + tire model from handling.cfg, carcols paint), knockable props
  from object.dat (lamp posts, hydrants, signs, bins... fly off above their uproot
  impulse while the car ploughs on)

```
cargo run -p sa-app -- "<game dir>"        # or SA_DIR env var
cargo run -p sa-formats --example validate --release -- "<game dir>"
```

Controls: `F2` toggles walk / fly.
- Walk: click to grab the mouse (Esc releases), WASD, Shift sprint, Alt walk, Space jump, wheel = zoom,
  `V` spawns a car in front of you (cycles models), `F` enters / exits the nearest car.
- Drive: W throttle, S brake / reverse, A/D steer, Space handbrake. The camera swings
  behind the car when the mouse is left alone.
- Fly: hold RMB to look, WASD/QE move, Shift fast, wheel = speed. Switching back to
  walk drops the player below the camera.

Debug env vars: `SA_POS=x,y,z,yaw,pitch` (GTA coords) start camera, `SA_PLAYER=x,y,z`
player spawn (optional 4th value: heading in degrees, 90 = west), `SA_FLY=1` start in
fly mode, `SA_AUTOWALK=1` hold forward / full throttle, `SA_DRIVE=infernus` spawn that car
and get in once the player can move, `SA_SHOT=out.png` save a screenshot once streaming
settles (or at `SA_SHOT_AFTER=<secs>`), then exit. `SA_LISTPROPS=x,y,radius` prints
knockable props near a position. `SA_FPS_CAP=<fps>` caps the frame rate.

Physics (Rapier, car forces, ped controller, prop impacts) runs in `FixedUpdate` at
120 Hz, so handling is identical at any frame rate; cars and the ped render at a pose
interpolated between physics steps.

## Build times

Dev builds use `rust-lld` (`.cargo/config.toml`), Bevy `dynamic_linking` (sa-app's
default `dev` feature) and no debuginfo for dependencies. A clean build is ~15 min;
incremental rebuilds of sa-app take about a minute. Because Bevy is a DLL, run through
`cargo run` (it sets PATH). For a standalone binary:
`cargo build --release -p sa-app --no-default-features`.
