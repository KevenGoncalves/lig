//! End-to-end test of lig on Windows: runs `lig shell` in a pseudo console, as Windows
//! Terminal does, types into it and checks the popup on screen.
//!
//!   cargo run --release -- <lig.exe> [shell…]     (default: pwsh powershell cmd)
//!
//! On Unix it drives bash the same way, with a scratch HOME, to check the harness itself.

#[allow(dead_code)]
#[path = "../../../src/pty.rs"]
mod pty;

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

const ROWS: u16 = 30;
const COLS: u16 = 120;

struct Term {
    // Field order: the reader's channel outlives the pseudo console, which (before Windows 11
    // 24H2) blocks on close until its output is read.
    w: Box<dyn Write + Send>,
    pty: pty::Pty,
    rx: Receiver<Vec<u8>>,
    parser: vt100::Parser,
    /// Everything lig wrote, for the report when a check fails.
    raw: Vec<u8>,
}

impl Term {
    fn spawn(cmd: &pty::Command) -> anyhow::Result<Term> {
        let (pty, mut reader, w) = pty::Pty::spawn(cmd, ROWS, COLS)?;
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let mut buf = [0u8; 16384];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 || tx.send(buf[..n].to_vec()).is_err() {
                    break;
                }
            }
        });
        Ok(Term { w, pty, rx, parser: vt100::Parser::new(ROWS, COLS, 0), raw: vec![] })
    }

    fn pump(&mut self, until: Instant) -> bool {
        let Some(left) = until.checked_duration_since(Instant::now()) else { return false };
        match self.rx.recv_timeout(left) {
            Ok(b) => {
                self.parser.process(&b);
                self.raw.extend(b);
                while let Ok(b) = self.rx.try_recv() {
                    self.parser.process(&b);
                    self.raw.extend(b);
                }
                true
            }
            Err(_) => false,
        }
    }

    fn wait(&mut self, ms: u64, f: impl Fn(&Term) -> bool) -> bool {
        let until = Instant::now() + Duration::from_millis(ms);
        loop {
            if f(self) {
                return true;
            }
            if !self.pump(until) && Instant::now() >= until {
                return f(self);
            }
        }
    }

    /// Reads until nothing new arrives for `ms`.
    fn settle(&mut self, ms: u64) {
        while self.pump(Instant::now() + Duration::from_millis(ms)) {}
    }

    /// Keys for lig; lost if it already exited (the checks then fail and say so).
    fn send(&mut self, s: &str) {
        let _ = self.w.write_all(s.as_bytes()).and_then(|_| self.w.flush());
    }

    /// Types like a person: one key at a time.
    fn type_text(&mut self, s: &str) {
        for c in s.chars() {
            self.send(&c.to_string());
            self.settle(60);
        }
    }

    fn rows(&self) -> Vec<String> {
        self.parser.screen().rows(0, COLS).collect()
    }

    fn cursor_row(&self) -> usize {
        self.parser.screen().cursor_position().0 as usize
    }

    fn cursor_line(&self) -> String {
        self.rows().get(self.cursor_row()).map(|r| r.trim_end().to_string()).unwrap_or_default()
    }

    /// Text on some row other than the cursor's: the popup.
    fn popup_has(&self, text: &str) -> bool {
        let at = self.cursor_row();
        self.rows().iter().enumerate().any(|(i, r)| i != at && r.contains(text))
    }

    fn at_prompt(&self) -> bool {
        let line = self.cursor_line();
        line.ends_with('>') || line.ends_with('$')
    }

    fn dump(&self) -> String {
        let mut out = String::new();
        for (i, r) in self.rows().iter().enumerate() {
            if !r.trim().is_empty() {
                out += &format!("{i:2}| {}\n", r.trim_end());
            }
        }
        let tail = &self.raw[self.raw.len().saturating_sub(3000)..];
        out + &format!(
            "cursor {:?}, {} bytes from lig, the last:\n{}\n",
            self.parser.screen().cursor_position(),
            self.raw.len(),
            tail.escape_ascii()
        )
    }
}

impl Drop for Term {
    fn drop(&mut self) {
        // Ends the shell and lig; the pseudo console closes with `pty`.
        self.send("exit\r");
        self.settle(300);
        let _ = &self.pty;
    }
}

#[derive(Default)]
struct Report {
    rows: Vec<(String, bool, String)>,
}

impl Report {
    fn check(&mut self, name: String, ok: bool, detail: impl Into<String>) -> bool {
        let detail = detail.into();
        println!("[{}] {name}  {detail}", if ok { "PASS" } else { "FAIL" });
        self.rows.push((name, ok, detail));
        ok
    }
}

/// A scratch folder to start in, with two folders to complete.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lig-e2e-{name}-{}", std::process::id()));
    for sub in ["alpha", "beta"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    dir
}

fn log_tail() -> String {
    let log = std::env::temp_dir().join("lig").join("lig.log");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let lines: Vec<&str> = text.lines().collect();
    lines[lines.len().saturating_sub(40)..].join("\n")
}

