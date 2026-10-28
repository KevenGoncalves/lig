//! The completion engine: walks a command line through its spec and
//! produces ranked suggestions.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::config::Behavior;
use crate::exec::{self, Cmd};
use crate::help::{HelpLoader, Lookup};
use crate::parser::{self, CommandLine, Token};
use crate::spec::{
    self, Arg, GenCtx, GenSource, Generator, LoadSpec, Opt, Subcommand, SuggestionDef, Template, Trigger,
};
pub use crate::spec::{Icon, Kind};
use crate::specs;
use crate::store::Store;

#[derive(Debug, Clone)]
pub struct Suggestion {
    pub label: String,
    pub description: String,
    pub kind: Kind,
    /// A more specific icon than the kind's, if known.
    pub icon: Option<Icon>,
    /// Text to insert after deleting `delete` characters left of the cursor.
    pub insert: String,
    pub delete: usize,
    /// Move the cursor this many characters back after inserting.
    pub back: usize,
    /// The text the suggestion was matched against (for highlighting).
    pub query: String,
}

#[derive(Debug, Default)]
pub struct Completion {
    pub items: Vec<Suggestion>,
    /// Some generators were skipped and still need to run.
    pub incomplete: bool,
    /// What is still being fetched, for a "loading" message.
    pub loading: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ShellEnv {
    pub cwd: PathBuf,
    pub path: Option<String>,
    pub aliases: Arc<HashMap<String, String>>,
    pub functions: Arc<Vec<String>>,
    pub builtins: Arc<Vec<String>>,
}

/// A candidate before filtering and ranking (see [`Engine::finish`]).
struct Cand {
    /// Every name it can be typed as (`-v`, `--verbose`); matched against `query`.
    names: Vec<String>,
    label: String,
    description: Option<String>,
    /// Inserted instead of the name, if set (may contain `{cursor}`).
    insert_value: Option<String>,
    priority: f64,
    /// Only shown when typed exactly.
    hidden: bool,
    kind: Kind,
    icon: Option<Icon>,
    query: String,
    /// Characters left of the cursor that accepting it replaces.
    delete: usize,
    /// Typed after the name: `" "`, an option's `=`, or nothing.
    suffix: String,
    /// Already matched by the generator itself; keep as is.
    unfiltered: bool,
}

/// The state of one completion request.
struct Cx<'a> {
    cl: &'a CommandLine,
    env: &'a ShellEnv,
    /// Token values, the one under the cursor last (what generators see).
    tokens: Vec<String>,
    cands: Vec<Cand>,
    /// Generators and `--help` may run (second pass); otherwise only caches are used.
    run_generators: bool,
    incomplete: bool,
    loading: Option<String>,
    /// The popup was asked for (Ctrl+Space).
    manual: bool,
    /// Offer `.` with paths: for option values and subcommand arguments
    /// (`cargo install --path .`), not a command's own argument (`cd .`).
    offer_dot: bool,
    /// The command whose arguments are being completed (`nvim` in `sudo nvim `).
    command: String,
    /// The argument has no generated values, so `.`, Enter and Back don't crowd them out.
    only_paths: bool,
}

impl Cx<'_> {
    /// Something needs to run before the suggestions are complete.
    fn wait(&mut self, what: String) {
        self.incomplete = true;
        self.loading.get_or_insert(what);
    }

    fn default_query(&self) -> (String, usize) {
        (self.cl.current.value.clone(), self.cl.current.raw.chars().count())
    }

    /// Add a candidate matched against `query`; accepting it replaces the last `delete` characters.
    fn push(&mut self, meta: &spec::Meta, kind: Kind, suffix: &str, query: &str, delete: usize) -> &mut Cand {
        let mut c = cand(meta, kind, suffix);
        c.query = query.to_string();
        c.delete = delete;
        self.cands.push(c);
        self.cands.last_mut().expect("just pushed")
    }

    fn push_def(&mut self, d: &SuggestionDef, fallback: Kind, query: &str, delete: usize) -> &mut Cand {
        self.push(&d.meta, d.kind.unwrap_or(fallback), " ", query, delete)
    }
}

fn cand(m: &spec::Meta, kind: Kind, suffix: &str) -> Cand {
    let label = m.display_name.clone().unwrap_or_else(|| match kind {
        Kind::Option => m.names.join(", "),
        _ => m.names.first().cloned().unwrap_or_default(),
    });
    let names = if m.names.is_empty() { vec![label.clone()] } else { m.names.clone() };
    Cand {
        names,
        label,
        description: m.description.clone(),
        insert_value: m.insert_value.clone(),
        priority: m.priority.unwrap_or(50.0),
        hidden: m.hidden,
        kind,
        icon: m.icon,
        query: String::new(),
        delete: 0,
        suffix: suffix.to_string(),
        unfiltered: false,
    }
}

/// More than this is never useful in a popup.
const MAX_SUGGESTIONS: usize = 500;

pub struct Engine {
    store: Store,
    cfg: Behavior,
    help: HelpLoader,
    /// Loaded specs by command name (`None`: there is none).
    specs: HashMap<String, Option<Arc<Subcommand>>>,
    /// Generator results by generator, context and trigger state.
    gen_cache: HashMap<String, (Instant, Vec<SuggestionDef>)>,
    /// The last results per generator and context, shown while fresh ones load.
    last_results: HashMap<String, Vec<SuggestionDef>>,
    /// Executables on PATH: (when listed, the PATH, names).
    path_cmds: Option<(Instant, String, Arc<BTreeSet<String>>)>,
    spec_cmds: Option<Arc<HashSet<String>>>,
}

fn exec_vars(env: &ShellEnv) -> Vec<(String, String)> {
    env.path.iter().map(|p| ("PATH".to_string(), p.clone())).collect()
}

impl Engine {
    pub fn new(store: Store, cfg: Behavior) -> Self {
        let help = HelpLoader::new(Duration::from_millis(cfg.help_timeout_ms), cfg.help_skip.clone());
        Engine {
            store,
            cfg,
            help,
            specs: HashMap::new(),
            gen_cache: HashMap::new(),
            last_results: HashMap::new(),
            path_cmds: None,
            spec_cmds: None,
        }
    }

