//! The script VM (scm.md §2-10): `CTheScripts` / `CRunningScript` — main.scm in ScriptSpace,
//! the 96-script pool with its active / idle lists, parameter decoding, variables, conditions,
//! jumps, gosub, switch, threads and the mission block. Game commands go to a [`Host`].

use std::collections::HashMap;

use super::ops::OPS;

/// MAIN's part of ScriptSpace; the mission block follows (`ScriptSpace + 200000`).
pub const MAIN_SIZE: usize = 200_000;
pub const MISSION_SIZE: usize = 69_000;
const POOL: usize = 96;
const MISSION_LOCALS: usize = 1024;

/// A variable reference (`GetPointerToScriptVariable`): a byte offset into ScriptSpace, or a
/// dword index into the running script's locals.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Addr {
    Global(usize),
    Local(usize),
}

/// `CRunningScript` (0xE0 bytes).
#[derive(Clone)]
pub struct Script {
    pub name: String,
    next: Option<usize>,
    prev: Option<usize>,
    /// Base offset in ScriptSpace for negative labels (0 for MAIN-space threads).
    pub base: usize,
    pub ip: usize,
    stack: [usize; 8],
    sp: usize,
    pub locals: [i32; 34],
    pub active: bool,
    pub cond: bool,
    pub use_cleanup: bool,
    pub external: bool,
    pub wake: u32,
    logical_op: u16,
    not_flag: bool,
    pub death_arrest_enabled: bool,
    pub death_arrest_executed: bool,
    pub scene_skip: i32,
    pub is_mission: bool,
}

impl Script {
    /// `CRunningScript::Init` (0x4648E0).
    fn init() -> Self {
        Self {
            name: "noname".into(),
            next: None,
            prev: None,
            base: 0,
            ip: 0,
            stack: [0; 8],
            sp: 0,
            locals: [0; 34],
            active: false,
            cond: false,
            use_cleanup: false,
            external: false,
            wake: 0,
            logical_op: 0,
            not_flag: false,
            death_arrest_enabled: true,
            death_arrest_executed: false,
            scene_skip: 0,
            is_mission: false,
        }
    }
}

/// What a command handler returns (0 continue / 1 yield).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Flow {
    Continue,
    Yield,
}

/// The game side of the script commands.
pub trait Host {
    /// `CTimer::m_snTimeInMilliseconds`.
    fn now_ms(&self) -> u32;
    /// `CTimer::ms_fTimeStep` (50 fps units) for the timed arithmetic.
    fn timestep(&self) -> f32 {
        1.0
    }
    /// `IsCutsceneSkipButtonBeingPressed` (scene skip).
    fn skip_pressed(&mut self) -> bool {
        false
    }
    /// `IsRestartingAfterDeath() || IsRestartingAfterArrest()` of the player in focus.
    fn player_restarting(&mut self) -> bool {
        false
    }
    /// A command the VM itself does not handle; None = unimplemented (params skipped).
    fn command(&mut self, x: &mut Exec, op: u16) -> Option<Flow>;
}

/// `CTheScripts` state.
pub struct Vm {
    /// The whole main.scm (missions are copied from here into the mission block).
    file: Vec<u8>,
    pub space: Vec<u8>,
    pub mission_locals: Vec<i32>,
    pub scripts: Vec<Script>,
    active: Option<usize>,
    idle: Option<usize>,
    pub mission_offsets: Vec<u32>,
    pub model_names: Vec<String>,
    /// `OnAMissionFlag`: byte offset of the mission flag global (0 = none).
    pub on_a_mission_flag: usize,
    pub running_mission: bool,
    /// SWITCH accumulation (0xA43F50..).
    switch_pairs: Vec<(i32, i32)>,
    switch_var: i32,
    switch_default: i32,
    switch_to_read: i32,
    /// `ScriptParams`.
    pub params: [i32; 32],
    pub commands_executed: u32,
    unknown: HashMap<u16, u32>,
    counts: Vec<u8>,
    names: Vec<&'static str>,
}

