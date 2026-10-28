//! Packs `specs/lig/` and `specs/fig/` into `lig_specs.bin` / `fig_specs.bin`, embedded in the binary.
//! Format (little-endian):
//! `"LIGSPEC1" | u32 count | count × (u16 name_len, name, u32 offset, u32 len) | deflate blobs`.
//! Each spec is deflated separately so lig only inflates the ones it uses.

use std::io::Write;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "json") {
            out.push(p);
        }
    }
}

fn pack(root: &Path, dest: &Path) {
    println!("cargo:rerun-if-changed={}", root.display());
    let mut files = vec![];
    walk(root, &mut files);
    files.sort();
    let mut index = Vec::new();
    let mut blobs = Vec::new();
    for f in &files {
        let name = f.strip_prefix(root).unwrap().with_extension("").to_string_lossy().replace('\\', "/");
        let data = std::fs::read(f).unwrap();
        // Fail the build on broken JSON rather than at completion time.
        if let Err(e) = serde_json::from_slice::<serde_json::Value>(&data) {
            panic!("{}: {e}", f.display());
        }
        let mut enc = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::best());
        enc.write_all(&data).unwrap();
        let z = enc.finish().unwrap();
        index.push((name, blobs.len() as u32, z.len() as u32));
        blobs.extend_from_slice(&z);
    }
    let mut out = b"LIGSPEC1".to_vec();
    out.extend((index.len() as u32).to_le_bytes());
    for (name, off, len) in &index {
        out.extend((name.len() as u16).to_le_bytes());
        out.extend(name.as_bytes());
        out.extend(off.to_le_bytes());
        out.extend(len.to_le_bytes());
    }
    out.extend(blobs);
    std::fs::write(dest, out).unwrap();
}

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    pack(Path::new("specs/lig"), &out.join("lig_specs.bin"));
    pack(Path::new("specs/fig"), &out.join("fig_specs.bin"));
}
