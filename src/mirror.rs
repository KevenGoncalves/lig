//! The zellij "mirror": lig rewrites its own argv and cwd to look like the
//! foreground app on its inner PTY, so zellij restores the pane correctly.
//!
//! Why: zellij 0.45 serializes a pane (session resurrection) from
//! `/proc/<pane pid>/cmdline` and `/proc/<pane pid>/cwd`. Inside zellij lig is
//! the pane process, so without this every pane would come back as `lig shell`
//! in lig's startup directory, whatever ran in it.
//!
//! How: a thread named `lig-mirror` ([`spawn`]) looks at the shell's
//! foreground process group once a second, maps it through [`rules`] and writes
//! the result over lig's argv area (fields 48/49 of `/proc/self/stat`), then
//! `chdir`s to the app's cwd. `main()` re-execs `lig shell` once with a hidden
//! 4096-space positional, so the area has room for long command lines.
//!
//! NUL rules:
//! - The kernel returns the argv area up to `arg_end` only while the area's
//!   last byte is NUL; otherwise it treats the area as `setproctitle` text and
//!   reads on into the environment. So the last byte is never written.
//! - zellij reads the cmdline through sysinfo, which splits it on NUL and drops
//!   empty segments, so a NUL-joined, zero-filled layout reads back as exactly
//!   those args.
//!
//! `std::env::args()` reads this same memory: it must not be called after
//! [`spawn`] (clap has already parsed the args by then).
//!
//! Everything works on bytes (`OsString`); only `basename(argv[0])` is
//! compared, and never through a lossy UTF-8 conversion.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::thread::JoinHandle;
use std::time::Duration;

/// The mirror thread's name; its panics go to `crate::log`, not the pane.
const THREAD: &str = "lig-mirror";

/// Which rule produced a result; used for the log line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Shell,
    Deny,
    App,
    TooLong,
}

const SHELLS: &[&[u8]] = &[
    b"sh", b"bash", b"zsh", b"fish", b"dash", b"env", b"nice", b"ionice", b"nohup", b"timeout", b"time", b"stdbuf",
    b"xargs", b"watch",
];

const DENY: &[&[u8]] = &[
    b"sudo", b"doas", b"su", b"pkexec", b"rm", b"rmdir", b"mv", b"dd", b"shred", b"wipefs", b"fdisk", b"parted",
    b"chmod", b"chown", b"kill", b"pkill", b"killall", b"git",
];

/// NUL-join `args`, zero-fill to `len`; `None` if it does not fit. The last
/// byte is always NUL, so the result is never truncated.
pub fn layout(args: &[OsString], len: usize) -> Option<Vec<u8>> {
    let needed: usize = args.iter().map(|a| a.len() + 1).sum();
    if len == 0 || needed > len {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for a in args {
        out.extend_from_slice(a.as_bytes());
        out.push(0);
    }
    out.resize(len, 0);
    Some(out)
}

fn basename(argv: &[OsString]) -> &[u8] {
    argv.first().and_then(|a| Path::new(a).file_name()).map_or(&[][..], |n| n.as_bytes())
}

/// Decide what a restored pane should run: `None` means a plain shell.
pub fn rules(argv: &[OsString]) -> (Option<Vec<OsString>>, Rule) {
    // A login shell's argv[0] is `-zsh`, `-bash`: drop that one dash.
    let base = basename(argv);
    let base = base.strip_prefix(b"-").unwrap_or(base);
    if base.is_empty() || SHELLS.contains(&base) {
        (None, Rule::Shell)
    } else if DENY.contains(&base) || base.starts_with(b"mkfs") {
        (None, Rule::Deny)
    } else {
        (Some(argv.to_vec()), Rule::App)
    }
}

/// What the mirror last wrote: the full argv area layout and the cwd.
/// `State::default()` matches nothing real, so the first tick always writes.
#[derive(Default, PartialEq)]
struct State {
    argv: Vec<u8>,
    cwd: PathBuf,
}

fn rule_name(rule: Rule) -> &'static str {
    match rule {
        Rule::Shell => "shell",
        Rule::Deny => "deny",
        Rule::App => "app",
        Rule::TooLong => "toolong",
    }
}

