//! Completion spec model, shaped like Fig's spec format, with a builder DSL
//! and a JSON/TOML reader/writer.

// The builder DSL is a public authoring API; not every method is used.
#![allow(dead_code)]

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::exec::{self, Cmd};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Command,
    Subcommand,
    Option,
    Arg,
    File,
    Folder,
    Shortcut,
    /// Not a suggestion: a status message ("Searching npm…").
    Info,
    /// Run the command line as it is (the "Enter" item on top of folders).
    Execute,
    /// Go up one folder in the path being typed (the "Back" item).
    Back,
}

/// What a suggestion is, for its icon (falls back to its [`Kind`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Icon {
    Alias,
    Function,
    Builtin,
    Package,
    Npm,
    Rust,
    Python,
    Docker,
    Container,
    GitBranch,
    GitTag,
    GitCommit,
    GitRemote,
    GitStash,
    Process,
    Service,
    Host,
    User,
    Version,
    Script,
    Folder,
    Volume,
    Network,
    Cloud,
    Terminal,
}

impl Icon {
    pub fn name(self) -> &'static str {
        match self {
            Icon::Alias => "alias",
            Icon::Function => "function",
            Icon::Builtin => "builtin",
            Icon::Package => "package",
            Icon::Npm => "npm",
            Icon::Rust => "rust",
            Icon::Python => "python",
            Icon::Docker => "docker",
            Icon::Container => "container",
            Icon::GitBranch => "git-branch",
            Icon::GitTag => "git-tag",
            Icon::GitCommit => "git-commit",
            Icon::GitRemote => "git-remote",
            Icon::GitStash => "git-stash",
            Icon::Process => "process",
            Icon::Service => "service",
            Icon::Host => "host",
            Icon::User => "user",
            Icon::Version => "version",
            Icon::Script => "script",
            Icon::Folder => "folder",
            Icon::Volume => "volume",
            Icon::Network => "network",
            Icon::Cloud => "cloud",
            Icon::Terminal => "terminal",
        }
    }

    /// Icon names usable in spec files (`"icon": "git-branch"`).
    pub fn from_name(n: &str) -> Option<Icon> {
        Some(match n {
            "alias" => Icon::Alias,
            "function" => Icon::Function,
            "builtin" => Icon::Builtin,
            "package" => Icon::Package,
            "npm" => Icon::Npm,
            "rust" => Icon::Rust,
            "python" => Icon::Python,
            "docker" => Icon::Docker,
            "container" => Icon::Container,
            "git-branch" => Icon::GitBranch,
            "git-tag" => Icon::GitTag,
            "git-commit" => Icon::GitCommit,
            "git-remote" => Icon::GitRemote,
            "git-stash" => Icon::GitStash,
            "process" => Icon::Process,
            "service" => Icon::Service,
            "host" => Icon::Host,
            "user" => Icon::User,
            "version" => Icon::Version,
            "script" => Icon::Script,
            "folder" => Icon::Folder,
            "volume" => Icon::Volume,
            "network" => Icon::Network,
            "cloud" => Icon::Cloud,
            "terminal" => Icon::Terminal,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default)]
pub struct Meta {
    pub icon: Option<Icon>,
    pub names: Vec<String>,
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub insert_value: Option<String>,
    pub priority: Option<f64>,
    pub hidden: bool,
}

#[derive(Debug, Clone, Default)]
pub struct SuggestionDef {
    pub meta: Meta,
    pub kind: Option<Kind>,
}

#[derive(Debug, Clone)]
pub enum LoadSpec {
    /// Another spec by command name (built-in, user, or `--help`).
    Name(String),
    /// Parse `<argv> --help` when this subcommand is entered.
    Help(Vec<String>),
}

#[derive(Debug, Clone, Default)]
pub struct Directives {
    /// `-abc` is one flag, not `-a -b -c` (e.g. `find -name`, `kill -9`).
    pub flags_are_posix_noncompliant: bool,
    pub option_arg_separators: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Subcommand {
    pub meta: Meta,
    pub subcommands: Vec<Arc<Subcommand>>,
    pub options: Vec<Arc<Opt>>,
    pub args: Vec<Arc<Arg>>,
    pub additional_suggestions: Vec<SuggestionDef>,
    pub load_spec: Option<LoadSpec>,
    pub directives: Option<Directives>,
    /// lig: replace the converted withfig spec instead of merging with it (`skipFigSpec`).
    pub skip_fig_spec: bool,
}

#[derive(Debug, Clone, Default)]
pub struct Opt {
    pub meta: Meta,
    pub args: Vec<Arc<Arg>>,
    pub is_persistent: bool,
    pub is_repeatable: bool,
    /// The separator to insert between the flag and its value (`=`).
    pub separator: Option<String>,
    pub exclusive_on: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Arg {
    pub name: String,
    pub is_optional: bool,
    pub is_variadic: bool,
    pub options_can_break_variadic: bool,
    /// The rest of the line is another command (`sudo`, `time`, …).
    pub is_command: bool,
    /// Suggest command names, without switching to their spec (`which`, `man`).
    pub command_names: bool,
    pub suggestions: Vec<SuggestionDef>,
    pub template: Vec<Template>,
    pub generators: Vec<Arc<Generator>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    Filepaths,
    Folders,
}

/// When cached generator results must be recomputed while typing a token.
#[derive(Debug, Clone)]
pub enum Trigger {
    /// When the part of the token up to the last of these characters changes.
    Chars(String),
    /// On every keystroke.
    Change,
    /// When the first `n` characters change (and only once there are `n`).
    Prefix(usize),
}

pub struct GenCtx<'a> {
    pub cwd: &'a Path,
    pub env: &'a [(String, String)],
    /// All tokens of the command, the one being completed last.
    pub tokens: &'a [String],
    pub timeout: Duration,
}

impl GenCtx<'_> {
    /// The token being completed.
    pub fn current(&self) -> &str {
        self.tokens.last().map_or("", String::as_str)
    }

    /// Run a command in the shell's directory; stdout on success.
    pub fn run(&self, argv: &[&str]) -> Option<String> {
        let argv: Vec<String> = argv.iter().map(|s| s.to_string()).collect();
        let out = exec::run(Cmd::Argv(&argv), self.cwd, self.env, self.timeout);
        (out.status == 0).then_some(out.stdout)
    }

    /// Run a command; its non-empty output lines, trimmed.
    pub fn lines(&self, argv: &[&str]) -> Vec<String> {
        self.run(argv)
            .map(|s| s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_string).collect())
            .unwrap_or_default()
    }

    pub fn has_flag(&self, names: &[&str]) -> bool {
        self.tokens.iter().any(|t| names.contains(&t.as_str()))
    }

    /// The value given to one of these flags (`-n ns`, `--namespace=ns`).
    pub fn flag_value(&self, names: &[&str]) -> Option<String> {
        let toks = &self.tokens[..self.tokens.len().saturating_sub(1)];
        for (i, t) in toks.iter().enumerate() {
            if names.contains(&t.as_str())
                && let Some(v) = toks.get(i + 1)
            {
                return Some(v.clone());
            }
            if let Some((n, v)) = t.split_once('=')
                && names.contains(&n)
            {
                return Some(v.to_string());
            }
        }
        None
    }

    /// Positional (non-flag) tokens after the command, excluding the current one.
    pub fn positionals(&self) -> Vec<&str> {
        let toks = &self.tokens[..self.tokens.len().saturating_sub(1)];
        toks.iter().skip(1).filter(|t| !t.starts_with('-')).map(String::as_str).collect()
    }
}

