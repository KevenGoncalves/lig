//! The commands that ran (only those that exited 0 with `behavior.history_success_only`),
//! shared by every lig session, newest first and each once; Ctrl+R shows them
//! (`behavior.history`). The first time, the shells' own histories seed it (their exit
//! codes are unknown, so those keep their failures).
//!
//! The file has one `<unix time>[@<folder>]\t<command>` line per run, appended; a command's
//! `\` and newlines are written `\\` and `\n`. The folder is where it ran, for the folder's
//! own history (`keys.folder_history`); older lig reads such a line's time as 0.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// More lines than this many times the unique commands: rewrite the file without repeats.
const COMPACT: usize = 2;

pub struct History {
    path: PathBuf,
    /// (time, command, folder), newest first, each command once per folder.
    entries: Vec<Run>,
    /// The file's length when read: another session appended if it grew.
    read_len: u64,
    /// Read yet: not before the first command or search, so a new tab starts at once.
    loaded: bool,
    /// Whose shell histories seed it.
    home: Option<PathBuf>,
}

/// A command, when it ran, and where (if known).
type Run = (u64, String, Option<String>);

/// Private, like the shells' own history files.
fn private_file(path: &Path, append: bool) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.create(true).write(true).append(append).truncate(!append);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

impl History {
    /// `~/.local/state/lig/history`, read when first needed.
    pub fn open() -> Self {
        History {
            path: crate::state_dir().join("history"),
            entries: vec![],
            read_len: 0,
            loaded: false,
            home: dirs::home_dir(),
        }
    }

    #[cfg(test)]
    fn at(path: PathBuf) -> Self {
        History { path, entries: vec![], read_len: 0, loaded: false, home: None }
    }

    /// The first time, seed the file from the shells' histories.
    fn seed(&self) {
        if self.path.exists() {
            return;
        }
        let mut seed = self.home.as_deref().map(import).unwrap_or_default();
        seed.sort_by_key(|(time, _)| *time);
        let text: String = seed.iter().map(|(time, command)| line(*time, command, None)).collect();
        if let Some(dir) = self.path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        use std::io::Write;
        let _ = private_file(&self.path, false).and_then(|mut f| f.write_all(text.as_bytes()));
    }

    /// Read the file again if another session added to it.
    fn reload(&mut self) {
        if !self.loaded {
            self.seed();
            self.loaded = true;
        }
        let len = std::fs::metadata(&self.path).map_or(0, |m| m.len());
        if len == self.read_len && self.read_len > 0 {
            return;
        }
        let text = String::from_utf8_lossy(&std::fs::read(&self.path).unwrap_or_default()).into_owned();
        let runs: Vec<Run> = text.lines().filter_map(parse_line).collect();
        self.entries = newest_unique_runs(runs.iter().cloned());
        self.read_len = len;
        if runs.len() > COMPACT * self.entries.len() + 1000 {
            let text: String =
                self.entries.iter().rev().map(|(time, command, dir)| line(*time, command, dir.as_deref())).collect();
            let tmp = self.path.with_extension("tmp");
            use std::io::Write;
            let written = private_file(&tmp, false).and_then(|mut f| f.write_all(text.as_bytes()));
            if written.is_ok() && std::fs::rename(&tmp, &self.path).is_ok() {
                self.read_len = text.len() as u64;
            }
        }
    }

    /// A command that ran, in `dir` if known.
    pub fn record(&mut self, command: &str, dir: Option<&str>) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        let time = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        // Seeded before the first line, which would otherwise make the file look old.
        self.seed();
        let before = std::fs::metadata(&self.path).map_or(0, |m| m.len());
        let text = line(time, command, dir);
        use std::io::Write;
        if let Err(e) = private_file(&self.path, true).and_then(|mut f| f.write_all(text.as_bytes())) {
            crate::log(&format!("history: {e}"));
            return;
        }
        if !self.loaded {
            return;
        }
        // Only our line was added: no need to read the file again.
        if before == self.read_len {
            self.read_len += text.len() as u64;
        }
        let dir = dir.filter(|d| storable(d)).map(str::to_string);
        self.entries.retain(|(_, c, d)| !(c == command && *d == dir));
        self.entries.insert(0, (time, command.to_string(), dir));
    }

    /// The commands matching `query`, newest first (see [`matching`]); with `dir`, only those
    /// that ran there.
    pub fn search(&mut self, query: &str, fuzzy: bool, dir: Option<&str>) -> Vec<(u64, String)> {
        self.reload();
        matching(&in_dir(&self.entries, dir), query, fuzzy)
    }
}

