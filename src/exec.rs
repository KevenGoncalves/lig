//! Running generator commands with a timeout.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::remote::Remote;

#[derive(Debug, Default, Clone)]
pub struct Output {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

pub enum Cmd<'a> {
    /// Run through `bash -c` (on Windows, Git's bash, if installed).
    Shell(&'a str),
    /// Run directly.
    Argv(&'a [String]),
}

/// Runs locally, or on the host of a `lig ssh` session.
pub fn run(cmd: Cmd<'_>, cwd: &Path, env: &[(String, String)], timeout: Duration, remote: Option<&Remote>) -> Output {
    if let Some(r) = remote {
        let argv = r.argv(cmd, cwd, env);
        return run(Cmd::Argv(&argv), Path::new(""), &[], timeout, None);
    }
    let mut c = match cmd {
        Cmd::Shell(s) => {
            let Some(sh) = posix_shell() else {
                return Output {
                    stderr: "no POSIX shell for this generator".into(),
                    status: 127,
                    ..Default::default()
                };
            };
            let mut c = Command::new(sh);
            c.arg("-c").arg(s);
            c
        }
        Cmd::Argv(argv) => {
            let Some((prog, args)) = argv.split_first() else {
                return Output { status: 127, ..Default::default() };
            };
            let mut c = Command::new(prog);
            c.args(args);
            c
        }
    };
    if cwd.is_dir() {
        c.current_dir(cwd);
    }
    c.envs(env.iter().map(|(k, v)| (k, v)))
        .env("LIG_GENERATOR", "1")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    detach(&mut c);
    let mut child = match c.spawn() {
        Ok(ch) => ch,
        Err(e) => return Output { stderr: e.to_string(), status: 127, ..Default::default() },
    };
    let mut so = child.stdout.take().unwrap();
    let mut se = child.stderr.take().unwrap();
    let t_out = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = so.read_to_end(&mut b);
        b
    });
    let t_err = std::thread::spawn(move || {
        let mut b = Vec::new();
        let _ = se.read_to_end(&mut b);
        b
    });
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.code().unwrap_or(1),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(2)),
            _ => {
                kill(&mut child);
                let _ = child.wait();
                break 124;
            }
        }
    };
    let stdout = String::from_utf8_lossy(&t_out.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&t_err.join().unwrap_or_default()).into_owned();
    Output { stdout, stderr, status }
}

fn posix_shell() -> Option<PathBuf> {
    if cfg!(windows) {
        // Not System32's bash.exe: that one runs in WSL, away from the user's files.
        let git = std::env::var_os("ProgramFiles").map(|p| PathBuf::from(p).join(r"Git\bin\bash.exe"));
        return git.filter(|p| p.exists());
    }
    Some(PathBuf::from(if Path::new("/bin/bash").exists() { "/bin/bash" } else { "/bin/sh" }))
}

/// Its own process group, killed as a whole on timeout; on Windows, no console window.
#[cfg(unix)]
fn detach(c: &mut Command) {
    use std::os::unix::process::CommandExt;
    c.process_group(0);
}

#[cfg(windows)]
fn detach(c: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    c.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(unix)]
fn kill(child: &mut std::process::Child) {
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
}

#[cfg(windows)]
fn kill(child: &mut std::process::Child) {
    let _ = child.kill();
}