/// A generator written in Rust (see `specs/gens.rs`).
pub type GenFn = dyn Fn(&GenCtx) -> Vec<SuggestionDef> + Send + Sync;

/// Where a generator's suggestions come from.
pub enum GenSource {
    /// A native generator, referenced from specs as `{"builtin": "<id>"}`.
    Rust(Box<GenFn>),
    /// Run a command (argv, or a shell string) and shape its output.
    Script {
        argv: Vec<String>,
        shell: Option<String>,
        output: Output,
    },
    Static(Vec<SuggestionDef>),
}

/// How command output becomes suggestions.
#[derive(Debug, Clone)]
pub enum Output {
    /// Split on `split_on`, drop `skip` header lines, optionally keep one
    /// whitespace-separated column.
    Lines { split_on: String, skip: usize, field: Option<usize> },
    /// Fill templates from JSON output.
    Json(JsonTemplate),
}

/// `name` / `description` are templates: literal text with JSON paths between
/// `\u{1}` markers, e.g. `"\u{1}$.apps[*].name\u{1}"`. Paths start at `$` and
/// use `.field`, `[N]`, `[*]` (array items), `{*}` (object values) and
/// `{key}` (object keys); all paths iterate the same collection.
#[derive(Debug, Clone)]
pub struct JsonTemplate {
    pub name: String,
    pub description: Option<String>,
    /// One JSON document per line.
    pub lines: bool,
}

