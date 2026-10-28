//! Specs for commands without a built-in one, parsed from `--help` or the man page.
//! `--help` is only run (no stdin, no display) for ELF binaries and python/node/ruby scripts,
//! whose argument parsers handle it before doing anything; output is cached per executable version.

use std::collections::HashMap;
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use crate::exec::{self, Cmd};
use crate::spec::{LoadSpec, Opt, Subcommand, Template, arg, cmd, opt};

/// Subcommand help is parsed lazily up to this depth (`cmd a b c`).
const MAX_DEPTH: usize = 4;

pub enum Lookup {
    /// Known: the parsed spec, or `None` when there is nothing to parse.
    Found(Option<Arc<Subcommand>>),
    /// Not cached; running the command is needed.
    NeedsRun,
}

/// Parses and caches `--help` specs (in memory, and the raw text on disk).
pub struct HelpLoader {
    cache_dir: PathBuf,
    timeout: Duration,
    /// Commands never to run `--help` for (`help_skip`).
    skip: Vec<String>,
    /// Parsed specs by command line (`"docker compose"`).
    mem: HashMap<String, Option<Arc<Subcommand>>>,
}

pub fn cache_dir() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("lig").join("help")
}

/// FNV-1a, stable across builds (used for cache file names).
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

fn which(name: &str, path: &str) -> Option<PathBuf> {
    if name.contains('/') {
        return Some(PathBuf::from(name));
    }
    path.split(':')
        .filter(|d| !d.is_empty())
        .map(|d| Path::new(d).join(name))
        .find(|p| std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0))
}

/// May `--help` be executed for this file?
fn help_is_safe(exe: &Path) -> bool {
    let Ok(mut f) = std::fs::File::open(exe) else { return false };
    let mut head = [0u8; 128];
    let n = f.read(&mut head).unwrap_or(0);
    let head = &head[..n];
    if head.starts_with(b"\x7fELF") {
        return true;
    }
    if head.starts_with(b"#!") {
        let line = head.split(|&b| b == b'\n').next().unwrap_or(b"");
        let line = String::from_utf8_lossy(line);
        return ["python", "node", "ruby", "deno", "bun"].iter().any(|i| line.contains(i));
    }
    false
}

/// Drop man-style overstrike (`x\bx`, `_\bx`) and ANSI escapes.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x08' => {
                out.pop();
            }
            '\x1b' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for n in chars.by_ref() {
                        if n.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            }
            _ => out.push(c),
        }
    }
    out
}

impl HelpLoader {
    pub fn new(timeout: Duration, skip: Vec<String>) -> Self {
        HelpLoader { cache_dir: cache_dir(), timeout, skip, mem: HashMap::new() }
    }

