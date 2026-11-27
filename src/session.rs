//! `lig shell`: runs the shell in a PTY, mirrors its screen with vt100 and draws the suggestion popup.
//! Input, output and resizes feed one event loop ([`Session`]); completion runs on a worker thread.

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded, unbounded};
use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::engine::{Engine, Kind, ShellEnv, Suggestion};
use crate::keys::{self, Action, Bindings, Key};
use crate::osc::{Msg, OscFilter};
use crate::popup::{self, Rect};
use crate::pty::{self, Pty};
use crate::remote::Remote;
use crate::store::Store;
use crate::term;

/// Wait for output to settle this long before drawing.
const DEBOUNCE: Duration = Duration::from_millis(4);
/// Upper bound on the debounce wait.
const MAX_DELAY: Duration = Duration::from_millis(40);
/// How long Enter waits for suggestions that are still loading.
const ENTER_WAIT: Duration = Duration::from_millis(1500);
/// [`ENTER_WAIT`] on a `lig ssh` host.
const REMOTE_ENTER_WAIT: Duration = Duration::from_millis(300);
/// Show "Searching…" only when results take longer than this.
const LOADING_DELAY: Duration = Duration::from_millis(150);
const DOUBLE_TAP: Duration = Duration::from_millis(500);
/// While another program has the terminal, look again this often: it can exit without output.
const FOREIGN_RECHECK: Duration = Duration::from_millis(200);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(100);
const SNAPSHOT_ATTEMPTS: u8 = 3;
/// Key sequence bound to the insert function in bash and zsh.
const INSERT_KEY: &str = "\x1b[9147~";
/// Asks zsh or bash what its Tab would offer (see shell/post.zsh, shell/post.bash).
const ZSH_COMPLETE_KEY: &[u8] = b"\x1b[9148~";
/// How long zsh or bash has to answer that before it's asked once more.
const SHELL_ANSWER_WAIT: Duration = Duration::from_secs(2);
/// Ctrl+Alt+Shift+F12: the insert key of fish and PowerShell.
const SNAPSHOT_INSERT_KEY: &str = "\x1b[24;8~";
/// Ctrl+Alt+Shift+F11: asks PowerShell for its line (fish gets `SIGUSR1`).
const REPORT_KEY: &str = "\x1b[23;8~";
/// Asks bash over `lig mosh` for its line, the request's id after it (see shell/post.bash).
const HOST_REPORT_KEY: &str = "\x1b[9149~";
/// How long a host's answer to that may take: it crosses mosh, then ssh.
const HOST_SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(1500);
/// How long after a host's shell says it's ready lig first asks it for its line.
const HOST_FIRST_ASK: Duration = Duration::from_millis(100);
/// cmd's prompt marker arrives ahead of the prompt it marks (ConPTY passes it through
/// before drawing), so the input starts where the cursor rests once output pauses this long.
const PROMPT_SETTLE: Duration = Duration::from_millis(30);
/// How often a running session looks for a new theme in `config.toml` (when it draws, and
/// while the popup is open).
const CONFIG_POLL: Duration = Duration::from_millis(250);
/// Resizes come in bursts (a dragged window edge): wait for them to stop this long before
/// asking the terminal where its cursor went.
const RESIZE_SETTLE: Duration = Duration::from_millis(40);
/// How long the terminal has to answer where its cursor is.
const CURSOR_REPORT_WAIT: Duration = Duration::from_millis(200);
/// Bound output retained to rebuild the startup screen from its reported origin.
const STARTUP_REPLAY_LIMIT: usize = 1024 * 1024;
/// cmd's internal commands, for command names (cmd can't report them).
const CMD_BUILTINS: &[&str] = &[
    "assoc", "break", "call", "cd", "chdir", "cls", "color", "copy", "date", "del", "dir", "echo", "endlocal", "erase",
    "exit", "for", "ftype", "goto", "if", "md", "mkdir", "mklink", "move", "path", "pause", "popd", "prompt", "pushd",
    "rd", "rem", "ren", "rename", "rmdir", "set", "setlocal", "shift", "start", "time", "title", "type", "ver",
    "verify", "vol",
];

enum Event {
    Input(Vec<u8>, Instant),
    /// The startup reader waits for an acknowledgment before reading again.
    Output(Vec<u8>, Option<Sender<bool>>),
    Exit,
    Resize,
    /// Suggestions for `line`; `loading` names what is still being fetched; `command` is the
    /// command they complete, aliases expanded.
    Done {
        line: String,
        items: Vec<Suggestion>,
        loading: Option<String>,
        command: String,
        /// Ask the shell (zsh or bash) for its own completions of this line.
        ask_shell: Option<String>,
    },
    /// nu's own commands, read from the nu lig runs (see [`crate::nushell`]).
    ShellSpecs(crate::nushell::ShellSpecs),
    /// The host's nu's own commands (see [`crate::nushell::host_specs`]).
    HostShellSpecs(crate::nushell::ShellSpecs),
    /// An insertion for a host's fish or nu is in its file (see `host_insert`): press its key.
    HostInsert {
        ok: bool,
        run: bool,
    },
    /// A `lig ssh` host's `.zsh_history` and `.bash_history`; `ok` if they could be read.
    HostHistory {
        ok: bool,
        zsh: Vec<u8>,
        bash: Vec<u8>,
    },
}

struct Request {
    line: String,
    env: ShellEnv,
    /// Asked for with the toggle key.
    manual: bool,
}

fn cursor_position(params: &[u8]) -> Option<(u16, u16)> {
    let (row, col) = std::str::from_utf8(params).ok()?.split_once(';')?;
    let (row, col) = (row.parse().ok()?, col.parse().ok()?);
    (row != 0 && col != 0).then_some((row, col))
}

/// `config.toml`'s modification time and size: the file is read again only when they change.
fn config_stamp() -> Option<(std::time::SystemTime, u64)> {
    let m = std::fs::metadata(crate::config::config_path()).ok()?;
    Some((m.modified().ok()?, m.len()))
}

/// `rect` moved up `by` rows (down if negative), clipped to a screen `rows` high.
fn shifted(rect: Rect, by: i32, rows: u16) -> Option<Rect> {
    let top = (i32::from(rect.row) - by).max(0);
    let bottom = (i32::from(rect.row) + i32::from(rect.height) - by).min(i32::from(rows));
    (top < bottom).then(|| Rect { row: top as u16, height: (bottom - top) as u16, ..rect })
}

/// Resize the screen model to `rows`×`cols` and scroll it so its cursor lands on `cursor`,
/// where the terminal's is. Returns the rows it scrolled up (down if negative).
fn follow_terminal(parser: &mut vt100::Parser, rows: u16, cols: u16, (row, col): (u16, u16)) -> i32 {
    let shift = i32::from(parser.screen().cursor_position().0) - i32::from(row);
    if shift > 0 {
        // Up before shrinking, out of the rows that shrinking cuts off the bottom.
        parser.process(format!("\x1b[{shift}S").as_bytes());
    }
    parser.screen_mut().set_size(rows, cols);
    if shift < 0 {
        // Down after growing, into the rows growing adds at the bottom.
        parser.process(format!("\x1b[{}T", -shift).as_bytes());
    }
    parser.process(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
    shift
}

/// A resize, or a pager's exit, after which the screen model must catch up with the terminal:
/// terminals keep their content anchored to the cursor (scrolling it into or out of their
/// scrollback) where vt100 only cuts or adds rows at the bottom. lig asks the terminal where its
/// cursor is and scrolls the model by as much; shell output waits meanwhile, so both cursors are
/// compared at the same point of the stream.
struct Resync {
    /// The size the model is to get (`None`: it has it already).
    size: Option<(u16, u16)>,
    ask_at: Instant,
    /// When it was asked; the answer is awaited for [`CURSOR_REPORT_WAIT`].
    asked: Option<Instant>,
    /// Resized again after asking: the answer is for an older size.
    stale: bool,
    /// Give a child DSR completed during resize its own response window.
    shell_reply_until: Option<Instant>,
    /// Shell output that waits.
    held: Vec<u8>,
}

#[derive(Debug, PartialEq, Eq)]
enum ResyncQuery {
    Wait,
    Fallback,
    Ask,
}

fn resync_query(now: Instant, ask_at: Instant, shell_reply_until: Option<Instant>, shell_pending: bool) -> ResyncQuery {
    if !shell_pending {
        ResyncQuery::Ask
    } else if now < shell_reply_until.unwrap_or(ask_at).max(ask_at + CURSOR_REPORT_WAIT) {
        ResyncQuery::Wait
    } else {
        ResyncQuery::Fallback
    }
}

/// Whether the terminal may be sending a cursor report.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reports {
    /// None is awaited: all input is keys.
    None,
    /// One is awaited: complete reports come out, and a possible start waits for the rest.
    Due,
    /// Only for a question the shell asked longer ago than [`CURSOR_REPORT_WAIT`]: complete
    /// reports still come out, but a possible start (a lone Esc) goes on at once.
    Overdue,
}

#[derive(Debug, PartialEq, Eq)]
enum CursorInputPart {
    Keys(Vec<u8>),
    Report(Vec<u8>, (u16, u16)),
}

/// One input stream for startup, child and resize CPRs. Track paste delimiters
/// even between queries so a deadline cannot turn pasted bytes into a reply.
#[derive(Default)]
struct CursorInput {
    partial: Vec<u8>,
    partial_until: Option<Instant>,
    paste: bool,
    delimiter_prefix: usize,
}

impl CursorInput {
    fn track_paste(&mut self, byte: u8) {
        let delimiter = if self.paste { b"\x1b[201~" } else { b"\x1b[200~" };
        if byte == delimiter[self.delimiter_prefix] {
            self.delimiter_prefix += 1;
            if self.delimiter_prefix == delimiter.len() {
                self.paste = !self.paste;
                self.delimiter_prefix = 0;
            }
        } else {
            self.delimiter_prefix = usize::from(byte == 0x1b);
        }
    }

    fn flush_partial(&mut self) -> Vec<u8> {
        self.partial_until = None;
        let bytes = std::mem::take(&mut self.partial);
        for &byte in &bytes {
            self.track_paste(byte);
        }
        bytes
    }

    fn feed(&mut self, data: Vec<u8>, reports: Reports) -> Vec<CursorInputPart> {
        if self.partial.is_empty() && self.delimiter_prefix == 0 && !data.contains(&0x1b) {
            return vec![CursorInputPart::Keys(data)];
        }
        let mut bytes = std::mem::take(&mut self.partial);
        bytes.extend(data);
        let mut parts = vec![];
        let mut keys = vec![];
        let mut at = 0;
        while at < bytes.len() {
            let rest = &bytes[at..];
            if reports != Reports::None && !self.paste && rest[0] == 0x1b {
                let mut incomplete = rest.len() == 1;
                if let Some(params) = rest.strip_prefix(b"\x1b[") {
                    // A u16 row and column take at most 11 parameter bytes.
                    match params.iter().take(12).position(|b| !b.is_ascii_digit() && *b != b';') {
                        Some(end) if params[end] == b'R' => {
                            if let Some(position) = cursor_position(&params[..end]) {
                                if !keys.is_empty() {
                                    parts.push(CursorInputPart::Keys(std::mem::take(&mut keys)));
                                }
                                self.delimiter_prefix = 0;
                                at += end + 3;
                                parts.push(CursorInputPart::Report(rest[..end + 3].to_vec(), position));
                                continue;
                            }
                        }
                        None if params.len() < 12 => incomplete = true,
                        _ => {}
                    }
                }
                if incomplete && reports == Reports::Due {
                    self.partial.extend_from_slice(rest);
                    break;
                }
            }
            self.track_paste(rest[0]);
            keys.push(rest[0]);
            at += 1;
        }
        if self.partial.is_empty() {
            self.partial_until = None;
        } else {
            self.partial_until.get_or_insert_with(|| Instant::now() + CURSOR_REPORT_WAIT);
        }
        if !keys.is_empty() {
            parts.push(CursorInputPart::Keys(keys));
        }
        parts
    }
}

/// Count cursor-position queries in the child's output stream. PTY reads may
/// split the four-byte sequence, so keep just enough tail to join the next
/// chunk. The bytes themselves are still forwarded immediately.
#[derive(Default)]
struct CursorQueryCounter {
    tail: Vec<u8>,
}

impl CursorQueryCounter {
    fn observe(&mut self, output: &[u8]) -> usize {
        const QUERY: &[u8] = b"\x1b[6n";
        let mut joined = std::mem::take(&mut self.tail);
        joined.extend_from_slice(output);
        let count = joined.windows(QUERY.len()).filter(|w| *w == QUERY).count();
        let keep = joined.len().min(QUERY.len() - 1);
        self.tail.extend_from_slice(&joined[joined.len() - keep..]);
        count
    }

    fn incomplete(&self) -> bool {
        let query = b"\x1b[6n";
        (1..query.len()).any(|n| self.tail.ends_with(&query[..n]))
    }
}

/// Child output held from its first cursor query until Lig's startup query no
/// longer owns the next cursor report. Holding the rest too preserves the
/// position the child asked about.
#[derive(Default)]
struct StartupOutput {
    bytes: Vec<u8>,
    msgs: Vec<(usize, Msg)>,
    query: bool,
}

impl StartupOutput {
    fn gate(&mut self, bytes: Vec<u8>, mut msgs: Vec<(usize, Msg)>) -> (Vec<u8>, Vec<(usize, Msg)>) {
        let previous = self.bytes.len();
        for (pos, _) in &mut msgs {
            *pos += previous;
        }
        self.bytes.extend_from_slice(&bytes);
        self.msgs.extend(msgs);
        if self.query {
            return (vec![], vec![]);
        }

        const QUERY: &[u8] = b"\x1b[6n";
        let split = if let Some(at) = self.bytes.windows(QUERY.len()).position(|w| w == QUERY) {
            self.query = true;
            at
        } else {
            let keep = (1..QUERY.len()).rev().find(|&n| self.bytes.ends_with(&QUERY[..n])).unwrap_or(0);
            self.bytes.len() - keep
        };

        let held = self.bytes.split_off(split);
        let forwarded = std::mem::replace(&mut self.bytes, held);
        let mut forwarded_msgs = vec![];
        let mut held_msgs = vec![];
        for (pos, msg) in std::mem::take(&mut self.msgs) {
            if pos <= split {
                forwarded_msgs.push((pos, msg));
            } else {
                // Handle a private OSC inside a recognized DSR after the whole
                // sequence, so handling the OSC cannot interrupt its write.
                let pos = pos - split;
                held_msgs.push((if self.query { pos.max(QUERY.len()) } else { pos }, msg));
            }
        }
        self.msgs = held_msgs;
        (forwarded, forwarded_msgs)
    }

    fn into_parts(self) -> (Vec<u8>, Vec<(usize, Msg)>) {
        (self.bytes, self.msgs)
    }
}

#[derive(Debug, PartialEq, Eq)]
enum StartupWake {
    Wait,
    Drain,
    Expire,
}

struct StartupDeadline {
    until: Instant,
    queued: Option<usize>,
}

impl StartupDeadline {
    fn new(asked: Instant) -> Self {
        Self { until: asked + CURSOR_REPORT_WAIT, queued: None }
    }

    fn at(&mut self, now: Instant, queued: impl FnOnce() -> usize) -> StartupWake {
        if now < self.until {
            return StartupWake::Wait;
        }
        // Check only the events already queued at expiry, so continuous
        // output cannot postpone the deadline.
        let left = self.queued.get_or_insert_with(queued);
        if *left == 0 {
            StartupWake::Expire
        } else {
            *left -= 1;
            StartupWake::Drain
        }
    }

    fn accepts(&self, read_at: Instant) -> bool {
        read_at <= self.until
    }
}

/// Track the startup DSR without delaying child output or ordinary input.
struct StartupCursor {
    deadline: StartupDeadline,
    replay: Option<OriginReplay>,
    output: StartupOutput,
    /// Once a DSR is held, the reader waits here instead of buffering more output.
    reader_ack: Option<Sender<bool>>,
}