fn run_shell(r: &mut Report, lig: &Path, shell: &str) -> anyhow::Result<()> {
    let dir = scratch(shell);
    let mut cmd = pty::Command::new(lig);
    cmd.arg("shell").arg("--shell").arg(shell).env("LIG_LOG", "1");
    cmd.cwd = Some(dir.clone());
    if cfg!(unix) {
        // bash with lig's hooks and nothing of the user's.
        let rc =
            format!("eval \"$('{0}' init bash pre)\"\nPS1='\\W$ '\neval \"$('{0}' init bash post)\"\n", lig.display());
        std::fs::write(dir.join(".bashrc"), rc)?;
        cmd.env("HOME", &dir).env("LIG_TTY", "");
    }
    let mut t = Term::spawn(&cmd)?;
    let started = t.wait(90_000, Term::at_prompt);
    t.settle(1500);
    let mut ok = r.check(format!("{shell}: prompt"), started, t.cursor_line());
    // PowerShell prints errors from the integration (an older PSReadLine, …) with these.
    let error = t.rows().into_iter().find(|r| r.contains("FullyQualifiedErrorId") || r.contains("CategoryInfo"));
    ok &= r.check(format!("{shell}: no errors at startup"), error.is_none(), error.unwrap_or_default().trim());

    t.type_text("git chec");
    let shown = t.wait(15_000, |t| t.popup_has("checkout"));
    ok &= r.check(format!("{shell}: popup for `git chec`"), shown, t.cursor_line());
    t.settle(300);
    t.send("\r");
    let accepted = t.wait(5_000, |t| t.cursor_line().ends_with("git checkout"));
    ok &= r.check(format!("{shell}: Enter accepts `checkout`"), accepted, t.cursor_line());
    let closed = t.wait(3_000, |t| !t.popup_has("checkout"));
    ok &= r.check(format!("{shell}: popup closes after accepting"), closed, "");
    // Clear the line (Esc in PowerShell and cmd, Ctrl+U in bash).
    t.send(if cfg!(windows) { "\x1b" } else { "\x15" });
    t.wait(3_000, |t| t.at_prompt());
    t.settle(500);

    t.type_text("cd al");
    let folder = t.wait(15_000, |t| t.popup_has("alpha"));
    ok &= r.check(format!("{shell}: popup for folders"), folder, t.cursor_line());
    t.settle(300);
    t.send("\r");
    let picked = t.wait(5_000, |t| t.cursor_line().trim_end_matches(['\\', '/']).ends_with("cd alpha"));
    ok &= r.check(format!("{shell}: Enter picks the folder"), picked, t.cursor_line());
    t.settle(300);
    // The folder's popup offers ↵ Enter first: it runs the line.
    t.send("\r");
    let moved = t.wait(15_000, |t| t.at_prompt() && t.cursor_line().contains("alpha"));
    ok &= r.check(format!("{shell}: the line ran in the new folder"), moved, t.cursor_line());

    // Shell-specific suggestions: cmd's `/` switches, PowerShell's cmdlets and `$env:`.
    let checks: &[(&str, &str)] = match shell {
        "cmd" => &[("dir /", "/s"), ("ipconfig /", "/all")],
        "pwsh" | "powershell" => &[("Get-ChildItem -Rec", "-Recurse"), ("echo $env:PAT", "env:PATH")],
        _ => &[],
    };
    for &(line, want) in checks {
        t.type_text(line);
        let shown = t.wait(15_000, |t| t.popup_has(want));
        ok &= r.check(format!("{shell}: `{line}` offers `{want}`"), shown, t.cursor_line());
        // Close the popup, then clear the line.
        t.send("\x1b");
        t.settle(300);
        t.send("\x1b");
        t.wait(3_000, |t| t.at_prompt());
        t.settle(500);
    }

    if !ok {
        println!("--- {shell} screen\n{}--- lig.log\n{}", t.dump(), log_tail());
    }
    drop(t);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let lig = PathBuf::from(args.next().expect("usage: lig-windows-e2e <lig> [shell…]"));
    let lig = std::fs::canonicalize(&lig).unwrap_or(lig);
    let mut shells: Vec<String> = args.collect();
    if shells.is_empty() {
        shells = if cfg!(windows) { ["pwsh", "powershell", "cmd"] } else { ["bash", "", ""] }
            .iter()
            .filter(|s| !s.is_empty())
            .map(|s| s.to_string())
            .collect();
    }
    let mut r = Report::default();
    for shell in &shells {
        if let Err(e) = run_shell(&mut r, &lig, shell) {
            r.check(format!("{shell}: started"), false, format!("{e:#}"));
        }
    }
    let mut summary = String::from("| check | result | detail |\n| --- | --- | --- |\n");
    for (name, ok, detail) in &r.rows {
        summary +=
            &format!("| {name} | {} | `{}` |\n", if *ok { "pass" } else { "**FAIL**" }, detail.replace('|', "\\|"));
    }
    if let Ok(p) = std::env::var("GITHUB_STEP_SUMMARY") {
        let os = std::env::var("ImageOS").unwrap_or_default();
        let mut f = std::fs::OpenOptions::new().append(true).create(true).open(p)?;
        writeln!(f, "## lig end to end ({os})\n\n{summary}")?;
    }
    let failed = r.rows.iter().filter(|(_, ok, _)| !ok).count();
    println!("\n{} checks, {failed} failed", r.rows.len());
    std::process::exit(i32::from(failed > 0));
}
