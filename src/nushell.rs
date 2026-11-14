//! nu's own commands complete from nu's signatures (shell/nu-specs.nu), not from the
//! coreutils specs that share their names (`ls`, `rm`, `open`…). Read once per nu binary.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::spec::Subcommand;

const SCRIPT: &str = include_str!("../shell/nu-specs.nu");

/// Specs by command name, for the shell lig runs.
pub type ShellSpecs = Arc<HashMap<String, Arc<Subcommand>>>;

/// The specs of the nu at `nu`, cached until that binary changes.
pub fn specs(nu: &Path) -> Option<ShellSpecs> {
    let meta = std::fs::metadata(nu).ok()?;
    let mut stamp = std::collections::hash_map::DefaultHasher::new();
    (nu, meta.len(), meta.modified().ok()).hash(&mut stamp);
    let cache = crate::help::cache_dir().join(format!("nu-{:016x}.json", stamp.finish()));
    // A cache that doesn't parse (cut short, say) is read from nu again.
    let cached = std::fs::read_to_string(&cache).ok().and_then(|text| serde_json::from_str::<Vec<Value>>(&text).ok());
    let flat = match cached {
        Some(flat) => flat,
        None => {
            let argv = [nu.to_string_lossy().into_owned(), "--no-config-file".into(), "-c".into(), SCRIPT.into()];
            let out =
                crate::exec::run(crate::exec::Cmd::Argv(&argv), Path::new("/"), &[], Duration::from_secs(10), None);
            let flat = (out.status == 0).then(|| serde_json::from_str::<Vec<Value>>(&out.stdout).ok()).flatten();
            let Some(flat) = flat else {
                crate::log(&format!("nu specs: exit {}: {}", out.status, out.stderr.trim()));
                return None;
            };
            // Written whole, then renamed: another session never reads half of it.
            let tmp = cache.with_extension(format!("{}.tmp", std::process::id()));
            let _ = std::fs::create_dir_all(crate::help::cache_dir());
            if std::fs::write(&tmp, &out.stdout).is_ok() {
                let _ = std::fs::rename(&tmp, &cache);
            }
            flat
        }
    };
    let specs: HashMap<String, Arc<Subcommand>> = nest(flat)
        .iter()
        .map(|v| (v["name"].as_str().unwrap_or_default().to_string(), Arc::new(crate::spec::from_value(v))))
        .collect();
    crate::log(&format!("nu specs: {} commands", specs.len()));
    Some(Arc::new(specs))
}

/// `str replace` becomes the subcommand `replace` of `str` (made bare if nu has none).
fn nest(mut flat: Vec<Value>) -> Vec<Value> {
    let name = |v: &Value| v["name"].as_str().unwrap_or_default().to_string();
    flat.sort_by_key(|v| name(v).split(' ').count());
    let mut roots: Vec<Value> = vec![];
    for mut spec in flat {
        let full = name(&spec);
        let mut words: Vec<&str> = full.split(' ').collect();
        let Some(last) = words.pop() else { continue };
        spec["name"] = last.into();
        let mut list = &mut roots;
        for word in words {
            let i = match list.iter().position(|v| v["name"] == word) {
                Some(i) => i,
                None => {
                    list.push(json!({ "name": word }));
                    list.len() - 1
                }
            };
            let parent = list[i].as_object_mut().expect("specs are objects");
            list = parent.entry("subcommands").or_insert_with(|| json!([])).as_array_mut().expect("an array");
        }
        list.push(spec);
    }
    roots
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subcommands_nest_under_their_parents() {
        let flat = vec![
            json!({ "name": "str replace", "description": "Replace" }),
            json!({ "name": "str", "description": "Strings" }),
            json!({ "name": "ls" }),
            json!({ "name": "polars into-df" }),
        ];
        let roots = nest(flat);
        let names: Vec<&str> = roots.iter().map(|v| v["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["str", "ls", "polars"]);
        assert_eq!(roots[0]["subcommands"][0]["name"], "replace");
        assert_eq!(roots[0]["description"], "Strings");
        assert_eq!(roots[2]["subcommands"][0]["name"], "into-df");
    }
}
