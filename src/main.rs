//! lig: Fig-style autocomplete for zsh, bash, fish, PowerShell and cmd, in any terminal.
//! `lig shell` ([`session`]) runs the shell in a PTY and draws the popup; [`engine`] makes the suggestions.

mod config;
mod engine;
mod exec;
mod help;
mod icons;
mod keys;
#[cfg(target_os = "linux")]
mod mirror;
#[cfg(target_os = "macos")]
#[path = "mirror_macos.rs"]
mod mirror;
#[cfg(unix)]
mod mirror_policy;
mod osc;
mod parser;
mod popup;
mod process;
#[cfg(target_os = "linux")]
mod procfs;
mod pty;
mod remote;
mod session;
mod spec;
mod specs;
mod store;
mod term;
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
const PRE_PWSH: &str = include_str!("../shell/pre.ps1");

#[derive(Parser)]
#[command(name = "lig", version, about = "Fig-style IDE autocomplete for your terminal")]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    #[command(hide = true)]
    ShellPath { pid: u32 },
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
    /// Print shell integration code for fish, zsh, bash or PowerShell
    Init {
        #[arg(value_enum)]
        shell: ShellKind,
        /// `pre` goes at the top of the rc file, `post` at the bottom; `all` does both
        #[arg(value_enum, default_value_t = Part::All)]
        part: Part,
    },
    /// Add lig to your fish, zsh, bash or PowerShell configuration (default: your shell); for
    /// cmd, add a Windows Terminal profile
    Install {
        #[arg(value_enum)]
        shell: Option<ShellKind>,
    },
    /// Remove lig from your fish, zsh, bash or PowerShell configuration, or cmd's profile
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
    /// ssh with lig's popup on the host (zsh or bash there; nothing is installed)
    Ssh {
        /// Arguments for ssh: options and the destination
        #[arg(trailing_var_arg = true, allow_hyphen_values = true, required = true)]
        args: Vec<String>,
    },
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

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ShellKind {
    Fish,
    Zsh,
    Bash,
    /// PowerShell 7
    Pwsh,
    /// Windows PowerShell 5.1
    Powershell,
    /// cmd.exe (a Windows Terminal profile; cmd has no configuration file)
    Cmd,
}

impl ShellKind {
    fn detect(shell: &str) -> Result<Self> {
        let name = Path::new(shell).file_name().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
        match name.strip_suffix(".exe").unwrap_or(&name) {
            "fish" => Ok(Self::Fish),
            "bash" => Ok(Self::Bash),
            "zsh" => Ok(Self::Zsh),
            "pwsh" => Ok(Self::Pwsh),
            "powershell" => Ok(Self::Powershell),
            "cmd" => Ok(Self::Cmd),
            _ => anyhow::bail!("unsupported shell {shell:?}; specify fish, bash, zsh, pwsh, powershell or cmd"),
        }
    }

    /// `$SHELL`; on Windows the shell lig was started from, or PowerShell.
    fn current() -> Result<Self> {
        #[cfg(windows)]
        {
            let parent = process::parent(std::process::id()).and_then(|(_, name)| Self::detect(&name).ok());
            Ok(parent.unwrap_or(if find_on_path("pwsh").is_some() { Self::Pwsh } else { Self::Powershell }))
        }
        #[cfg(unix)]
        Self::detect(&std::env::var("SHELL").unwrap_or_default())
    }

