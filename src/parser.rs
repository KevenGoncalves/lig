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

#[allow(unused_assignments)]
pub fn parse(line: &str) -> CommandLine {
    let mut tokens: Vec<Token> = Vec::new();
    let mut cur = Token::default();
    let mut in_token = false;
    let mut quote: Option<char> = None;
    let mut escape = false;
    let mut next_redirect = false;
    let mut chars = line.chars().peekable();

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

    while let Some(c) = chars.next() {
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
                cur.raw.push(c);
                if c == '"' {
                    quote = None;
                } else if c == '\\' && matches!(chars.peek(), Some('"' | '\\' | '$' | '`')) {
                    let n = chars.next().unwrap();
                    cur.raw.push(n);
                    cur.value.push(n);
                } else {
                    cur.value.push(c);
                }
                continue;
            }
            _ => {}
        }
        match c {
            '\\' => {
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
            '\n' | ';' | '|' | '&' | '(' | ')' | '`' => {
                finish!();
                // A command separator: start over with a fresh command.
                tokens.clear();
                next_redirect = false;
            }
            '<' | '>' => {
                // `2>` / `&>`: drop a pure-digit fd prefix.
                if in_token && cur.raw.chars().all(|d| d.is_ascii_digit()) {
                    cur = Token::default();
                    in_token = false;
                }
                finish!();
                while matches!(chars.peek(), Some('>' | '<' | '&' | '|')) {
                    chars.next();
                }
                next_redirect = true;
            }
            '$' if chars.peek() == Some(&'(') => {
                chars.next();
                finish!();
                tokens.clear();
                next_redirect = false;
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

/// Escape a value for insertion into an unquoted shell word.
pub fn escape(value: &str) -> String {
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
    }

    #[test]
    fn escape_roundtrip() {
        assert_eq!(escape("a b(1).txt"), r"a\ b\(1\).txt");
    }
}
