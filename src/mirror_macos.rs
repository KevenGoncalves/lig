//! Explicit zellij resurrection metadata on macOS (no argv memory mutation).
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Longest NUL-joined command restored, matching the Linux mirror's 4096-byte argv pad.
const MAX_COMMAND: usize = 4096;

fn restore_command(argv: &[OsString], shell: &str) -> Vec<String> {
    let (command, _) = crate::mirror_policy::rules(argv);
    command
        .filter(|args| crate::mirror_policy::fits(args, MAX_COMMAND))
        .and_then(|args| args.into_iter().map(|a| a.into_string().ok()).collect())
        .unwrap_or_else(|| vec![shell.into()])
}

fn invoke(args: &[String]) -> crate::exec::Output {
    crate::exec::run(crate::exec::Cmd::Argv(args), Path::new(""), &[], Duration::from_secs(2), None)
}

/// Mirror state, written by the mirror thread and read back by `lig doctor`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    Starting,
    Active,
    /// This zellij has no `set-resurrect-command`.
    Unsupported,
    /// zellij could not be run at all.
    Missing,
    /// The capability probe kept timing out.
    Timeout,
    /// The last update did not reach zellij; retried next tick.
    Failed,
}
impl Status {
    const ALL: [Status; 6] =
        [Status::Starting, Status::Active, Status::Unsupported, Status::Missing, Status::Timeout, Status::Failed];
    fn as_str(self) -> &'static str {
        match self {
            Status::Starting => "starting",
            Status::Active => "active",
            Status::Unsupported => "unsupported",
            Status::Missing => "missing",
            Status::Timeout => "timeout",
            Status::Failed => "failed",
        }
    }
    fn parse(s: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|status| status.as_str() == s)
    }
}

/// Probe for `set-resurrect-command`: the resulting state, and why when it is not `Active`.
/// A timeout gives `None`, so the caller probes again.
fn supported() -> (Option<Status>, String) {
    let out = invoke(&["zellij".into(), "action".into(), "set-resurrect-command".into(), "--help".into()]);
    let detail = out.stderr.trim();
    match out.status {
        0 => (Some(Status::Active), String::new()),
        124 => (None, "mirror: zellij capability probe timed out".into()),
        127 => (Some(Status::Missing), format!("mirror: cannot run zellij: {detail}")),
        s => (Some(Status::Unsupported), format!("mirror: zellij lacks set-resurrect-command (exit {s}): {detail}")),
    }
}

/// Probes that time out in a row before the mirror gives up for this shell.
const PROBE_ATTEMPTS: u32 = 5;

fn status_path(pid: u32) -> Result<PathBuf, String> {
    status_path_in(&std::env::temp_dir(), pid)
}

fn status_path_in(tmp: &Path, pid: u32) -> Result<PathBuf, String> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt};
    // Private directory: never follow another user's symlink in a shared temp directory.
    let uid = unsafe { libc::geteuid() };
    let dir = tmp.join(format!("lig-mirror-{uid}"));
    match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
        Ok(()) => (),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
        Err(e) => return Err(format!("cannot create {}: {e}", dir.display())),
    }
    let meta = std::fs::symlink_metadata(&dir).map_err(|e| format!("cannot stat {}: {e}", dir.display()))?;
    if !meta.is_dir() || meta.uid() != uid || meta.mode() & 0o077 != 0 {
        return Err(format!("{} is not a private directory owned by uid {uid}", dir.display()));
    }
    Ok(dir.join(format!("{pid}")))
}