    fn name(self) -> &'static str {
        match self {
            ShellKind::Fish => "fish",
            ShellKind::Zsh => "zsh",
            ShellKind::Bash => "bash",
            ShellKind::Pwsh => "pwsh",
            ShellKind::Powershell => "powershell",
            ShellKind::Cmd => "cmd",
        }
    }

    fn powershell(self) -> bool {
        matches!(self, ShellKind::Pwsh | ShellKind::Powershell)
    }

    fn rc(self) -> PathBuf {
        let documents =
            || dirs::document_dir().unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join("Documents"));
        match self {
            ShellKind::Fish => {
                fish_rc(std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from), dirs::home_dir().unwrap_or_default())
            }
            ShellKind::Zsh => {
                let dir = std::env::var_os("ZDOTDIR").map(PathBuf::from).or_else(dirs::home_dir).unwrap_or_default();
                dir.join(".zshrc")
            }
            ShellKind::Bash => dirs::home_dir().unwrap_or_default().join(".bashrc"),
            ShellKind::Pwsh if cfg!(windows) => documents().join("PowerShell/Microsoft.PowerShell_profile.ps1"),
            ShellKind::Pwsh => {
                dirs::config_dir().unwrap_or_default().join("powershell/Microsoft.PowerShell_profile.ps1")
            }
            ShellKind::Powershell => documents().join("WindowsPowerShell/Microsoft.PowerShell_profile.ps1"),
            // A Windows Terminal profile fragment (see `cmd_profile`).
            ShellKind::Cmd => {
                dirs::data_local_dir().unwrap_or_default().join("Microsoft/Windows Terminal/Fragments/lig/lig.json")
            }
        }
    }
}

/// A program on `PATH`, trying `PATHEXT`'s extensions on Windows.
fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT").unwrap_or_else(|_| ".EXE".into()).split(';').map(str::to_string).collect()
    } else {
        vec![String::new()]
    };
    engine::path_dirs(&path)
        .flat_map(|d| exts.iter().map(move |e| Path::new(d).join(format!("{name}{e}"))))
        .find(|p| engine::command_name(p).is_some())
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
    /// Start a user spec from the one lig uses (built-in, or parsed from --help)
    New {
        command: String,
        /// Parse `<command> --help` even if there is a built-in spec
        #[arg(long)]
        from_help: bool,
        /// Replace an existing user spec
        #[arg(long)]
        force: bool,
    },
    /// Report what lig can't use in spec files (default: every user spec)
    Check {
        /// Spec files, or command names in the user specs folder
        specs: Vec<String>,
    },
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
        .map(|p| homebrew_opt(&p).filter(|opt| opt.exists()).unwrap_or(p))
        .map(|p| plain_path(&p.to_string_lossy()).to_string())
        .unwrap_or_else(|| "lig".into())
}