    /// Complete `line` (the text left of the cursor). Without `run_generators` only
    /// caches are used and `incomplete` tells whether running could add more.
    /// `manual` (Ctrl+Space) completes command names and empty words regardless of config.
    pub fn complete(&mut self, line: &str, env: &ShellEnv, run_generators: bool, manual: bool) -> Completion {
        let mut cl = parser::parse(line);
        expand_aliases(&mut cl, env);
        let mut cx = Cx {
            tokens: cl.values(),
            cl: &cl,
            env,
            cands: vec![],
            run_generators,
            incomplete: false,
            loading: None,
            manual,
            offer_dot: false,
            command: String::new(),
            only_paths: true,
        };

        if cl.current.redirect {
            self.files(&mut cx, false);
        } else if cl.tokens.is_empty() {
            let v = &cl.current.value;
            if (self.cfg.complete_commands && !v.is_empty()) || cx.manual {
                if v.contains('/') {
                    self.files(&mut cx, false);
                } else {
                    self.commands(&mut cx);
                }
            }
        } else if self.cfg.show_on_empty_token || manual || !cl.current.raw.is_empty() {
            let cmd = basename(&cl.tokens[0].value).to_string();
            cx.command = cmd.clone();
            if !self.cfg.disabled_commands.contains(&cmd)
                && let Some(root) = self.spec(&cmd, &mut cx)
            {
                let root = self.resolve(&root, &mut cx);
                self.walk(root, &mut cx);
            }
        }
        let (incomplete, loading) = (cx.incomplete, cx.loading.take());
        Completion { items: self.finish(cx), incomplete, loading }
    }

    // ------------------------------------------------------------ walking

    /// Follow the typed tokens through the spec, then suggest what fits the word under the cursor.
    fn walk(&mut self, root: Arc<Subcommand>, cx: &mut Cx<'_>) {
        let tokens: Vec<Token> = cx.cl.tokens[1..].to_vec();
        let mut node = root;
        let mut persistent: Vec<Arc<Opt>> = vec![];
        let mut used: Vec<Arc<Opt>> = vec![];
        let mut arg_idx = 0usize;
        // A positional value was typed: no more subcommands.
        let mut saw_positional = false;
        let mut no_more_opts = false;
        // An option still waiting for its argument number `k`.
        let mut pending: Option<(Arc<Opt>, usize)> = None;
        let mut directives = node.directives();
        // No subcommand reached yet: positionals are the command's own.
        let mut at_root = true;

        for tok in &tokens {
            let t = tok.value.as_str();
            if let Some((opt, k)) = pending.take() {
                let a = &opt.args[k];
                let breaks = a.is_variadic && a.options_can_break_variadic && is_flag(t);
                if !breaks {
                    if a.is_variadic {
                        pending = Some((opt, k));
                    } else if opt.args.get(k + 1).is_some_and(|n| !n.is_optional) {
                        pending = Some((opt.clone(), k + 1));
                    }
                    continue;
                }
            }
            if !no_more_opts && t == "--" {
                no_more_opts = true;
                continue;
            }
            if !no_more_opts && is_flag(t) {
                if let Some((opt, inline)) = match_option(&node, &persistent, t, &directives) {
                    if !inline && opt.takes_arg() {
                        pending = Some((opt.clone(), 0));
                    }
                    used.push(opt);
                }
                continue;
            }
            if !saw_positional && let Some(sub) = node.find_subcommand(t).cloned() {
                persistent.extend(node.options.iter().filter(|o| o.is_persistent).cloned());
                node = self.resolve(&sub, cx);
                if let Some(d) = &node.directives {
                    directives = d.clone();
                }
                arg_idx = 0;
                at_root = false;
                continue;
            }
            saw_positional = true;
            let Some(arg) = current_arg(&node, arg_idx) else { continue };
            if arg.is_command {
                // The rest of the line is another command.
                let Some(s) = self.spec(basename(t), cx) else { return };
                cx.command = basename(t).to_string();
                node = self.resolve(&s, cx);
                persistent.clear();
                used.clear();
                arg_idx = 0;
                saw_positional = false;
                no_more_opts = false;
                directives = node.directives();
                at_root = true;
                continue;
            }
            if !arg.is_variadic {
                arg_idx += 1;
            }
        }

        // --- the token under the cursor
        let cur = cx.cl.current.clone();
        if let Some((opt, k)) = pending {
            let a = opt.args[k].clone();
            if !(a.is_variadic && a.options_can_break_variadic && is_flag(&cur.value)) {
                let (q, d) = cx.default_query();
                cx.offer_dot = true;
                self.arg_suggestions(&a, cx, &q, d);
                return;
            }
        }
        if !no_more_opts && cur.value.starts_with('-') {
            // `--opt=value`
            let seps: Vec<String> = if directives.option_arg_separators.is_empty() {
                vec!["=".into()]
            } else {
                directives.option_arg_separators.clone()
            };
            for sep in &seps {
                if let Some((name, value)) = cur.value.split_once(sep.as_str())
                    && let Some(opt) = find_option(&node, &persistent, name)
                {
                    if let Some(a) = opt.args.first().cloned() {
                        let delete = cur.raw.rsplit_once(sep.as_str()).map_or(0, |(_, r)| r.chars().count());
                        cx.offer_dot = true;
                        self.arg_suggestions(&a, cx, value, delete);
                    }
                    return;
                }
            }
            self.options(&node, &persistent, &used, cx);
            return;
        }
        if !saw_positional {
            let (q, d) = cx.default_query();
            for s in &node.subcommands {
                cx.push(&s.meta, Kind::Subcommand, " ", &q, d);
            }
            for s in &node.additional_suggestions {
                cx.push_def(s, Kind::Shortcut, &q, d);
            }
        }
        let arg = current_arg(&node, arg_idx);
        if let Some(a) = &arg {
            if a.is_command {
                // A command name (`sudo ls`): like the first word, only on demand.
                if (self.cfg.complete_commands && !cur.value.is_empty()) || cx.manual {
                    self.commands(cx);
                }
            } else if a.command_names {
                self.commands(cx);
            } else {
                let (q, d) = cx.default_query();
                cx.offer_dot = !at_root;
                self.arg_suggestions(a, cx, &q, d);
            }
        }
        // Options are offered when nothing else is, and next to plain file arguments (`java ` → `-jar`).
        let files_only = arg.as_ref().is_some_and(|a| {
            a.suggestions.is_empty()
                && a.generators.is_empty()
                && !a.is_command
                && !a.command_names
                && !a.template.is_empty()
        });
        if !cx.incomplete && (files_only || (cx.cands.is_empty() && arg.is_none_or(|a| a.is_optional))) {
            self.options(&node, &persistent, &used, cx);
        }
    }