/// Suggestions computed while typing (branches, containers, packages…).
pub struct Generator {
    /// Stable id, part of the result cache key.
    pub id: String,
    pub source: GenSource,
    pub trigger: Option<Trigger>,
    /// Match only the text after the last of these characters (`/`, `,`, `:`).
    pub query_term: Option<String>,
    pub cache_ttl: Option<Duration>,
    /// Match results against what's typed (off when the command already did).
    pub filter: bool,
}

impl std::fmt::Debug for Generator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Generator({})", self.id)
    }
}

impl Subcommand {
    pub fn find_subcommand(&self, name: &str) -> Option<&Arc<Subcommand>> {
        self.subcommands.iter().find(|s| s.meta.names.iter().any(|n| n == name))
    }

    pub fn directives(&self) -> Directives {
        self.directives.clone().unwrap_or_default()
    }
}

impl Opt {
    /// The option needs a value (`-o FILE`).
    pub fn takes_arg(&self) -> bool {
        self.args.first().is_some_and(|a| !a.is_optional)
    }
}

// ---------------------------------------------------------------- output shaping

impl Output {
    pub fn suggestions(&self, out: &str) -> Vec<SuggestionDef> {
        match self {
            Output::Lines { split_on, skip, field } => out
                .split(split_on.as_str())
                .skip(*skip)
                .filter_map(|l| match field {
                    Some(f) => l.split_whitespace().nth(*f),
                    None => Some(l.trim()),
                })
                .filter(|s| !s.is_empty())
                .map(sug)
                .collect(),
            Output::Json(t) => t.suggestions(out),
        }
    }
}

/// Delimits JSON paths inside a template (see [`JsonTemplate`]).
const TAG: char = '\u{1}';
/// Path parts that iterate a collection.
const MARKERS: [&str; 3] = ["[*]", "{*}", "{key}"];

/// `(start, end)` of the first iteration marker in a path.
fn marker(p: &str) -> Option<(usize, usize)> {
    MARKERS.iter().filter_map(|m| p.find(m).map(|i| (i, i + m.len()))).min()
}

/// Walk `$.a.b[0].c` from `v`.
fn json_get<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = v;
    let mut rest = path.strip_prefix('$').unwrap_or(path);
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('.') {
            let end = r.find(['.', '[']).unwrap_or(r.len());
            cur = cur.get(&r[..end])?;
            rest = &r[end..];
        } else {
            let r = rest.strip_prefix('[')?;
            let end = r.find(']')?;
            let idx = &r[..end];
            cur = if idx == "*" { cur.get(0)? } else { cur.get(idx.parse::<usize>().ok()?)? };
            rest = &r[end + 1..];
        }
    }
    Some(cur)
}

/// A scalar as text (anything else: empty).
fn json_text(v: Option<&Value>) -> String {
    match v {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => String::new(),
    }
}

impl JsonTemplate {
    /// Fill `t` for one `item` (with `key` for objects) of the collection at
    /// `base`; paths outside the collection are read from `root`.
    fn render(t: &str, root: &Value, base: &str, key: Option<&str>, item: &Value) -> String {
        let mut out = String::new();
        for (i, part) in t.split(TAG).enumerate() {
            if i % 2 == 0 {
                out.push_str(part);
                continue;
            }
            let text = if !base.is_empty() && part == format!("{base}{{key}}") {
                key.unwrap_or("").to_string()
            } else if let Some(rest) =
                MARKERS.iter().find_map(|m| part.strip_prefix(&format!("{base}{m}")).filter(|_| !base.is_empty()))
            {
                json_text(json_get(item, &format!("${rest}")))
            } else {
                json_text(json_get(root, part))
            };
            out.push_str(&text);
        }
        out.trim().to_string()
    }

    /// One suggestion per item of the iterated collection (one in all if no path iterates).
    fn items(&self, root: &Value, out: &mut Vec<SuggestionDef>) {
        let base = self
            .name
            .split(TAG)
            .skip(1)
            .step_by(2)
            .chain(self.description.iter().flat_map(|d| d.split(TAG).skip(1).step_by(2)))
            .find_map(|p| marker(p).map(|(s, _)| p[..s].to_string()))
            .unwrap_or_default();
        let mut push = |key: Option<&str>, item: &Value| {
            let name = Self::render(&self.name, root, &base, key, item);
            if name.is_empty() {
                return;
            }
            let mut s = sug(name);
            if let Some(d) = &self.description {
                s = s.desc(Self::render(d, root, &base, key, item));
            }
            out.push(s);
        };
        if base.is_empty() {
            push(None, root);
            return;
        }
        match json_get(root, &base) {
            Some(Value::Array(a)) => a.iter().for_each(|x| push(None, x)),
            Some(Value::Object(o)) => o.iter().for_each(|(k, x)| push(Some(k), x)),
            _ => {}
        }
    }

