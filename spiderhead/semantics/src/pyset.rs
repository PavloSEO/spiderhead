//! Set iteration-order emulation for str and int elements.
//! Iteration order follows a fixed-seed hash table layout, so output stays stable.
//!
//! Set iteration order leaks into cluster
//! names, Louvain neighbour order and JSON payloads, so byte-exact output requires
//! reproducing the table layout: siphash13 hashing with a zero seed,
//! open addressing with up-to-9-slot linear runs before perturb jumps, and
//! quadruple growth (`used*4`) checked after each insert.

/// SipHash-1-3 round used by `siphash13`.
fn sip_round(v: &mut [u64; 4]) {
    v[0] = v[0].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(13) ^ v[0];
    v[0] = v[0].rotate_left(32);
    v[2] = v[2].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(16) ^ v[2];
    v[0] = v[0].wrapping_add(v[3]);
    v[3] = v[3].rotate_left(21) ^ v[0];
    v[2] = v[2].wrapping_add(v[1]);
    v[1] = v[1].rotate_left(17) ^ v[2];
    v[2] = v[2].rotate_left(32);
}

/// `siphash13` keyed with `k0 = k1 = 0`
/// (a zero hash seed).
fn siphash13(data: &[u8]) -> u64 {
    let mut v = [
        0x736f6d6570736575u64,
        0x646f72616e646f6d,
        0x6c7967656e657261,
        0x7465646279746573,
    ];
    let (blocks, rem) = data.as_chunks::<8>();
    for block in blocks {
        let t = u64::from_le_bytes(*block);
        v[3] ^= t;
        sip_round(&mut v);
        v[0] ^= t;
    }
    let mut b = (data.len() as u64) << 56;
    for (i, byte) in rem.iter().enumerate() {
        b |= (*byte as u64) << (8 * i);
    }
    v[3] ^= b;
    sip_round(&mut v);
    v[0] ^= b;
    v[2] ^= 0xff;
    sip_round(&mut v);
    sip_round(&mut v);
    sip_round(&mut v);
    (v[0] ^ v[1]) ^ (v[2] ^ v[3])
}

/// String hash: siphash13 over the UCS-1/2/4 code-unit buffer
/// (little-endian code units), `-1` remapped to `-2`; empty string hashes to 0.
pub fn py_str_hash(s: &str) -> u64 {
    if s.is_empty() {
        return 0;
    }
    let max_cp = s.chars().map(|c| c as u32).max().unwrap_or(0);
    let mut buf = Vec::with_capacity(s.len() * 4);
    if max_cp <= 0xff {
        for c in s.chars() {
            buf.push(c as u8);
        }
    } else if max_cp <= 0xffff {
        for c in s.chars() {
            buf.extend_from_slice(&(c as u16).to_le_bytes());
        }
    } else {
        for c in s.chars() {
            buf.extend_from_slice(&(c as u32).to_le_bytes());
        }
    }
    let h = siphash13(&buf) as i64;
    if h == -1 {
        (-2i64) as u64
    } else {
        h as u64
    }
}

/// Integer hash for `|v| < 2^61` (negative handled by sign + mod,
/// `-1` remapped to `-2`), sufficient for the pipeline's integer sets.
pub fn py_int_hash(v: i64) -> u64 {
    const MOD: i64 = (1i64 << 61) - 1;
    let sign = if v < 0 { -1i64 } else { 1 };
    let mut h = v.unsigned_abs() as i64 % MOD;
    h *= sign;
    if h == -1 {
        h = -2;
    }
    h as u64
}

const MIN_SIZE: usize = 8;
const PERTURB_SHIFT: u32 = 5;
const LINEAR_PROBES: usize = 9;

/// Slot content: live key, deleted marker, or empty.
/// (`key == NULL` is Empty, `key == dummy` is Dummy, `hash == -1`).
#[derive(Clone)]
enum SlotKey<K> {
    Empty,
    Dummy,
    Key(K),
}

