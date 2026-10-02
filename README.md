# sa-rs

GTA San Andreas world renderer in Rust + Bevy, reading assets from an existing
PC 1.0 install (nothing is modified or redistributed).

- `crates/sa-formats` — IMG, DFF, TXD, IDE, IPL (text + binary stream), gta.dat
- `crates/sa-app` — Bevy viewer: distance streaming, HD/LOD via `VisibilityRange`,
  BC textures uploaded as-is

```
cargo run -p sa-app -- "<game dir>"        # or SA_DIR env var
cargo run -p sa-formats --example validate --release -- "<game dir>"
```

Controls: hold RMB to look, WASD/QE move, Shift fast, wheel = speed.
Debug: `SA_POS=x,y,z,yaw,pitch` (GTA coords) sets the start camera,
`SA_SHOT=out.png` saves a screenshot once streaming settles and exits.
