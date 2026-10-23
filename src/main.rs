//! lig: Fig-style autocomplete for zsh, bash, and fish, in any terminal.
//!
//! `lig shell` ([`session`]) runs the shell in a PTY and draws the popup;
//! [`engine`] turns a command line into suggestions using the specs
//! ([`spec`], [`specs`]), `--help` output ([`help`]) and generators.

mod config;
mod engine;
mod exec;
mod help;
mod icons;
mod keys;
mod mirror;
mod osc;
mod parser;
mod popup;
mod procfs;
mod session;
mod spec;
mod specs;
mod store;
mod theme;

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};

const PRE_ZSH: &str = include_str!("../shell/pre.zsh");
const POST_ZSH: &str = include_str!("../shell/post.zsh");
const PRE_FISH: &str = include_str!("../shell/pre.fish");
const POST_FISH: &str = include_str!("../shell/post.fish");
const PRE_BASH: &str = include_str!("../shell/pre.bash");
const POST_BASH: &str = include_str!("../shell/post.bash");

#[derive(Parser)]
#[command(name = "lig", version, about = "Fig-style IDE autocomplete for your Linux terminal")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a shell inside lig (the shell integration does this for you)
    Shell {
        /// Shell to run (default: $SHELL)
        #[arg(long)]
        shell: Option<PathBuf>,
        /// Start it as a login shell
        #[arg(long)]
        login: bool,
        /// Spaces added by the zellij re-exec: room for the mirrored argv
        #[arg(hide = true, value_parser = parse_pad)]
        pad: Option<String>,
    },
    /// Print shell integration code for fish, zsh, or bash
    Init {
        #[arg(value_enum)]
        shell: ShellKind,
        /// `pre` goes at the top of the rc file, `post` at the bottom; `all` does both
        #[arg(value_enum, default_value_t = Part::All)]
        part: Part,
    },
    /// Add lig to your fish, zsh, or bash configuration (default: your $SHELL)
    Install {
        #[arg(value_enum)]
        shell: Option<ShellKind>,
    },
    /// Remove lig from your fish, zsh, or bash configuration (default: your $SHELL)
    Uninstall {
        #[arg(value_enum)]
        shell: Option<ShellKind>,
    },
    /// Manage completion specs
    Specs {
        #[command(subcommand)]
        cmd: SpecsCmd,
    },
    /// Print the suggestions for a command line (for debugging specs)
    Complete {
        /// Working directory to complete in
        #[arg(long)]
        cwd: Option<PathBuf>,
        /// Pretend this shell alias exists (`--alias gco='git checkout'`)
        #[arg(long = "alias", value_name = "NAME=VALUE")]
        aliases: Vec<String>,
        /// Text left of the cursor, e.g. "git checkout "
        line: String,
    },
    /// Check the installation
    Doctor,
    /// Update lig: pull its source and reinstall it with cargo
    Update {
        /// The source checkout to build (default: where this lig was built from)
        #[arg(long)]
        source: Option<PathBuf>,
    },
    /// List every icon (in the mode you use, next to its Unicode fallback)
    Icons,
    /// Show the name of each key you press, for the [keys] config
    Keys,
    /// Show a sample popup with your settings (or try others)
    Preview(PreviewArgs),
    /// Print the default configuration (save it to `lig config path`)
    Config {
        #[arg(value_enum, default_value_t = ConfigCmd::Default)]
        what: ConfigCmd,
    },
}

/// The hidden `lig shell` pad: only spaces, so `lig shell typo` is still an error.
fn parse_pad(s: &str) -> std::result::Result<String, String> {
    if !s.is_empty() && s.bytes().all(|b| b == b' ') {
        Ok(s.to_string())
    } else {
        Err(format!("unexpected argument '{s}'"))
    }
}