/// What a `lig ssh` host remembers: the commands run on it in this session, then its zsh and
/// bash history files. In memory only: the host's history is its shells' to keep.
#[derive(Default)]
pub struct HostHistory {
    /// Run in this session, newest first, with the host's folder.
    session: Vec<Run>,
    /// From the host's files, newest first.
    files: Vec<(u64, String)>,
    /// The files were asked for.
    pub asked: bool,
}

/// Prints a host's `.zsh_history` and `.bash_history`, one base64 line each (empty if missing).
pub const HOST_FILES: &str =
    r#"for f in "${ZDOTDIR:-$HOME}/.zsh_history" "$HOME/.bash_history"; do base64 <"$f" | tr -d '\n'; echo; done"#;

impl HostHistory {
    pub fn record(&mut self, command: &str, dir: Option<&str>) {
        let command = command.trim();
        if command.is_empty() {
            return;
        }
        let time = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let dir = dir.map(str::to_string);
        self.session.retain(|(_, c, d)| !(c == command && *d == dir));
        self.session.insert(0, (time, command.to_string(), dir));
    }

    /// The host's files, as [`HOST_FILES`] sent them.
    pub fn read_files(&mut self, zsh_history: &[u8], bash_history: &[u8]) {
        let mut runs = zsh(zsh_history);
        runs.extend(bash(&String::from_utf8_lossy(bash_history)));
        runs.sort_by_key(|(time, _)| *time);
        self.files = newest_unique(runs.into_iter());
    }

    /// With `dir`, only what ran there in this session: the host's files don't say where.
    pub fn search(&self, query: &str, fuzzy: bool, dir: Option<&str>) -> Vec<(u64, String)> {
        let mut all = in_dir(&self.session, dir);
        if dir.is_none() {
            all.extend(
                self.files.iter().filter(|(_, c)| !all.iter().any(|(_, s)| s == c)).cloned().collect::<Vec<_>>(),
            );
        }
        matching(&all, query, fuzzy)
    }
}

/// The commands in `entries` (newest first) matching `query`, newest first: those starting
/// with it, then those containing it, then (with `fuzzy`) those with its letters in order.
fn matching(entries: &[(u64, String)], query: &str, fuzzy: bool) -> Vec<(u64, String)> {
    let query = query.trim_start().to_lowercase();
    let mut tiers: [Vec<(u64, String)>; 3] = Default::default();
    for (time, command) in entries {
        let lower = command.to_lowercase();
        let tier = if lower.starts_with(&query) {
            0
        } else if lower.contains(&query) {
            1
        } else if fuzzy && in_order(&query, &lower) {
            2
        } else {
            continue;
        };
        tiers[tier].push((*time, command.clone()));
    }
    tiers.into_iter().flatten().collect()
}

fn in_order(query: &str, text: &str) -> bool {
    let mut chars = text.chars();
    query.chars().filter(|c| !c.is_whitespace()).all(|q| chars.any(|c| c == q))
}

/// The runs in `dir`, or each command once (its newest run) when `dir` is `None`.
fn in_dir(runs: &[Run], dir: Option<&str>) -> Vec<(u64, String)> {
    let mut seen = std::collections::HashSet::new();
    runs.iter()
        .filter(|(_, c, d)| match dir {
            Some(dir) => d.as_deref() == Some(dir),
            None => seen.insert(c.clone()),
        })
        .map(|(t, c, _)| (*t, c.clone()))
        .collect()
}

