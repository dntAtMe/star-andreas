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
| Water (water.dat, CWaterLevel) | 🟡 | water.dat grid, GetWaterLevel(NoWaves) with SA's wave function, RenderWater (2 layers, waves within 48 units, flow scroll, timecyc colour/alpha, sea beyond the map, sea bed); no water fog, boat wakes, water1.dat, underwater draw order |
| Pedestrian population (CPopulation, CPopCycle, ped paths) | 🟡 | peds.ide / pedstats / popcycle / pedgrp / info.zon + main.scm zone settings, nodes0..63 ped nodes, the zone's 8 streamed models, civilian AddToPopulation with GeneratePedCreationCoors, ManagePed removal and fade; no cops, gangs, dealers, couples, attractors, interiors |
| NPC wandering (CTaskComplexWanderStandard, GoToPoint) | 🟡 | FindNextNodeWandering, junction turns, u-turns, crossings with the ped light cycle, scratch-head when stuck, NPC SetMoveAnim; no avoidance, events (flee / fight), ambient tasks |
| Motorbikes (CBike, ProcessBikeWheel) | 🟡 | bike handling (! lines), suspension lines, ProcessBikeWheel, visual lean from lateral g, balance damping, upright torque, wheelies / stoppies, lean torques (arrow keys), steer limit, burnout, knock-off detection, fork / swing arm / wheel / chassis frames; no rider (hidden) or rider anims, bicycles (CBmx), quad, bike buoyancy, AI, rest detection, fire |
| Object damage (CObject::ObjectDamage) | 🟡 | all object.dat columns, every object.dat model a CObject (mass >= 99998 static-collide), health / change-model ("_dam" atomics) / smash / breakable hide, TryToExplode / Explode (type 9, credited to the player), hit / destroy FX, damage from bullets (gun break modes), melee (×10), explosions (f×300) and impacts (> 20, bikes ×3, pass-through when destroyed); breakable pieces (BreakablePlugin data, BreakManager_c: per-material / per-triangle smash pieces, spin, settle, ground bounce with smoke/sparks, fade); no glass, dummy respawn, doors, lamppost tilt rule |
| Road traffic (CCarCtrl, CAutoPilot) | 🟡 | car nodes and navi links (lanes, lights), cargrp.dat models, GenerateOneRandomCar with the 160 m / 38 m creation rings, PickNextNodeRandomly with lanes and turn rules, SteerAICarWithPhysics FollowPath (pursuit point, bend / lane speed, gas / brake), traffic lights, slowing for cars and peds, stuck reverse, PossiblyRemoveVehicle distances; cars spawn in PHYSICS (no SIMPLE rails); drivers and passengers (SetUpDriverAndPassengersForVehicle, ChooseCivilianOccupationForVehicle by car mask / class, special drivers), the AI only with a driver, a killed driver's handbrake stop, JoinCarWithRoadSystem; carjacking (CAR_pullout / CAR_jacked, ComputeSlowJackedPed) with the DRAGGED_OUT_CAR response (flee, gesture + fight, gesture + 10 s jack-back and flee-drive); no police, gangs, boats, bikes, mad drivers, weaving |
| Boats (CBoat, ProcessBoatControl, ProcessBuoyancyBoat) | 🟡 | boat handling (`%` lines), 3×3 boat buoyancy with volume tables and wave-normal damping, thrust / rudder / aquaplaning / sideslip / handbrake drag / water and turn resistance / wave slam, prop and rudder animation, wake trail and rendering, bow splashes, damage / burning / blow-up and sinking; no hull water mask, boat AI, anchoring, marquis boom, flying radar, fire FX, boat camera |
| Swimming (CTaskComplexInWater / CTaskSimpleSwim, player) | 🟡 | tread / breaststroke / crawl / dive / underwater / jump-out states, surface hold, resurfacing pitch, breath drain and refill, render pitch and roll, exit in shallow water; no climbing out, NPC swimmers, torso twist, swim fx, camera tilt |
| Buoyancy (cBuoyancy) | 🟡 | peds (float, breath, drowning), cars (sinking, engine off), object.dat objects; no boats, no splashes |
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
| Ped health, damage, death (player, NPCs) | 🟡 | CPedDamageResponseCalculator (player ×0.33, NPC pedstats defendWeakness, armour, kill test, NPC headshot force-death: rifles always, player free aim always, else 1 in 8), skinned bullet hit col model (12 bone spheres, piece types), NPC gun reactions (body-part dam_* via BeHit, partial flinch while moving, shotgun torso knock-down, FLOOR_hit), cop pistol skill COP (colt_cop two-handed), player stamina regen (run +0.15, idle/walk +0.5, in car), respawn flushes tasks and IK, falls, explosions, car hits, bullet flinches, knock-down + get-up, death anims, WASTED + hospital respawn; no NPCs yet, no burning / drowning / choking, hospitals hard-coded (no main.scm) |
| Ragdoll / falls | ❌ | |

