//! MNDX root file (name -> content key), as used by StarCraft II / Heroes.
//!
//! Ported from CascLib's `CascRootFile_MNDX.cpp` (Ladislav Zezula, MIT). The root
//! holds three "MAR" name databases (package names, package-stripped names, full
//! names) plus a table of CKey entries. Each MAR is a LOUDS-encoded trie whose edge
//! labels are single chars or fragments, the fragments themselves possibly stored in
//! a nested (child) trie. Instead of porting CascLib's search-by-mask machinery we
//! walk the package and stripped-name tries once and build `package/stripped` names,
//! like CascLib's `LoadFileNames` (the full-name MAR is parsed but not needed). Unlike
//! CascLib we keep entries whose ckey is missing from the encoding table.
//!
//! Stored names are lowercase with `/` separators, e.g.
//! `mods/liberty.sc2mod/base.sc2data/gamedata/unitdata.xml`; [`Root::get`] normalizes
//! its argument the same way, so lookups are case- and slash-insensitive.

use anyhow::{Context, Result, ensure};

use crate::casc::Key;

/// Flag on the last CKey entry of a same-name group (the low 24 bits are the package index).
const LAST_IN_GROUP: u32 = 0x8000_0000;

pub struct Root {
    /// `(name, ckey)` sorted by name.
    files: Vec<(String, Key)>,
}

impl Root {
    pub fn parse(d: &[u8]) -> Result<Self> {
        let mut r = Rd(d);
        ensure!(r.take(4)? == b"MNDX", "not an MNDX root");
        let (header_ver, format_ver) = (r.u32()?, r.u32()?);
        ensure!(header_ver <= 2 && (1..=2).contains(&format_ver), "MNDX v{header_ver}/{format_ver} unsupported");
        if header_ver == 2 {
            r.take(8)?; // two build numbers
        }
        let h = (0..7).map(|_| r.u32().map(|v| v as usize)).collect::<Result<Vec<_>>>()?;
        let &[mar_off, mar_count, mar_size, ck_off, ck_count, name_count, ck_size] = &h[..] else { unreachable!() };
        ensure!(mar_count == 3 && mar_size == 20 && ck_size == 24, "unexpected MNDX layout");

        let mars = (0..mar_count)
            .map(|i| {
                let info = sub(d, mar_off + i * mar_size, 20)?;
                let (size, off) = (le32(info, 4) as usize, le32(info, 12) as usize);
                let mut r = Rd(sub(d, off, size).context("MAR data")?);
                ensure!(r.take(4)? == b"MAR\0", "bad MAR signature");
                Db::load(&mut r).with_context(|| format!("MAR {i}"))
            })
            .collect::<Result<Vec<_>>>()?;
        ensure!(mars[1].file_idx.valid as usize == name_count, "stripped-name count mismatch");

        // CKey entries: { flags/package u32, ckey [16], content size u32 }, grouped by name.
        let ck = sub(d, ck_off, ck_count * ck_size).context("CKey entries")?;
        let entry = |i: usize| (le32(ck, i * 24), <Key>::try_from(&ck[i * 24 + 4..i * 24 + 20]).unwrap());
        let mut group_start = vec![0];
        group_start.extend((0..ck_count).filter(|&i| entry(i).0 & LAST_IN_GROUP != 0).map(|i| i + 1));
        ensure!(group_start.len() > name_count, "CKey groups ({}) < names ({name_count})", group_start.len() - 1);

        let mut packages: Vec<Option<Vec<u8>>> = Vec::new();
        mars[0].walk(&mut |idx, name| {
            let idx = idx as usize;
            if packages.len() <= idx {
                packages.resize(idx + 1, None);
            }
            packages[idx] = Some(name.to_vec());
        });

        let mut files = Vec::with_capacity(ck_count);
        mars[1].walk(&mut |idx, name| {
            let Some(&start) = group_start.get(idx as usize).filter(|_| (idx as usize) < name_count) else { return };
            for i in start..ck_count {
                let (flags, ckey) = entry(i);
                if let Some(Some(pkg)) = packages.get((flags & 0xFF_FFFF) as usize) {
                    files.push((normalize_bytes([pkg, &b"/"[..], name].concat()), ckey));
                }
                if flags & LAST_IN_GROUP != 0 {
                    break;
                }
            }
        });
        // Nested packages (e.g. a `.sc2map` inside a campaign) list some files under several
        // package/name splits that join to the same full name (and the same ckey).
        files.sort_unstable_by(|a, b| a.0.cmp(&b.0));
        files.dedup_by(|a, b| a.0 == b.0);
        Ok(Self { files })
    }

    /// Content key of a file; `name` is matched case-insensitively, `\` == `/`.
    pub fn get(&self, name: &str) -> Option<Key> {
        let name = normalize(name);
        self.files.binary_search_by(|(n, _)| n.as_str().cmp(&name)).ok().map(|i| self.files[i].1)
    }