/// Each command once per folder, at its latest run there, newest first.
fn newest_unique_runs(runs: impl DoubleEndedIterator<Item = Run>) -> Vec<Run> {
    let mut seen = std::collections::HashSet::new();
    runs.rev().filter(|(_, command, dir)| seen.insert((command.clone(), dir.clone()))).collect()
}

/// Each command once, at its latest run, newest first.
fn newest_unique(runs: impl DoubleEndedIterator<Item = (u64, String)>) -> Vec<(u64, String)> {
    let mut seen = std::collections::HashSet::new();
    runs.rev().filter(|(_, command)| seen.insert(command.clone())).collect()
}

/// A folder that fits in a line's first field (no tab, no newline).
fn storable(dir: &str) -> bool {
    !dir.is_empty() && !dir.contains(['\t', '\n'])
}

fn line(time: u64, command: &str, dir: Option<&str>) -> String {
    let at = dir.filter(|d| storable(d)).map(|d| format!("@{d}")).unwrap_or_default();
    format!("{time}{at}\t{}\n", command.replace('\\', "\\\\").replace('\n', "\\n"))
}

fn parse_line(line: &str) -> Option<Run> {
    let (when, command) = line.split_once('\t')?;
    let (time, dir) = when.split_once('@').map_or((when, None), |(t, d)| (t, Some(d.to_string())));
    let out = unescape(command);
    // lig's own hooks, which nu used to record as lines that ran (see shell/post.nu).
    (!out.trim().is_empty() && !matches!(out.as_str(), "__lig_insert" | "_lig_insert"))
        .then(|| (time.parse().unwrap_or(0), out, dir))
}

/// `\n` is a newline and `\x` is `x`, read left to right (lig's file and fish's history).
fn unescape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some(other) => out.push(other),
            None => out.push('\\'),
        }
    }
    out
}

/// What zsh, bash, fish and nu remember, oldest first within each.
fn import(home: &Path) -> Vec<(u64, String)> {
    let read = |p: &Path| std::fs::read(p).ok();
    let mut out = vec![];
    let zdotdir = std::env::var_os("ZDOTDIR").map(PathBuf::from).unwrap_or_else(|| home.to_path_buf());
    let path = zdotdir.join(".zsh_history");
    if let Some(bytes) = read(&path) {
        out.extend(dated(zsh(&bytes), &path));
    }
    let path = home.join(".bash_history");
    if let Some(bytes) = read(&path) {
        out.extend(dated(bash(&String::from_utf8_lossy(&bytes)), &path));
    }
    // fish keeps XDG's folders on macOS too.
    let data = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|d| d.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    if let Some(bytes) = read(&data.join("fish/fish_history")) {
        out.extend(fish(&String::from_utf8_lossy(&bytes)));
    }
    let config = dirs::config_dir().unwrap_or_else(|| home.join(".config"));
    let path = config.join("nushell/history.txt");
    if let Some(bytes) = read(&path) {
        let text = String::from_utf8_lossy(&bytes);
        let runs = text.lines().filter(|l| !l.trim().is_empty()).map(|l| (0, l.to_string())).collect();
        out.extend(dated(runs, &path));
    }
    out
}

/// Undated commands (bash without `HISTTIMEFORMAT`, nu's history.txt) end when their file last
/// changed, a second apart in order: they sort among the dated ones by how recent they are,
/// not all below them.
fn dated(mut runs: Vec<(u64, String)>, path: &Path) -> Vec<(u64, String)> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    let Some(last) = modified.and_then(|t| t.duration_since(UNIX_EPOCH).ok()).map(|d| d.as_secs()) else {
        return runs;
    };
    let n = runs.len() as u64;
    for (i, run) in runs.iter_mut().enumerate() {
        if run.0 == 0 {
            run.0 = last.saturating_sub(n - 1 - i as u64);
        }
    }
    runs
}

