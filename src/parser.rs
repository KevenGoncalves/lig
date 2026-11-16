//! Shell tokenizer: splits the text left of the cursor into the tokens of the last simple
//! command (after the final `|`, `&&`, `;`, `$(` …), keeping both raw and unquoted text.

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Token {
    /// Exactly what the user typed, quotes and backslashes included.
    pub raw: String,
    /// The value after shell unquoting.
    pub value: String,
    /// Quote character the token is currently inside of (unterminated), if any.
    pub open_quote: Option<char>,
    /// The token is the target of a redirection (`> file`, `< file`).
    pub redirect: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CommandLine {
    /// Completed tokens of the current command (env assignments stripped).
    pub tokens: Vec<Token>,
    /// The token under the cursor (may be empty when the line ends in a space).
    pub current: Token,
}

impl CommandLine {
    /// All token values, the current one last.
    pub fn values(&self) -> Vec<String> {
        self.tokens.iter().chain(std::iter::once(&self.current)).map(|t| t.value.clone()).collect()
    }
}

/// The escape character: `\` in POSIX shells; on Windows `\` separates paths and PowerShell
/// escapes with a backtick (cmd's `^` is rare enough to treat as text).
const ESCAPE: char = if cfg!(windows) { '`' } else { '\\' };

/// A `$(`, `` ` ``, `<(` or `(` not closed yet: what the line goes back to when it is.
struct Open {
    tokens: Vec<Token>,
    cur: Token,
    in_token: bool,
    next_redirect: bool,
    quote: Option<char>,
    /// `$(…)`, `` `…` `` and `<(…)` are part of a word; `(…)` is a command of its own.
    word: bool,
    backtick: bool,
    /// Where it starts in the line, for the word's text.
    start: usize,
}

#[allow(unused_assignments)]
pub fn parse(line: &str) -> CommandLine {
    let chars: Vec<char> = line.chars().collect();
    let mut tokens: Vec<Token> = Vec::new();
    let mut cur = Token::default();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut escape = false;
    let mut next_redirect = false;
    let mut open: Vec<Open> = Vec::new();
    let mut i = 0;

    macro_rules! finish {
        () => {
            if in_token {
                cur.redirect = next_redirect;
                next_redirect = false;
                tokens.push(std::mem::take(&mut cur));
                in_token = false;
            }
        };
    }
    macro_rules! enter {
        ($word:expr, $backtick:expr, $start:expr) => {
            open.push(Open {
                tokens: std::mem::take(&mut tokens),
                cur: std::mem::take(&mut cur),
                in_token,
                next_redirect,
                quote,
                word: $word,
                backtick: $backtick,
                start: $start,
            });
            in_token = false;
            next_redirect = false;
            quote = None;
        };
    }
    macro_rules! leave {
        () => {
            let o = open.pop().unwrap();
            tokens = o.tokens;
            cur = o.cur;
            in_token = o.in_token;
            next_redirect = o.next_redirect;
            quote = o.quote;
            if o.word {
                let text: String = chars[o.start..i].iter().collect();
                cur.raw.push_str(&text);
                cur.value.push_str(&text);
                in_token = true;
            }
        };
    }

    while i < chars.len() {
        let c = chars[i];
        i += 1;
        let next = chars.get(i).copied();
        if escape {
            escape = false;
            cur.raw.push(c);
            if c != '\n' {
                cur.value.push(c);
            }
            in_token = true;
            continue;
        }
        match quote {
            Some('\'') => {
                cur.raw.push(c);
                if c == '\'' {
                    quote = None;
                } else {
                    cur.value.push(c);
                }
                continue;
            }
            Some('"') => {
                if c == '$' && next == Some('(') {
                    i += 1;
                    enter!(true, false, i - 2);
                } else if c == '`' && ESCAPE != '`' {
                    enter!(true, true, i - 1);
                } else if c == ESCAPE
                    && let Some(n @ ('"' | '\\' | '$' | '`')) = next
                {
                    i += 1;
                    cur.raw.extend([c, n]);
                    cur.value.push(n);
                } else {
                    cur.raw.push(c);
                    if c == '"' {
                        quote = None;
                    } else {
                        cur.value.push(c);
                    }
                }
                continue;
            }
            _ => {}
        }
        match c {
            c if c == ESCAPE => {
                cur.raw.push(c);
                escape = true;
                in_token = true;
            }
            '\'' | '"' => {
                cur.raw.push(c);
                quote = Some(c);
                in_token = true;
            }
            ' ' | '\t' => finish!(),
            // `&>file`: a redirection, not `&`.
            '&' if next == Some('>') => {
                finish!();
                while matches!(chars.get(i), Some('>' | '|')) {
                    i += 1;
                }
                next_redirect = true;
            }
            '$' | '<' | '>' if next == Some('(') => {
                i += 1;
                enter!(true, false, i - 2);
            }
            // On Windows the backtick is the escape character, matched above.
            '`' if open.last().is_some_and(|o| o.backtick) => {
                leave!();
            }
            '`' => {
                enter!(true, true, i - 1);
            }
            ')' if open.last().is_some_and(|o| !o.backtick) => {
                leave!();
            }
            '(' => {
                finish!();
                enter!(false, false, i - 1);
            }
            '\n' | ';' | '|' | '&' | ')' => {
                finish!();
                tokens.clear();
                next_redirect = false;
            }
            '<' | '>' => {
                // `2>`: drop a pure-digit fd prefix.
                if in_token && cur.raw.chars().all(|d| d.is_ascii_digit()) {
                    cur = Token::default();
                    in_token = false;
                }
                finish!();
                while matches!(chars.get(i), Some('>' | '<' | '&' | '|')) {
                    i += 1;
                }
                next_redirect = true;
            }
            _ => {
                cur.raw.push(c);
                cur.value.push(c);
                in_token = true;
            }
        }
    }
    cur.open_quote = quote;
    cur.redirect = next_redirect;

    // Redirection targets are not command arguments.
    tokens.retain(|t| !t.redirect);
    // Leading `VAR=value` assignments.
    let skip = tokens.iter().take_while(|t| is_assignment(&t.value)).count();
    tokens.drain(..skip);

    CommandLine { tokens, current: cur }
}

fn is_assignment(s: &str) -> bool {
    match s.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
                && !name.starts_with(|c: char| c.is_ascii_digit())
        }
        None => false,
    }
}