#[derive(Args)]
struct PreviewArgs {
    /// Theme: default, catppuccin, nord, dracula, gruvbox, tokyonight, light, terminal
    #[arg(long)]
    theme: Option<String>,
    /// rounded, square or none
    #[arg(long)]
    border: Option<String>,
    /// compact, normal or relaxed
    #[arg(long)]
    density: Option<String>,
    /// auto, nerd, unicode or none
    #[arg(long)]
    icons: Option<String>,
    /// auto, columns (60) or percent ("50%")
    #[arg(long)]
    width: Option<String>,
    /// rows (8) or percent ("30%")
    #[arg(long)]
    height: Option<String>,
    /// right or bottom
    #[arg(long)]
    description: Option<String>,
    /// Show every theme
    #[arg(long)]
    all: bool,
    /// Show every density
    #[arg(long)]
    densities: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum ShellKind {
    Fish,
    Zsh,
    Bash,
}

impl ShellKind {
    fn detect(shell: &str) -> Result<Self> {
        match Path::new(shell).file_name().and_then(|s| s.to_str()) {
            Some("fish") => Ok(Self::Fish),
            Some("bash") => Ok(Self::Bash),
            Some("zsh") => Ok(Self::Zsh),
            _ => anyhow::bail!("unsupported shell {shell:?}; specify fish, bash, or zsh"),
        }
    }

    fn current() -> Result<Self> {
        Self::detect(&std::env::var("SHELL").unwrap_or_default())
    }

    fn name(self) -> &'static str {
        match self {
            ShellKind::Fish => "fish",
            ShellKind::Zsh => "zsh",
            ShellKind::Bash => "bash",
        }
    }

    /// The rc file lig is installed into.
    fn rc(self) -> PathBuf {
        match self {
            ShellKind::Fish => {
                fish_rc(std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from), dirs::home_dir().unwrap_or_default())
            }
            ShellKind::Zsh => {
                let dir = std::env::var_os("ZDOTDIR").map(PathBuf::from).or_else(dirs::home_dir).unwrap_or_default();
                dir.join(".zshrc")
            }
            ShellKind::Bash => dirs::home_dir().unwrap_or_default().join(".bashrc"),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Part {
    Pre,
    Post,
    All,
}

#[derive(Clone, Copy, ValueEnum)]
enum ConfigCmd {
    Default,
    Path,
}

#[derive(Subcommand)]
enum SpecsCmd {
    /// List commands with a built-in or user spec
    List,
    /// Show where user specs and the --help cache live
    Path,
    /// Forget cached --help parsing results
    ClearCache,
    /// List the native generators spec files can use (`{"builtin": "<id>"}`)
    Generators,
}

/// Append a line to the debug log when `LIG_LOG` is set.
pub fn log(msg: &str) {
    if std::env::var_os("LIG_LOG").is_none() {
        return;
    }
    let dir = dirs::state_dir().unwrap_or_else(std::env::temp_dir).join("lig");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("lig.log")) {
        let _ = writeln!(f, "{msg}");
    }
}

/// This executable's path, for the shell scripts to run.
fn exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lig".into())
}

/// `lig init`: the integration script for `shell`.
fn init_script(shell: ShellKind, part: Part) -> String {
    let (pre, post) = match shell {
        ShellKind::Fish => (PRE_FISH, POST_FISH),
        ShellKind::Zsh => (PRE_ZSH, POST_ZSH),
        ShellKind::Bash => (PRE_BASH, POST_BASH),
    };
    let mut s = String::new();
    if part != Part::Post {
        s.push_str(pre);
    }
    if part != Part::Pre {
        s.push_str(post);
    }
    s.replace("@LIG_BIN@", &if matches!(shell, ShellKind::Fish) { fish_quote(&exe()) } else { exe() })
}

// Markers around the blocks `lig install` adds to the rc file.
const PRE_BEGIN: &str = "# >>> lig pre >>>";
const PRE_END: &str = "# <<< lig pre <<<";
const POST_BEGIN: &str = "# >>> lig post >>>";
const POST_END: &str = "# <<< lig post <<<";

/// `text` without the lines from `begin` to `end` (inclusive).
fn strip_block(text: &str, begin: &str, end: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    for line in text.split_inclusive('\n') {
        let t = line.trim_end();
        if t == begin {
            skipping = true;
        } else if t == end && skipping {
            skipping = false;
        } else if !skipping {
            out.push_str(line);
        }
    }
    out
}