struct StatusFile {
    path: PathBuf,
    logged: std::cell::Cell<bool>,
}
impl StatusFile {
    fn write(&self, state: Status) {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        // The directory is private; O_NOFOLLOW also rejects a stale symlink at this PID.
        let result = std::fs::OpenOptions::new()
            .create(true)
            .truncate(true)
            .write(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&self.path)
            .and_then(|mut f| f.write_all(state.as_str().as_bytes()));
        // Log the first failure only: this runs every second.
        if let Err(e) = result {
            if !self.logged.replace(true) {
                crate::log(&format!("mirror: cannot write status {}: {e}", self.path.display()));
            }
        }
    }
}
impl Drop for StatusFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
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
    let path = match status_path(pid) {
        Ok(path) => path,
        Err(e) => return format!("{prefix} status directory unusable: {e}"),
    };
    let state = std::fs::symlink_metadata(&path).ok().and_then(|meta| {
        if !meta.is_file() || meta.modified().ok()?.elapsed().ok()? > Duration::from_secs(6) {
            return None;
        }
        Status::parse(&std::fs::read_to_string(&path).ok()?)
    });
    match state {
        Some(Status::Active) => "[ok  ] zellij mirror: explicit resurrection API".into(),
        Some(Status::Unsupported) => {
            format!("{prefix} this zellij lacks set-resurrect-command; autocomplete still works")
        }
        Some(Status::Missing) => format!("{prefix} cannot run zellij (see LIG_LOG); autocomplete still works"),
        Some(Status::Timeout) => format!("{prefix} zellij did not answer the capability check (see LIG_LOG)"),
        Some(Status::Failed) => format!("{prefix} zellij update failed (see LIG_LOG)"),
        Some(Status::Starting) => format!("{prefix} starting"),
        None => format!("{prefix} not active (see LIG_LOG)"),
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
    let skip = |why: &str| {
        crate::log(&format!("mirror: not started: {why}"));
        None
    };
    let Some(session) = std::env::var("ZELLIJ_SESSION_NAME").ok().filter(|s| !s.is_empty()) else {
        return skip("ZELLIJ_SESSION_NAME is unset or empty");
    };
    let Some(pane) = std::env::var("ZELLIJ_PANE_ID").ok().filter(|s| s.parse::<u32>().is_ok()) else {
        return skip("ZELLIJ_PANE_ID is unset or not a number");
    };
    // A lig nested in this pane (or a terminal started from it) inherits the pane id:
    // only the outermost lig may set the pane's resurrect command.
    // Pane ids are small per-session numbers, so the owner names the session too.
    if std::env::var(PANE_OWNER_ENV).is_ok_and(|owner| owner == pane_owner(&session, &pane)) {
        return skip("an outer lig already mirrors this pane");
    }
    let Ok(shell) = shell.into_string() else { return skip("$SHELL is not UTF-8") };
    let Some(shell_identity) = crate::process::identity(shell_pid) else {
        return skip("cannot read the shell's start time");
    };
    let status = match status_path(std::process::id()) {
        Ok(path) => StatusFile { path, logged: Default::default() },
        Err(e) => return skip(&e),
    };
    status.write(Status::Starting);
    let thread = std::thread::Builder::new().name("lig-mirror".into()).spawn(move || {
        let mut available = None;
        let mut timeouts = 0;
        let mut previous = None;
        // The loop runs every second: log a message only when it differs from the last one.
        let mut last_log = String::new();
        let note = |last_log: &mut String, msg: String| {
            if msg != *last_log {
                crate::log(&msg);
                *last_log = msg;
            }
        };
        while crate::process::identity(shell_pid) == Some(shell_identity) {
            if available.is_none() {
                let (state, why) = supported();
                if !why.is_empty() {
                    note(&mut last_log, why);
                }
                timeouts += 1;
                available = state.or((timeouts >= PROBE_ATTEMPTS).then_some(Status::Timeout));
            }
            match available {
                None => status.write(Status::Starting),
                Some(state) if state != Status::Active => status.write(state),
                Some(_) => {
                    let snapshot = crate::process::foreground(shell_pid).and_then(|pid| {
                        let before = crate::process::identity(pid)?;
                        let argv = crate::process::argv(pid)?;
                        // Never restore an application in another directory: fall back to the shell.
                        let cwd = crate::process::cwd(pid)?;
                        (crate::process::identity(pid) == Some(before)).then(|| (restore_command(&argv, &shell), cwd))
                    });
                    // On a disappearing/unreadable foreground process, replace stale commands with the shell.
                    let next = safe_state(
                        snapshot.unwrap_or_else(|| {
                            let cwd = crate::process::cwd(shell_pid)
                                .or_else(|| std::env::current_dir().ok())
                                .unwrap_or_else(|| PathBuf::from("/"));
                            (vec![shell.clone()], cwd)
                        }),
                        &shell,
                    );
                    if previous.as_ref() == Some(&next) {
                        status.write(Status::Active);
                    } else if let Some(args) = update_args(&session, &pane, &next.1, &next.0) {
                        let out = invoke(&args);
                        if out.status == 0 {
                            previous = Some(next);
                            status.write(Status::Active);
                            // A failure that comes back after this success is worth logging again.
                            last_log.clear();
                        } else {
                            // Retry next tick; do not claim that an unsent state is active.
                            status.write(Status::Failed);
                            note(
                                &mut last_log,
                                format!("mirror: zellij update failed (exit {}): {}", out.status, out.stderr.trim()),
                            );
                        }
                    } else {
                        status.write(Status::Failed);
                        note(&mut last_log, "mirror: working directory is not UTF-8".into());
                    }
                }
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
    thread.map_err(|e| crate::log(&format!("mirror: cannot start thread: {e}"))).ok()
}

/// Set on the shell lig runs, so a nested lig in the same zellij pane leaves the pane to this one.
pub const PANE_OWNER_ENV: &str = "LIG_MIRROR_PANE";

/// The `PANE_OWNER_ENV` value for a zellij session and pane.
pub fn pane_owner(session: &str, pane: &str) -> String {
    format!("{session}:{pane}")
}

#[cfg(test)]
mod tests {
    use super::*;
    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }
    #[test]
    fn non_utf8_cwd_replaces_application_with_shell_at_utf8_ancestor() {
        use std::os::unix::ffi::OsStringExt;
        let cwd = PathBuf::from(OsString::from_vec(b"/tmp/invalid-\xff".to_vec()));
        let old = (vec!["nvim".into(), "file".into()], PathBuf::from("/tmp"));
        let next = safe_state((old.0.clone(), cwd), "/bin/zsh");
        assert_ne!(next, old);
        assert_eq!(next, (vec!["/bin/zsh".into()], PathBuf::from("/tmp")));
    }
    #[test]
    fn status_directory_must_be_private_and_real() {
        // Another user could plant either in a shared temp directory to redirect lig's writes.
        use std::os::unix::fs::PermissionsExt;
        let uid = unsafe { libc::geteuid() };
        let base = std::env::temp_dir().join(format!("lig-status-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let (open, linked, fresh) = (base.join("open"), base.join("linked"), base.join("fresh"));
        for dir in [&open, &linked, &fresh] {
            std::fs::create_dir_all(dir).unwrap();
        }
        let shared = open.join(format!("lig-mirror-{uid}"));
        std::fs::create_dir(&shared).unwrap();
        std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o777)).unwrap();
        let link = linked.join(format!("lig-mirror-{uid}"));
        std::os::unix::fs::symlink(&shared, &link).unwrap();
        // A symlink's own mode is 0755; make it look private so only the is-a-directory check can reject it.
        let c_link = std::ffi::CString::new(link.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::lchmod(c_link.as_ptr(), 0o700) }, 0);
        let results = (status_path_in(&open, 1), status_path_in(&linked, 1), status_path_in(&fresh, 1));
        let planted = fresh.join(format!("lig-mirror-{uid}/2"));
        let target = base.join("target");
        std::fs::write(&target, "untouched").unwrap();
        std::os::unix::fs::symlink(&target, &planted).unwrap();
        StatusFile { path: planted.clone(), logged: Default::default() }.write(Status::Active);
        let after = std::fs::read_to_string(&target).unwrap();
        std::fs::remove_dir_all(&base).unwrap();
        assert!(results.0.is_err(), "group/other-writable directory accepted");
        assert!(results.1.is_err(), "symlinked directory accepted");
        assert_eq!(results.2, Ok(fresh.join(format!("lig-mirror-{uid}/1"))));
        assert_eq!(after, "untouched", "status write followed a planted symlink");
    }
    #[test]
    fn status_round_trips_through_its_file_text() {
        // `lig doctor` reads what the mirror thread wrote; a mismatch reads as "not active".
        for status in Status::ALL {
            assert_eq!(Status::parse(status.as_str()), Some(status));
        }
        assert_eq!(Status::parse("bogus"), None);
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