fn rd_u32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(d[o..o + 4].try_into().unwrap())
}

impl Vm {
    /// `CTheScripts::Init` (0x468D50): ScriptSpace ← the first 200 000 bytes, header tables,
    /// the idle pool, and MAIN started at offset 0 (`StartTestScript`).
    pub fn new(file: Vec<u8>) -> Self {
        let mut space = vec![0u8; MAIN_SIZE + MISSION_SIZE];
        let n = file.len().min(MAIN_SIZE);
        space[..n].copy_from_slice(&file[..n]);
        // Header segments (§2): seg1 models, seg2 missions.
        let seg1 = rd_u32(&file, 3) as usize;
        let num_models = rd_u32(&file, seg1 + 8) as usize;
        let model_names = (0..num_models)
            .map(|i| {
                let b = &file[seg1 + 12 + i * 24..seg1 + 12 + (i + 1) * 24];
                let e = b.iter().position(|&c| c == 0).unwrap_or(24);
                String::from_utf8_lossy(&b[..e]).into_owned()
            })
            .collect();
        let seg2 = rd_u32(&file, seg1 + 3) as usize;
        let num_missions = u16::from_le_bytes([file[seg2 + 16], file[seg2 + 17]]) as usize;
        let mission_offsets = (0..num_missions).map(|i| rd_u32(&file, seg2 + 24 + 4 * i)).collect();
        let mut counts = vec![u8::MAX; 0x8000];
        let mut names = vec![""; 0x8000];
        for &(op, n, name) in OPS {
            counts[op as usize] = n;
            names[op as usize] = name;
        }
        let mut vm = Self {
            file,
            space,
            mission_locals: vec![0; MISSION_LOCALS],
            scripts: vec![Script::init(); POOL],
            active: None,
            idle: None,
            mission_offsets,
            model_names,
            on_a_mission_flag: 0,
            running_mission: false,
            switch_pairs: Vec::new(),
            switch_var: 0,
            switch_default: 0,
            switch_to_read: 0,
            params: [0; 32],
            commands_executed: 0,
            unknown: HashMap::new(),
            counts,
            names,
        };
        // The idle list in array order; the last one ends up at the head.
        for i in 0..POOL {
            vm.push_front(i, false);
        }
        vm.start_new_script(0);
        vm
    }

