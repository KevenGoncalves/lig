//! Explicit zellij resurrection metadata on macOS (no argv memory mutation).
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

fn restore_command(argv: &[OsString], shell: &str) -> Vec<String> {
    let (command, _) = crate::mirror_policy::rules(argv);
    command
        .filter(|args| crate::mirror_policy::layout(args, 4096).is_some())
        .and_then(|args| args.into_iter().map(|a| a.into_string().ok()).collect())
        .unwrap_or_else(|| vec![shell.into()])
}

fn invoke(args: &[String]) -> crate::exec::Output {
    crate::exec::run(crate::exec::Cmd::Argv(args), Path::new(""), &[], Duration::from_secs(2), None)
}

fn supported() -> bool {
    invoke(&["zellij".into(), "action".into(), "set-resurrect-command".into(), "--help".into()]).status == 0
}

fn status_path(pid: u32) -> Option<PathBuf> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // Private directory: never follow another user's symlink in a shared temp directory.
    let uid = unsafe { libc::geteuid() };
    let dir = std::env::temp_dir().join(format!("lig-mirror-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(_) => return None,
    }
    let meta = std::fs::symlink_metadata(&dir).ok()?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return None;
    }
    Some(dir.join(format!("{pid}")))
}

struct StatusFile(PathBuf);
impl StatusFile {
    fn write(&self, state: &str) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // The directory is private; O_NOFOLLOW also rejects a stale symlink at this PID.
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.0)
        {
            let _ = f.write_all(state.as_bytes());
        }
    }
}
impl Drop for StatusFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

pub fn doctor_status(pid: Option<u32>, zellij: bool, disabled: bool) -> String {
    let prefix = "[off ] zellij mirror:";
    if !zellij {
        return format!("{prefix} not in zellij");
    }
    if disabled {
        return format!("{prefix} LIG_NO_MIRROR set");
    }
    let Some(pid) = pid else { return format!("{prefix} not in a lig pane") };
    let state = status_path(pid).and_then(|path| {
        let meta = std::fs::symlink_metadata(&path).ok()?;
        if !meta.is_file() || meta.modified().ok()?.elapsed().ok()? > Duration::from_secs(6) {
            return None;
        }
        std::fs::read_to_string(path).ok()
    });
    match state.as_deref() {
        Some("active") => "[ok  ] zellij mirror: explicit resurrection API".into(),
        Some("unsupported") => format!("{prefix} this zellij lacks set-resurrect-command; autocomplete still works"),
        Some("failed") => format!("{prefix} zellij update failed (see LIG_LOG)"),
        Some("starting") => format!("{prefix} starting"),
        _ => format!("{prefix} not active (see LIG_LOG)"),
    }
}

fn safe_state(state: (Vec<String>, PathBuf), shell: &str) -> (Vec<String>, PathBuf) {
    if state.1.to_str().is_some() {
        return state;
    }
    // Zellij's API requires UTF-8. Never run an application in a substituted cwd.
    let cwd = state
        .1
        .ancestors()
        .find(|p| p.is_absolute() && p.to_str().is_some())
        .unwrap_or_else(|| Path::new("/"))
        .to_path_buf();
    (vec![shell.into()], cwd)
}

fn update_args(session: &str, pane: &str, cwd: &Path, argv: &[String]) -> Option<Vec<String>> {
    let mut args = vec![
        "zellij".into(),
        "--session".into(),
        session.into(),
        "action".into(),
        "set-resurrect-command".into(),
        "--pane-id".into(),
        pane.into(),
        "--cwd".into(),
        cwd.to_str()?.into(),
        "--".into(),
    ];
    args.extend_from_slice(argv);
    Some(args)
}

pub fn spawn(shell_pid: u32, shell: OsString) -> Option<std::thread::JoinHandle<()>> {
    if std::env::var_os("ZELLIJ").is_none() || std::env::var_os("LIG_NO_MIRROR").is_some() {
        return None;
    }
    let session = std::env::var("ZELLIJ_SESSION_NAME").ok().filter(|s| !s.is_empty())?;
    let pane = std::env::var("ZELLIJ_PANE_ID").ok().filter(|s| s.parse::<u32>().is_ok())?;
    let shell = shell.into_string().ok()?;
    let shell_identity = crate::process::identity(shell_pid)?;
    let status = StatusFile(status_path(std::process::id())?);
    status.write("starting");
    std::thread::Builder::new()
        .name("lig-mirror".into())
        .spawn(move || {
            let available = supported();
            if !available {
                crate::log("mirror: zellij lacks the explicit resurrection API");
            }
            let mut previous = None;
            while crate::process::identity(shell_pid) == Some(shell_identity) {
                if !available {
                    status.write("unsupported");
                } else {
                    let snapshot = crate::process::foreground(shell_pid).and_then(|pid| {
                        let before = crate::process::identity(pid)?;
                        let argv = crate::process::argv(pid)?;
                        let cwd = crate::process::cwd(pid).or_else(|| crate::process::cwd(shell_pid))?;
                        (crate::process::identity(pid) == Some(before)).then(|| (restore_command(&argv, &shell), cwd))
                    });
                    // On a disappearing/unreadable foreground process, replace stale commands with the shell.
                    let next = Some(safe_state(
                        snapshot.unwrap_or_else(|| {
                            let cwd = crate::process::cwd(shell_pid)
                                .or_else(|| std::env::current_dir().ok())
                                .unwrap_or_else(|| PathBuf::from("/"));
                            (vec![shell.clone()], cwd)
                        }),
                        &shell,
                    ));
                    if let Some(next) = next {
                        if previous.as_ref() == Some(&next) {
                            status.write("active");
                        } else if let Some(args) = update_args(&session, &pane, &next.1, &next.0) {
                            if invoke(&args).status == 0 {
                                previous = Some(next);
                                status.write("active");
                            } else {
                                // Retry next tick; do not claim that an unsent state is active.
                                status.write("failed");
                                crate::log("mirror: zellij update failed or timed out");
                            }
                        } else {
                            status.write("failed");
                        }
                    } else {
                        status.write("failed");
                    }
                }
                std::thread::sleep(Duration::from_secs(1));
            }
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }
    #[test]
    fn non_utf8_cwd_replaces_application_with_shell_at_readable_ancestor() {
        use std::os::unix::ffi::OsStringExt;
        let cwd = PathBuf::from(OsString::from_vec(b"/tmp/invalid-\xff".to_vec()));
        let old = (vec!["nvim".into(), "file".into()], PathBuf::from("/tmp"));
        let next = safe_state((old.0.clone(), cwd), "/bin/zsh");
        assert_ne!(next, old);
        assert_eq!(next, (vec!["/bin/zsh".into()], PathBuf::from("/tmp")));
    }
    #[test]
    fn preserves_arguments_and_excludes_unsafe_commands() {
        let args = ["nvim", "two words", "", "a;echo bad", "single'quote", "日本語"];
        assert_eq!(restore_command(&os(&args), "/bin/zsh"), args);
        for cmd in ["sudo", "rm", "git", "-zsh", "env", "mkfs.ext4"] {
            assert_eq!(restore_command(&os(&[cmd, "arg"]), "/bin/zsh"), ["/bin/zsh"]);
        }
        assert_eq!(restore_command(&os(&["nvim", &"x".repeat(5000)]), "/bin/zsh"), ["/bin/zsh"]);
    }
}
