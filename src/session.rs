//! `lig shell`: runs the shell inside a PTY, mirrors its screen with vt100,
//! listens to the shell integration (`shell/`) and draws the suggestion popup.
//!
//! Threads read the keyboard, the shell's output and resizes into one event
//! loop ([`Session`]); completion runs on a worker thread so typing never
//! waits for it.

use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::Result;
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, unbounded};
use nix::sys::termios::{SetArg, Termios, cfmakeraw, tcgetattr, tcsetattr};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use unicode_width::UnicodeWidthStr;

use crate::config::Config;
use crate::engine::{Engine, Kind, ShellEnv, Suggestion};
use crate::keys::{self, Action, Bindings, Key};
use crate::osc::{Msg, OscFilter};
use crate::popup::{self, Rect};
use crate::store::Store;

/// Wait for output to settle this long before drawing.
const DEBOUNCE: Duration = Duration::from_millis(4);
/// …but never longer than this.
const MAX_DELAY: Duration = Duration::from_millis(40);
/// How long Enter waits for suggestions that are still loading.
const ENTER_WAIT: Duration = Duration::from_millis(1500);
/// Show "Searching…" only when results take longer than this.
const LOADING_DELAY: Duration = Duration::from_millis(150);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_millis(100);
const SNAPSHOT_ATTEMPTS: u8 = 3;
/// Key sequence bound to the insert function in bash and zsh.
const INSERT_KEY: &str = "\x1b[9147~";

enum Event {
    /// Keys from the user.
    Input(Vec<u8>),
    /// Output from the shell.
    Output(Vec<u8>),
    Exit,
    Resize,
    /// Suggestions for `line`; `loading` names what is still being fetched.
    Done {
        line: String,
        items: Vec<Suggestion>,
        loading: Option<String>,
    },
}

/// A completion request for the worker.
struct Request {
    line: String,
    env: ShellEnv,
    /// Asked for with the toggle key.
    manual: bool,
}

fn winsize(fd: i32) -> Option<(u16, u16)> {
    let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::ioctl(fd, libc::TIOCGWINSZ, &mut ws) } == 0;
    (ok && ws.ws_row > 0 && ws.ws_col > 0).then_some((ws.ws_row, ws.ws_col))
}

/// Remove one complete ANSI cursor report, preserving typeahead and other replies.
fn take_cursor_report(input: &mut Vec<u8>) -> Option<(u16, u16)> {
    for start in 0..input.len().saturating_sub(1) {
        let Some(rest) = input[start..].strip_prefix(b"\x1b[") else { continue };
        let Some(end) = rest.iter().position(|b| !b.is_ascii_digit() && *b != b';') else { continue };
        if rest[end] != b'R' {
            continue;
        }
        let Some((row, col)) = std::str::from_utf8(&rest[..end]).ok().and_then(|s| s.split_once(';')) else { continue };
        let (Ok(row), Ok(col)) = (row.parse::<u16>(), col.parse::<u16>()) else { continue };
        if row == 0 || col == 0 {
            continue;
        }
        input.drain(start..start + 2 + end + 1);
        return Some((row, col));
    }
    None
}

/// The outer terminal can already contain output when lig starts. Establish its
/// real origin before forwarding child output; a new vt100 screen assumes (1,1).
fn terminal_origin() -> (Option<(u16, u16)>, Vec<u8>) {
    let mut input = Vec::new();
    let mut out = std::io::stdout().lock();
    if out.write_all(b"\x1b[6n").and_then(|_| out.flush()).is_err() {
        return (None, input);
    }
    let deadline = Instant::now() + Duration::from_millis(200);
    while Instant::now() < deadline {
        let wait = deadline.saturating_duration_since(Instant::now()).as_millis().max(1) as i32;
        let mut fd = libc::pollfd { fd: 0, events: libc::POLLIN, revents: 0 };
        let ready = unsafe { libc::poll(&mut fd, 1, wait) };
        if ready < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
            continue;
        }
        if ready <= 0 || fd.revents & libc::POLLIN == 0 {
            break;
        }
        let mut bytes = [0; 4096];
        let n = unsafe { libc::read(0, bytes.as_mut_ptr().cast(), bytes.len()) };
        if n <= 0 {
            break;
        }
        input.extend_from_slice(&bytes[..n as usize]);
        if let Some(position) = take_cursor_report(&mut input) {
            return (Some(position), input);
        }
    }
    (None, input)
}

/// Replace this process with the plain shell (used when lig can't run).
fn exec_shell(shell: &Path, login: bool) -> ! {
    let mut c = std::process::Command::new(shell);
    if login {
        c.arg("-l");
    }
    let err = c.env("LIG_DISABLE", "1").exec();
    eprintln!("lig: cannot run {}: {err}", shell.display());
    std::process::exit(127);
}