/// `(arg_start, arg_end)` from the text of `/proc/<pid>/stat` (fields 48/49).
fn argv_area_from(stat_text: &str) -> Option<(usize, usize)> {
    let f = crate::procfs::stat_fields(stat_text)?;
    let start = f.get(45)?.parse().ok()?;
    let end = f.get(46)?.parse().ok()?;
    (end > start).then_some((start, end))
}

/// This process's argv area.
fn argv_area() -> Option<(usize, usize)> {
    argv_area_from(&crate::procfs::stat(std::process::id())?)
}

/// Write `bytes` over `area`: the new content first, then zero the old tail.
/// The last byte stays as it is (NUL, see the module docs).
fn write_area(area: &mut [u8], bytes: &[u8]) {
    let Some(last) = area.len().checked_sub(1) else {
        return;
    };
    let bytes = &bytes[..bytes.len().min(last)];
    let content = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    area[..content].copy_from_slice(&bytes[..content]);
    area[content..last].fill(0);
}

/// Write `bytes` over this process's argv area.
///
/// # Safety
/// `area` must be this process's own argv area as reported by
/// `/proc/self/stat`, and only the mirror thread may call this.
unsafe fn apply(area: (usize, usize), bytes: &[u8]) {
    // SAFETY: the caller guarantees `area` is our argv area: mapped, writable
    // memory of `area.1 - area.0` bytes that lives as long as the process.
    // Nothing else holds a reference to it: std only keeps raw pointers, and
    // `std::env::args()` is never called after `spawn` (see the module docs).
    let buf = unsafe { std::slice::from_raw_parts_mut(area.0 as *mut u8, area.1 - area.0) };
    write_area(buf, bytes);
}

/// The decision for one tick. `None` keeps the previous state: the foreground
/// process vanished or its cmdline was unreadable or empty (routine races).
fn tick(
    prev: &State,
    fg_cmdline: std::io::Result<Vec<u8>>,
    fg_cwd: Option<PathBuf>,
    shell_cwd: Option<PathBuf>,
    shell: &OsStr,
    area_len: usize,
) -> Option<(State, Rule)> {
    let raw = fg_cmdline.ok().filter(|c| !c.is_empty())?;
    let raw = raw.strip_suffix(b"\0").unwrap_or(&raw);
    let argv: Vec<OsString> = raw.split(|&b| b == 0).map(|a| OsString::from_vec(a.to_vec())).collect();
    let (cmd, mut rule) = rules(&argv);
    let shell_argv = || vec![shell.to_os_string()];
    let argv = match layout(&cmd.unwrap_or_else(shell_argv), area_len) {
        Some(a) => a,
        None => {
            rule = Rule::TooLong;
            layout(&shell_argv(), area_len)?
        }
    };
    let cwd = [fg_cwd, shell_cwd]
        .into_iter()
        .flatten()
        .find(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| prev.cwd.clone());
    Some((State { argv, cwd }, rule))
}

/// Run `f`, catching a panic; `false` if it panicked.
fn guarded(f: impl FnOnce()) -> bool {
    catch_unwind(AssertUnwindSafe(f)).is_ok()
}

/// Route panics of the mirror thread to `crate::log`: stderr is the raw-mode
/// pane. Every other thread keeps the previous hook.
fn install_panic_hook() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() == Some(THREAD) {
                crate::log(&format!("mirror: panic: {info}"));
            } else {
                prev(info);
            }
        }));
    });
}

/// The argv for the log line: up to 80 bytes, NULs shown as spaces.
fn display_argv(argv: &[u8]) -> String {
    let end = argv.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    let shown: Vec<u8> = argv[..end.min(80)].iter().map(|&b| if b == 0 { b' ' } else { b }).collect();
    String::from_utf8_lossy(&shown).into_owned()
}

/// The foreground process group of the shell's terminal (stat field 8).
fn foreground(shell_pid: u32) -> Option<u32> {
    let text = crate::procfs::stat(shell_pid)?;
    let tpgid: i64 = crate::procfs::stat_fields(&text)?.get(5)?.parse().ok()?;
    u32::try_from(tpgid).ok().filter(|&p| p > 0)
}

