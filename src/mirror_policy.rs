//! Shared resurrection rules and bounded argv layout.
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

/// NUL-join `args` and zero-fill to `len` (the last byte is always NUL); `None` if it does not fit.
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
