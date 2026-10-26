//! Terminal input as keys, key names for the `[keys]` config, and the
//! bindings between them.
//!
//! A key is a base key plus Ctrl/Alt/Shift. Names look like `ctrl-n`,
//! `alt-shift-up`, `f5` or `ctrl-pagedown`; anything else a terminal sends can
//! be bound by its escape sequence (`\x1b[…`). `lig keys` prints the name of
//! each key you press.

use std::io::{Read, Write};

/// The base key, without modifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Code {
    /// A printable character (`a`, `A`, `?`, ` `).
    Char(char),
    Tab,
    Enter,
    Esc,
    Backspace,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    F(u8),
    /// Anything else, by its bytes (unknown sequences, pastes).
    Raw(Vec<u8>),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Key {
    pub code: Code,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    const fn plain(code: Code) -> Key {
        Key { code, ctrl: false, alt: false, shift: false }
    }

    pub const ENTER: Key = Key::plain(Code::Enter);
    pub const TAB: Key = Key::plain(Code::Tab);
    pub const SHIFT_TAB: Key = Key { code: Code::Tab, ctrl: false, alt: false, shift: true };
    pub const UP: Key = Key::plain(Code::Up);
    pub const DOWN: Key = Key::plain(Code::Down);

    pub const fn ctrl(c: char) -> Key {
        Key { code: Code::Char(c), ctrl: true, alt: false, shift: false }
    }

    /// One spelling per key, so what the terminal sends and what the config
    /// names compare equal: `ctrl-i` is Tab and `ctrl-A` is `ctrl-a` (that's
    /// all most terminals can send), `shift-a` is `A`.
    fn normalized(mut self) -> Key {
        if let Code::Char(c) = self.code {
            if self.ctrl {
                let base = match c.to_ascii_lowercase() {
                    'i' => Some(Code::Tab),
                    'm' => Some(Code::Enter),
                    '[' => Some(Code::Esc),
                    '@' => Some(Code::Char(' ')),
                    _ => None,
                };
                if let Some(code) = base {
                    self.code = code;
                    self.ctrl = c == '@';
                } else if c.is_ascii_alphabetic() {
                    self.code = Code::Char(c.to_ascii_lowercase());
                    self.shift = false;
                }
            } else if c.is_alphabetic() && (self.shift || c.is_uppercase()) {
                self.code = Code::Char(c.to_uppercase().next().unwrap_or(c));
                self.shift = false;
            }
        }
        self
    }

    /// The name [`Key::parse_name`] reads back (`ctrl-alt-x`, `shift-up`, `f5`).
    pub fn name(&self) -> String {
        let base = match &self.code {
            Code::Char(' ') => "space".to_string(),
            Code::Char('-') => "minus".to_string(),
            Code::Char(c) => c.to_string(),
            Code::Tab => "tab".into(),
            Code::Enter => "enter".into(),
            Code::Esc => "esc".into(),
            Code::Backspace => "backspace".into(),
            Code::Up => "up".into(),
            Code::Down => "down".into(),
            Code::Left => "left".into(),
            Code::Right => "right".into(),
            Code::Home => "home".into(),
            Code::End => "end".into(),
            Code::PageUp => "pageup".into(),
            Code::PageDown => "pagedown".into(),
            Code::Insert => "insert".into(),
            Code::Delete => "delete".into(),
            Code::F(n) => format!("f{n}"),
            Code::Raw(bytes) => return escape(bytes),
        };
        let mods = [(self.ctrl, "ctrl-"), (self.alt, "alt-"), (self.shift, "shift-")];
        mods.iter().filter(|(on, _)| *on).map(|(_, m)| *m).collect::<String>() + &base
    }

    /// A key from its config name: modifiers (`ctrl`, `alt`/`meta`, `shift`,
    /// joined with `-` or `+`) and a key, or an escape sequence (`\x1b[1;5C`).
    pub fn parse_name(name: &str) -> Option<Key> {
        let name = name.trim();
        if name.starts_with("\\x") || name.starts_with("\\e") || name.contains('\x1b') {
            return Some(Key::plain(Code::Raw(unescape(name))).normalized_raw());
        }
        let mut key = Key::plain(Code::Esc);
        let mut rest = name;
        loop {
            let lower = rest.to_ascii_lowercase();
            let Some((m, len)) = ["ctrl", "control", "alt", "meta", "shift"].iter().find_map(|m| {
                let after = lower.strip_prefix(m)?;
                (after.starts_with(['-', '+']) && after.len() > 1).then_some((*m, m.len() + 1))
            }) else {
                break;
            };
            match m {
                "ctrl" | "control" => key.ctrl = true,
                "alt" | "meta" => key.alt = true,
                _ => key.shift = true,
            }
            rest = &rest[len..];
        }
        let mut chars = rest.chars();
        key.code = match (chars.next(), chars.next()) {
            (Some(c), None) => Code::Char(c),
            _ => match rest.to_ascii_lowercase().replace(['-', '_'], "").as_str() {
                "tab" => Code::Tab,
                "backtab" => {
                    key.shift = true;
                    Code::Tab
                }
                "enter" | "return" | "cr" => Code::Enter,
                "esc" | "escape" => Code::Esc,
                "backspace" | "bs" => Code::Backspace,
                "space" | "spc" => Code::Char(' '),
                "minus" => Code::Char('-'),
                "plus" => Code::Char('+'),
                "up" => Code::Up,
                "down" => Code::Down,
                "left" => Code::Left,
                "right" => Code::Right,
                "home" => Code::Home,
                "end" => Code::End,
                "pageup" | "pgup" => Code::PageUp,
                "pagedown" | "pgdn" | "pgdown" => Code::PageDown,
                "insert" | "ins" => Code::Insert,
                "delete" | "del" => Code::Delete,
                f => Code::F(f.strip_prefix('f')?.parse().ok().filter(|n| (1..=24).contains(n))?),
            },
        };
        Some(key.normalized())
    }

    /// A raw sequence that is a known key is that key (`\x1b[A` is `up`).
    fn normalized_raw(self) -> Key {
        match &self.code {
            Code::Raw(bytes) if !bytes.is_empty() => {
                let (len, key) = one(bytes);
                if len == bytes.len() { key } else { self }
            }
            _ => self,
        }
    }
}