/// Resolve fish's XDG configuration directory.
fn fish_rc(xdg: Option<PathBuf>, home: PathBuf) -> PathBuf {
    xdg.filter(|p| p.is_absolute()).unwrap_or_else(|| home.join(".config")).join("fish/config.fish")
}

fn fish_quote(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn managed_body(old: &str) -> Result<String> {
    let mut expected_end = None;
    for line in old.lines().map(str::trim_end) {
        if let Some(end) = match line {
            PRE_BEGIN => Some(PRE_END),
            POST_BEGIN => Some(POST_END),
            _ => None,
        } {
            anyhow::ensure!(expected_end.is_none(), "nested lig configuration block");
            expected_end = Some(end);
        } else if line == PRE_END || line == POST_END {
            anyhow::ensure!(expected_end == Some(line), "unmatched lig configuration marker");
            expected_end = None;
        }
    }
    anyhow::ensure!(expected_end.is_none(), "unterminated lig configuration block");
    Ok(strip_block(&strip_block(old, PRE_BEGIN, PRE_END), POST_BEGIN, POST_END))
}

fn installed_text(shell: ShellKind, old: &str, bin: &str) -> Result<String> {
    let body = managed_body(old)?;
    let sh = shell.name();
    let command = |part| {
        if matches!(shell, ShellKind::Fish) {
            format!("eval ({} init fish {part} | string collect)", fish_quote(bin))
        } else {
            format!("eval \"$(\"{bin}\" init {sh} {part})\"")
        }
    };
    Ok(format!(
        "{PRE_BEGIN}\n{}\n{PRE_END}\n{body}{}{POST_BEGIN}\n{}\n{POST_END}\n",
        command("pre"),
        if body.is_empty() || body.ends_with('\n') { "" } else { "\n" },
        command("post")
    ))
}

/// Add managed blocks and retain a backup of the existing file.
fn install_at(shell: ShellKind, rc: &Path, bin: &str) -> Result<()> {
    let old = match std::fs::read_to_string(rc) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let new = installed_text(shell, &old, bin)?;
    std::fs::create_dir_all(rc.parent().context("configuration has no parent")?)?;
    let backup = rc.with_file_name(format!("{}.lig-backup", rc.file_name().unwrap().to_string_lossy()));
    if rc.exists() && !backup.exists() {
        std::fs::copy(rc, &backup)?;
    }
    std::fs::write(rc, new).with_context(|| format!("writing {}", rc.display()))
}

fn install(shell: ShellKind) -> Result<()> {
    let rc = shell.rc();
    install_at(shell, &rc, &exe())?;
    println!("lig: updated {}", rc.display());
    println!("lig: open a new terminal (or run `exec {}`) to start using it.", shell.name());
    Ok(())
}

fn uninstall(shell: ShellKind) -> Result<()> {
    let rc = shell.rc();
    let old = match std::fs::read_to_string(&rc) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    std::fs::write(&rc, managed_body(&old)?)?;
    println!("lig: removed from {}", rc.display());
    Ok(())
}

/// Where this binary was built from; `lig update` rebuilds it from there.
const SOURCE_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Run a command, showing its output; fail if it fails.
fn run(cmd: &mut std::process::Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("running {:?}", cmd.get_program()))?;
    anyhow::ensure!(status.success(), "{:?} failed ({status})", cmd.get_program());
    Ok(())
}

