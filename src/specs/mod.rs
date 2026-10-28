//! Built-in JSON specs: hand-written in `specs/lig/` (generators by id, see `gens.rs`) and
//! ~1500 converted from withfig/autocomplete in `specs/fig/`. When both exist they are merged,
//! the lig spec winning; user specs in `~/.config/lig/specs/` override both.

pub mod archive;
pub mod gens;

use crate::spec::Subcommand;

pub use archive::{FIG, LIG};

/// Commands with a hand-written spec.
pub fn lig_names() -> impl Iterator<Item = &'static str> {
    LIG.names()
}

/// The spec for `name`: hand-written merged with the converted one (unless
/// the hand-written one says `skipFigSpec`).
pub fn builtin(name: &str) -> Option<Subcommand> {
    // `.` is `source`.
    let name = if name == "." { "source" } else { name };
    match (LIG.load(name), FIG.load(name)) {
        (Some(b), Some(f)) if !b.skip_fig_spec => Some(merge(&b, &f)),
        (b, f) => b.or(f),
    }
}

/// Merge `extra` into `base`: `base` wins, `extra` adds what's missing.
pub fn merge(base: &Subcommand, extra: &Subcommand) -> Subcommand {
    let mut out = base.clone();
    if out.meta.description.is_none() {
        out.meta.description = extra.meta.description.clone();
    }
    for o in &extra.options {
        if !out.options.iter().any(|b| b.meta.names.iter().any(|n| o.meta.names.contains(n))) {
            out.options.push(o.clone());
        }
    }
    for s in &extra.subcommands {
        match out.subcommands.iter().position(|b| b.meta.names.iter().any(|n| s.meta.names.contains(n))) {
            Some(i) => out.subcommands[i] = std::sync::Arc::new(merge(&out.subcommands[i], s)),
            None => out.subcommands.push(s.clone()),
        }
    }
    if out.args.is_empty() {
        out.args = extra.args.clone();
    } else {
        // Same position: keep ours, add their values (e.g. Fig's popular pip packages).
        for (i, ea) in extra.args.iter().enumerate().take(out.args.len()) {
            out.args[i] = std::sync::Arc::new(merge_arg(&out.args[i], ea));
        }
    }
    if out.additional_suggestions.is_empty() {
        out.additional_suggestions = extra.additional_suggestions.clone();
    }
    if out.load_spec.is_none() && base.subcommands.is_empty() && base.args.is_empty() {
        out.load_spec = extra.load_spec.clone();
    }
    out
}

/// `base`'s argument plus the suggestions and generators only `extra` has.
fn merge_arg(base: &crate::spec::Arg, extra: &crate::spec::Arg) -> crate::spec::Arg {
    let mut out = base.clone();
    for s in &extra.suggestions {
        if !out.suggestions.iter().any(|b| b.meta.names == s.meta.names) {
            out.suggestions.push(s.clone());
        }
    }
    // Our templates are precise (`cd`: folders only); theirs only count when we have none.
    if out.template.is_empty() {
        out.template = extra.template.clone();
    }
    for g in &extra.generators {
        if !out.generators.iter().any(|b| b.id == g.id) {
            out.generators.push(g.clone());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn merge_combines_args() {
        use crate::spec::{arg, cmd};
        let ours = cmd("pip", "").sub(cmd("install", "").arg(arg("package").files()));
        let fig = cmd("pip", "")
            .sub(cmd("install", "Install packages").arg(arg("package").suggest(&["requests", "numpy"])))
            .sub(cmd("freeze", ""));
        let m = super::merge(&ours, &fig);
        let install = m.find_subcommand("install").unwrap();
        assert_eq!(install.args[0].suggestions.len(), 2);
        assert_eq!(install.args[0].template.len(), 1);
        let cd = cmd("cd", "").arg(arg("dir").folders());
        let fig_cd = cmd("cd", "").arg(arg("dir").files());
        assert_eq!(super::merge(&cd, &fig_cd).args[0].template, [crate::spec::Template::Folders]);
        assert_eq!(install.meta.description.as_deref(), Some("Install packages"));
        assert!(m.find_subcommand("freeze").is_some());
    }

    /// Specs round-trip through JSON unchanged (no unknown keys) and use only existing generators.
    #[test]
    fn lig_specs_are_valid() {
        let names: Vec<&str> = super::lig_names().collect();
        assert!(names.len() >= 50, "{}", names.len());
        for n in names {
            let v = super::LIG.json(n).unwrap();
            let spec = crate::spec::from_value(&v);
            assert_eq!(spec.meta.names[0], n, "{n}: name doesn't match the file name");
            assert_eq!(crate::spec::to_value(&spec), v, "{n}: keys lig doesn't understand");
            let text = v.to_string();
            for id in text.split("\"builtin\":\"").skip(1).map(|r| r.split('"').next().unwrap()) {
                assert!(super::gens::builtin(id).is_some(), "{n}: unknown generator {id}");
            }
        }
    }
}