/// Bytes as text: printable ASCII as is, the rest as `\xNN`.
fn escape(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|&b| if b.is_ascii_graphic() && b != b'\\' { (b as char).to_string() } else { format!("\\x{b:02x}") })
        .collect()
}

/// `\x1b`, `\e` and `\\` escapes to bytes.
fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'\\' && i + 1 < b.len() {
            match b[i + 1] {
                b'e' => {
                    out.push(0x1b);
                    i += 2;
                    continue;
                }
                b'\\' => {
                    out.push(b'\\');
                    i += 2;
                    continue;
                }
                b'x' => {
                    if let Some(v) = s.get(i + 2..i + 4).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                        out.push(v);
                        i += 4;
                        continue;
                    }
                }
                _ => {}
            }
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

/// Split an input chunk into `(bytes, key)` pairs.
pub fn split(input: &[u8]) -> Vec<(&[u8], Key)> {
    let mut out = vec![];
    let mut i = 0;
    while i < input.len() {
        let rest = &input[i..];
        let (len, key) = one(rest);
        let len = len.max(1).min(rest.len());
        out.push((&rest[..len], key));
        i += len;
    }
    out
}

/// The first key in `s` (not empty): its length in bytes, and which key it is.
fn one(s: &[u8]) -> (usize, Key) {
    let (len, key) = match s[0] {
        0x1b => escape_sequence(s),
        0x00 => (1, Key::ctrl(' ')),
        b'\t' => (1, Key::TAB),
        b'\r' => (1, Key::ENTER),
        0x7f => (1, Key::plain(Code::Backspace)),
        c @ 0x01..=0x1a => (1, Key::ctrl((b'a' + c - 1) as char)),
        c @ 0x1c..=0x1f => (1, Key::ctrl((b'\\' + c - 0x1c) as char)),
        _ => {
            let len = utf8_len(s).min(s.len());
            match std::str::from_utf8(&s[..len]).ok().and_then(|t| t.chars().next()) {
                Some(c) => (len, Key::plain(Code::Char(c))),
                None => (len, Key::plain(Code::Raw(s[..len].to_vec()))),
            }
        }
    };
    (len, key.normalized())
}

/// A key starting with ESC: Esc itself, a CSI/SS3 sequence, or Alt+key.
fn escape_sequence(s: &[u8]) -> (usize, Key) {
    if s.len() == 1 {
        return (1, Key::plain(Code::Esc));
    }
    match s[1] {
        b'[' => {
            if s[2..].starts_with(b"200~") {
                // Bracketed paste: one opaque chunk.
                let end = find(s, b"\x1b[201~").map_or(s.len(), |p| p + 6);
                return (end, Key::plain(Code::Raw(s[..end].to_vec())));
            }
            let Some(fin) = s[2..].iter().position(|b| (0x40..=0x7e).contains(b)) else {
                return (s.len(), Key::plain(Code::Raw(s.to_vec())));
            };
            let len = 2 + fin + 1;
            let key = csi(&s[2..2 + fin], s[2 + fin]).unwrap_or_else(|| Key::plain(Code::Raw(s[..len].to_vec())));
            (len, key)
        }
        b'O' if s.len() >= 3 => {
            let code = match s[2] {
                b'A' => Code::Up,
                b'B' => Code::Down,
                b'C' => Code::Right,
                b'D' => Code::Left,
                b'H' => Code::Home,
                b'F' => Code::End,
                c @ b'P'..=b'S' => Code::F(c - b'P' + 1),
                _ => Code::Raw(s[..3].to_vec()),
            };
            (3, Key::plain(code))
        }
        // Alt+key: ESC, then the key.
        _ => {
            let (len, mut key) = one(&s[1..]);
            key.alt = true;
            (1 + len, key)
        }
    }
}

