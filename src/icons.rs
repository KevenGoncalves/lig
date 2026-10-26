//! Popup icons: Nerd Font glyphs when available, plain Unicode otherwise.
//!
//! `icons = "auto"` uses Nerd Font glyphs when a Nerd Font is installed
//! (`fc-list`, checked once in the background); set `icons = "nerd"` when the
//! terminal provides them some other way.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::config::IconOverride;
use crate::spec::{Icon, Kind};

const UNKNOWN: u8 = 0;
const YES: u8 = 1;
const NO: u8 = 2;
static NERD: AtomicU8 = AtomicU8::new(UNKNOWN);

fn nerd_font_installed() -> bool {
    let out = std::process::Command::new("fc-list").args([":", "family"]).output();
    out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("Nerd Font"))
}

/// Start detecting Nerd Font support (call once at startup).
pub fn detect_in_background() {
    std::thread::spawn(detect);
}

/// Detect Nerd Font support now.
pub fn detect() {
    NERD.store(if nerd_font_installed() { YES } else { NO }, Ordering::Relaxed);
}

/// Which glyph set to use for the configured `icons` style.
pub fn use_nerd(style: &str) -> bool {
    match style {
        "nerd" => true,
        "auto" => NERD.load(Ordering::Relaxed) == YES,
        _ => false,
    }
}

const GREY: &str = "38;5;244";

/// `[icons]` overrides by normalized name: `(glyph, SGR color)`, each optional.
type Overrides = HashMap<String, (Option<&'static str>, Option<&'static str>)>;

static OVERRIDES: OnceLock<Overrides> = OnceLock::new();

/// The name of a suggestion kind in `[icons]`.
fn kind_name(kind: Kind) -> &'static str {
    match kind {
        Kind::Command => "command",
        Kind::Subcommand => "subcommand",
        Kind::Option => "option",
        Kind::Arg => "arg",
        Kind::File => "file",
        Kind::Folder => "folder",
        Kind::Shortcut => "shortcut",
        Kind::Info => "loading",
        Kind::Execute => "enter",
        Kind::Back => "back",
    }
}

const KINDS: [Kind; 10] = [
    Kind::Command,
    Kind::Subcommand,
    Kind::Option,
    Kind::Arg,
    Kind::File,
    Kind::Folder,
    Kind::Shortcut,
    Kind::Info,
    Kind::Execute,
    Kind::Back,
];

/// An `[icons]` key as looked up: lowercase; `_` as `-` in names (`git_branch`).
fn normalize(key: &str) -> String {
    let k = key.trim().to_lowercase();
    if k.contains('.') { k } else { k.replace('_', "-") }
}

/// Use the `[icons]` section of the config (call once, before drawing).
/// Unknown names are reported; file names are taken as they are.
pub fn set_overrides(icons: &BTreeMap<String, IconOverride>) {
    let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
    let mut map = Overrides::new();
    for (key, value) in icons {
        let name = normalize(key);
        let known = KINDS.iter().any(|k| kind_name(*k) == name)
            || Icon::from_name(&name).is_some()
            || name.contains('.')
            || key.starts_with(|c: char| c.is_uppercase());
        if !known {
            eprintln!("lig: unknown icon {key:?} in [icons] (`lig icons` lists the names)");
            continue;
        }
        let (glyph, color) = match value {
            IconOverride::Glyph(g) => (Some(g.clone()), None),
            IconOverride::Style(s) => (s.glyph.clone(), s.color.clone()),
        };
        map.insert(name, (glyph.map(leak), color.map(|c| leak(crate::config::sgr_color(&c, 38)))));
    }
    let _ = OVERRIDES.set(map);
}

/// `(glyph, SGR color)` for a suggestion: the built-in one, then the
/// `[icons]` overrides from general to specific (kind, icon, `*.ext`, file name).
pub fn glyph(kind: Kind, icon: Option<Icon>, label: &str, nerd: bool) -> (&'static str, &'static str) {
    let built_in = default_glyph(kind, icon, label, nerd);
    match OVERRIDES.get() {
        Some(map) if !map.is_empty() => apply(map, kind, icon, label, built_in),
        _ => built_in,
    }
}