/// `: <time>:<duration>;<command>` (extended history) or plain lines; a trailing `\` continues
/// the command on the next line. zsh writes some bytes "metafied": 0x83, then the byte ^ 32.
fn zsh(bytes: &[u8]) -> Vec<(u64, String)> {
    let mut plain = Vec::with_capacity(bytes.len());
    let mut it = bytes.iter();
    while let Some(&b) = it.next() {
        plain.push(if b == 0x83 { it.next().map_or(b, |n| n ^ 32) } else { b });
    }
    let text = String::from_utf8_lossy(&plain);
    let mut out: Vec<(u64, String)> = vec![];
    let mut continued = false;
    for raw in text.lines() {
        if continued && let Some((_, command)) = out.last_mut() {
            command.push('\n');
            command.push_str(raw.strip_suffix('\\').unwrap_or(raw));
        } else {
            let (time, command) = match raw.strip_prefix(": ").and_then(|r| r.split_once(';')) {
                Some((meta, command)) => (meta.split(':').next().and_then(|t| t.parse().ok()).unwrap_or(0), command),
                None => (0, raw),
            };
            out.push((time, command.strip_suffix('\\').unwrap_or(command).to_string()));
        }
        continued = raw.ends_with('\\');
    }
    out.retain(|(_, c)| !c.trim().is_empty());
    out
}

/// Plain lines; `#<time>` lines (HISTTIMEFORMAT) date the next one.
fn bash(text: &str) -> Vec<(u64, String)> {
    let mut time = 0;
    let mut out = vec![];
    for l in text.lines() {
        match l.strip_prefix('#').and_then(|t| t.parse().ok()) {
            Some(t) => time = t,
            None if !l.trim().is_empty() => out.push((time, l.to_string())),
            None => {}
        }
    }
    out
}

