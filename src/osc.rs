//! Extracts lig's private OSC 6973 messages from the shell's output stream.
//! The shell integration writes `ESC ] 6973 ; <kind> [; payload] BEL`:
//!   `R[;<pid>]`            fish signal handler ready, reader active
//!   `Q;<id>;<B;…|H>`       reply to a fish snapshot request
//!   `B;<cursor>;<buffer>`  edit buffer changed (zsh/fish)
//!   `S`                    the prompt ends here; input starts (bash)
//!   `J`                    fish rejected an insertion for an old buffer
//!   `H`                    hide (isearch, vi command mode, …)
//!   `F`                    line finished (command about to run)
//!   `P;<PATH>`             the shell's PATH
//!   `C;<cwd>`              the shell's working directory
//!   `A;<k=v>\x1e<k=v>…`    the shell's aliases
//!   `N;<name>\x1e<name>…`  the shell's function names
//!   `K;<name>\x1e<name>…`  the shell's builtins

const PREFIX: &[u8] = b"\x1b]6973;";
/// An unterminated message longer than this isn't ours: pass it through.
const MAX_PENDING: usize = 1 << 20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Msg {
    /// Fish's signal handler is installed; `pid` is the fish process to signal.
    Ready {
        pid: Option<u32>,
    },
    InsertRejected,
    Snapshot {
        id: u64,
        buffer: Option<(usize, String)>,
    },
    Buffer {
        cursor: usize,
        text: String,
    },
    PromptEnd,
    Hide,
    Finish,
    Path(String),
    Cwd(String),
    Aliases(Vec<(String, String)>),
    Functions(Vec<String>),
    Builtins(Vec<String>),
}

/// Removes our messages from the output; keeps state across reads, since a
/// message can be split between two of them.
#[derive(Default)]
pub struct OscFilter {
    pending: Vec<u8>,
}

impl OscFilter {
    /// Bytes held back waiting for the rest of a possible message.
    pub fn take_pending(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.pending)
    }

    /// Append the bytes meant for the terminal to `out`, and our messages to
    /// `msgs`, each with its position in `out`.
    pub fn feed(&mut self, data: &[u8], out: &mut Vec<u8>, msgs: &mut Vec<(usize, Msg)>) {
        let mut buf = std::mem::take(&mut self.pending);
        buf.extend_from_slice(data);
        let mut i = 0;
        while i < buf.len() {
            let Some(off) = buf[i..].iter().position(|&b| b == 0x1b) else {
                out.extend_from_slice(&buf[i..]);
                break;
            };
            let j = i + off;
            out.extend_from_slice(&buf[i..j]);
            let rest = &buf[j..];
            if rest.len() < PREFIX.len() && PREFIX.starts_with(rest) {
                self.pending = rest.to_vec();
                return;
            }
            if !rest.starts_with(PREFIX) {
                out.push(0x1b);
                i = j + 1;
                continue;
            }
            let body = &rest[PREFIX.len()..];
            match body.iter().position(|&b| b == 0x07 || b == 0x1b) {
                None if rest.len() < MAX_PENDING => {
                    self.pending = rest.to_vec();
                    return;
                }
                None => break,
                Some(k) => {
                    if let Some(m) = parse(&body[..k]) {
                        msgs.push((out.len(), m));
                    }
                    // A stray ESC ends the sequence but stays in the stream.
                    i = j + PREFIX.len() + k + usize::from(body[k] == 0x07);
                }
            }
        }
    }
}