/// Windows shells take a word with spaces or special characters in double quotes.
fn quote_windows(value: &str, special: &str) -> String {
    if value.contains(|c: char| c.is_whitespace() || special.contains(c)) {
        format!("\"{value}\"")
    } else {
        value.to_string()
    }
}

/// Escape a value for insertion into an unquoted shell word.
pub fn escape(value: &str) -> String {
    if cfg!(windows) {
        return quote_windows(value, "'&|;<>()[]{}`$#,@^");
    }
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if " \t\n'\"\\$`&|;<>()[]{}*?!#~".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Escape only characters that would split or terminate the word.
pub fn escape_minimal(value: &str) -> String {
    if cfg!(windows) {
        return quote_windows(value, "&|;<>()`");
    }
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        if " \t\n'\"\\$`&|;<>()".contains(c) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    #[test]
    fn windows_paths_and_quoting() {
        assert_eq!(parse(r"cd C:\Users\me\Doc").current.value, r"C:\Users\me\Doc");
        let cl = parse(r#"cat "my file" a` b"#);
        assert_eq!(vals(&cl), ["cat", "my file"]);
        assert_eq!(cl.current.value, "a b");
        assert_eq!(escape("My Files"), "\"My Files\"");
        assert_eq!(escape(r"src\app"), r"src\app");
    }

    fn vals(cl: &CommandLine) -> Vec<&str> {
        cl.tokens.iter().map(|t| t.value.as_str()).collect()
    }

    #[test]
    fn simple() {
        let cl = parse("git checkout ma");
        assert_eq!(vals(&cl), ["git", "checkout"]);
        assert_eq!(cl.current.value, "ma");
    }

    #[test]
    fn trailing_space() {
        let cl = parse("git ");
        assert_eq!(vals(&cl), ["git"]);
        assert_eq!(cl.current.raw, "");
    }

    // POSIX paths and quoting; Windows has its own tests.
    #[cfg(unix)]
    #[test]
    fn quotes_and_escapes() {
        let cl = parse(r#"cat "my file" a\ b 'c d"#);
        assert_eq!(vals(&cl), ["cat", "my file", "a b"]);
        assert_eq!(cl.current.value, "c d");
        assert_eq!(cl.current.raw, "'c d");
        assert_eq!(cl.current.open_quote, Some('\''));
    }

    #[test]
    fn separators() {
        let cl = parse("cd foo && git co");
        assert_eq!(vals(&cl), ["git"]);
        let cl = parse("ls | grep -");
        assert_eq!(vals(&cl), ["grep"]);
        assert_eq!(cl.current.value, "-");
        let cl = parse("echo $(git br");
        assert_eq!(vals(&cl), ["git"]);
        let cl = parse(r#"git commit -m "$(git log -1 --form"#);
        assert_eq!(vals(&cl), ["git", "log", "-1"]);
        let cl = parse("diff <(sort a) <(sort -");
        assert_eq!(vals(&cl), ["sort"]);
        let cl = parse("echo `git br");
        assert_eq!(vals(&cl), ["git"]);
        let cl = parse("(cd src && make t");
        assert_eq!(vals(&cl), ["make"]);
    }

    #[test]
    fn a_closed_substitution_is_part_of_its_word() {
        let cl = parse("ls $(pwd)/src -");
        assert_eq!(vals(&cl), ["ls", "$(pwd)/src"]);
        assert_eq!(cl.current.value, "-");
        let cl = parse(r#"git commit -m "$(date) done" --am"#);
        assert_eq!(vals(&cl), ["git", "commit", "-m", "$(date) done"]);
        assert_eq!(cl.current.value, "--am");
        let cl = parse("echo `hostname` $(echo $(id -u)) -");
        assert_eq!(vals(&cl), ["echo", "`hostname`", "$(echo $(id -u))"]);
        let cl = parse("diff <(sort a) -");
        assert_eq!(vals(&cl), ["diff", "<(sort a)"]);
        let cl = parse("(cd src) && git a");
        assert_eq!(vals(&cl), ["git"]);
        let cl = parse("a=(x y) git c");
        assert_eq!(vals(&cl), ["git"]);
    }

    #[test]
    fn env_and_redirects() {
        let cl = parse("FOO=1 BAR=2 make > out.txt ins");
        assert_eq!(vals(&cl), ["make"]);
        assert_eq!(cl.current.value, "ins");
        let cl = parse("sort < fi");
        assert!(cl.current.redirect);
        let cl = parse("cmd 2>/dev/null x");
        assert_eq!(vals(&cl), ["cmd"]);
        let cl = parse("make &> log ins");
        assert_eq!(vals(&cl), ["make"]);
        assert!(!cl.current.redirect);
        assert!(parse("make &>lo").current.redirect);
    }

    // POSIX paths and quoting; Windows has its own tests.
    #[cfg(unix)]
    #[test]
    fn escape_roundtrip() {
        assert_eq!(escape("a b(1).txt"), r"a\ b\(1\).txt");
    }
}
