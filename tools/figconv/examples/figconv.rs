//! figconv: convert the withfig/autocomplete specs into lig's JSON spec format.
//!
//! This is a development tool; lig itself contains no JavaScript engine.
//! Input: the TypeScript sources compiled to self-contained IIFE bundles
//! (see `tools/update-fig-specs.sh`). Each spec is evaluated once here, in
//! QuickJS, and written out as static JSON:
//!
//! * functions are removed;
//! * generators become `{"builtin": "<id>"}` when they run a command lig has a
//!   native Rust generator for (git branches, docker containers, …),
//!   `{"script": …}` when their post-processing is verified to be "one
//!   suggestion per line", `{"template": …}` for file listings, and are
//!   dropped otherwise;
//! * descriptions are shortened to their first line;
//! * versioned specs point at their newest version.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow, bail};
use rquickjs::{Context, Ctx, Function, Runtime};
use serde_json::{Map, Value, json};

const PRELUDE: &str = r#"
globalThis.console = { log() {}, warn() {}, error() {}, info() {}, debug() {} };
globalThis.process = { env: {}, platform: "linux", argv: [], cwd() { return "/home/user"; } };
globalThis.Buffer = { from(s) { return { toString() { return String(s); } }; } };
globalThis.require = function (m) {
  if (m === "process") return globalThis.process;
  if (m === "buffer") return { Buffer: globalThis.Buffer };
  return {};
};
globalThis.setTimeout = function (f) { f(); return 0; };
globalThis.__fns = [];
globalThis.__ser = function (v) {
  return JSON.stringify(v === undefined ? null : v, function (k, x) {
    if (typeof x === "function") { __fns.push(x); return { __fn: __fns.length - 1 }; }
    if (x instanceof RegExp) return String(x);
    return x;
  });
};
globalThis.__src = function (id) { return String(__fns[id]); };
globalThis.__record = [];
globalThis.__fake = "";
globalThis.__exec = function (x) {
  __record.push(x);
  return Promise.resolve(typeof x === "string" ? __fake : { stdout: __fake, stderr: "", status: 0, exitCode: 0 });
};
// JSON probe: JSON.parse returns a proxy whose property paths come back as
// tagged strings ("\u0001$[*].name\u0001") in whatever the function builds.
const __T = "\u0001";
const __realParse = JSON.parse;
const __STR = Object.getOwnPropertyNames(String.prototype);
const __ARR = Object.getOwnPropertyNames(Array.prototype);
function __tag(p) { return __T + p + __T; }
function __mk(path) {
  const keyMarker = __tag(path + "{key}");
  return new Proxy([], {
    get(t, k) {
      if (k === Symbol.toPrimitive) return function () { return __tag(path); };
      if (k === Symbol.iterator) { const it = [__mk(path + "[*]")]; return function () { return it[Symbol.iterator](); }; }
      if (typeof k === "symbol") return undefined;
      if (k === "toString" || k === "valueOf" || k === "toJSON") return function () { return __tag(path); };
      if (k === "then") return undefined;
      if (k === "length") return 1;
      if (k === keyMarker) return __mk(path + "{*}");
      if (/^[0-9]+$/.test(k)) return __mk(path + "[" + k + "]");
      if (__ARR.includes(k)) return function (...a) { return Array.prototype[k].apply([__mk(path + "[*]")], a); };
      if (__STR.includes(k)) return function (...a) { return String.prototype[k].apply(__tag(path), a); };
      return __mk(path + "." + k);
    },
    ownKeys(t) { return ["length", keyMarker]; },
    getOwnPropertyDescriptor(t, k) {
      if (k === "length") return Reflect.getOwnPropertyDescriptor(t, "length");
      return { value: __mk(path + "{*}"), enumerable: true, configurable: true, writable: true };
    },
    has() { return true; },
  });
}
globalThis.__jsonProbe = function (on) { JSON.parse = on ? function () { return __mk("$"); } : __realParse; };
globalThis.__call = function (id, args) {
  globalThis.__done = false; globalThis.__out = null; globalThis.__err = null; globalThis.__record = [];
  const a = __realParse(args).map(function (x) { return x === "\u0000EXEC" ? __exec : x; });
  Promise.resolve()
    .then(function () { return __fns[id].apply(null, a); })
    .then(function (v) { __out = __ser(v); __done = true; },
          function (e) { __err = String(e); __done = true; });
};
"#;