/// A CSI sequence (`ESC [ params final`) as a key, if it is one.
fn csi(params: &[u8], fin: u8) -> Option<Key> {
    let params = std::str::from_utf8(params).ok()?;
    // Kitty sub-parameters (`9:1`) don't matter here.
    let nums: Vec<u32> = params.split(';').map(|p| p.split(':').next().unwrap_or("").parse().unwrap_or(1)).collect();
    let num = |i: usize| nums.get(i).copied().unwrap_or(1);
    let code = match fin {
        b'A' => Code::Up,
        b'B' => Code::Down,
        b'C' => Code::Right,
        b'D' => Code::Left,
        b'H' => Code::Home,
        b'F' => Code::End,
        b'P'..=b'S' => Code::F(fin - b'P' + 1),
        b'Z' => return Some(with_mods(Key::SHIFT_TAB, num(1))),
        // xterm's modifyOtherKeys: `27;mods;codepoint~`.
        b'~' if num(0) == 27 => codepoint(num(2))?,
        b'~' => match num(0) {
            1 | 7 => Code::Home,
            2 => Code::Insert,
            3 => Code::Delete,
            4 | 8 => Code::End,
            5 => Code::PageUp,
            6 => Code::PageDown,
            n @ 11..=15 => Code::F((n - 10) as u8),
            n @ 17..=21 => Code::F((n - 11) as u8),
            n @ 23..=26 => Code::F((n - 12) as u8),
            n @ 28..=29 => Code::F((n - 13) as u8),
            n @ 31..=34 => Code::F((n - 14) as u8),
            _ => return None,
        },
        // Kitty's keyboard protocol: `codepoint;mods u`.
        b'u' => codepoint(num(0))?,
        _ => return None,
    };
    Some(with_mods(Key::plain(code), num(1)))
}

/// The key a Unicode codepoint stands for in CSI u / modifyOtherKeys.
fn codepoint(n: u32) -> Option<Code> {
    Some(match n {
        9 => Code::Tab,
        13 => Code::Enter,
        27 => Code::Esc,
        127 | 8 => Code::Backspace,
        _ => Code::Char(char::from_u32(n).filter(|c| !c.is_control())?),
    })
}

/// Apply an xterm modifier parameter (1 + shift·1 + alt·2 + ctrl·4).
fn with_mods(mut key: Key, param: u32) -> Key {
    let m = param.saturating_sub(1);
    key.shift |= m & 1 != 0;
    key.alt |= m & 2 != 0;
    key.ctrl |= m & 4 != 0;
    key
}

/// Length of the UTF-8 character `s` starts with (1 if invalid).
fn utf8_len(s: &[u8]) -> usize {
    match s.first() {
        Some(b) if b & 0xe0 == 0xc0 => 2,
        Some(b) if b & 0xf0 == 0xe0 => 3,
        Some(b) if b & 0xf8 == 0xf0 => 4,
        _ => 1,
    }
}

/// Position of `n` in `h`.
fn find(h: &[u8], n: &[u8]) -> Option<usize> {
    h.windows(n.len()).position(|w| w == n)
}

/// What a key does while the popup is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Accept,
    Next,
    Prev,
    Dismiss,
    Toggle,
}

/// The `[keys]` config, resolved.
pub struct Bindings(Vec<(Key, Action)>);

impl Bindings {
    /// Unknown key names are reported and skipped.
    pub fn new(k: &crate::config::Keys) -> Self {
        let mut v = vec![];
        for (names, action) in [
            (&k.accept, Action::Accept),
            (&k.next, Action::Next),
            (&k.prev, Action::Prev),
            (&k.dismiss, Action::Dismiss),
            (&k.toggle, Action::Toggle),
        ] {
            for n in names {
                match Key::parse_name(n) {
                    Some(key) => v.push((key, action)),
                    None => eprintln!(
                        "lig: unknown key name {n:?} in config (like ctrl-n, alt-j, shift-up, f5; \
                         `lig keys` shows the name of any key)"
                    ),
                }
            }
        }
        Bindings(v)
    }

