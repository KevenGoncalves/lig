use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Shell,
    Deny,
    App,
    #[cfg(target_os = "linux")]
    TooLong,
}

const SHELLS: &[&[u8]] = &[
    b"sh", b"bash", b"zsh", b"fish", b"nu", b"dash", b"env", b"nice", b"ionice", b"nohup", b"timeout", b"time",
    b"stdbuf", b"xargs", b"watch",
];

const DENY: &[&[u8]] = &[
    b"sudo", b"doas", b"su", b"pkexec", b"rm", b"rmdir", b"mv", b"dd", b"shred", b"wipefs", b"fdisk", b"parted",
    b"chmod", b"chown", b"kill", b"pkill", b"killall", b"git",
];

pub fn fits(args: &[OsString], len: usize) -> bool {
    len > 0 && args.iter().map(|a| a.len() + 1).sum::<usize>() <= len
}

/// NUL-join `args` and zero-fill to `len` (the last byte is always NUL); `None` if it does not fit.
pub fn layout(args: &[OsString], len: usize) -> Option<Vec<u8>> {
    if !fits(args, len) {
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

/// What the mirror last wrote. The default matches nothing real, so the first tick always writes.
#[derive(Default, PartialEq)]
pub struct State {
    pub argv: Vec<u8>,
    pub cwd: PathBuf,
}

/// Write `bytes` over `area` and zero the old tail; the last byte is never touched.
pub fn write_area(area: &mut [u8], bytes: &[u8]) {
    let Some(last) = area.len().checked_sub(1) else {
        return;
    };
    let bytes = &bytes[..bytes.len().min(last)];
    let content = bytes.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    area[..content].copy_from_slice(&bytes[..content]);
    area[content..last].fill(0);
}

/// `chdir` before `write_argv`, so a reader never sees the new argv with the old cwd.
/// A failed `chdir` leaves `prev.cwd` alone, so it is retried. `true` if anything changed.
pub fn commit(
    prev: &mut State,
    next: State,
    write_argv: impl FnOnce(&[u8]),
    chdir: impl FnOnce(&Path) -> bool,
) -> bool {
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

/// Send the mirror threads' panics (`lig-mirror…`) to `crate::log`: stderr is the raw-mode pane.
pub fn log_panics() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| {
        let prev = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if std::thread::current().name().is_some_and(|n| n.starts_with("lig-mirror")) {
                crate::log(&format!("mirror: panic: {info}"));
            } else {
                prev(info);
            }
        }));
    });
}

fn basename(argv: &[OsString]) -> &[u8] {
    argv.first().and_then(|a| Path::new(a).file_name()).map_or(&[][..], |n| n.as_bytes())
}

pub fn rules(argv: &[OsString]) -> (Option<Vec<OsString>>, Rule) {
    // A mirrored lig (one nested in this one) ends in empty strings: the zero fill on Linux,
    // the unused argument slots on macOS.
    let argv = &argv[..argv.iter().rposition(|a| !a.is_empty()).map_or(0, |i| i + 1)];
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::ffi::OsStringExt;

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
        for x in "sh bash zsh fish nu dash env nice ionice nohup timeout time stdbuf xargs watch".split(' ') {
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
        assert_eq!(rules(&os(&["--zsh"])).1, Rule::App);
    }

    #[test]
    fn rules_drop_a_mirrored_ligs_empty_tail() {
        assert_eq!(rules(&os(&["agent", "--resume", "", "", ""])), (Some(os(&["agent", "--resume"])), Rule::App));
        assert_eq!(rules(&os(&["agent", "", "x"])).0, Some(os(&["agent", "", "x"])));
        assert_eq!(rules(&os(&["", "", ""])), (None, Rule::Shell));
    }

    #[test]
    fn rules_app_unchanged() {
        let r = rules(&os(&["nvim", "foo"]));
        assert_eq!(r, (Some(os(&["nvim", "foo"])), Rule::App));
    }

    #[test]
    fn rules_resume_flags_pass_through() {
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
    fn write_area_keeps_last_nul() {
        let mut area = [0u8; 32];
        write_area(&mut area, &layout(&os(&["nvim", "--clean", "README.md"]), 32).unwrap());
        assert_eq!(&area[..23], b"nvim\0--clean\0README.md\0");
        assert_eq!(area[31], 0);
        write_area(&mut area, &layout(&os(&["/bin/zsh"]), 32).unwrap());
        assert_eq!(&area[..9], b"/bin/zsh\0");
        assert!(area[9..].iter().all(|&b| b == 0));
        let mut area = [0u8; 8];
        write_area(&mut area, b"abcdefgh");
        assert_eq!(&area, b"abcdefg\0");
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
        let next = State { argv: b"nvim\0".to_vec(), cwd: PathBuf::from("/gone") };
        assert!(!commit(&mut prev, next, |_| panic!("argv unchanged"), |_| false));
    }

    #[test]
    fn fits_counts_every_nul() {
        // macOS restores a command only if it fits, so an off-by-one changes which commands restore.
        assert!(fits(&os(&["ab", "c"]), 5));
        assert!(!fits(&os(&["ab", "c"]), 4));
        assert!(!fits(&os(&[]), 0));
    }
}