/// `lig update`: pull the source checkout (when it tracks a remote branch) and
/// `cargo install` it. Without a checkout, install from the repository.
fn update(source: Option<PathBuf>) -> Result<()> {
    use std::process::Command;
    let old = env!("CARGO_PKG_VERSION");
    let dir = source.unwrap_or_else(|| PathBuf::from(SOURCE_DIR));
    let mut install = Command::new("cargo");
    install.args(["install", "--locked"]);
    if dir.join("Cargo.toml").exists() {
        let git = |args: &[&str]| Command::new("git").arg("-C").arg(&dir).args(args).output();
        let upstream = git(&["rev-parse", "--abbrev-ref", "@{upstream}"]).ok().filter(|o| o.status.success());
        match upstream {
            Some(o) => {
                let branch = String::from_utf8_lossy(&o.stdout).trim().to_string();
                println!("lig: pulling {branch} into {}", dir.display());
                run(Command::new("git").arg("-C").arg(&dir).args(["pull", "--ff-only"]))?;
            }
            None => println!("lig: {} doesn't track a remote branch; building it as it is", dir.display()),
        }
        println!("lig: building and installing from {}", dir.display());
        install.arg("--path").arg(&dir);
    } else {
        let repo = env!("CARGO_PKG_REPOSITORY");
        println!("lig: {} isn't there anymore; installing from {repo}", dir.display());
        install.args(["--git", repo]);
    }
    run(&mut install)?;
    // The freshly installed binary, where cargo put it.
    let bin = std::env::var_os("CARGO_INSTALL_ROOT")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("CARGO_HOME").map(PathBuf::from))
        .or_else(|| dirs::home_dir().map(|h| h.join(".cargo")))
        .unwrap_or_default()
        .join("bin/lig");
    let new = std::process::Command::new(&bin)
        .arg("--version")
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().trim_start_matches("lig ").to_string())
        .unwrap_or_else(|_| "?".into());
    let what = if new == old { format!("reinstalled {new}") } else { format!("updated {old} → {new}") };
    println!("lig: {what}. Open a new terminal tab to use it (open tabs keep the old version).");
    Ok(())
}

