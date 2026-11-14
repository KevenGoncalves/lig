//! The mirror on macOS: lig rewrites its own argv and cwd to look like the foreground app on
//! its inner PTY, as on Linux (see mirror.rs), for `ps -t`, WezTerm and tmux's
//! `#{pane_current_path}`. zellij's resurrection takes its own API (mirror_macos.rs).
//!
//! `ps` reads argv from the process's memory (`KERN_PROCARGS2`), so writing over the strings
//! `_NSGetArgv` points at shows there; `main()` re-execs `lig shell` with a 4096-space pad to
//! make room. The kernel keeps the original argc, so `ps` shows that many strings: a command
//! with more arguments is cut, one with fewer ends in spaces.
//!
//! `std::env::args()` reads this same memory: never call it after [`spawn`].

use std::ffi::OsString;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::Duration;

use crate::mirror_policy::{State, commit, layout, log_panics, rules, write_area};

unsafe extern "C" {
    fn _NSGetArgc() -> *mut libc::c_int;
    fn _NSGetArgv() -> *mut *mut *mut libc::c_char;
}

/// The argv strings, which the kernel lays out back to back: `(start, len)`.
fn argv_area() -> Option<(usize, usize)> {
    // SAFETY: dyld sets both up before `main`; they stay valid for the process's lifetime.
    let (argc, argv) = unsafe { (*_NSGetArgc(), *_NSGetArgv()) };
    let argc = usize::try_from(argc).ok().filter(|&n| n > 0)?;
    // SAFETY: `argv` holds `argc` pointers to NUL-terminated strings.
    let start = unsafe { *argv } as usize;
    let mut end = start;
    for i in 0..argc {
        let arg = unsafe { *argv.add(i) };
        if arg.is_null() || arg as usize != end {
            return None;
        }
        end += unsafe { libc::strlen(arg) } + 1;
    }
    Some((start, end - start))
}

/// The foreground app in its directory, or the shell (in the app's directory, as on Linux).
/// `None` keeps the previous state: the app vanished or its argv was unreadable.
fn snapshot(shell_pid: u32, shell: &OsString, len: usize, prev: &State) -> Option<State> {
    let fg = crate::process::foreground(shell_pid)?;
    let argv = crate::process::argv(fg).filter(|a| !a.is_empty())?;
    let argv = rules(&argv).0.and_then(|cmd| layout(&cmd, len)).or_else(|| layout(&[shell.clone()], len))?;
    let cwd = crate::process::cwd(fg).or_else(|| crate::process::cwd(shell_pid)).unwrap_or_else(|| prev.cwd.clone());
    Some(State { argv, cwd })
}

/// `None` with `$LIG_NO_MIRROR` set, or when the argv strings aren't where they should be.
pub fn spawn(shell_pid: u32, shell: OsString) -> Option<std::thread::JoinHandle<()>> {
    if std::env::var_os("LIG_NO_MIRROR").is_some() {
        return None;
    }
    let Some((start, len)) = argv_area() else {
        crate::log("mirror: argv area unreadable; not mirroring");
        return None;
    };
    let identity = crate::process::identity(shell_pid)?;
    log_panics();
    let thread = std::thread::Builder::new().name("lig-mirror-argv".into()).spawn(move || {
        let mut prev = State::default();
        while crate::process::identity(shell_pid) == Some(identity) {
            let step = catch_unwind(AssertUnwindSafe(|| {
                let Some(next) = snapshot(shell_pid, &shell, len, &prev) else { return };
                commit(
                    &mut prev,
                    next,
                    |bytes| {
                        // SAFETY: lig's own argv strings, writable for the process's lifetime; only
                        // this thread writes them, and `std::env::args()` is never called again.
                        write_area(unsafe { std::slice::from_raw_parts_mut(start as *mut u8, len) }, bytes);
                    },
                    |dir| std::env::set_current_dir(dir).is_ok(),
                );
            }));
            if step.is_err() {
                crate::log("mirror: stopped after a panic");
                return;
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    thread.inspect_err(|e| crate::log(&format!("mirror: cannot start thread: {e}"))).ok()
}