    pub fn op_name(&self, op: u16) -> &'static str {
        self.names.get(op as usize).copied().unwrap_or("")
    }

    fn head(&mut self, active: bool) -> &mut Option<usize> {
        if active { &mut self.active } else { &mut self.idle }
    }

    /// `AddScriptToList` (push front).
    fn push_front(&mut self, i: usize, active: bool) {
        let head = *self.head(active);
        self.scripts[i].next = head;
        self.scripts[i].prev = None;
        if let Some(h) = head {
            self.scripts[h].prev = Some(i);
        }
        *self.head(active) = Some(i);
    }

    /// `RemoveScriptFromList`.
    fn unlink(&mut self, i: usize, active: bool) {
        let (p, n) = (self.scripts[i].prev, self.scripts[i].next);
        match p {
            Some(p) => self.scripts[p].next = n,
            None => *self.head(active) = n,
        }
        if let Some(n) = n {
            self.scripts[n].prev = p;
        }
    }

    /// `StartNewScript(ip)` (0x464C20).
    pub fn start_new_script(&mut self, ip: usize) -> Option<usize> {
        let i = self.idle?;
        self.unlink(i, false);
        self.scripts[i] = Script::init();
        self.scripts[i].ip = ip;
        self.push_front(i, true);
        self.scripts[i].active = true;
        Some(i)
    }

    /// `TERMINATE_THIS_SCRIPT` / `ShutdownThisScript`.
    fn terminate(&mut self, i: usize) {
        if self.scripts[i].is_mission {
            self.running_mission = false;
        }
        self.unlink(i, true);
        self.push_front(i, false);
        self.scripts[i].active = false;
    }

    /// The active scripts in list order.
    pub fn active_scripts(&self) -> Vec<usize> {
        let mut v = Vec::new();
        let mut c = self.active;
        while let Some(i) = c {
            v.push(i);
            c = self.scripts[i].next;
        }
        v
    }

    /// `0417 LOAD_AND_LAUNCH_MISSION_INTERNAL`.
    pub fn launch_mission(&mut self, n: usize) {
        let Some(&off) = self.mission_offsets.get(n) else { return };
        let off = off as usize;
        let end = (off + MISSION_SIZE).min(self.file.len());
        let len = end - off;
        self.space[MAIN_SIZE..MAIN_SIZE + len].copy_from_slice(&self.file[off..end]);
        self.space[MAIN_SIZE + len..].fill(0);
        self.mission_locals.fill(0);
        if let Some(s) = self.start_new_script(MAIN_SIZE) {
            let sc = &mut self.scripts[s];
            sc.use_cleanup = true;
            sc.is_mission = true;
            sc.base = MAIN_SIZE;
        }
        self.running_mission = true;
    }

    pub fn global(&self, off: usize) -> i32 {
        self.space.get(off..off + 4).map_or(0, |b| i32::from_le_bytes(b.try_into().unwrap()))
    }

    pub fn set_global(&mut self, off: usize, v: i32) {
        if let Some(b) = self.space.get_mut(off..off + 4) {
            b.copy_from_slice(&v.to_le_bytes());
        }
    }

    /// `CTheScripts::Process` (0x46A000): every active script once, in list order.
    pub fn process(&mut self, host: &mut dyn Host, dt_ms: u32) {
        self.commands_executed = 0;
        self.mission_locals[32] = self.mission_locals[32].wrapping_add(dt_ms as i32);
        self.mission_locals[33] = self.mission_locals[33].wrapping_add(dt_ms as i32);
        let mut cur = self.active;
        while let Some(s) = cur {
            let next = self.scripts[s].next;
            self.scripts[s].locals[33] = self.scripts[s].locals[33].wrapping_add(dt_ms as i32);
            self.scripts[s].locals[32] = self.scripts[s].locals[32].wrapping_add(dt_ms as i32);
            self.process_script(s, host);
            match next {
                Some(n) if self.scripts[n].active => cur = Some(n),
                _ => break,
            }
        }
    }

    /// `CRunningScript::Process` (0x469F00).
    fn process_script(&mut self, s: usize, host: &mut dyn Host) {
        let sc = &mut self.scripts[s];
        if sc.scene_skip != 0 && host.skip_pressed() {
            let sc = &mut self.scripts[s];
            sc.ip = if sc.scene_skip < 0 { (sc.base as i64 - sc.scene_skip as i64) as usize } else { sc.scene_skip as usize };
            sc.scene_skip = 0;
            sc.wake = 0;
        }
        if self.scripts[s].use_cleanup {
            self.death_arrest_check(s, host);
        }
        self.switch_pairs.clear();
        self.switch_to_read = 0;
        if self.scripts[s].wake > host.now_ms() {
            return;
        }
        let mut x = Exec { vm: self, s };
        loop {
            x.vm.commands_executed += 1;
            let ip = x.vm.scripts[s].ip;
            let Some(raw) = x.vm.space.get(ip..ip + 2).map(|b| u16::from_le_bytes([b[0], b[1]])) else {
                x.vm.scripts[s].wake = u32::MAX;
                return;
            };
            x.vm.scripts[s].ip += 2;
            x.vm.scripts[s].not_flag = raw & 0x8000 != 0;
            let op = raw & 0x7FFF;
            let flow = match x.core(op, host) {
                Some(f) => f,
                None => match host.command(&mut x, op) {
                    Some(f) => f,
                    None => x.unknown(op),
                },
            };
            if flow == Flow::Yield || !x.vm.scripts[s].active {
                return;
            }
        }
    }

    /// `DoDeathArrestCheck` (scm.md §5.3).
    fn death_arrest_check(&mut self, s: usize, host: &mut dyn Host) {
        let sc = &self.scripts[s];
        if !sc.death_arrest_enabled || self.on_a_mission_flag == 0 || self.global(self.on_a_mission_flag) != 1 {
            return;
        }
        if !host.player_restarting() {
            return;
        }
        let sc = &mut self.scripts[s];
        if sc.sp > 1 {
            sc.sp = 1;
        }
        if sc.sp == 0 {
            return;
        }
        sc.sp -= 1;
        sc.ip = sc.stack[sc.sp];
        sc.death_arrest_executed = true;
        sc.wake = 0;
        let f = self.on_a_mission_flag;
        self.set_global(f, 0);
    }
}