fn doctor() -> Result<()> {
    let ok = |b: bool| if b { "ok  " } else { "FAIL" };
    for shell in [ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish] {
        let path = shell.rc();
        let rc = std::fs::read_to_string(&path).unwrap_or_default();
        let has = rc.contains(PRE_BEGIN) && rc.contains(POST_BEGIN);
        if has || ShellKind::current().is_ok_and(|current| shell.name() == current.name()) {
            println!("[{}] {} integration", ok(has), path.display());
        }
    }
    let tty = unsafe {
        let p = libc::ttyname(0);
        (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
    };
    let inside = tty.is_some() && std::env::var("LIG_TTY").ok() == tty;
    println!("[{}] this terminal is running inside lig", ok(inside));
    let cfg = config::config_path();
    if cfg.exists() {
        let valid =
            std::fs::read_to_string(&cfg).ok().and_then(|t| toml::from_str::<config::Config>(&t).ok()).is_some();
        println!("[{}] config {}", ok(valid), cfg.display());
    } else {
        println!("[ok  ] config: defaults ({} not present)", cfg.display());
    }
    let mut e = engine::Engine::new(store::Store::default(), config::Config::load().behavior);
    let env =
        engine::ShellEnv { cwd: std::env::current_dir()?, path: std::env::var("PATH").ok(), ..Default::default() };
    let n = e.complete("git ", &env, false, false).items.len();
    println!("[{}] engine: `git ` → {n} suggestions", ok(n > 0));
    let n = e.complete("ls --", &env, true, false).items.len();
    println!("[{}] --help parsing: `ls --` → {n} suggestions", ok(n > 0));
    Ok(())
}

/// `lig complete`: print what the popup would show for `line`.
fn complete(line: &str, cwd: Option<PathBuf>, aliases: Vec<String>) -> Result<()> {
    let cwd = match cwd {
        Some(c) => c,
        None => std::env::current_dir()?,
    };
    let mut e = engine::Engine::new(store::Store::default(), config::Config::load().behavior);
    let env = engine::ShellEnv {
        cwd,
        path: std::env::var("PATH").ok(),
        aliases: std::sync::Arc::new(
            aliases
                .into_iter()
                .filter_map(|a| a.split_once('=').map(|(k, v)| (k.to_string(), v.to_string())))
                .collect(),
        ),
        ..Default::default()
    };
    let t = std::time::Instant::now();
    let c = e.complete(line, &env, true, true);
    let elapsed = t.elapsed();
    for s in &c.items {
        println!(
            "{:<11} {:<32} {:<28} {}",
            format!("{:?}", s.kind),
            s.label,
            format!("{:?}-{}", s.insert, s.delete),
            s.description
        );
    }
    eprintln!("{} suggestions in {:.1?}", c.items.len(), elapsed);
    Ok(())
}

/// `lig specs list`: every command with a spec, and where it comes from.
fn list_specs() {
    let mut all: BTreeMap<String, &str> = BTreeMap::new();
    for c in specs::FIG.names() {
        all.insert(c.to_string(), "fig");
    }
    for c in specs::lig_names() {
        let src = if all.contains_key(c) { "lig + fig" } else { "lig" };
        all.insert(c.to_string(), src);
    }
    for c in store::Store::default().commands() {
        all.insert(c, "user");
    }
    for (c, src) in &all {
        println!("{c:<32} {src}");
    }
    eprintln!("{} commands ({} lig + {} fig spec files embedded)", all.len(), specs::LIG.count(), specs::FIG.count());
}

/// `lig icons`: every icon in the current mode, with its Unicode fallback.
fn list_icons() {
    icons::detect();
    let style = config::Config::load().ui.icons;
    let nerd = icons::use_nerd(&style);
    println!("icons = \"{style}\" → using {}\n", if nerd { "Nerd Font glyphs" } else { "Unicode symbols" });
    let fallback = icons::legend(false);
    for (i, (g, c, what)) in icons::legend(nerd).into_iter().enumerate() {
        let alt = fallback.get(i).filter(|_| nerd).map_or(String::new(), |(ug, _, _)| format!("   (unicode: {ug})"));
        println!("  \x1b[{c}m{g}\x1b[0m  {what}{alt}");
    }
}

/// `lig preview`: sample popups with the config, overridden by the flags.
fn preview(args: PreviewArgs) {
    icons::detect();
    let mut ui = config::Config::load().ui;
    ui.theme = args.theme.unwrap_or(ui.theme);
    ui.border = args.border.unwrap_or(ui.border);
    ui.density = args.density.unwrap_or(ui.density);
    ui.icons = args.icons.unwrap_or(ui.icons);
    if let Some(w) = args.width {
        ui.width = config::Size::Text(w);
    }
    if let Some(h) = args.height {
        ui.height = config::Size::Text(h);
    }
    if let Some(d) = args.description {
        ui.description_position = d;
    }
    let cols = unsafe {
        let mut ws: libc::winsize = std::mem::zeroed();
        if libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) == 0 && ws.ws_col > 0 { ws.ws_col } else { 100 }
    };
    if args.densities {
        for d in ["compact", "normal", "relaxed"] {
            ui.density = d.into();
            println!("\x1b[1m{d}\x1b[0m");
            println!("{}", popup::preview(&ui, cols));
        }
        return;
    }
    let names: Vec<String> =
        if args.all { theme::NAMES.iter().map(|s| s.to_string()).collect() } else { vec![ui.theme.clone()] };
    for name in names {
        ui.theme = name.clone();
        println!("\x1b[1m{name}\x1b[0m  (border = {}, density = {}, icons = {})", ui.border, ui.density, ui.icons);
        println!("{}", popup::preview(&ui, cols));
    }
}

fn default_shell() -> PathBuf {
    std::env::var_os("SHELL").map(PathBuf::from).filter(|p| Path::new(p).exists()).unwrap_or_else(|| "/bin/zsh".into())
}

/// Bytes of padding the zellij re-exec appends to `lig shell`'s argv.
const ARGV_PAD: usize = 4096;

/// Should `lig shell` re-exec itself with a pad positional, so the zellij
/// mirror has room for long command lines (see `mirror`)? Detection uses the
/// positional, never an env var, so a nested lig does not inherit it.
fn needs_reexec(args: &[std::ffi::OsString], zellij: bool, no_mirror: bool) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let padded = args.iter().skip(2).any(|a| !a.is_empty() && a.as_bytes().iter().all(|&b| b == b' '));
    zellij && !no_mirror && args.get(1).is_some_and(|a| a == "shell") && !padded
}

