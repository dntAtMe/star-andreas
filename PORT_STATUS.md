# Port status

Tracks what of GTA San Andreas (PC 1.0 US) has been ported to `sa-rs`, how faithfully, and what is left.
Ports follow the original executable's logic (reverse engineered from the binary), not
re-implementations "in the spirit of" SA.

Legend: ✅ ported (1:1 with the exe where it matters) · 🟡 partial / approximated · ❌ not started

Last updated: 2026-10-05.

## Assets and world

| System | Status | Notes |
|---|---|---|
| IMG, DFF, TXD, IDE, IPL (text + binary), gta.dat | ✅ | `sa-formats` |
| COL1/2/3 collision | ✅ | |
| DFF skin / HAnim, extra vertex colours (night prelight) | ✅ | |
| DFF 2d effects (2dfx) | 🟡 | lights decoded; particle, ped attractor, enex, roadsign, escalator… skipped |
| IFP (ANP3) animations | ✅ | |
| Anim blending (RpAnimBlend, CAnimBlendAssociation, group table) | ✅ | partial layers, movement phase lock, root motion; 3D root extraction and off-screen path not used |
| Streaming (distance, HD/LOD) | 🟡 | own scheme, not CStreaming / CRenderer lists |
| Interiors / entry-exits | ❌ | |
| Water (water.dat, CWaterLevel) | ❌ | placeholder sea plane only; blocks boats, underwater, splashes |
| Cull zones (tunnels, no-rain zones) | ❌ | |
| Time-cycle boxes (IPL `tcyc`) | ❌ | |

## Physics (`sa-physics`)

| System | Status | Notes |
|---|---|---|
| CPhysical (forces, collision response, friction, shift) | ✅ | |
| CCollision (sphere/box/line/triangle tests, ProcessColModels) | ✅ | |
| CWorld::Process loop, sectors, line of sight | 🟡 | dynamic bodies scanned as lists, not repeat sectors |
| Knockable props (object.dat uproot) | ✅ | |
| CAutomobile (suspension, wheels, transmission, handling) | ✅ | |
| Car damage (CDamageManager, VehicleDamage, doors, flying parts) | ✅ | bumpers don't bounce |
| Burst tyres (BurstTyre, DoBurstAndSoftGroundRatios, burst ProcessWheel) | ✅ | also sand sinking and bumpy surfaces |
| CPed collision / movement | 🟡 | player only; root motion from the anim blend clump |
| Bikes, boats, helis, planes, trains | ❌ | |
| Ped health, damage, death | ❌ | explosions/fire push peds but don't hurt them |
| Ragdoll / falls | ❌ | |

## Gameplay

| System | Status | Notes |
|---|---|---|
| Enter / exit vehicles | 🟡 | instant, no animations |
| Player locomotion (PlayerControlZelda, SetRealMoveAnim, sprint, walk_start, run stops) | ✅ | turning on the spot, adrenaline, fat/muscle groups missing |
| Crouch (CTaskSimpleDuck, PlayerControlDucked, crouch walk, crouch rolls, crouch fire) | ✅ | |
| Jump / in-air / land tasks | 🟡 | launch, glide, FALL_fall, land anims; climbing and the CTaskSimpleFall get-up missing |
| Explosions (CExplosion, TriggerExplosion, chain fuses) | ✅ | object damage / exploding objects not ported |
| Fires (CFireManager, CFire, creeping fire) | ✅ | peds catching fire not ported |
| CWeaponInfo (weapon.dat, skills), CWeapon (ammo, reload, Update, Fire) | ✅ | |
| Player weapon control (ProcessPlayerWeapon, CTaskSimpleUseGun, switching, anim groups) | ✅ | PC mouse free aim; lock-on, pistol whip, burst fire missing |
| Instant-hit bullets (FireInstantHit, shotgun pellets, DoBulletImpact) | 🟡 | vehicles (damage, force, tyres), objects (force), buildings; ped damage, CGlass, ObjectDamage breaking, petrol cap, water splashes missing |
| Aiming IK (CPedIK torso, IKChainManager CCD chains: arms, head look-at) | ✅ | bone limits from ms_boneInfos; chains only used by the gun task so far |
| Thrown weapons (CTaskSimpleThrowProjectile, grenade, tear gas, molotov, satchel + detonator) | ✅ | satchels stop where they hit instead of attaching to moving entities; tear gas choking needs ped damage |
| Rocket launcher (1st-person rocket camera, CProjectileInfo rockets) | 🟡 | heat-seeker lock-on and homing not ported (fires plain rockets, as SA does without a lock) |
| Area effect (FireAreaEffect, CShotInfo: flamethrower, extinguisher, spraycan) | 🟡 | spray tags and ped hits need ped damage / tags |
| Sniper, camera, melee | ❌ | |
| Traffic and pedestrians (population, paths, AI) | ❌ | |
| Wanted level, police | ❌ | |
| Pickups, collectables | ❌ | |
| Missions / SCM script interpreter | ❌ | |
| HUD, radar, menus | 🟡 | crosshair, weapon icon and ammo only; debug HUD + ImGui debug UI |
| Audio | ❌ | |
| Save / load | ❌ | |