#[derive(Clone)]
struct Slot<K> {
    key: SlotKey<K>,
    hash: i64,
}

impl<K> Slot<K> {
    fn empty() -> Self {
        Self {
            key: SlotKey::Empty,
            hash: 0,
        }
    }
}

/// Ordered set with the fixed-seed iteration order.
/// Use wherever a set's iteration order reaches observable
/// output; membership-only sets can safely use this too.
#[derive(Clone)]
pub struct PySet<K: Clone + PartialEq> {
    table: Vec<Slot<K>>,
    used: usize,
    fill: usize,
    hasher: fn(&K) -> u64,
}

impl<K: Clone + PartialEq> PySet<K> {
    /// Create an empty set with an initial 8-slot table.
    pub fn new(hasher: fn(&K) -> u64) -> Self {
        Self {
            table: vec![Slot::empty(); MIN_SIZE],
            used: 0,
            fill: 0,
            hasher,
        }
    }

    fn mask(&self) -> usize {
        self.table.len() - 1
    }

    /// Number of elements.
    pub fn len(&self) -> usize {
        self.used
    }

    /// Whether the set is empty.
    pub fn is_empty(&self) -> bool {
        self.used == 0
    }

    /// Probe: linear run of up to `LINEAR_PROBES + 1` slots
    /// from `hash & mask`, then `i = (i*5 + 1 + perturb) & mask` jumps with
    /// `perturb` shifted before every jump. Returns the hit index or the
    /// first slot where a lookup would stop (empty or dummy-reusable).
    fn lookkey(&self, key: &K, hash: i64) -> LookResult {
        let mask = self.mask();
        let mut i = (hash as u64 as usize) & mask;
        let mut perturb = hash as u64;
        loop {
            let mut idx = i;
            let mut probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            let mut freeslot: Option<usize> = None;
            loop {
                let entry = &self.table[idx];
                match &entry.key {
                    SlotKey::Empty => {
                        return LookResult {
                            idx,
                            found: false,
                            freeslot,
                        };
                    }
                    SlotKey::Dummy => {
                        freeslot = Some(idx);
                    }
                    SlotKey::Key(k) => {
                        if entry.hash == hash && k == key {
                            return LookResult {
                                idx,
                                found: true,
                                freeslot,
                            };
                        }
                    }
                }
                idx += 1;
                if probes == 0 {
                    break;
                }
                probes -= 1;
            }
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + 1 + perturb as usize) & mask;
        }
    }

    /// Membership probe.
    pub fn contains(&self, key: &K) -> bool {
        let hash = (self.hasher)(key) as i64;
        self.lookkey(key, hash).found
    }

    /// Insert a key known to be absent into `table`.
    /// Checks slot `i`, then up to `LINEAR_PROBES` following slots, then jumps.
    fn insert_clean(table: &mut [Slot<K>], key: K, hash: i64) {
        let mask = table.len() - 1;
        let mut i = (hash as u64 as usize) & mask;
        let mut perturb = hash as u64;
        loop {
            let mut idx = i;
            if matches!(table[idx].key, SlotKey::Empty) {
                table[idx] = Slot {
                    key: SlotKey::Key(key),
                    hash,
                };
                return;
            }
            if i + LINEAR_PROBES <= mask {
                for _ in 0..LINEAR_PROBES {
                    idx += 1;
                    if matches!(table[idx].key, SlotKey::Empty) {
                        table[idx] = Slot {
                            key: SlotKey::Key(key),
                            hash,
                        };
                        return;
                    }
                }
            }
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + 1 + perturb as usize) & mask;
        }
    }

    /// Resize to the smallest power of two strictly above `minused`.
    fn resize(&mut self, minused: usize) {
        let mut newsize = MIN_SIZE;
        while newsize <= minused && newsize > 0 {
            newsize <<= 1;
        }
        let mut table = vec![Slot::empty(); newsize];
        for entry in &self.table {
            if let SlotKey::Key(k) = &entry.key {
                Self::insert_clean(&mut table, k.clone(), entry.hash);
            }
        }
        self.table = table;
        self.fill = self.used;
    }

    /// Deduplicating insert. Grow check
    /// (`fill*5 >= mask*3` → resize to `used*4`, or `used*2` above 50 000)
    /// runs after the element lands.
    pub fn add(&mut self, key: K) {
        let hash = (self.hasher)(&key) as i64;
        let mask = self.mask();
        let mut i = (hash as u64 as usize) & mask;
        let mut perturb = hash as u64;
        let mut freeslot: Option<usize> = None;
        let landing = 'outer: loop {
            let mut idx = i;
            let mut probes = if i + LINEAR_PROBES <= mask {
                LINEAR_PROBES
            } else {
                0
            };
            let free = loop {
                let entry = &self.table[idx];
                match &entry.key {
                    SlotKey::Empty => break Some(idx),
                    SlotKey::Dummy => freeslot = Some(idx),
                    SlotKey::Key(k) => {
                        if entry.hash == hash && k == &key {
                            return; // already present
                        }
                    }
                }
                idx += 1;
                if probes == 0 {
                    break None;
                }
                probes -= 1;
            };
            if let Some(slot) = free {
                break 'outer slot;
            }
            perturb >>= PERTURB_SHIFT;
            i = (i * 5 + 1 + perturb as usize) & mask;
        };
        let slot = freeslot.unwrap_or(landing);
        if freeslot.is_none() {
            self.fill += 1;
        }
        self.table[slot] = Slot {
            key: SlotKey::Key(key),
            hash,
        };
        self.used += 1;
        if self.fill * 5 >= mask * 3 {
            let minused = if self.used > 50000 {
                self.used * 2
            } else {
                self.used * 4
            };
            self.resize(minused);
        }
    }

    /// Remove a key by marking its slot as a dummy, so the iteration order of
    /// the survivors is unchanged.
    pub fn discard(&mut self, key: &K) {
        let hash = (self.hasher)(key) as i64;
        let res = self.lookkey(key, hash);
        if res.found {
            self.table[res.idx] = Slot {
                key: SlotKey::Dummy,
                hash: -1,
            };
            self.used -= 1;
        }
    }

    /// Yield live keys in slot order.
    /// This is the iteration order of the set.
    pub fn iter(&self) -> impl Iterator<Item = K> + '_ {
        self.table.iter().filter_map(|s| match &s.key {
            SlotKey::Key(k) => Some(k.clone()),
            _ => None,
        })
    }

    /// Collect into a `Vec` in iteration order.
    pub fn to_vec(&self) -> Vec<K> {
        self.iter().collect()
    }

    /// Merge another set (`update` / copy path):
    /// presize once, then insert `other`'s entries in `other`'s slot order
    /// via `insert_clean` when the target table is empty, else `add_entry`.
    pub fn update(&mut self, other: &PySet<K>) {
        if other.used == 0 {
            return;
        }
        if (self.fill + other.used) * 5 >= self.mask() * 3 {
            self.resize((self.used + other.used) * 2);
        }
        if self.fill == 0 && self.mask() == other.mask() && other.fill == other.used {
            self.table = other.table.clone();
            self.used = other.used;
            self.fill = other.fill;
            return;
        }
        if self.fill == 0 {
            self.fill = other.used;
            self.used = other.used;
            let mut table = std::mem::take(&mut self.table);
            for entry in &other.table {
                if let SlotKey::Key(k) = &entry.key {
                    Self::insert_clean(&mut table, k.clone(), entry.hash);
                }
            }
            self.table = table;
            return;
        }
        for k in other.iter() {
            self.add(k);
        }
    }

    /// Copy into a fresh empty set.
    pub fn copy(&self) -> PySet<K> {
        let mut out = PySet::new(self.hasher);
        out.update(self);
        out
    }

    /// Intersection: iterates the smaller of the two sets and checks membership in the larger one.
    pub fn intersection(&self, other: &PySet<K>) -> PySet<K> {
        let mut out = PySet::new(self.hasher);
        if other.len() <= self.len() {
            for k in other.iter() {
                if self.contains(&k) {
                    out.add(k);
                }
            }
        } else {
            for k in self.iter() {
                if other.contains(&k) {
                    out.add(k);
                }
            }
        }
        out
    }

    /// Difference: when `len(s) >> 2 > len(t)` the set is copied and
    /// `t`'s elements are discarded in place (the result keeps `s`'s layout);
    /// otherwise it builds a fresh set adding survivors in `s` order.
    pub fn difference(&self, other: &PySet<K>) -> PySet<K> {
        if (self.len() >> 2) > other.len() {
            let mut out = self.copy();
            for k in other.iter() {
                out.discard(&k);
            }
            return out;
        }
        let mut out = PySet::new(self.hasher);
        for k in self.iter() {
            if !other.contains(&k) {
                out.add(k);
            }
        }
        out
    }

    /// Union: copy `self`, then merge `other`.
    pub fn union(&self, other: &PySet<K>) -> PySet<K> {
        let mut out = self.copy();
        out.update(other);
        out
    }
}