    /// Follow a subcommand's `load_spec` (another spec, or its `--help`).
    fn resolve(&mut self, sub: &Arc<Subcommand>, cx: &mut Cx<'_>) -> Arc<Subcommand> {
        let loaded = match &sub.load_spec {
            Some(LoadSpec::Name(n)) => self.spec(n, cx),
            Some(LoadSpec::Help(argv)) => self.help_spec(argv, cx),
            None => return sub.clone(),
        };
        match loaded {
            Some(l) => {
                let mut merged = (*l).clone();
                merged.meta = sub.meta.clone();
                merged.options.extend(sub.options.iter().cloned());
                merged.subcommands.extend(sub.subcommands.iter().cloned());
                merged.load_spec = None;
                Arc::new(merged)
            }
            None => sub.clone(),
        }
    }

    /// The spec for a command: user file, built-in, or parsed from `--help`.
    fn spec(&mut self, name: &str, cx: &mut Cx<'_>) -> Option<Arc<Subcommand>> {
        if let Some(s) = self.specs.get(name) {
            if s.is_some() || !self.cfg.help_parsing {
                return s.clone();
            }
        } else {
            let s = match self.store.load(name) {
                Some(Ok(s)) => Some(Arc::new(s)),
                Some(Err(e)) => {
                    crate::log(&format!("spec {e}"));
                    None
                }
                None => specs::builtin(name).map(Arc::new),
            };
            self.specs.insert(name.to_string(), s.clone());
            if s.is_some() || !self.cfg.help_parsing {
                return s;
            }
        }
        self.help_spec(&[name.to_string()], cx)
    }

    fn help_spec(&mut self, argv: &[String], cx: &mut Cx<'_>) -> Option<Arc<Subcommand>> {
        match self.help.lookup(argv, &cx.env.cwd, &exec_vars(cx.env), cx.run_generators) {
            Lookup::Found(s) => s,
            Lookup::NeedsRun => {
                cx.wait(format!("Reading `{} --help`", argv.join(" ")));
                None
            }
        }
    }

    // ------------------------------------------------------------ sources

    fn options(&mut self, node: &Subcommand, persistent: &[Arc<Opt>], used: &[Arc<Opt>], cx: &mut Cx<'_>) {
        let (q, d) = cx.default_query();
        for o in node.options.iter().chain(persistent) {
            if !o.is_repeatable && used.iter().any(|u| Arc::ptr_eq(u, o)) {
                continue;
            }
            if used.iter().any(|u| u.meta.names.iter().any(|n| o.exclusive_on.contains(n))) {
                continue;
            }
            let suffix = match &o.separator {
                Some(sep) if o.takes_arg() => sep.as_str(),
                _ => " ",
            };
            cx.push(&o.meta, Kind::Option, suffix, &q, d);
        }
    }

    fn arg_suggestions(&mut self, a: &Arg, cx: &mut Cx<'_>, query: &str, delete: usize) {
        cx.only_paths = a.generators.is_empty();
        for s in &a.suggestions {
            cx.push_def(s, Kind::Arg, query, delete);
        }
        if a.template.contains(&Template::Filepaths) {
            self.files(cx, false);
        } else if a.template.contains(&Template::Folders) {
            self.files(cx, true);
        }
        for g in &a.generators {
            self.generator(g, cx, query, delete);
        }
    }

    fn generator(&mut self, g: &Arc<Generator>, cx: &mut Cx<'_>, query: &str, delete: usize) {
        if !self.cfg.online && g.id.starts_with("online:") {
            return;
        }
        let cur = &cx.cl.current;
        let (query, delete) = match g.query_term.as_deref().and_then(|chars| cur.value.rfind(|c| chars.contains(c))) {
            Some(i) => {
                let q = cur.value[i + 1..].to_string();
                let d = q.chars().count();
                (q, d)
            }
            None => (query.to_string(), delete),
        };
        let v = &cur.value;
        let trigger_key = match &g.trigger {
            None => String::new(),
            Some(Trigger::Chars(chars)) => {
                v.rfind(|c| chars.contains(c)).map_or(String::new(), |i| v[..=i].to_string())
            }
            Some(Trigger::Change) => v.clone(),
            Some(Trigger::Prefix(n)) => v.chars().take(*n).collect(),
        };
        let before = &cx.tokens[..cx.tokens.len() - 1];
        let key = format!("{}\0{}\0{}\0{}", g.id, cx.env.cwd.display(), before.join("\x1f"), trigger_key);
        let ttl = g.cache_ttl.unwrap_or(Duration::from_millis(self.cfg.generator_cache_ms));

        // Same generator and context, any trigger state: shown while fresh results are fetched.
        let context = format!("{}\0{}\0{}", g.id, cx.env.cwd.display(), before.join("\x1f"));
        let defs = match self.gen_cache.get(&key) {
            Some((t, defs)) if t.elapsed() < ttl => defs.clone(),
            _ if !cx.run_generators => {
                cx.wait(loading_label(&g.id));
                if let Some(stale) = self.last_results.get(&context) {
                    for d in stale {
                        cx.push_def(d, Kind::Arg, &query, delete);
                    }
                }
                return;
            }
            _ => {
                let defs = self.run_generator(g, cx);
                self.gen_cache.insert(key, (Instant::now(), defs.clone()));
                if !defs.is_empty() {
                    self.last_results.insert(context, defs.clone());
                }
                defs
            }
        };
        let icon = generator_icon(&g.id);
        for d in &defs {
            let c = cx.push_def(d, Kind::Arg, &query, delete);
            c.unfiltered = !g.filter;
            c.icon = c.icon.or(icon);
        }
    }