const EXEC: &str = "\u{0}EXEC";

struct Js {
    rt: Runtime,
    ctx: Context,
}

fn js_err(ctx: &Ctx<'_>, e: rquickjs::Error) -> anyhow::Error {
    if matches!(e, rquickjs::Error::Exception) {
        let v = ctx.catch();
        if let Some(ex) = v.as_exception() {
            return anyhow!("{}", ex.message().unwrap_or_default());
        }
        return anyhow!("{v:?}");
    }
    anyhow!("{e}")
}

impl Js {
    fn new() -> Result<Js> {
        let rt = Runtime::new()?;
        rt.set_memory_limit(1 << 30);
        let ctx = Context::full(&rt)?;
        ctx.with(|ctx| ctx.eval::<(), _>(PRELUDE).map_err(|e| js_err(&ctx, e)))?;
        Ok(Js { rt, ctx })
    }

    fn load(&self, src: &str) -> Result<Value> {
        let json = self.ctx.with(|ctx| -> Result<String> {
            ctx.eval::<(), _>(src).map_err(|e| js_err(&ctx, e))?;
            ctx.eval::<String, _>("__ser(__lig_module)").map_err(|e| js_err(&ctx, e))
        })?;
        Ok(serde_json::from_str(&json)?)
    }

    fn source(&self, f: u64) -> String {
        self.ctx.with(|ctx| {
            let src: Function = ctx.globals().get("__src").unwrap();
            src.call::<_, String>((f,)).unwrap_or_default()
        })
    }

    /// Call a spec function; `fake` is what `executeShellCommand` returns.
    /// Returns the result and the commands it tried to run.
    fn call(&self, f: u64, args: &[Value], fake: &str) -> Result<(Value, Vec<Value>)> {
        self.call_probe(f, args, fake, false)
    }

    /// Like `call`, optionally with `JSON.parse` replaced by the probe.
    fn call_probe(&self, f: u64, args: &[Value], fake: &str, json_probe: bool) -> Result<(Value, Vec<Value>)> {
        let args = serde_json::to_string(args)?;
        self.ctx.with(|ctx| -> Result<()> {
            ctx.globals().set("__fake", fake)?;
            let probe: Function = ctx.globals().get("__jsonProbe")?;
            probe.call::<_, ()>((json_probe,)).map_err(|e| js_err(&ctx, e))?;
            let call: Function = ctx.globals().get("__call")?;
            call.call::<_, ()>((f, args)).map_err(|e| js_err(&ctx, e))
        })?;
        for _ in 0..100_000 {
            match self.rt.execute_pending_job() {
                Ok(true) | Err(_) => continue,
                Ok(false) => break,
            }
        }
        self.ctx.with(|ctx| -> Result<(Value, Vec<Value>)> {
            let g = ctx.globals();
            let probe: Function = g.get("__jsonProbe")?;
            probe.call::<_, ()>((false,)).map_err(|e| js_err(&ctx, e))?;
            if !g.get::<_, bool>("__done")? {
                bail!("did not settle");
            }
            if let Ok(err) = g.get::<_, String>("__err") {
                bail!("threw: {err}");
            }
            let out: String = g.get("__out")?;
            let rec: String = ctx.eval("JSON.stringify(__record)").map_err(|e| js_err(&ctx, e))?;
            Ok((serde_json::from_str(&out)?, serde_json::from_str(&rec)?))
        })
    }
}

fn fn_id(v: &Value) -> Option<u64> {
    v.get("__fn")?.as_u64()
}