    /// The spec for `argv` (`["docker"]`, `["docker", "compose"]`).
    pub fn lookup(&mut self, argv: &[String], cwd: &Path, env: &[(String, String)], allow_run: bool) -> Lookup {
        let key = argv.join(" ");
        if let Some(s) = self.mem.get(&key) {
            return Lookup::Found(s.clone());
        }
        if argv.is_empty() || argv.len() > MAX_DEPTH || self.skip.iter().any(|s| *s == argv[0]) {
            return Lookup::Found(None);
        }
        let path = env
            .iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v.clone())
            .or_else(|| std::env::var("PATH").ok())
            .unwrap_or_default();
        let Some(exe) = which(&argv[0], &path) else {
            self.mem.insert(key, None);
            return Lookup::Found(None);
        };
        let stamp = match std::fs::metadata(&exe) {
            Ok(m) => format!("{}:{}:{}", exe.display(), m.mtime(), m.size()),
            Err(_) => return Lookup::Found(None),
        };
        let file = self.cache_dir.join(format!("{:016x}.json", fnv(&key)));
        let cached = std::fs::read_to_string(&file)
            .ok()
            .and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok())
            .filter(|v| v["key"] == key.as_str() && v["stamp"] == stamp.as_str())
            .and_then(|v| v["text"].as_str().map(str::to_string));
        let text = match cached {
            Some(t) => t,
            None if !allow_run => return Lookup::NeedsRun,
            None => {
                let t = self.fetch(argv, &exe, cwd, env);
                let _ = std::fs::create_dir_all(&self.cache_dir);
                let _ = std::fs::write(&file, json!({"key": key, "stamp": stamp, "text": t}).to_string());
                t
            }
        };
        let spec = parse(&text, argv).map(Arc::new);
        self.mem.insert(key, spec.clone());
        Lookup::Found(spec)
    }

    /// `--help` output if it parses into something, else the man page (top-level commands only).
    fn fetch(&self, argv: &[String], exe: &Path, cwd: &Path, env: &[(String, String)]) -> String {
        let mut vars = env.to_vec();
        for (k, v) in [
            ("DISPLAY", ":lig-none"),
            ("WAYLAND_DISPLAY", "lig-none"),
            ("PAGER", "cat"),
            ("GIT_PAGER", "cat"),
            ("MANPAGER", "cat"),
            ("NO_COLOR", "1"),
            ("TERM", "dumb"),
            ("COLUMNS", "100"),
            ("MANWIDTH", "100"),
        ] {
            vars.push((k.into(), v.into()));
        }
        if help_is_safe(exe) {
            let mut a = argv.to_vec();
            a.push("--help".into());
            let out = exec::run(Cmd::Argv(&a), cwd, &vars, self.timeout);
            let text = if out.stdout.trim().is_empty() { out.stderr } else { out.stdout };
            if parse(&text, argv).is_some_and(|s| !s.options.is_empty() || !s.subcommands.is_empty()) {
                return clean(&text);
            }
        }
        if argv.len() == 1 {
            let a = vec!["man".to_string(), "-P".into(), "cat".into(), argv[0].clone()];
            let out = exec::run(Cmd::Argv(&a), cwd, &vars, self.timeout);
            if out.status == 0 {
                return clean(&out.stdout);
            }
        }
        String::new()
    }
}

fn indent(l: &str) -> usize {
    l.len() - l.trim_start().len()
}

/// Split at the first run of 2+ spaces or a tab.
fn split_gap(t: &str) -> (&str, &str) {
    let b = t.as_bytes();
    for i in 0..b.len() {
        if b[i] == b'\t' || (b[i] == b' ' && b.get(i + 1) == Some(&b' ')) {
            return (t[..i].trim_end(), t[i..].trim());
        }
    }
    (t, "")
}

fn valid_flag(n: &str) -> bool {
    let body = n.trim_start_matches('-');
    let dashes = n.len() - body.len();
    (1..=2).contains(&dashes)
        && body.chars().next().is_some_and(|c| c.is_ascii_alphanumeric() || c == '?')
        && body.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:+?#".contains(c))
}

fn valid_word(n: &str) -> bool {
    n.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && n.chars().all(|c| c.is_ascii_alphanumeric() || "-_.:".contains(c))
}

fn placeholder_arg(p: &str, optional: bool) -> crate::spec::Arg {
    let inner = p.trim_matches(|c| "<>[]{}=".contains(c));
    let mut a = arg(&inner.to_lowercase());
    if optional {
        a = a.optional();
    }
    if p.starts_with('{') && inner.contains(',') {
        let items: Vec<&str> = inner.split(',').map(str::trim).filter(|s| !s.is_empty()).collect();
        return a.suggest(&items);
    }
    let u = inner.to_uppercase();
    if u.contains("DIR") {
        a = a.folders();
    } else if u.contains("FILE") || u.contains("PATH") {
        a = a.files();
    }
    a
}

fn possible_values(desc: &str) -> Vec<String> {
    if let Some(i) = desc.find("possible values:") {
        let rest = &desc[i + 16..];
        let rest = rest.split(']').next().unwrap_or(rest);
        return rest.split(',').map(|v| v.trim().to_string()).filter(|v| valid_word(v)).collect();
    }
    if desc.contains(" is '") || desc.contains(" are '") {
        let parts: Vec<&str> = desc.split('\'').collect();
        return parts.iter().skip(1).step_by(2).map(|v| v.to_string()).filter(|v| valid_word(v)).collect();
    }
    vec![]
}