struct LookResult {
    idx: usize,
    found: bool,
    /// Reports the first
    /// free or dummy slot; callers currently only need `idx`/`found`.
    #[allow(dead_code)]
    freeslot: Option<usize>,
}

/// String set (`hash(str)` semantics).
pub type PyStrSet = PySet<String>;

/// Build a string set by adding items in order.
pub fn py_str_set<I: IntoIterator<Item = String>>(items: I) -> PyStrSet {
    let mut set = PySet::new(|s: &String| py_str_hash(s));
    for item in items {
        set.add(item);
    }
    set
}

/// Build an integer set by adding items in order.
pub fn py_int_set<I: IntoIterator<Item = i64>>(items: I) -> PySet<i64> {
    let mut set = PySet::new(|v: &i64| py_int_hash(*v));
    for item in items {
        set.add(item);
    }
    set
}

#[cfg(test)]
mod tests {
    use super::*;

    // Expected orders recorded with a zero hash seed.
    #[test]
    fn str_set_order_matches_cpython() {
        let set = py_str_set(
            [
                "a", "b", "c", "абв", "hello", "world", "seo", "audit", "x", "",
            ]
            .iter()
            .map(|s| s.to_string()),
        );
        assert_eq!(
            set.to_vec(),
            ["", "x", "seo", "audit", "b", "c", "a", "world", "абв", "hello"]
        );
    }