/// Native generators in lig (`src/specs/gens.rs`), matched by the command a
/// Fig generator runs.
fn builtin_for(cmd: &str, post_src: &str) -> Option<String> {
    let c = cmd.trim();
    // aws & co: `file://` + local paths.
    if let Some(prefix) = c.strip_prefix("echo ").filter(|p| p.ends_with("://")) {
        return Some(format!("files:{prefix}"));
    }
    let has = |s: &str| c.contains(s);
    let rules: &[(bool, &str)] = &[
        (has("git") && has("stash list"), "git:stashes"),
        (has("git") && (has("for-each-ref") || has(" branch")), "git:branches"),
        (has("git") && has(" tag"), "git:tags"),
        (has("git") && has(" remote"), "git:remotes"),
        (has("git") && (has(" log") || has("rev-list")), "git:commits"),
        (has("git") && (has(" status") || has("diff --name-only") || has("ls-files -m")), "git:unstaged"),
        (has("docker") && (has("ps -a") || has("container ls -a") || has("ps --all")), "docker:containers"),
        (has("docker") && (has(" ps") || has("container ls")), "docker:running"),
        (has("docker") && (has(" images") || has("image ls")), "docker:images"),
        (has("docker") && has("network ls"), "docker:networks"),
        (has("docker") && has("volume ls"), "docker:volumes"),
        (has("compose") && has("--services"), "docker:compose-services"),
        (has("systemctl") && has("list-unit"), "systemd:units"),
        (c.starts_with("ps ") || c == "ps", "sys:pids"),
        (has("tmux") && (has("list-sessions") || has(" ls")), "tmux:sessions"),
        (has("kubectl") && has("get-contexts"), "kube:contexts"),
        (has("kubectl") && has("get namespace"), "kube:namespaces"),
        (has("kubectl") && (has("get pods") || has("get pod")), "kube:pods"),
        (has("make") && (has("-qp") || has("-pRrq")), "make:targets"),
        (has("package.json") && post_src.contains("scripts"), "npm:scripts"),
        (has("package.json") && post_src.contains("ependencies"), "npm:deps"),
        (has("rustup") && has("toolchain list"), "rustup:toolchains"),
        (has("dpkg") || has("apt list --installed") || has("apt-mark"), "apt:installed"),
        (has(".ssh/config") || has("known_hosts"), "ssh:hosts"),
        ((has("pip") && (has(" list") || has(" freeze"))), "pip:installed"),
        (has("cargo metadata"), "cargo:packages"),
        (has("cargo install --list"), "cargo:installed"),
    ];
    rules.iter().find(|(m, _)| *m).map(|(_, id)| id.to_string())
}

/// Fig icons (`fig://icon?type=git`, emoji) → lig icon names.
fn icon_name(fig: &str) -> Option<&'static str> {
    let kind = fig.strip_prefix("fig://icon?type=").map(|t| t.split('&').next().unwrap_or(t));
    Some(match (kind, fig) {
        (Some("npm" | "yarn" | "node"), _) => "npm",
        (Some("package" | "box"), _) | (_, "📦" | "🍺") => "package",
        (Some("docker"), _) | (_, "🐳") => "docker",
        (Some("git"), _) | (_, "🌿" | "🌱") => "git-branch",
        (Some("aws" | "kubernetes" | "gcloud" | "azure" | "heroku" | "netlify" | "vercel"), _) | (_, "🌐") => "cloud",
        (Some("folder"), _) | (_, "📁" | "📂") => "folder",
        (Some("user"), _) | (_, "👤") => "user",
        (Some("python"), _) | (_, "🐍") => "python",
        (Some("rust"), _) | (_, "🦀") => "rust",
        (Some("command" | "commandkey"), _) | (_, "💻") => "terminal",
        (_, "🏷" | "🏷️") => "git-tag",
        _ => return None,
    })
}

fn cmdline(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Array(a) => Some(a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")),
        Value::Object(o) => {
            let mut s = o.get("command")?.as_str()?.to_string();
            for a in o.get("args").and_then(Value::as_array).into_iter().flatten() {
                s.push(' ');
                s.push_str(a.as_str().unwrap_or(""));
            }
            Some(s)
        }
        _ => None,
    }
}

/// `{script}` value for lig: a shell string or an argv array.
fn script_value(v: &Value) -> Option<Value> {
    match v {
        Value::String(_) => Some(v.clone()),
        Value::Array(a) if a.iter().all(Value::is_string) => Some(v.clone()),
        Value::Object(o) => {
            let mut argv = vec![o.get("command")?.clone()];
            argv.extend(o.get("args").and_then(Value::as_array).into_iter().flatten().cloned());
            Some(Value::Array(argv))
        }
        _ => None,
    }
}