fn apply(
    map: &Overrides,
    kind: Kind,
    icon: Option<Icon>,
    label: &str,
    (mut glyph, mut color): (&'static str, &'static str),
) -> (&'static str, &'static str) {
    let mut keys = vec![kind_name(kind).to_string()];
    if let Some(i) = icon {
        keys.push(i.name().to_string());
    }
    if kind == Kind::File {
        let name = label.to_lowercase();
        if let Some((_, ext)) = name.rsplit_once('.') {
            keys.push(format!("*.{ext}"));
        }
        keys.push(name);
    }
    for key in keys {
        if let Some((g, c)) = map.get(&key) {
            glyph = g.unwrap_or(glyph);
            color = c.unwrap_or(color);
        }
    }
    (glyph, color)
}

/// The built-in `(glyph, SGR color)` for a suggestion.
fn default_glyph(kind: Kind, icon: Option<Icon>, label: &str, nerd: bool) -> (&'static str, &'static str) {
    if nerd {
        if let Some(i) = icon {
            return nerd_icon(i);
        }
        if kind == Kind::File {
            return file_icon(label);
        }
    } else if let Some(i) = icon {
        return unicode_icon(i);
    }
    let color = match kind {
        Kind::Command => "38;5;213",
        Kind::Subcommand => "38;5;81",
        Kind::Option => "38;5;221",
        Kind::Arg => "38;5;114",
        Kind::File => "38;5;250",
        Kind::Folder => "38;5;75",
        Kind::Shortcut => "38;5;209",
        Kind::Info => GREY,
        Kind::Execute => "38;5;114",
        Kind::Back => "38;5;75",
    };
    let g = if nerd {
        match kind {
            Kind::Command => "\u{f120}",    // terminal
            Kind::Subcommand => "\u{ea8c}", // symbol-method
            Kind::Option => "\u{f024}",     // flag
            Kind::Arg => "\u{ea88}",        // symbol-variable
            Kind::File => "\u{f15b}",       // file
            Kind::Folder => "\u{f07b}",     // folder
            Kind::Shortcut => "\u{f0e7}",   // bolt
            Kind::Info => "\u{f110}",       // spinner
            Kind::Execute => "↵",
            Kind::Back => "\u{f062}", // arrow-up
        }
    } else {
        match kind {
            Kind::Command => "❯",
            Kind::Subcommand => "▸",
            Kind::Option => "◆",
            Kind::Arg => "•",
            Kind::File => "▫",
            Kind::Folder => "▪",
            Kind::Shortcut => "★",
            Kind::Info => "…",
            Kind::Execute => "↵",
            Kind::Back => "↑",
        }
    };
    (g, color)
}

fn nerd_icon(i: Icon) -> (&'static str, &'static str) {
    match i {
        Icon::Alias => ("\u{f44c}", "38;5;213"),    // link
        Icon::Function => ("\u{f121}", "38;5;213"), // code
        Icon::Builtin => ("\u{f489}", "38;5;213"),  // terminal
        Icon::Package => ("\u{f487}", "38;5;179"),  // package
        Icon::Npm => ("\u{e71e}", "38;5;167"),
        Icon::Rust => ("\u{e7a8}", "38;5;173"),
        Icon::Python => ("\u{e73c}", "38;5;221"),
        Icon::Docker => ("\u{f308}", "38;5;39"),
        Icon::Container => ("\u{f1b2}", "38;5;39"), // cube
        Icon::GitBranch => ("\u{e725}", "38;5;208"),
        Icon::GitTag => ("\u{f412}", "38;5;208"),
        Icon::GitCommit => ("\u{f417}", "38;5;208"),
        Icon::GitRemote => ("\u{f401}", "38;5;208"), // repo
        Icon::GitStash => ("\u{f411}", "38;5;208"),  // archive
        Icon::Process => ("\u{f423}", "38;5;114"),   // gear
        Icon::Service => ("\u{f085}", "38;5;114"),   // cogs
        Icon::Host => ("\u{f233}", "38;5;75"),       // server
        Icon::User => ("\u{f007}", "38;5;75"),
        Icon::Version => ("\u{f454}", "38;5;141"), // versions
        Icon::Script => ("\u{f04b}", "38;5;114"),  // play
        Icon::Folder => ("\u{f07b}", "38;5;75"),
        Icon::Volume => ("\u{f0a0}", "38;5;75"),  // hdd
        Icon::Network => ("\u{f0e8}", "38;5;75"), // sitemap
        Icon::Cloud => ("\u{f0c2}", "38;5;75"),
        Icon::Terminal => ("\u{f120}", "38;5;213"),
    }
}

