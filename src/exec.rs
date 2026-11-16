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
    let timed_out = loop {
        if exited(&mut child) {
            break false;
        }
        if Instant::now() >= deadline {
            kill(&mut child);
            break true;
        }
        std::thread::sleep(Duration::from_millis(2));
    };
    // A background process it started (`cmd &`) can hold the pipes open after it exits.
    let done = |until: Instant| {
        while !(t_out.is_finished() && t_err.is_finished()) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(2));
        }
        t_out.is_finished() && t_err.is_finished()
    };
    if !done(deadline) {
        kill(&mut child);
        done(Instant::now() + Duration::from_millis(100));
    }
    let status = match child.wait() {
        _ if timed_out => 124,
        Ok(st) => st.code().unwrap_or(1),
        Err(_) => 1,
    };
    let collect = |t: std::thread::JoinHandle<Vec<u8>>| {
        let bytes = if t.is_finished() { t.join().unwrap_or_default() } else { Vec::new() };
        String::from_utf8_lossy(&bytes).into_owned()
    };
    let (stdout, stderr) = (collect(t_out), collect(t_err));
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

/// Whether the child has exited, leaving it unreaped until `wait`: its ID, which is its process
/// group's, stays taken meanwhile, so `kill` can't reach a process that reused it.
#[cfg(unix)]
fn exited(child: &mut std::process::Child) -> bool {
    let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
    let flags = libc::WEXITED | libc::WNOHANG | libc::WNOWAIT;
    let r = unsafe { libc::waitid(libc::P_PID, child.id() as libc::id_t, &mut info, flags) };
    r != 0 || info.si_signo != 0
}

#[cfg(windows)]
fn exited(child: &mut std::process::Child) -> bool {
    child.try_wait().map_or(true, |s| s.is_some())
}

#[cfg(unix)]
fn kill(child: &mut std::process::Child) {
    unsafe { libc::kill(-(child.id() as i32), libc::SIGKILL) };
}

#[cfg(windows)]
fn kill(child: &mut std::process::Child) {
    let _ = child.kill();
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_background_process_holding_the_pipe_does_not_outlast_the_timeout() {
        let start = Instant::now();
        let out = run(Cmd::Shell("echo hi; sleep 5 &"), Path::new(""), &[], Duration::from_millis(300), None);
        assert!(start.elapsed() < Duration::from_secs(2), "{:?}", start.elapsed());
        assert_eq!(out.status, 0);
        assert_eq!(out.stdout, "hi\n");
    }

    #[test]
    fn exit_codes_and_timeouts() {
        assert_eq!(run(Cmd::Shell("exit 3"), Path::new(""), &[], Duration::from_secs(5), None).status, 3);
        let start = Instant::now();
        let out = run(Cmd::Shell("echo a; sleep 5"), Path::new(""), &[], Duration::from_millis(200), None);
        assert!(start.elapsed() < Duration::from_secs(2), "{:?}", start.elapsed());
        assert_eq!((out.status, out.stdout.as_str()), (124, "a\n"));
    }
}
