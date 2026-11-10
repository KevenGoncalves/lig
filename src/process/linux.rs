use std::path::{Path, PathBuf};

pub fn cwd(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
}
pub fn executable(pid: u32) -> Option<PathBuf> {
    std::fs::read_link(format!("/proc/{pid}/exe")).ok()
}

/// Whether a non-shell program (fzf via Ctrl+T, a pager…) uses the terminal while a line is edited.
/// Widgets run in the shell's process group, so the foreground group can't tell: look for a non-shell
/// descendant in that group with the tty open beyond stderr (prompt helpers like `$(git …)` only have fd 2).
pub fn foreign_program(shell: u32, tty: &Path) -> bool {
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

pub fn children(pid: u32) -> Vec<u32> {
    std::fs::read_dir(format!("/proc/{pid}/task"))
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|t| std::fs::read_to_string(t.path().join("children")).ok())
        .flat_map(|s| s.split_whitespace().filter_map(|p| p.parse().ok()).collect::<Vec<u32>>())
        .collect()
}