fn main() -> Result<()> {
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let zellij = std::env::var_os("ZELLIJ").is_some();
    if needs_reexec(&args, zellij, std::env::var_os("LIG_NO_MIRROR").is_some()) {
        use std::os::unix::process::CommandExt;
        let err = std::process::Command::new("/proc/self/exe")
            .arg0(&args[0])
            .args(&args[1..])
            .arg(" ".repeat(ARGV_PAD))
            .exec();
        log(&format!("mirror: re-exec with argv padding failed: {err}; continuing unpadded"));
    }
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Shell { shell, login, pad } => {
            if pad.is_some() {
                // Re-exec'd through /proc/self/exe, which names the process "exe"; restore
                // "lig" for ps, htop, pgrep -x and killall. Main thread, before any other.
                // SAFETY: PR_SET_NAME reads a NUL-terminated string of at most 16 bytes.
                unsafe { libc::prctl(libc::PR_SET_NAME, c"lig".as_ptr()) };
            }
            icons::detect_in_background();
            let code = session::run(shell.unwrap_or_else(default_shell), login)?;
            std::process::exit(code);
        }
        Cmd::Init { shell, part } => print!("{}", init_script(shell, part)),
        Cmd::Install { shell } => install(shell.map(Ok).unwrap_or_else(ShellKind::current)?)?,
        Cmd::Uninstall { shell } => uninstall(shell.map(Ok).unwrap_or_else(ShellKind::current)?)?,
        Cmd::Specs { cmd: SpecsCmd::List } => list_specs(),
        Cmd::Specs { cmd: SpecsCmd::Path } => {
            println!("user specs: {}", store::user_dir().display());
            println!("--help cache: {}", help::cache_dir().display());
        }
        Cmd::Specs { cmd: SpecsCmd::Generators } => {
            for (id, what) in specs::gens::BUILTIN_IDS {
                println!("{id:<30} {what}");
            }
            println!("{:<30} <prefix> then local paths (e.g. files:file://)", "files:<prefix>");
            println!("{:<30} names of a kubernetes resource type", "kube:names:<type>");
        }
        Cmd::Specs { cmd: SpecsCmd::ClearCache } => {
            let dir = help::cache_dir();
            if dir.exists() {
                std::fs::remove_dir_all(&dir)?;
            }
            println!("lig: cleared {}", dir.display());
        }
        Cmd::Complete { cwd, aliases, line } => complete(&line, cwd, aliases)?,
        Cmd::Doctor => doctor()?,
        Cmd::Update { source } => update(source)?,
        Cmd::Icons => list_icons(),
        Cmd::Keys => keys::show(&keys::Bindings::new(&config::Config::load().keys))?,
        Cmd::Preview(args) => preview(args),
        Cmd::Config { what: ConfigCmd::Path } => println!("{}", config::config_path().display()),
        Cmd::Config { what: ConfigCmd::Default } => {
            print!("{}", toml::to_string_pretty(&config::Config::default())?);
        }
    }
    Ok(())
}

#[cfg(test)]
mod shell_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn os(v: &[&str]) -> Vec<std::ffi::OsString> {
        v.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn needs_reexec_only_for_shell_in_zellij() {
        let a = os(&["lig", "shell", "--shell", "/usr/bin/zsh"]);
        assert!(needs_reexec(&a, true, false));
        assert!(!needs_reexec(&a, false, false));
        assert!(!needs_reexec(&a, true, true));
        assert!(!needs_reexec(&os(&["lig", "doctor"]), true, false));
    }

    #[test]
    fn shell_pad_accepts_only_spaces() {
        let pad = " ".repeat(4096);
        assert!(Cli::try_parse_from(["lig", "shell", "--shell", "/usr/bin/zsh", pad.as_str()]).is_ok());
        assert!(Cli::try_parse_from(["lig", "shell"]).is_ok());
        assert!(Cli::try_parse_from(["lig", "shell", "typo"]).is_err());
        assert!(Cli::try_parse_from(["lig", "shell", " x "]).is_err());
        assert!(Cli::try_parse_from(["lig", "shell", ""]).is_err());
    }

    #[test]
    fn needs_reexec_false_when_padded() {
        let pad = " ".repeat(4096);
        let a = os(&["lig", "shell", "--shell", "/usr/bin/zsh", &pad]);
        assert!(!needs_reexec(&a, true, false));
    }

