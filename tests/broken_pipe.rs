//! Printing subcommands exit quietly when their stdout is already closed (`lig doctor | head`).
#![cfg(unix)]

use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

fn into_closed_pipe(args: &[&str]) -> (std::process::ExitStatus, String) {
    let (read, write) = nix::unistd::pipe().unwrap();
    // Closed before lig starts, so its first write fails every time.
    drop(read);
    let home = std::env::temp_dir().join(format!("lig-broken-pipe-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_lig"))
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_CACHE_HOME", home.join(".cache"))
        .stdout(Stdio::from(write))
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&home);
    (out.status, String::from_utf8_lossy(&out.stderr).into_owned())
}

#[test]
fn printing_commands_die_of_sigpipe_without_a_panic() {
    for args in
        [&["doctor"][..], &["specs", "list"], &["specs", "path"], &["init", "zsh"], &["config"], &["complete", "git "]]
    {
        let (status, stderr) = into_closed_pipe(args);
        assert!(!stderr.contains("panicked"), "lig {args:?}: {stderr}");
        assert_eq!(status.signal(), Some(libc::SIGPIPE), "lig {args:?}: {status:?} {stderr}");
    }
}