    #[test]
    fn big_set_order_matches_cpython() {
        let expected: Vec<String> = [
            "w4", "w1", "w16", "w11", "w48", "w31", "w43", "w3", "w24", "w27", "w6", "w38", "w7",
            "w29", "w23", "w46", "w26", "w30", "w40", "w20", "w25", "w36", "w17", "w21", "w34",
            "w37", "w39", "w15", "w0", "w13", "w14", "w32", "w49", "w33", "w41", "w8", "w44",
            "w22", "w9", "w12", "w42", "w18", "w19", "w45", "w2", "w47", "w5", "w10", "w28", "w35",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        let set = py_str_set((0..50).map(|i| format!("w{i}")));
        assert_eq!(set.to_vec(), expected);
    }

    #[test]
    fn int_set_order_matches_cpython() {
        let set = py_int_set(0..20);
        assert_eq!(set.to_vec(), (0..20).collect::<Vec<_>>());
    }

    #[test]
    fn str_hash_golden() {
        // Zero-seed hashes of 'a', 'b', 'абв' and ''.
        assert_eq!(py_str_hash("a") as i64, 4644417185603328019);
        assert_eq!(py_str_hash("b") as i64, -8420940591063670578);
        assert_eq!(py_str_hash("абв") as i64, -8852227582061620260);
        assert_eq!(py_str_hash(""), 0);
    }
}