/// `- cmd: <command>` then `  when: <time>`; the command's `\\` and `\n` are escaped.
fn fish(text: &str) -> Vec<(u64, String)> {
    let mut out: Vec<(u64, String)> = vec![];
    for l in text.lines() {
        if let Some(command) = l.strip_prefix("- cmd: ") {
            out.push((0, unescape(command)));
        } else if let (Some(time), Some(last)) = (l.trim_start().strip_prefix("when: "), out.last_mut()) {
            last.0 = time.trim().parse().unwrap_or(0);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hosts_history_is_its_session_then_its_files() {
        let mut h = HostHistory::default();
        assert!(h.search("", false, None).is_empty());
        h.read_files(b": 100:0;make deploy\n: 200:0;git pull\n", b"ls\nmake deploy\n");
        h.record("git pull", None);
        h.record("echo new", None);
        let found: Vec<String> = h.search("", false, None).into_iter().map(|(_, c)| c).collect();
        assert_eq!(found, ["echo new", "git pull", "make deploy", "ls"]);
        let found: Vec<String> = h.search("ma", false, None).into_iter().map(|(_, c)| c).collect();
        assert_eq!(found, ["make deploy"]);
    }

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("lig-history-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("history")
    }

    fn commands(found: Vec<(u64, String)>) -> Vec<String> {
        found.into_iter().map(|(_, c)| c).collect()
    }

    #[test]
    fn newest_first_each_once_shared_between_sessions() {
        let path = temp("shared");
        std::fs::write(&path, "1\tgit status\n2\tls\n3\tgit status\n").unwrap();
        let mut one = History::at(path.clone());
        let mut two = History::at(path.clone());
        assert_eq!(commands(one.search("", false, None)), ["git status", "ls"]);
        two.record("cargo test", None);
        one.record("multi\\line\ncommand", None);
        assert_eq!(commands(one.search("", false, None)), ["multi\\line\ncommand", "cargo test", "git status", "ls"]);
        assert_eq!(commands(two.search("", false, None))[0], "multi\\line\ncommand");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn search_ranks_prefix_then_substring_then_fuzzy() {
        let path = temp("search");
        std::fs::write(&path, "1\tgit commit -m wip\n2\tcargo build\n3\tgit checkout main\n4\techo git\n").unwrap();
        let mut h = History::at(path.clone());
        assert_eq!(commands(h.search("git", false, None)), ["git checkout main", "git commit -m wip", "echo git"]);
        assert_eq!(commands(h.search("gcm", false, None)), [] as [&str; 0]);
        assert_eq!(commands(h.search("gcm", true, None)), ["git checkout main", "git commit -m wip"]);
        assert_eq!(commands(h.search("CARGO", false, None)), ["cargo build"]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_file_is_private_and_read_only_when_needed() {
        let path = temp("private");
        let mut h = History::at(path.clone());
        assert!(!path.exists(), "nothing read or written until needed");
        h.record("echo one", None);
        assert_eq!(commands(h.search("", false, None))[0], "echo one");
        h.record("echo two", None);
        assert_eq!(commands(h.search("", false, None))[..2], ["echo two", "echo one"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn many_repeats_are_compacted() {
        let path = temp("compact");
        let text: String = (0..3000).map(|i| line(i, if i % 2 == 0 { "a" } else { "b" }, None)).collect();
        std::fs::write(&path, text).unwrap();
        let mut h = History::at(path.clone());
        assert_eq!(commands(h.search("", false, None)), ["b", "a"]);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "2998\ta\n2999\tb\n");
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn imports_each_shells_history() {
        // `→` is E2 86 92; zsh writes its last two bytes metafied.
        let zsh_text = b": 1700000000:0;git status\n: 1700000005:0;echo one\\\ntwo\nplain \xe2\x83\xa6\x83\xb2\n";
        assert_eq!(
            zsh(zsh_text),
            [(1700000000, "git status".into()), (1700000005, "echo one\ntwo".into()), (0, "plain \u{2192}".into())]
        );
        assert_eq!(bash("#100\nls -la\n\ncd /tmp\n"), [(100, "ls -la".into()), (100, "cd /tmp".into())]);
        assert_eq!(
            fish("- cmd: git push\n  when: 1700000100\n- cmd: echo a\\nb\n  when: 7\n  paths:\n    - x\n"),
            [(1700000100, "git push".into()), (7, "echo a\nb".into())]
        );
        // fish writes `echo \n` (a backslash and an n) as `echo \\n`.
        assert_eq!(fish("- cmd: echo \\\\n\n"), [(0, "echo \\n".into())]);
    }

    #[test]
    fn each_folder_has_its_own_history() {
        let path = temp("folders");
        let mut h = History::at(path.clone());
        h.record("make", Some("/src/a"));
        h.record("ls", Some("/src/b"));
        h.record("make", Some("/src/b"));
        h.record("cargo test", None);
        assert_eq!(commands(h.search("", false, None)), ["cargo test", "make", "ls"]);
        assert_eq!(commands(h.search("", false, Some("/src/a"))), ["make"]);
        assert_eq!(commands(h.search("", false, Some("/src/b"))), ["make", "ls"]);
        // Read back by another session, folders included; a tab can't go in the line's first field.
        let mut other = History::at(path);
        assert_eq!(commands(other.search("", false, Some("/src/a"))), ["make"]);
        assert_eq!(line(7, "x", Some("/a\tb")), "7\tx\n");
        assert_eq!(parse_line("7@/home/me@work\tls -la"), Some((7, "ls -la".into(), Some("/home/me@work".into()))));
        let mut host = HostHistory::default();
        host.record("git pull", Some("/srv"));
        host.read_files(b"", b"uptime\n");
        assert_eq!(commands(host.search("", false, Some("/srv"))), ["git pull"]);
        assert_eq!(commands(host.search("", false, None)), ["git pull", "uptime"]);
    }

    #[test]
    fn ligs_own_hooks_are_not_listed() {
        assert_eq!(parse_line("5\t__lig_insert"), None);
        assert_eq!(parse_line("5\techo __lig_insert"), Some((5, "echo __lig_insert".into(), None)));
    }

    #[test]
    fn undated_commands_end_when_their_file_changed() {
        let path = std::env::temp_dir().join(format!("lig-dated-{}", std::process::id()));
        std::fs::write(&path, "").unwrap();
        let at = UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
        std::fs::File::options().append(true).open(&path).unwrap().set_modified(at).unwrap();
        let runs = vec![(0, "a".into()), (500, "b".into()), (0, "c".into())];
        assert_eq!(dated(runs, &path), [(999_998, "a".into()), (500, "b".into()), (1_000_000, "c".into())]);
        std::fs::remove_file(&path).unwrap();
    }
}
