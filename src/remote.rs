//! `lig ssh`: ssh with lig's shell integration for the remote zsh or bash, and a
//! multiplexed connection that lig runs completions over. Nothing is installed on the
//! host: the hooks go in a temp dir that the remote shell removes at its first prompt.
//!
//! `lig ssh` tells the lig session around it with `X;<pid>;<control socket>;<destination>`
//! before ssh starts and `Y` after it ends; in between, the remote shell's messages drive
//! the popup and commands for suggestions run on the host.

#[cfg(unix)]
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::exec::Cmd;

const POST_ZSH: &str = include_str!("../shell/post.zsh");
const POST_BASH: &str = include_str!("../shell/post.bash");
const BOOTSTRAP: &str = include_str!("../shell/remote.sh");
const ZSHENV: &str = include_str!("../shell/remote.zshenv");
const BASHRC: &str = include_str!("../shell/remote.bashrc");

/// A host the session's shell is logged in to through `lig ssh` (or `lig mosh`).
#[derive(Debug, Clone)]
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
            // `quote` keeps an empty string empty, which would drop an empty argument.
            Cmd::Argv(argv) => format!(
                "exec {}",
                argv.iter().map(|a| if a.is_empty() { "''".into() } else { quote(a) }).collect::<Vec<_>>().join(" ")
            ),
        };
        let control = self.control.to_string_lossy().into_owned();
        [
            "ssh",
            "-S",
            &control,
            "-o",
            "ControlMaster=no",
            // Over the open connection only: without it ssh would log in afresh for each
            // suggestion, which is slow, and ProxyCommand runs only for a new connection.
            "-o",
            "ProxyCommand=false",
            "-o",
            "BatchMode=yes",
            "-o",
            "RemoteCommand=none",
            "-T",
            &self.dest,
            &login_command(&script),
        ]
        .map(str::to_string)
        .to_vec()
    }
}

/// `script` for `sh`, as a line any login shell runs the same way (sh, bash, zsh, fish, nu):
/// base64 has nothing to quote. `eval "$(cat)"` reads the whole script before running it, so
/// a command that reads its input can't take the rest of the script.
fn login_command(script: &str) -> String {
    format!("echo {} | base64 -d | sh -c 'eval \"$(cat)\" </dev/null'", base64(script.as_bytes()))
}

