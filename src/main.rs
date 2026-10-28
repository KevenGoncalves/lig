//! lig: Fig-style autocomplete for zsh, bash, and fish, in any terminal.
//! `lig shell` ([`session`]) runs the shell in a PTY and draws the popup; [`engine`] makes the suggestions.

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
    /// Update lig to the latest release on GitHub
    Update {
        /// Only check whether a newer release is available
        #[arg(long)]
        check: bool,
        /// Reinstall even if this is already the latest release
        #[arg(long)]
        force: bool,
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

fn exe() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.canonicalize().ok())
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|| "lig".into())
}

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

/// A relative `$XDG_CONFIG_HOME` is ignored.
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

/// Backs up the original rc file once, as `<rc>.lig-backup`.
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

fn run(cmd: &mut std::process::Command) -> Result<()> {
    let status = cmd.status().with_context(|| format!("running {:?}", cmd.get_program()))?;
    anyhow::ensure!(status.success(), "{:?} failed ({status})", cmd.get_program());
    Ok(())
}

/// A version as numbers, for comparing (`v1.0.10` > `1.0.9`).
fn version_key(v: &str) -> Vec<u64> {
    v.trim_start_matches('v').split(['.', '-']).map_while(|p| p.parse().ok()).collect()
}

/// Installs the static (musl) release build, which runs on any Linux, after checking its sha256.
fn update(check: bool, force: bool) -> Result<()> {
    use std::process::Command;
    let current = env!("CARGO_PKG_VERSION");
    let repo = env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/");
    let curl = |args: &[&str]| -> Result<String> {
        let out = Command::new("curl")
            .args(["-fsSL", "--connect-timeout", "10", "--max-time", "60"])
            .args(args)
            .output()
            .context("running curl (is it installed?)")?;
        anyhow::ensure!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr).trim());
        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    };

    let release: serde_json::Value = serde_json::from_str(
        &curl(&[
            "-H",
            "Accept: application/vnd.github+json",
            &format!("https://api.github.com/repos/{repo}/releases/latest"),
        ])
        .context("looking up the latest release")?,
    )?;
    let tag = release["tag_name"].as_str().context("the latest release has no tag")?;
    let latest = tag.trim_start_matches('v');
    if version_key(latest) <= version_key(current) && !force {
        let note = if version_key(latest) < version_key(current) { " (newer than the latest release)" } else { "" };
        println!("lig: {current} is up to date{note}.");
        return Ok(());
    }
    if check {
        println!("lig: {latest} is available (you have {current}). Run `lig update` to install it.");
        return Ok(());
    }

    let arch = match std::env::consts::ARCH {
        a @ ("x86_64" | "aarch64") => a,
        other => anyhow::bail!("there's no prebuilt lig for {other}; build it from source instead"),
    };
    let name = format!("lig-{latest}-{arch}-unknown-linux-musl");
    let asset = |file: &str| {
        release["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["name"] == file)
            .and_then(|a| a["browser_download_url"].as_str().map(str::to_string))
    };
    let archive = format!("{name}.tar.gz");
    let url = asset(&archive).with_context(|| format!("{tag} has no {archive}"))?;
    let sum_url = asset(&format!("{archive}.sha256")).with_context(|| format!("{tag} has no checksum"))?;

    // Next to the binary being replaced, so the final rename stays on one filesystem.
    let target = std::env::current_exe()?.canonicalize()?;
    let dir = target.parent().context("lig has no parent folder")?;
    let work = tempfile_dir(dir).with_context(|| {
        format!("can't write to {} (install the update with sudo, or as the owner of that folder)", dir.display())
    })?;
    let result = (|| -> Result<()> {
        println!("lig: downloading {archive}");
        run(Command::new("curl").args(["-fsSL", "--max-time", "300", "-o"]).arg(work.join(&archive)).arg(&url))?;
        std::fs::write(work.join(format!("{archive}.sha256")), curl(&[&sum_url])?)?;
        run(Command::new("sha256sum").args(["--quiet", "-c", &format!("{archive}.sha256")]).current_dir(&work))
            .context("the download doesn't match its checksum")?;
        run(Command::new("tar").args(["xzf", &archive]).current_dir(&work))?;
        let new = work.join(&name).join("lig");
        let version = Command::new(&new).arg("--version").output().context("the new lig doesn't run")?;
        anyhow::ensure!(version.status.success(), "the new lig doesn't run");
        std::fs::rename(&new, &target).with_context(|| format!("replacing {}", target.display()))?;
        Ok(())
    })();
    let _ = std::fs::remove_dir_all(&work);
    result?;
    let what =
        if latest == current { format!("reinstalled {latest}") } else { format!("updated {current} → {latest}") };
    println!("lig: {what} ({}). Open a new terminal tab to use it (open tabs keep the old version).", target.display());
    Ok(())
}