    fn run_generator(&self, g: &Generator, cx: &Cx<'_>) -> Vec<SuggestionDef> {
        let vars = exec_vars(cx.env);
        let timeout = Duration::from_millis(self.cfg.generator_timeout_ms);
        match &g.source {
            GenSource::Rust(f) => f(&GenCtx { cwd: &cx.env.cwd, env: &vars, tokens: &cx.tokens, timeout }),
            GenSource::Script { argv, shell, output } => {
                let out = match shell {
                    Some(s) => exec::run(Cmd::Shell(s), &cx.env.cwd, &vars, timeout),
                    None => exec::run(Cmd::Argv(argv), &cx.env.cwd, &vars, timeout),
                };
                output.suggestions(&out.stdout)
            }
            GenSource::Static(list) => list.clone(),
        }
    }

    fn files(&mut self, cx: &mut Cx<'_>, folders_only: bool) {
        if folders_only && cx.only_paths {
            folder_navigation(cx);
        }
        let cl = cx.cl;
        let cur = &cl.current;
        if cx.offer_dot && cx.only_paths && (cur.value.is_empty() || cur.value == ".") {
            let meta = spec::Meta {
                names: vec![".".into()],
                description: Some("The current folder".into()),
                insert_value: Some(".".into()),
                priority: Some(1000.0),
                icon: Some(Icon::Folder),
                ..Default::default()
            };
            // Listed with the values, first: Enter after `--path ` picks it.
            cx.push(&meta, Kind::Arg, "", &cur.value, cur.raw.chars().count());
            cx.offer_dot = false;
        }
        let (dir_part, base) = match cur.value.rfind('/') {
            Some(i) => (&cur.value[..=i], &cur.value[i + 1..]),
            None => ("", cur.value.as_str()),
        };
        let delete = match cur.raw.rfind('/') {
            Some(i) => cur.raw[i + 1..].chars().count(),
            None => cur.raw.chars().count(),
        };
        let dir = if dir_part.is_empty() {
            cx.env.cwd.clone()
        } else if let Some(rel) = dir_part.strip_prefix("~/") {
            dirs::home_dir().unwrap_or_default().join(rel)
        } else {
            // An absolute `dir_part` replaces the cwd.
            cx.env.cwd.join(dir_part)
        };
        let Ok(rd) = std::fs::read_dir(&dir) else { return };
        let mut entries: Vec<(bool, String)> = rd
            .flatten()
            .take(5000)
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.starts_with('.') && !base.starts_with('.') {
                    return None;
                }
                let is_dir = std::fs::metadata(e.path()).map(|m| m.is_dir()).unwrap_or(false);
                (is_dir || !folders_only).then_some((is_dir, name))
            })
            .collect();
        // Folders first, then by name.
        entries.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.to_lowercase().cmp(&b.1.to_lowercase())));
        for (is_dir, name) in entries {
            let (kind, label, suffix) =
                if is_dir { (Kind::Folder, format!("{name}/"), "") } else { (Kind::File, name, " ") };
            let meta = spec::Meta { names: vec![label], ..Default::default() };
            cx.push(&meta, kind, suffix, base, delete);
        }
    }

    /// Executables on the shell's PATH; those with a built-in or user spec first.
    fn commands(&mut self, cx: &mut Cx<'_>) {
        let path = self.path_commands(cx.env);
        let store = &self.store;
        let with_spec = self
            .spec_cmds
            .get_or_insert_with(|| {
                let mut s: HashSet<String> = specs::lig_names().map(str::to_string).collect();
                s.extend(specs::FIG.names().map(str::to_string));
                s.extend(store.commands());
                Arc::new(s)
            })
            .clone();
        let (q, d) = cx.default_query();
        for (name, value) in cx.env.aliases.iter() {
            let meta = spec::Meta {
                names: vec![name.clone()],
                description: Some(format!("alias for {value}")),
                priority: Some(56.0),
                icon: Some(Icon::Alias),
                ..Default::default()
            };
            cx.push(&meta, Kind::Command, " ", &q, d);
        }
        let shell_names = cx
            .env
            .functions
            .iter()
            .map(|n| (n, "shell function", 54.0, Icon::Function))
            .chain(cx.env.builtins.iter().map(|n| (n, "shell builtin", 53.0, Icon::Builtin)));
        for (name, what, priority, icon) in shell_names {
            let meta = spec::Meta {
                names: vec![name.clone()],
                description: Some(what.into()),
                priority: Some(priority),
                icon: Some(icon),
                ..Default::default()
            };
            cx.push(&meta, Kind::Command, " ", &q, d);
        }
        for name in path.iter() {
            let meta = spec::Meta {
                names: vec![name.clone()],
                priority: Some(if with_spec.contains(name) { 55.0 } else { 50.0 }),
                ..Default::default()
            };
            cx.push(&meta, Kind::Command, " ", &q, d);
        }
    }

    fn path_commands(&mut self, env: &ShellEnv) -> Arc<BTreeSet<String>> {
        let path = env.path.clone().or_else(|| std::env::var("PATH").ok()).unwrap_or_default();
        if let Some((t, p, set)) = &self.path_cmds
            && *p == path
            && t.elapsed() < Duration::from_secs(30)
        {
            return set.clone();
        }
        let mut set = BTreeSet::new();
        for dir in path.split(':').filter(|d| !d.is_empty()) {
            for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
                if let Ok(m) = std::fs::metadata(e.path())
                    && m.is_file()
                    && m.permissions().mode() & 0o111 != 0
                {
                    set.insert(e.file_name().to_string_lossy().into_owned());
                }
            }
        }
        let set = Arc::new(set);
        self.path_cmds = Some((Instant::now(), path, set.clone()));
        set
    }

    // ------------------------------------------------------------ ranking

    /// Filter the candidates by what's typed, rank them and build the suggestions.
    fn finish(&self, cx: Cx<'_>) -> Vec<Suggestion> {
        struct Scored {
            /// From [`match_tier`]: 0 exact, 1 prefix, 2 fuzzy.
            tier: u8,
            priority: f64,
            /// Index in `cx.cands` (spec order breaks ties).
            idx: usize,
            name: String,
        }

        let cur = &cx.cl.current;
        let mut scored: Vec<Scored> = vec![];
        for (idx, c) in cx.cands.iter().enumerate() {
            if c.names.iter().all(|n| n.trim().is_empty())
                && c.insert_value.is_none()
                && !matches!(c.kind, Kind::Execute | Kind::Back)
            {
                continue;
            }
            // Rank by the best-matching name, but insert the first name (in
            // spec order) that matches as a prefix: `b` → `build`, not `b`.
            let tiers: Vec<Option<u8>> = if c.unfiltered {
                vec![Some(1); c.names.len()]
            } else {
                c.names.iter().map(|n| match_tier(n, &c.query, self.cfg.fuzzy)).collect()
            };
            let Some(tier) = tiers.iter().flatten().min().copied() else { continue };
            let pick = if tier <= 1 {
                tiers.iter().position(|t| matches!(t, Some(0 | 1)))
            } else {
                tiers.iter().position(Option::is_some)
            };
            let pick = pick.unwrap_or(0);
            let name = &c.names[pick];
            if c.hidden && tier != 0 {
                continue;
            }
            scored.push(Scored { tier, priority: c.priority, idx, name: name.clone() });
        }
        // Fuzzy matches only when nothing matches by prefix.
        if scored.iter().any(|s| s.tier < 2) {
            scored.retain(|s| s.tier < 2);
        }
        // Subcommands and values first, then options, then files; files first
        // for commands that open them (`nvim `, `code `).
        let files_first = self.cfg.files_first || self.cfg.files_first_commands.contains(&cx.command);
        let kind_tier = |idx: usize| match cx.cands[idx].kind {
            Kind::Option => 1,
            Kind::File | Kind::Folder if !files_first => 2,
            _ => 0,
        };
        let fuzzy = |s: &Scored| s.tier == 2;
        scored.sort_by(|a, b| {
            fuzzy(a)
                .cmp(&fuzzy(b))
                .then(kind_tier(a.idx).cmp(&kind_tier(b.idx)))
                .then(b.priority.total_cmp(&a.priority))
                .then(a.tier.cmp(&b.tier))
                .then(a.idx.cmp(&b.idx))
        });

        let mut seen = HashSet::new();
        let mut out = vec![];
        for Scored { idx, name, .. } in scored {
            let c = &cx.cands[idx];
            let (insert, back) = insertion(c, &name, cur, self.cfg.insert_space);
            // Enter and Back can both insert "": keep them apart from values.
            if !matches!(c.kind, Kind::Execute | Kind::Back) && !seen.insert(insert.clone()) {
                continue;
            }
            out.push(Suggestion {
                label: c.label.clone(),
                description: c.description.as_deref().unwrap_or("").lines().next().unwrap_or("").trim().to_string(),
                kind: c.kind,
                icon: c.icon,
                insert,
                delete: c.delete,
                back,
                query: c.query.clone(),
            });
            if out.len() >= MAX_SUGGESTIONS {
                break;
            }
        }
        // Nothing to offer if the only suggestion is exactly what's typed —
        // except a folder (accepting adds its `/`).
        if out.len() == 1 && out[0].kind != Kind::Folder && out[0].insert.trim_end() == cur.raw {
            out.clear();
        }
        out
    }
}