/// Parse an option's spec part: `-o, --output=FILE`, `--color[=WHEN]`, `-p <SPEC>`.
fn parse_option(spec: &str, desc: &str) -> Option<Opt> {
    let mut names: Vec<String> = vec![];
    let mut argument = None;
    let mut eq = false;
    for tok in spec.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
        if tok.starts_with('-') {
            // `--verbose...` (clap: repeatable)
            let tok = tok.trim_end_matches("...");
            let (name, rest) = match tok.find(['=', '[']) {
                Some(i) => (&tok[..i], &tok[i..]),
                None => (tok, ""),
            };
            if !valid_flag(name) {
                return None;
            }
            names.push(name.to_string());
            if !rest.is_empty() {
                let optional = rest.starts_with('[');
                eq = rest.contains('=');
                argument = Some(placeholder_arg(rest.trim_start_matches('['), optional));
            }
        } else if names.is_empty() {
            return None;
        } else if tok.starts_with(['<', '[', '{']) || tok.chars().all(|c| c.is_ascii_uppercase() || "_-.".contains(c)) {
            if argument.is_none() {
                argument = Some(placeholder_arg(tok, tok.starts_with('[')));
            }
        } else {
            break;
        }
    }
    if names.is_empty() {
        return None;
    }
    // `[possible values: a, b, c]` (clap), `WHEN is 'always', 'auto', or 'never'` (GNU)
    let values = possible_values(desc);
    if !values.is_empty() {
        let refs: Vec<&str> = values.iter().map(String::as_str).collect();
        argument = Some(match argument.take() {
            Some(mut a) if a.suggestions.is_empty() => {
                a = a.suggest(&refs);
                a
            }
            Some(a) => a,
            None => {
                eq = true;
                arg("value").optional().suggest(&refs)
            }
        });
    }
    let mut o = opt(&names.join(","), desc);
    if let Some(a) = argument {
        o = o.arg(a);
        if eq {
            o = o.eq();
        }
    }
    Some(o)
}

/// Build a spec from help/man text. `None` if nothing useful was found.
pub fn parse(text: &str, argv: &[String]) -> Option<Subcommand> {
    let text = clean(text);
    let lines: Vec<&str> = text.lines().collect();
    let next_desc = |i: usize, ind: usize| -> String {
        lines[i + 1..]
            .iter()
            .find(|l| !l.trim().is_empty())
            .filter(|l| indent(l) > ind)
            .map(|l| l.trim().to_string())
            .unwrap_or_default()
    };

    let mut options: Vec<Opt> = vec![];
    let mut subs: Vec<(Vec<String>, String)> = vec![];
    let mut args: Vec<crate::spec::Arg> = vec![];
    let mut in_args = false;
    let mut arg_indent: Option<usize> = None;
    let mut in_commands = false;
    // Indent of the current commands section's items (set by the first); prose is indented differently.
    let mut item_indent: Option<usize> = None;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if t.is_empty() {
            continue;
        }
        let ind = indent(line);
        // Section headers: `Commands:`, `COMMANDS`, `Available Commands:` …
        if ind == 0 && (t.ends_with(':') || t.chars().all(|c| !c.is_ascii_lowercase())) && !t.starts_with('-') {
            let header = t.to_lowercase();
            in_commands = header.contains("command");
            in_args = !in_commands && header.contains("argument");
            item_indent = None;
            arg_indent = None;
            continue;
        }
        if t.starts_with('-') {
            let (spec, desc) = split_gap(t);
            let desc = if desc.is_empty() { next_desc(i, ind) } else { desc.to_string() };
            let desc = desc.split_whitespace().collect::<Vec<_>>().join(" ");
            if let Some(o) = parse_option(spec, &desc)
                && !options.iter().any(|x| x.meta.names == o.meta.names)
            {
                options.push(o);
            }
            continue;
        }
        if in_args {
            // One item per line at the same indent; deeper lines continue a description.
            if ind == 0 || arg_indent.is_some_and(|w| w != ind) {
                continue;
            }
            let (left, desc) = split_gap(t);
            let desc = if desc.is_empty() { next_desc(i, ind) } else { desc.to_string() };
            if let Some(a) = parse_positional(left, &desc) {
                arg_indent.get_or_insert(ind);
                args.push(a);
            }
            continue;
        }
        if !in_commands || ind == 0 || item_indent.is_some_and(|w| w != ind) {
            continue;
        }
        let (left, desc) = split_gap(t);
        let left = left.replace(", ", ",");
        let mut words = left.split_whitespace();
        let first = words.next().unwrap_or("");
        let rest: Vec<&str> = words.collect();
        // `name  description` on one line (help output), or `name [args]`
        // with the description on the next, more indented line (man pages).
        let same_line = !desc.is_empty()
            && rest.first().is_none_or(|w| {
                w.starts_with(['<', '[', '{', '-', '.']) || w.starts_with(|c: char| c.is_ascii_uppercase())
            });
        let man_style = desc.is_empty() && !next_desc(i, ind).is_empty();
        let bare = desc.is_empty() && rest.is_empty();
        if !(same_line || man_style || bare) {
            continue;
        }
        let names: Vec<String> = first
            .split(',')
            .map(|n| n.trim().trim_end_matches(':').to_string())
            .filter(|n| valid_word(n) && n.starts_with(|c: char| c.is_ascii_lowercase()))
            .collect();
        if names.is_empty() {
            continue;
        }
        item_indent.get_or_insert(ind);
        if subs.iter().any(|(n, _)| n[0] == names[0]) {
            continue;
        }
        let desc = if desc.is_empty() { next_desc(i, ind) } else { desc.to_string() };
        subs.push((names, desc.split_whitespace().collect::<Vec<_>>().join(" ")));
    }
    if options.is_empty() && subs.is_empty() && args.is_empty() {
        return None;
    }
    let mut spec = cmd(argv.last().map_or("", String::as_str), "").opts(options);
    for (names, desc) in subs {
        let mut sub = cmd(&names.join(","), &desc);
        if argv.len() < MAX_DEPTH {
            let mut a = argv.to_vec();
            a.push(names[0].clone());
            sub.load_spec = Some(LoadSpec::Help(a));
        }
        spec = spec.sub(sub);
    }
    if !args.is_empty() {
        for a in args {
            spec = spec.arg(a);
        }
    } else if spec.subcommands.is_empty() {
        let mut a = arg("file").optional().variadic();
        a.template.push(Template::Filepaths);
        spec = spec.arg(a);
    }
    Some(spec)
}

