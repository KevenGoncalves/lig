//! Shared `/proc/<pid>/stat` parsing.

/// The contents of `/proc/<pid>/stat`.
pub fn stat(pid: u32) -> Option<String> {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()
}

/// The whitespace-split fields after the last `)` (the comm may hold spaces
/// and parentheses): index 0 is stat field 3 (state), so field N is index N-3.
pub fn stat_fields(text: &str) -> Option<Vec<&str>> {
    let (_, rest) = text.rsplit_once(')')?;
    Some(rest.split_whitespace().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_after_comm_with_parens_and_spaces() {
        let t = "42 (my (odd) prog) S 1 42 42 34816 42 4194304";
        let f = stat_fields(t).unwrap();
        assert_eq!(f[0], "S");
        assert_eq!(f[2], "42");
        assert_eq!(f[5], "42"); // pgrp, tpgid
    }

    #[test]
    fn fields_none_without_paren() {
        assert!(stat_fields("garbage").is_none());
    }

    #[test]
    fn stat_matches_old_closure() {
        // Oracle: foreign_program's closure as of 0909f22 (src/session.rs:365-371), copied verbatim
        // but taking the text instead of reading the pid.
        let old = |s: String| -> Option<(String, i64)> {
            let (head, rest) = s.rsplit_once(')')?;
            let comm = head.split_once('(')?.1.to_string();
            let pgrp = rest.split_whitespace().nth(2)?.parse().ok()?;
            Some((comm, pgrp))
        };
        let new = |s: &str| -> Option<(String, i64)> {
            let fields = stat_fields(s)?;
            let comm = s[..s.rfind(')')?].split_once('(')?.1.to_string();
            Some((comm, fields.get(2)?.parse().ok()?))
        };
        let own = std::fs::read_to_string("/proc/self/stat").unwrap();
        for t in [own.as_str(), "42 (my (odd) prog) S 1 42 42 34816 42 4194304", "garbage"] {
            assert_eq!(old(t.to_string()), new(t), "{t}");
        }
        assert!(new("garbage").is_none());
    }
}