/// For a folder argument: "Enter" once a folder is complete (`cd src/`), then "Back" (one folder up).
fn folder_navigation(cx: &mut Cx<'_>) {
    // Not next to subcommands (`opencode [directory]`): they matter more.
    if cx.cands.iter().any(|c| c.kind == Kind::Subcommand) {
        return;
    }
    let cl = cx.cl;
    let cur = &cl.current;
    if cur.value.ends_with('/') && !cx.cands.iter().any(|c| c.kind == Kind::Execute) {
        let line = cl.tokens.iter().map(|t| t.raw.as_str()).chain([cur.raw.as_str()]).collect::<Vec<_>>().join(" ");
        let meta = spec::Meta {
            names: vec![String::new()],
            display_name: Some("Enter".into()),
            description: Some(format!("Run `{line}`")),
            priority: Some(1000.0),
            ..Default::default()
        };
        cx.push(&meta, Kind::Execute, "", "", 0).unfiltered = true;
    }
    if (cur.value.is_empty() || cur.value.ends_with('/'))
        && !cx.cands.iter().any(|c| c.kind == Kind::Back)
        && let Some(up) = parent_path(&cur.raw)
    {
        let shown = if up.is_empty() { "the current folder".to_string() } else { format!("`{up}`") };
        let meta = spec::Meta {
            names: vec![String::new()],
            display_name: Some("Back".into()),
            description: Some(format!("Up to {shown}")),
            insert_value: Some(up),
            priority: Some(999.0),
            ..Default::default()
        };
        cx.push(&meta, Kind::Back, "", "", cur.raw.chars().count()).unfiltered = true;
    }
}

fn generator_icon(id: &str) -> Option<Icon> {
    let (ns, what) = id.split_once(':').unwrap_or((id, ""));
    Some(match (ns, what) {
        ("git", "branches" | "local-branches") => Icon::GitBranch,
        ("git", "tags") => Icon::GitTag,
        ("git", "commits") => Icon::GitCommit,
        ("git", "remotes") => Icon::GitRemote,
        ("git", "stashes") => Icon::GitStash,
        ("docker", "containers" | "running" | "compose-services") => Icon::Container,
        ("docker", "images") | ("online", "dockerhub") => Icon::Docker,
        ("docker", "networks") => Icon::Network,
        ("docker", "volumes") => Icon::Volume,
        ("docker", "contexts") | ("kube", _) => Icon::Cloud,
        ("npm", "scripts") | ("make", _) | ("cargo", "bin" | "example" | "test" | "bench") => Icon::Script,
        ("npm", "deps") | ("online", "npm") => Icon::Npm,
        ("sys", "pids" | "process-names") => Icon::Process,
        ("sys", "users") => Icon::User,
        ("systemd", _) => Icon::Service,
        ("apt", _) | ("snap", _) | ("extrepo", _) | ("online", "snap") => Icon::Package,
        ("ssh", _) => Icon::Host,
        ("tmux", _) => Icon::Terminal,
        ("pip", _) | ("online", "pypi" | "pypi-top") => Icon::Python,
        ("rustup", _) | ("asdf", "installed" | "versions") => Icon::Version,
        ("asdf", "plugins" | "plugins-all") | ("dnf", "installed" | "available") => Icon::Package,
        ("dnf", "repos" | "repos-enabled" | "repos-disabled") => Icon::Cloud,
        ("cargo", _) | ("online", "crates") => Icon::Rust,
        ("zoxide", _) => Icon::Folder,
        _ => return None,
    })
}

