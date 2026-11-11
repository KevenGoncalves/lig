use std::ffi::OsString;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    Shell,
    Deny,
    App,
    #[cfg(target_os = "linux")]
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

pub fn fits(args: &[OsString], len: usize) -> bool {
    len > 0 && args.iter().map(|a| a.len() + 1).sum::<usize>() <= len
}

/// NUL-join `args` and zero-fill to `len` (the last byte is always NUL); `None` if it does not fit.
#[cfg(any(target_os = "linux", test))]
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

fn basename(argv: &[OsString]) -> &[u8] {
    argv.first().and_then(|a| Path::new(a).file_name()).map_or(&[][..], |n| n.as_bytes())
}

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
        assert_eq!(rules(&os(&["--zsh"])).1, Rule::App);
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
    fn fits_counts_every_nul() {
        // macOS restores a command only if it fits, so an off-by-one changes which commands restore.
        assert!(fits(&os(&["ab", "c"]), 5));
        assert!(!fits(&os(&["ab", "c"]), 4));
        assert!(!fits(&os(&[]), 0));
    }
}