#[derive(Default)]
struct OriginReplay {
    bytes: Vec<u8>,
    prompt: Option<usize>,
}

impl OriginReplay {
    fn extend(&mut self, bytes: &[u8]) -> bool {
        if self.bytes.len().saturating_add(bytes.len()) > STARTUP_REPLAY_LIMIT {
            return false;
        }
        self.bytes.extend_from_slice(bytes);
        true
    }

    fn rebase(self, rows: u16, cols: u16, (row, col): (u16, u16)) -> (vt100::Parser, Option<PromptEnd>) {
        let mut parser = vt100::Parser::new(rows, cols, usize::from(rows));
        parser.process(format!("\x1b[{};{}H", row.min(rows), col.min(cols)).as_bytes());
        let at = self.prompt.unwrap_or(self.bytes.len());
        parser.process(&self.bytes[..at]);
        let anchor = self.prompt.map(|_| {
            let screen = parser.screen();
            let (row, col) = screen.cursor_position();
            PromptEnd { row, col, prefix: screen.contents_between(row, 0, row, col) }
        });
        parser.process(&self.bytes[at..]);
        (parser, anchor)
    }
}

impl StartupCursor {
    fn new() -> Option<Self> {
        term::write(b"\x1b[6n").ok()?;
        Some(Self {
            deadline: StartupDeadline::new(Instant::now()),
            replay: Some(OriginReplay::default()),
            output: StartupOutput::default(),
            reader_ack: None,
        })
    }
}

/// Replace this process with the plain shell (used when lig can't run).
#[cfg(unix)]
fn exec_shell(shell: &Path, login: bool) -> ! {
    use std::os::unix::process::CommandExt;
    let mut c = std::process::Command::new(shell);
    if login {
        c.arg("-l");
    }
    let err = c.env("LIG_DISABLE", "1").exec();
    eprintln!("lig: cannot run {}: {err}", shell.display());
    std::process::exit(127);
}

/// Run the plain shell in this console and exit with its status (Windows can't replace a process).
#[cfg(windows)]
fn exec_shell(shell: &Path, _login: bool) -> ! {
    match std::process::Command::new(shell).env("LIG_DISABLE", "1").status() {
        Ok(st) => std::process::exit(st.code().unwrap_or(1)),
        Err(err) => {
            eprintln!("lig: cannot run {}: {err}", shell.display());
            std::process::exit(127);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ShellKind {
    Zsh,
    Bash,
    Fish,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum InsertionTransport {
    Inline,
    File,
    /// Typed as keys: backspaces, the text, then Left (cmd has no way to bind a function).
    Keys,
}

/// The shell's name without `.exe`, lowercase: `pwsh`, `cmd`, `bash`.
fn shell_name(shell: &Path) -> String {
    let name = shell.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_lowercase();
    name.strip_suffix(".exe").map(str::to_string).unwrap_or(name)
}

fn is_powershell(shell: &Path) -> bool {
    matches!(shell_name(shell).as_str(), "pwsh" | "powershell")
}

impl InsertionTransport {
    fn for_shell(shell: &Path) -> Self {
        match shell_name(shell).as_str() {
            "bash" | "fish" | "nu" | "pwsh" | "powershell" => Self::File,
            "cmd" => Self::Keys,
            _ => Self::Inline,
        }
    }
}

/// The keys that make the edit `<delete>;<back>;<text>`.
fn insertion_keys(payload: &str) -> Option<Vec<u8>> {
    let mut parts = payload.splitn(3, ';');
    let delete: usize = parts.next()?.parse().ok()?;
    let back: usize = parts.next()?.parse().ok()?;
    let text = parts.next()?;
    Some(format!("{}{text}{}", "\x7f".repeat(delete), "\x1b[D".repeat(back)).into_bytes())
}

/// Build the insertion only after its file payload has been written successfully.
fn insertion_bytes(transport: InsertionTransport, fish: bool, file: &Path, payload: &str) -> std::io::Result<Vec<u8>> {
    match transport {
        InsertionTransport::File => {
            std::fs::write(file, payload)?;
            Ok(if fish { SNAPSHOT_INSERT_KEY.as_bytes().to_vec() } else { INSERT_KEY.as_bytes().to_vec() })
        }
        InsertionTransport::Inline => Ok(format!("{INSERT_KEY}{payload}\x07").into_bytes()),
        InsertionTransport::Keys => insertion_keys(payload).ok_or_else(|| std::io::Error::other("bad insertion")),
    }
}

/// PowerShell's integration (see shell/lig.ps1), run after its profile.
const PWSH_INIT: &str = include_str!("../shell/lig.ps1");

/// How the shell starts inside lig: its arguments and what it's told through the environment.
fn shell_command(shell: &Path, login: bool) -> pty::Command {
    let mut cmd = pty::Command::new(shell);
    if is_powershell(shell) {
        // Encoded (UTF-16LE, base64): no quoting, and no execution policy, which a script file
        // would need (Windows PowerShell's default allows none).
        let utf16: Vec<u8> = PWSH_INIT.encode_utf16().flat_map(u16::to_le_bytes).collect();
        cmd.arg("-NoLogo").arg("-NoExit").arg("-EncodedCommand").arg(crate::remote::base64(&utf16));
    } else if shell_name(shell) == "cmd" {
        // A prompt marker and the folder ahead of the prompt the user has (or cmd's default).
        let prompt = std::env::var("PROMPT").unwrap_or_else(|_| "$P$G".into());
        cmd.env("PROMPT", format!("$E]6973;C;$P\x07$E]6973;S\x07{prompt}"));
    } else if login {
        cmd.arg("-l");
    }
    cmd.cwd = std::env::current_dir().ok();
    cmd.env("LIG_PID", std::process::id().to_string());
    cmd
}

pub fn run(shell: PathBuf, login: bool) -> Result<i32> {
    if !term::is_terminal() {
        exec_shell(&shell, login);
    }
    let (rows, cols) = term::size().unwrap_or((24, 80));
    // bash, fish, nu and PowerShell read accepted completions from this file.
    let runtime = match crate::private_dir() {
        Ok(dir) => dir,
        Err(e) => {
            eprintln!("lig: {e}; starting the shell without lig");
            exec_shell(&shell, login);
        }
    };
    let insert_file = runtime.join(format!("lig-{}.insert", std::process::id()));
    let query_file = insert_file.with_extension("query");
    let mut cmd = shell_command(&shell, login);
    #[cfg(target_os = "macos")]
    if let (Ok(session), Ok(pane)) = (std::env::var("ZELLIJ_SESSION_NAME"), std::env::var("ZELLIJ_PANE_ID")) {
        cmd.env(crate::mirror::PANE_OWNER_ENV, crate::mirror::pane_owner(&session, &pane));
    }
    cmd.env("LIG_INSERT_FILE", &insert_file);
    cmd.env("LIG_QUERY_FILE", &query_file);
    let (pty_handle, reader, writer) = match Pty::spawn(&cmd, rows, cols, term::pixels()) {
        Ok(spawned) => spawned,
        Err(e) => {
            eprintln!("lig: starting {} failed: {e:#}", shell.display());
            exec_shell(&shell, login);
        }
    };
    let tty = pty_handle.tty_name();
    let shell_pid = pty_handle.pid();
    #[cfg(unix)]
    let _mirror = shell_pid.and_then(|pid| {
        crate::mirror::spawn(pid, std::env::var_os("SHELL").unwrap_or_else(|| shell.clone().into_os_string()))
    });
    // macOS: the zellij mirror above takes zellij's API; this one is what `ps`, tmux and WezTerm see.
    #[cfg(target_os = "macos")]
    let _mirror_argv = shell_pid.and_then(|pid| {
        crate::mirror_argv::spawn(pid, std::env::var_os("SHELL").unwrap_or_else(|| shell.clone().into_os_string()))
    });

    let raw = term::RawMode::enable()?;
    crate::log(&format!("startup shell={} size={rows}x{cols} origin=async", shell.display()));
    let parser = vt100::Parser::new(rows, cols, usize::from(rows));
    let cfg = Config::load();
    crate::icons::set_overrides(&cfg.icons);

    let (tx, rx) = unbounded::<Event>();
    let startup_cursor = StartupCursor::new();
    spawn_stdin_reader(tx.clone());
    spawn_pty_reader(reader, tx.clone(), startup_cursor.is_some());
    if shell_name(&shell) == "nu" {
        let (tx, nu) = (tx.clone(), shell.clone());
        std::thread::spawn(move || {
            if let Some(specs) = crate::nushell::specs(&nu) {
                let _ = tx.send(Event::ShellSpecs(specs));
            }
        });
    }
    let resized = tx.clone();
    term::watch_resize(move || resized.send(Event::Resize).is_ok())?;
    let exited = tx.clone();
    pty_handle.on_exit(move || {
        let _ = exited.send(Event::Exit);
    });
    let (req_tx, req_rx) = unbounded::<Request>();
    let behavior = cfg.behavior.clone();
    let events = tx.clone();
    std::thread::spawn(move || worker(req_rx, tx, behavior));
    std::thread::spawn(crate::help::prune_cache);

    let mut s = Session {
        events,
        insert_file,
        query_file,
        fish_sequence: 0,
        unknown_rows: 0,
        startup_cursor,
        cursor_input: CursorInput::default(),
        insertion_transport: InsertionTransport::for_shell(&shell),
        fish_shell: shell_name(&shell) == "fish" || is_powershell(&shell),
        report_key: is_powershell(&shell),
        cmd_shell: shell_name(&shell) == "cmd",
        nu_shell: shell_name(&shell) == "nu",
        zsh_shell: false,
        nu_tail: vec![],
        prompt_pending: false,
        held: vec![],
        unconfirmed: String::new(),
        unconfirmed_at: Instant::now(),
        fish_refresh: None,
        prompt_end: None,
        shell_nav: false,
        cwd_fallback: std::env::current_dir().unwrap_or_else(|_| "/".into()),
        bindings: Bindings::new(&cfg.keys),
        shell_asked: None,
        shell_unanswered: vec![],
        shell_asked_at: None,
        shell_asked_again: None,
        history: cfg.behavior.history.then(crate::history::History::open),
        history_mode: false,
        history_folder: false,
        run_after_insert: None,
        cfg,
        pty: writer,
        shell_pty: pty_handle,
        parser,
        osc: OscFilter::default(),
        editing: false,
        hidden: false,
        line: String::new(),
        env: ShellEnv {
            builtins: if shell_name(&shell) == "cmd" {
                std::sync::Arc::new(CMD_BUILTINS.iter().map(|b| b.to_string()).collect())
            } else {
                Default::default()
            },
            shell: Some(shell.clone()),
            ..Default::default()
        },
        shell_pid,
        tty,
        foreign: false,
        foreign_checked: Instant::now(),
        remote: None,
        req_tx,
        items: vec![],
        items_line: String::new(),
        items_command: String::new(),
        selected: 0,
        scroll: 0,
        navigated: false,
        just_accepted: false,
        close_after_insert: false,
        shown: None,
        stale_since: None,
        manual: false,
        loading: None,
        pending_enter: None,
        enter_after: None,
        deferred: vec![],
        dismissed: None,
        dismissed_by: None,
        drawn: None,
        resync: None,
        normal_unsynced: false,
        shell_cursor_queries: 0,
        shell_cursor_asked: None,
        cursor_query_counter: CursorQueryCounter::default(),
        config_stamp: config_stamp(),
        config_checked: Instant::now(),
        sync_open: false,
        dirty: false,
        dirty_since: Instant::now(),
        last_output: Instant::now(),
    };
    s.event_loop(&rx);

    s.clear_popup();
    let _ = std::fs::remove_file(&s.insert_file);
    let _ = std::fs::remove_file(&s.query_file);
    drop(raw);
    Ok(s.shell_pty.wait())
}

fn spawn_stdin_reader(tx: Sender<Event>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match term::read(&mut buf) {
                Ok(n) if n > 0 => {
                    let read_at = Instant::now();
                    if tx.send(Event::Input(buf[..n].to_vec(), read_at)).is_err() {
                        break;
                    }
                }
                _ => {
                    let _ = tx.send(Event::Exit);
                    break;
                }
            }
        }
    });
}

fn spawn_pty_reader(mut reader: Box<dyn Read + Send>, tx: Sender<Event>, mut startup: bool) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Event::Exit);
                    break;
                }
                Ok(n) => {
                    let data = buf[..n].to_vec();
                    if startup {
                        // Only one chunk can be outstanding during startup.
                        let (ack, next) = bounded(1);
                        if tx.send(Event::Output(data, Some(ack))).is_err() {
                            break;
                        }
                        match next.recv() {
                            Ok(more_startup) => startup = more_startup,
                            Err(_) => break,
                        }
                    } else if tx.send(Event::Output(data, None)).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// The local shell's state, kept while its `lig ssh` runs.
struct RemoteSession {
    #[cfg_attr(windows, allow(dead_code))]
    pid: u32,
    local_env: ShellEnv,
    fish_shell: bool,
    zsh_shell: bool,
    insertion_transport: InsertionTransport,
    /// Its commands, for Ctrl+R there; the local history keeps none of them.
    history: crate::history::HostHistory,
    /// Over `lig mosh`: its client always has the alternate screen, which isn't a full-screen app's.
    mosh: bool,
    /// Fish or nu there takes insertions from this file, which lig writes over its ssh connection.
    insert_file: Option<String>,
    /// The host's nu was asked for its commands' specs.
    nu_specs_asked: bool,
}

/// `X` must come from a running `lig ssh`: anything printed to the terminal could claim to be one.
fn is_lig_ssh(pid: u32, control: &str) -> bool {
    let dir = crate::lig_dir();
    // Both sides from the same query, so symlinks and a replaced binary resolve alike.
    let theirs = crate::process::executable(pid);
    Path::new(control) == dir.join(format!("ssh-{pid}"))
        && theirs.is_some()
        && theirs == crate::process::executable(std::process::id())
}

/// Add `key` to the text typed so far, or give up (`None`) if it isn't a plain character.
fn push_typed(typed: &mut Option<String>, key: &Key) {
    match (&key.code, key.ctrl || key.alt) {
        (keys::Code::Char(c), false) => {
            if let Some(t) = typed {
                t.push(*c);
            }
        }
        _ => *typed = None,
    }
}

/// Whether Enter picks the selected suggestion instead of running the line.
/// `auto`: unless the word already equals a suggestion; `after-space`: only after a space;
/// `always`: while the popup is open. A navigated selection is always picked; except with
/// `always`, the Enter right after a pick runs the line.
fn enter_accepts(mode: &str, word_empty: bool, complete: bool, navigated: bool, just_accepted: bool) -> bool {
    match mode {
        "never" => false,
        "always" => true,
        _ if navigated => true,
        _ if just_accepted => false,
        "after-space" => word_empty,
        _ => word_empty || !complete,
    }
}

/// What Enter does while the popup is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Enter {
    /// Insert the selected suggestion.
    Pick,
    /// Insert the selected suggestion, then run the line.
    PickAndRun,
    /// Send Enter to the shell: run the line as typed.
    Run,
    /// Only close the popup.
    Close,
}

/// [`enter_accepts`], plus `runs`: the command is in `enter_runs_commands`, so a pick also
/// runs the line, and Enter never stops at only closing the popup.
fn enter_action(
    mode: &str,
    runs: bool,
    word_empty: bool,
    complete: bool,
    navigated: bool,
    just_accepted: bool,
) -> Enter {
    // `always`: Enter on a complete word only closes the popup.
    if mode == "always" && !navigated && complete {
        return if runs { Enter::Run } else { Enter::Close };
    }
    match (enter_accepts(mode, word_empty, complete, navigated, just_accepted), runs) {
        (false, _) => Enter::Run,
        (true, false) => Enter::Pick,
        (true, true) => Enter::PickAndRun,
    }
}