/// A typed directory path (as typed, quotes/escapes kept) one level up:
/// `src/app/` → `src/`, `src/` → ``, `` → `../`, `../` → `../../`.
fn parent_path(raw: &str) -> Option<String> {
    if raw == "/" {
        return None;
    }
    let trimmed = raw.trim_end_matches('/');
    let (head, last) = match trimmed.rfind('/') {
        Some(i) => (&trimmed[..=i], &trimmed[i + 1..]),
        None => ("", trimmed),
    };
    Some(match last {
        // Nothing to strip: go further up.
        "" | "." | ".." | "~" => format!("{raw}../"),
        _ => head.to_string(),
    })
}

fn loading_label(id: &str) -> String {
    match id {
        "online:npm" => "Searching npm",
        "online:crates" => "Searching crates.io",
        "online:dockerhub" => "Searching Docker Hub",
        "online:pypi" => "Fetching versions from PyPI",
        "online:pypi-top" => "Fetching popular PyPI packages",
        "online:snap" => "Searching the Snap Store",
        "apt:available" => "Searching apt packages",
        "extrepo:available" => "Fetching the extrepo index",
        "asdf:versions" => "Fetching versions with asdf",
        "asdf:plugins-all" => "Fetching the asdf plugin index",
        "dnf:available" => "Reading dnf's package list",
        "opencode:models" => "Listing opencode models",
        _ => "Loading suggestions",
    }
    .to_string()
}

/// Replace a leading alias (`gco` → `git checkout`), as zsh would.
fn expand_aliases(cl: &mut CommandLine, env: &ShellEnv) {
    let mut seen = HashSet::new();
    while let Some(first) = cl.tokens.first() {
        let Some(value) = env.aliases.get(&first.value) else { break };
        if !seen.insert(first.value.clone()) {
            break;
        }
        let expanded = parser::parse(&format!("{value} ")).tokens;
        if expanded.is_empty() {
            break;
        }
        cl.tokens.splice(0..1, expanded);
    }
}

fn insertion(c: &Cand, name: &str, cur: &Token, space: bool) -> (String, usize) {
    if let Some(iv) = &c.insert_value {
        return match iv.split_once("{cursor}") {
            Some((a, b)) => (format!("{a}{b}"), b.chars().count()),
            None => (iv.clone(), 0),
        };
    }
    let is_dir = c.kind == Kind::Folder || name.ends_with('/');
    let bare = if is_dir { name.trim_end_matches('/') } else { name };
    // A trailing space only with `insert_space`; `=` and `/` always.
    let suffix = match (is_dir, c.suffix.as_str()) {
        (true, _) => "/",
        (false, " ") if !space => "",
        (false, s) => s,
    };
    match cur.open_quote {
        Some(q) if c.kind != Kind::Option => {
            let body = match q {
                '\'' => bare.replace('\'', r"'\''"),
                _ => bare.chars().fold(String::new(), |mut s, ch| {
                    if matches!(ch, '"' | '\\' | '$' | '`') {
                        s.push('\\');
                    }
                    s.push(ch);
                    s
                }),
            };
            let open = if c.delete >= cur.raw.chars().count() { q.to_string() } else { String::new() };
            if is_dir { (format!("{open}{body}/"), 0) } else { (format!("{open}{body}{q}{suffix}"), 0) }
        }
        _ if matches!(c.kind, Kind::File | Kind::Folder) => (format!("{}{suffix}", parser::escape(bare)), 0),
        // Values from specs may be globs or `~/…`; only escape what would break the word.
        _ => (format!("{}{suffix}", parser::escape_minimal(bare)), 0),
    }
}

fn is_flag(t: &str) -> bool {
    t.starts_with('-') && t.len() > 1
}

fn basename(s: &str) -> &str {
    s.rsplit('/').next().unwrap_or(s)
}

fn current_arg(node: &Subcommand, idx: usize) -> Option<Arc<Arg>> {
    node.args.get(idx).or_else(|| node.args.last().filter(|a| a.is_variadic)).cloned()
}

fn find_option(node: &Subcommand, persistent: &[Arc<Opt>], name: &str) -> Option<Arc<Opt>> {
    node.options.iter().chain(persistent).find(|o| o.meta.names.iter().any(|n| n == name)).cloned()
}

/// Returns the option a flag token refers to and whether its argument is
/// already part of the token (`--foo=bar`, `-ovalue`, `-abc`).
fn match_option(node: &Subcommand, persistent: &[Arc<Opt>], t: &str, d: &spec::Directives) -> Option<(Arc<Opt>, bool)> {
    if let Some(o) = find_option(node, persistent, t) {
        return Some((o, false));
    }
    if let Some((name, _)) = t.split_once('=')
        && let Some(o) = find_option(node, persistent, name)
    {
        return Some((o, true));
    }
    for sep in &d.option_arg_separators {
        if let Some((name, _)) = t.split_once(sep.as_str())
            && let Some(o) = find_option(node, persistent, name)
        {
            return Some((o, true));
        }
    }
    if !t.starts_with("--") && !d.flags_are_posix_noncompliant {
        let chars: Vec<char> = t[1..].chars().collect();
        let mut last = None;
        for (j, ch) in chars.iter().enumerate() {
            match find_option(node, persistent, &format!("-{ch}")) {
                Some(o) if o.takes_arg() => return Some((o, j + 1 < chars.len())),
                Some(o) => last = Some(o),
                None => break,
            }
        }
        return last.map(|o| (o, true));
    }
    None
}

/// 0 = exact, 1 = prefix, 2 = fuzzy (subsequence); smart-case.
pub fn match_tier(name: &str, query: &str, fuzzy: bool) -> Option<u8> {
    if query.is_empty() {
        return Some(1);
    }
    let sensitive = query.chars().any(char::is_uppercase);
    let (n, q) =
        if sensitive { (name.to_string(), query.to_string()) } else { (name.to_lowercase(), query.to_lowercase()) };
    if n == q {
        return Some(0);
    }
    if n.starts_with(&q) {
        return Some(1);
    }
    // Fuzzy: subsequence, anchored on the first significant character.
    let (n, q) = (n.trim_start_matches('-'), q.trim_start_matches('-'));
    if fuzzy && q.chars().count() >= 2 && n.chars().next() == q.chars().next() {
        let mut it = n.chars();
        if q.chars().all(|qc| it.by_ref().any(|nc| nc == qc)) {
            return Some(2);
        }
    }
    None
}