/// One pass of the loop. `false` on an invariant break (the argv area moved
/// or became unreadable).
fn step(shell_pid: u32, shell: &OsStr, area: (usize, usize), prev: &mut State) -> bool {
    if argv_area() != Some(area) {
        return false;
    }
    let Some(fg) = foreground(shell_pid) else {
        return true;
    };
    let cmdline = std::fs::read(format!("/proc/{fg}/cmdline"));
    let fg_cwd = std::fs::read_link(format!("/proc/{fg}/cwd")).ok();
    let shell_cwd = std::fs::read_link(format!("/proc/{shell_pid}/cwd")).ok();
    let Some((next, rule)) = tick(prev, cmdline, fg_cwd, shell_cwd, shell, area.1 - area.0) else {
        return true;
    };
    let (argv, cwd) = (display_argv(&next.argv), next.cwd.clone());
    let changed = commit(
        prev,
        next,
        // SAFETY: `area` is our argv area (checked above) and this is the mirror thread.
        |bytes| unsafe { apply(area, bytes) },
        |dir| std::env::set_current_dir(dir).is_ok(),
    );
    if changed {
        crate::log(&format!("mirror: fg={fg} rule={} argv={argv} cwd={}", rule_name(rule), cwd.display()));
    }
    true
}

/// Move `prev` to `next`: `chdir` first, then `write_argv`, so zellij never
/// sees the new argv with the old cwd. `prev.cwd` only follows a successful
/// `chdir`, so a failed one is retried next tick. `true` if anything changed.
fn commit(prev: &mut State, next: State, write_argv: impl FnOnce(&[u8]), chdir: impl FnOnce(&Path) -> bool) -> bool {
    let mut changed = false;
    if next.cwd != prev.cwd && chdir(&next.cwd) {
        prev.cwd = next.cwd;
        changed = true;
    }
    if next.argv != prev.argv {
        write_argv(&next.argv);
        prev.argv = next.argv;
        changed = true;
    }
    changed
}

/// The mirror loop: on a panic or an invariant break it writes `[$SHELL]`
/// once, logs once and returns.
fn run(shell_pid: u32, shell: &OsStr, area: (usize, usize)) {
    let mut prev = State::default();
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let mut ok = false;
        if guarded(|| ok = step(shell_pid, shell, area, &mut prev)) && ok {
            continue;
        }
        if let Some(bytes) = layout(&[shell.to_os_string()], area.1 - area.0) {
            // SAFETY: `area` came from `argv_area()` in `spawn`; this is the mirror thread.
            unsafe { apply(area, &bytes) };
        }
        crate::log("mirror: stopped (panic or argv area changed); pane shows the shell");
        return;
    }
}