## Cameras

| System | Status | Notes |
|---|---|---|
| On-foot follow camera (Process_FollowPed_SA, PC mouse) | ✅ | zoom on Home (V spawns cars); sphere-sweep collision approximated by a ray |
| Aim camera (Process_AimWeapon) and StartTransition easing | ✅ | |
| Car camera | ❌ | simple orbit |
| 1st-person weapon cameras (sniper, rocket, camera) | ❌ | |

## Clock, weather, time of day

| System | Status | Notes |
|---|---|---|
| CClock | ✅ | engine default start 12:00 (main.scm start time not read) |
| CWeather (cycles, regions, wind, rain, fog, lightning…) | ✅ | no cull zones, no water (UnderWaterness = 0), no sun-glare vector |
| Rain visuals (splashes, mist, streaks, sandstorm) | ✅ | rain grain post effect not ported |
| CTimeCycle (timecyc.dat, colours, sun vector) | ✅ | loader quirks reproduced; no IPL boxes / script extra colours |
| Sky gradient, fog, far clip, below-horizon grey | ✅ | |
| Moon, stars, low clouds, rainbow | ✅ | |
| Fluffy / volumetric clouds, SF moving fog, plane trails, shooting star | ❌ | |
| Sun reflection on water | ❌ | needs water |
| Building day/night prelight blend + ambient | ✅ | custom world material, gamma space |
| Ped / car lighting (Amb_Obj, directional) | 🟡 | PC DirMult is 0 (debug override available); Bevy PBR, not RW fixed function |

## Lights

| System | Status | Notes |
|---|---|---|
| CCoronas (register, fade, LOS, render, flares, wet reflections) | ✅ | flare LOS vs vehicles/peds and chromatic ghosts missing |
| Sun coronas + dazzle (LightsMult) | ✅ | |
| 2dfx world lights (ProcessLightsForEntity) | ✅ | riots, CHECK_DIRECTION, sun-glare effects missing |
| Traffic lights (DisplayActualLight) | 🟡 | lens quads (CBrightLights) and walk sign missing |
| CPointLights | 🟡 | drawn as Bevy point/spot lights (SA lights per object, not per pixel) |
| Point-light fog glow (RenderFogEffect) | ❌ | |
| Vehicle head / tail / brake lights, pools, beams, lamp materials | ✅ | alarms, ZR-350 pop-ups, gang rule, tunnels missing |
| Sirens, taxi light, FBI Rancher | ✅ | no siren key yet (debug UI toggle) |
| Bike / train lights, heli searchlight | ❌ | |

## Effects

| System | Status | Notes |
|---|---|---|
| Particle FX runtime (effects.fxp, FxManager/FxSystem/emitters) | ✅ | SMOKE secondary particles, GROUNDCOLLIDE missing |
| Car engine smoke, fire_car, explosions FX | ✅ | |
| Collision sparks, debris, collision smoke | ✅ | |
| Scrape sparks (ApplyFriction) | ✅ | |
| Wheel particles (tyre smoke, dirt, grass, sand, spray) | ✅ | |
| Exhaust smoke | ❌ | |
| Bullet impacts (AddBulletImpact, AddWood, AddTyreBurst), gunsmoke, shells, traces | ✅ | shells fall through the ground (GROUNDCOLLIDE missing); blood only without pools |
| Model gun flash, muzzle light | ✅ | |
| Heat haze post effect | ✅ | underwater variant missing |
| Colour filter post effect | ✅ | |
| SpeedFX, rain grain, underwater ripple, night/IR vision | ❌ | |
| Static / permanent shadows (scorches, fire glow, light pools) | ✅ | |
| Real-time car / ped shadows | ❌ | |
| Skid marks | ❌ | |

## Rendering differences from SA (known)

- Bevy (wgpu) renderer; map geometry uses a custom gamma-space material to match RW's fixed function,
  peds and cars still use Bevy PBR.
- Alpha-tested map textures use a 0.5 cutoff instead of SA's blend + alpha ref 2.
- Physics runs at 30 Hz (SA's typical frame rate) with interpolation; some frame-rate dependent
  originals (rain streaks) are tied to that step.

## Suggested next steps

1. Water (CWaterLevel): unblocks boats, splashes, underwater fog, sun reflection.
2. Ped health / damage / death (CPedDamageResponseCalculator), then projectiles and melee.
3. Traffic and pedestrian population.
4. Real-time shadows and skid marks.
5. Other vehicle classes (bikes first).