    /// `specs/lig/lig.json` completes lig itself: every command and flag of
    /// the CLI must be in it.
    #[test]
    fn lig_spec_matches_the_cli() {
        use clap::CommandFactory;
        fn check(cli: &clap::Command, spec: &spec::Subcommand, path: &str) {
            for a in cli.get_arguments() {
                if let Some(long) = a.get_long() {
                    let flag = format!("--{long}");
                    assert!(
                        spec.options.iter().any(|o| o.meta.names.contains(&flag)),
                        "specs/lig/lig.json lacks `{path} {flag}`"
                    );
                }
            }
            for sub in cli.get_subcommands() {
                let name = sub.get_name();
                let s =
                    spec.find_subcommand(name).unwrap_or_else(|| panic!("specs/lig/lig.json lacks `{path} {name}`"));
                // clap's `help` repeats every command under it; nothing to complete there.
                if name != "help" {
                    check(sub, s, &format!("{path} {name}"));
                }
            }
        }
        let mut cli = Cli::command();
        cli.build();
        check(&cli, &specs::builtin("lig").expect("specs/lig/lig.json"), "lig");
    }

    #[test]
    fn detection_paths_and_quoting() {
        assert!(matches!(ShellKind::detect("/usr/bin/fish").unwrap(), ShellKind::Fish));
        assert!(matches!(ShellKind::detect("bash").unwrap(), ShellKind::Bash));
        assert!(matches!(ShellKind::detect("/bin/zsh").unwrap(), ShellKind::Zsh));
        assert!(ShellKind::detect("/bin/notfish").is_err());
        assert_eq!(fish_rc(Some("/custom".into()), "/home/test".into()), PathBuf::from("/custom/fish/config.fish"));
        assert_eq!(
            fish_rc(Some("relative".into()), "/home/test".into()),
            PathBuf::from("/home/test/.config/fish/config.fish")
        );
        assert_eq!(fish_quote("a'b\\c"), "'a\\'b\\\\c'");
        for part in [Part::Pre, Part::Post, Part::All] {
            let script = init_script(ShellKind::Fish, part);
            assert!(!script.contains("@LIG_BIN@"));
            assert!(!script.contains("$("));
        }
    }

    #[test]
    fn install_preserves_content_backup_and_permissions() {
        let dir = std::env::temp_dir().join(format!(
            "lig-install-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let rc = dir.join("fish/config.fish");
        install_at(ShellKind::Fish, &rc, "/tmp/a 'quoted' lig").unwrap();
        let first = std::fs::read_to_string(&rc).unwrap();
        install_at(ShellKind::Fish, &rc, "/tmp/a 'quoted' lig").unwrap();
        assert_eq!(first, std::fs::read_to_string(&rc).unwrap());
        let body = "# user config\nset -g thing 'hello'\n\n";
        std::fs::write(&rc, body).unwrap();
        let backup = rc.with_file_name("config.fish.lig-backup");
        // A backup from the first existing-file install must not be overwritten.
        let existing_backup = std::fs::read(&backup).unwrap();
        std::fs::set_permissions(&rc, std::fs::Permissions::from_mode(0o600)).unwrap();
        install_at(ShellKind::Fish, &rc, "/tmp/lig").unwrap();
        assert_eq!(std::fs::read(&backup).unwrap(), existing_backup);
        assert_eq!(managed_body(&std::fs::read_to_string(&rc).unwrap()).unwrap(), body);
        assert_eq!(std::fs::metadata(&rc).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn malformed_blocks_are_rejected() {
        for body in [
            format!("{PRE_BEGIN}\nkeep this\n"),
            format!("{POST_END}\n"),
            format!("{PRE_BEGIN}  \nkeep this\n"),
            format!("{PRE_BEGIN}\n{POST_BEGIN}\n{PRE_END}\n{POST_END}\n"),
            format!("{PRE_BEGIN}\n{PRE_BEGIN}\n{PRE_END}\n"),
        ] {
            assert!(installed_text(ShellKind::Fish, &body, "lig").is_err());
        }
        assert_eq!(managed_body("ordinary content").unwrap(), "ordinary content");
    }
}