/// Start the mirror thread for the shell `shell_pid`; `shell` is the `$SHELL`
/// bytes a restored idle pane runs. `None` outside zellij, with
/// `$LIG_NO_MIRROR` set, or when the argv area is unreadable.
pub fn spawn(shell_pid: u32, shell: OsString) -> Option<JoinHandle<()>> {
    if std::env::var_os("ZELLIJ").is_none() || std::env::var_os("LIG_NO_MIRROR").is_some() {
        return None;
    }
    let Some(area) = argv_area() else {
        crate::log("mirror: argv area unreadable; not mirroring");
        return None;
    };
    install_panic_hook();
    std::thread::Builder::new()
        .name(THREAD.into())
        .spawn(move || run(shell_pid, &shell, area))
        .inspect_err(|e| crate::log(&format!("mirror: spawn failed: {e}")))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn layout_exact_fit() {
        assert_eq!(layout(&os(&["ab", "c"]), 6).unwrap(), b"ab\0c\0\0");
    }

    #[test]
    fn layout_overflow_is_none() {
        assert!(layout(&os(&["abcdef"]), 6).is_none());
    }

    #[test]
    fn layout_last_byte_nul() {
        for (a, n) in [(os(&["ab", "c"]), 5), (os(&["abcde"]), 6), (os(&[]), 3)] {
            let v = layout(&a, n).unwrap();
            assert_eq!(v.len(), n);
            assert_eq!(v[n - 1], 0);
        }
    }

    #[test]
    fn rules_shells_and_wrappers_are_none() {
        for x in "sh bash zsh fish dash env nice ionice nohup timeout time stdbuf xargs watch".split(' ') {
            for name in [x.to_string(), format!("/usr/bin/{x}")] {
                assert_eq!(rules(&os(&[&name, "a"])), (None, Rule::Shell), "{name}");
            }
        }
    }

    #[test]
    fn rules_denylist_is_none() {
        for x in ["sudo", "./rm", "/usr/bin/git", "mkfs.ext4", "pkill"] {
            assert_eq!(rules(&os(&[x, "a"])), (None, Rule::Deny), "{x}");
        }
        assert_eq!(rules(&os(&["run0", "a"])).1, Rule::App);
    }

    #[test]
    fn rules_login_shell_dash_is_shell() {
        for x in ["-zsh", "-bash", "/bin/-sh"] {
            assert_eq!(rules(&os(&[x])), (None, Rule::Shell), "{x}");
        }
        assert_eq!(rules(&os(&["-sudo", "a"])), (None, Rule::Deny));
        // Only one dash is stripped.
        assert_eq!(rules(&os(&["--zsh"])).1, Rule::App);
    }

    #[test]
    fn rules_app_unchanged() {
        let r = rules(&os(&["nvim", "foo"]));
        assert_eq!(r, (Some(os(&["nvim", "foo"])), Rule::App));
    }

    #[test]
    fn rules_resume_flags_pass_through() {
        // Programs get no special treatment: their argv is mirrored as it is.
        for a in [os(&["agent"]), os(&["agent", "--resume", "abc"]), os(&["agent", "-c", "fix x"])] {
            assert_eq!(rules(&a), (Some(a.clone()), Rule::App));
        }
    }

    #[test]
    fn rules_keeps_non_utf8_bytes() {
        let argv = vec![OsString::from("nvim"), OsString::from_vec(b"caf\xe9.txt".to_vec())];
        let (out, rule) = rules(&argv);
        assert_eq!(rule, Rule::App);
        assert_eq!(out.unwrap()[1].as_bytes(), b"caf\xe9.txt");
    }

    #[test]
    fn argv_area_from_real_stat_layout() {
        // 52 fields: pid, comm, then fields 3..=52; fields 48/49 are the argv area.
        let mut rest: Vec<String> = (3..=52).map(|n| n.to_string()).collect();
        rest[48 - 3] = "1000".into();
        rest[49 - 3] = "1100".into();
        let text = format!("42 (lig) {}\n", rest.join(" "));
        assert_eq!(argv_area_from(&text), Some((1000, 1100)));
    }

    #[test]
    fn argv_area_from_malformed_is_none() {
        assert_eq!(argv_area_from("1 (x) S"), None);
    }

    #[test]
    fn write_area_keeps_last_nul() {
        // 32, not 16: "nvim\0--clean\0README.md\0" alone is 22 bytes.
        let mut area = [0u8; 32];
        write_area(&mut area, &layout(&os(&["nvim", "--clean", "README.md"]), 32).unwrap());
        assert_eq!(&area[..23], b"nvim\0--clean\0README.md\0");
        assert_eq!(area[31], 0);
        write_area(&mut area, &layout(&os(&["/bin/zsh"]), 32).unwrap());
        assert_eq!(&area[..9], b"/bin/zsh\0");
        assert!(area[9..].iter().all(|&b| b == 0));
        // The last byte is never touched, even by a layout that is not NUL there.
        let mut area = [0u8; 8];
        write_area(&mut area, b"abcdefgh");
        assert_eq!(&area, b"abcdefg\0");
    }

    fn cmd(v: &[&str]) -> std::io::Result<Vec<u8>> {
        Ok(v.iter().flat_map(|a| a.bytes().chain([0])).collect())
    }

    fn zsh() -> &'static OsStr {
        OsStr::new("/bin/zsh")
    }

    #[test]
    fn tick_keeps_last_on_vanished_fg() {
        let gone = Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(tick(&State::default(), gone, None, None, zsh(), 64).is_none());
        assert!(tick(&State::default(), Ok(Vec::new()), None, None, zsh(), 64).is_none());
    }

    #[test]
    fn tick_too_long_reports_shell() {
        let long = "x".repeat(100);
        let (s, rule) = tick(&State::default(), cmd(&["nvim", &long]), None, None, zsh(), 64).unwrap();
        assert_eq!(s.argv, layout(&os(&["/bin/zsh"]), 64).unwrap());
        assert_eq!(rule, Rule::TooLong);
    }

    #[test]
    fn idle_writes_inherited_shell_bytes() {
        let (s, rule) = tick(&State::default(), cmd(&["/usr/bin/zsh"]), None, None, zsh(), 64).unwrap();
        assert_eq!(s.argv, layout(&os(&["/bin/zsh"]), 64).unwrap());
        assert_eq!(rule, Rule::Shell);
    }

    #[test]
    fn tick_exec_nvim_mirrors() {
        // `exec nvim foo`: the shell pid itself is the foreground process.
        let (s, rule) = tick(&State::default(), cmd(&["nvim", "foo"]), None, None, zsh(), 64).unwrap();
        assert_eq!(s.argv, layout(&os(&["nvim", "foo"]), 64).unwrap());
        assert_eq!(rule, Rule::App);
    }

    #[test]
    fn tick_first_call_writes() {
        let shell_cwd = Some(PathBuf::from("/home"));
        let (s, _) = tick(&State::default(), cmd(&["zsh"]), None, shell_cwd, zsh(), 64).unwrap();
        assert_eq!(s.argv, layout(&os(&["/bin/zsh"]), 64).unwrap());
        assert_eq!(s.cwd, PathBuf::from("/home"));
        assert!(s != State::default());
    }

    #[test]
    fn tick_cwd_precedence() {
        let prev = State { argv: Vec::new(), cwd: PathBuf::from("/prev") };
        let fg = || Some(PathBuf::from("/fg"));
        let sh = || Some(PathBuf::from("/sh"));
        let pick = |fg, sh| tick(&prev, cmd(&["nvim"]), fg, sh, zsh(), 64).unwrap().0.cwd;
        assert_eq!(pick(fg(), sh()), PathBuf::from("/fg"));
        assert_eq!(pick(Some(PathBuf::new()), sh()), PathBuf::from("/sh"));
        assert_eq!(pick(None, sh()), PathBuf::from("/sh"));
        assert_eq!(pick(None, None), PathBuf::from("/prev"));
    }

    #[test]
    fn commit_chdirs_before_argv() {
        let order = std::cell::RefCell::new(Vec::new());
        let mut prev = State::default();
        let next = State { argv: b"nvim\0".to_vec(), cwd: PathBuf::from("/tmp") };
        let changed = commit(
            &mut prev,
            next,
            |_| order.borrow_mut().push("argv"),
            |_| {
                order.borrow_mut().push("chdir");
                true
            },
        );
        assert!(changed);
        assert_eq!(*order.borrow(), ["chdir", "argv"]);
        assert_eq!(prev.cwd, PathBuf::from("/tmp"));
        assert_eq!(prev.argv, b"nvim\0");
    }

    #[test]
    fn commit_keeps_old_cwd_when_chdir_fails() {
        let mut prev = State { argv: b"zsh\0".to_vec(), cwd: PathBuf::from("/old") };
        let next = State { argv: b"nvim\0".to_vec(), cwd: PathBuf::from("/gone") };
        assert!(commit(&mut prev, next, |_| (), |_| false));
        assert_eq!(prev.cwd, PathBuf::from("/old"), "a failed chdir is retried next tick");
        assert_eq!(prev.argv, b"nvim\0");
        // Same argv, chdir still failing: nothing changed, nothing to log.
        let next = State { argv: b"nvim\0".to_vec(), cwd: PathBuf::from("/gone") };
        assert!(!commit(&mut prev, next, |_| panic!("argv unchanged"), |_| false));
    }

    #[test]
    fn guarded_body_reports_panic() {
        assert!(!guarded(|| panic!("x")));
        assert!(guarded(|| ()));
    }
}