fn unicode_icon(i: Icon) -> (&'static str, &'static str) {
    match i {
        Icon::Alias | Icon::Function | Icon::Builtin | Icon::Terminal => ("❯", "38;5;213"),
        Icon::Package | Icon::Npm | Icon::Rust | Icon::Python => ("◈", "38;5;179"),
        Icon::Docker | Icon::Container => ("◈", "38;5;39"),
        Icon::GitBranch | Icon::GitTag | Icon::GitCommit | Icon::GitRemote | Icon::GitStash => ("⎇", "38;5;208"),
        Icon::Process | Icon::Service | Icon::Script => ("•", "38;5;114"),
        Icon::Host | Icon::User | Icon::Cloud | Icon::Network | Icon::Volume => ("◆", "38;5;75"),
        Icon::Version => ("#", "38;5;141"),
        Icon::Folder => ("▪", "38;5;75"),
    }
}

/// File icons by name/extension (Nerd Font only).
fn file_icon(name: &str) -> (&'static str, &'static str) {
    let lower = name.to_lowercase();
    match lower.as_str() {
        "dockerfile" | "containerfile" => return ("\u{f308}", "38;5;39"),
        "makefile" | "justfile" | "cmakelists.txt" => return ("\u{e615}", "38;5;250"),
        "license" | "licence" | "copying" => return ("\u{f0fc}", "38;5;179"),
        "cargo.lock" | "package-lock.json" | "yarn.lock" | "pnpm-lock.yaml" | "poetry.lock" => {
            return ("\u{f023}", "38;5;244");
        }
        _ => {}
    }
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    match ext {
        "rs" => ("\u{e7a8}", "38;5;173"),
        "py" | "pyi" => ("\u{e73c}", "38;5;221"),
        "js" | "mjs" | "cjs" => ("\u{e74e}", "38;5;185"),
        "ts" | "mts" | "cts" => ("\u{e628}", "38;5;75"),
        "jsx" | "tsx" => ("\u{e7ba}", "38;5;81"),
        "json" | "jsonc" => ("\u{e60b}", "38;5;185"),
        "md" | "markdown" => ("\u{e73e}", "38;5;250"),
        "toml" | "yaml" | "yml" | "ini" | "conf" | "cfg" | "env" => ("\u{e615}", "38;5;250"),
        "sh" | "bash" | "zsh" | "fish" => ("\u{e795}", "38;5;114"),
        "html" | "htm" => ("\u{e736}", "38;5;208"),
        "css" | "scss" | "sass" => ("\u{e749}", "38;5;75"),
        "go" => ("\u{e724}", "38;5;81"),
        "java" | "jar" => ("\u{e738}", "38;5;173"),
        "c" | "h" => ("\u{e61e}", "38;5;75"),
        "cpp" | "cc" | "cxx" | "hpp" => ("\u{e61d}", "38;5;75"),
        "rb" => ("\u{e739}", "38;5;167"),
        "php" => ("\u{e73d}", "38;5;141"),
        "lua" => ("\u{e620}", "38;5;75"),
        "vim" => ("\u{e7c5}", "38;5;114"),
        "swift" => ("\u{e755}", "38;5;208"),
        "lock" => ("\u{f023}", "38;5;244"),
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" | "ico" | "bmp" => ("\u{f1c5}", "38;5;141"),
        "zip" | "tar" | "gz" | "xz" | "bz2" | "7z" | "zst" | "rar" | "deb" | "rpm" => ("\u{f1c6}", "38;5;179"),
        "pdf" => ("\u{f1c1}", "38;5;167"),
        "txt" | "log" => ("\u{f0f6}", "38;5;250"),
        "csv" | "tsv" | "xls" | "xlsx" => ("\u{f1c3}", "38;5;114"),
        "mp3" | "wav" | "flac" | "ogg" => ("\u{f1c7}", "38;5;141"),
        "mp4" | "mkv" | "webm" | "mov" => ("\u{f1c8}", "38;5;141"),
        _ => ("\u{f15b}", "38;5;250"),
    }
}