/// Where prompt marks (OSC 133;A, sent by terminal integrations like Ghostty's) start in `out`.
fn prompt_marks(out: &[u8]) -> Vec<usize> {
    const MARK: &[u8] = b"\x1b]133;A";
    out.windows(MARK.len() + 1)
        .enumerate()
        .filter(|(_, w)| w.starts_with(MARK) && matches!(w[MARK.len()], b';' | b'\x07' | b'\x1b'))
        .map(|(i, _)| i)
        .collect()
}

/// How long ago, roughly: `5m ago`, `3h ago`, `2d ago`.
fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3600 => format!("{}m ago", secs / 60),
        3600..86400 => format!("{}h ago", secs / 3600),
        _ => format!("{}d ago", secs / 86400),
    }
}

/// nu's own shell-integration marks say what lig's do for other shells: the prompt ends
/// (`133;B`, before the input on every redraw) and a command starts (`133;C`).
fn nu_marks(out: &[u8]) -> Vec<(usize, Msg)> {
    const MARK: &[u8] = b"\x1b]133;";
    out.windows(MARK.len() + 2)
        .enumerate()
        .filter(|(_, w)| w.starts_with(MARK) && matches!(w[MARK.len() + 1], b';' | b'\x07' | b'\x1b'))
        .filter_map(|(i, w)| match w[MARK.len()] {
            b'B' => Some((i, Msg::PromptEnd)),
            b'C' => Some((i, Msg::Finish)),
            _ => None,
        })
        .collect()
}

/// The start of a nu mark the output ends in, which the next output finishes.
fn unfinished_nu_mark(out: &[u8]) -> &[u8] {
    const MARK: &[u8] = b"\x1b]133;";
    let start = out.len().saturating_sub(MARK.len() + 1);
    let Some(i) = out[start..].iter().rposition(|&b| b == 0x1b) else { return &[] };
    let rest = &out[start + i..];
    let unfinished = if rest.len() <= MARK.len() {
        MARK.starts_with(rest)
    } else {
        rest.starts_with(MARK) && matches!(rest[MARK.len()], b'B' | b'C')
    };
    if unfinished { rest } else { &[] }
}

/// Output that only sets terminal modes, or none: the screen and the line stay as they were.
/// Fish 4.0 toggles bracketed paste, the keypad and the keyboard protocols around each signal
/// handler it runs, so around each snapshot reply too.
fn only_modes(mut bytes: &[u8]) -> bool {
    while !bytes.is_empty() {
        bytes = match bytes {
            [0x1b, b'=' | b'>', rest @ ..] => rest,
            [0x1b, b'[', b'?' | b'<' | b'=' | b'>', rest @ ..] => {
                let Some(end) = rest.iter().position(|b| !b.is_ascii_digit() && *b != b';') else { return false };
                if !matches!(rest[end], b'h' | b'l' | b'm' | b'u') {
                    return false;
                }
                // Except switching to or from the alternate screen.
                let screen_switch = |p: &[u8]| matches!(p, b"47" | b"1047" | b"1049");
                if bytes[2] == b'?' && rest[..end].split(|&b| b == b';').any(screen_switch) {
                    return false;
                }
                &rest[end + 1..]
            }
            _ => return false,
        };
    }
    true
}

/// Answers the newest request: quick results (no commands run) first, then full ones.
fn worker(rx: Receiver<Request>, tx: Sender<Event>, behavior: crate::config::Behavior) {
    let mut engine = Engine::new(Store::default(), behavior);
    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let c = engine.complete(&req.line, &req.env, false, req.manual);
        let _ = tx.send(Event::Done {
            line: req.line.clone(),
            items: c.items,
            loading: c.loading,
            command: c.command,
            ask_shell: c.ask_shell,
        });
        if c.incomplete && rx.is_empty() {
            let c = engine.complete(&req.line, &req.env, true, req.manual);
            let _ = tx.send(Event::Done {
                line: req.line,
                items: c.items,
                loading: c.loading.filter(|_| c.ask_shell.is_some()),
                command: c.command,
                ask_shell: c.ask_shell,
            });
        }
    }
}

/// Where the input starts on screen (bash: right after the prompt marker).
struct PromptEnd {
    row: u16,
    col: u16,
    /// The prompt text on that row, to find it again after scrolling.
    prefix: String,
}

struct PendingSnapshot {
    id: u64,
    deadline: Instant,
    changed: bool,
}

#[derive(Default)]
struct FishRefresh {
    due: Option<Instant>,
    in_flight: Option<PendingSnapshot>,
    attempts: u8,
    stale: bool,
    /// [`SNAPSHOT_TIMEOUT`] unless set.
    timeout: Option<Duration>,
}

impl FishRefresh {
    fn changed(&mut self) {
        self.stale = true;
        self.attempts = 0;
        if let Some(request) = &mut self.in_flight {
            request.changed = true;
        }
        self.due = Some(Instant::now() + DEBOUNCE);
    }

    fn wake(&self) -> Option<Instant> {
        self.in_flight.as_ref().map(|request| request.deadline).or(self.due)
    }

    fn take_due(&mut self, id: u64) -> bool {
        if !self.wake().is_some_and(|due| due <= Instant::now()) {
            return false;
        }
        self.due = None;
        self.in_flight = None;
        if self.attempts == SNAPSHOT_ATTEMPTS {
            return false;
        }
        self.attempts += 1;
        let deadline = Instant::now() + self.timeout.unwrap_or(SNAPSHOT_TIMEOUT);
        self.in_flight = Some(PendingSnapshot { id, deadline, changed: false });
        true
    }

    fn reported(&mut self, id: u64) -> bool {
        if !self.in_flight.as_ref().is_some_and(|request| request.id == id) {
            return false;
        }
        let request = self.in_flight.take().unwrap();
        self.attempts = 0;
        if request.changed {
            self.due = Some(Instant::now() + DEBOUNCE);
        } else {
            self.stale = false;
            self.due = None;
        }
        true
    }
}

struct Session {
    query_file: PathBuf,
    fish_sequence: u64,
    /// Rows above the startup cursor are absent from the screen model.
    unknown_rows: u16,
    startup_cursor: Option<StartupCursor>,
    cursor_input: CursorInput,
    insertion_transport: InsertionTransport,
    /// The shell reports its line when asked, as fish does; PowerShell speaks the same
    /// protocol (see shell/lig.ps1).
    fish_shell: bool,
    /// Asked with [`REPORT_KEY`] instead of `SIGUSR1` (PowerShell).
    report_key: bool,
    /// cmd: the line is read off the screen like bash's, but the marker comes early
    /// (see [`PROMPT_SETTLE`]), and Enter ends the line.
    cmd_shell: bool,
    /// nu: the line is read off the screen like bash's, between nu's own prompt-end mark
    /// and the cursor (see [`nu_marks`]); insertions take fish's key.
    nu_shell: bool,
    /// zsh reports its exact line: an insertion can check it (see shell/post.zsh).
    zsh_shell: bool,
    /// A nu mark cut short at the end of the last output, finished by the next.
    nu_tail: Vec<u8>,
    /// cmd's prompt marker arrived and output hasn't paused yet.
    prompt_pending: bool,
    /// Keys typed while `prompt_pending`, sent once the input start is known.
    held: Vec<u8>,
    /// Text typed to fish that no report has shown yet. Fish is asked by a signal, which can
    /// overtake keys still in the terminal: a reply without them is for an older line.
    unconfirmed: String,
    /// When text was last added to `unconfirmed`: older text is let go (see [`ENTER_WAIT`]).
    unconfirmed_at: Instant,
    fish_refresh: Option<FishRefresh>,
    cfg: Config,
    /// The line the shell was last asked about (see [`ZSH_COMPLETE_KEY`]).
    shell_asked: Option<String>,
    /// The lines asked about and not answered yet, oldest first, and when the last was asked.
    shell_unanswered: Vec<String>,
    shell_asked_at: Option<Instant>,
    /// The line asked about a second time (see [`Session::ask_shell_again`]).
    shell_asked_again: Option<String>,
    /// The commands that ran (`behavior.history`), and whether the popup shows them now.
    history: Option<crate::history::History>,
    history_mode: bool,
    /// The history popup lists only what ran in the shell's folder (`keys.folder_history`).
    history_folder: bool,
    /// An accept-and-run waits for the shell to show this line (left of the cursor) before
    /// its Enter: fish and PowerShell can turn an insertion down (see `accept_selected`).
    run_after_insert: Option<String>,
    insert_file: PathBuf,
    /// Set by the prompt marker of bash, nu or cmd: the line is read off the screen.
    prompt_end: Option<PromptEnd>,
    /// The last key went to the shell's history or completion (Up/Down/Tab): its lines get
    /// no popup, so those keys keep going to the shell. Typing ends it.
    shell_nav: bool,
    bindings: Bindings,
    pty: Box<dyn Write + Send>,
    shell_pty: Pty,
    parser: vt100::Parser,
    osc: OscFilter,

    // Shell state reported by the integration.
    /// The shell is editing a command line (from the prompt until Enter).
    editing: bool,
    /// Something else uses the line (history search, the shell's own menu).
    hidden: bool,
    /// Edit buffer left of the cursor.
    line: String,
    env: ShellEnv,
    shell_pid: Option<u32>,
    tty: Option<PathBuf>,
    /// Another program uses the terminal (see [`foreign_program`]): no popup until it's gone.
    foreign: bool,
    foreign_checked: Instant,
    /// The shell is logged in to a host through `lig ssh`.
    remote: Option<RemoteSession>,
    cwd_fallback: PathBuf,
    req_tx: Sender<Request>,
    /// For the session's own background work (a `lig ssh` host's history).
    events: Sender<Event>,

    // Popup state.
    items: Vec<Suggestion>,
    /// The line `items` were computed for.
    items_line: String,
    /// The command `items` complete, aliases expanded.
    items_command: String,
    selected: usize,
    /// Index of the first visible item.
    scroll: usize,
    navigated: bool,
    /// A suggestion was just accepted and nothing typed since: Enter runs.
    just_accepted: bool,
    /// Keep the popup closed for the line the accepted insert produces.
    close_after_insert: bool,
    /// Where the last render showed suggestions.
    shown: Option<Rect>,
    /// Since when the popup shows suggestions for an older line (see `render`).
    stale_since: Option<Instant>,
    /// The popup was asked for with the toggle key (until the line is run).
    manual: bool,
    /// (line, message, since) while slower suggestions are being fetched.
    loading: Option<(String, String, Instant)>,
    /// When Enter (or a popup key, see `popup_key_should_wait`) was pressed while the line or
    /// its suggestions were on the way: it waits, up to [`ENTER_WAIT`].
    pending_enter: Option<Instant>,
    /// That Enter and the input after it, replayed in order.
    deferred: Vec<u8>,
    /// Enter typed in the same chunk as other keys: the line before them, and the line they
    /// make if plain characters. Enter waits for it (see [`Session::enter_ready`]).
    enter_after: Option<(String, Option<String>)>,
    /// The popup was closed for this line; it reopens when the line changes.
    dismissed: Option<String>,
    dismissed_by: Option<(Key, Instant)>,
    drawn: Option<Rect>,
    /// The model catching up with a resized terminal (see [`Resync`]).
    resync: Option<Resync>,
    /// The terminal was resized while a pager had the alternate screen: the normal screen is
    /// checked once it's back.
    normal_unsynced: bool,
    /// Cursor position questions the shell (or a program) sent the terminal, not answered yet:
    /// lig asks only when there are none, so it never takes their answer.
    shell_cursor_queries: usize,
    /// When the shell last asked one.
    shell_cursor_asked: Option<Instant>,
    /// Finds those questions even when a PTY read splits `ESC [ 6 n`.
    cursor_query_counter: CursorQueryCounter,
    /// `config.toml` as last read (see [`config_stamp`]), and when it was last looked at.
    config_stamp: Option<(std::time::SystemTime, u64)>,
    config_checked: Instant,
    /// A synchronized-output block is open (see `begin_sync`).
    sync_open: bool,
    /// A redraw is due, once output settles (see [`DEBOUNCE`]).
    dirty: bool,
    dirty_since: Instant,
    last_output: Instant,
}