struct RawMode(Termios);

impl RawMode {
    fn enable() -> Result<RawMode> {
        let orig = tcgetattr(std::io::stdin())?;
        let mut raw = orig.clone();
        cfmakeraw(&mut raw);
        tcsetattr(std::io::stdin(), SetArg::TCSANOW, &raw)?;
        Ok(RawMode(orig))
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, &self.0);
    }
}

/// The shell that is talking to lig right now.
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
}

impl InsertionTransport {
    fn for_shell(shell: &Path) -> Self {
        match shell.file_name().and_then(|s| s.to_str()) {
            Some("bash" | "fish") => Self::File,
            _ => Self::Inline,
        }
    }
}

/// Build the insertion only after its file payload has been written successfully.
fn insertion_bytes(transport: InsertionTransport, fish: bool, file: &Path, payload: &str) -> std::io::Result<Vec<u8>> {
    match transport {
        InsertionTransport::File => {
            std::fs::write(file, payload)?;
            Ok(if fish { b"\x1b[24;8~".to_vec() } else { INSERT_KEY.as_bytes().to_vec() })
        }
        InsertionTransport::Inline => Ok(format!("{INSERT_KEY}{payload}\x07").into_bytes()),
    }
}

pub fn run(shell: PathBuf, login: bool) -> Result<i32> {
    if unsafe { libc::isatty(0) == 0 || libc::isatty(1) == 0 } {
        exec_shell(&shell, login);
    }
    let (rows, cols) = winsize(0).unwrap_or((24, 80));
    let size = PtySize { rows, cols, pixel_width: 0, pixel_height: 0 };
    let pair = match native_pty_system().openpty(size) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("lig: openpty failed: {e}");
            exec_shell(&shell, login);
        }
    };
    let mut cmd = CommandBuilder::new(&shell);
    if login {
        cmd.arg("-l");
    }
    if let Ok(cwd) = std::env::current_dir() {
        cmd.cwd(cwd);
    }
    let tty = pair.master.tty_name();
    if let Some(tty) = &tty {
        cmd.env("LIG_TTY", tty);
    }
    cmd.env("LIG_PID", std::process::id().to_string());
    // bash reads accepted completions from this file (see shell/post.bash).
    let insert_file =
        dirs::runtime_dir().unwrap_or_else(std::env::temp_dir).join(format!("lig-{}.insert", std::process::id()));
    let query_file = insert_file.with_extension("query");
    cmd.env("LIG_INSERT_FILE", &insert_file);
    cmd.env("LIG_QUERY_FILE", &query_file);
    let mut child = match pair.slave.spawn_command(cmd) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("lig: spawning {} failed: {e}", shell.display());
            exec_shell(&shell, login);
        }
    };
    drop(pair.slave);
    let shell_pid = child.process_id();
    let _mirror = shell_pid.and_then(|pid| {
        crate::mirror::spawn(pid, std::env::var_os("SHELL").unwrap_or_else(|| shell.clone().into_os_string()))
    });

    let reader = pair.master.try_clone_reader()?;
    let writer = pair.master.take_writer()?;
    let raw = RawMode::enable()?;
    let (origin, typeahead) = terminal_origin();
    crate::log(&format!(
        "startup shell={} size={rows}x{cols} origin={origin:?} typeahead={}",
        shell.display(),
        typeahead.len()
    ));
    let mut parser = vt100::Parser::new(rows, cols, usize::from(rows));
    if let Some((row, col)) = origin {
        parser.process(format!("\x1b[{};{}H", row.min(rows), col.min(cols)).as_bytes());
    }
    let cfg = Config::load();
    crate::icons::set_overrides(&cfg.icons);

    let (tx, rx) = unbounded::<Event>();
    if !typeahead.is_empty() {
        let _ = tx.send(Event::Input(typeahead));
    }
    spawn_stdin_reader(tx.clone());
    spawn_pty_reader(reader, tx.clone());
    spawn_resize_watcher(tx.clone())?;
    let (req_tx, req_rx) = unbounded::<Request>();
    let behavior = cfg.behavior.clone();
    std::thread::spawn(move || worker(req_rx, tx, behavior));

    let mut s = Session {
        insert_file,
        query_file,
        fish_sequence: 0,
        unknown_rows: origin.map_or(0, |(row, _)| row.saturating_sub(1).min(rows)),
        insertion_transport: InsertionTransport::for_shell(&shell),
        fish_shell: shell.file_name().is_some_and(|name| name == "fish"),
        fish_refresh: None,
        prompt_end: None,
        shell_nav: false,
        cwd_fallback: std::env::current_dir().unwrap_or_else(|_| "/".into()),
        bindings: Bindings::new(&cfg.keys),
        cfg,
        out: std::io::stdout(),
        pty: writer,
        master: pair.master,
        parser,
        osc: OscFilter::default(),
        editing: false,
        hidden: false,
        line: String::new(),
        env: ShellEnv::default(),
        shell_pid,
        tty,
        foreign: false,
        req_tx,
        items: vec![],
        items_line: String::new(),
        selected: 0,
        scroll: 0,
        navigated: false,
        just_accepted: false,
        close_after_insert: false,
        shown: false,
        manual: false,
        loading: None,
        pending_enter: None,
        enter_after: None,
        deferred: vec![],
        dismissed: None,
        drawn: None,
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
    let code = child.wait().map(|st| st.exit_code() as i32).unwrap_or(0);
    Ok(code)
}