impl Engine {
    #[cfg(test)]
    fn insert_spec(&mut self, s: Subcommand) {
        self.specs.insert(s.meta.names[0].clone(), Some(Arc::new(s)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{arg, cmd, gen_fn, opt, sug};

    fn tool() -> Subcommand {
        cmd("tool", "")
            .opts([
                opt("-v, --verbose", "Loud").persistent(),
                opt("--color", "").eq().arg(arg("when").suggest(&["always", "never", "auto"])),
                opt("-o, --output", "").arg(arg("file").files()),
            ])
            .subs([
                cmd("build, b", "Build it").arg(arg("target").generator(gen_fn("test:targets", |cx| {
                    let out = cx.run(&["printf", "alpha\nbeta\n"]).unwrap_or_default();
                    out.lines().map(|l| sug(l).desc("from gen")).collect()
                }))),
                cmd("bench", ""),
                cmd("commit", "").opt(opt("-m", "").insert("-m '{cursor}'").arg(arg("msg"))),
                cmd("hidden-cmd", "").hidden(),
                cmd("run", "").arg(arg("cmd").command()),
                cmd("edit", "").opt(opt("--force", "")).arg(arg("file").files()),
            ])
    }

    fn engine() -> (Engine, ShellEnv) {
        let dir =
            std::env::temp_dir().join(format!("lig-test-{}-{:?}", std::process::id(), std::thread::current().id()));
        std::fs::create_dir_all(dir.join("specs")).unwrap();
        std::fs::create_dir_all(dir.join("work/sub dir")).unwrap();
        std::fs::write(dir.join("work/file.txt"), "").unwrap();
        std::fs::write(
            dir.join("specs/mytool.toml"),
            "name = \"mytool\"\n[[subcommands]]\nname = [\"hello\", \"hi\"]\ndescription = \"Say hello\"\n[[subcommands.args]]\nname = \"who\"\nsuggestions = [\"world\", \"you\"]\n",
        )
        .unwrap();
        let cfg = Behavior { help_parsing: false, insert_space: true, ..Behavior::default() };
        let mut e = Engine::new(Store::with_dirs(vec![dir.join("specs")]), cfg);
        e.insert_spec(tool());
        (e, ShellEnv { cwd: dir.join("work"), ..Default::default() })
    }

    fn labels(e: &mut Engine, env: &ShellEnv, line: &str) -> Vec<(String, String, usize)> {
        e.complete(line, env, true, false).items.into_iter().map(|s| (s.label, s.insert, s.delete)).collect()
    }

    #[test]
    fn subcommands_and_aliases() {
        let (mut e, env) = engine();
        let r = labels(&mut e, &env, "tool b");
        assert_eq!(r[0], ("build".into(), "build ".into(), 1));
        assert_eq!(r[1].0, "bench");
        assert!(!labels(&mut e, &env, "tool ").iter().any(|r| r.0 == "hidden-cmd"));
        assert_eq!(labels(&mut e, &env, "tool hidden-c").len(), 0);
    }

    #[test]
    fn options_args_and_generators() {
        let (mut e, env) = engine();
        let r = labels(&mut e, &env, "tool --c");
        assert_eq!(r, vec![("--color".into(), "--color=".into(), 3)]);
        let r = labels(&mut e, &env, "tool --color=a");
        assert_eq!(r, vec![("always".into(), "always ".into(), 1), ("auto".into(), "auto ".into(), 1)]);
        let r = labels(&mut e, &env, "tool build -");
        assert!(r.iter().any(|x| x.0 == "-v, --verbose"));
        let r = labels(&mut e, &env, "tool -v build ");
        assert_eq!(r.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["alpha", "beta"]);
        let c = e.complete("tool commit -", &env, true, false);
        assert_eq!((c.items[0].insert.as_str(), c.items[0].back), ("-m ''", 1));
    }

    #[test]
    fn files_and_option_args() {
        let (mut e, env) = engine();
        let r = labels(&mut e, &env, "tool -o ");
        assert_eq!(
            r,
            vec![
                (".".into(), ".".into(), 0),
                ("sub dir/".into(), "sub\\ dir/".into(), 0),
                ("file.txt".into(), "file.txt ".into(), 0)
            ]
        );
        assert_eq!(labels(&mut e, &env, "tool edit ")[0].0, ".");
        assert!(labels(&mut e, &env, "tool -o .").is_empty());
        e.insert_spec(cmd("view", "").arg(arg("file").files()));
        assert!(labels(&mut e, &env, "view ").iter().all(|r| r.0 != "."));
        let r = labels(&mut e, &env, "tool -o \"su");
        assert_eq!(r, vec![("sub dir/".into(), "\"sub dir/".into(), 3)]);
    }

    #[test]
    fn generators_are_deferred() {
        let (mut e, env) = engine();
        let c = e.complete("tool build ", &env, false, false);
        assert!(c.incomplete && c.items.is_empty());
        assert_eq!(c.loading.as_deref(), Some("Loading suggestions"));
        assert_eq!(e.complete("tool build ", &env, true, false).items.len(), 2);
        let c = e.complete("tool build a", &env, false, false);
        assert!(!c.incomplete);
        assert_eq!(c.items[0].label, "alpha");
    }

    #[test]
    fn user_toml_spec_and_nested_commands() {
        let (mut e, env) = engine();
        let r = labels(&mut e, &env, "mytool h");
        assert_eq!(r, vec![("hello".into(), "hello ".into(), 1)]);
        let r = labels(&mut e, &env, "mytool hi w");
        assert_eq!(r, vec![("world".into(), "world ".into(), 1)]);
        let r = labels(&mut e, &env, "tool run mytool he");
        assert_eq!(r[0].0, "hello");
    }

    #[test]
    fn every_embedded_spec_loads() {
        let names: Vec<&str> = specs::FIG.names().collect();
        assert!(names.len() > 700, "only {} specs", names.len());
        for n in names {
            assert!(specs::FIG.load(n).is_some_and(|s| !s.meta.names.is_empty()), "{n}");
        }
    }

    #[test]
    fn converted_and_merged_specs() {
        let (mut e, env) = engine();
        assert_eq!(labels(&mut e, &env, "brew ins")[0].0, "install");
        assert!(labels(&mut e, &env, "aws s3 ").iter().any(|r| r.0 == "cp"));
        assert!(!labels(&mut e, &env, "heroku ").is_empty());
        let r = labels(&mut e, &env, "git rebase --");
        assert!(r.iter().any(|x| x.0 == "--autosquash"));
        assert!(r.iter().any(|x| x.0 == "--keep-base"), "{r:?}");
    }

    #[test]
    fn aliases_functions_and_ranking() {
        let (mut e, mut env) = engine();
        env.aliases = Arc::new(HashMap::from([
            ("t".to_string(), "tool -v".to_string()),
            ("loop".to_string(), "loop x".to_string()),
        ]));
        env.functions = Arc::new(vec!["toolfn".to_string()]);
        env.builtins = Arc::new(vec!["exit".to_string()]);
        assert!(labels(&mut e, &env, "exi").is_empty());
        assert_eq!(e.complete("exi", &env, true, true).items[0].label, "exit");
        assert!(labels(&mut e, &env, "sudo exi").is_empty());
        assert!(labels(&mut e, &env, "sudo -u root exi").is_empty());
        assert_eq!(e.complete("sudo exi", &env, true, true).items[0].label, "exit");
        let r = labels(&mut e, &env, "t b");
        assert_eq!(r[0].0, "build");
        assert!(!labels(&mut e, &env, "t build -").iter().any(|x| x.0 == "-v, --verbose"));
        assert!(labels(&mut e, &env, "loop ").is_empty());
        let r: Vec<String> = e.complete("t", &env, true, true).items.into_iter().map(|s| s.label).collect();
        assert!(r.iter().any(|x| x == "t") && r.iter().any(|x| x == "toolfn"));
        let r = labels(&mut e, &env, "mytool hello ");
        assert_eq!(r[0].0, "world");
        let r = labels(&mut e, &env, "tool edit ");
        let opt = r.iter().position(|x| x.0 == "--force").unwrap();
        let file = r.iter().position(|x| x.0 == "file.txt").unwrap();
        assert!(opt < file, "{r:?}");
    }

    #[test]
    fn files_first_for_commands_that_open_them() {
        let (mut e, env) = engine();
        let mut file_arg = arg("file").variadic();
        file_arg.template = vec![Template::Filepaths, Template::Folders];
        e.insert_spec(cmd("nvim", "").opts([opt("-R", "Read-only")]).arg(file_arg.clone()));
        e.insert_spec(cmd("java", "").opts([opt("-jar", "Run a jar")]).arg(file_arg));
        let r = labels(&mut e, &env, "nvim ");
        let kinds: Vec<&str> = r.iter().map(|x| x.0.as_str()).collect();
        assert_eq!(kinds, ["sub dir/", "file.txt", "-R"]);
        assert!(labels(&mut e, &env, "sudo nvim ").first().is_some_and(|x| x.0 == "sub dir/"));
        assert_eq!(labels(&mut e, &env, "java ")[0].0, "-jar");
    }

    #[test]
    fn no_space_after_completion_by_default() {
        let (_, env) = engine();
        let dir = env.cwd.parent().unwrap().join("specs");
        let mut e = Engine::new(Store::with_dirs(vec![dir]), Behavior { help_parsing: false, ..Behavior::default() });
        e.insert_spec(tool());
        let r = labels(&mut e, &env, "tool b");
        assert_eq!(r[0], ("build".into(), "build".into(), 1));
        assert_eq!(labels(&mut e, &env, "tool build --verb")[0].1, "--verbose");
        assert_eq!(labels(&mut e, &env, "tool --out")[0].1, "--output");
        assert_eq!(labels(&mut e, &env, "tool --c")[0].1, "--color=");
        assert!(labels(&mut e, &env, "tool -o").is_empty());
        assert!(labels(&mut e, &env, "tool bench").is_empty());
        assert_eq!(labels(&mut e, &env, "tool -o su")[0].1, "sub\\ dir/");
        assert_eq!(labels(&mut e, &env, "tool -o \"fi")[0].1, "\"file.txt\"");
    }

    #[test]
    fn enter_item_on_top_of_folders() {
        let (mut e, env) = engine();
        e.insert_spec(cmd("go-to", "").arg(arg("dir").folders()));
        let c = e.complete("go-to sub\\ dir/", &env, true, false);
        assert_eq!(c.items[0].kind, Kind::Execute);
        assert_eq!(c.items[0].label, "Enter");
        assert_eq!(c.items[0].description, "Run `go-to sub\\ dir/`");
        assert_eq!((c.items[1].kind, c.items[1].insert.as_str(), c.items[1].delete), (Kind::Back, "", 9));
        let c = e.complete("go-to ", &env, true, false);
        assert_eq!((c.items[0].kind, c.items[0].insert.as_str()), (Kind::Back, "../"));
        assert!(e.complete("go-to su", &env, true, false).items.iter().all(|s| s.kind != Kind::Execute));
        assert!(e.complete("tool -o sub\\ dir/", &env, true, false).items.iter().all(|s| s.kind != Kind::Execute));
    }

    #[test]
    fn parent_paths() {
        assert_eq!(parent_path("src/app/").as_deref(), Some("src/"));
        assert_eq!(parent_path("src/").as_deref(), Some(""));
        assert_eq!(parent_path("").as_deref(), Some("../"));
        assert_eq!(parent_path("../").as_deref(), Some("../../"));
        assert_eq!(parent_path("~/").as_deref(), Some("~/../"));
        assert_eq!(parent_path("/usr/lib/").as_deref(), Some("/usr/"));
        assert_eq!(parent_path("/usr/").as_deref(), Some("/"));
        assert_eq!(parent_path("/"), None);
        assert_eq!(parent_path("my\\ dir/sub/").as_deref(), Some("my\\ dir/"));
    }

    #[test]
    fn builtin_git() {
        let (mut e, env) = engine();
        let r = labels(&mut e, &env, "git chec");
        assert_eq!(r[0].0, "checkout");
        let r = labels(&mut e, &env, "sudo git commit --am");
        assert_eq!(r[0].0, "--amend");
    }

    #[test]
    fn builtin_php() {
        let (mut e, env) = engine();
        assert!(labels(&mut e, &env, "php --i").iter().any(|r| r.0 == "--ini"));
        assert!(labels(&mut e, &env, "php art").iter().any(|r| r.0 == "artisan"));
    }
}