/// Every icon, for `lig icons`: `(glyph, color, name in [icons], what)`.
pub fn legend(nerd: bool) -> Vec<(&'static str, &'static str, String, String)> {
    let kinds = [
        (Kind::Command, "command"),
        (Kind::Subcommand, "subcommand"),
        (Kind::Option, "option / flag"),
        (Kind::Arg, "argument value"),
        (Kind::Folder, "folder"),
        (Kind::File, "file (generic)"),
        (Kind::Shortcut, "shortcut"),
        (Kind::Info, "loading message"),
        (Kind::Execute, "run the command (folders)"),
        (Kind::Back, "up one folder"),
    ];
    let icons = [
        (Icon::Alias, "alias"),
        (Icon::Function, "shell function"),
        (Icon::Builtin, "shell builtin"),
        (Icon::Package, "package (apt, snap, brew, pip list)"),
        (Icon::Npm, "npm package"),
        (Icon::Rust, "crate"),
        (Icon::Python, "PyPI package"),
        (Icon::Docker, "docker image"),
        (Icon::Container, "container"),
        (Icon::GitBranch, "git branch"),
        (Icon::GitTag, "git tag"),
        (Icon::GitCommit, "git commit"),
        (Icon::GitRemote, "git remote"),
        (Icon::GitStash, "git stash"),
        (Icon::Process, "process"),
        (Icon::Service, "systemd unit"),
        (Icon::Host, "ssh host"),
        (Icon::User, "user"),
        (Icon::Version, "version"),
        (Icon::Script, "script / make target"),
        (Icon::Folder, "directory (zoxide)"),
        (Icon::Volume, "volume"),
        (Icon::Network, "network"),
        (Icon::Cloud, "cloud / kubernetes"),
        (Icon::Terminal, "terminal session"),
    ];
    let mut out = vec![];
    for (k, what) in kinds {
        let label = if k == Kind::File { "x" } else { "" };
        let (g, c) = glyph(k, None, label, nerd);
        out.push((g, c, kind_name(k).to_string(), what.to_string()));
    }
    for (i, what) in icons {
        let (g, c) = glyph(Kind::Arg, Some(i), "", nerd);
        out.push((g, c, i.name().to_string(), what.to_string()));
    }
    if nerd {
        for f in [
            "main.rs",
            "app.py",
            "index.js",
            "app.ts",
            "data.json",
            "README.md",
            "config.toml",
            "run.sh",
            "Dockerfile",
            "photo.png",
            "backup.tar.gz",
            "Cargo.lock",
        ] {
            let (g, c) = glyph(Kind::File, None, f, true);
            let key = f.rsplit_once('.').map_or(f.to_string(), |(_, e)| format!("*.{e}"));
            out.push((g, c, key, format!("file: {f}")));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_specific_icons() {
        assert_eq!(glyph(Kind::Arg, Some(Icon::GitBranch), "main", true).0, "\u{e725}");
        assert_eq!(glyph(Kind::File, None, "main.rs", true).0, "\u{e7a8}");
        assert_eq!(glyph(Kind::File, None, "Dockerfile", true).0, "\u{f308}");
        assert_eq!(glyph(Kind::Folder, None, "src/", true).0, "\u{f07b}");
        assert_eq!(glyph(Kind::Option, None, "-v", false).0, "◆");
        assert_eq!(glyph(Kind::Arg, Some(Icon::Npm), "react", false).0, "◈");
        assert!(use_nerd("nerd") && !use_nerd("unicode") && !use_nerd("none"));
    }

    #[test]
    fn overrides_go_from_general_to_specific() {
        let mut map = Overrides::new();
        map.insert("file".into(), (None, Some("38;5;1")));
        map.insert("*.rs".into(), (Some("R"), None));
        map.insert("dockerfile".into(), (Some("D"), Some("38;5;2")));
        map.insert("git-branch".into(), (Some("B"), None));
        let get = |kind, icon, label: &str| apply(&map, kind, icon, label, default_glyph(kind, icon, label, true));
        // `*.rs` sets the glyph, `file` the color of every file.
        assert_eq!(get(Kind::File, None, "main.rs"), ("R", "38;5;1"));
        assert_eq!(get(Kind::File, None, "notes.md").1, "38;5;1");
        assert_eq!(get(Kind::File, None, "Dockerfile"), ("D", "38;5;2"));
        // An icon name keeps the built-in color; untouched kinds stay built-in.
        assert_eq!(get(Kind::Arg, Some(Icon::GitBranch), "main"), ("B", "38;5;208"));
        assert_eq!(get(Kind::Option, None, "-v"), default_glyph(Kind::Option, None, "-v", true));
        assert_eq!(normalize("Git_Branch"), "git-branch");
        assert_eq!(normalize("*.RS"), "*.rs");
    }
}
