//! `lig ssh`: ssh with lig's shell integration for the remote zsh or bash, and a
//! multiplexed connection that lig runs completions over. Nothing is installed on the
//! host: the hooks go in a temp dir that the remote shell removes at its first prompt.
//!
//! `lig ssh` tells the lig session around it with `X;<pid>;<control socket>;<destination>`
//! before ssh starts and `Y` after it ends; in between, the remote shell's messages drive
//! the popup and commands for suggestions run on the host.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::exec::Cmd;

const POST_ZSH: &str = include_str!("../shell/post.zsh");
const POST_BASH: &str = include_str!("../shell/post.bash");
const BOOTSTRAP: &str = include_str!("../shell/remote.sh");
const ZSHENV: &str = include_str!("../shell/remote.zshenv");
const BASHRC: &str = include_str!("../shell/remote.bashrc");

/// A host the session's shell is logged in to through `lig ssh`.
#[derive(Debug)]
pub struct Remote {
    pub control: PathBuf,
    pub dest: String,
}

impl Remote {
    /// The local argv that runs `cmd` on the host, in `cwd`, with `env`, over the open connection.
    pub fn argv(&self, cmd: Cmd<'_>, cwd: &Path, env: &[(String, String)]) -> Vec<String> {
        let mut script = String::new();
        if !cwd.as_os_str().is_empty() {
            script += &format!("cd {} 2>/dev/null; ", quote(&cwd.to_string_lossy()));
        }
        script += "export LIG_GENERATOR=1";
        for (k, v) in env {
            script += &format!(" {k}={}", quote(v));
        }
        script += "; ";
        script += &match cmd {
            Cmd::Shell(s) => format!("command -v bash >/dev/null && exec bash -c {0}; exec sh -c {0}", quote(s)),
            Cmd::Argv(argv) => format!("exec {}", argv.iter().map(|a| quote(a)).collect::<Vec<_>>().join(" ")),
        };
        let control = self.control.to_string_lossy().into_owned();
        [
            "ssh",
            "-S",
            &control,
            "-o",
            "ControlMaster=no",
            "-o",
            "BatchMode=yes",
            "-o",
            "RemoteCommand=none",
            "-T",
            &self.dest,
            // Run by the user's login shell, whichever it is.
            &format!("sh -c {}", quote(&script)),
        ]
        .map(str::to_string)
        .to_vec()
    }
}

/// Single-quoted for a POSIX shell or fish; empty stays empty.
pub fn quote(s: &str) -> String {
    if s.is_empty() { String::new() } else { format!("'{}'", s.replace('\'', r"'\''")) }
}

/// `ls` of the folder in `path` (a typed path: `src/`, `~/x/`, `/etc/`), folders marked with `/`.
pub fn ls_command(path: &str) -> String {
    let dir = path.rfind('/').map_or("", |i| &path[..=i]);
    let quoted = match dir.strip_prefix("~/") {
        Some(rest) => format!("~/{}", quote(rest)),
        None => quote(dir),
    };
    format!("ls -1Ap -- {quoted}")
}

/// ssh options that take a value (`-p 22`), to find the destination after them.
const VALUE_OPTIONS: &str = "BbcDEeFIiJLlmOopQRSWw";

/// Index of the destination in ssh's arguments, if there's one and no remote command after it.
fn destination(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            return (i + 2 == args.len()).then_some(i + 1);
        }
        if let Some(flags) = a.strip_prefix('-').filter(|f| !f.is_empty()) {
            // `-p22` has its value attached; `-p 22` takes the next argument.
            if let Some(at) = flags.find(|c| VALUE_OPTIONS.contains(c))
                && at + 1 == flags.len()
            {
                i += 1;
            }
            i += 1;
            continue;
        }
        return (i + 1 == args.len()).then_some(i);
    }
    None
}

/// The remote command: decodes and runs the bootstrap with `sh`, whatever the login shell is.
fn remote_command() -> String {
    let script = BOOTSTRAP
        .replace("@ZSHENV@", ZSHENV)
        .replace("@POST_ZSH@", POST_ZSH)
        .replace("@BASHRC@", BASHRC)
        .replace("@POST_BASH@", POST_BASH);
    format!("echo {} | base64 -d | sh", base64(script.as_bytes()))
}

fn base64(bytes: &[u8]) -> String {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
        for i in 0..4 {
            out.push(if i <= chunk.len() { ABC[(n >> (18 - 6 * i) & 63) as usize] as char } else { '=' });
        }
    }
    out
}

fn in_lig_session() -> bool {
    let tty = unsafe { libc::ttyname(0) };
    !tty.is_null()
        && std::env::var("LIG_TTY").is_ok_and(|t| t.as_bytes() == unsafe { std::ffi::CStr::from_ptr(tty) }.to_bytes())
}

/// `lig ssh <ssh args>`: plain ssh outside lig, or with a remote command.
pub fn ssh(args: &[String]) -> Result<i32> {
    let Some(dest) = destination(args).filter(|_| in_lig_session()) else {
        use std::os::unix::process::CommandExt;
        return Err(std::process::Command::new("ssh").args(args).exec().into());
    };
    let dir = dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join("lig");
    std::fs::create_dir_all(&dir)?;
    let control = dir.join(format!("ssh-{}", std::process::id()));
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    write!(tty, "\x1b]6973;X;{};{};{}\x07", std::process::id(), control.display(), args[dest])?;
    let status = std::process::Command::new("ssh")
        .args(["-t", "-o", "ControlMaster=yes", "-o", "ControlPersist=no", "-o"])
        .arg(format!("ControlPath={}", control.display()))
        .args(args)
        .arg(remote_command())
        .status();
    let _ = write!(tty, "\x1b]6973;Y\x07");
    let _ = std::fs::remove_file(&control);
    Ok(status?.code().unwrap_or(255))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_string).collect()
    }

    #[test]
    fn base64_matches_the_tool() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn finds_the_destination() {
        assert_eq!(destination(&args("pi")), Some(0));
        assert_eq!(destination(&args("-p 2222 -A admin@pi")), Some(3));
        assert_eq!(destination(&args("-p2222 -i key pi")), Some(3));
        assert_eq!(destination(&args("-v -- pi")), Some(2));
        assert_eq!(destination(&args("pi uptime")), None);
        assert_eq!(destination(&args("-p 22")), None);
    }

    #[test]
    fn commands_run_in_the_shells_folder() {
        let r = Remote { control: "/run/s".into(), dest: "pi".into() };
        let argv: Vec<String> = ["git", "branch", "it's"].map(str::to_string).to_vec();
        let a = r.argv(Cmd::Argv(&argv), Path::new("/home/me/a b"), &[("PATH".into(), "/usr/bin".into())]);
        assert_eq!(a[..3], ["ssh", "-S", "/run/s"]);
        assert_eq!(a[a.len() - 2], "pi");
        let script =
            "cd '/home/me/a b' 2>/dev/null; export LIG_GENERATOR=1 PATH='/usr/bin'; exec 'git' 'branch' 'it'\\''s'";
        assert_eq!(a[a.len() - 1], format!("sh -c {}", quote(script)));
        assert_eq!(ls_command("~/Doc"), "ls -1Ap -- ~/");
        assert_eq!(ls_command("/etc/ss"), "ls -1Ap -- '/etc/'");
        assert_eq!(ls_command("x"), "ls -1Ap -- ");
        assert_eq!(ls_command("it's/a"), "ls -1Ap -- 'it'\\''s/'");
    }
}
