//! Spec archives embedded by `build.rs`, inflated one spec at a time on first use.

use std::collections::HashMap;
use std::io::Read;
use std::sync::OnceLock;

use crate::spec::{self, Subcommand};

pub struct Archive {
    data: &'static [u8],
    index: OnceLock<HashMap<&'static str, (usize, usize)>>,
}

/// `specs/lig/`: the hand-written specs.
pub static LIG: Archive = Archive::new(include_bytes!(concat!(env!("OUT_DIR"), "/lig_specs.bin")));
/// `specs/fig/`: converted from withfig/autocomplete.
pub static FIG: Archive = Archive::new(include_bytes!(concat!(env!("OUT_DIR"), "/fig_specs.bin")));

impl Archive {
    const fn new(data: &'static [u8]) -> Archive {
        Archive { data, index: OnceLock::new() }
    }

    fn index(&self) -> &HashMap<&'static str, (usize, usize)> {
        self.index.get_or_init(|| {
            let a = self.data;
            let mut map = HashMap::new();
            if !a.starts_with(b"LIGSPEC1") {
                return map;
            }
            let u32_at = |i: usize| u32::from_le_bytes(a[i..i + 4].try_into().unwrap()) as usize;
            let count = u32_at(8);
            let mut i = 12;
            let mut entries = Vec::with_capacity(count);
            for _ in 0..count {
                let len = u16::from_le_bytes([a[i], a[i + 1]]) as usize;
                let name = std::str::from_utf8(&a[i + 2..i + 2 + len]).unwrap_or("");
                i += 2 + len;
                entries.push((name, u32_at(i), u32_at(i + 4)));
                i += 8;
            }
            for (name, off, len) in entries {
                map.insert(name, (i + off, len));
            }
            map
        })
    }

    /// The spec file `name` (`git`, `aws/s3`, …) as JSON.
    pub fn json(&self, name: &str) -> Option<serde_json::Value> {
        let &(off, len) = self.index().get(name)?;
        let mut json = Vec::new();
        flate2::read::DeflateDecoder::new(&self.data[off..off + len]).read_to_end(&mut json).ok()?;
        serde_json::from_slice(&json).ok()
    }

    pub fn load(&self, name: &str) -> Option<Subcommand> {
        self.json(name).map(|v| spec::from_value(&v))
    }

    /// Top-level command names (no `aws/s3`-style sub-specs).
    pub fn names(&self) -> impl Iterator<Item = &'static str> {
        self.index().keys().copied().filter(|n| !n.contains('/'))
    }

    pub fn count(&self) -> usize {
        self.index().len()
    }
}