    pub fn suggestions(&self, out: &str) -> Vec<SuggestionDef> {
        let mut v = vec![];
        if self.lines {
            for line in out.lines().filter(|l| !l.trim().is_empty()) {
                if let Ok(root) = serde_json::from_str::<Value>(line) {
                    self.items(&root, &mut v);
                }
            }
        } else if let Ok(root) = serde_json::from_str::<Value>(out.trim()) {
            self.items(&root, &mut v);
        }
        v
    }
}

// ---------------------------------------------------------------- DSL

fn split_names(names: &str) -> Vec<String> {
    names.split(',').map(str::trim).filter(|s| !s.is_empty()).map(str::to_string).collect()
}

/// A (sub)command. `names` is comma separated: `"remove, rm"`.
pub fn cmd(names: &str, desc: &str) -> Subcommand {
    Subcommand {
        meta: Meta {
            names: split_names(names),
            description: (!desc.is_empty()).then(|| desc.into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// An option. `names` is comma separated: `"-v, --verbose"`.
pub fn opt(names: &str, desc: &str) -> Opt {
    Opt {
        meta: Meta {
            names: split_names(names),
            description: (!desc.is_empty()).then(|| desc.into()),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn arg(name: &str) -> Arg {
    Arg { name: name.into(), options_can_break_variadic: true, ..Default::default() }
}

pub fn sug(name: impl Into<String>) -> SuggestionDef {
    SuggestionDef { meta: Meta { names: vec![name.into()], ..Default::default() }, kind: None }
}

pub fn gen_fn(id: &str, f: impl Fn(&GenCtx) -> Vec<SuggestionDef> + Send + Sync + 'static) -> Generator {
    Generator {
        id: id.into(),
        source: GenSource::Rust(Box::new(f)),
        trigger: None,
        query_term: None,
        cache_ttl: None,
        filter: true,
    }
}

impl SuggestionDef {
    pub fn desc(mut self, d: impl Into<String>) -> Self {
        let d = d.into();
        self.meta.description = (!d.is_empty()).then_some(d);
        self
    }
    pub fn kind(mut self, k: Kind) -> Self {
        self.kind = Some(k);
        self
    }
    pub fn priority(mut self, p: f64) -> Self {
        self.meta.priority = Some(p);
        self
    }
    pub fn insert(mut self, v: impl Into<String>) -> Self {
        self.meta.insert_value = Some(v.into());
        self
    }
    pub fn icon(mut self, i: Icon) -> Self {
        self.meta.icon = Some(i);
        self
    }
}

impl Subcommand {
    pub fn sub(mut self, s: Subcommand) -> Self {
        self.subcommands.push(Arc::new(s));
        self
    }
    pub fn subs(mut self, s: impl IntoIterator<Item = Subcommand>) -> Self {
        self.subcommands.extend(s.into_iter().map(Arc::new));
        self
    }
    pub fn opt(mut self, o: Opt) -> Self {
        self.options.push(Arc::new(o));
        self
    }
    pub fn opts(mut self, o: impl IntoIterator<Item = Opt>) -> Self {
        self.options.extend(o.into_iter().map(Arc::new));
        self
    }
    pub fn arg(mut self, a: Arg) -> Self {
        self.args.push(Arc::new(a));
        self
    }
    pub fn hidden(mut self) -> Self {
        self.meta.hidden = true;
        self
    }
    pub fn priority(mut self, p: f64) -> Self {
        self.meta.priority = Some(p);
        self
    }
    pub fn posix_noncompliant(mut self) -> Self {
        self.directives.get_or_insert_with(Default::default).flags_are_posix_noncompliant = true;
        self
    }
    pub fn shortcut(mut self, s: SuggestionDef) -> Self {
        self.additional_suggestions.push(s.kind(Kind::Shortcut));
        self
    }
    pub fn help_from(mut self, argv: &[&str]) -> Self {
        self.load_spec = Some(LoadSpec::Help(argv.iter().map(|s| s.to_string()).collect()));
        self
    }
    pub fn load(mut self, name: &str) -> Self {
        self.load_spec = Some(LoadSpec::Name(name.into()));
        self
    }
}

impl Opt {
    pub fn arg(mut self, a: Arg) -> Self {
        self.args.push(Arc::new(a));
        self
    }
    pub fn persistent(mut self) -> Self {
        self.is_persistent = true;
        self
    }
    pub fn repeatable(mut self) -> Self {
        self.is_repeatable = true;
        self
    }
    /// The value is attached with `=` (`--color=auto`).
    pub fn eq(mut self) -> Self {
        self.separator = Some("=".into());
        self
    }
    pub fn exclusive(mut self, names: &[&str]) -> Self {
        self.exclusive_on = names.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn insert(mut self, v: &str) -> Self {
        self.meta.insert_value = Some(v.into());
        self
    }
    pub fn priority(mut self, p: f64) -> Self {
        self.meta.priority = Some(p);
        self
    }
}

impl Arg {
    pub fn optional(mut self) -> Self {
        self.is_optional = true;
        self
    }
    pub fn variadic(mut self) -> Self {
        self.is_variadic = true;
        self
    }
    pub fn files(mut self) -> Self {
        self.template.push(Template::Filepaths);
        self
    }
    pub fn folders(mut self) -> Self {
        self.template.push(Template::Folders);
        self
    }
    pub fn command(mut self) -> Self {
        self.is_command = true;
        self
    }
    pub fn command_names(mut self) -> Self {
        self.command_names = true;
        self
    }
    pub fn generator(mut self, g: Generator) -> Self {
        self.generators.push(Arc::new(g));
        self
    }
    pub fn suggest(mut self, items: &[&str]) -> Self {
        self.suggestions.extend(items.iter().map(|s| sug(*s)));
        self
    }
    /// `(name, description)` pairs.
    pub fn suggest_desc(mut self, items: &[(&str, &str)]) -> Self {
        self.suggestions.extend(items.iter().map(|(n, d)| sug(*n).desc(*d)));
        self
    }
}

impl Generator {
    pub fn trigger(mut self, t: Trigger) -> Self {
        self.trigger = Some(t);
        self
    }
    pub fn query_after(mut self, chars: &str) -> Self {
        self.query_term = Some(chars.into());
        self
    }
    pub fn unfiltered(mut self) -> Self {
        self.filter = false;
        self
    }
    pub fn ttl(mut self, ms: u64) -> Self {
        self.cache_ttl = Some(Duration::from_millis(ms));
        self
    }
}

// ---------------------------------------------------------------- spec → JSON

fn names_value(names: &[String]) -> Value {
    match names {
        [one] => Value::String(one.clone()),
        many => Value::Array(many.iter().cloned().map(Value::String).collect()),
    }
}

fn meta_into(m: &Meta, o: &mut serde_json::Map<String, Value>) {
    o.insert("name".into(), names_value(&m.names));
    if let Some(d) = &m.display_name {
        o.insert("displayName".into(), d.clone().into());
    }
    if let Some(d) = &m.description {
        o.insert("description".into(), d.clone().into());
    }
    if let Some(i) = &m.insert_value {
        o.insert("insertValue".into(), i.clone().into());
    }
    if let Some(p) = m.priority {
        o.insert("priority".into(), p.into());
    }
    if m.hidden {
        o.insert("hidden".into(), true.into());
    }
    if let Some(i) = m.icon {
        o.insert("icon".into(), i.name().into());
    }
}

fn suggestion_value(s: &SuggestionDef) -> Value {
    let plain = s.meta.description.is_none()
        && s.meta.insert_value.is_none()
        && s.meta.priority.is_none()
        && s.meta.icon.is_none()
        && s.meta.display_name.is_none()
        && s.kind.is_none()
        && s.meta.names.len() == 1;
    if plain {
        return Value::String(s.meta.names[0].clone());
    }
    let mut o = serde_json::Map::new();
    meta_into(&s.meta, &mut o);
    if let Some(k) = s.kind {
        let t = match k {
            Kind::File => "file",
            Kind::Folder => "folder",
            Kind::Subcommand => "subcommand",
            Kind::Option => "option",
            Kind::Shortcut => "shortcut",
            _ => "arg",
        };
        o.insert("type".into(), t.into());
    }
    Value::Object(o)
}

fn arg_value(a: &Arg) -> Value {
    let mut o = serde_json::Map::new();
    if !a.name.is_empty() {
        o.insert("name".into(), a.name.clone().into());
    }
    for (key, on) in [
        ("isOptional", a.is_optional),
        ("isVariadic", a.is_variadic),
        ("isCommand", a.is_command),
        ("commandNames", a.command_names),
    ] {
        if on {
            o.insert(key.into(), true.into());
        }
    }
    if !a.options_can_break_variadic {
        o.insert("optionsCanBreakVariadicArg".into(), false.into());
    }
    if !a.suggestions.is_empty() {
        o.insert("suggestions".into(), a.suggestions.iter().map(suggestion_value).collect());
    }
    if !a.template.is_empty() {
        let t: Vec<Value> = a
            .template
            .iter()
            .map(|t| match t {
                Template::Filepaths => "filepaths".into(),
                Template::Folders => "folders".into(),
            })
            .collect();
        o.insert("template".into(), if t.len() == 1 { t[0].clone() } else { Value::Array(t) });
    }
    if !a.generators.is_empty() {
        let gens: Vec<Value> = a.generators.iter().map(|g| serde_json::json!({ "builtin": g.id })).collect();
        o.insert("generators".into(), Value::Array(gens));
    }
    Value::Object(o)
}

fn args_value(args: &[Arc<Arg>]) -> Value {
    match args {
        [one] => arg_value(one),
        many => many.iter().map(|a| arg_value(a)).collect(),
    }
}

fn opt_value(o: &Opt) -> Value {
    let mut m = serde_json::Map::new();
    meta_into(&o.meta, &mut m);
    if !o.args.is_empty() {
        m.insert("args".into(), args_value(&o.args));
    }
    if o.is_persistent {
        m.insert("isPersistent".into(), true.into());
    }
    if o.is_repeatable {
        m.insert("isRepeatable".into(), true.into());
    }
    match o.separator.as_deref() {
        Some("=") => {
            m.insert("requiresSeparator".into(), true.into());
        }
        Some(sep) => {
            m.insert("requiresSeparator".into(), sep.into());
        }
        None => {}
    }
    if !o.exclusive_on.is_empty() {
        m.insert("exclusiveOn".into(), o.exclusive_on.clone().into());
    }
    Value::Object(m)
}

/// A spec as JSON in the same (Fig-style) format `from_value` reads.
pub fn to_value(s: &Subcommand) -> Value {
    let mut o = serde_json::Map::new();
    meta_into(&s.meta, &mut o);
    if !s.options.is_empty() {
        o.insert("options".into(), s.options.iter().map(|x| opt_value(x)).collect());
    }
    if !s.subcommands.is_empty() {
        o.insert("subcommands".into(), s.subcommands.iter().map(|x| to_value(x)).collect());
    }
    if !s.args.is_empty() {
        o.insert("args".into(), args_value(&s.args));
    }
    if !s.additional_suggestions.is_empty() {
        o.insert("additionalSuggestions".into(), s.additional_suggestions.iter().map(suggestion_value).collect());
    }
    match &s.load_spec {
        Some(LoadSpec::Name(n)) => {
            o.insert("loadSpec".into(), n.clone().into());
        }
        Some(LoadSpec::Help(argv)) => {
            o.insert("loadHelp".into(), argv.clone().into());
        }
        None => {}
    }
    if let Some(d) = &s.directives {
        let mut pd = serde_json::Map::new();
        if d.flags_are_posix_noncompliant {
            pd.insert("flagsArePosixNoncompliant".into(), true.into());
        }
        if !d.option_arg_separators.is_empty() {
            pd.insert("optionArgSeparators".into(), d.option_arg_separators.clone().into());
        }
        if !pd.is_empty() {
            o.insert("parserDirectives".into(), Value::Object(pd));
        }
    }
    if s.skip_fig_spec {
        o.insert("skipFigSpec".into(), true.into());
    }
    Value::Object(o)
}

// ---------------------------------------------------------------- user specs (JSON/TOML)

fn string(v: &Value, key: &str) -> Option<String> {
    v.get(key)?.as_str().map(str::to_string)
}

fn flag(v: &Value, key: &str) -> bool {
    v.get(key).and_then(Value::as_bool).unwrap_or(false)
}

fn one_or_many(v: Option<&Value>) -> Vec<&Value> {
    match v {
        Some(Value::Array(a)) => a.iter().collect(),
        Some(Value::Null) | None => vec![],
        Some(v) => vec![v],
    }
}

fn names(v: Option<&Value>) -> Vec<String> {
    one_or_many(v).into_iter().filter_map(|n| n.as_str().map(str::to_string)).collect()
}

fn meta(v: &Value) -> Meta {
    Meta {
        icon: v.get("icon").and_then(Value::as_str).and_then(Icon::from_name),
        names: names(v.get("name")),
        display_name: string(v, "displayName"),
        description: string(v, "description"),
        insert_value: string(v, "insertValue"),
        priority: v.get("priority").and_then(Value::as_f64),
        hidden: flag(v, "hidden"),
    }
}

fn kind_from_str(s: &str) -> Option<Kind> {
    Some(match s {
        "file" => Kind::File,
        "folder" => Kind::Folder,
        "subcommand" => Kind::Subcommand,
        "option" => Kind::Option,
        "arg" => Kind::Arg,
        "shortcut" | "special" => Kind::Shortcut,
        _ => return None,
    })
}

fn suggestion(v: &Value) -> Option<SuggestionDef> {
    match v {
        Value::String(s) => Some(sug(s.clone())),
        Value::Object(_) => {
            let meta = meta(v);
            (!meta.names.is_empty())
                .then(|| SuggestionDef { meta, kind: v.get("type").and_then(Value::as_str).and_then(kind_from_str) })
        }
        _ => None,
    }
}

fn templates(v: Option<&Value>) -> Vec<Template> {
    names(v)
        .iter()
        .filter_map(|t| match t.as_str() {
            "filepaths" => Some(Template::Filepaths),
            "folders" => Some(Template::Folders),
            _ => None,
        })
        .collect()
}

/// Build a spec from a JSON/TOML value in (a static subset of) Fig's format.
/// Generators may use `script` (a shell string or an argv array) and `splitOn`.
pub fn from_value(v: &Value) -> Subcommand {
    let directives = v.get("parserDirectives").map(|d| Directives {
        flags_are_posix_noncompliant: flag(d, "flagsArePosixNoncompliant"),
        option_arg_separators: names(d.get("optionArgSeparators")),
    });
    Subcommand {
        meta: meta(v),
        subcommands: one_or_many(v.get("subcommands")).into_iter().map(|s| Arc::new(from_value(s))).collect(),
        options: one_or_many(v.get("options")).into_iter().map(|o| Arc::new(opt_from(o))).collect(),
        args: one_or_many(v.get("args")).into_iter().map(|a| Arc::new(arg_from(a))).collect(),
        additional_suggestions: one_or_many(v.get("additionalSuggestions"))
            .into_iter()
            .filter_map(suggestion)
            .collect(),
        load_spec: string(v, "loadSpec").map(LoadSpec::Name).or_else(|| {
            let argv = names(v.get("loadHelp"));
            (!argv.is_empty()).then_some(LoadSpec::Help(argv))
        }),
        directives,
        skip_fig_spec: flag(v, "skipFigSpec"),
    }
}

fn opt_from(v: &Value) -> Opt {
    let separator = match (v.get("requiresSeparator"), v.get("requiresEquals")) {
        (Some(Value::String(s)), _) => Some(s.clone()),
        (Some(Value::Bool(true)), _) | (_, Some(Value::Bool(true))) => Some("=".into()),
        _ => None,
    };
    Opt {
        meta: meta(v),
        args: one_or_many(v.get("args")).into_iter().map(|a| Arc::new(arg_from(a))).collect(),
        is_persistent: flag(v, "isPersistent"),
        is_repeatable: matches!(v.get("isRepeatable"), Some(Value::Bool(true) | Value::Number(_))),
        separator,
        exclusive_on: names(v.get("exclusiveOn")),
    }
}

fn arg_from(v: &Value) -> Arg {
    Arg {
        name: string(v, "name").unwrap_or_default(),
        is_optional: flag(v, "isOptional"),
        is_variadic: flag(v, "isVariadic"),
        options_can_break_variadic: v.get("optionsCanBreakVariadicArg").and_then(Value::as_bool).unwrap_or(true),
        is_command: flag(v, "isCommand"),
        command_names: flag(v, "commandNames"),
        suggestions: one_or_many(v.get("suggestions")).into_iter().filter_map(suggestion).collect(),
        template: templates(v.get("template"))
            .into_iter()
            .chain(one_or_many(v.get("generators")).into_iter().flat_map(|g| templates(g.get("template"))))
            .collect(),
        generators: one_or_many(v.get("generators")).into_iter().filter_map(generator_from).map(Arc::new).collect(),
    }
}

fn generator_from(v: &Value) -> Option<Generator> {
    if let Some(id) = v.get("builtin").and_then(Value::as_str) {
        return crate::specs::gens::builtin(id);
    }
    let (source, id) = if let Some(list) = v.get("suggestions") {
        let list = one_or_many(Some(list)).into_iter().filter_map(suggestion).collect();
        (GenSource::Static(list), format!("static:{}", fnv(&list_key(v))))
    } else {
        let (argv, shell) = match v.get("script")? {
            Value::String(s) => (vec![], Some(s.clone())),
            Value::Array(a) => (a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect(), None),
            _ => return None,
        };
        let output = match v.get("json") {
            Some(j) => Output::Json(JsonTemplate {
                name: string(j, "name")?,
                description: string(j, "description"),
                lines: flag(j, "lines"),
            }),
            None => Output::Lines {
                split_on: string(v, "splitOn").unwrap_or_else(|| "\n".into()),
                skip: v.get("skipLines").and_then(Value::as_u64).unwrap_or(0) as usize,
                field: v.get("field").and_then(Value::as_u64).map(|f| f as usize),
            },
        };
        (GenSource::Script { argv, shell, output }, format!("script:{:x}", fnv(&list_key(v))))
    };
    Some(Generator {
        id,
        source,
        trigger: v.get("trigger").and_then(Value::as_str).map(|s| Trigger::Chars(s.into())),
        query_term: string(v, "getQueryTerm"),
        cache_ttl: v.get("cache").and_then(|c| c.get("ttl")).and_then(Value::as_u64).map(Duration::from_millis),
        filter: true,
    })
}

fn list_key(v: &Value) -> String {
    v.to_string()
}

/// FNV-1a: stable ids for generators from files.
fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: Vec<SuggestionDef>) -> Vec<(String, Option<String>)> {
        v.into_iter().map(|s| (s.meta.names[0].clone(), s.meta.description)).collect()
    }

    #[test]
    fn json_templates() {
        let t = |name: &str, desc: Option<&str>, lines| JsonTemplate {
            name: name.into(),
            description: desc.map(Into::into),
            lines,
        };
        let out = r#"{"Buckets":[{"Name":"a","Region":"eu"},{"Name":"b","Region":"us"}],"Owner":{"ID":"me"}}"#;
        let r =
            t("\u{1}$.Buckets[*].Name\u{1}", Some("in \u{1}$.Buckets[*].Region\u{1} of \u{1}$.Owner.ID\u{1}"), false)
                .suggestions(out);
        assert_eq!(names(r), [("a".into(), Some("in eu of me".into())), ("b".into(), Some("in us of me".into()))]);
        assert_eq!(
            names(t("\u{1}$.ids[*]\u{1}", None, false).suggestions(r#"{"ids":["x",2]}"#)),
            [("x".into(), None), ("2".into(), None)]
        );
        let r = t("\u{1}${key}\u{1}", Some("\u{1}${*}\u{1}"), false).suggestions(r#"{"build":"tsc","test":"jest"}"#);
        assert_eq!(names(r), [("build".into(), Some("tsc".into())), ("test".into(), Some("jest".into()))]);
        let r = t("\u{1}$.Names\u{1}", None, true).suggestions("{\"Names\":\"web\"}\n{\"Names\":\"db\"}\n");
        assert_eq!(names(r), [("web".into(), None), ("db".into(), None)]);
    }

    #[test]
    fn line_outputs() {
        let o = Output::Lines { split_on: "\n".into(), skip: 1, field: Some(0) };
        assert_eq!(
            names(o.suggestions("NAME  STATUS\nweb   up\ndb    down\n")),
            [("web".into(), None), ("db".into(), None)]
        );
    }

    #[test]
    fn generator_kinds_from_json() {
        let a = arg_from(&serde_json::json!({"name": "x", "generators": [
            {"template": "folders"},
            {"builtin": "git:branches"},
            {"suggestions": ["one", {"name": "two"}]},
            {"script": ["echo", "hi"], "json": {"name": "\u{1}$\u{1}"}},
            {"builtin": "no-such-generator"},
        ]}));
        assert_eq!(a.template, [Template::Folders]);
        assert_eq!(a.generators.len(), 3);
        assert!(matches!(&a.generators[1].source, GenSource::Static(l) if l.len() == 2));
    }
}