/// A message from its payload (`<kind>;<rest>`); unknown kinds are ignored.
fn parse(payload: &[u8]) -> Option<Msg> {
    let s = String::from_utf8_lossy(payload);
    let (kind, rest) = s.split_once(';').unwrap_or((&s, ""));
    match kind {
        "R" => Some(Msg::Ready { pid: rest.parse().ok() }),
        "J" => Some(Msg::InsertRejected),
        "Q" => {
            let (id, state) = rest.split_once(';')?;
            let buffer = if state == "H" {
                None
            } else {
                let (cursor, text) = state.strip_prefix("B;")?.split_once(';')?;
                Some((cursor.parse().ok()?, text.to_string()))
            };
            Some(Msg::Snapshot { id: id.parse().ok()?, buffer })
        }
        "B" => {
            let (cursor, text) = rest.split_once(';')?;
            Some(Msg::Buffer { cursor: cursor.parse().ok()?, text: text.to_string() })
        }
        "H" => Some(Msg::Hide),
        "S" => Some(Msg::PromptEnd),
        "F" => Some(Msg::Finish),
        "P" => Some(Msg::Path(rest.to_string())),
        "C" => Some(Msg::Cwd(rest.to_string())),
        "A" => Some(Msg::Aliases(
            rest.split('\x1e').filter_map(|p| p.split_once('=')).map(|(k, v)| (k.to_string(), v.to_string())).collect(),
        )),
        "N" => Some(Msg::Functions(rest.split('\x1e').filter(|f| !f.is_empty()).map(str::to_string).collect())),
        "K" => Some(Msg::Builtins(rest.split('\x1e').filter(|f| !f.is_empty()).map(str::to_string).collect())),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_replies_have_ids_and_only_buffer_or_hide_state() {
        assert_eq!(
            parse("Q;7;B;6;café;🙂\n".as_bytes()),
            Some(Msg::Snapshot { id: 7, buffer: Some((6, "café;🙂\n".into())) })
        );
        assert_eq!(parse(b"Q;8;H"), Some(Msg::Snapshot { id: 8, buffer: None }));
        for invalid in [b"Q;bad;H".as_slice(), b"Q;9;Q;10;H", b"Q;9;R", b"Q;9;B;bad;text"] {
            assert_eq!(parse(invalid), None);
        }
    }

    #[test]
    fn fish_ready_unicode_and_hide() {
        let mut f = OscFilter::default();
        let (mut out, mut msgs) = (vec![], vec![]);
        for byte in "\x1b]6973;R\x07\x1b]6973;B;6;café;🙂\n\x07\x1b]6973;H\x07".as_bytes() {
            f.feed(&[*byte], &mut out, &mut msgs);
        }
        assert!(out.is_empty());
        assert_eq!(
            msgs.into_iter().map(|(_, m)| m).collect::<Vec<_>>(),
            vec![Msg::Ready { pid: None }, Msg::Buffer { cursor: 6, text: "café;🙂\n".into() }, Msg::Hide]
        );
        assert_eq!(parse(b"R;4242"), Some(Msg::Ready { pid: Some(4242) }));
    }

    #[test]
    fn extracts_and_passes_through() {
        let mut f = OscFilter::default();
        let (mut out, mut msgs) = (vec![], vec![]);
        f.feed(b"a\x1b[1mb\x1b]6973;B;3;git st\x07c\x1b]0;title\x07", &mut out, &mut msgs);
        assert_eq!(out, b"a\x1b[1mbc\x1b]0;title\x07");
        assert_eq!(msgs, vec![(6, Msg::Buffer { cursor: 3, text: "git st".into() })]);
        let (mut out, mut msgs) = (vec![], vec![]);
        f.feed(b"$ \x1b]6973;S\x07ls", &mut out, &mut msgs);
        assert_eq!((out.as_slice(), msgs), (b"$ ls".as_slice(), vec![(2, Msg::PromptEnd)]));
    }

    #[test]
    fn split_across_chunks() {
        let mut f = OscFilter::default();
        let (mut out, mut msgs) = (vec![], vec![]);
        let data = b"x\x1b]6973;B;1;a;b\x07y\x1b]6973;F\x07";
        for b in data {
            f.feed(std::slice::from_ref(b), &mut out, &mut msgs);
        }
        assert_eq!(out, b"xy");
        let msgs: Vec<Msg> = msgs.into_iter().map(|m| m.1).collect();
        assert_eq!(msgs, vec![Msg::Buffer { cursor: 1, text: "a;b".into() }, Msg::Finish]);
    }

    #[test]
    fn aliases_and_functions() {
        let mut f = OscFilter::default();
        let (mut out, mut msgs) = (vec![], vec![]);
        f.feed("\x1b]6973;A;gco=git checkout\x1ell=ls -l\x07\x1b]6973;N;z\x1ezi\x07".as_bytes(), &mut out, &mut msgs);
        let msgs: Vec<Msg> = msgs.into_iter().map(|m| m.1).collect();
        assert_eq!(
            msgs,
            vec![
                Msg::Aliases(vec![("gco".into(), "git checkout".into()), ("ll".into(), "ls -l".into())]),
                Msg::Functions(vec!["z".into(), "zi".into()]),
            ]
        );
    }

    #[test]
    fn lone_escape_at_end_is_held() {
        let mut f = OscFilter::default();
        let (mut out, mut msgs) = (vec![], vec![]);
        f.feed(b"q\x1b", &mut out, &mut msgs);
        assert_eq!(out, b"q");
        f.feed(b"[A", &mut out, &mut msgs);
        assert_eq!(out, b"q\x1b[A");
    }
}