/// Forward the user's keys (stdin) to the event loop.
fn spawn_stdin_reader(tx: Sender<Event>) {
    std::thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            let n = unsafe { libc::read(0, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                let _ = tx.send(Event::Exit);
                break;
            }
            if tx.send(Event::Input(buf[..n as usize].to_vec())).is_err() {
                break;
            }
        }
    });
}

/// Forward the shell's output to the event loop.
fn spawn_pty_reader(mut reader: Box<dyn Read + Send>, tx: Sender<Event>) {
    std::thread::spawn(move || {
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = tx.send(Event::Exit);
                    break;
                }
                Ok(n) => {
                    if tx.send(Event::Output(buf[..n].to_vec())).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

/// Report terminal resizes (SIGWINCH) to the event loop.
fn spawn_resize_watcher(tx: Sender<Event>) -> Result<()> {
    let mut signals = signal_hook::iterator::Signals::new([libc::SIGWINCH])?;
    std::thread::spawn(move || {
        for _ in signals.forever() {
            if tx.send(Event::Resize).is_err() {
                break;
            }
        }
    });
    Ok(())
}

/// Whether a program other than the shell uses the terminal while a line is
/// being edited: a widget running fzf (Ctrl+T), a pager… These run in the
/// shell's own process group (command substitution), so the terminal's
/// foreground group can't tell. Look for a descendant of the shell that isn't
/// a shell itself, is in its process group (background jobs have their own),
/// and has the terminal open for more than stderr (prompt helpers like
/// `$(git …)` only inherit that).
fn foreign_program(shell: u32, tty: &Path) -> bool {
    let stat = |pid: u32| -> Option<(String, i64)> {
        let s = crate::procfs::stat(pid)?;
        let fields = crate::procfs::stat_fields(&s)?;
        let comm = s[..s.rfind(')')?].split_once('(')?.1.to_string();
        let pgrp = fields.get(2)?.parse().ok()?;
        Some((comm, pgrp))
    };
    let mut todo = children(shell);
    if todo.is_empty() {
        return false;
    }
    let Some((_, group)) = stat(shell) else { return false };
    while let Some(pid) = todo.pop() {
        todo.extend(children(pid));
        let Some((comm, pgrp)) = stat(pid) else { continue };
        if pgrp != group || matches!(comm.as_str(), "zsh" | "bash" | "sh" | "dash") {
            continue;
        }
        let on_tty = std::fs::read_dir(format!("/proc/{pid}/fd")).into_iter().flatten().flatten().any(|e| {
            e.file_name() != "2" && std::fs::read_link(e.path()).is_ok_and(|t| t == tty || t == Path::new("/dev/tty"))
        });
        if on_tty {
            return true;
        }
    }
    false
}

/// A process's children (`/proc/<pid>/task/*/children`).
fn children(pid: u32) -> Vec<u32> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
        .flat_map(|s| s.split_whitespace().filter_map(|p| p.parse().ok()).collect::<Vec<u32>>())
        .collect()
}

/// Add `key` to the text typed so far, or give up (`None`) if it isn't a plain
/// character (arrows, Backspace, shortcuts…).
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

/// Whether Enter picks the selected suggestion (instead of running the line).
///
/// `"auto"`: pick, unless the word typed is already complete (equals a
/// suggestion: `git status`); `"after-space"`: only when the line ends in a
/// space; `"always"`; `"never"`. A selection moved with Tab/arrows is always
/// picked, and the Enter right after a pick runs the line.
fn enter_accepts(mode: &str, word_empty: bool, complete: bool, navigated: bool, just_accepted: bool) -> bool {
    match mode {
        "never" => false,
        _ if navigated => true,
        _ if just_accepted => false,
        "always" => true,
        "after-space" => word_empty,
        _ => word_empty || !complete,
    }
}

/// Answer completion requests, newest first: quick results (no commands run)
/// right away, then again once generators and `--help` have run.
fn worker(rx: Receiver<Request>, tx: Sender<Event>, behavior: crate::config::Behavior) {
    let mut engine = Engine::new(Store::default(), behavior);
    while let Ok(mut req) = rx.recv() {
        while let Ok(newer) = rx.try_recv() {
            req = newer;
        }
        let c = engine.complete(&req.line, &req.env, false, req.manual);
        let _ = tx.send(Event::Done { line: req.line.clone(), items: c.items, loading: c.loading });
        if c.incomplete && rx.is_empty() {
            let c = engine.complete(&req.line, &req.env, true, req.manual);
            let _ = tx.send(Event::Done { line: req.line, items: c.items, loading: None });
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
        self.in_flight = Some(PendingSnapshot { id, deadline: Instant::now() + SNAPSHOT_TIMEOUT, changed: false });
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
    insertion_transport: InsertionTransport,
    fish_shell: bool,
    fish_refresh: Option<FishRefresh>,
    cfg: Config,
    insert_file: PathBuf,
    /// Set by bash's prompt marker: the line is read off the screen.
    prompt_end: Option<PromptEnd>,
    /// The last key sent to the shell is one the shell itself acts on —
    /// history (Up/Down) or its own completion (Tab): the lines it produces get
    /// no popup, so those keys keep going to the shell. Typing ends it.
    shell_nav: bool,
    bindings: Bindings,
    out: std::io::Stdout,
    pty: Box<dyn Write + Send>,
    master: Box<dyn MasterPty + Send>,
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
    /// The PTY the shell runs on (`/dev/pts/N`).
    tty: Option<PathBuf>,
    /// Another program uses the terminal while the line is edited (fzf's
    /// Ctrl+T): no popup until it's gone. See [`foreign_program`].
    foreign: bool,
    cwd_fallback: PathBuf,
    req_tx: Sender<Request>,

    // Popup state.
    items: Vec<Suggestion>,
    /// The line `items` were computed for.
    items_line: String,
    selected: usize,
    /// The first item shown (the list scrolls).
    scroll: usize,
    /// The user moved the selection on the current line.
    navigated: bool,
    /// A suggestion was just accepted and nothing typed since: Enter runs.
    just_accepted: bool,
    /// Keep the popup closed for the line the accepted insert produces.
    close_after_insert: bool,
    /// The last render showed suggestions.
    shown: bool,
    /// The popup was asked for with the toggle key (until the line is run).
    manual: bool,
    /// (line, message, since) while slower suggestions are being fetched.
    loading: Option<(String, String, Instant)>,
    /// When Enter was pressed while loading (it waits, see [`ENTER_WAIT`]).
    pending_enter: Option<Instant>,
    /// That Enter and the input after it, replayed in order.
    deferred: Vec<u8>,
    /// For an Enter typed right after other keys (one input chunk): the line
    /// before them, and the line they make if they're plain characters. Enter
    /// waits until the shell reports it (see [`Session::enter_ready`]).
    enter_after: Option<(String, Option<String>)>,
    /// The popup was closed for this line; it reopens when the line changes.
    dismissed: Option<String>,
    /// Where the popup is drawn.
    drawn: Option<Rect>,
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
            if self.fish_refresh.as_ref().and_then(FishRefresh::wake).is_some_and(|due| due <= Instant::now()) {
                self.fish_sequence += 1;
                if self.fish_refresh.as_mut().unwrap().take_due(self.fish_sequence) {
                    let sent = std::fs::write(&self.query_file, self.fish_sequence.to_string()).is_ok()
                        && self
                            .shell_pid
                            .is_some_and(|pid| unsafe { libc::kill(pid as libc::pid_t, libc::SIGUSR1) } == 0);
                    if !sent {
                        crate::log("fish snapshot request failed");
                        self.fish_refresh = None;
                        self.hidden = true;
                    }
                }
            }
            let mut deadline = self.dirty.then(|| (self.last_output + DEBOUNCE).min(self.dirty_since + MAX_DELAY));
            if let Some(w) = self.fish_refresh.as_ref().and_then(FishRefresh::wake) {
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if let Some(w) = self.loading_wake() {
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            if let Some(t) = self.pending_enter {
                let w = t + ENTER_WAIT;
                deadline = Some(deadline.map_or(w, |d| d.min(w)));
            }
            let ev = if let Some(due) = deadline {
                match rx.recv_deadline(due) {
                    Ok(ev) => ev,
                    Err(RecvTimeoutError::Timeout) => {
                        // A snapshot request alone must not repaint the popup.
                        if self
                            .fish_refresh
                            .as_ref()
                            .and_then(FishRefresh::wake)
                            .is_some_and(|due| due <= Instant::now())
                        {
                            continue;
                        }
                        if self.pending_enter.is_some_and(|t| t.elapsed() >= ENTER_WAIT) {
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
                Event::Output(data) => self.on_output(&data),
                Event::Input(data) => self.on_input(&data),
                Event::Done { line, items, loading } => self.on_done(line, items, loading),
                Event::Resize => self.on_resize(),
                Event::Exit => return,
            }
        }
    }

    fn write_out(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        let mut out = self.out.lock();
        let _ = out.write_all(bytes);
        let _ = out.flush();
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
        self.visible() && self.shown
    }

    fn fish_stale(&self) -> bool {
        self.fish_refresh.as_ref().is_some_and(|refresh| refresh.stale)
    }

    fn visible(&self) -> bool {
        !self.fish_stale()
            && self.editing
            && !self.hidden
            && !self.items.is_empty()
            && self.items_line == self.line
            && self.dismissed.as_deref() != Some(self.line.as_str())
            && !self.parser.screen().alternate_screen()
            && !self.foreign
    }

    // ------------------------------------------------------------ events

    fn on_output(&mut self, data: &[u8]) {
        let mut fwd = Vec::with_capacity(data.len());
        let mut msgs = vec![];
        self.osc.feed(data, &mut fwd, &mut msgs);
        let mut w = vec![];
        if !fwd.is_empty() {
            if let Some(refresh) = &mut self.fish_refresh {
                refresh.changed();
            }
            // Take the popup off the screen before the shell draws, so the
            // screen is exactly what the shell (and our vt100 model) expects.
            self.erase(&mut w);
            w.extend_from_slice(&fwd);
        }
        self.write_out(&w);
        // Each message sees the screen as it was where it appeared (bash's
        // prompt marker records the cursor there).
        let mut at = 0;
        for (pos, m) in msgs {
            self.parser.process(&fwd[at..pos]);
            at = pos;
            self.on_msg(m);
        }
        self.parser.process(&fwd[at..]);
        if self.editing && self.prompt_end.is_some() {
            self.read_line_from_screen();
        }
        self.foreign =
            self.editing && self.shell_pid.zip(self.tty.as_deref()).is_some_and(|(pid, tty)| foreign_program(pid, tty));
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

    /// The edit buffer reported by zsh or fish.
    fn on_buffer(&mut self, cursor: usize, text: &str) {
        crate::log(&format!("buffer cursor={cursor} characters={}", text.chars().count()));
        self.editing = true;
        self.hidden = false;
        self.set_line(text.chars().take(cursor).collect());
    }

    /// Talk to `kind` from now on: the shell lig started may have been
    /// replaced (`exec bash`, `exec fish`), and each one reports its line and
    /// takes completions its own way.
    fn switch_shell(&mut self, kind: ShellKind) {
        let fish = kind == ShellKind::Fish;
        if fish != self.fish_shell || (kind == ShellKind::Zsh && self.prompt_end.is_some()) {
            crate::log(&format!("shell is now {kind:?}"));
        }
        self.fish_shell = fish;
        if !fish {
            self.fish_refresh = None;
        }
        if kind != ShellKind::Bash {
            // Only bash's line is read off the screen.
            self.prompt_end = None;
        }
        self.insertion_transport =
            if kind == ShellKind::Zsh { InsertionTransport::Inline } else { InsertionTransport::File };
    }

    /// The text left of the cursor changed (reported by zsh, or read off the
    /// screen for bash).
    fn set_line(&mut self, line: String) {
        if line != self.line {
            if std::mem::take(&mut self.close_after_insert) {
                self.dismissed = Some(line.clone());
            }
            if self.shell_nav {
                self.dismissed = Some(line.clone());
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

    /// bash: the input is whatever is on screen between the prompt marker
    /// and the cursor.
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
                self.just_accepted = false;
                self.close_after_insert = false;
                self.dismissed = None;
                self.items_line.clear();
                self.request();
            }
            Msg::Ready { pid } => {
                crate::log(&format!("reader ready fish={} pid={:?} from={pid:?}", self.fish_shell, self.shell_pid));
                // Fish that replaced the shell lig started (`exec fish`): same process.
                if !self.fish_shell && pid.is_some() && pid == self.shell_pid {
                    self.switch_shell(ShellKind::Fish);
                }
                if self.fish_shell {
                    self.editing = true;
                    let mut refresh = FishRefresh::default();
                    refresh.changed();
                    self.fish_refresh = Some(refresh);
                }
            }
            Msg::Snapshot { id, buffer } => {
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
            Msg::PromptEnd => {
                self.switch_shell(ShellKind::Bash);
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
            Msg::Finish => {
                crate::log("reader finished");
                self.fish_refresh = None;
                self.editing = false;
                self.just_accepted = false;
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
                // Follow the inner shell so hosts that read our cwd from `/proc` see the shell's
                // directory
                let _ = std::env::set_current_dir(&c);
                self.env.cwd = PathBuf::from(c);
            }
            Msg::Aliases(a) => self.env.aliases = std::sync::Arc::new(a.into_iter().collect()),
            Msg::Functions(f) => self.env.functions = std::sync::Arc::new(f),
            Msg::Builtins(b) => self.env.builtins = std::sync::Arc::new(b),
        }
        self.mark_dirty();
    }

    /// The "Searching npm…" message, once loading has taken a moment.
    fn loading_message(&self) -> Option<&str> {
        let (line, label, since) = self.loading.as_ref()?;
        let show = *line == self.line
            && since.elapsed() >= LOADING_DELAY
            && self.editing
            && !self.hidden
            && !self.visible()
            && self.dismissed.as_deref() != Some(self.line.as_str())
            && !self.parser.screen().alternate_screen()
            && !self.foreign;
        show.then_some(label.as_str())
    }

    /// When to wake up to show the loading message.
    fn loading_wake(&self) -> Option<Instant> {
        let (line, _, since) = self.loading.as_ref()?;
        let at = *since + LOADING_DELAY;
        (*line == self.line && Instant::now() < at).then_some(at)
    }

    fn request(&mut self) {
        let mut env = self.env.clone();
        if env.cwd.as_os_str().is_empty() {
            env.cwd = self
                .shell_pid
                .and_then(|p| std::fs::read_link(format!("/proc/{p}/cwd")).ok())
                .unwrap_or_else(|| self.cwd_fallback.clone());
        }
        let _ = self.req_tx.send(Request { line: self.line.clone(), env, manual: self.manual });
        // Until the answer arrives this line counts as loading (the worker may
        // still be busy with an earlier, slower request). The first word only
        // pops up on demand, so it doesn't get a message.
        let first_word = !self.line.trim_start().contains(char::is_whitespace);
        if !first_word || self.manual {
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

    /// The word being typed already equals one of the suggestions (`git
    /// status`, `--amend`): accepting it would change nothing.
    fn word_complete(&self) -> bool {
        let chars: Vec<char> = self.line.chars().collect();
        self.items.iter().any(|s| {
            let typed: String = chars[chars.len().saturating_sub(s.delete)..].iter().collect();
            !typed.is_empty() && s.insert.trim_end().trim_end_matches('/') == typed.trim_end_matches('/')
        })
    }

    fn enter_picks(&self) -> bool {
        enter_accepts(
            &self.cfg.behavior.enter_accepts,
            self.word_empty(),
            self.word_complete(),
            self.navigated,
            self.just_accepted,
        )
    }

    /// Replay a deferred Enter (and the keys after it) now that the
    /// suggestions are final, or loading took too long.
    fn resolve_pending_enter(&mut self) {
        if self.pending_enter.take().is_none() {
            return;
        }
        self.enter_after = None;
        self.loading = None;
        let data = std::mem::take(&mut self.deferred);
        if self.fish_stale() {
            // The reader did not acknowledge in time. Preserve native Enter
            // behavior rather than accepting a suggestion for an old buffer.
            self.write_pty(&data);
        } else {
            self.on_input(&data);
        }
    }

    fn on_done(&mut self, line: String, items: Vec<Suggestion>, loading: Option<String>) {
        if line != self.line {
            return; // stale
        }
        let fin = loading.is_none();
        self.apply_done(line, items, loading);
        if fin && self.pending_enter.is_some() && !self.fish_stale() && self.enter_ready() {
            self.render();
            self.resolve_pending_enter();
        }
    }

    fn apply_done(&mut self, line: String, items: Vec<Suggestion>, loading: Option<String>) {
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
        // When slower results arrive for the same line, keep the selection
        // only if the user moved it; otherwise start from the top again.
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
        // Keys typed while a deferred Enter waits go after it, in order.
        if self.pending_enter.is_some() {
            self.deferred.extend_from_slice(data);
            return;
        }
        let mut fwd: Vec<u8> = vec![];
        let mut redraw = false;
        let keys = keys::split(data);
        // Once a key in this chunk has gone to the shell, the popup state is
        // stale until the shell redraws: the rest of the chunk goes through as is
        // (pastes, fast macros).
        let mut passthrough = false;
        // What the keys sent to the shell in this chunk type, if they're all
        // plain characters.
        let mut typed: Option<String> = Some(String::new());
        for (n, (raw, key)) in keys.iter().enumerate() {
            let (raw, key) = (*raw, key.clone());
            if passthrough {
                // Enter right after keys this chunk sent (`mv a.txt Doc` and
                // Enter typed fast): the line isn't updated yet. Hold Enter
                // until the shell reports it and its suggestions are ready.
                if key == Key::ENTER && self.enter_can_wait() {
                    let expected = typed.take().map(|t| format!("{}{t}", self.line));
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
            if key == Key::ENTER && self.enter_should_wait() {
                self.pending_enter = Some(Instant::now());
                let rest: usize = keys[n..].iter().map(|(r, _)| r.len()).sum();
                self.deferred = data[data.len() - rest..].to_vec();
                break;
            }
            if self.bindings.action(&key) == Some(Action::Toggle) && self.can_show() {
                self.toggle();
                redraw = true;
                continue;
            }
            if !self.popup_open() {
                if key != Key::ENTER {
                    self.just_accepted = false;
                }
                // Keys the shell uses for history and its own completion.
                self.shell_nav =
                    [Key::UP, Key::DOWN, Key::ctrl('p'), Key::ctrl('n'), Key::TAB, Key::SHIFT_TAB].contains(&key);
                push_typed(&mut typed, &key);
                fwd.extend_from_slice(raw);
                passthrough = true;
                continue;
            }
            let mut action = self.bindings.action(&key);
            // Next with a single suggestion: nothing to cycle through, take it.
            if action == Some(Action::Next) && self.items.len() == 1 {
                action = Some(Action::Accept);
            }
            // Enter completes (`go bui` → build) or picks after a space, but
            // runs a word that's already complete; the Enter after a pick runs.
            if action == Some(Action::Accept) && key == Key::ENTER && !self.enter_picks() {
                action = None;
            }
            match action {
                Some(Action::Next) => {
                    self.navigated = true;
                    self.selected = (self.selected + 1) % self.items.len();
                    redraw = true;
                }
                Some(Action::Prev) if self.selected == 0 => {
                    // Up from the first item closes the popup (and the next Up
                    // goes to the shell's history).
                    self.dismiss();
                    redraw = true;
                }
                Some(Action::Prev) => {
                    self.navigated = true;
                    self.selected -= 1;
                    redraw = true;
                }
                Some(Action::Dismiss | Action::Toggle) => {
                    self.dismiss();
                    redraw = true;
                }
                Some(Action::Accept) => {
                    self.just_accepted = true;
                    passthrough = true;
                    // The insertion changes the line in ways not worth predicting.
                    typed = None;
                    self.accept_selected(&mut fwd);
                    redraw = true;
                }
                None => {
                    // Not for the popup: the key goes to the shell.
                    if key == Key::ENTER {
                        self.dismissed = Some(self.line.clone());
                        redraw = true;
                    } else {
                        self.just_accepted = false;
                    }
                    passthrough = true;
                    fwd.extend_from_slice(raw);
                }
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
    }

    /// The popup may be shown now (the shell is editing a line, nothing hides it).
    fn can_show(&self) -> bool {
        self.editing && !self.hidden && !self.parser.screen().alternate_screen() && !self.foreign
    }

    /// Enter may be held back to act on up-to-date suggestions.
    fn enter_can_wait(&self) -> bool {
        self.can_show()
            && self.pending_enter.is_none()
            && self.bindings.action(&Key::ENTER) == Some(Action::Accept)
            && self.cfg.behavior.enter_accepts != "never"
    }

    /// A held Enter may go on: the shell has reported the line typed before it
    /// (or hides the popup).
    fn enter_ready(&self) -> bool {
        match &self.enter_after {
            _ if self.hidden => true,
            None => true,
            Some((_, Some(expected))) => self.line == *expected,
            Some((before, None)) => self.line != *before,
        }
    }

    /// Enter while this line's suggestions are still loading waits for them
    /// (briefly), so it acts on the final list, not a partial one.
    fn enter_should_wait(&self) -> bool {
        self.pending_enter.is_none()
            && self.bindings.action(&Key::ENTER) == Some(Action::Accept)
            && self.cfg.behavior.enter_accepts != "never"
            && !self.just_accepted
            && self.editing
            && !self.hidden
            && (self.fish_stale()
                || (self.dismissed.as_deref() != Some(self.line.as_str())
                    && self.loading.as_ref().is_some_and(|(l, _, _)| *l == self.line)))
    }

    /// The toggle key (Ctrl+Space): close the popup, or ask for suggestions
    /// even where it doesn't open by itself (command names).
    fn toggle(&mut self) {
        if self.popup_open() {
            self.dismiss();
        } else {
            self.manual = true;
            self.dismissed = None;
            self.navigated = false;
            self.items_line.clear();
            self.request();
        }
    }

    /// Close the popup for this line; it comes back when the line changes.
    fn dismiss(&mut self) {
        self.dismissed = Some(self.line.clone());
        self.manual = false;
    }

    /// Send the selected suggestion to the shell, to edit the line with.
    fn accept_selected(&mut self, fwd: &mut Vec<u8>) {
        let s = &self.items[self.selected];
        if s.kind == Kind::Execute {
            // The "Enter" item: run the line.
            fwd.push(b'\r');
        } else {
            // A completed word needs no popup until the next keystroke; after
            // `--option ` or a folder (`src/`) the next suggestions show right
            // away. Judged on the resulting line: Back from `Downloads/`
            // inserts nothing and leaves `cd `, which keeps it open.
            let kept: String = {
                let n = self.line.chars().count();
                self.line.chars().take(n.saturating_sub(s.delete)).collect()
            };
            let result = format!("{kept}{}", s.insert);
            self.close_after_insert = !(result.is_empty() || result.ends_with(|c: char| c.is_whitespace() || c == '/'));
            // Fish checks the expected prefix before changing the buffer.
            let payload = if self.fish_shell {
                format!("{};{};{};{}{}", s.delete, s.back, self.line.chars().count(), self.line, s.insert)
            } else {
                format!("{};{};{}", s.delete, s.back, s.insert)
            };
            match insertion_bytes(self.insertion_transport, self.fish_shell, &self.insert_file, &payload) {
                Ok(bytes) => fwd.extend_from_slice(&bytes),
                Err(error) => crate::log(&format!("completion insertion failed: {error}")),
            }
        }
        self.items.clear();
        self.items_line.clear();
    }

    fn on_resize(&mut self) {
        let Some((rows, cols)) = winsize(0) else { return };
        self.clear_popup();
        let _ = self.master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
        self.parser.screen_mut().set_size(rows, cols);
        self.mark_dirty();
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
            // Where the command starts: wrapped rows above the cursor row
            // belong to it too.
            let (crow, _) = screen.cursor_position();
            let mut top = crow;
            while top > 0 && screen.row_wrapped(top - 1) {
                top -= 1;
            }
            popup::layout_at(&self.cfg.ui, shown, screen.size(), screen.cursor_position(), top, tail.width() as u16)
        }
    }

    fn render(&mut self) {
        self.dirty = false;
        let mut w = vec![];
        let stuck = self.osc.take_pending();
        if !stuck.is_empty() {
            self.erase(&mut w);
            w.extend_from_slice(&stuck);
            self.parser.process(&stuck);
        }
        // Either the suggestions, or a one-line "Searching…" message.
        let message = self.loading_message().map(|label| {
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
        let shown: Vec<Suggestion> = if self.visible() { self.items.clone() } else { message.into_iter().collect() };
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
        if std::env::var_os("LIG_LOG").is_some() {
            crate::log(&format!(
                "render editing={} hidden={} items={} current={} alternate={} cursor={:?} size={:?} rect={:?}",
                self.editing,
                self.hidden,
                self.items.len(),
                self.items_line == self.line,
                self.parser.screen().alternate_screen(),
                self.parser.screen().cursor_position(),
                self.parser.screen().size(),
                layout.as_ref().map(|l| l.rect)
            ));
        }
        // What the user sees; it stays set across the brief erase/redraw
        // around shell output.
        self.shown = layout.is_some() && !is_message;
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
    fn startup_cursor_report_preserves_typeahead_and_other_replies() {
        let mut input = b"git \x1b[?1;2c\x1b[12;24".to_vec();
        assert_eq!(take_cursor_report(&mut input), None);
        input.extend_from_slice(b"Rch");
        assert_eq!(take_cursor_report(&mut input), Some((12, 24)));
        assert_eq!(input, b"git \x1b[?1;2cch");
        for invalid in [b"\x1b[0;1R".as_slice(), b"\x1b[1;2;3R", b"\x1b[999999;2R", b"\x1b[2;3H"] {
            let mut input = invalid.to_vec();
            assert_eq!(take_cursor_report(&mut input), None);
            assert_eq!(input, invalid);
        }
        let mut parser = vt100::Parser::new(30, 100, 0);
        parser.process(b"\x1b[12;24Htest> git ch");
        assert_eq!(parser.screen().cursor_position(), (11, 35));
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
    fn insertion_transport_is_independent_of_prompt_markers() {
        assert_eq!(InsertionTransport::for_shell(Path::new("/bin/fish")), InsertionTransport::File);
        assert_eq!(InsertionTransport::for_shell(Path::new("/bin/bash")), InsertionTransport::File);
        assert_eq!(InsertionTransport::for_shell(Path::new("/bin/zsh")), InsertionTransport::Inline);
    }

    #[test]
    fn enter_rule() {
        // (word_empty, complete, navigated, just_accepted)
        // `z ` → pick; `go bui` → complete it; `git status` → run.
        assert!(enter_accepts("auto", true, false, false, false));
        assert!(enter_accepts("auto", false, false, false, false));
        assert!(!enter_accepts("auto", false, true, false, false));
        // Enter, Enter: pick, then run. Tab to an item, then Enter: picked.
        assert!(!enter_accepts("auto", true, false, false, true));
        assert!(enter_accepts("auto", false, true, true, false));
        // Other modes.
        assert!(!enter_accepts("after-space", false, false, false, false));
        assert!(enter_accepts("always", false, true, false, false));
        assert!(!enter_accepts("never", true, false, true, false));
    }
}