/// Single-quoted for `sh`; empty stays empty.
pub fn quote(s: &str) -> String {
    if s.is_empty() {
        return String::new();
    }
    format!("'{}'", s.replace('\'', r"'\''"))
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
const VALUE_OPTIONS: &str = "BbcDEeFIiJLlmOopPQRSWw";

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
/// With `out`, the hooks write their messages to that file (`lig mosh`).
fn remote_command(out: Option<&str>) -> String {
    let script = out.map(|f| format!("LIG_OUT={f}\n")).unwrap_or_default()
        + &BOOTSTRAP
            .replace("@ZSHENV@", ZSHENV)
            .replace("@POST_ZSH@", POST_ZSH)
            .replace("@BASHRC@", BASHRC)
            .replace("@POST_BASH@", POST_BASH);
    format!("echo {} | base64 -d | sh", base64(script.as_bytes()))
}

/// mosh options whose value can be the next argument (`-p 60001`, `--ssh "ssh -p 2222"`).
const MOSH_VALUE_OPTIONS: &[&str] = &[
    "-p",
    "--port",
    "--ssh",
    "--server",
    "--client",
    "--predict",
    "--family",
    "--bind-server",
    "--experimental-remote-ip",
];

/// Index of mosh's destination, if there's one and no remote command after it.
fn mosh_destination(args: &[String]) -> Option<usize> {
    let mut i = 0;
    while i < args.len() {
        let a = &args[i];
        if a == "--" {
            return (i + 2 == args.len()).then_some(i + 1);
        }
        if a.starts_with('-') && a.len() > 1 {
            // `--ssh=…` and `-p60001` carry their value.
            if MOSH_VALUE_OPTIONS.contains(&a.as_str()) {
                i += 1;
            }
            i += 1;
            continue;
        }
        return (i + 1 == args.len()).then_some(i);
    }
    None
}

/// The ssh command mosh logs in with (`--ssh`), as words.
fn mosh_ssh(args: &[String]) -> Vec<String> {
    let mut ssh = None;
    for (i, a) in args.iter().enumerate() {
        if a == "--" {
            break;
        }
        if let Some(v) = a.strip_prefix("--ssh=") {
            ssh = Some(v.to_string());
        } else if a == "--ssh" {
            ssh = args.get(i + 1).cloned();
        }
    }
    let words: Vec<String> = ssh.unwrap_or_default().split_whitespace().map(str::to_string).collect();
    if words.is_empty() { vec!["ssh".into()] } else { words }
}

/// The complete lig messages (`ESC ] 6973 ; … BEL`) at the start of `buf`, taken out of it.
fn take_messages(buf: &mut Vec<u8>) -> Vec<Vec<u8>> {
    let mut out = vec![];
    loop {
        let Some(start) = buf.windows(7).position(|w| w == b"\x1b]6973;") else {
            // Keep a start cut off at the end of the read.
            let keep = buf.iter().rposition(|&b| b == 0x1b).unwrap_or(buf.len());
            buf.drain(..keep);
            return out;
        };
        let Some(len) = buf[start..].iter().position(|&b| b == 0x07) else {
            buf.drain(..start);
            return out;
        };
        out.push(buf[start..=start + len].to_vec());
        buf.drain(..=start + len);
    }
}

/// [`base64`] back to bytes; whatever isn't base64 is skipped.
pub(crate) fn base64_decode(text: &str) -> Vec<u8> {
    const ABC: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = vec![];
    let (mut acc, mut bits) = (0u32, 0);
    for six in text.bytes().filter_map(|b| ABC.iter().position(|&c| c == b)) {
        acc = acc << 6 | six as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

pub(crate) fn base64(bytes: &[u8]) -> String {
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

#[cfg(unix)]
fn in_lig_session() -> bool {
    let tty = unsafe { libc::ttyname(0) };
    !tty.is_null()
        && std::env::var("LIG_TTY").is_ok_and(|t| t.as_bytes() == unsafe { std::ffi::CStr::from_ptr(tty) }.to_bytes())
}

/// Windows: plain ssh (the remote popup needs lig's Unix PTY handling).
#[cfg(windows)]
pub fn ssh(args: &[String]) -> Result<i32> {
    let _ = (destination(args), remote_command);
    Ok(std::process::Command::new("ssh").args(args).status()?.code().unwrap_or(255))
}

/// Windows: plain mosh.
#[cfg(windows)]
pub fn mosh(args: &[String]) -> Result<i32> {
    let _ = (mosh_destination(args), mosh_ssh, take_messages);
    Ok(std::process::Command::new("mosh").args(args).status()?.code().unwrap_or(255))
}

/// `lig ssh <ssh args>`: plain ssh outside lig, or with a remote command.
#[cfg(unix)]
pub fn ssh(args: &[String]) -> Result<i32> {
    let Some(dest) = destination(args).filter(|_| in_lig_session()) else {
        use std::os::unix::process::CommandExt;
        return Err(std::process::Command::new("ssh").args(args).exec().into());
    };
    let dir = match crate::private_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("lig: {e}; running plain ssh");
            use std::os::unix::process::CommandExt;
            return Err(std::process::Command::new("ssh").args(args).exec().into());
        }
    };
    let control = dir.join(format!("ssh-{}", std::process::id()));
    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    write!(tty, "\x1b]6973;X;{};{};{}\x07", std::process::id(), control.display(), args[dest])?;
    let status = std::process::Command::new("ssh")
        .args(["-t", "-o", "ControlMaster=yes", "-o", "ControlPersist=no", "-o"])
        .arg(format!("ControlPath={}", control.display()))
        .args(args)
        .arg(remote_command(None))
        .status();
    let _ = write!(tty, "\x1b]6973;Y\x07");
    let _ = std::fs::remove_file(&control);
    Ok(status?.code().unwrap_or(255))
}

/// `lig mosh <mosh args>`: plain mosh outside lig, or with a remote command. mosh drops the
/// hooks' messages (it sends the screen, not the bytes), so they go to a file on the host
/// that lig follows over an ssh connection of its own, which also runs what suggestions need.
/// zsh only: bash's line is read off the screen at marks mosh drops too.
#[cfg(unix)]
pub fn mosh(args: &[String]) -> Result<i32> {
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex, atomic::AtomicBool, atomic::Ordering};
    let plain = || Command::new("mosh").args(args).exec().into();
    let Some(dest) = mosh_destination(args).filter(|_| in_lig_session()) else {
        return Err(plain());
    };
    let dir = match crate::private_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("lig: {e}; running plain mosh");
            return Err(plain());
        }
    };
    let control = dir.join(format!("ssh-{}", std::process::id()));
    let mut id = [0u8; 8];
    let _ = std::fs::File::open("/dev/urandom").and_then(|mut f| std::io::Read::read_exact(&mut f, &mut id));
    let out = format!("/tmp/lig-mosh-{}", id.iter().map(|b| format!("{b:02x}")).collect::<String>());
    let host = Remote { control: control.clone(), dest: args[dest].clone() };

    // Never asks for a password: without keys or an agent, suggestions that run on the host are off.
    let ssh = mosh_ssh(args);
    let mut master = Command::new(&ssh[0])
        .args(&ssh[1..])
        .args(["-o", "ControlMaster=yes", "-o", "ControlPersist=no", "-o", "BatchMode=yes", "-N", "-T", "-o"])
        .arg(format!("ControlPath={}", control.display()))
        .arg(&args[dest])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .ok();

    let mut tty = std::fs::OpenOptions::new().write(true).open("/dev/tty")?;
    write!(tty, "\x1b]6973;X;{};{};{};mosh\x07", std::process::id(), control.display(), args[dest])?;

    // Follows the file from where it left off, connection after connection; each message
    // goes to the terminal in one write, so mosh's output can't split it.
    let done = Arc::new(AtomicBool::new(false));
    let tail: Arc<Mutex<Option<std::process::Child>>> = Arc::default();
    {
        let (done, tail, host, out, mut tty) =
            (done.clone(), tail.clone(), host.clone(), out.clone(), tty.try_clone()?);
        std::thread::spawn(move || {
            let mut read = 0usize;
            while !done.load(Ordering::Relaxed) {
                if !host.control.exists() {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
                let cmd = format!("tail -c +{} -F {out} 2>/dev/null", read + 1);
                let argv = host.argv(Cmd::Shell(&cmd), Path::new(""), &[]);
                let Ok(mut child) = Command::new(&argv[0])
                    .args(&argv[1..])
                    .stdin(Stdio::null())
                    .stdout(Stdio::piped())
                    .stderr(Stdio::null())
                    .spawn()
                else {
                    std::thread::sleep(std::time::Duration::from_secs(1));
                    continue;
                };
                let mut stdout = child.stdout.take().unwrap();
                *tail.lock().unwrap() = Some(child);
                let (mut buf, mut chunk) = (vec![], [0u8; 65536]);
                while let Ok(n) = std::io::Read::read(&mut stdout, &mut chunk) {
                    if n == 0 {
                        break;
                    }
                    read += n;
                    buf.extend_from_slice(&chunk[..n]);
                    for message in take_messages(&mut buf) {
                        let _ = tty.write_all(&message);
                    }
                }
                if let Some(mut child) = tail.lock().unwrap().take() {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                std::thread::sleep(std::time::Duration::from_millis(500));
            }
        });
    }

    let mut mosh = Command::new("mosh");
    mosh.args(args);
    if !args.iter().any(|a| a == "--") {
        mosh.arg("--");
    }
    let status = mosh.args(["sh", "-c", &remote_command(Some(&out))]).status();

    done.store(true, Ordering::Relaxed);
    if let Some(mut child) = tail.lock().unwrap().take() {
        let _ = child.kill();
    }
    let _ = write!(tty, "\x1b]6973;Y\x07");
    let _ = crate::exec::run(
        Cmd::Shell(&format!("rm -f {out}")),
        Path::new(""),
        &[],
        std::time::Duration::from_secs(3),
        Some(&host),
    );
    if let Some(master) = master.as_mut() {
        let _ = master.kill();
        let _ = master.wait();
    }
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
        for s in [&b""[..], b"f", b"fo", b"foo", b"\x83\xff\x00 zsh"] {
            assert_eq!(base64_decode(&base64(s)), s);
        }
    }

    #[test]
    fn finds_moshs_destination_and_ssh() {
        assert_eq!(mosh_destination(&args("pi")), Some(0));
        assert_eq!(mosh_destination(&args("-p 60001 --ssh=ssh admin@pi")), Some(3));
        assert_eq!(mosh_destination(&args("--predict always -- pi")), Some(3));
        assert_eq!(mosh_destination(&args("pi htop")), None);
        assert_eq!(mosh_destination(&args("pi -- htop")), None);
        let a: Vec<String> = ["--ssh=ssh -p 2222 -i key", "pi"].map(str::to_string).to_vec();
        assert_eq!(mosh_ssh(&a), ["ssh", "-p", "2222", "-i", "key"]);
        assert_eq!(mosh_ssh(&args("--ssh ssh pi")), ["ssh"]);
        assert_eq!(mosh_ssh(&args("pi")), ["ssh"]);
    }

    #[test]
    fn reads_whole_messages_from_the_hosts_file() {
        let mut buf = b"\x1b]6973;C;/home\x07\x1b]6973;B;2;ls\x07\x1b]6973;V;A=".to_vec();
        assert_eq!(take_messages(&mut buf), [b"\x1b]6973;C;/home\x07".to_vec(), b"\x1b]6973;B;2;ls\x07".to_vec()]);
        assert_eq!(buf, b"\x1b]6973;V;A=");
        buf.extend_from_slice(b"1\x07");
        assert_eq!(take_messages(&mut buf), [b"\x1b]6973;V;A=1\x07".to_vec()]);
        assert!(buf.is_empty());
        // A start split between two reads waits for the rest.
        let mut buf = b"\x1b]69".to_vec();
        assert!(take_messages(&mut buf).is_empty());
        assert_eq!(buf, b"\x1b]69");
    }

    #[test]
    fn finds_the_destination() {
        assert_eq!(destination(&args("pi")), Some(0));
        assert_eq!(destination(&args("-p 2222 -A admin@pi")), Some(3));
        assert_eq!(destination(&args("-p2222 -i key pi")), Some(3));
        assert_eq!(destination(&args("-v -- pi")), Some(2));
        assert_eq!(destination(&args("-P work pi")), Some(2));
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
        assert_eq!(a[a.len() - 1], login_command(script));
        let argv: Vec<String> = ["printf", "%s|", "", "x"].map(str::to_string).to_vec();
        let a = r.argv(Cmd::Argv(&argv), Path::new(""), &[]);
        assert_eq!(a[a.len() - 1], login_command("export LIG_GENERATOR=1; exec 'printf' '%s|' '' 'x'"));
        assert_eq!(ls_command("~/Doc"), "ls -1Ap -- ~/");
        assert_eq!(ls_command("/etc/ss"), "ls -1Ap -- '/etc/'");
        assert_eq!(ls_command("x"), "ls -1Ap -- ");
        assert_eq!(ls_command("it's/a"), "ls -1Ap -- 'it'\\''s/'");
    }

    // Windows' `bash` may be WSL's, which runs the script in another system.
    #[cfg(unix)]
    #[test]
    fn commands_survive_any_login_shell() {
        let nu = std::env::var("LIG_TEST_NU").unwrap_or_else(|_| "nu".into());
        // A command that reads its input gets nothing, not the rest of the script.
        let script = format!("printf '%s|' {} {}; cat\nread x; echo \"got$x\"", quote(r"it's"), quote(r"a\\b\'c"));
        for (sh, no_rc) in
            [("sh", None), ("bash", Some("--norc")), ("zsh", Some("-f")), ("fish", Some("-N")), (&*nu, Some("-n"))]
        {
            let mut cmd = std::process::Command::new(sh);
            cmd.args(no_rc).arg("-c").arg(login_command(&script));
            let Ok(out) = cmd.output() else {
                continue;
            };
            assert_eq!(String::from_utf8_lossy(&out.stdout), "it's|a\\\\b\\'c|got\n", "{sh}");
        }
    }
}