    /// All `(name, ckey)` pairs, sorted by name.
    pub fn names(&self) -> impl Iterator<Item = (&str, Key)> {
        self.files.iter().map(|(n, k)| (n.as_str(), *k))
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

fn normalize(name: &str) -> String {
    normalize_bytes(name.trim_start_matches(['/', '\\']).as_bytes().to_vec())
}

fn normalize_bytes(mut b: Vec<u8>) -> String {
    for c in &mut b {
        *c = if *c == b'\\' { b'/' } else { c.to_ascii_lowercase() };
    }
    String::from_utf8(b).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).into_owned())
}

fn le32(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(d[at..at + 4].try_into().unwrap())
}

fn sub(d: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    d.get(at..at.checked_add(len).context("overflow")?).context("root file truncated")
}

/// Little-endian byte stream (CascLib's `TByteStream`).
struct Rd<'a>(&'a [u8]);

impl<'a> Rd<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        ensure!(n <= self.0.len(), "MAR data truncated");
        let (a, b) = self.0.split_at(n);
        self.0 = b;
        Ok(a)
    }

    fn u32(&mut self) -> Result<u32> {
        Ok(le32(self.take(4)?, 0))
    }

    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// `TGenericArray`: u64 byte length, items, padding to 8 bytes.
    fn array(&mut self, item: usize) -> Result<&'a [u8]> {
        let len = self.u64()?;
        ensure!(len <= u32::MAX as u64 && (len as usize).is_multiple_of(item), "bad MAR array length {len}");
        let data = self.take(len as usize)?;
        self.take((8 - len as usize % 8) % 8)?;
        Ok(data)
    }

    fn u32s(&mut self) -> Result<Vec<u32>> {
        Ok(self.array(4)?.as_chunks::<4>().0.iter().map(|c| u32::from_le_bytes(*c)).collect())
    }
}

/// `TSparseArray`: a bit vector with rank/select. CascLib uses the stored per-0x200-bit
/// base values with packed sub-counts; we rebuild a plain per-word rank table instead
/// (checked against the stored base values) and use the stored select samples.
struct Bits {
    words: Vec<u32>,
    /// Set bits before word `i` (one extra trailing entry).
    rank: Vec<u32>,
    /// Position of every 0x200-th clear / set bit (`IndexToItem0` / `IndexToItem1`).
    samples: [Vec<u32>; 2],
    total: u32,
    valid: u32,
}

impl Bits {
    fn load(r: &mut Rd) -> Result<Self> {
        let words = r.u32s()?;
        let (total, valid) = (r.u32()?, r.u32()?);
        ensure!(valid <= total, "sparse array: valid > total");
        let base = r.array(12)?; // BASEVALS: rank every 0x200 bits + 7 packed sub-counts (64 bits)
        let samples = [r.u32s()?, r.u32s()?];

        let mut rank = Vec::with_capacity(words.len() + 1);
        let mut acc = 0;
        rank.push(0);
        for w in &words {
            acc += w.count_ones();
            rank.push(acc);
        }
        for (g, b) in base.as_chunks::<12>().0.iter().enumerate() {
            if let Some(&expected) = rank.get(g * 16) {
                ensure!(le32(b, 0) == expected, "sparse array rank mismatch at group {g}");
            }
        }
        Ok(Self { words, rank, samples, total, valid })
    }

    fn get(&self, i: u32) -> bool {
        self.words.get(i as usize >> 5).is_some_and(|w| w >> (i & 31) & 1 != 0)
    }

    /// Set bits before position `i` (`GetItemValueAt`).
    fn rank1(&self, i: u32) -> u32 {
        let w = i as usize >> 5;
        self.rank[w] + (self.words.get(w).copied().unwrap_or(0) & ((1u32 << (i & 31)) - 1)).count_ones()
    }

    /// Position of the `k`-th (0-based) set bit, or clear bit if `!ones` (`GetItem1` / `GetItem0`).
    fn select(&self, k: u32, ones: bool) -> u32 {
        let before = |w: usize| if ones { self.rank[w] } else { (w as u32 * 32) - self.rank[w] };
        // Last word whose preceding count is <= k, between the neighbouring samples.
        let (s, g) = (&self.samples[ones as usize], k as usize >> 9);
        let mut lo = s.get(g).map_or(0, |&p| p as usize >> 5);
        let mut hi = s.get(g + 1).map_or(usize::MAX, |&p| (p as usize >> 5) + 1).min(self.words.len());
        while lo + 1 < hi {
            let mid = (lo + hi) / 2;
            if before(mid) <= k { lo = mid } else { hi = mid }
        }
        let mut x = if ones { self.words[lo] } else { !self.words[lo] };
        for _ in 0..k - before(lo) {
            x &= x - 1;
        }
        lo as u32 * 32 + x.trailing_zeros()
    }
}

