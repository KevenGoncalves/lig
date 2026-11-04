//! User-provided specs: `~/.config/lig/specs/<command>.json` or `.toml`.
//! They take precedence over the built-in ones.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Component, Path, PathBuf};

use crate::spec::{self, Subcommand};

pub fn user_dir() -> PathBuf {
    crate::config::config_dir().join("specs")
}

#[derive(Clone)]
pub struct Store {
    dirs: Vec<PathBuf>,
}

impl Default for Store {
    fn default() -> Self {
        Store { dirs: vec![user_dir()] }
    }
}

pub fn safe(name: &str) -> bool {
    !name.is_empty() && Path::new(name).components().all(|c| matches!(c, Component::Normal(_)))
}

impl Store {
    #[cfg(test)]
    pub fn with_dirs(dirs: Vec<PathBuf>) -> Self {
        Store { dirs }
    }

    pub fn load(&self, name: &str) -> Option<Result<Subcommand, String>> {
        if !safe(name) {
            return None;
        }
        for d in &self.dirs {
            for ext in ["json", "toml"] {
                let path = d.join(format!("{name}.{ext}"));
                let Ok(text) = fs::read_to_string(&path) else { continue };
                let value: Result<serde_json::Value, String> = if ext == "json" {
                    serde_json::from_str(&text).map_err(|e| e.to_string())
                } else {
                    toml::from_str(&text).map_err(|e| e.to_string())
                };
                return Some(value.map(|v| spec::from_value(&v)).map_err(|e| format!("{}: {e}", path.display())));
            }
        }
        None
    }

    pub fn commands(&self) -> BTreeSet<String> {
        let mut out = BTreeSet::new();
        for d in &self.dirs {
            for e in fs::read_dir(d).into_iter().flatten().flatten() {
                let p = e.path();
                if let (Some(stem), Some(ext)) = (p.file_stem(), p.extension())
                    && (ext == "json" || ext == "toml")
                {
                    out.insert(stem.to_string_lossy().into_owned());
                }
            }
        }
        out
    }
}