fn tempfile_dir(parent: &Path) -> std::io::Result<PathBuf> {
    let dir = parent.join(format!(".lig-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir(&dir)?;
    Ok(dir)
}

/// `read` returns the lig shell's `/proc/<pid>/cmdline`, already polled.
fn mirror_status(lig_pid: Option<u32>, zellij: bool, no_mirror: bool, read: impl Fn() -> Option<Vec<u8>>) -> String {
    if !zellij {
        return "[off ] zellij mirror: not in zellij".into();
    }
    if no_mirror {
        return "[off ] zellij mirror: LIG_NO_MIRROR set".into();
    }
    if lig_pid.is_none() {
        return "[off ] zellij mirror: not in a lig pane".into();
    }
    match lig_pid.and_then(|_| read()) {
        Some(c) if shows_doctor(&c) => format!("[ok  ] zellij mirror: on, argv area {} bytes", c.len()),
        _ => "[off ] zellij mirror: not mirroring (see LIG_LOG)".into(),
    }
}

fn shows_doctor(cmdline: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let mut args = cmdline.split(|&b| b == 0);
    let lig =
        args.next().is_some_and(|a| Path::new(std::ffi::OsStr::from_bytes(a)).file_name() == Some("lig".as_ref()));
    lig && args.next() == Some(b"doctor")
}

/// Polls for up to 1.5 s, until the mirror shows `lig doctor`.
fn read_lig_cmdline(pid: u32) -> Option<Vec<u8>> {
    let mut last = None;
    for _ in 0..15 {
        last = std::fs::read(format!("/proc/{pid}/cmdline")).ok();
        if last.as_deref().is_some_and(shows_doctor) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    last
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
    let lig_pid = std::env::var("LIG_PID").ok().and_then(|v| v.parse::<u32>().ok());
    println!(
        "{}",
        mirror_status(
            lig_pid,
            std::env::var_os("ZELLIJ").is_some(),
            std::env::var_os("LIG_NO_MIRROR").is_some(),
            || lig_pid.and_then(read_lig_cmdline),
        )
    );
    Ok(())
}

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

fn list_icons() {
    icons::detect();
    let cfg = config::Config::load();
    icons::set_overrides(&cfg.icons);
    let style = cfg.ui.icons;
    let nerd = icons::use_nerd(&style);
    println!("icons = \"{style}\" → using {}\n", if nerd { "Nerd Font glyphs" } else { "Unicode symbols" });
    let fallback = icons::legend(false);
    for (i, (g, c, name, what)) in icons::legend(nerd).into_iter().enumerate() {
        let alt = fallback.get(i).filter(|_| nerd).map_or(String::new(), |(ug, ..)| format!("   (unicode: {ug})"));
        println!("  \x1b[{c}m{g}\x1b[0m  {name:<12} {what}{alt}");
    }
    println!("\nChange any of them in the [icons] section of {}.", config::config_path().display());
}

fn preview(args: PreviewArgs) {
    icons::detect();
    let cfg = config::Config::load();
    icons::set_overrides(&cfg.icons);
    let mut ui = cfg.ui;
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

/// Re-exec `lig shell` with a pad positional so the zellij mirror has room for long
/// command lines. Detected by the positional, not an env var, so a nested lig doesn't inherit it.
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
                // The /proc/self/exe re-exec names the process "exe"; restore "lig" before spawning threads.
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
        Cmd::Update { check, force } => update(check, force)?,
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
    fn mirror_status_off_outside_zellij() {
        assert_eq!(mirror_status(Some(1), false, false, || None), "[off ] zellij mirror: not in zellij");
    }

    #[test]
    fn mirror_status_off_when_disabled() {
        assert_eq!(mirror_status(Some(1), true, true, || None), "[off ] zellij mirror: LIG_NO_MIRROR set");
    }

    #[test]
    fn mirror_status_on_when_cmdline_shows_doctor() {
        let mut area = b"lig\0doctor\0".to_vec();
        area.resize(4105, 0);
        assert_eq!(
            mirror_status(Some(1), true, false, || Some(area.clone())),
            "[ok  ] zellij mirror: on, argv area 4105 bytes"
        );
    }

    #[test]
    fn mirror_status_on_with_full_lig_path() {
        let mut area = b"/usr/bin/lig\0doctor\0".to_vec();
        area.resize(4105, 0);
        assert_eq!(
            mirror_status(Some(1), true, false, || Some(area.clone())),
            "[ok  ] zellij mirror: on, argv area 4105 bytes"
        );
        let other = b"/usr/bin/liggy\0doctor\0".to_vec();
        assert!(mirror_status(Some(1), true, false, || Some(other.clone())).contains("not mirroring"));
        let other = b"lig\0doctors\0".to_vec();
        assert!(mirror_status(Some(1), true, false, || Some(other.clone())).contains("not mirroring"));
    }

    #[test]
    fn mirror_status_not_mirroring() {
        let orig = b"lig\0shell\0--shell\0/usr/bin/zsh\0".to_vec();
        let want = "[off ] zellij mirror: not mirroring (see LIG_LOG)";
        assert_eq!(mirror_status(Some(1), true, false, || Some(orig.clone())), want);
        assert_eq!(mirror_status(Some(1), true, false, || None), want);
    }

    #[test]
    fn mirror_status_without_lig_pid_is_not_a_lig_pane() {
        let want = "[off ] zellij mirror: not in a lig pane";
        assert_eq!(mirror_status(None, true, false, || panic!("read without LIG_PID")), want);
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

    #[test]
    fn release_versions_compare_as_numbers() {
        assert!(version_key("v1.0.10") > version_key("1.0.9"));
        assert!(version_key("v1.1.0") > version_key("1.0.99"));
        assert_eq!(version_key("v1.0.5"), version_key("1.0.5"));
        assert!(version_key("2.0.0-rc1") > version_key("1.9.9"));
    }

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