/// `TFileNameDatabase`: one LOUDS trie.
struct Db {
    /// LOUDS topology: for each node, a 1 per child then a 0.
    collision: Bits,
    /// Nodes that terminate a name; rank gives the name index.
    file_idx: Bits,
    /// Nodes whose label is a fragment (offset = hi bits << 8 | lo byte) rather than one char.
    hi_idx: Bits,
    lo: Vec<u8>,
    hi_words: Vec<u32>,
    hi_bits: u32,
    hi_mask: u32,
    /// NUL-terminated fragments (when there is no child trie), or ended by `marks`.
    frags: Vec<u8>,
    marks: Bits,
    child: Option<Box<Db>>,
    /// `HASH_ENTRY { node, next, fragment/char }`; a lookup cache over the trie.
    hash: Vec<[u32; 3]>,
    /// Nodes `<=` this are roots of child-trie fragment walks (`field_214`).
    leaf_limit: u32,
}

impl Db {
    fn load(r: &mut Rd) -> Result<Self> {
        let collision = Bits::load(r)?;
        let file_idx = Bits::load(r)?;
        let hi_idx = Bits::load(r)?;
        let lo = r.array(1)?.to_vec();
        let hi_words = r.u32s()?;
        let (hi_bits, hi_mask) = (r.u32()?, r.u32()?);
        ensure!(hi_bits <= 32, "bad hi-bits width");
        r.u64()?; // entry count
        let frags = r.array(1)?.to_vec();
        let marks = Bits::load(r)?;
        let child = if hi_idx.valid != 0 && frags.is_empty() { Some(Box::new(Db::load(r)?)) } else { None };
        let hash: Vec<[u32; 3]> = r.array(12)?.as_chunks::<12>().0.iter().map(|c| [le32(c, 0), le32(c, 4), le32(c, 8)]).collect();
        ensure!(hash.len().is_power_of_two(), "hash table size not a power of two");
        let leaf_limit = r.u32()?;
        r.u32()?; // bit mask (search tuning flags)
        Ok(Self { collision, file_idx, hi_idx, lo, hi_words, hi_bits, hi_mask, frags, marks, child, hash, leaf_limit })
    }

    /// `TBitEntryArray::GetItem`
    fn hi(&self, i: u32) -> u32 {
        let bit = i as u64 * self.hi_bits as u64;
        let (w, s) = ((bit >> 5) as usize, (bit & 31) as u32);
        let mut v = self.hi_words[w] >> s;
        if s + self.hi_bits > 32 {
            v |= self.hi_words[w + 1] << (32 - s);
        }
        v & self.hi_mask
    }

    /// `GetPathFragmentOffset1`
    fn frag_offset(&self, node: u32) -> u32 {
        self.hi(self.hi_idx.rank1(node)) << 8 | self.lo[node as usize] as u32
    }

    /// Appends the label of trie node `node`.
    fn push_label(&self, node: u32, out: &mut Vec<u8>) {
        if self.hi_idx.get(node) {
            self.copy_fragment(self.frag_offset(node), out);
        } else {
            out.push(self.lo[node as usize]);
        }
    }

    fn copy_fragment(&self, off: u32, out: &mut Vec<u8>) {
        match &self.child {
            Some(c) => c.copy_by_index(off, out),
            None if self.marks.total == 0 => out.extend(self.frags[off as usize..].iter().take_while(|&&b| b != 0)),
            None => {
                // `TPathFragmentTable::CopyPathFragment` (marked variant; unused by SC2).
                let mut i = off;
                while !self.marks.get(i) {
                    out.push(self.frags[i as usize]);
                    i += 1;
                }
            }
        }
    }

    /// `CopyPathFragmentByIndex`: emits a fragment stored in this (child) trie by walking
    /// from node `ti` up towards the root.
    fn copy_by_index(&self, mut ti: u32, out: &mut Vec<u8>) {
        loop {
            let [node, next, frag] = self.hash[ti as usize & (self.hash.len() - 1)];
            if ti == next {
                if frag & 0xFFFF_FF00 == 0xFFFF_FF00 {
                    out.push(frag as u8);
                } else {
                    self.copy_fragment(frag, out);
                }
                ti = node;
                if ti == 0 {
                    return;
                }
            } else {
                self.push_label(ti, out);
                if ti <= self.leaf_limit {
                    return;
                }
                ti = self.collision.select(ti, true).wrapping_sub(ti).wrapping_sub(1);
            }
        }
    }

    /// Calls `f(name_index, name)` for every name in the trie, depth first
    /// (the order of CascLib's `DoSearch` with an empty mask).
    fn walk(&self, f: &mut dyn FnMut(u32, &[u8])) {
        self.walk_node(0, &mut Vec::with_capacity(256), f);
    }

    fn walk_node(&self, node: u32, buf: &mut Vec<u8>, f: &mut dyn FnMut(u32, &[u8])) {
        if self.file_idx.get(node) {
            f(self.file_idx.rank1(node), buf);
        }
        // Children of `node` are the run of 1s after its 0-th marker; child ids follow on.
        let pos = self.collision.select(node, false) + 1;
        let first = pos - node - 1;
        let mut j = 0;
        while self.collision.get(pos + j) {
            let len = buf.len();
            self.push_label(first + j, buf);
            self.walk_node(first + j, buf, f);
            buf.truncate(len);
            j += 1;
        }
    }
}