    /// The action bound to `key`, if any.
    pub fn action(&self, key: &Key) -> Option<Action> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, a)| *a)
    }
}

/// `lig keys`: the name of each key you press and what it's bound to, to
/// find what to write in the config. `q` or Ctrl-C quits.
pub fn show(bindings: &Bindings) -> std::io::Result<()> {
    use nix::sys::termios::{SetArg, cfmakeraw, tcgetattr, tcsetattr};
    let stdin = std::io::stdin();
    let orig = tcgetattr(&stdin)?;
    let mut raw = orig.clone();
    cfmakeraw(&mut raw);
    tcsetattr(&stdin, SetArg::TCSANOW, &raw)?;
    let mut out = std::io::stdout();
    write!(out, "Press keys to see their names for the [keys] config (q or Ctrl-C quits).\r\n\r\n")?;
    write!(out, "  {:<18} {:<20} bound to\r\n", "terminal sends", "name")?;
    out.flush()?;
    let mut buf = [0u8; 256];
    'read: loop {
        let n = stdin.lock().read(&mut buf)?;
        if n == 0 {
            break;
        }
        for (bytes, key) in split(&buf[..n]) {
            if key == Key::plain(Code::Char('q')) || key == Key::ctrl('c') {
                break 'read;
            }
            let bound = bindings.action(&key).map_or("-".to_string(), |a| format!("{a:?}").to_lowercase());
            write!(out, "  {:<18} {:<20} {bound}\r\n", escape(bytes), key.name())?;
            out.flush()?;
        }
    }
    tcsetattr(&stdin, SetArg::TCSANOW, &orig)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(input: &[u8]) -> Vec<String> {
        split(input).into_iter().map(|(_, k)| k.name()).collect()
    }

    #[test]
    fn splits() {
        assert_eq!(
            keys(b"a\x1b[A\x1bOB\t\x1b[Z\x00\x1b"),
            ["a", "up", "down", "tab", "shift-tab", "ctrl-space", "esc"]
        );
        assert_eq!(keys("é\x1bx\x0e\x7f".as_bytes()), ["é", "alt-x", "ctrl-n", "backspace"]);
        assert_eq!(
            keys(b"\x1b[1;5C\x1b[1;2A\x1b[6~\x1b[15~\x1bOP\x1b[3;3~\x1b\x18"),
            ["ctrl-right", "shift-up", "pagedown", "f5", "f1", "alt-delete", "ctrl-alt-x"]
        );
        // Kitty's protocol and xterm's modifyOtherKeys.
        assert_eq!(keys(b"\x1b[9;5u\x1b[27;5;9~\x1b[97;7u\x1b[65;2u"), ["ctrl-tab", "ctrl-tab", "ctrl-alt-a", "A"]);
        assert_eq!(keys(b"\r\n\x1b[106;5u\x1b[109;5u"), ["enter", "ctrl-j", "ctrl-j", "enter"]);
        // Unknown sequences keep their bytes.
        assert_eq!(keys(b"\x1b[99;3x"), ["\\x1b[99;3x"]);
    }

    #[test]
    fn names() {
        let k = |n: &str| Key::parse_name(n).unwrap();
        assert_eq!(k("ctrl-space"), k("ctrl-@"));
        assert_eq!(k("Ctrl+A"), k("ctrl-a"));
        assert_eq!(k("shift-a"), k("A"));
        assert_eq!(k("backtab"), Key::SHIFT_TAB);
        assert_eq!(k("ctrl-i"), Key::TAB);
        assert_eq!(k("ctrl-m"), Key::ENTER);
        assert_ne!(k("ctrl-j"), Key::ENTER);
        assert_eq!(k("meta-shift-page-up"), split(b"\x1b[5;4~")[0].1);
        assert_eq!(k("ctrl--"), k("ctrl-minus"));
        assert_eq!(k("\\x1b[A"), Key::UP);
        assert_eq!(k("\\e[99;3x"), split(b"\x1b[99;3x")[0].1);
        assert!(Key::parse_name("ctrl-nope").is_none() && Key::parse_name("f99").is_none());
        // Every name `lig keys` prints reads back as the same key.
        let input = b"\x1b[1;5C\x1b[5;4~\x1b[24~\x1b\x18\x00\x1b[9;5u\x1b[99;3x x?-";
        for (_, key) in split(input) {
            assert_eq!(Key::parse_name(&key.name()), Some(key.clone()), "{}", key.name());
        }
    }
}