impl Session {
    fn event_loop(&mut self, rx: &Receiver<Event>) {
        loop {
            let startup_wake =
                self.startup_cursor.as_mut().map_or(StartupWake::Wait, |p| p.deadline.at(Instant::now(), || rx.len()));
            if startup_wake == StartupWake::Expire {
                self.expire_startup_cursor();
            }
            self.expire_cursor_input();
            self.resync_tick();
            if self.shell_answer_due().is_some_and(|due| due <= Instant::now()) {
                self.shell_asked_at = None;
                self.ask_shell_again();
            }
            if self.fish_refresh.as_ref().and_then(FishRefresh::wake).is_some_and(|due| due <= Instant::now()) {
                self.fish_sequence += 1;
                if self.fish_refresh.as_mut().unwrap().take_due(self.fish_sequence) {
                    let sent = if let Some(r) = &self.remote {
                        // The host can't read the query file: bash reads the id after the key;
                        // fish can't, and answers 0, taken for the request in flight.
                        let ask = if r.insert_file.is_some() {
                            REPORT_KEY.to_string()
                        } else {
                            format!("{HOST_REPORT_KEY}{}\x07", self.fish_sequence)
                        };
                        self.write_pty(ask.as_bytes());
                        true
                    } else {
                        std::fs::write(&self.query_file, self.fish_sequence.to_string()).is_ok() && self.ask_for_line()
                    };
                    if !sent {
                        crate::log("fish snapshot request failed");
                        self.fish_refresh = None;
                        self.hidden = true;
                    }
                }
            }
            // No drawing while the model catches up with the terminal (see `Resync`).
            let mut deadline = (self.dirty && self.resync.is_none())
                .then(|| (self.last_output + DEBOUNCE).min(self.dirty_since + MAX_DELAY));
            if let Some(w) = self.resync_wake() {
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if let Some(w) = self.shell_answer_due() {
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if let Some(p) = &self.startup_cursor {
                deadline = Some(deadline.map_or(p.deadline.until, |d| d.min(p.deadline.until)));
            }
            if self.startup_cursor.is_none()
                && let Some(w) = self.cursor_input.partial_until
            {
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            let woken_for_more = deadline;
            if self.drawn.is_some() {
                // An open popup takes a new theme without waiting for a key.
                let w = self.config_checked + CONFIG_POLL;
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if let Some(w) = self.fish_refresh.as_ref().and_then(FishRefresh::wake) {
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if let Some(w) = self.loading_wake() {
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if let Some(w) = self.keep_wake() {
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if let Some(t) = self.pending_enter {
                let w = t + self.enter_wait();
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if self.foreign {
                let w = self.foreign_checked + FOREIGN_RECHECK;
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if self.prompt_pending {
                let w = self.last_output + PROMPT_SETTLE;
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            let ev = if startup_wake == StartupWake::Drain {
                match rx.try_recv() {
                    Ok(ev) => ev,
                    Err(_) => {
                        self.expire_startup_cursor();
                        continue;
                    }
                }
            } else if let Some(due) = deadline {
                match rx.recv_deadline(due) {
                    Ok(ev) => ev,
                    Err(RecvTimeoutError::Timeout) => {
                        self.expire_cursor_input();
                        if self.drawn.is_some()
                            && self.config_checked.elapsed() >= CONFIG_POLL
                            && !self.reload_theme()
                            && woken_for_more.is_none_or(|d| d > Instant::now())
                        {
                            // Only the theme check was due, and nothing changed: no redraw.
                            continue;
                        }
                        if self.prompt_pending && self.last_output.elapsed() >= PROMPT_SETTLE {
                            self.prompt_settled();
                        }
                        if self.foreign && self.foreign_checked.elapsed() >= FOREIGN_RECHECK {
                            self.update_foreign();
                            if self.foreign {
                                continue;
                            }
                        }
                        // A snapshot request alone must not repaint the popup.
                        if self
                            .fish_refresh
                            .as_ref()
                            .and_then(FishRefresh::wake)
                            .is_some_and(|due| due <= Instant::now())
                        {
                            continue;
                        }
                        if self.pending_enter.is_some_and(|t| t.elapsed() >= self.enter_wait()) {
                            self.resolve_pending_enter();
                        }
                        self.render();
                        continue;
                    }
                    Err(RecvTimeoutError::Disconnected) => return,
                }
            } else {
                match rx.recv() {
                    Ok(ev) => ev,
                    Err(_) => return,
                }
            };
            match ev {
                Event::Output(data, ack) => {
                    // A resize can be pending during startup, but it cannot
                    // query the terminal until the startup CPR is resolved.
                    // Finish a child DSR already on the wire before resync
                    // holds the rest of the output.
                    if let Some(r) = self
                        .resync
                        .as_mut()
                        .filter(|_| self.startup_cursor.is_none() && !self.cursor_query_counter.incomplete())
                    {
                        r.held.extend_from_slice(&data);
                    } else {
                        self.on_output(&data);
                    }
                    if let Some(ack) = ack {
                        if let Some(probe) = self.startup_cursor.as_mut().filter(|p| p.output.query) {
                            probe.reader_ack = Some(ack);
                        } else {
                            let _ = ack.send(self.startup_cursor.is_some());
                        }
                    }
                }
                Event::Input(data, read_at) => self.on_terminal_input(data, read_at),
                Event::Done { line, items, loading, command, ask_shell } => {
                    if let Some(asked) = ask_shell {
                        self.ask_shell(asked);
                    }
                    self.on_done(line, items, loading, command)
                }
                Event::Resize => self.on_resize(),
                // The local nu's, even when they come during a `lig ssh`.
                Event::ShellSpecs(specs) => match &mut self.remote {
                    Some(r) => r.local_env.shell_specs = Some(specs),
                    None => {
                        self.env.shell_specs = Some(specs);
                        self.items_line.clear();
                        self.request();
                    }
                },
                Event::HostShellSpecs(specs) => {
                    if self.remote.is_some() {
                        self.env.shell_specs = Some(specs);
                        self.items_line.clear();
                        self.request();
                    }
                }
                Event::HostInsert { ok, run } => {
                    if ok && self.remote.is_some() {
                        let mut keys = SNAPSHOT_INSERT_KEY.as_bytes().to_vec();
                        if run {
                            keys.push(b'\r');
                        }
                        self.write_pty(&keys);
                        if let Some(refresh) = &mut self.fish_refresh {
                            refresh.changed();
                        }
                    }
                }
                Event::HostHistory { ok, zsh, bash } => {
                    if let Some(r) = &mut self.remote {
                        if ok {
                            r.history.read_files(&zsh, &bash);
                        } else {
                            // Ask again at the next Ctrl+R.
                            r.history.asked = false;
                        }
                        if self.history_mode {
                            self.items_line.clear();
                            self.request();
                        }
                    }
                }
                Event::Exit => return,
            }
        }
    }

    fn awaited_reports(&self) -> Reports {
        let lig_asked = self.startup_cursor.is_some() || self.resync.as_ref().is_some_and(|r| r.asked.is_some());
        let shell_asked_lately = self.shell_cursor_asked.is_some_and(|at| at.elapsed() < CURSOR_REPORT_WAIT);
        match self.shell_cursor_queries {
            _ if lig_asked => Reports::Due,
            0 => Reports::None,
            // A question the terminal never answered would otherwise hold every later Esc.
            _ if shell_asked_lately => Reports::Due,
            _ => Reports::Overdue,
        }
    }

    fn on_terminal_input(&mut self, data: Vec<u8>, read_at: Instant) {
        for part in self.cursor_input.feed(data, self.awaited_reports()) {
            match part {
                CursorInputPart::Keys(bytes) => self.on_input(&bytes),
                CursorInputPart::Report(_, position) if self.startup_cursor.is_some() => {
                    // While the child query is still held, this is Lig's reply,
                    // even if it arrived too late to use for the startup model.
                    let origin = self.startup_cursor.as_ref().filter(|p| p.deadline.accepts(read_at)).map(|_| position);
                    self.finish_startup_cursor(origin);
                }
                CursorInputPart::Report(bytes, _) if self.shell_cursor_queries > 0 => {
                    // CPRs for the shell (nu asks at each redraw) are replies,
                    // so pass them through without processing them as keys.
                    self.shell_cursor_queries -= 1;
                    self.write_pty(&bytes);
                }
                CursorInputPart::Report(_, position) if self.resync.as_ref().is_some_and(|r| r.asked.is_some()) => {
                    self.resynced(Some(position));
                }
                CursorInputPart::Report(bytes, _) => self.on_input(&bytes),
            }
        }
        if self.awaited_reports() != Reports::Due {
            self.flush_cursor_partial();
        }
    }

    fn flush_cursor_partial(&mut self) {
        let bytes = self.cursor_input.flush_partial();
        if !bytes.is_empty() {
            self.on_input(&bytes);
        }
    }

    fn expire_cursor_input(&mut self) {
        // The startup timeout already limits the lifetime of a partial CPR.
        // Keep its prefix while consuming input queued before the deadline.
        if self.startup_cursor.is_none() && self.cursor_input.partial_until.is_some_and(|w| Instant::now() >= w) {
            self.flush_cursor_partial();
        }
    }

    fn expire_startup_cursor(&mut self) {
        self.flush_cursor_partial();
        self.finish_startup_cursor(None);
    }

    fn finish_startup_cursor(&mut self, origin: Option<(u16, u16)>) {
        let Some(probe) = self.startup_cursor.take() else { return };
        let StartupCursor { replay, output, reader_ack, .. } = probe;
        let replay_missing = replay.is_none();
        crate::log(&format!("startup origin={origin:?}"));
        if let Some(origin) = origin {
            if let Some(replay) = replay {
                let (rows, cols) = self.parser.screen().size();
                let (parser, prompt) = replay.rebase(rows, cols, origin);
                self.parser = parser;
                self.unknown_rows = origin.0.saturating_sub(1).min(rows);
                if self.prompt_end.is_some() {
                    self.prompt_end = prompt;
                    self.read_line_from_screen();
                }
            }
        }
        // Release the child query at the original startup deadline if Lig got
        // no answer. Standard CPRs carry no ID, so a reply after that deadline
        // has the same ambiguity as with the old synchronous probe.
        let (output, msgs) = output.into_parts();
        if !output.is_empty() || !msgs.is_empty() {
            self.on_filtered_output(output, msgs);
        }
        if replay_missing {
            // A resize or replay overflow makes the origin insufficient.
            // If only the CPR timed out, keep the provisional model as upstream
            // did: asking the same terminal again would delay the first popup.
            self.schedule_resync();
        }
        if self.waiting_for_shell_cursor_report()
            && let Some(r) = &mut self.resync
        {
            r.shell_reply_until = Some(Instant::now() + CURSOR_REPORT_WAIT);
        }
        if let Some(ack) = reader_ack {
            let _ = ack.send(false);
        }
        self.mark_dirty();
    }

    fn schedule_resync(&mut self) {
        if self.resync.is_none()
            && let Some((rows, cols)) = term::size()
        {
            self.resync = Some(Resync {
                size: Some((rows, cols)),
                ask_at: Instant::now(),
                asked: None,
                stale: false,
                shell_reply_until: None,
                held: vec![],
            });
        }
    }

    fn process_child_screen(&mut self, bytes: &[u8]) {
        if let Some(probe) = &mut self.startup_cursor
            && let Some(replay) = &mut probe.replay
            && !replay.extend(bytes)
        {
            probe.replay = None;
        }
        self.parser.process(bytes);
    }

    fn record_startup_prompt(&mut self) {
        if let Some(replay) = self.startup_cursor.as_mut().and_then(|p| p.replay.as_mut()) {
            replay.prompt = Some(replay.bytes.len());
        }
    }

    /// Ask fish (a signal) or PowerShell (a key) to report its line.
    fn ask_for_line(&mut self) -> bool {
        if self.report_key {
            self.write_pty(REPORT_KEY.as_bytes());
            return true;
        }
        #[cfg(unix)]
        return self.shell_pid.is_some_and(|pid| unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR1) } == 0);
        #[cfg(windows)]
        return false;
    }

    fn update_foreign(&mut self) {
        // A `lig ssh` killed before it could say so.
        #[cfg(unix)]
        if self.remote.as_ref().is_some_and(|r| unsafe { libc::kill(r.pid as i32, 0) } != 0) {
            self.end_remote();
        }
        // Windows has no tty to look for: any program the shell runs counts.
        let tty = self.tty.clone().or_else(|| cfg!(windows).then(PathBuf::new));
        // `ssh` itself uses the terminal; the remote shell reports what it's doing.
        self.foreign = self.editing
            && self.remote.is_none()
            && self.shell_pid.zip(tty.as_deref()).is_some_and(|(pid, tty)| crate::process::foreign_program(pid, tty));
        self.foreign_checked = Instant::now();
    }

    fn write_out(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let _ = term::write(bytes);
    }

    fn write_pty(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let _ = self.pty.write_all(bytes);
        let _ = self.pty.flush();
    }

    fn mark_dirty(&mut self) {
        if !self.dirty {
            self.dirty = true;
            self.dirty_since = Instant::now();
        }
    }

    fn begin_sync(&mut self, w: &mut Vec<u8>) {
        if !self.sync_open {
            // Synchronized output: the terminal shows erase+redraw atomically.
            w.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
            self.sync_open = true;
        }
    }

    fn end_sync(&mut self, w: &mut Vec<u8>) {
        if self.sync_open {
            w.extend_from_slice(b"\x1b[?2026l");
            self.sync_open = false;
        }
    }

    fn erase(&mut self, w: &mut Vec<u8>) {
        if let Some(r) = self.drawn.take() {
            self.begin_sync(w);
            popup::erase(w, self.parser.screen(), r);
            popup::restore_cursor(w, self.parser.screen());
        }
    }

    fn clear_popup(&mut self) {
        let mut w = vec![];
        self.erase(&mut w);
        self.end_sync(&mut w);
        self.write_out(&w);
    }

    /// A popup with suggestions is on screen (keys go to it).
    fn popup_open(&self) -> bool {
        self.visible() && self.shown.is_some()
    }

    fn fish_stale(&self) -> bool {
        self.fish_refresh.as_ref().is_some_and(|refresh| refresh.stale)
    }

    /// Automatic suggestions may be drawn, or the user explicitly asked for them.
    fn completion_requested(&self) -> bool {
        !self.cfg.behavior.toggle_only || self.manual
    }

    fn visible(&self) -> bool {
        !self.fish_stale() && self.items_line == self.line && self.drawable()
    }

    /// Nothing hides the suggestions, though they may be for an older line.
    fn drawable(&self) -> bool {
        self.can_show()
            && self.completion_requested()
            && !self.items.is_empty()
            && self.dismissed.as_deref() != Some(self.line.as_str())
    }

    // ------------------------------------------------------------ events

    fn on_output(&mut self, data: &[u8]) {
        let mut fwd = Vec::with_capacity(data.len());
        let mut msgs = vec![];
        self.osc.feed(data, &mut fwd, &mut msgs);
        if let Some(probe) = &mut self.startup_cursor {
            (fwd, msgs) = probe.output.gate(fwd, msgs);
        }
        self.on_filtered_output(fwd, msgs);
    }

    /// Child output after private shell messages have been removed and any
    /// startup cursor-query conflict has been deferred.
    fn on_filtered_output(&mut self, mut fwd: Vec<u8>, mut msgs: Vec<(usize, Msg)>) {
        // Another shell's own hooks speaking: nu was replaced (`exec bash`). Over `lig ssh` they
        // are the remote shell's, and nu is back when it ends.
        let local_nu = self.nu_shell && self.remote.is_none();
        let other_shell = |m: &Msg| matches!(m, Msg::PromptEnd | Msg::Buffer { .. } | Msg::Ready { .. });
        if local_nu && msgs.iter().any(|(_, m)| other_shell(m)) {
            crate::log("shell is no longer nu");
            self.nu_shell = false;
            self.nu_tail.clear();
            self.env.shell_specs = None;
        }
        if self.nu_shell && self.remote.is_none() {
            let tail = std::mem::take(&mut self.nu_tail);
            let joined = [tail.as_slice(), &fwd].concat();
            msgs.extend(nu_marks(&joined).into_iter().map(|(i, m)| (i.saturating_sub(tail.len()), m)));
            msgs.sort_by_key(|(pos, _)| *pos);
            self.nu_tail = unfinished_nu_mark(&joined).to_vec();
        }
        let pager = self.parser.screen().alternate_screen();
        // Count only queries about to go out, including those released from
        // the startup buffer, so their CPRs can bypass key handling.
        let shell_queries = self.cursor_query_counter.observe(&fwd);
        self.shell_cursor_queries += shell_queries;
        if shell_queries > 0 {
            self.shell_cursor_asked = Some(Instant::now());
        }
        // ConPTY asks its terminal for win32-input-mode; passed on, the terminal would send lig
        // keys it can't read. lig types plain VT, which ConPTY reads too.
        if cfg!(windows) {
            for mode in [b"\x1b[?9001h", b"\x1b[?9001l"] {
                while let Some(i) = fwd.windows(mode.len()).position(|w| w == mode) {
                    fwd.drain(i..i + mode.len());
                    // Messages after it move back with the bytes.
                    for (pos, _) in msgs.iter_mut().filter(|(pos, _)| *pos > i) {
                        *pos = pos.saturating_sub(mode.len()).max(i);
                    }
                }
            }
        }
        let mut w = vec![];
        // ConPTY wraps PowerShell's replies in cursor and mode sequences: only a changed
        // screen means a changed line there (else each reply would ask again). On a host the
        // line only changes by keys lig passes on (nu redraws it after each reply): output
        // doesn't count.
        let host = self.remote.is_some();
        let before = (self.report_key && !host).then(|| self.screen_state());
        if !only_modes(&fwd) {
            if let Some(refresh) = self.fish_refresh.as_mut().filter(|_| before.is_none() && !host) {
                refresh.changed();
            }
            // Erase the popup first so the screen matches what the shell and vt100 expect.
            self.erase(&mut w);
        }
        // Each message sees the screen as of its position (bash's prompt marker reads the cursor).
        let mut marks = prompt_marks(&fwd).into_iter().peekable();
        let mut msgs = msgs.into_iter().peekable();
        let mut at = 0;
        loop {
            let next_msg = msgs.peek().map(|&(pos, _)| pos);
            let Some(pos) = next_msg.into_iter().chain(marks.peek().copied()).min() else { break };
            w.extend_from_slice(&fwd[at..pos]);
            self.process_child_screen(&fwd[at..pos]);
            at = pos;
            if next_msg == Some(pos) {
                let (_, m) = msgs.next().unwrap();
                self.write_out(&std::mem::take(&mut w));
                self.on_msg(m);
            } else {
                marks.next();
                // The terminal starts a new line at a prompt mark; do it here so vt100 does too.
                if self.parser.screen().cursor_position().1 != 0 {
                    w.extend_from_slice(b"\r\n");
                    self.process_child_screen(b"\r\n");
                }
            }
        }
        w.extend_from_slice(&fwd[at..]);
        self.write_out(&w);
        if shell_queries > 0
            && let Some(r) = &mut self.resync
        {
            r.shell_reply_until = Some(Instant::now() + CURSOR_REPORT_WAIT);
        }
        self.process_child_screen(&fwd[at..]);
        if before.is_some_and(|b| b != self.screen_state())
            && let Some(refresh) = &mut self.fish_refresh
        {
            refresh.changed();
        }
        if self.editing && self.prompt_end.is_some() {
            self.read_line_from_screen();
        }
        // Back from a pager that saw a resize: check the normal screen now.
        if pager && !self.parser.screen().alternate_screen() && std::mem::take(&mut self.normal_unsynced) {
            self.resync = Some(Resync {
                size: None,
                ask_at: Instant::now(),
                asked: None,
                stale: false,
                shell_reply_until: None,
                held: vec![],
            });
        }
        self.update_foreign();
        self.last_output = Instant::now();
        self.mark_dirty();
        if self.pending_enter.is_some()
            && !self.fish_stale()
            && self.enter_ready()
            && (self.hidden || (self.loading.is_none() && self.items_line == self.line))
        {
            self.render();
            self.resolve_pending_enter();
        }
    }

    fn screen_state(&self) -> (String, (u16, u16)) {
        let screen = self.parser.screen();
        (screen.contents(), screen.cursor_position())
    }

    fn on_buffer(&mut self, cursor: usize, text: &str) {
        crate::log(&format!("buffer cursor={cursor} characters={}", text.chars().count()));
        self.editing = true;
        self.hidden = false;
        self.set_line(text.chars().take(cursor).collect());
    }

    /// The shell may have been replaced (`exec fish`); each kind reports its line and
    /// takes completions its own way.
    fn switch_shell(&mut self, kind: ShellKind) {
        let fish = kind == ShellKind::Fish;
        if fish != self.fish_shell || (kind == ShellKind::Zsh && self.prompt_end.is_some()) {
            crate::log(&format!("shell is now {kind:?}"));
        }
        self.fish_shell = fish;
        self.zsh_shell = kind == ShellKind::Zsh;
        if !fish {
            self.fish_refresh = None;
        }
        if kind != ShellKind::Bash {
            // Only bash's line is read off the screen.
            self.prompt_end = None;
        }
        // A remote shell can't read our files: the payload follows the key.
        self.insertion_transport = if kind == ShellKind::Zsh || self.remote.is_some() {
            InsertionTransport::Inline
        } else {
            InsertionTransport::File
        };
    }

    /// `lig ssh` started: until it ends, the remote shell drives the popup.
    fn start_remote(&mut self, pid: u32, control: String, dest: String, mosh: bool) {
        if self.remote.is_some() || !is_lig_ssh(pid, &control) {
            crate::log(&format!("ignored remote start from pid {pid}"));
            return;
        }
        crate::log(&format!("remote {dest}"));
        let remote = Some(std::sync::Arc::new(Remote { control: control.into(), dest }));
        let local_env = std::mem::replace(&mut self.env, ShellEnv { remote, ..Default::default() });
        self.remote = Some(RemoteSession {
            pid,
            local_env,
            fish_shell: self.fish_shell,
            zsh_shell: self.zsh_shell,
            insertion_transport: self.insertion_transport,
            history: Default::default(),
            mosh,
            insert_file: None,
            nu_specs_asked: false,
        });
        self.fish_shell = false;
        self.zsh_shell = false;
        self.fish_refresh = None;
        self.prompt_end = None;
        self.insertion_transport = InsertionTransport::Inline;
        self.reset_line();
    }

    fn end_remote(&mut self) {
        let Some(r) = self.remote.take() else { return };
        crate::log("remote ended");
        self.env = r.local_env;
        self.fish_shell = r.fish_shell;
        self.zsh_shell = r.zsh_shell;
        self.insertion_transport = r.insertion_transport;
        self.prompt_end = None;
        self.reset_line();
    }

    fn reset_line(&mut self) {
        self.editing = false;
        self.foreign = false;
        self.unconfirmed.clear();
        self.items.clear();
        self.items_line.clear();
        self.line.clear();
        self.dismissed = None;
    }

    fn set_line(&mut self, line: String) {
        // The insertion an accept-and-run waits for is on the line: run it.
        if self.run_after_insert.as_deref() == Some(line.as_str()) {
            self.run_after_insert = None;
            self.write_pty(b"\r");
        }
        if line != self.line {
            // The typed text the new line shows is confirmed; any other change starts over.
            self.unconfirmed = match line.strip_prefix(self.line.as_str()) {
                Some(added) if self.unconfirmed.starts_with(added) => self.unconfirmed[added.len()..].to_string(),
                _ => String::new(),
            };
            // The popup closes after a finished word, or for the shell's own history: like Esc,
            // that ends a toggled popup (with `toggle_only`, typing doesn't bring it back).
            if std::mem::take(&mut self.close_after_insert) || self.shell_nav {
                self.dismissed = Some(line.clone());
                self.manual = false;
            }
            // A cleared line (Ctrl+C) ends a toggled popup as running it does.
            if line.is_empty() {
                self.manual = false;
            }
        }
        if line != self.line || self.items_line != line {
            if self.dismissed.as_deref() != Some(line.as_str()) {
                self.dismissed = None;
            }
            self.line = line;
            self.request();
        }
    }

    /// bash: the input is the screen text between the prompt marker and the cursor.
    fn read_line_from_screen(&mut self) {
        let screen = self.parser.screen();
        let (rows, cols) = screen.size();
        let Some(pe) = self.prompt_end.as_mut() else { return };
        // Long lines at the bottom scroll the screen: find the prompt again.
        if screen.contents_between(pe.row, 0, pe.row, pe.col) != pe.prefix {
            let Some(r) =
                (0..=pe.row.min(rows - 1)).rev().find(|&r| screen.contents_between(r, 0, r, pe.col) == pe.prefix)
            else {
                return;
            };
            pe.row = r;
        }
        let (cr, cc) = screen.cursor_position();
        if (cr, cc) < (pe.row, pe.col) {
            return;
        }
        let row_text = screen.contents_between(cr, 0, cr, cols);
        // Readline's incremental search and numeric arguments replace the prompt.
        if ["(reverse-i-search)", "(i-search)", "(arg: "].iter().any(|m| row_text.contains(m)) {
            self.hidden = true;
            return;
        }
        self.hidden = false;
        let line = screen.contents_between(pe.row, pe.col, cr, cc);
        self.set_line(line);
    }

    fn on_msg(&mut self, m: Msg) {
        match m {
            Msg::InsertRejected => {
                self.run_after_insert = None;
                self.just_accepted = false;
                self.close_after_insert = false;
                self.dismissed = None;
                self.items_line.clear();
                self.request();
            }
            Msg::Ready { pid, insert_file, nu } => {
                crate::log(&format!("reader ready fish={} pid={:?} from={pid:?}", self.fish_shell, self.shell_pid));
                // Fish that replaced the shell lig started (`exec fish`): same process.
                if !self.fish_shell && pid.is_some() && pid == self.shell_pid {
                    self.switch_shell(ShellKind::Fish);
                }
                // A host's shell that says it's ready answers when asked: fish, and bash over
                // `lig mosh` (mosh drops what would mark its line on the screen).
                let host = self.remote.is_some();
                if let Some(r) = &mut self.remote {
                    self.fish_shell = true;
                    r.insert_file = insert_file;
                    // nu's own commands, from the host's nu (they don't change during a session).
                    if nu && !std::mem::replace(&mut r.nu_specs_asked, true) {
                        self.fetch_host_nu_specs();
                    }
                }
                if self.fish_shell {
                    self.editing = true;
                    let mut refresh =
                        FishRefresh { timeout: host.then_some(HOST_SNAPSHOT_TIMEOUT), ..Default::default() };
                    refresh.changed();
                    // nu says it's ready before its line editor reads keys: a request sent at
                    // once would be echoed and lost.
                    if host {
                        refresh.due = Some(Instant::now() + HOST_FIRST_ASK);
                    }
                    self.fish_refresh = Some(refresh);
                }
            }
            Msg::Snapshot { id, buffer } => {
                // 0: a host's fish, which can't say which request it answers (see above).
                let in_flight = self.fish_refresh.as_ref().and_then(|r| r.in_flight.as_ref()).map(|p| p.id);
                let id = if id == 0 && self.remote.is_some() { in_flight.unwrap_or(0) } else { id };
                if self.fish_refresh.as_mut().is_some_and(|refresh| refresh.reported(id)) {
                    match buffer {
                        Some((cursor, text)) => self.on_buffer(cursor, &text),
                        None => self.hidden = true,
                    }
                }
            }
            Msg::Buffer { cursor, text } => {
                // Only zsh sends its buffer unasked (fish's comes as snapshots).
                self.switch_shell(ShellKind::Zsh);
                self.on_buffer(cursor, &text);
            }
            Msg::PromptEnd if self.cmd_shell => {
                // The prompt itself is still to come (see `PROMPT_SETTLE`).
                self.prompt_pending = true;
                self.prompt_end = None;
                self.editing = false;
            }
            Msg::PromptEnd => {
                self.switch_shell(ShellKind::Bash);
                self.record_startup_prompt();
                let screen = self.parser.screen();
                let (row, col) = screen.cursor_position();
                let prefix = screen.contents_between(row, 0, row, col);
                self.prompt_end = Some(PromptEnd { row, col, prefix });
                self.editing = true;
                self.hidden = false;
            }
            Msg::Hide => {
                crate::log("reader hidden");
                self.hidden = true;
            }
            Msg::ShellCompletions { line, items } => {
                match self.shell_unanswered.iter().position(|asked| *asked == line) {
                    // Answers come in order: any asked before it are lost.
                    Some(i) => drop(self.shell_unanswered.drain(..=i)),
                    None => self.ask_shell_again(),
                }
                if self.shell_unanswered.is_empty() {
                    self.shell_asked_at = None;
                }
                self.env.shell_answer = Some(std::sync::Arc::new((line, items)));
                self.items_line.clear();
                self.request();
            }
            Msg::Ran { status, command } => {
                // A leading space keeps a command out, as the shells' `ignorespace` options do.
                if (status == 0 || !self.cfg.behavior.history_success_only)
                    && !command.starts_with(' ')
                    && let Some(history) = self.history.as_mut()
                {
                    // Where it was typed: each shell says it ran before it says where it now is.
                    let dir = Some(self.env.cwd.to_string_lossy().into_owned()).filter(|d| !d.is_empty());
                    // A `lig ssh` host's commands are its own.
                    match &mut self.remote {
                        Some(r) => r.history.record(&command, dir.as_deref()),
                        None => history.record(&command, dir.as_deref()),
                    }
                }
            }
            Msg::Finish => {
                crate::log("reader finished");
                self.history_mode = false;
                self.fish_refresh = None;
                self.unconfirmed.clear();
                self.editing = false;
                self.just_accepted = false;
                // A pick that ran its line right away (`enter_runs_commands`) never saw its line:
                // the next line, at the next prompt, isn't the one to close the popup for.
                self.close_after_insert = false;
                self.manual = false;
                self.loading = None;
                if self.pending_enter.take().is_some() {
                    self.enter_after = None;
                    let data = std::mem::take(&mut self.deferred);
                    self.write_pty(&data);
                }
                self.items.clear();
                self.items_line.clear();
                self.line.clear();
                self.dismissed = None;
            }
            Msg::Path(p) => self.env.path = Some(p),
            Msg::Cwd(c) => {
                // Follow the shell's cwd so hosts reading ours from `/proc` inherit it.
                if self.remote.is_none() {
                    let _ = std::env::set_current_dir(&c);
                }
                self.env.cwd = PathBuf::from(c);
            }
            Msg::Aliases(a) => self.env.aliases = std::sync::Arc::new(a.into_iter().collect()),
            Msg::Functions(f) => self.env.functions = std::sync::Arc::new(f),
            Msg::Builtins(b) => self.env.builtins = std::sync::Arc::new(b),
            Msg::Vars(v) => self.env.vars = std::sync::Arc::new(v),
            // A `lig ssh` host's file isn't on this machine.
            Msg::HistoryFile { zsh, path } => {
                if self.remote.is_none()
                    && let Some(history) = self.history.as_mut()
                {
                    let shell =
                        if zsh { crate::history::ShellHistory::Zsh } else { crate::history::ShellHistory::Bash };
                    history.report_file(PathBuf::from(path), shell);
                }
            }
            Msg::Remote { pid, control, dest, mosh } => self.start_remote(pid, control, dest, mosh),
            Msg::RemoteEnd => self.end_remote(),
        }
        self.mark_dirty();
    }

    /// The "Searching npm…" message, once loading has taken a moment.
    fn loading_message(&self) -> Option<&str> {
        let (line, label, since) = self.loading.as_ref()?;
        let show = *line == self.line
            && since.elapsed() >= LOADING_DELAY
            && self.completion_requested()
            && self.editing
            && !self.hidden
            && !self.visible()
            && self.dismissed.as_deref() != Some(self.line.as_str())
            && !self.full_screen_app()
            && !self.foreign;
        show.then_some(label.as_str())
    }

    fn loading_wake(&self) -> Option<Instant> {
        let (line, _, since) = self.loading.as_ref()?;
        let at = *since + LOADING_DELAY;
        (*line == self.line && Instant::now() < at).then_some(at)
    }

    /// When to wake up to take down suggestions kept for an older line.
    fn keep_wake(&self) -> Option<Instant> {
        // `render` waits for a resync: waking for it meanwhile would spin.
        self.shown.and(self.stale_since).filter(|_| self.resync.is_none()).map(|since| since + LOADING_DELAY)
    }

    fn request(&mut self) {
        if self.history_mode {
            self.show_history();
            return;
        }
        // With `toggle_only`, nothing runs (generators, registry searches) until the toggle.
        if !self.completion_requested() {
            self.loading = None;
            return;
        }
        let mut env = self.env.clone();
        if env.cwd.as_os_str().is_empty() {
            env.cwd = self.shell_pid.and_then(crate::process::cwd).unwrap_or_else(|| self.cwd_fallback.clone());
        }
        let _ = self.req_tx.send(Request { line: self.line.clone(), env, manual: self.manual });
        // Loading until answered (the worker may be busy with an older request);
        // the first word only pops up on demand (or with `complete_commands`), so it gets no message.
        let first_word = !self.line.trim_start().contains(char::is_whitespace);
        if !first_word || self.manual || self.cfg.behavior.complete_commands {
            if !self.loading.as_ref().is_some_and(|(l, _, _)| *l == self.line) {
                self.loading = Some((self.line.clone(), "Loading suggestions".into(), Instant::now()));
            }
        } else {
            self.loading = None;
        }
    }

    /// Nothing typed for the current word: the line ends in a space.
    fn word_empty(&self) -> bool {
        self.line.is_empty() || self.line.ends_with(char::is_whitespace)
    }

    /// The word being typed already equals a suggestion: accepting it would change nothing.
    fn word_complete(&self) -> bool {
        let chars: Vec<char> = self.line.chars().collect();
        self.items.iter().any(|s| {
            let typed: String = chars[chars.len().saturating_sub(s.delete)..].iter().collect();
            !typed.is_empty() && s.insert.trim_end() == typed
        })
    }

    fn enter_action(&self) -> Enter {
        if self.history_mode {
            return Enter::Pick;
        }
        let runs = self.cfg.behavior.enter_runs_commands.contains(&self.items_command);
        enter_action(
            &self.cfg.behavior.enter_accepts,
            runs,
            self.word_empty(),
            self.word_complete(),
            self.navigated,
            self.just_accepted,
        )
    }

    /// Replay a deferred Enter and the keys after it, once suggestions are final or loading timed out.
    fn resolve_pending_enter(&mut self) {
        if self.pending_enter.take().is_none() {
            return;
        }
        self.enter_after = None;
        self.loading = None;
        let data = std::mem::take(&mut self.deferred);
        if self.fish_stale() || (self.remote.is_some() && !self.unconfirmed().is_empty()) {
            // The shell didn't report the line in time: send Enter as is rather than accept for
            // an old line.
            self.write_pty(&data);
        } else {
            self.on_input(&data);
        }
    }

    fn on_done(&mut self, line: String, items: Vec<Suggestion>, loading: Option<String>, command: String) {
        if line != self.line {
            return; // stale
        }
        let fin = loading.is_none();
        self.apply_done(line, items, loading, command);
        if fin && self.pending_enter.is_some() && !self.fish_stale() && self.enter_ready() {
            self.render();
            self.resolve_pending_enter();
        }
    }

    fn apply_done(&mut self, line: String, mut items: Vec<Suggestion>, loading: Option<String>, command: String) {
        // Shown as text, whatever a generator or the history holds; what's inserted is untouched.
        for s in &mut items {
            if s.label.chars().any(char::is_control) {
                s.label = popup::printable(&s.label);
            }
            if s.description.chars().any(char::is_control) {
                s.description = popup::printable(&s.description);
            }
        }
        self.items_command = command;
        self.loading = match (loading, self.loading.take()) {
            // Still loading the same line: keep the original start time.
            (Some(label), Some((l, _, since))) if l == line => Some((line.clone(), label, since)),
            (Some(label), _) => Some((line.clone(), label, Instant::now())),
            (None, _) => None,
        };
        let same_line = self.items_line == line;
        let keep = self.items.get(self.selected).map(|s| s.label.clone());
        self.items = items;
        self.items_line = line;
        // Slower results for the same line keep the selection only if the user moved it.
        self.selected = if same_line && self.navigated {
            keep.and_then(|l| self.items.iter().position(|s| s.label == l)).unwrap_or(0)
        } else {
            0
        };
        if !same_line {
            self.navigated = false;
        }
        if self.selected == 0 {
            self.scroll = 0;
        }
        self.mark_dirty();
    }

    fn on_input(&mut self, data: &[u8]) {
        let plain = keys::plain_keypad(data);
        let data = plain.as_deref().unwrap_or(data);
        // Keys typed while a deferred Enter waits go after it, in order.
        if self.pending_enter.is_some() {
            self.deferred.extend_from_slice(data);
            return;
        }
        // A key typed before the shell showed an accept-and-run's insertion: it no longer runs.
        self.run_after_insert = None;
        // Until cmd's prompt is drawn, typed text would be taken for part of it.
        if self.prompt_pending {
            self.held.extend_from_slice(data);
            return;
        }
        let mut fwd: Vec<u8> = vec![];
        let mut redraw = false;
        let keys = keys::split(data);
        // Once a key has gone to the shell, popup state is stale until it redraws:
        // the rest of the chunk goes through as is (pastes, fast macros).
        let mut passthrough = false;
        // What this chunk's keys type, if they're all plain characters.
        let mut typed: Option<String> = Some(String::new());
        for (n, (raw, key)) in keys.iter().enumerate() {
            let (raw, key) = (*raw, key.clone());
            if passthrough {
                // Enter typed fast after other keys: the line isn't updated yet, so hold
                // Enter until the shell reports it and its suggestions are ready.
                if key == Key::ENTER && self.enter_can_wait() {
                    let expected = typed.take().map(|t| format!("{}{}{t}", self.line, self.unconfirmed()));
                    self.enter_after = Some((self.line.clone(), expected));
                    self.pending_enter = Some(Instant::now());
                    let rest: usize = keys[n..].iter().map(|(r, _)| r.len()).sum();
                    self.deferred = data[data.len() - rest..].to_vec();
                    break;
                }
                push_typed(&mut typed, &key);
                fwd.extend_from_slice(raw);
                continue;
            }
            if (key == Key::ENTER && self.enter_should_wait()) || self.popup_key_should_wait(&key) {
                if !self.unconfirmed().is_empty() {
                    let expected = format!("{}{}", self.line, self.unconfirmed());
                    self.enter_after = Some((self.line.clone(), Some(expected)));
                }
                self.pending_enter = Some(Instant::now());
                let rest: usize = keys[n..].iter().map(|(r, _)| r.len()).sum();
                self.deferred = data[data.len() - rest..].to_vec();
                break;
            }
            let history_key = matches!(self.bindings.action(&key), Some(Action::History | Action::FolderHistory));
            if history_key && self.history.is_some() && self.can_show() {
                self.toggle_history(self.bindings.action(&key) == Some(Action::FolderHistory));
                redraw = true;
                continue;
            }
            if self.bindings.action(&key) == Some(Action::Toggle) && self.can_show() {
                self.toggle();
                redraw = true;
                continue;
            }
            if self.repeated_dismiss(&key) {
                continue;
            }
            // Ctrl+C abandons the line in every shell: no popup for it, even when the shell
            // doesn't report the next one (a zle-line-init that replaced lig's, #49).
            if key == Key::ctrl('c') && self.bindings.action(&key).is_none() {
                self.dismiss();
                redraw = true;
            }
            if !self.popup_open() {
                if key != Key::ENTER {
                    self.just_accepted = false;
                }
                self.shell_nav =
                    [Key::UP, Key::DOWN, Key::ctrl('p'), Key::ctrl('n'), Key::TAB, Key::SHIFT_TAB].contains(&key);
                push_typed(&mut typed, &key);
                fwd.extend_from_slice(raw);
                passthrough = true;
                continue;
            }
            let mut action = self.bindings.action(&key);
            // Esc Esc in one read arrives as Alt+Esc.
            let unmodified = Key { alt: false, ..key.clone() };
            if action.is_none() && key.alt && self.bindings.action(&unmodified) == Some(Action::Dismiss) {
                action = Some(Action::Dismiss);
            }
            if action == Some(Action::Next) && self.items.len() == 1 {
                action = Some(Action::Accept);
            }
            let mut run = false;
            if action == Some(Action::Accept) && key == Key::ENTER {
                match self.enter_action() {
                    Enter::Close => {
                        self.dismiss();
                        redraw = true;
                        continue;
                    }
                    Enter::Run => action = None,
                    Enter::Pick => {}
                    Enter::PickAndRun => run = true,
                }
            }
            match action {
                Some(Action::Next) => {
                    self.navigated = true;
                    self.selected = (self.selected + 1) % self.items.len();
                    redraw = true;
                }
                Some(Action::Prev) if self.selected == 0 && !self.cfg.behavior.wrap_around => {
                    // Up from the first item closes the popup; the next Up goes to shell history.
                    self.dismiss();
                    redraw = true;
                }
                Some(Action::Prev) => {
                    self.navigated = true;
                    self.selected = (self.selected + self.items.len() - 1) % self.items.len();
                    redraw = true;
                }
                Some(Action::History | Action::FolderHistory) => {
                    // No history (`behavior.history` off): the key is the shell's.
                    passthrough = true;
                    fwd.extend_from_slice(raw);
                }
                Some(Action::Dismiss | Action::Toggle) => {
                    self.dismiss();
                    if action == Some(Action::Dismiss) && !key.alt {
                        self.dismissed_by = Some((key.clone(), Instant::now()));
                    }
                    redraw = true;
                }
                Some(Action::Accept | Action::AcceptAndRun) => {
                    self.just_accepted = true;
                    passthrough = true;
                    // The insertion changes the line in ways not worth predicting.
                    typed = None;
                    self.accept_selected(&mut fwd, run || action == Some(Action::AcceptAndRun));
                    redraw = true;
                }
                None => {
                    if key == Key::ENTER {
                        self.dismissed = Some(self.line.clone());
                        redraw = true;
                    } else {
                        self.just_accepted = false;
                    }
                    passthrough = true;
                    push_typed(&mut typed, &key);
                    fwd.extend_from_slice(raw);
                }
            }
        }
        // Only fish's reports can overtake keys (zsh's and bash's come in the output, PowerShell's
        // answer a key), and any shell's over `lig ssh`, a round trip behind; after other keys
        // the line can't be predicted.
        let reports_lag = (self.fish_refresh.is_some() && !self.report_key) || self.remote.is_some();
        if !fwd.is_empty() && reports_lag && self.pending_enter.is_none() {
            match &typed {
                Some(t) if !t.is_empty() => {
                    self.unconfirmed.push_str(t);
                    self.unconfirmed_at = Instant::now();
                }
                Some(_) => {}
                None => self.unconfirmed.clear(),
            }
        }
        if !fwd.is_empty()
            && let Some(refresh) = &mut self.fish_refresh
        {
            refresh.changed();
            redraw = true;
        }
        if redraw {
            self.render();
        }
        self.write_pty(&fwd);
        // cmd says nothing when a line runs; Enter (or a pasted newline) runs it.
        if self.cmd_shell && self.editing && fwd.contains(&b'\r') {
            self.on_msg(Msg::Finish);
        }
    }

    /// cmd's output paused after its prompt marker: the input starts at the cursor.
    fn prompt_settled(&mut self) {
        self.prompt_pending = false;
        self.record_startup_prompt();
        let screen = self.parser.screen();
        let (row, col) = screen.cursor_position();
        let prefix = screen.contents_between(row, 0, row, col);
        crate::log(&format!("cmd prompt at {row},{col}"));
        self.prompt_end = Some(PromptEnd { row, col, prefix });
        self.editing = true;
        self.hidden = false;
        self.line.clear();
        self.request();
        let held = std::mem::take(&mut self.held);
        if !held.is_empty() {
            self.on_input(&held);
        }
    }

    /// The popup may be shown now (the shell is editing a line, nothing hides it).
    fn can_show(&self) -> bool {
        self.startup_cursor.is_none() && self.editing && !self.hidden && !self.full_screen_app() && !self.foreign
    }

    /// A full-screen app (vim, less…) has the screen: the alternate one, except mosh's client's.
    fn full_screen_app(&self) -> bool {
        self.parser.screen().alternate_screen() && !self.remote.as_ref().is_some_and(|r| r.mosh)
    }

    /// Enter may be held back to act on up-to-date suggestions.
    fn enter_can_wait(&self) -> bool {
        self.can_show()
            && self.completion_requested()
            && self.pending_enter.is_none()
            && self.bindings.action(&Key::ENTER) == Some(Action::Accept)
            && self.cfg.behavior.enter_accepts != "never"
    }

    /// A held Enter may go on: the shell reported the line typed before it, or hides the popup.
    fn enter_ready(&self) -> bool {
        match &self.enter_after {
            _ if self.hidden => true,
            None => true,
            Some((_, Some(expected))) => self.line == *expected,
            Some((before, None)) => self.line != *before,
        }
    }

    /// How long Enter waits for the line's suggestions: briefly on a `lig ssh` host, where
    /// they come over the network, so a slow link doesn't hold the keys typed after it.
    fn enter_wait(&self) -> Duration {
        if self.remote.is_some() { REMOTE_ENTER_WAIT } else { ENTER_WAIT }
    }

    /// Enter waits briefly for this line's loading suggestions, to act on the final list.
    fn enter_should_wait(&self) -> bool {
        self.pending_enter.is_none()
            && self.completion_requested()
            && self.bindings.action(&Key::ENTER) == Some(Action::Accept)
            && self.cfg.behavior.enter_accepts != "never"
            && !self.just_accepted
            && self.editing
            && !self.hidden
            && (self.fish_stale()
                || !self.unconfirmed().is_empty()
                || (self.dismissed.as_deref() != Some(self.line.as_str())
                    && self.loading.as_ref().is_some_and(|(l, _, _)| *l == self.line)))
    }

    /// A popup key (Tab, arrows, accept) while the popup on screen is kept from an older line
    /// (fish hasn't reported the new one yet): passed on, the shell would take it. It waits
    /// like Enter does.
    fn popup_key_should_wait(&self, key: &Key) -> bool {
        self.pending_enter.is_none()
            && self.shown.is_some()
            && self.drawable()
            && (self.fish_stale() || !self.unconfirmed().is_empty())
            && matches!(
                self.bindings.action(key),
                Some(Action::Next | Action::Prev | Action::Accept | Action::AcceptAndRun)
            )
    }

    /// Text typed to fish that no report has shown yet, if typed recently.
    fn unconfirmed(&self) -> &str {
        if self.unconfirmed_at.elapsed() < ENTER_WAIT { &self.unconfirmed } else { "" }
    }

    /// Close the popup, or ask for suggestions even where it doesn't open by itself (command names).
    fn toggle(&mut self) {
        if self.popup_open() {
            self.dismiss();
        } else {
            self.manual = true;
            self.shell_nav = false;
            self.dismissed = None;
            self.navigated = false;
            self.items_line.clear();
            self.request();
        }
    }

    /// Ask the shell what its Tab would offer for `line`, once per line, and only while zsh or
    /// bash is the shell at the prompt: the key means nothing to another one (`exec fish`).
    fn ask_shell(&mut self, line: String) {
        let local = self.remote.is_none() && !self.fish_shell && !self.nu_shell && !self.cmd_shell;
        // zsh reports its buffer (inline insertions); bash's line is read after its prompt marker.
        let zsh = self.insertion_transport == InsertionTransport::Inline;
        let bash = self.insertion_transport == InsertionTransport::File && self.prompt_end.is_some();
        if !local
            || !(zsh || bash)
            || !self.editing
            || line != self.line
            || self.shell_asked.as_deref() == Some(line.as_str())
        {
            return;
        }
        self.shell_unanswered.push(line.clone());
        if self.shell_unanswered.len() > 8 {
            self.shell_unanswered.remove(0);
        }
        self.shell_asked = Some(line);
        self.shell_asked_at = Some(Instant::now());
        self.write_pty(ZSH_COMPLETE_KEY);
    }

    /// When to ask the shell again if its answer hasn't come.
    fn shell_answer_due(&self) -> Option<Instant> {
        self.shell_asked_at.filter(|_| !self.shell_unanswered.is_empty()).map(|at| at + SHELL_ANSWER_WAIT)
    }

    /// The shell's answer is late, or it answered for a line lig didn't ask about: ask once
    /// more for the line on screen, so a lost answer doesn't leave the popup on "Asking zsh".
    fn ask_shell_again(&mut self) {
        let Some(line) = self.shell_asked.clone() else { return };
        if line != self.line || self.shell_asked_again.as_deref() == Some(line.as_str()) {
            return;
        }
        crate::log("asking the shell again");
        self.shell_asked_again = Some(line.clone());
        self.shell_asked = None;
        self.ask_shell(line.clone());
        // Not asked (no longer editing): still the line asked about, not one to ask about anew.
        self.shell_asked.get_or_insert(line);
    }

    /// Show the commands that ran before, or leave them for the usual suggestions.
    /// Open the history (all of it, or this folder's), switch between the two, or close it.
    fn toggle_history(&mut self, folder: bool) {
        self.history_mode = !(self.history_mode && self.history_folder == folder);
        self.history_folder = folder;
        self.manual = self.history_mode;
        self.shell_nav = false;
        self.dismissed = None;
        self.navigated = false;
        self.items_line.clear();
        self.request();
    }

    /// The host's nu's own commands, read in the background.
    fn fetch_host_nu_specs(&self) {
        let Some(remote) = self.env.remote.clone() else { return };
        let events = self.events.clone();
        std::thread::spawn(move || {
            if let Some(specs) = crate::nushell::host_specs(&remote) {
                let _ = events.send(Event::HostShellSpecs(specs));
            }
        });
    }

    /// Fish on a host reads an insertion from its file: written there over the ssh connection, in
    /// the background, before the key that applies it. Fish checks the line is still the one lig saw.
    fn host_insert(&self, file: String, payload: String, run: bool) {
        let Some(remote) = self.env.remote.clone() else { return };
        let events = self.events.clone();
        std::thread::spawn(move || {
            let write = format!("printf %s {} > {}", crate::remote::quote(&payload), crate::remote::quote(&file));
            let cmd = crate::exec::Cmd::Shell(&write);
            let out = crate::exec::run(cmd, Path::new(""), &[], Duration::from_secs(5), Some(&remote));
            let _ = events.send(Event::HostInsert { ok: out.status == 0, run });
        });
    }

    /// Reads the `lig ssh` host's history files, in the background.
    fn fetch_host_history(&self) {
        let Some(remote) = self.env.remote.clone() else { return };
        let events = self.events.clone();
        std::thread::spawn(move || {
            let cmd = crate::exec::Cmd::Shell(crate::history::HOST_FILES);
            let out = crate::exec::run(cmd, Path::new(""), &[], Duration::from_secs(10), Some(&remote));
            let mut files = out.stdout.lines().map(crate::remote::base64_decode);
            let (zsh, bash) = (files.next().unwrap_or_default(), files.next().unwrap_or_default());
            let _ = events.send(Event::HostHistory { ok: out.status == 0, zsh, bash });
        });
    }

    /// The history matching the line, newest first; picking one replaces the line.
    fn show_history(&mut self) {
        let fuzzy = self.cfg.behavior.fuzzy;
        // `keys.folder_history`: only what ran in the shell's folder.
        let dir = self.history_folder.then(|| self.env.cwd.to_string_lossy().into_owned());
        let Some(history) = self.history.as_mut() else { return };
        // On a `lig ssh` host, its history: what ran there now, then its shells' files.
        let found = match &mut self.remote {
            Some(r) => {
                let fetch = !std::mem::replace(&mut r.history.asked, true);
                let found = r.history.search(&self.line, fuzzy, dir.as_deref());
                if fetch {
                    self.fetch_host_history();
                }
                found
            }
            None => history.search(&self.line, fuzzy, dir.as_deref()),
        };
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
        let delete = self.line.chars().count();
        let local = self.history_folder;
        let items = found
            .into_iter()
            .take(500)
            .map(|(time, command)| Suggestion {
                label: command.replace('\n', " ⏎ "),
                description: {
                    let when = if time == 0 { String::new() } else { ago(now.saturating_sub(time)) };
                    // Alt+R's list says it's this folder's.
                    match (local, when.is_empty()) {
                        (false, _) => when,
                        (true, true) => "local".into(),
                        (true, false) => format!("local · {when}"),
                    }
                },
                kind: Kind::Command,
                icon: Some(crate::spec::Icon::History),
                insert: command,
                delete,
                back: 0,
                query: self.line.trim_start().to_string(),
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            // Nothing matches: back to the usual suggestions.
            self.history_mode = false;
            self.manual = false;
            self.request();
            return;
        }
        self.loading = None;
        // As the worker's answers do: an Enter held for this line can go now.
        self.on_done(self.line.clone(), items, None, String::new());
    }

    /// Esc again right after closing the popup; sent on, zsh would read the next key as Alt+key.
    fn repeated_dismiss(&mut self, key: &Key) -> bool {
        match self.dismissed_by.take() {
            Some((k, at)) => k == *key && at.elapsed() < DOUBLE_TAP && !self.popup_open(),
            None => false,
        }
    }

    /// Close the popup for this line; it comes back when the line changes.
    fn dismiss(&mut self) {
        self.history_mode = false;
        self.dismissed = Some(self.line.clone());
        self.manual = false;
    }

    /// Insert the selected suggestion; with `run`, run the line after it (`enter_runs_commands`).
    fn accept_selected(&mut self, fwd: &mut Vec<u8>, run: bool) {
        self.history_mode = false;
        let s = &self.items[self.selected];
        if s.kind == Kind::Execute {
            // The "Enter" item: run the line.
            fwd.push(b'\r');
        } else {
            // Close after a completed word; stay open if the resulting line is empty or ends
            // in a space or `/` (`--option `, `src/`), so the next suggestions show right away.
            let kept: String = {
                let n = self.line.chars().count();
                self.line.chars().take(n.saturating_sub(s.delete)).collect()
            };
            let result = format!("{kept}{}", s.insert);
            self.close_after_insert =
                !(result.is_empty() || result.ends_with(|c: char| c.is_whitespace() || crate::engine::is_sep(c)));
            // Fish (and PowerShell, and a host's shells that are asked) may turn the insertion down,
            // and an Enter queued behind it would run the half-typed line: there the Enter waits
            // for the line the insertion makes.
            let confirm = run && self.fish_shell;
            if confirm {
                let left = result.chars().count().saturating_sub(s.back);
                self.run_after_insert = Some(result.chars().take(left).collect());
            }
            let run = run && !confirm;
            // Fish and zsh check the line left of the cursor is the one lig saw before changing it
            // (over `lig ssh` the shell's reports come a round trip late).
            let payload = if self.fish_shell {
                format!("{};{};{};{}{}", s.delete, s.back, self.line.chars().count(), self.line, s.insert)
            } else if self.zsh_shell {
                format!("{};{};{}\x1f{}", s.delete, s.back, s.insert, self.line)
            } else {
                format!("{};{};{}", s.delete, s.back, s.insert)
            };
            let fish_key = self.fish_shell || self.nu_shell;
            if let Some(file) = self.remote.as_ref().and_then(|r| r.insert_file.clone()) {
                self.host_insert(file, payload, run);
            } else {
                match insertion_bytes(self.insertion_transport, fish_key, &self.insert_file, &payload) {
                    Ok(bytes) => {
                        fwd.extend_from_slice(&bytes);
                        // The shell applies the insertion before it reads the Enter queued behind it.
                        if run {
                            fwd.push(b'\r');
                        }
                    }
                    Err(error) => crate::log(&format!("completion insertion failed: {error}")),
                }
            }
        }
        self.items.clear();
        self.items_line.clear();
    }

    /// The shell learns the new size right away; the model follows once the terminal says where
    /// its content went (see [`Resync`]).
    fn on_resize(&mut self) {
        let Some((rows, cols)) = term::size() else { return };
        self.shell_pty.resize(rows, cols, term::pixels());
        if let Some(probe) = &mut self.startup_cursor {
            // Its reply is for the old geometry. Keep owning and consume it
            // before resize asks again, but do not replay it into the model.
            probe.replay = None;
        }
        let ask_at = Instant::now() + RESIZE_SETTLE;
        match &mut self.resync {
            Some(r) => {
                r.size = Some((rows, cols));
                r.ask_at = ask_at;
                r.stale |= r.asked.is_some();
            }
            None => {
                self.resync = Some(Resync {
                    size: Some((rows, cols)),
                    ask_at,
                    asked: None,
                    stale: false,
                    shell_reply_until: None,
                    held: vec![],
                });
            }
        }
    }

    fn resync_wake(&self) -> Option<Instant> {
        let r = self.resync.as_ref()?;
        if let Some(probe) = &self.startup_cursor {
            return Some(probe.deadline.until);
        }
        Some(match r.asked {
            Some(at) => at + CURSOR_REPORT_WAIT,
            // Waiting for the shell's own answers: look again shortly.
            None if self.waiting_for_shell_cursor_report() && Instant::now() >= r.ask_at => Instant::now() + DEBOUNCE,
            None => r.ask_at,
        })
    }

    fn resync_tick(&mut self) {
        if self.startup_cursor.is_some() {
            return;
        }
        let now = Instant::now();
        let pager = self.parser.screen().alternate_screen();
        let shell_cursor_pending = self.waiting_for_shell_cursor_report();
        let Some(r) = &mut self.resync else { return };
        match r.asked {
            None if now >= r.ask_at => {
                if pager {
                    // A pager redraws itself at the new size; the normal screen is checked when it exits.
                    let r = self.resync.take().unwrap();
                    if let Some((rows, cols)) = r.size {
                        self.parser.screen_mut().set_size(rows, cols);
                    }
                    self.normal_unsynced = true;
                    self.mark_dirty();
                    self.on_output(&r.held);
                    return;
                }
                match resync_query(now, r.ask_at, r.shell_reply_until, shell_cursor_pending) {
                    ResyncQuery::Wait => return,
                    ResyncQuery::Fallback => {
                        // No DSR here: an unfinished child query could still be
                        // on the wire, or its response could still be in flight.
                        self.shell_cursor_queries = 0;
                        self.flush_cursor_partial();
                        self.resynced(None);
                        return;
                    }
                    ResyncQuery::Ask => {}
                }
                r.asked = Some(now);
                self.write_out(b"\x1b[6n");
            }
            Some(at) if now >= at + CURSOR_REPORT_WAIT => {
                self.flush_cursor_partial();
                self.resynced(None);
            }
            _ => {}
        }
    }

    fn waiting_for_shell_cursor_report(&self) -> bool {
        self.shell_cursor_queries > 0 || self.cursor_query_counter.incomplete()
    }

    /// The terminal said where its cursor is (1-based), or didn't in time: move the model, the
    /// popup on it and bash's prompt marker to match, then let the held output through.
    fn resynced(&mut self, report: Option<(u16, u16)>) {
        let Some(r) = self.resync.as_mut() else { return };
        if std::mem::take(&mut r.stale) {
            // For a size since replaced: ask again.
            r.asked = None;
            return;
        }
        let r = self.resync.take().unwrap();
        let (rows, cols) = r.size.unwrap_or_else(|| self.parser.screen().size());
        let old_cols = self.parser.screen().size().1;
        let (row, col) = self.parser.screen().cursor_position();
        // Without an answer, assume what most terminals do: scroll just enough to keep the cursor on screen.
        let (row, col) = report.map_or((row, col), |(r, c)| (r - 1, c - 1));
        let shift = follow_terminal(&mut self.parser, rows, cols, (row.min(rows - 1), col.min(cols - 1)));
        crate::log(&format!("resized to {rows}x{cols}: content moved {shift} rows (reported: {})", report.is_some()));
        if let Some(prompt) = &mut self.prompt_end {
            prompt.row = (i32::from(prompt.row) - shift).clamp(0, i32::from(rows) - 1) as u16;
        }
        self.drawn = self.drawn.and_then(|rect| shifted(rect, shift, rows));
        self.shown = self.shown.and_then(|rect| shifted(rect, shift, rows));
        if shift < 0 || cols != old_cols {
            // Rows above came back from the terminal's scrollback or were rewrapped: unknown now
            // (`known_from` keeps this to the rows above the cursor).
            self.unknown_rows = u16::MAX;
        }
        self.mark_dirty();
        if !r.held.is_empty() {
            self.on_output(&r.held);
        }
    }

    // ------------------------------------------------------------ drawing

    fn known_from(&mut self) -> u16 {
        let screen = self.parser.screen_mut();
        if screen.alternate_screen() {
            return 0;
        }
        screen.set_scrollback(usize::MAX);
        let scrolled = screen.scrollback().min(usize::from(u16::MAX)) as u16;
        screen.set_scrollback(0);
        let row = screen.cursor_position().0;
        // A clear or resize can move the prompt above the startup row.
        self.unknown_rows = self.unknown_rows.min(scrolled.saturating_add(row));
        self.unknown_rows.saturating_sub(scrolled)
    }

    fn popup_layout(&self, shown: &[Suggestion]) -> Option<popup::Layout> {
        if shown.is_empty() {
            None
        } else {
            let screen = self.parser.screen();
            let delete = shown[0].delete;
            let tail: String = {
                let n = self.line.chars().count();
                self.line.chars().skip(n.saturating_sub(delete)).collect()
            };
            // Wrapped rows above the cursor belong to the command too.
            let (crow, _) = screen.cursor_position();
            let mut top = crow;
            while top > 0 && screen.row_wrapped(top - 1) {
                top -= 1;
            }
            popup::layout_at(&self.cfg.ui, shown, screen.size(), screen.cursor_position(), top, tail.width() as u16)
        }
    }

    /// The theme and colors from `config.toml`, if it changed; true if they did. Other settings
    /// still need a new session. A file being written, or with a mistake, keeps the current theme.
    fn reload_theme(&mut self) -> bool {
        self.config_checked = Instant::now();
        let stamp = config_stamp();
        // Some filesystems keep modification times in whole seconds: a second write within the
        // same second, of the same size, would look unchanged, so a recent file is read anyway.
        let recent = stamp.is_some_and(|(t, _)| t.elapsed().is_ok_and(|e| e < Duration::from_secs(2)));
        if stamp == self.config_stamp && !recent {
            return false;
        }
        self.config_stamp = stamp;
        let Ok(text) = std::fs::read_to_string(crate::config::config_path()) else { return false };
        let ui = match Config::parse(&text) {
            Ok(c) => c.ui,
            Err(e) => {
                crate::log(&format!("config: keeping the theme: {e}"));
                return false;
            }
        };
        if ui.theme == self.cfg.ui.theme && ui.colors == self.cfg.ui.colors {
            return false;
        }
        crate::log(&format!("config: theme {} reloaded", ui.theme));
        self.cfg.ui.theme = ui.theme;
        self.cfg.ui.colors = ui.colors;
        true
    }

    fn render(&mut self) {
        if self.startup_cursor.is_some() {
            self.dirty = false;
            return;
        }
        // The model is about to move (see `Resync`): draw once it has.
        if self.resync.is_some() {
            self.mark_dirty();
            return;
        }
        if self.config_checked.elapsed() >= CONFIG_POLL {
            self.reload_theme();
        }
        self.dirty = false;
        let mut w = vec![];
        let stuck = self.osc.take_pending();
        if !stuck.is_empty() {
            self.erase(&mut w);
            w.extend_from_slice(&stuck);
            self.process_child_screen(&stuck);
        }
        let fresh = self.visible();
        if fresh {
            self.stale_since = None;
        }
        // While the new line (fish) or its suggestions are on the way, keep the popup briefly
        // instead of blinking. Keys still act only on current suggestions (`popup_open`).
        let keep = !fresh
            && self.shown.is_some()
            && self.drawable()
            && self.stale_since.get_or_insert_with(Instant::now).elapsed() < LOADING_DELAY;
        let message = self.loading_message().filter(|_| !keep).map(|label| {
            // Placed like a suggestion for the word being typed.
            let word = self.line.chars().rev().take_while(|c| !c.is_whitespace()).count();
            Suggestion {
                label: label.to_string(),
                description: String::new(),
                kind: Kind::Info,
                icon: None,
                insert: String::new(),
                delete: word,
                back: 0,
                query: String::new(),
            }
        });
        let is_message = message.is_some();
        let shown: Vec<Suggestion> = if fresh || keep { self.items.clone() } else { message.into_iter().collect() };
        let mut layout = self.popup_layout(&shown);
        let known_from = self.known_from();
        if let Some(l) = &layout
            && l.rect.row < known_from
        {
            let (rows, _) = self.parser.screen().size();
            let (row, col) = self.parser.screen().cursor_position();
            let scroll = l.rect.height.saturating_sub(rows - row - 1).min(row);
            if scroll > 0 {
                self.erase(&mut w);
                self.begin_sync(&mut w);
                // Make blank space below the prompt without overwriting older output.
                let bytes =
                    format!("\x1b[{rows};1H{}\x1b[{};{}H", "\n".repeat(usize::from(scroll)), row + 1 - scroll, col + 1);
                w.extend_from_slice(bytes.as_bytes());
                self.parser.process(bytes.as_bytes());
                if let Some(prompt) = &mut self.prompt_end {
                    prompt.row = prompt.row.saturating_sub(scroll);
                }
                layout = self.popup_layout(&shown);
            }
            if layout.as_ref().is_some_and(|l| l.rect.row < self.known_from()) {
                layout = None;
            }
        }
        // Kept suggestions stay put: their anchor (`delete`) is for the old line.
        if keep && let (Some(l), Some(old)) = (layout.as_mut(), self.shown) {
            l.rect.col = old.col.min(self.parser.screen().size().1.saturating_sub(l.rect.width));
        }
        if std::env::var_os("LIG_LOG").is_some() {
            crate::log(&format!(
                "render editing={} hidden={} foreign={} items={} current={} alternate={} cursor={:?} size={:?} rect={:?}",
                self.editing,
                self.hidden,
                self.foreign,
                self.items.len(),
                self.items_line == self.line,
                self.parser.screen().alternate_screen(),
                self.parser.screen().cursor_position(),
                self.parser.screen().size(),
                layout.as_ref().map(|l| l.rect)
            ));
        }
        // Stays set across the brief erase/redraw around shell output.
        self.shown = layout.as_ref().filter(|_| !is_message).map(|l| l.rect);
        match (&layout, self.drawn) {
            (Some(l), Some(old)) if l.rect == old => {}
            _ => self.erase(&mut w),
        }
        if let Some(l) = layout {
            let h = l.visible();
            if self.selected < self.scroll {
                self.scroll = self.selected;
            } else if self.selected >= self.scroll + h {
                self.scroll = self.selected + 1 - h;
            }
            self.scroll = self.scroll.min(self.items.len().saturating_sub(h));
            self.begin_sync(&mut w);
            let (selected, scroll) = if is_message { (usize::MAX, 0) } else { (self.selected, self.scroll) };
            popup::draw(&mut w, &self.cfg.ui, &l, &shown, selected, scroll);
            popup::restore_cursor(&mut w, self.parser.screen());
            self.drawn = Some(l.rect);
        }
        self.end_sync(&mut w);
        self.write_out(&w);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fish_refresh_rejects_stale_replies_and_stays_idle_after_ack() {
        let mut refresh = FishRefresh::default();
        assert!(refresh.wake().is_none());
        refresh.changed();
        refresh.due = Some(Instant::now());
        assert!(refresh.take_due(1));
        refresh.changed();
        assert!(!refresh.reported(0));
        assert!(refresh.reported(1));
        assert!(refresh.stale);
        refresh.due = Some(Instant::now());
        assert!(refresh.take_due(2));
        assert!(!refresh.reported(1));
        assert!(refresh.stale);
        assert!(refresh.reported(2));
        assert!(!refresh.stale);
        assert!(refresh.wake().is_none());
    }

    #[test]
    fn fish_refresh_bounds_retries_and_recovers_on_input() {
        let mut refresh = FishRefresh::default();
        refresh.changed();
        refresh.due = Some(Instant::now());
        for id in 1..=u64::from(SNAPSHOT_ATTEMPTS) {
            assert!(refresh.take_due(id));
            refresh.in_flight.as_mut().unwrap().deadline = Instant::now();
        }
        assert!(!refresh.take_due(4));
        assert!(refresh.wake().is_none());
        assert!(!refresh.reported(3));
        refresh.changed();
        refresh.due = Some(Instant::now());
        assert!(refresh.take_due(5));
        assert!(!refresh.reported(3));
        assert!(refresh.reported(5));
        assert!(!refresh.stale);
    }

    #[test]
    fn cursor_input_keeps_paste_state_across_timeouts_and_idle_reads() {
        use CursorInputPart::{Keys, Report};
        let mut cursor = CursorInput::default();
        assert!(cursor.feed(b"\x1b[20".to_vec(), Reports::Due).is_empty());
        assert_eq!(cursor.flush_partial(), b"\x1b[20");
        assert_eq!(cursor.feed(b"0~\x1b[9;9R".to_vec(), Reports::None), [Keys(b"0~\x1b[9;9R".to_vec())]);
        assert_eq!(cursor.feed(b"\x1b[7;8R\x1b[20".to_vec(), Reports::Due), [Keys(b"\x1b[7;8R\x1b[20".to_vec())]);
        assert_eq!(
            cursor.feed(b"1~\x1b[5;6R".to_vec(), Reports::Due),
            [Keys(b"1~".to_vec()), Report(b"\x1b[5;6R".to_vec(), (5, 6))]
        );
    }

    #[test]
    fn startup_drains_only_the_events_already_queued_at_expiry() {
        let asked = Instant::now();
        let mut deadline = StartupDeadline::new(asked);
        let due = asked + CURSOR_REPORT_WAIT;
        let just_before = due - Duration::from_millis(1);
        let just_after = due + Duration::from_millis(1);
        assert_eq!(deadline.at(just_before, || unreachable!()), StartupWake::Wait);
        assert_eq!(deadline.at(due, || 2), StartupWake::Drain);
        assert_eq!(deadline.at(just_after, || unreachable!()), StartupWake::Drain);
        assert!(deadline.accepts(just_before));
        assert!(!deadline.accepts(just_after));
        assert_eq!(deadline.at(just_after, || unreachable!()), StartupWake::Expire);
    }

    #[test]
    fn resize_waits_for_a_split_query_and_never_asks_through_an_unfinished_one() {
        let asked = Instant::now();
        let due = asked + CURSOR_REPORT_WAIT;
        assert_eq!(resync_query(due - Duration::from_millis(1), asked, None, true), ResyncQuery::Wait);
        assert_eq!(resync_query(due, asked, None, true), ResyncQuery::Fallback);

        let finished = due - Duration::from_millis(1);
        let reply_until = Some(finished + CURSOR_REPORT_WAIT);
        assert_eq!(resync_query(due, asked, reply_until, true), ResyncQuery::Wait);
        assert_eq!(resync_query(due, asked, reply_until, false), ResyncQuery::Ask);
        assert_eq!(resync_query(reply_until.unwrap(), asked, reply_until, true), ResyncQuery::Fallback);
    }

    #[test]
    fn startup_replay_uses_only_emitted_bytes_at_prompt_marks() {
        let mark = b"\x1b]133;A\x07$ ";
        for (before, expected) in [(b"".as_slice(), (6, 9)), (b"\r\n".as_slice(), (7, 2))] {
            let mut bytes = [before, mark].concat();
            let anchor = bytes.len();
            bytes.extend_from_slice(b"command");
            let replay = OriginReplay { bytes, prompt: Some(anchor) };
            let (parser, prompt) = replay.rebase(24, 80, (7, 8));
            let prompt = prompt.unwrap();
            assert_eq!((prompt.row, prompt.col), expected);
            assert!(prompt.prefix.ends_with("$ "));
            assert_eq!(parser.screen().cursor_position(), (expected.0, expected.1 + 7));
        }
    }

    #[test]
    fn startup_replay_is_bounded() {
        let mut replay = OriginReplay::default();
        assert!(replay.extend(&vec![b'x'; STARTUP_REPLAY_LIMIT]));
        assert!(!replay.extend(b"y"));
        assert_eq!(replay.bytes.len(), STARTUP_REPLAY_LIMIT);
    }

    #[test]
    fn cursor_input_preserves_typeahead_and_splits_consecutive_reports() {
        use CursorInputPart::{Keys, Report};
        let mut cursor = CursorInput::default();
        assert_eq!(cursor.feed(b"git \x1b[?1;2c\x1b[12;24".to_vec(), Reports::Due), [Keys(b"git \x1b[?1;2c".to_vec())]);
        assert_eq!(
            cursor.feed(b"R\x1b[9;10Rch".to_vec(), Reports::Due),
            [Report(b"\x1b[12;24R".to_vec(), (12, 24)), Report(b"\x1b[9;10R".to_vec(), (9, 10)), Keys(b"ch".to_vec())]
        );
        for invalid in [b"\x1b[0;1R".as_slice(), b"\x1b[1;2;3R", b"\x1b[999999;2R", b"\x1b[2;3H", b"\x1b[A"] {
            assert_eq!(cursor.feed(invalid.to_vec(), Reports::Due), [Keys(invalid.to_vec())]);
        }
        assert_eq!(cursor.feed(b"\x1b[1;1R".to_vec(), Reports::None), [Keys(b"\x1b[1;1R".to_vec())]);
        assert!(cursor.feed(b"\x1b".to_vec(), Reports::Due).is_empty());
        assert_eq!(cursor.flush_partial(), b"\x1b");
    }

    #[test]
    fn overdue_cursor_reports_still_come_out_but_never_hold_an_esc() {
        use CursorInputPart::{Keys, Report};
        let mut cursor = CursorInput::default();
        assert_eq!(cursor.feed(b"\x1b".to_vec(), Reports::Overdue), [Keys(b"\x1b".to_vec())]);
        assert_eq!(cursor.feed(b"\x1b[12;".to_vec(), Reports::Overdue), [Keys(b"\x1b[12;".to_vec())]);
        assert_eq!(cursor.partial_until, None);
        assert_eq!(
            cursor.feed(b"x\x1b[3;4R".to_vec(), Reports::Overdue),
            [Keys(b"x".to_vec()), Report(b"\x1b[3;4R".to_vec(), (3, 4))]
        );
    }

    #[test]
    fn child_cursor_queries_are_counted_across_output_chunks() {
        let mut queries = CursorQueryCounter::default();
        assert_eq!(queries.observe(b"before\x1b["), 0);
        assert!(queries.incomplete());
        assert_eq!(queries.observe(b"6"), 0);
        assert!(queries.incomplete());
        assert_eq!(queries.observe(b"nafter\x1b[6n\x1b"), 2);
        assert_eq!(queries.observe(b"[6n"), 1);
        assert!(!queries.incomplete());
        assert_eq!(queries.observe(b"not a query"), 0);
    }

    #[test]
    fn startup_defers_a_split_child_query_and_the_output_after_it() {
        let mut output = StartupOutput::default();
        let (forwarded, msgs) = output.gate(b"before\x1b[6".to_vec(), vec![]);
        assert_eq!(forwarded, b"before");
        assert!(msgs.is_empty());
        let (forwarded, msgs) = output.gate(b"nafter".to_vec(), vec![]);
        assert!(forwarded.is_empty());
        assert!(msgs.is_empty());
        let (held, msgs) = output.into_parts();
        assert_eq!(held, b"\x1b[6nafter");
        assert!(msgs.is_empty());
    }

    #[test]
    fn terminal_modes_alone_are_not_a_changed_line() {
        // What fish 4.0 prints around each signal handler.
        let fish = b"\x1b[?2004l\x1b[>4;0m\x1b[=0u\x1b>\x1b[?2004h\x1b[>4;1m\x1b[=5u\x1b=";
        assert!(only_modes(fish));
        assert!(only_modes(b""));
        assert!(only_modes(b"\x1b[?u\x1b[<u"));
        assert!(!only_modes(b"\x1b[?2004hx"));
        assert!(!only_modes(b"\x1b[2K"));
        assert!(!only_modes(b"\x1b[1m"));
        assert!(!only_modes(b"\x1b[?25"));
        assert!(!only_modes(b"\x1b[?1049h"));
        assert!(!only_modes(b"\x1b[?2004;47l"));
    }

    #[test]
    fn shifted_popup_is_clipped_to_the_screen() {
        let rect = Rect { row: 10, col: 4, height: 5, width: 20 };
        assert_eq!(shifted(rect, 3, 30), Some(Rect { row: 7, ..rect }));
        assert_eq!(shifted(rect, 12, 30), Some(Rect { row: 0, height: 3, ..rect }));
        assert_eq!(shifted(rect, -18, 30), Some(Rect { row: 28, height: 2, ..rect }));
        assert_eq!(shifted(rect, 0, 12), Some(Rect { height: 2, ..rect }));
        assert_eq!(shifted(rect, 15, 30), None);
    }

    #[test]
    fn the_model_follows_the_terminals_cursor() {
        let mut parser = vt100::Parser::new(10, 20, 0);
        for n in 0..9 {
            parser.process(format!("line{n}\r\n").as_bytes());
        }
        parser.process(b"$ ");
        // Shrunk to 6 rows, the terminal kept the cursor (row 9) on its last row: 4 rows went up.
        assert_eq!(follow_terminal(&mut parser, 6, 20, (5, 2)), 4);
        assert_eq!(parser.screen().size(), (6, 20));
        assert_eq!(parser.screen().contents_between(0, 0, 0, 20), "line4");
        assert_eq!(parser.screen().contents_between(5, 0, 5, 20).trim_end(), "$");
        assert_eq!(parser.screen().cursor_position(), (5, 2));
        // Grown back, the terminal brought 3 rows back from its scrollback: they're blank here.
        assert_eq!(follow_terminal(&mut parser, 9, 20, (8, 2)), -3);
        assert_eq!(parser.screen().contents_between(3, 0, 3, 20), "line4");
        assert_eq!(parser.screen().cursor_position(), (8, 2));
    }

    #[test]
    fn failed_file_insertion_never_produces_a_key() {
        let missing = std::env::temp_dir().join(format!("lig-missing-{}", std::process::id())).join("payload");
        assert!(insertion_bytes(InsertionTransport::File, true, &missing, "2;1;café;").is_err());
        assert_eq!(
            insertion_bytes(InsertionTransport::Inline, false, &missing, "2;1;café;").unwrap(),
            format!("{INSERT_KEY}2;1;café;\x07").into_bytes()
        );
    }

    #[test]
    fn cmd_gets_insertions_as_keys() {
        assert_eq!(InsertionTransport::for_shell(Path::new("cmd.exe")), InsertionTransport::Keys);
        assert_eq!(InsertionTransport::for_shell(Path::new("pwsh.exe")), InsertionTransport::File);
        assert_eq!(insertion_keys("2;1;checkout ").unwrap(), b"\x7f\x7fcheckout \x1b[D");
        assert_eq!(insertion_keys("0;0;a;b").unwrap(), b"a;b");
        assert_eq!(insertion_keys("x;0;a"), None);
    }

    #[test]
    fn insertion_transport_is_independent_of_prompt_markers() {
        assert_eq!(InsertionTransport::for_shell(Path::new("/bin/fish")), InsertionTransport::File);
        assert_eq!(InsertionTransport::for_shell(Path::new("/bin/bash")), InsertionTransport::File);
        assert_eq!(InsertionTransport::for_shell(Path::new("/bin/zsh")), InsertionTransport::Inline);
    }

    #[test]
    fn enter_rule() {
        // Args: (mode, word_empty, complete, navigated, just_accepted).
        assert!(enter_accepts("auto", true, false, false, false));
        assert!(enter_accepts("auto", false, false, false, false));
        assert!(!enter_accepts("auto", false, true, false, false));
        assert!(!enter_accepts("auto", true, false, false, true));
        assert!(enter_accepts("auto", false, true, true, false));
        assert!(!enter_accepts("after-space", false, false, false, false));
        assert!(enter_accepts("always", false, true, false, false));
        assert!(enter_accepts("always", false, false, false, true));
        assert!(!enter_accepts("never", true, false, true, false));
    }

    #[test]
    fn enter_runs_rule() {
        use Enter::*;
        // Args: (mode, runs, word_empty, complete, navigated, just_accepted).
        // Other commands keep the plain rule.
        assert_eq!(enter_action("auto", false, false, false, false, false), Pick);
        assert_eq!(enter_action("auto", false, false, true, false, false), Run);
        assert_eq!(enter_action("always", false, false, true, false, false), Close);
        // `z chez`: picking the match also jumps there; so does a selection moved with Tab.
        assert_eq!(enter_action("auto", true, false, false, false, false), PickAndRun);
        assert_eq!(enter_action("auto", true, true, false, false, false), PickAndRun);
        assert_eq!(enter_action("auto", true, false, true, true, false), PickAndRun);
        // Where Enter wouldn't pick, it runs the line as typed.
        assert_eq!(enter_action("auto", true, false, true, false, false), Run);
        assert_eq!(enter_action("auto", true, false, false, false, true), Run);
        assert_eq!(enter_action("after-space", true, false, false, false, false), Run);
        assert_eq!(enter_action("never", true, true, false, true, false), Run);
        // `always` on a complete word runs instead of only closing the popup.
        assert_eq!(enter_action("always", true, false, true, false, false), Run);
        assert_eq!(enter_action("always", true, false, false, false, false), PickAndRun);
    }

    #[test]
    fn nu_marks_are_prompt_ends_and_starts() {
        let out = b"\x1b]133;A;k=i\x1b\\/tmp> \x1b]133;B\x1b\\gi\r\n\x1b]133;C\x1b\\\x1b]133;D;1\x1b\\";
        let marks: Vec<(usize, Msg)> = nu_marks(out);
        assert_eq!(marks, [(19, Msg::PromptEnd), (32, Msg::Finish)]);
        assert!(nu_marks(b"\x1b]133;Bx\x1b]133;").is_empty());
        // Cut short at the end of one output, finished by the next.
        assert_eq!(unfinished_nu_mark(b"/tmp> \x1b]13"), b"\x1b]13");
        assert_eq!(unfinished_nu_mark(b"/tmp> \x1b]133;B"), b"\x1b]133;B");
        assert_eq!(unfinished_nu_mark(b"/tmp> \x1b]133;B\x1b\\"), b"");
        assert_eq!(unfinished_nu_mark(b"\x1b[0m"), b"");
        assert_eq!(unfinished_nu_mark(b"\x1b]133;A"), b"");
    }

    #[test]
    fn finds_prompt_marks() {
        assert_eq!(prompt_marks(b"out\x1b]133;A;cl=line\x07$ \x1b]133;B\x07"), [3]);
        assert_eq!(prompt_marks(b"\x1b]133;A\x07\x1b]133;A\x1b\\\x1b]133;AB\x07"), [0, 8]);
        assert_eq!(prompt_marks(b"\x1b]133;A"), [] as [usize; 0]);
    }
}