## Gameplay

| System | Status | Notes |
|---|---|---|
| NPC event responses (CEventGroup / decision makers) | 🟡 | PedEvent.txt + R_Norm/R_Tough/R_Weak/... decision makers with the add-time weighted roll; shot fired / whizzed by (45 m, silenced needs sight), gun aimed at (free-aim ray, seeing range), potential get run over (SlowCarDownForPeds + handler: evasive step / dive + get up / hands up / shake fist), seen panicked ped, dead ped, damage; responses: smart flee 911/910 (sprint wander away by octant, re-target, 60 m end, path hand-over), duck 415/427, react to gun aimed at 601 (heading, hands up 3–5 s, walk away 10 s; cower for GUN_PANIC), temporary-event parking; fight back: KillPedOnFoot 1000 → melee 1001 (seek at a run to 1 m, FightingControl 1019: attack timer from the shooting rate, block rolls, ChooseMovement shuffles/steps, AIChooseAttackMove, 8 m give-up), armed fighting 1002 (IsTargetVisible's 10 s LOS cache, seek with the shrinking radius, DECIDE: approach / close in / strafe / pause, GunControl 1020 timed bursts from the shooting rate, reload, back away under 4 m, end past weapon range; NPC UseGun with arm IK at the target and the AI spread), unarmed peds flee armed attackers; no acquaintances, groups, inform friends, look-at, cover points, ducking, weapon give at fight start, flee 909, investigate dead ped 600, speech |
| Wanted level (CWanted / CCrime) | 🟡 | chaos ↔ level table and caps, SetWantedLevel / NoDrop / Cheat / max level, ReportCrime with WorkOutPolicePresence (cops, police vehicles), the 16-slot crime queue (500 ms report, once per victim per 10 s), ReportCrimeNow K table and parole, decay (1/s, 2/s countryside outdoors, none ≥ 2 stars in cities, not near police or in a cop car); crimes from shots (1), bullet hits (4), player damage (2), explosions (4/5), running peds over (10/11), car theft (6), destroyed cars (20); HUD stars, cops, police cars, busted not yet |
| Police on foot | 🟡 | cops from FindNewPedType's cop share and the forced cop from 3 stars (model by zone level), CCopPed city setup (nightstick + pistol, rate 30, accuracy 60), the pursuit list (CanCopJoinPursuit / SetPursuitCop / displacing the farthest, RemoveExcess, 3 s re-join), PolicePursuit → ArrestPed → KillPedOnFoot (melee or armed by SetWeapon: nightstick at 1 star unless the player holds a gun, pistol from 2 stars), arrest when the target is down within 2 m / 3 m (ARRESTgun, target held down), BUSTED (fade, fine by level, weapons taken, police-station restart from main.scm); no arrests from cars, no police cars, helis, roadblocks |
| Melee hit reactions | 🟡 | ComputeDamageAnim for melee: fight-move hit anims (219 + move, combo group), FALL moves and low-health finishers knock down (KO anims, FallAndGetUp 1000/(rate·0.025) ms, knock force), HIT_front/side/behind, FLOOR_hit; no blocking victims |
| Enter / exit vehicles | 🟡 | player into empty cars: car search (10 m box, EvaluateCarPosition), GetNearestCarDoor (incl. passenger door + shuffle), door points from the anim groups' door offsets, run to the door, align / open door / get in / close door / shuffle anims with the line-up utility (600 ms fade, z blends, get-in slerp) and ProcessOpenDoor angles; bikes / boats warped in; exit from cars: brake until CanPedStepOutCar, get-out anim with the exit line-up (seat → door point, z after half the anim, slerp to upright) and the OpenIn door window, close door from outside (CloseOut window, skipped with stick input), set ped out (car abandoned); no jump-out at speed, crawl-out, PositionPedOutOfCollision; seated driver: SetPedPositionInCar (bike lean matrix), AddInCarAnim, car/boat ProcessDrivingAnims (skill sets, steer L/R, look-back), bike ProcessRiderAnims; no enter/exit sequences or doors yet |
| Player locomotion (PlayerControlZelda, SetRealMoveAnim, sprint, walk_start, run stops) | ✅ | turning on the spot, adrenaline, fat/muscle groups missing |
| Crouch (CTaskSimpleDuck, PlayerControlDucked, crouch walk, crouch rolls, crouch fire) | ✅ | |
| Jump / in-air / land tasks | 🟡 | launch, glide, FALL_fall, land anims; climbing and the CTaskSimpleFall get-up missing |
| Explosions (CExplosion, TriggerExplosion, chain fuses) | ✅ | object damage / exploding objects not ported |
| Fires (CFireManager, CFire, creeping fire) | ✅ | peds catching fire not ported |
| CWeaponInfo (weapon.dat, skills), CWeapon (ammo, reload, Update, Fire) | ✅ | |
| Player weapon control (ProcessPlayerWeapon, CTaskSimpleUseGun, switching, anim groups) | ✅ | PC mouse free aim; lock-on, pistol whip, burst fire missing |
| Instant-hit bullets (FireInstantHit, shotgun pellets, DoBulletImpact) | 🟡 | vehicles (damage, force, tyres), objects (force), buildings, peds (GenerateDamageEvent: weapon damage, pellets × hits, point blank 150, no same-type NPC hits), NPC shots at a target entity (spine, AI spread); CGlass, ObjectDamage breaking, petrol cap, water splashes missing |
| Aiming IK (CPedIK torso, IKChainManager CCD chains: arms, head look-at) | ✅ | bone limits from ms_boneInfos; chains only used by the gun task so far |
| Thrown weapons (CTaskSimpleThrowProjectile, grenade, tear gas, molotov, satchel + detonator) | ✅ | satchels stop where they hit instead of attaching to moving entities; tear gas choking needs ped damage |
| Rocket launcher (1st-person rocket camera, CProjectileInfo rockets) | 🟡 | heat-seeker lock-on and homing not ported (fires plain rockets, as SA does without a lock) |
| Area effect (FireAreaEffect, CShotInfo: flamethrower, extinguisher, spraycan) | 🟡 | spray tags and ped hits need ped damage / tags |
| Melee (melee.dat, CTaskSimpleFight, PlayerControlFighter, FightStrike) | 🟡 | combos and chaining, block, ground kick, moving attack, fight styles (player KICK_STD), shuffles, car damage and object pushes, ped damage + blood; no lock-on/mouse target, stealth kill, pistol whip, NPC fighting, audio, victims' hit anims |
| Sniper, camera | ❌ | |
| Traffic and pedestrians (population, paths, AI) | ❌ | |
| Wanted level, police | ❌ | |
| Pickups, collectables | ❌ | |
| Missions / SCM script interpreter | ❌ | |
| HUD, radar, menus | 🟡 | crosshair, weapon icon and ammo only; debug HUD + ImGui debug UI |
| Audio | ❌ | |
| Save / load | ❌ | |

## HUD

| Feature | Status | Notes |
|---|---|---|
| CFont | 🟡 | fonts.txd glyph grid with the exact UV insets, fonts.dat widths, style remaps (pricedown / menu), colour and `~n~` tokens, shadow (1 pass) and outline (8 passes), proportional advance with edge, orientation and word wrapping (ProcessCurrentString), justify gap; no button icons, slant, background box |
| CHud player info | 🟡 | clock, money ($%08d with the rolling display), weapon icon (fist / `<model>icon`), ammo `reserve-clip`, health (flashing < 10 hp, max-health width), armour and breath bars (DrawBarChart), wanted stars (2 s flash, parole stars, empty slots), zone name popup (CPlaceName, FindSmallestZoneForPosition over info.zon, DrawAreaName fade states, GOTHIC light blue) and vehicle name popup (vehicles.ide game name, DrawVehicleName, MENU green); GXT loader (american.gxt main + mission tables, CRC key hash); no help box, messages, `~1~`/`~k~` inserts |
| Radar (CRadar) | 🟡 | 3×3 radarNN tiles around the player, rotated with the camera heading, range 180 m on foot / 180–350 m by vehicle speed, clipped to the disc 24-gon, sea colour off the map, radardisc ring, north blip on the rim, player arrow (CSprite2d::Draw vertex order); no other blips, plane horizon / altimeter, gang overlay |

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

1. Carjacking, jump-out, bikes mount, radar blips and zone/vehicle names (docs ready), traffic drivers and SIMPLE rails mode, glass, bicycles, police cars (docs ready), pickups and flight (docs ready).
2. NPC ped damage responses.
3. Traffic and pedestrian population.
4. Real-time shadows and skid marks.
5. Other vehicle classes (bikes first).