fn names_of(v: &Value) -> Vec<String> {
    v.as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| match s {
            Value::String(s) => Some(s.clone()),
            Value::Object(o) => match o.get("name")? {
                Value::String(n) => Some(n.clone()),
                Value::Array(a) => a.first()?.as_str().map(str::to_string),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

fn short(d: &str) -> String {
    let first = d.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if first.chars().count() <= 160 {
        return first.to_string();
    }
    let cut: String = first.chars().take(160).collect();
    match cut.rfind(". ") {
        Some(i) if i > 40 => cut[..=i].to_string(),
        _ => format!("{}…", cut.trim_end()),
    }
}

#[derive(Default)]
struct Stats {
    specs: usize,
    generators: usize,
    builtin: BTreeMap<String, usize>,
    script: usize,
    columns: usize,
    json: usize,
    static_: usize,
    template: usize,
    dropped: usize,
    dropped_cmds: BTreeMap<String, usize>,
    errors: Vec<String>,
}

struct Conv<'a> {
    js: &'a Js,
    stats: &'a mut Stats,
}

const DROP_KEYS: &[&str] = &[
    "generateSpec",
    "filterStrategy",
    "isDangerous",
    "requiresSubcommand",
    "previewComponent",
    "isModule",
    "isScript",
    "suggestCurrentToken",
    "deprecated",
    "debounce",
    "isSticky",
];

impl Conv<'_> {
    fn clean(&mut self, v: Value) -> Option<Value> {
        match v {
            Value::Object(o) if o.contains_key("__fn") => None,
            Value::Object(o) => {
                let mut out = Map::new();
                let mut inline: Option<Map<String, Value>> = None;
                for (k, val) in o {
                    match k.as_str() {
                        "generators" => {
                            let list: Vec<Value> = match val {
                                Value::Array(a) => a,
                                v => vec![v],
                            };
                            let gens: Vec<Value> = list.into_iter().filter_map(|g| self.generator(g)).collect();
                            if !gens.is_empty() {
                                out.insert(k, Value::Array(gens));
                            }
                        }
                        "loadSpec" => match val {
                            Value::String(_) => {
                                out.insert(k, val);
                            }
                            Value::Object(ref m) if !m.contains_key("__fn") => {
                                if let Some(Value::Object(c)) = self.clean(val) {
                                    inline = Some(c);
                                }
                            }
                            _ => {}
                        },
                        "icon" => {
                            if let Some(i) = val.as_str().and_then(icon_name) {
                                out.insert(k, Value::String(i.into()));
                            }
                        }
                        "description" => {
                            if let Some(d) = val.as_str() {
                                let d = short(d);
                                if !d.is_empty() {
                                    out.insert(k, Value::String(d));
                                }
                            }
                        }
                        "parserDirectives" => {
                            let mut pd = Map::new();
                            for key in ["flagsArePosixNoncompliant", "optionArgSeparators"] {
                                if let Some(x) = val.get(key).filter(|x| !x.is_object()) {
                                    pd.insert(key.into(), x.clone());
                                }
                            }
                            if !pd.is_empty() {
                                out.insert(k, Value::Object(pd));
                            }
                        }
                        "requiresEquals" => {
                            if val == Value::Bool(true) {
                                out.insert("requiresSeparator".into(), Value::Bool(true));
                            }
                        }
                        _ if DROP_KEYS.contains(&k.as_str()) => {}
                        _ => {
                            if let Some(c) = self.clean(val) {
                                out.insert(k, c);
                            }
                        }
                    }
                }
                // An inline `loadSpec: {…}` contributes its structure.
                if let Some(inl) = inline {
                    for (k, v) in inl {
                        if matches!(k.as_str(), "subcommands" | "options" | "args" | "additionalSuggestions") {
                            out.entry(k).or_insert(v);
                        }
                    }
                }
                Some(Value::Object(out))
            }
            Value::Array(a) => Some(Value::Array(a.into_iter().filter_map(|x| self.clean(x)).collect())),
            other => Some(other),
        }
    }

    fn generator(&mut self, g: Value) -> Option<Value> {
        self.stats.generators += 1;
        let o = g.as_object()?;
        let mut out = Map::new();
        for key in ["trigger", "getQueryTerm", "splitOn"] {
            if let Some(Value::String(s)) = o.get(key) {
                out.insert(key.into(), Value::String(s.clone()));
            }
        }
        if let Some(ttl) = o.get("cache").and_then(|c| c.get("ttl")).filter(|t| t.is_number()) {
            out.insert("cache".into(), json!({"ttl": ttl}));
        }
        if let Some(t) = o.get("template") {
            self.stats.template += 1;
            out.insert("template".into(), t.clone());
            return Some(Value::Object(out));
        }
        let js = self.js;
        let tokens = json!(["cmd", ""]);
        let post = o.get("postProcess").and_then(fn_id);
        let mut src = post.map(|p| js.source(p)).unwrap_or_default();

        // `custom: async (tokens, executeShellCommand, context) => …`:
        // run it against a recording executeShellCommand.
        if let Some(c) = o.get("custom").and_then(fn_id) {
            src.push_str(&js.source(c));
            let gctx = json!({"currentWorkingDirectory": "/home/user", "searchTerm": "", "environmentVariables": {}});
            let args = [tokens.clone(), Value::from(EXEC), gctx.clone()];
            let (first, rec) = js.call(c, &args, "").unwrap_or((Value::Null, vec![]));
            if rec.is_empty() {
                // No command: a token-independent list is just static suggestions.
                let other = js.call(c, &[json!(["cmd", "sub", "x", ""]), Value::from(EXEC), gctx], "").map(|r| r.0);
                if !names_of(&first).is_empty() && other.is_ok_and(|o| o == first) {
                    self.stats.static_ += 1;
                    let list = self.clean(first)?;
                    return Some(json!({"suggestions": list}));
                }
                return self.drop(None);
            }
            let cmd = rec.first().and_then(cmdline);
            let run = |fake: &str, probe: bool| {
                js.call_probe(c, &args, fake, probe).ok().filter(|(_, r)| r.len() == 1).map(|(v, _)| v)
            };
            let shape = if rec.len() == 1 { infer(&run) } else { None };
            return self.finish(out, &src, cmd, rec.first().and_then(script_value), shape);
        }

        let script = match o.get("script")? {
            s @ (Value::String(_) | Value::Array(_)) => Some(s.clone()),
            f @ Value::Object(_) => {
                // `script: (tokens) => …` — usable when it doesn't depend on the tokens.
                let f = fn_id(f)?;
                let a = js.call(f, std::slice::from_ref(&tokens), "").ok().map(|r| r.0);
                let b = js.call(f, &[json!(["cmd", "sub", "x", ""])], "").ok().map(|r| r.0);
                match (a, b) {
                    (Some(a), Some(b)) if a == b => Some(a),
                    (Some(a), _) => return self.finish(out, &src, cmdline(&a), None, None),
                    _ => None,
                }
            }
            _ => None,
        };
        let cmd = script.as_ref().and_then(cmdline);
        let shape = match post {
            None => Some(Shape::Lines),
            Some(p) => infer(&|fake: &str, probe: bool| {
                js.call_probe(p, &[Value::from(fake), tokens.clone()], "", probe).ok().map(|(v, _)| v)
            }),
        };
        self.finish(out, &src, cmd, script.and_then(|s| script_value(&s)), shape)
    }

    fn drop(&mut self, cmd: Option<String>) -> Option<Value> {
        self.stats.dropped += 1;
        *self.stats.dropped_cmds.entry(cmd.unwrap_or_else(|| "<no command>".into())).or_default() += 1;
        None
    }

    /// Decide what a generator becomes in lig.
    fn finish(
        &mut self,
        mut out: Map<String, Value>,
        src: &str,
        cmd: Option<String>,
        script: Option<Value>,
        shape: Option<Shape>,
    ) -> Option<Value> {
        if let Some(c) = &cmd {
            if let Some(id) = builtin_for(c, src) {
                *self.stats.builtin.entry(id.clone()).or_default() += 1;
                out.insert("builtin".into(), Value::String(id));
                return Some(Value::Object(out));
            }
            if c.starts_with("ls ") || c == "ls" {
                self.stats.template += 1;
                return Some(json!({"template": "filepaths"}));
            }
        }
        let (Some(script), Some(shape)) = (script, shape) else { return self.drop(cmd) };
        out.insert("script".into(), script);
        match shape {
            Shape::Lines => self.stats.script += 1,
            Shape::Field { skip, field } => {
                self.stats.columns += 1;
                out.insert("skipLines".into(), json!(skip));
                out.insert("field".into(), json!(field));
            }
            Shape::Json { name, description, lines } => {
                self.stats.json += 1;
                let mut j = Map::new();
                if lines {
                    j.insert("lines".into(), Value::Bool(true));
                }
                j.insert("name".into(), Value::String(name));
                if let Some(d) = description {
                    j.insert("description".into(), Value::String(d));
                }
                out.insert("json".into(), Value::Object(j));
            }
        }
        Some(Value::Object(out))
    }
}

/// How a generator turns command output into suggestions.
enum Shape {
    /// One suggestion per line.
    Lines,
    /// Whitespace-separated column `field`, after `skip` header lines.
    Field { skip: usize, field: usize },
    /// Templates over the parsed JSON (see `json_shape`).
    Json { name: String, description: Option<String>, lines: bool },
}

/// Text produced by doing arithmetic/dates on the probe proxies.
fn probe_junk(s: &str) -> bool {
    ["Invalid Date", "NaN", "undefined", "[object", "null"].iter().any(|j| s.contains(j))
}

/// A JSON-lines item: templates over `$` without iteration.
fn json_line_shape(item: &Value) -> Option<(String, Option<String>)> {
    let (name, desc) = match item {
        Value::String(s) => (s.clone(), None),
        Value::Object(o) => (
            o.get("name")?.as_str()?.to_string(),
            o.get("description").and_then(Value::as_str).filter(|d| !probe_junk(d)).map(str::to_string),
        ),
        _ => return None,
    };
    let paths = tag_paths(&name);
    let all: Vec<&str> = paths.iter().copied().chain(desc.as_deref().map(tag_paths).unwrap_or_default()).collect();
    if paths.is_empty() || all.iter().any(|p| iteration_base(p).is_some()) {
        return None;
    }
    Some((name, desc))
}

/// Probe a post-processing function with synthetic outputs.
fn infer(run: &dyn Fn(&str, bool) -> Option<Value>) -> Option<Shape> {
    let names = |fake: &str| run(fake, false).map(|v| names_of(&v)).unwrap_or_default();
    if names("alpha\nbeta") == ["alpha", "beta"] {
        return Some(Shape::Lines);
    }
    if names("alpha one\nbeta two") == ["alpha", "beta"] {
        return Some(Shape::Field { skip: 0, field: 0 });
    }
    if names("NAME VALUE\nalpha one\nbeta two") == ["alpha", "beta"] {
        return Some(Shape::Field { skip: 1, field: 0 });
    }
    if names("one alpha\ntwo beta") == ["alpha", "beta"] {
        return Some(Shape::Field { skip: 0, field: 1 });
    }
    if names("NAME VALUE\none alpha\ntwo beta") == ["alpha", "beta"] {
        return Some(Shape::Field { skip: 1, field: 1 });
    }
    if let Some((name, description)) = run("{}", true).as_ref().and_then(json_shape) {
        return Some(Shape::Json { name, description, lines: false });
    }
    // One JSON document per line (`--format '{{json .}}'`).
    let v = run("{}\n{}", true)?;
    if v.as_array().is_some_and(|a| a.len() == 2) {
        let (name, description) = json_line_shape(&v[0])?;
        return Some(Shape::Json { name, description, lines: true });
    }
    None
}

const TAG: char = '\u{1}';

/// The tagged paths in a template string.
fn tag_paths(t: &str) -> Vec<&str> {
    t.split(TAG).skip(1).step_by(2).collect()
}

/// Where a path iterates (`$.apps[*]…` → `$.apps[*]`), if it does.
fn iteration_base(p: &str) -> Option<&str> {
    let i = [p.find("[*]").map(|i| i + 3), p.find("{*}").map(|i| i + 3), p.find("{key}").map(|i| i + 5)]
        .into_iter()
        .flatten()
        .min()?;
    Some(&p[..i])
}

/// From the probe result, the name/description templates of one item.
fn json_shape(v: &Value) -> Option<(String, Option<String>)> {
    let item = v.as_array()?.first()?;
    let (name, desc) = match item {
        Value::String(s) => (s.clone(), None),
        Value::Object(o) => {
            let name = match o.get("name")? {
                Value::String(s) => s.clone(),
                Value::Array(a) => a.first()?.as_str()?.to_string(),
                _ => return None,
            };
            (name, o.get("description").and_then(Value::as_str).map(str::to_string))
        }
        _ => return None,
    };
    let paths = tag_paths(&name);
    if paths.is_empty() || name.matches(TAG).count() % 2 != 0 {
        return None;
    }
    // Everything must iterate over the same collection, at most once.
    let base = iteration_base(paths[0])?;
    let all: Vec<&str> = paths.iter().copied().chain(desc.as_deref().map(tag_paths).unwrap_or_default()).collect();
    for p in &all {
        if let Some(b) = iteration_base(p) {
            let tail = &p[b.len()..];
            let markers = ["[*]", "{*}", "{key}"];
            let strip = |x: &str| markers.iter().find_map(|m| x.strip_suffix(m)).unwrap_or(x).to_string();
            if strip(b) != strip(base) || markers.iter().any(|m| tail.contains(m)) {
                return None;
            }
        }
    }
    let desc = desc.filter(|d| d.matches(TAG).count() % 2 == 0 && !probe_junk(d));
    Some((name, desc))
}

fn convert_file(path: &Path, rel: &str, stats: &mut Stats) -> Result<Option<Value>> {
    let src = std::fs::read_to_string(path)?;
    let js = Js::new()?;
    let module = js.load(&src)?;
    let Some(def) = module.get("default") else { return Ok(None) };
    let mut conv = Conv { js: &js, stats };
    if let Some(f) = fn_id(def) {
        // Versioned spec: `default(version)` → `{versionedSpecPath}`; take the newest.
        if module.get("getVersionCommand").is_none() {
            return Ok(None);
        }
        let (res, _) = js.call(f, &[Value::Null], "")?;
        let target = res.get("versionedSpecPath").and_then(Value::as_str).context("no versionedSpecPath")?;
        let name = rel.trim_end_matches("/index");
        return Ok(Some(json!({"name": name.rsplit('/').next().unwrap_or(name), "loadSpec": target})));
    }
    if !def.is_object() || def.get("name").is_none() {
        return Ok(None);
    }
    Ok(conv.clean(def.clone()))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        if p.is_dir() {
            walk(&p, out);
        } else if p.extension().is_some_and(|x| x == "js") {
            out.push(p);
        }
    }
}

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let (Some(input), Some(output)) = (args.get(1), args.get(2)) else {
        bail!("usage: figconv <compiled-specs-dir> <output-dir>");
    };
    let (input, output) = (Path::new(input), Path::new(output));
    let mut files = vec![];
    walk(input, &mut files);
    files.sort();
    let mut stats = Stats::default();
    let mut bytes = 0usize;
    for path in &files {
        let rel = path.strip_prefix(input)?.with_extension("").to_string_lossy().into_owned();
        match convert_file(path, &rel, &mut stats) {
            Ok(Some(v)) => {
                // `heroku/index` (a versioned spec's entry point) is the `heroku` spec.
                let name = match rel.strip_suffix("/index") {
                    Some(base) if !input.join(format!("{base}.js")).exists() => base.to_string(),
                    _ => rel.clone(),
                };
                let dest = output.join(format!("{name}.json"));
                std::fs::create_dir_all(dest.parent().unwrap())?;
                let text = serde_json::to_string(&v)?;
                bytes += text.len();
                std::fs::write(dest, text)?;
                stats.specs += 1;
            }
            Ok(None) => {}
            Err(e) => stats.errors.push(format!("{rel}: {e}")),
        }
    }
    eprintln!("specs written: {} ({:.1} MB)", stats.specs, bytes as f64 / 1e6);
    eprintln!(
        "generators: {} → builtin {} · lines {} · columns {} · json {} · static {} · template {} · dropped {}",
        stats.generators,
        stats.builtin.values().sum::<usize>(),
        stats.script,
        stats.columns,
        stats.json,
        stats.static_,
        stats.template,
        stats.dropped
    );
    for (id, n) in &stats.builtin {
        eprintln!("  builtin {id}: {n}");
    }
    let mut dropped: Vec<_> = stats.dropped_cmds.iter().collect();
    dropped.sort_by(|a, b| b.1.cmp(a.1));
    eprintln!("most common dropped generator commands:");
    for (c, n) in dropped.iter().take(40) {
        eprintln!("  {n:4}  {}", c.chars().take(100).collect::<String>());
    }
    for e in &stats.errors {
        eprintln!("error: {e}");
    }
    Ok(())
}