/// A positional from an `Arguments:` item: `[SHELL]`, `<FILE>...`, `{start,stop}`, `name`.
fn parse_positional(left: &str, desc: &str) -> Option<crate::spec::Arg> {
    if left.contains(char::is_whitespace) || left.starts_with('-') || left.is_empty() {
        return None;
    }
    let variadic = left.ends_with("...");
    let token = left.trim_end_matches("...");
    let mut a = placeholder_arg(token, token.starts_with('['));
    if a.name.is_empty() {
        return None;
    }
    if variadic {
        a = a.variadic();
    }
    let values = possible_values(desc);
    if !values.is_empty() {
        a.template.clear();
        a = a.suggest(&values.iter().map(String::as_str).collect::<Vec<_>>());
    }
    Some(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn positional_arguments() {
        // clap
        let text = "Add lig to your shell\n\nUsage: lig install [SHELL]\n\nArguments:\n  [SHELL]  [possible values: fish, zsh, bash]\n\nOptions:\n  -h, --help  Print help\n";
        let s = parse(text, &["install".into()]).unwrap();
        assert_eq!(s.args.len(), 1);
        assert!(s.args[0].is_optional && s.args[0].template.is_empty());
        let vals: Vec<&str> = s.args[0].suggestions.iter().map(|x| x.meta.names[0].as_str()).collect();
        assert_eq!(vals, ["fish", "zsh", "bash"]);
        // argparse
        let text = "usage: tool [-h] {start,stop} paths [paths ...]\n\npositional arguments:\n  {start,stop}  What to do\n  <DIR>...      Folders to watch, one\n                or more\n\noptions:\n  -h, --help    show help\n";
        let s = parse(text, &["tool".into()]).unwrap();
        assert_eq!(s.args.len(), 2, "{:?}", s.args.iter().map(|a| &a.name).collect::<Vec<_>>());
        assert_eq!(s.args[0].suggestions.len(), 2);
        assert!(s.args[1].is_variadic && s.args[1].template == [Template::Folders]);
    }

    fn names(s: &Subcommand) -> Vec<String> {
        s.options.iter().map(|o| o.meta.names.join(",")).collect()
    }

    #[test]
    fn gnu_style() {
        let text = "Usage: ls [OPTION]... [FILE]...\n\nMandatory arguments:\n  -a, --all                  do not ignore entries starting with .\n      --block-size=SIZE      with -l, scale sizes by SIZE\n      --color[=WHEN]         color the output WHEN\n  -I, --ignore=PATTERN       do not list implied entries matching shell PATTERN\n  -T, --tabsize=COLS         assume tab stops at each COLS instead of 8\n";
        let s = parse(text, &["ls".into()]).unwrap();
        assert_eq!(names(&s), ["-a,--all", "--block-size", "--color", "-I,--ignore", "-T,--tabsize"]);
        assert_eq!(s.options[0].meta.description.as_deref(), Some("do not ignore entries starting with ."));
        assert!(s.options[1].takes_arg() && s.options[1].separator.is_some());
        assert!(!s.options[2].takes_arg());
        assert_eq!(s.args.len(), 1);
        let text = "  -c, --color  Colorize. [possible values: always, auto, never]\n      --when=WHEN  run WHEN is 'now', 'later'\n";
        let s = parse(text, &["x".into()]).unwrap();
        let vals = |o: &Opt| o.args[0].suggestions.iter().map(|x| x.meta.names[0].clone()).collect::<Vec<_>>();
        assert_eq!(vals(&s.options[0]), ["always", "auto", "never"]);
        assert_eq!(vals(&s.options[1]), ["now", "later"]);
    }

    #[test]
    fn clap_and_cobra_style() {
        let text = "A tool\n\nUsage: tool [OPTIONS] <COMMAND>\n\nCommands:\n  build, b    Compile the current package\n  run, r      Run a binary\n  help        Print help\n\nOptions:\n  -p, --package <SPEC>  Package to build\n  -v, --verbose...      Use verbose output\n\nAvailable Commands:\n  completion  Generate the autocompletion script\n";
        let s = parse(text, &["tool".into()]).unwrap();
        let subs: Vec<Vec<String>> = s.subcommands.iter().map(|c| c.meta.names.clone()).collect();
        assert_eq!(subs, [vec!["build", "b"], vec!["run", "r"], vec!["help"], vec!["completion"]]);
        assert!(s.options[0].takes_arg());
        assert!(matches!(&s.subcommands[0].load_spec, Some(LoadSpec::Help(a)) if a == &["tool", "build"]));
    }

    #[test]
    fn man_subcommands_with_justified_prose() {
        let text = "SUBCOMMANDS\n   search key\n       Searches for a repository  where  the  given  argument  key  (a  regular\n       expression)  matches either the name of the repository, its description,\n       or the URL of the repository in question. The full YAML configuration of\n       all repositories that have a match are printed to standard output.\n\n       To search for all repositories, don't provide any search key.\n\n   enable repository_name\n       Enable the repository named repository_name.\n\n       •   If the repository had not yet been created before, this creates  its\n           configuration from the current metadata.\n\n   disable repository_name\n       This simply adds a line \"Enabled: no\" to the apt configuration file,  so\n       that the repository is not enabled.\n\n   update repository_name\n       Re-writes the apt configuration file for this repository.\n\nFILES\n       /etc/extrepo/config.yaml Extrepo configuration file.\n";
        let s = parse(text, &["extrepo".into()]).unwrap();
        let subs: Vec<&str> = s.subcommands.iter().map(|c| c.meta.names[0].as_str()).collect();
        assert_eq!(subs, ["search", "enable", "disable", "update"]);
        assert!(s.subcommands[0].meta.description.as_deref().unwrap().starts_with("Searches for a repository"));
    }

    #[test]
    fn man_style() {
        let text = "NAME\n       foo - do things\n\nCOMMANDS\n       list-units [PATTERN...]\n           List units.\n\n       start UNIT...\n           Start units.\n\nOPTIONS\n       -t, --type=\n           The argument is a comma-separated list of unit types.\n\n       -q, --quiet\n           Suppress output.\n";
        let s = parse(text, &["foo".into()]).unwrap();
        let subs: Vec<&str> = s.subcommands.iter().map(|c| c.meta.names[0].as_str()).collect();
        assert_eq!(subs, ["list-units", "start"]);
        assert_eq!(names(&s), ["-t,--type", "-q,--quiet"]);
        assert_eq!(s.options[1].meta.description.as_deref(), Some("Suppress output."));
    }
}