/// One script executing commands: the parameter readers of `CRunningScript`.
pub struct Exec<'a> {
    pub vm: &'a mut Vm,
    pub s: usize,
}

impl Exec<'_> {
    pub fn script(&mut self) -> &mut Script {
        &mut self.vm.scripts[self.s]
    }

    fn u8(&mut self) -> u8 {
        let ip = self.vm.scripts[self.s].ip;
        self.vm.scripts[self.s].ip += 1;
        self.vm.space.get(ip).copied().unwrap_or(0)
    }

    fn i16(&mut self) -> i16 {
        i16::from_le_bytes([self.u8(), self.u8()])
    }

    fn u16(&mut self) -> u16 {
        u16::from_le_bytes([self.u8(), self.u8()])
    }

    fn i32(&mut self) -> i32 {
        i32::from_le_bytes([self.u8(), self.u8(), self.u8(), self.u8()])
    }

    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.u8()).collect()
    }

    fn local(&self, i: usize) -> i32 {
        if self.vm.scripts[self.s].is_mission {
            self.vm.mission_locals.get(i).copied().unwrap_or(0)
        } else {
            self.vm.scripts[self.s].locals.get(i).copied().unwrap_or(0)
        }
    }

    fn local_mut(&mut self, i: usize) -> Option<&mut i32> {
        if self.vm.scripts[self.s].is_mission {
            self.vm.mission_locals.get_mut(i)
        } else {
            self.vm.scripts[self.s].locals.get_mut(i)
        }
    }

    pub fn read(&self, a: Addr) -> i32 {
        match a {
            Addr::Global(o) => self.vm.global(o),
            Addr::Local(i) => self.local(i),
        }
    }

    pub fn write(&mut self, a: Addr, v: i32) {
        match a {
            Addr::Global(o) => self.vm.set_global(o, v),
            Addr::Local(i) => {
                if let Some(x) = self.local_mut(i) {
                    *x = v;
                }
            }
        }
    }

    /// `n` bytes at a variable (strings live in consecutive dwords).
    fn read_bytes(&self, a: Addr, n: usize) -> Vec<u8> {
        match a {
            Addr::Global(o) => self.vm.space.get(o..o + n).map(|b| b.to_vec()).unwrap_or_default(),
            Addr::Local(i) => (0..n.div_ceil(4)).flat_map(|k| self.local(i + k).to_le_bytes()).take(n).collect(),
        }
    }

    fn write_bytes(&mut self, a: Addr, data: &[u8]) {
        match a {
            Addr::Global(o) => {
                if let Some(b) = self.vm.space.get_mut(o..o + data.len()) {
                    b.copy_from_slice(data);
                }
            }
            Addr::Local(i) => {
                for (k, c) in data.chunks(4).enumerate() {
                    let mut w = self.local(i + k).to_le_bytes();
                    w[..c.len()].copy_from_slice(c);
                    self.write(Addr::Local(i + k), i32::from_le_bytes(w));
                }
            }
        }
    }

    /// The index of an array parameter: `flags & 0x80` → global int at byte offset, else local.
    fn array_index(&mut self) -> (usize, i32) {
        let base = self.u16() as usize;
        let idx_var = self.u16() as usize;
        let _size = self.u8();
        let flags = self.u8();
        let i = if flags & 0x80 != 0 { self.vm.global(idx_var) } else { self.local(idx_var) };
        (base, i)
    }

    /// One `CollectParameters` value; None for the `00` end-of-list type.
    fn value(&mut self) -> Option<i32> {
        let t = self.u8();
        Some(match t {
            0 => return None,
            1 => self.i32(),
            2 => {
                let o = self.u16() as usize;
                self.vm.global(o)
            }
            3 => {
                let i = self.u16() as usize;
                self.local(i)
            }
            4 => self.u8() as i8 as i32,
            5 => self.i16() as i32,
            6 => self.i32(),
            7 => {
                let (b, i) = self.array_index();
                self.vm.global((b as i64 + 4 * i as i64) as usize)
            }
            8 => {
                let (b, i) = self.array_index();
                self.local((b as i64 + i as i64) as usize)
            }
            _ => 0,
        })
    }

    /// `CollectParameters(n)` into `params`.
    pub fn collect(&mut self, n: usize) {
        for k in 0..n {
            if let Some(v) = self.value() {
                self.vm.params[k] = v;
            }
        }
    }

    pub fn int(&mut self) -> i32 {
        self.value().unwrap_or(0)
    }

    pub fn float(&mut self) -> f32 {
        f32::from_bits(self.int() as u32)
    }

    pub fn ints<const N: usize>(&mut self) -> [i32; N] {
        std::array::from_fn(|_| self.int())
    }

    pub fn floats<const N: usize>(&mut self) -> [f32; N] {
        std::array::from_fn(|_| self.float())
    }

    /// `GetPointerToScriptVariable`.
    pub fn var(&mut self) -> Addr {
        let t = self.u8();
        match t {
            2 | 0xA | 0x10 => Addr::Global(self.u16() as usize),
            3 | 0xB | 0x11 => Addr::Local(self.u16() as usize),
            7 => {
                let (b, i) = self.array_index();
                Addr::Global((b as i64 + 4 * i as i64) as usize)
            }
            8 => {
                let (b, i) = self.array_index();
                Addr::Local((b as i64 + i as i64) as usize)
            }
            0xC => {
                let (b, i) = self.array_index();
                Addr::Global((b as i64 + 8 * i as i64) as usize)
            }
            0xD => {
                let (b, i) = self.array_index();
                Addr::Local((b as i64 + 2 * i as i64) as usize)
            }
            0x12 => {
                let (b, i) = self.array_index();
                Addr::Global((b as i64 + 16 * i as i64) as usize)
            }
            0x13 => {
                let (b, i) = self.array_index();
                Addr::Local((b as i64 + 4 * i as i64) as usize)
            }
            _ => Addr::Global(0),
        }
    }

    /// `StoreParameters`: write the values through the next output variables.
    pub fn store(&mut self, vals: &[i32]) {
        for &v in vals {
            let a = self.var();
            self.write(a, v);
        }
    }

    /// `ReadTextLabelFromScript` (8 or 16 byte strings).
    pub fn text(&mut self) -> String {
        let t = self.u8();
        let raw: Vec<u8> = match t {
            9 => self.bytes(8),
            0xE => {
                let n = self.u8() as usize;
                self.bytes(n)
            }
            0xF => self.bytes(16),
            0xA | 0xB | 0xC | 0xD | 0x10 | 0x11 | 0x12 | 0x13 => {
                // Re-read as a variable reference.
                self.vm.scripts[self.s].ip -= 1;
                let a = self.var();
                let n = if t >= 0x10 { 16 } else { 8 };
                self.read_bytes(a, n)
            }
            _ => Vec::new(),
        };
        let e = raw.iter().position(|&c| c == 0).unwrap_or(raw.len());
        String::from_utf8_lossy(&raw[..e]).into_owned()
    }

    /// The parameter count of an opcode (from the handlers).
    pub fn vm_count(&self, op: u16) -> usize {
        self.vm.counts.get(op as usize).copied().filter(|&n| n != u8::MAX).unwrap_or(0) as usize
    }

    /// Skip one parameter of any type.
    pub fn skip_param(&mut self) {
        let t = self.u8();
        let n = match t {
            0 => 0,
            1 | 6 => 4,
            2 | 3 | 5 | 0xA | 0xB | 0x10 | 0x11 => 2,
            4 => 1,
            7 | 8 | 0xC | 0xD | 0x12 | 0x13 => 6,
            9 => 8,
            0xE => self.u8() as usize,
            0xF => 16,
            _ => 0,
        };
        self.vm.scripts[self.s].ip += n;
    }

    /// `UpdateCompareFlag` (0x4859D0).
    pub fn cond(&mut self, b: bool) {
        let sc = &mut self.vm.scripts[self.s];
        let b = if sc.not_flag { !b } else { b };
        match sc.logical_op {
            0 => sc.cond = b,
            1..=8 => {
                sc.cond &= b;
                sc.logical_op = if sc.logical_op == 1 { 0 } else { sc.logical_op - 1 };
            }
            21..=28 => {
                sc.cond |= b;
                sc.logical_op = if sc.logical_op == 21 { 0 } else { sc.logical_op - 1 };
            }
            _ => {}
        }
    }

    /// `UpdatePC`.
    pub fn jump(&mut self, label: i32) {
        let sc = &mut self.vm.scripts[self.s];
        sc.ip = if label < 0 { (sc.base as i64 - label as i64) as usize } else { label as usize };
    }

    pub fn mission_cleanup(&self) -> bool {
        self.vm.scripts[self.s].use_cleanup
    }

    /// An unimplemented command: its parameters are skipped (counts from the handlers).
    fn unknown(&mut self, op: u16) -> Flow {
        let n = self.vm.counts.get(op as usize).copied().unwrap_or(u8::MAX);
        let c = self.vm.unknown.entry(op).or_insert(0);
        *c += 1;
        if *c == 1 {
            let name = self.vm.op_name(op);
            bevy::log::warn!("script {}: unimplemented {op:04X} {name}", self.vm.scripts[self.s].name);
        }
        if n == u8::MAX {
            // Unknown parameter count: the original would desync; stop this script.
            self.vm.scripts[self.s].wake = u32::MAX;
            return Flow::Yield;
        }
        for _ in 0..n {
            self.skip_param();
        }
        Flow::Continue
    }

    /// Typed values until `00` into the new script's locals (`ReadParametersForNewlyCreatedScript`).
    fn args_into(&mut self, ns: Option<usize>) {
        let mut k = 0;
        while let Some(v) = self.value() {
            if let Some(ns) = ns {
                if let Some(l) = self.vm.scripts[ns].locals.get_mut(k) {
                    *l = v;
                }
            }
            k += 1;
        }
    }

    fn binop(&mut self, float: bool, f: fn(i32, i32) -> i32, g: fn(f32, f32) -> f32) {
        let a = self.var();
        let v = self.int();
        let cur = self.read(a);
        let r = if float { g(f32::from_bits(cur as u32), f32::from_bits(v as u32)).to_bits() as i32 } else { f(cur, v) };
        self.write(a, r);
    }

    fn compare(&mut self, float: bool, f: fn(i32, i32) -> bool, g: fn(f32, f32) -> bool) {
        let a = self.int();
        let b = self.int();
        let r = if float { g(f32::from_bits(a as u32), f32::from_bits(b as u32)) } else { f(a, b) };
        self.cond(r);
    }

    /// The commands the VM handles itself (flow, variables, threads); None = the host's.
    fn core(&mut self, op: u16, host: &mut dyn Host) -> Option<Flow> {
        let add = |a: i32, b: i32| a.wrapping_add(b);
        let sub = |a: i32, b: i32| a.wrapping_sub(b);
        let mul = |a: i32, b: i32| a.wrapping_mul(b);
        let div = |a: i32, b: i32| if b == 0 { 0 } else { a.wrapping_div(b) };
        let ops_i: [fn(i32, i32) -> i32; 4] = [add, sub, mul, div];
        let ops_f: [fn(f32, f32) -> f32; 4] = [|a, b| a + b, |a, b| a - b, |a, b| a * b, |a, b| a / b];
        match op {
            0x0000 => {}
            0x0001 => {
                let ms = self.int();
                self.script().wake = host.now_ms().wrapping_add(ms as u32);
                return Some(Flow::Yield);
            }
            0x0002 => {
                let l = self.int();
                self.jump(l);
            }
            // Assignments ($ / @ = int / float): a raw 32-bit copy.
            0x0004..=0x0007 | 0x0084..=0x008B | 0x04AE | 0x04AF => {
                let a = self.var();
                let v = self.int();
                self.write(a, v);
            }
            // += -= *= /= with a value.
            0x0008..=0x0017 => {
                let k = ((op - 0x08) / 4) as usize;
                self.binop(op & 1 == 1, ops_i[k], ops_f[k]);
            }
            0x0058..=0x0077 => {
                let k = ((op - 0x58) / 8) as usize;
                self.binop(op & 1 == 1, ops_i[k], ops_f[k]);
            }
            // Timed += / -= (value × timestep).
            0x0078..=0x0083 => {
                let a = self.var();
                let v = self.float();
                let ts = host.timestep();
                let cur = f32::from_bits(self.read(a) as u32);
                let r = if op <= 0x007D { cur + v * ts } else { cur - v * ts };
                self.write(a, r.to_bits() as i32);
            }
            // int ← float (truncating) / float ← int.
            0x008C..=0x0093 => {
                let a = self.var();
                let v = self.int();
                let r = if op & 1 == 0 { f32::from_bits(v as u32) as i32 } else { (v as f32).to_bits() as i32 };
                self.write(a, r);
            }
            // ABS.
            0x0094..=0x0097 => {
                let a = self.var();
                let v = self.read(a);
                let r = if op <= 0x0095 { v.wrapping_abs() } else { f32::from_bits(v as u32).abs().to_bits() as i32 };
                self.write(a, r);
            }
            0x0018..=0x001F => self.compare(false, |a, b| a > b, |a, b| a > b),
            0x0020..=0x0027 => self.compare(true, |a, b| a > b, |a, b| a > b),
            0x0028..=0x002F => self.compare(false, |a, b| a >= b, |a, b| a >= b),
            0x0030..=0x0037 => self.compare(true, |a, b| a >= b, |a, b| a >= b),
            0x0038..=0x003C | 0x04A3 | 0x04A4 | 0x07D6 => self.compare(false, |a, b| a == b, |a, b| a == b),
            0x0042..=0x0046 => self.compare(true, |a, b| a == b, |a, b| a == b),
            0x004D => {
                let l = self.int();
                if !self.script().cond {
                    self.jump(l);
                }
            }
            0x004E => {
                self.vm.terminate(self.s);
                return Some(Flow::Yield);
            }
            0x004F => {
                let mut off = self.int();
                if off < 0 {
                    off = 0x4F;
                }
                let ns = self.vm.start_new_script(off as usize);
                self.args_into(ns);
            }
            0x0050 => {
                let l = self.int();
                let sc = self.script();
                let ip = sc.ip;
                if sc.sp < 8 {
                    sc.stack[sc.sp] = ip;
                    sc.sp += 1;
                }
                self.jump(l);
            }
            0x0051 => {
                let sc = self.script();
                if sc.sp > 0 {
                    sc.sp -= 1;
                    sc.ip = sc.stack[sc.sp];
                }
            }
            0x00D6 => {
                let n = self.int();
                let sc = self.script();
                match n {
                    0 => {
                        sc.logical_op = 0;
                        sc.cond = false;
                    }
                    1..=8 => {
                        sc.logical_op = n as u16 + 1;
                        sc.cond = true;
                    }
                    21..=28 => {
                        sc.logical_op = n as u16 + 1;
                        sc.cond = false;
                    }
                    _ => sc.logical_op = n as u16,
                }
            }
            0x00D7 => {
                let l = self.int();
                self.vm.start_new_script(l as usize);
            }
            0x0111 => {
                let b = self.int();
                self.script().death_arrest_enabled = b == 1;
            }
            0x0112 => {
                let b = self.script().death_arrest_executed;
                self.cond(b);
            }
            0x0180 => {
                self.u8();
                self.vm.on_a_mission_flag = self.u16() as usize;
            }
            0x03A4 => {
                let name = self.text().to_ascii_lowercase();
                self.script().name = name.chars().take(8).collect();
            }
            0x0417 => {
                let n = self.int();
                self.vm.launch_mission(n as usize);
            }
            0x0459 => {
                let name = self.text().to_ascii_lowercase();
                let mut c = self.vm.active;
                while let Some(i) = c {
                    c = self.vm.scripts[i].next;
                    if self.vm.scripts[i].name == name {
                        self.vm.terminate(i);
                    }
                }
                if !self.vm.scripts[self.s].active {
                    return Some(Flow::Yield);
                }
            }
            0x05A9 | 0x05AA => {
                let a = self.var();
                let t = self.text();
                let mut b = [0u8; 8];
                for (k, c) in t.bytes().take(8).enumerate() {
                    b[k] = c;
                }
                self.write_bytes(a, &b);
            }
            0x0662 => {
                self.text();
            }
            0x06CF | 0x0914 => {
                self.int();
            }
            0x0701 => self.script().scene_skip = 0,
            0x0707 => {
                let l = self.int();
                self.script().scene_skip = l;
            }
            0x0871 | 0x0872 => {
                let n = if op == 0x0871 { 14 } else { 18 };
                if op == 0x0871 {
                    self.vm.switch_var = self.int();
                    let cases = self.int();
                    let _has_default = self.int();
                    self.vm.switch_default = self.int();
                    self.vm.switch_to_read = 2 * cases;
                    self.vm.switch_pairs.clear();
                }
                let vals: Vec<i32> = (0..n).map(|_| self.int()).collect();
                let take = (self.vm.switch_to_read.min(n as i32) / 2) as usize;
                for k in 0..take {
                    self.vm.switch_pairs.push((vals[2 * k], vals[2 * k + 1]));
                }
                self.vm.switch_to_read -= n as i32;
                if self.vm.switch_to_read <= 0 {
                    let t = &self.vm.switch_pairs;
                    let v = self.vm.switch_var;
                    let mut label = self.vm.switch_default;
                    if !t.is_empty() {
                        let (mut lo, mut hi) = (0usize, t.len() - 1);
                        let mut found = None;
                        while hi - lo > 1 {
                            let mid = (lo + hi) / 2;
                            if t[mid].0 == v {
                                found = Some(mid);
                                break;
                            }
                            if v > t[mid].0 { lo = mid } else { hi = mid }
                        }
                        let found = found.or(if t[hi].0 == v { Some(hi) } else if t[lo].0 == v { Some(lo) } else { None });
                        if let Some(f) = found {
                            label = t[f].1;
                        }
                    }
                    self.vm.switch_pairs.clear();
                    self.vm.switch_to_read = 0;
                    self.jump(label);
                }
            }
            0x08B4 => {
                let [v, bit] = self.ints::<2>();
                self.cond(v & (1 << (bit & 31)) != 0);
            }
            _ => return None,
        }
        Some(Flow::Continue)
    }
}