/// Windows' canonical paths start with `\\?\`, which shells and profiles don't expect.
fn plain_path(p: &str) -> &str {
    p.strip_prefix(r"\\?\").filter(|rest| rest.as_bytes().get(1) == Some(&b':')).unwrap_or(p)
}

/// A PowerShell single-quoted string.
fn ps_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Homebrew keeps each version in `Cellar/<formula>/<version>`, removed after an upgrade;
/// `opt/<formula>` always links to the current one.
fn homebrew_opt(exe: &Path) -> Option<PathBuf> {
    let parts: Vec<_> = exe.components().collect();
    let cellar = parts.iter().rposition(|c| c.as_os_str() == "Cellar")?;
    let formula = parts.get(cellar + 1)?;
    let rest = parts.get(cellar + 3..).filter(|rest| !rest.is_empty())?;
    let mut opt: PathBuf = parts[..cellar].iter().collect();
    opt.push("opt");
    opt.push(formula);
    opt.extend(rest);
    Some(opt)
}

/// The package manager that installed `exe`, and how to update lig with it.
fn package_manager(exe: &Path) -> Option<(&'static str, &'static str)> {
    if homebrew_opt(exe).is_some() {
        return Some(("Homebrew", "brew upgrade lig"));
    }
    if exe.starts_with("/nix/store") {
        return Some(("Nix", "update the flake input or channel it comes from"));
    }
    if !exe.starts_with("/usr") || exe.starts_with("/usr/local") {
        return None;
    }
    [
        ("dpkg", "-S", "apt", "sudo apt update && sudo apt install --only-upgrade lig"),
        ("rpm", "-qf", "dnf", "sudo dnf upgrade lig"),
        ("pacman", "-Qo", "pacman", "your AUR helper, e.g. yay -Syu"),
    ]
    .into_iter()
    .find(|(tool, arg, ..)| {
        std::process::Command::new(tool)
            .arg(arg)
            .arg(exe)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
    .map(|(_, _, name, how)| (name, how))
}

fn init_script(shell: ShellKind, part: Part) -> String {
    // PowerShell's integration is loaded by lig itself (see shell/lig.ps1); cmd has none.
    let (pre, post) = match shell {
        ShellKind::Fish => (PRE_FISH, POST_FISH),
        ShellKind::Zsh => (PRE_ZSH, POST_ZSH),
        ShellKind::Bash => (PRE_BASH, POST_BASH),
        ShellKind::Pwsh | ShellKind::Powershell => (PRE_PWSH, ""),
        ShellKind::Cmd => ("", ""),
    };
    let mut s = String::new();
    if part != Part::Post {
        s.push_str(pre);
    }
    if part != Part::Pre {
        s.push_str(post);
    }
    let bin = match shell {
        ShellKind::Fish => fish_quote(&exe()),
        // Inside '…' in pre.ps1.
        ShellKind::Pwsh | ShellKind::Powershell => exe().replace('\'', "''"),
        _ => exe(),
    };
    s.replace("@LIG_BIN@", &bin)
}

// Markers around the blocks `lig install` adds to the rc file.
const PRE_BEGIN: &str = "# >>> lig pre >>>";
const PRE_END: &str = "# <<< lig pre <<<";
const POST_BEGIN: &str = "# >>> lig post >>>";
const POST_END: &str = "# <<< lig post <<<";
// macOS terminals start bash as a login shell, which skips ~/.bashrc unless the login file sources it.
const LOGIN_BEGIN: &str = "# >>> lig bashrc >>>";
const LOGIN_END: &str = "# <<< lig bashrc <<<";

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
    if shell.powershell() {
        // Only the top: lig loads the rest itself.
        let line = format!("Invoke-Expression (& {} init {sh} pre | Out-String)", ps_quote(bin));
        return Ok(format!("{PRE_BEGIN}\n{line}\n{PRE_END}\n{body}"));
    }
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

/// The file a login bash reads, in bash's order; `.bash_profile` when none exists.
fn bash_login_file(home: &Path) -> PathBuf {
    [".bash_profile", ".bash_login", ".profile"]
        .iter()
        .map(|n| home.join(n))
        .find(|p| p.exists())
        .unwrap_or_else(|| home.join(".bash_profile"))
}

fn login_text(old: &str) -> String {
    let body = strip_block(old, LOGIN_BEGIN, LOGIN_END);
    if body.contains(".bashrc") {
        return body;
    }
    let sep = if body.is_empty() || body.ends_with('\n') { "" } else { "\n" };
    format!("{body}{sep}{LOGIN_BEGIN}\n[ -r ~/.bashrc ] && . ~/.bashrc\n{LOGIN_END}\n")
}

/// Makes a login bash source `~/.bashrc`, where lig is installed.
fn install_bash_login(login: &Path) -> Result<()> {
    let old = match std::fs::read_to_string(login) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.into()),
    };
    let new = login_text(&old);
    if new != old {
        std::fs::write(login, new).with_context(|| format!("writing {}", login.display()))?;
        println!("lig: updated {} so login shells read ~/.bashrc", login.display());
    }
    Ok(())
}

/// A Windows Terminal profile that runs cmd inside lig (cmd reads no configuration file).
fn cmd_profile(bin: &str) -> String {
    let profile = serde_json::json!({
        "profiles": [{
            "name": "Command Prompt (lig)",
            "commandline": format!("\"{bin}\" shell --shell cmd.exe"),
            "icon": "ms-appx:///ProfileIcons/{0caa0dad-35be-5f56-a8ff-afceeeaa6101}.png",
            "startingDirectory": "%USERPROFILE%",
        }]
    });
    serde_json::to_string_pretty(&profile).unwrap_or_default() + "\n"
}

fn install(shell: ShellKind) -> Result<()> {
    let rc = shell.rc();
    if shell == ShellKind::Cmd {
        std::fs::create_dir_all(rc.parent().context("no folder for the profile")?)?;
        std::fs::write(&rc, cmd_profile(&exe()))?;
        println!("lig: added the Windows Terminal profile \"Command Prompt (lig)\" ({})", rc.display());
        println!("lig: open it from Windows Terminal's new-tab menu (restart Windows Terminal if it isn't there).");
        return Ok(());
    }
    install_at(shell, &rc, &exe())?;
    println!("lig: updated {}", rc.display());
    if cfg!(target_os = "macos") && matches!(shell, ShellKind::Bash) {
        install_bash_login(&bash_login_file(&dirs::home_dir().unwrap_or_default()))?;
    }
    if shell.powershell() {
        println!("lig: open a new terminal to start using it.");
    } else {
        println!("lig: open a new terminal (or run `exec {}`) to start using it.", shell.name());
    }
    Ok(())
}

fn uninstall(shell: ShellKind) -> Result<()> {
    let rc = shell.rc();
    if shell == ShellKind::Cmd {
        if rc.exists() {
            std::fs::remove_file(&rc)?;
            println!("lig: removed {}", rc.display());
        }
        return Ok(());
    }
    let old = match std::fs::read_to_string(&rc) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    std::fs::write(&rc, managed_body(&old)?)?;
    println!("lig: removed from {}", rc.display());
    if cfg!(target_os = "macos") && matches!(shell, ShellKind::Bash) {
        let login = bash_login_file(&dirs::home_dir().unwrap_or_default());
        if let Ok(text) = std::fs::read_to_string(&login) {
            std::fs::write(&login, strip_block(&text, LOGIN_BEGIN, LOGIN_END))?;
        }
    }
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

fn release_target(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Some("aarch64-unknown-linux-musl"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        ("windows", "aarch64") => Some("aarch64-pc-windows-msvc"),
        _ => None,
    }
}

/// Checks `archive` in `dir` against its `.sha256` file, with the system's tool.
fn verify_checksum(dir: &Path, archive: &str) -> Result<()> {
    if cfg!(windows) {
        let expected = std::fs::read_to_string(dir.join(format!("{archive}.sha256")))?;
        let expected = expected.split_whitespace().next().unwrap_or_default().to_lowercase();
        let out =
            std::process::Command::new("certutil").arg("-hashfile").arg(dir.join(archive)).arg("SHA256").output()?;
        let text = String::from_utf8_lossy(&out.stdout).to_lowercase();
        // The hash is the line of hex digits (older Windows spaces them out).
        let actual = text
            .lines()
            .map(|l| l.replace(' ', ""))
            .find(|l| l.len() == 64 && l.bytes().all(|b| b.is_ascii_hexdigit()));
        anyhow::ensure!(actual.as_deref() == Some(expected.as_str()), "the download doesn't match its checksum");
        return Ok(());
    }
    let mut cmd = if cfg!(target_os = "macos") {
        let mut cmd = std::process::Command::new("shasum");
        cmd.args(["-a", "256"]);
        cmd
    } else {
        std::process::Command::new("sha256sum")
    };
    run(cmd.arg("-c").arg(format!("{archive}.sha256")).current_dir(dir))
        .context("the download doesn't match its checksum")
}

fn update(check: bool, force: bool) -> Result<()> {
    use std::process::Command;
    let current = env!("CARGO_PKG_VERSION");
    let target = std::env::current_exe()?.canonicalize()?;
    let manager = package_manager(&target);
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
        let how = manager.map_or("lig update", |(_, how)| how);
        println!("lig: {latest} is available (you have {current}). Run `{how}` to install it.");
        return Ok(());
    }
    if let Some((name, how)) = manager {
        anyhow::bail!("lig was installed with {name}, so update it there: {how}");
    }

    let platform = release_target(std::env::consts::OS, std::env::consts::ARCH)
        .context("no prebuilt lig for this platform; build from source instead")?;
    let name = format!("lig-{latest}-{platform}");
    let asset = |file: &str| {
        release["assets"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|a| a["name"] == file)
            .and_then(|a| a["browser_download_url"].as_str().map(str::to_string))
    };
    let archive = if cfg!(windows) { format!("{name}.zip") } else { format!("{name}.tar.gz") };
    let url = asset(&archive).with_context(|| format!("{tag} has no {archive}"))?;
    let sum_url = asset(&format!("{archive}.sha256")).with_context(|| format!("{tag} has no checksum"))?;

    // Next to the binary being replaced, so the final rename stays on one filesystem.
    let dir = target.parent().context("lig has no parent folder")?;
    let work = tempfile_dir(dir).with_context(|| {
        format!("can't write to {} (install the update with sudo, or as the owner of that folder)", dir.display())
    })?;
    let result = (|| -> Result<()> {
        println!("lig: downloading {archive}");
        run(Command::new("curl").args(["-fsSL", "--max-time", "300", "-o"]).arg(work.join(&archive)).arg(&url))?;
        std::fs::write(work.join(format!("{archive}.sha256")), curl(&[&sum_url])?)?;
        verify_checksum(&work, &archive)?;
        // Windows' tar also reads zip files.
        run(Command::new("tar").args(["xf", &archive]).current_dir(&work))?;
        let new = work.join(&name).join(if cfg!(windows) { "lig.exe" } else { "lig" });
        let version = Command::new(&new).arg("--version").output().context("the new lig doesn't run")?;
        anyhow::ensure!(version.status.success(), "the new lig doesn't run");
        if cfg!(windows) {
            // A running program can't be replaced on Windows, but it can be renamed.
            let old = target.with_extension("old.exe");
            let _ = std::fs::remove_file(&old);
            std::fs::rename(&target, &old).with_context(|| format!("moving {} aside", target.display()))?;
        }
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
#[cfg(all(unix, any(target_os = "linux", test)))]
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

#[cfg(all(unix, any(target_os = "linux", test)))]
fn shows_doctor(cmdline: &[u8]) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let mut args = cmdline.split(|&b| b == 0);
    let lig =
        args.next().is_some_and(|a| Path::new(std::ffi::OsStr::from_bytes(a)).file_name() == Some("lig".as_ref()));
    lig && args.next() == Some(b"doctor")
}

/// Polls for up to 1.5 s, until the mirror shows `lig doctor`.
#[cfg(target_os = "linux")]
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

/// Whether this command runs in a shell that lig started.
fn inside_lig() -> bool {
    #[cfg(unix)]
    {
        let tty = unsafe {
            let p = libc::ttyname(0);
            (!p.is_null()).then(|| std::ffi::CStr::from_ptr(p).to_string_lossy().into_owned())
        };
        tty.is_some() && std::env::var("LIG_TTY").ok() == tty
    }
    #[cfg(windows)]
    {
        // lig → the shell → this command.
        let lig = std::env::var("LIG_PID").ok().and_then(|p| p.parse::<u32>().ok());
        let shell = process::parent(std::process::id()).map(|(pid, _)| pid);
        lig.is_some() && shell.and_then(process::parent).map(|(pid, _)| pid) == lig
    }
}

fn doctor() -> Result<()> {
    let ok = |b: bool| if b { "ok  " } else { "FAIL" };
    let shells: &[ShellKind] = if cfg!(windows) {
        &[ShellKind::Pwsh, ShellKind::Powershell, ShellKind::Cmd]
    } else {
        &[ShellKind::Zsh, ShellKind::Bash, ShellKind::Fish]
    };
    for &shell in shells {
        let path = shell.rc();
        let rc = std::fs::read_to_string(&path).unwrap_or_default();
        let has = match shell {
            ShellKind::Cmd => path.exists(),
            s if s.powershell() => rc.contains(PRE_BEGIN),
            _ => rc.contains(PRE_BEGIN) && rc.contains(POST_BEGIN),
        };
        if has || ShellKind::current().is_ok_and(|current| shell == current) {
            println!("[{}] {} integration", ok(has), path.display());
        }
    }
    println!("[{}] this terminal is running inside lig", ok(inside_lig()));
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
    // Windows has no `ls` program to parse.
    if cfg!(unix) {
        let n = e.complete("ls --", &env, true, false).items.len();
        println!("[{}] --help parsing: `ls --` → {n} suggestions", ok(n > 0));
    }
    #[cfg(unix)]
    let lig_pid = std::env::var("LIG_PID").ok().and_then(|v| v.parse::<u32>().ok());
    #[cfg(target_os = "linux")]
    println!(
        "{}",
        mirror_status(
            lig_pid,
            std::env::var_os("ZELLIJ").is_some(),
            std::env::var_os("LIG_NO_MIRROR").is_some(),
            || lig_pid.and_then(read_lig_cmdline),
        )
    );
    #[cfg(target_os = "macos")]
    println!(
        "{}",
        mirror::doctor_status(
            lig_pid,
            std::env::var_os("ZELLIJ").is_some(),
            std::env::var_os("LIG_NO_MIRROR").is_some()
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

fn new_spec(command: &str, from_help: bool, force: bool) -> Result<()> {
    anyhow::ensure!(store::safe(command), "invalid command name {command:?}");
    let path = store::user_dir().join(format!("{command}.json"));
    for p in [&path, &path.with_extension("toml")] {
        anyhow::ensure!(force || !p.exists(), "{} already exists (--force replaces it)", p.display());
    }
    let builtin = if from_help { None } else { specs::builtin(command).map(std::sync::Arc::new) };
    let (spec, source) = match builtin {
        Some(s) => (s, "the built-in spec".to_string()),
        None => {
            let cfg = config::Config::load().behavior;
            let mut loader = help::HelpLoader::new(std::time::Duration::from_millis(cfg.help_timeout_ms), vec![]);
            match loader.lookup(&[command.to_string()], &std::env::current_dir()?, &[], true) {
                help::Lookup::Found(Some(s)) => (s, format!("`{command} --help`")),
                _ => anyhow::bail!(
                    "lig has no spec for {command}, and couldn't parse `{command} --help` or its man page"
                ),
            }
        }
    };
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(&path, serde_json::to_string_pretty(&spec::to_value(&spec))? + "\n")?;
    println!("lig: wrote {} from {source}", path.display());
    println!("lig: {command} completes from it now; check your edits with `lig specs check {command}`");
    Ok(())
}

/// Prints what each spec can't use; `false` if any has a problem.
fn check_specs(targets: &[String]) -> Result<bool> {
    let dir = store::user_dir();
    let files: Vec<PathBuf> = if targets.is_empty() {
        let mut f: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "json" || e == "toml"))
            .collect();
        f.sort();
        anyhow::ensure!(!f.is_empty(), "no user specs in {}", dir.display());
        f
    } else {
        targets
            .iter()
            .map(|t| {
                let named = ["json", "toml"].map(|e| dir.join(format!("{t}.{e}")));
                [PathBuf::from(t)]
                    .into_iter()
                    .chain(named)
                    .find(|p| p.is_file())
                    .with_context(|| format!("no spec file {t} (nor {t}.json/.toml in {})", dir.display()))
            })
            .collect::<Result<_>>()?
    };
    let mut ok = true;
    for path in files {
        let problems = check_spec_file(&path);
        if problems.is_empty() {
            println!("{}: ok", path.display());
        }
        for p in &problems {
            println!("{}: {p}", path.display());
        }
        ok &= problems.is_empty();
    }
    Ok(ok)
}

fn check_spec_file(path: &Path) -> Vec<String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) => return vec![e.to_string()],
    };
    let value: Result<serde_json::Value, String> = if path.extension().is_some_and(|e| e == "toml") {
        toml::from_str(&text).map_err(|e| e.to_string())
    } else {
        serde_json::from_str(&text).map_err(|e| e.to_string())
    };
    let value = match value {
        Ok(v) => v,
        Err(e) => return vec![e.trim().to_string()],
    };
    let mut problems = spec::check(&value);
    let stem = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    if problems.is_empty() && !spec::from_value(&value).meta.names.contains(&stem) {
        problems.push(format!("`name` doesn't include {stem:?}: lig finds specs by file name"));
    }
    problems
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
    let cols = term::size().map_or(100, |(_, cols)| cols);
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
    if cfg!(windows) {
        return find_on_path("pwsh").unwrap_or_else(|| "powershell.exe".into());
    }
    std::env::var_os("SHELL").map(PathBuf::from).filter(|p| Path::new(p).exists()).unwrap_or_else(|| "/bin/zsh".into())
}

/// Bytes of padding the zellij re-exec appends to `lig shell`'s argv.
#[cfg(unix)]
const ARGV_PAD: usize = 4096;

/// Re-exec `lig shell` with a pad positional so the zellij mirror has room for long
/// command lines. Detected by the positional, not an env var, so a nested lig doesn't inherit it.
#[cfg(unix)]
fn needs_reexec(args: &[std::ffi::OsString], zellij: bool, no_mirror: bool) -> bool {
    use std::os::unix::ffi::OsStrExt;
    let padded = args.iter().skip(2).any(|a| !a.is_empty() && a.as_bytes().iter().all(|&b| b == b' '));
    zellij && !no_mirror && args.get(1).is_some_and(|a| a == "shell") && !padded
}

fn main() -> Result<()> {
    #[cfg(unix)]
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    #[cfg(unix)]
    let zellij = std::env::var_os("ZELLIJ").is_some();
    #[cfg(unix)]
    if cfg!(target_os = "linux") && needs_reexec(&args, zellij, std::env::var_os("LIG_NO_MIRROR").is_some()) {
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
        Cmd::ShellPath { pid } => {
            let path = process::executable(pid).context("cannot resolve shell executable")?;
            println!("{}", path.display());
        }
        Cmd::Shell { shell, login, pad } => {
            if pad.is_some() {
                // The /proc/self/exe re-exec names the process "exe"; restore "lig" before spawning threads.
                // SAFETY: PR_SET_NAME reads a NUL-terminated string of at most 16 bytes.
                #[cfg(target_os = "linux")]
                unsafe {
                    libc::prctl(libc::PR_SET_NAME, c"lig".as_ptr())
                };
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
        Cmd::Specs { cmd: SpecsCmd::New { command, from_help, force } } => new_spec(&command, from_help, force)?,
        Cmd::Specs { cmd: SpecsCmd::Check { specs } } => {
            if !check_specs(&specs)? {
                std::process::exit(1);
            }
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
        Cmd::Ssh { args } => std::process::exit(remote::ssh(&args)?),
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

#[cfg(all(test, unix))]
mod shell_tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn homebrew_paths_point_at_opt() {
        let opt = |p: &str| homebrew_opt(Path::new(p));
        assert_eq!(opt("/opt/homebrew/Cellar/lig/1.2.0/bin/lig"), Some("/opt/homebrew/opt/lig/bin/lig".into()));
        assert_eq!(
            opt("/home/linuxbrew/.linuxbrew/Cellar/lig/1.2.0_1/bin/lig"),
            Some("/home/linuxbrew/.linuxbrew/opt/lig/bin/lig".into())
        );
        assert_eq!(opt("/opt/homebrew/Cellar/lig"), None);
        assert_eq!(opt("/home/me/.local/bin/lig"), None);
        assert_eq!(package_manager(Path::new("/home/me/.local/bin/lig")), None);
        assert_eq!(package_manager(Path::new("/usr/local/bin/lig")), None);
        assert_eq!(package_manager(Path::new("/opt/homebrew/Cellar/lig/1.2.0/bin/lig")).unwrap().0, "Homebrew");
    }

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
    fn release_targets_cover_linux_macos_and_windows() {
        assert_eq!(release_target("linux", "x86_64"), Some("x86_64-unknown-linux-musl"));
        assert_eq!(release_target("linux", "aarch64"), Some("aarch64-unknown-linux-musl"));
        assert_eq!(release_target("macos", "aarch64"), Some("aarch64-apple-darwin"));
        assert_eq!(release_target("macos", "x86_64"), Some("x86_64-apple-darwin"));
        assert_eq!(release_target("windows", "x86_64"), Some("x86_64-pc-windows-msvc"));
        assert_eq!(release_target("windows", "aarch64"), Some("aarch64-pc-windows-msvc"));
        assert_eq!(release_target("freebsd", "x86_64"), None);
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
            for sub in cli.get_subcommands().filter(|s| !s.is_hide_set()) {
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
    fn bash_login_sources_bashrc_once_and_uninstall_restores_it() {
        let user = "export PATH=/x:$PATH";
        let installed = login_text(user);
        assert!(installed.starts_with(user), "user content kept");
        assert!(installed.contains("[ -r ~/.bashrc ] && . ~/.bashrc"));
        assert_eq!(login_text(&installed), installed, "idempotent");
        assert_eq!(strip_block(&installed, LOGIN_BEGIN, LOGIN_END), format!("{user}\n"));
        let own = "[ -f ~/.bashrc ] && source ~/.bashrc\n";
        assert_eq!(login_text(own), own, "user's own bashrc hook is respected");
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
