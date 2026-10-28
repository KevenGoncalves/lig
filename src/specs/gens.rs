//! Generators shared by the built-in specs.

use std::path::{Path, PathBuf};

use crate::spec::{GenCtx, Generator, Icon, Kind, SuggestionDef, Trigger, gen_fn, sug};

// ---------------------------------------------------------------- git

/// Local and remote branches, most recently committed first.
pub fn git_branches() -> Generator {
    gen_fn("git:branches", |cx| {
        let out = cx.lines(&[
            "git",
            "--no-optional-locks",
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)%09%(subject)",
            "refs/heads",
            "refs/remotes",
        ]);
        out.iter()
            .filter_map(|l| {
                let (r, subject) = l.split_once('\t').unwrap_or((l, ""));
                if let Some(b) = r.strip_prefix("refs/heads/") {
                    Some(sug(b).desc(subject).priority(60.0))
                } else {
                    let b = r.strip_prefix("refs/remotes/")?;
                    (!b.ends_with("/HEAD")).then(|| sug(b).desc(subject).priority(40.0))
                }
            })
            .collect()
    })
}

pub fn git_local_branches() -> Generator {
    gen_fn("git:local-branches", |cx| {
        cx.lines(&[
            "git",
            "--no-optional-locks",
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname:short)%09%(subject)",
            "refs/heads",
        ])
        .iter()
        .map(|l| {
            let (b, s) = l.split_once('\t').unwrap_or((l, ""));
            sug(b).desc(s)
        })
        .collect()
    })
}

pub fn git_remotes() -> Generator {
    gen_fn("git:remotes", |cx| {
        cx.lines(&["git", "remote", "-v"])
            .iter()
            .filter_map(|l| {
                let mut p = l.split_whitespace();
                let (name, url) = (p.next()?, p.next().unwrap_or(""));
                l.ends_with("(fetch)").then(|| sug(name).desc(url))
            })
            .collect()
    })
}

pub fn git_tags() -> Generator {
    gen_fn("git:tags", |cx| {
        cx.lines(&["git", "tag", "--sort=-creatordate"]).into_iter().map(|t| sug(t).desc("tag")).collect()
    })
}

pub fn git_commits() -> Generator {
    gen_fn("git:commits", |cx| {
        cx.lines(&["git", "--no-optional-locks", "log", "-n", "100", "--format=%h%x09%s%x09%cr"])
            .iter()
            .map(|l| {
                let mut p = l.splitn(3, '\t');
                let (h, s, when) = (p.next().unwrap_or(""), p.next().unwrap_or(""), p.next().unwrap_or(""));
                sug(h).desc(format!("{s} ({when})"))
            })
            .collect()
    })
}

pub fn git_stashes() -> Generator {
    gen_fn("git:stashes", |cx| {
        cx.lines(&["git", "stash", "list", "--format=%gd%x09%s"])
            .iter()
            .map(|l| {
                let (n, s) = l.split_once('\t').unwrap_or((l, ""));
                sug(n).desc(s)
            })
            .collect()
    })
}

/// Which changed files a git command is interested in.
#[derive(Clone, Copy)]
pub enum Changes {
    /// Modified in the worktree or untracked (`git add`).
    Unstaged,
    /// In the index (`git restore --staged`, `git reset`).
    Staged,
    /// Modified tracked files (`git restore`, `git diff`, `git checkout --`).
    Modified,
}

/// Files from `git status`, as paths relative to the shell's directory.
pub fn git_changed(which: Changes) -> Generator {
    let id = match which {
        Changes::Unstaged => "git:unstaged",
        Changes::Staged => "git:staged",
        Changes::Modified => "git:modified",
    };
    gen_fn(id, move |cx| {
        let prefix = cx.run(&["git", "rev-parse", "--show-prefix"]).unwrap_or_default();
        let prefix = prefix.trim();
        let Some(out) =
            cx.run(&["git", "--no-optional-locks", "-c", "core.quotePath=false", "status", "--porcelain", "-uall"])
        else {
            return vec![];
        };
        let mut v = vec![];
        for l in out.lines() {
            if l.len() < 4 {
                continue;
            }
            let (x, y) = (l.as_bytes()[0], l.as_bytes()[1]);
            let keep = match which {
                Changes::Unstaged => y != b' ' || x == b'?',
                Changes::Staged => x != b' ' && x != b'?',
                Changes::Modified => y != b' ' && x != b'?',
            };
            if !keep {
                continue;
            }
            let mut path = &l[3..];
            if let Some((_, new)) = path.split_once(" -> ") {
                path = new;
            }
            let path = path.trim_matches('"');
            let state = match (x, y) {
                (b'?', _) => "untracked",
                (_, b'M') | (b'M', _) => "modified",
                (_, b'D') | (b'D', _) => "deleted",
                (b'A', _) => "added",
                (b'R', _) => "renamed",
                (b'U', _) | (_, b'U') => "conflict",
                _ => "changed",
            };
            v.push(sug(relative_to(path, prefix)).desc(state).kind(Kind::File).priority(55.0));
        }
        v
    })
}

/// `path` is relative to the repo root; `prefix` is the cwd relative to it.
fn relative_to(path: &str, prefix: &str) -> String {
    if let Some(p) = path.strip_prefix(prefix) {
        return p.to_string();
    }
    let up = prefix.trim_end_matches('/').split('/').filter(|s| !s.is_empty()).count();
    format!("{}{path}", "../".repeat(up))
}

// ---------------------------------------------------------------- docker

pub fn docker_containers(all: bool) -> Generator {
    let id = if all { "docker:containers" } else { "docker:running" };
    gen_fn(id, move |cx| {
        let mut argv = vec!["docker", "ps", "--format", "{{.Names}}\t{{.Image}}\t{{.Status}}"];
        if all {
            argv.push("-a");
        }
        cx.lines(&argv)
            .iter()
            .map(|l| {
                let p: Vec<&str> = l.split('\t').collect();
                sug(p[0]).desc(format!("{} — {}", p.get(1).unwrap_or(&""), p.get(2).unwrap_or(&"")))
            })
            .collect()
    })
}

pub fn docker_images() -> Generator {
    gen_fn("docker:images", |cx| {
        cx.lines(&["docker", "images", "--format", "{{.Repository}}:{{.Tag}}\t{{.Size}}"])
            .iter()
            .filter(|l| !l.starts_with("<none>"))
            .map(|l| {
                let (n, s) = l.split_once('\t').unwrap_or((l, ""));
                sug(n).desc(s)
            })
            .collect()
    })
}

pub fn docker_list(id: &'static str, what: &'static str) -> Generator {
    gen_fn(id, move |cx| cx.lines(&["docker", what, "ls", "--format", "{{.Name}}"]).into_iter().map(sug).collect())
}

pub fn compose_services() -> Generator {
    gen_fn("docker:compose-services", |cx| {
        cx.lines(&["docker", "compose", "config", "--services"]).into_iter().map(|s| sug(s).desc("service")).collect()
    })
}

// ---------------------------------------------------------------- files in the project

/// Walk up from `cwd` to find `name`.
pub fn find_up(cwd: &Path, name: &str) -> Option<PathBuf> {
    cwd.ancestors().map(|d| d.join(name)).find(|p| p.is_file())
}

/// `package.json` scripts.
pub fn npm_scripts() -> Generator {
    gen_fn("npm:scripts", |cx| {
        let Some(pj) = find_up(cx.cwd, "package.json") else { return vec![] };
        let Ok(v) =
            std::fs::read_to_string(pj).map(|t| serde_json::from_str::<serde_json::Value>(&t).unwrap_or_default())
        else {
            return vec![];
        };
        v["scripts"]
            .as_object()
            .map(|o| o.iter().map(|(k, c)| sug(k.clone()).desc(c.as_str().unwrap_or("")).priority(60.0)).collect())
            .unwrap_or_default()
    })
}

/// `package.json` dependencies.
pub fn npm_deps() -> Generator {
    gen_fn("npm:deps", |cx| {
        let Some(pj) = find_up(cx.cwd, "package.json") else { return vec![] };
        let v: serde_json::Value =
            std::fs::read_to_string(pj).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        let mut out = vec![];
        for (section, label) in [
            ("dependencies", "dependency"),
            ("devDependencies", "dev dependency"),
            ("peerDependencies", "peer dependency"),
        ] {
            if let Some(o) = v[section].as_object() {
                out.extend(
                    o.iter().map(|(k, ver)| sug(k.clone()).desc(format!("{label} {}", ver.as_str().unwrap_or("")))),
                );
            }
        }
        out
    })
}

/// Cargo targets of a kind (`bin`, `example`, `test`, `bench`) from the
/// conventional directories and `[[bin]]`-style tables.
pub fn cargo_targets(kind: &'static str) -> Generator {
    gen_fn(&format!("cargo:{kind}"), move |cx| {
        let Some(manifest) = find_up(cx.cwd, "Cargo.toml") else { return vec![] };
        let root = manifest.parent().unwrap_or(cx.cwd);
        let dir = match kind {
            "bin" => "src/bin",
            "example" => "examples",
            "test" => "tests",
            _ => "benches",
        };
        let mut names: Vec<String> = std::fs::read_dir(root.join(dir))
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let p = e.path();
                if p.extension().is_some_and(|x| x == "rs") {
                    p.file_stem().map(|s| s.to_string_lossy().into_owned())
                } else if p.join("main.rs").is_file() {
                    p.file_name().map(|s| s.to_string_lossy().into_owned())
                } else {
                    None
                }
            })
            .collect();
        if let Ok(v) = std::fs::read_to_string(&manifest).map(|t| t.parse::<toml::Table>().unwrap_or_default()) {
            if let Some(arr) = v.get(kind).and_then(|x| x.as_array()) {
                names.extend(arr.iter().filter_map(|t| t.get("name")?.as_str().map(str::to_string)));
            }
            if kind == "bin"
                && root.join("src/main.rs").is_file()
                && let Some(n) = v.get("package").and_then(|p| p.get("name")).and_then(|n| n.as_str())
            {
                names.push(n.to_string());
            }
        }
        names.sort();
        names.dedup();
        names.into_iter().map(|n| sug(n).desc(kind)).collect()
    })
}

pub fn cargo_features() -> Generator {
    gen_fn("cargo:features", |cx| {
        let Some(manifest) = find_up(cx.cwd, "Cargo.toml") else { return vec![] };
        let v = std::fs::read_to_string(manifest).ok().and_then(|t| t.parse::<toml::Table>().ok()).unwrap_or_default();
        v.get("features")
            .and_then(|f| f.as_table())
            .map(|t| t.keys().map(|k| sug(k.clone()).desc("feature")).collect())
            .unwrap_or_default()
    })
    .query_after(",")
}

pub fn cargo_packages() -> Generator {
    gen_fn("cargo:packages", |cx| {
        let Some(out) = cx.run(&["cargo", "metadata", "--no-deps", "--format-version", "1", "--offline"]) else {
            return vec![];
        };
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
        v["packages"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|p| Some(sug(p["name"].as_str()?).desc(p["version"].as_str().unwrap_or(""))))
                    .collect()
            })
            .unwrap_or_default()
    })
    .ttl(30_000)
}

pub fn cargo_installed() -> Generator {
    gen_fn("cargo:installed", |cx| {
        cx.lines(&["cargo", "install", "--list"])
            .iter()
            .filter(|l| !l.starts_with(' '))
            .filter_map(|l| {
                let (n, rest) = l.split_once(' ')?;
                Some(sug(n).desc(rest.trim_end_matches(':')))
            })
            .collect()
    })
}

pub fn make_targets() -> Generator {
    gen_fn("make:targets", |cx| {
        let dir = cx.flag_value(&["-C", "--directory"]).map_or_else(|| cx.cwd.to_path_buf(), |d| cx.cwd.join(d));
        let file = cx
            .flag_value(&["-f", "--file", "--makefile"])
            .map(|f| dir.join(f))
            .or_else(|| ["GNUmakefile", "makefile", "Makefile"].iter().map(|n| dir.join(n)).find(|p| p.is_file()));
        let Some(text) = file.and_then(|f| std::fs::read_to_string(f).ok()) else { return vec![] };
        let mut out: Vec<SuggestionDef> = vec![];
        let mut last_comment = String::new();
        for line in text.lines() {
            if let Some(c) = line.strip_prefix("##").or_else(|| line.strip_prefix('#')) {
                last_comment = c.trim().to_string();
                continue;
            }
            if line.starts_with(['\t', ' ', '.']) || line.contains(":=") || line.contains("?=") {
                last_comment.clear();
                continue;
            }
            if let Some((targets, rest)) = line.split_once(':') {
                if rest.starts_with('=') || targets.contains(['$', '%', '=']) {
                    continue;
                }
                // `target: deps ## description`
                let desc =
                    rest.split_once("##").map(|(_, d)| d.trim().to_string()).unwrap_or_else(|| last_comment.clone());
                for t in targets.split_whitespace() {
                    if !out.iter().any(|s| s.meta.names[0] == t) {
                        out.push(sug(t).desc(desc.clone()));
                    }
                }
            }
            last_comment.clear();
        }
        out
    })
}

// ---------------------------------------------------------------- system

pub fn processes() -> Generator {
    gen_fn("sys:pids", |cx| {
        cx.lines(&["ps", "-eo", "pid=,comm="])
            .iter()
            .filter_map(|l| {
                let (pid, name) = l.trim().split_once(' ')?;
                Some(sug(pid).desc(name.trim()))
            })
            .rev()
            .collect()
    })
    .ttl(2000)
}

pub fn process_names() -> Generator {
    gen_fn("sys:process-names", |cx| {
        let mut names = cx.lines(&["ps", "-eo", "comm="]);
        names.sort();
        names.dedup();
        names.into_iter().map(|n| sug(n).desc("process")).collect()
    })
    .ttl(2000)
}

/// `systemctl` units; `--user` in the line lists user units.
pub fn systemd_units() -> Generator {
    gen_fn("systemd:units", |cx| {
        let user = cx.has_flag(&["--user"]);
        let mut base = vec!["systemctl", "--no-legend", "--plain", "--no-pager"];
        if user {
            base.push("--user");
        }
        let mut out: Vec<SuggestionDef> = vec![];
        let mut argv = base.clone();
        argv.extend(["list-units", "--all"]);
        for l in cx.lines(&argv) {
            let p: Vec<&str> = l.split_whitespace().collect();
            if p.len() >= 4 {
                let desc = format!("{} — {}", p[3], p[4..].join(" "));
                out.push(sug(p[0]).desc(desc).priority(if p[3] == "running" { 60.0 } else { 50.0 }));
            }
        }
        let mut argv = base;
        argv.push("list-unit-files");
        for l in cx.lines(&argv) {
            let p: Vec<&str> = l.split_whitespace().collect();
            if !p.is_empty() && !out.iter().any(|s| s.meta.names[0] == p[0]) {
                out.push(sug(p[0]).desc(p.get(1).copied().unwrap_or("")));
            }
        }
        out
    })
    .ttl(10_000)
}

pub fn apt_installed() -> Generator {
    gen_fn("apt:installed", |cx| {
        cx.lines(&["dpkg-query", "-W", "-f", "${Package}\n"]).into_iter().map(|p| sug(p).desc("installed")).collect()
    })
    .ttl(60_000)
}

/// Package names starting with the first two typed characters.
pub fn apt_available() -> Generator {
    gen_fn("apt:available", |cx| {
        let prefix: String = cx.current().chars().take(2).collect();
        if prefix.chars().count() < 2 {
            return vec![];
        }
        cx.lines(&["apt-cache", "pkgnames", &prefix]).into_iter().map(sug).collect()
    })
    .trigger(Trigger::Prefix(2))
    .ttl(300_000)
}

pub fn ssh_hosts() -> Generator {
    gen_fn("ssh:hosts", |_| ssh_host_list()).ttl(30_000)
}

/// Hosts from `~/.ssh/config` (following `Include`), the system config, and `known_hosts`.
pub fn ssh_host_list() -> Vec<SuggestionDef> {
    {
        let home = dirs::home_dir().unwrap_or_default();
        let mut hosts: Vec<SuggestionDef> = vec![];
        let mut add = |h: &str, desc: &str| {
            if !h.contains(['*', '?', '!']) && !hosts.iter().any(|s| s.meta.names[0] == h) {
                hosts.push(sug(h).desc(desc));
            }
        };
        let mut configs = vec![home.join(".ssh/config"), PathBuf::from("/etc/ssh/ssh_config")];
        let mut i = 0;
        while i < configs.len() && i < 32 {
            let text = std::fs::read_to_string(&configs[i]).unwrap_or_default();
            for line in text.lines() {
                let mut p = line.split_whitespace();
                match p.next().map(str::to_ascii_lowercase).as_deref() {
                    Some("host") => p.for_each(|h| add(h, "ssh config")),
                    Some("include") => {
                        for inc in p {
                            let inc = inc.replace('~', &home.to_string_lossy());
                            let path =
                                if inc.starts_with('/') { PathBuf::from(&inc) } else { home.join(".ssh").join(&inc) };
                            if let Some(parent) = path.parent().filter(|_| inc.contains('*')) {
                                configs.extend(
                                    std::fs::read_dir(parent).into_iter().flatten().flatten().map(|e| e.path()),
                                );
                            } else {
                                configs.push(path);
                            }
                        }
                    }
                    _ => {}
                }
            }
            i += 1;
        }
        let known = std::fs::read_to_string(home.join(".ssh/known_hosts")).unwrap_or_default();
        for line in known.lines() {
            if let Some(h) = line.split_whitespace().next().filter(|h| !h.starts_with('|')) {
                for h in h.split(',') {
                    add(h.trim_start_matches('[').split(']').next().unwrap_or(h), "known host");
                }
            }
        }
        hosts
    }
}

pub fn tmux_sessions() -> Generator {
    gen_fn("tmux:sessions", |cx| {
        cx.lines(&["tmux", "list-sessions", "-F", "#S\t#{session_windows} windows#{?session_attached, (attached),}"])
            .iter()
            .map(|l| {
                let (n, d) = l.split_once('\t').unwrap_or((l, ""));
                sug(n).desc(d)
            })
            .collect()
    })
}

pub fn pip_installed() -> Generator {
    gen_fn("pip:installed", |cx| {
        let prog = cx.tokens.first().map_or("pip", String::as_str);
        cx.lines(&[prog, "list", "--format=freeze", "--disable-pip-version-check"])
            .iter()
            .filter_map(|l| {
                let (n, v) = l.split_once("==")?;
                Some(sug(n).desc(v))
            })
            .collect()
    })
    .ttl(30_000)
}

pub fn rustup_toolchains() -> Generator {
    gen_fn("rustup:toolchains", |cx| {
        cx.lines(&["rustup", "toolchain", "list"])
            .iter()
            .map(|l| {
                let name = l.split_whitespace().next().unwrap_or(l);
                sug(name).desc(l.strip_prefix(name).unwrap_or("").trim())
            })
            .collect()
    })
}

pub fn rustup_list(id: &'static str, what: &'static str, installed: bool) -> Generator {
    gen_fn(id, move |cx| {
        let mut argv = vec!["rustup", what, "list"];
        if installed {
            argv.push("--installed");
        }
        cx.lines(&argv).into_iter().map(|t| sug(t.split_whitespace().next().unwrap_or("").to_string())).collect()
    })
    .ttl(60_000)
}

// ---------------------------------------------------------------- asdf

/// The plugin the line is about: `asdf install nodejs …`, `asdf list all nodejs …`.
fn asdf_plugin(cx: &GenCtx) -> Option<String> {
    cx.positionals().into_iter().skip(1).find(|p| *p != "all").map(str::to_string)
}

/// Installed plugins.
pub fn asdf_plugins() -> Generator {
    gen_fn("asdf:plugins", |cx| cx.lines(&["asdf", "plugin", "list"]).into_iter().map(sug).collect()).ttl(10_000)
}

/// Plugins in the asdf-plugins repository, with their URL.
pub fn asdf_plugins_all() -> Generator {
    gen_fn("asdf:plugins-all", |cx| {
        cx.lines(&["asdf", "plugin", "list", "all"])
            .into_iter()
            .filter_map(|l| {
                let mut words = l.split_whitespace();
                let name = words.next()?.to_string();
                Some(sug(name).desc(words.next().unwrap_or("")))
            })
            .collect()
    })
    .ttl(3_600_000)
}

/// Installed versions of the plugin on the line, the current one marked.
pub fn asdf_installed() -> Generator {
    gen_fn("asdf:installed", |cx| {
        let Some(plugin) = asdf_plugin(cx) else { return vec![] };
        cx.lines(&["asdf", "list", &plugin])
            .into_iter()
            // Skip messages like "No versions installed".
            .filter(|l| !l.contains(char::is_whitespace))
            .map(|l| match l.strip_prefix('*') {
                Some(v) => sug(v).desc("current").priority(60.0),
                None => sug(l),
            })
            .collect()
    })
    .ttl(5000)
}

/// Every version the plugin can install, newest first, after `latest`.
pub fn asdf_versions() -> Generator {
    gen_fn("asdf:versions", |cx| {
        let Some(plugin) = asdf_plugin(cx) else { return vec![] };
        let mut out = vec![sug("latest").desc("the latest stable version").priority(60.0)];
        let versions = cx.lines(&["asdf", "list", "all", &plugin]);
        out.extend(versions.into_iter().rev().filter(|l| !l.contains(char::is_whitespace)).map(sug));
        out
    })
    .ttl(3_600_000)
}

/// Commands with an asdf shim.
pub fn asdf_shims() -> Generator {
    gen_fn("asdf:shims", |_| {
        let data = std::env::var_os("ASDF_DATA_DIR")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|h| h.join(".asdf")))
            .unwrap_or_default();
        let mut names: Vec<String> = std::fs::read_dir(data.join("shims"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names.into_iter().map(sug).collect()
    })
    .ttl(10_000)
}

// ---------------------------------------------------------------- php

/// Laravel Artisan commands of the project, with their descriptions.
/// `php artisan --help` only describes `list`, so the commands come from
/// `list --format=json` (Symfony's descriptor), falling back to `--raw`.
pub fn artisan_commands() -> Generator {
    gen_fn("php:artisan-commands", |cx| {
        let mut out: Vec<SuggestionDef> = cx
            .run(&["php", "artisan", "list", "--format=json"])
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .map(|v| {
                v["commands"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|c| c["hidden"] != true)
                    .filter_map(|c| Some(sug(c["name"].as_str()?).desc(c["description"].as_str().unwrap_or(""))))
                    .collect()
            })
            .unwrap_or_default();
        if out.is_empty() {
            out.extend(cx.lines(&["php", "artisan", "list", "--raw"]).into_iter().map(sug));
        }
        out
    })
    .ttl(30_000)
}

// ---------------------------------------------------------------- ghostty

/// Themes Ghostty can load, and where they come from.
pub fn ghostty_themes() -> Generator {
    gen_fn("ghostty:themes", |cx| {
        cx.lines(&["ghostty", "+list-themes", "--plain"])
            .into_iter()
            .map(|l| match l.rsplit_once(" (") {
                // `Dracula (resources)`: shipped with Ghostty, `(user)`: yours.
                Some((name, "resources)")) => sug(name).desc("built-in"),
                Some((name, _)) => sug(name).desc("your theme"),
                None => sug(&*l),
            })
            .collect()
    })
    .ttl(3_600_000)
}

/// Font families Ghostty finds.
pub fn ghostty_fonts() -> Generator {
    gen_fn("ghostty:fonts", |cx| {
        let out = cx.run(&["ghostty", "+list-fonts"]).unwrap_or_default();
        // Families start at the margin; their faces are indented below them.
        out.lines().filter(|l| !l.is_empty() && !l.starts_with(char::is_whitespace)).map(sug).collect()
    })
    .ttl(3_600_000)
}

// ---------------------------------------------------------------- opencode

/// Models of the configured providers, as `provider/model`.
pub fn opencode_models() -> Generator {
    gen_fn("opencode:models", |cx| {
        cx.lines(&["opencode", "models"]).into_iter().filter(|l| l.contains('/')).map(sug).collect()
    })
    .ttl(60_000)
}

/// Sessions of the current project, newest first, with their title.
pub fn opencode_sessions() -> Generator {
    gen_fn("opencode:sessions", |cx| {
        let out = cx.run(&["opencode", "session", "list", "--format", "json"]).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
        v.as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| Some(sug(s["id"].as_str()?).desc(s["title"].as_str().unwrap_or(""))))
            .collect()
    })
    .ttl(10_000)
}

/// Agents that can be chosen (internal ones left out).
pub fn opencode_agents() -> Generator {
    gen_fn("opencode:agents", |cx| {
        let out = cx.run(&["opencode", "debug", "agents"]).unwrap_or_default();
        let v: serde_json::Value = serde_json::from_str(&out).unwrap_or_default();
        v.as_array()
            .into_iter()
            .flatten()
            .filter(|a| a["hidden"].as_bool() != Some(true))
            .filter_map(|a| Some(sug(a["id"].as_str()?).desc(a["description"].as_str().unwrap_or(""))))
            .collect()
    })
    .ttl(60_000)
}

// ---------------------------------------------------------------- dnf

/// Installed packages, from the rpm database.
pub fn dnf_installed() -> Generator {
    gen_fn("dnf:installed", |cx| {
        let mut names = cx.lines(&["rpm", "-qa", "--qf", "%{NAME}\\n"]);
        names.sort();
        names.dedup();
        names.into_iter().map(sug).collect()
    })
    .ttl(30_000)
}

/// Packages in the enabled repositories. Read from dnf's metadata cache
/// (`-C`: nothing is downloaded while typing) and kept for a few hours.
pub fn dnf_available() -> Generator {
    gen_fn("dnf:available", |cx| {
        let argv = ["dnf", "-q", "-C", "repoquery", "--available", "--qf", "%{name}\\n"];
        let out = cached_output(cx, &argv, "dnf-available.txt", std::time::Duration::from_secs(6 * 3600));
        let mut names: Vec<&str> =
            out.as_deref().unwrap_or("").lines().map(str::trim).filter(|l| !l.is_empty()).collect();
        names.sort_unstable();
        names.dedup();
        names.into_iter().map(sug).collect()
    })
    .ttl(600_000)
}

/// Repositories: `which` is `--all`, `--enabled` or `--disabled`.
pub fn dnf_repos(id: &'static str, which: &'static str) -> Generator {
    gen_fn(id, move |cx| {
        cx.lines(&["dnf", "-q", "-C", "repolist", which])
            .into_iter()
            .filter(|l| !l.starts_with("repo id"))
            .map(|l| {
                let (repo, name) = l.split_once(char::is_whitespace).unwrap_or((&l, ""));
                sug(repo).desc(name.trim())
            })
            .collect()
    })
    .ttl(60_000)
}

/// Transactions in the history, newest first, with what they did.
pub fn dnf_history() -> Generator {
    gen_fn("dnf:history", |cx| {
        cx.lines(&["dnf", "-q", "history", "list"])
            .into_iter()
            .filter_map(|l| {
                // dnf4 separates the columns with `|`, dnf5 with spaces.
                let (id, rest) = l.split_once(|c: char| c.is_whitespace() || c == '|')?;
                id.parse::<u64>().ok()?;
                let desc = rest.replace('|', " ").split_whitespace().collect::<Vec<_>>().join(" ");
                Some(sug(id).desc(desc))
            })
            .collect()
    })
    .ttl(10_000)
}

// ---------------------------------------------------------------- kubectl

fn kube_base(cx: &GenCtx) -> Vec<String> {
    let mut v = vec!["kubectl".to_string()];
    if let Some(ns) = cx.flag_value(&["-n", "--namespace"]) {
        v.push(format!("--namespace={ns}"));
    }
    if let Some(c) = cx.flag_value(&["--context"]) {
        v.push(format!("--context={c}"));
    }
    v
}

/// Names of the resource type typed before (`kubectl get deploy <here>`), or pods.
pub fn kube_names(default_type: Option<&str>) -> Generator {
    let default_type = default_type.map(str::to_string);
    gen_fn(&format!("kube:names:{}", default_type.as_deref().unwrap_or("")), move |cx| {
        let ty = match &default_type {
            Some(t) => t.clone(),
            None => match cx.positionals().get(1) {
                Some(t) => t.to_string(),
                None => return vec![],
            },
        };
        let mut argv = kube_base(cx);
        argv.extend(["get".into(), ty.clone(), "-o".into(), "name".into()]);
        let argv: Vec<&str> = argv.iter().map(String::as_str).collect();
        cx.lines(&argv).iter().map(|l| sug(l.rsplit('/').next().unwrap_or(l)).desc(ty.clone())).collect()
    })
    .ttl(5000)
}

pub fn kube_contexts() -> Generator {
    gen_fn("kube:contexts", |cx| {
        cx.lines(&["kubectl", "config", "get-contexts", "-o", "name"]).into_iter().map(sug).collect()
    })
}

pub fn kube_namespaces() -> Generator {
    gen_fn("kube:namespaces", |cx| {
        cx.lines(&["kubectl", "get", "namespaces", "-o", "name"])
            .iter()
            .map(|l| sug(l.trim_start_matches("namespace/")))
            .collect()
    })
    .ttl(30_000)
}

#[cfg(test)]
mod tests {
    use super::relative_to;

    #[test]
    fn every_listed_generator_exists() {
        for (id, _) in super::BUILTIN_IDS {
            assert!(super::builtin(id).is_some_and(|g| g.id == *id || id.starts_with("kube:")), "{id}");
        }
        assert!(super::builtin("files:file://").is_some() && super::builtin("kube:names:services").is_some());
    }

    #[test]
    fn git_paths_relative_to_cwd() {
        assert_eq!(relative_to("src/main.rs", ""), "src/main.rs");
        assert_eq!(relative_to("src/main.rs", "src/"), "main.rs");
        assert_eq!(relative_to("README.md", "src/specs/"), "../../README.md");
    }
}

/// `<prefix>` followed by local paths (`file://…`, `fileb://…`).
pub fn prefixed_files(prefix: String) -> Generator {
    gen_fn(&format!("files:{prefix}"), move |cx| {
        let cur = cx.current();
        let Some(rest) = cur.strip_prefix(prefix.as_str()) else {
            return vec![sug(prefix.clone()).desc("local file").insert(prefix.clone())];
        };
        let dir_part = rest.rfind('/').map_or("", |i| &rest[..=i]);
        let dir = if let Some(r) = dir_part.strip_prefix("~/") {
            dirs::home_dir().unwrap_or_default().join(r)
        } else {
            cx.cwd.join(dir_part)
        };
        let mut v: Vec<SuggestionDef> = std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() { sug(format!("{name}/")).kind(Kind::Folder) } else { sug(name).kind(Kind::File) }
            })
            .collect();
        v.sort_by(|a, b| a.meta.names[0].cmp(&b.meta.names[0]));
        v
    })
    .trigger(Trigger::Chars("/".into()))
    .query_after("/")
}

pub fn docker_contexts() -> Generator {
    gen_fn("docker:contexts", |cx| {
        cx.lines(&["docker", "context", "ls", "--format", "{{.Name}}"]).into_iter().map(sug).collect()
    })
}

/// Dependencies listed in the nearest `Cargo.toml`.
pub fn cargo_deps() -> Generator {
    gen_fn("cargo:deps", |cx| {
        let Some(m) = find_up(cx.cwd, "Cargo.toml") else { return vec![] };
        let v = std::fs::read_to_string(m).ok().and_then(|t| t.parse::<toml::Table>().ok()).unwrap_or_default();
        let mut out = vec![];
        for (sec, d) in [
            ("dependencies", "dependency"),
            ("dev-dependencies", "dev dependency"),
            ("build-dependencies", "build dependency"),
        ] {
            if let Some(t) = v.get(sec).and_then(|x| x.as_table()) {
                out.extend(t.keys().map(|k| sug(k.clone()).desc(d)));
            }
        }
        out
    })
}

/// ssh hosts as `host:` (for scp/rsync remote paths).
pub fn ssh_hosts_colon() -> Generator {
    gen_fn("ssh:hosts-colon", |_| {
        ssh_host_list()
            .into_iter()
            .map(|s| {
                let n = format!("{}:", s.meta.names[0]);
                sug(n.clone()).desc("remote host").insert(n)
            })
            .collect()
    })
    .ttl(30_000)
}

/// Local users from `/etc/passwd`.
pub fn sys_users() -> Generator {
    gen_fn("sys:users", |_| {
        std::fs::read_to_string("/etc/passwd")
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split(':').next())
            .map(sug)
            .collect()
    })
}

/// Ids accepted by [`builtin`] (besides `files:<prefix>` and `kube:names:<type>`).
pub const BUILTIN_IDS: &[(&str, &str)] = &[
    ("git:branches", "local and remote branches, recent first"),
    ("git:local-branches", "local branches"),
    ("git:remotes", "remotes"),
    ("git:tags", "tags"),
    ("git:commits", "the last 100 commits"),
    ("git:stashes", "stashes"),
    ("git:unstaged", "modified and untracked files"),
    ("git:staged", "staged files"),
    ("git:modified", "modified tracked files"),
    ("docker:containers", "all containers"),
    ("docker:running", "running containers"),
    ("docker:images", "local images"),
    ("docker:networks", "networks"),
    ("docker:volumes", "volumes"),
    ("docker:contexts", "contexts"),
    ("docker:compose-services", "compose services"),
    ("npm:scripts", "package.json scripts"),
    ("npm:deps", "package.json dependencies"),
    ("cargo:packages", "workspace packages"),
    ("cargo:features", "features of the nearest Cargo.toml"),
    ("cargo:deps", "dependencies of the nearest Cargo.toml"),
    ("cargo:bin", "binary targets"),
    ("cargo:example", "examples"),
    ("cargo:test", "integration tests"),
    ("cargo:bench", "benchmarks"),
    ("cargo:installed", "installed binaries (cargo install --list)"),
    ("rustup:toolchains", "installed toolchains"),
    ("rustup:targets", "all targets"),
    ("rustup:targets-installed", "installed targets"),
    ("rustup:components", "all components"),
    ("rustup:components-installed", "installed components"),
    ("asdf:plugins", "installed plugins"),
    ("asdf:plugins-all", "plugins in the asdf-plugins repository"),
    ("asdf:installed", "installed versions of the plugin on the line"),
    ("asdf:versions", "every version of the plugin on the line"),
    ("asdf:shims", "commands with a shim"),
    ("ghostty:themes", "Ghostty themes"),
    ("ghostty:fonts", "font families Ghostty finds"),
    ("opencode:models", "models of the configured providers"),
    ("opencode:sessions", "sessions of the current project"),
    ("opencode:agents", "agents"),
    ("dnf:installed", "installed packages (rpm)"),
    ("dnf:available", "packages in the enabled repositories (cached metadata)"),
    ("dnf:repos", "all repositories"),
    ("dnf:repos-enabled", "enabled repositories"),
    ("dnf:repos-disabled", "disabled repositories"),
    ("dnf:history", "transaction history"),
    ("make:targets", "Makefile targets"),
    ("sys:pids", "processes, by PID"),
    ("sys:process-names", "process names"),
    ("sys:users", "local users"),
    ("systemd:units", "systemd units (--user aware)"),
    ("apt:installed", "installed packages"),
    ("apt:available", "packages in the apt index"),
    ("snap:installed", "installed snaps"),
    ("extrepo:available", "repositories in the extrepo index"),
    ("extrepo:enabled", "enabled extrepo repositories"),
    ("extrepo:disabled", "disabled extrepo repositories"),
    ("extrepo:installed", "configured extrepo repositories"),
    ("ssh:hosts", "hosts from ssh config and known_hosts"),
    ("ssh:hosts-colon", "the same, as host: (scp)"),
    ("tmux:sessions", "tmux sessions"),
    ("pip:installed", "installed Python packages"),
    ("php:artisan-commands", "Laravel Artisan commands of the project"),
    ("kube:contexts", "kubeconfig contexts"),
    ("kube:namespaces", "namespaces"),
    ("kube:pods", "pods"),
    ("zoxide:dirs", "frecent directories (zoxide or ~/.z)"),
    ("online:npm", "npm registry search; versions after name@"),
    ("online:crates", "crates.io search; versions after name@"),
    ("online:dockerhub", "Docker Hub search; tags after image:"),
    ("online:pypi", "PyPI versions after name=="),
    ("online:pypi-top", "the 15,000 most downloaded PyPI packages"),
    ("online:snap", "Snap Store search"),
];

/// Native generators referenced by id from spec files (`{"builtin": "git:branches"}`).
pub fn builtin(id: &str) -> Option<Generator> {
    if let Some(prefix) = id.strip_prefix("files:") {
        return Some(prefixed_files(prefix.to_string()));
    }
    if let Some(ty) = id.strip_prefix("kube:names:") {
        return Some(kube_names((!ty.is_empty()).then_some(ty)));
    }
    Some(match id {
        "git:branches" => git_branches(),
        "git:local-branches" => git_local_branches(),
        "git:remotes" => git_remotes(),
        "git:tags" => git_tags(),
        "git:commits" => git_commits(),
        "git:stashes" => git_stashes(),
        "git:unstaged" => git_changed(Changes::Unstaged),
        "git:staged" => git_changed(Changes::Staged),
        "git:modified" => git_changed(Changes::Modified),
        "docker:containers" => docker_containers(true),
        "docker:running" => docker_containers(false),
        "docker:images" => docker_images(),
        "docker:networks" => docker_list("docker:networks", "network"),
        "docker:volumes" => docker_list("docker:volumes", "volume"),
        "docker:compose-services" => compose_services(),
        "npm:scripts" => npm_scripts(),
        "npm:deps" => npm_deps(),
        "make:targets" => make_targets(),
        "sys:pids" => processes(),
        "sys:process-names" => process_names(),
        "systemd:units" => systemd_units(),
        "apt:installed" => apt_installed(),
        "apt:available" => apt_available(),
        "ssh:hosts" => ssh_hosts(),
        "tmux:sessions" => tmux_sessions(),
        "pip:installed" => pip_installed(),
        "php:artisan-commands" => artisan_commands(),
        "rustup:toolchains" => rustup_toolchains(),
        "kube:contexts" => kube_contexts(),
        "kube:namespaces" => kube_namespaces(),
        "kube:pods" => kube_names(Some("pods")),
        "docker:contexts" => docker_contexts(),
        "cargo:deps" => cargo_deps(),
        "cargo:bin" => cargo_targets("bin"),
        "cargo:example" => cargo_targets("example"),
        "cargo:test" => cargo_targets("test"),
        "cargo:bench" => cargo_targets("bench"),
        "rustup:targets" => rustup_list("rustup:targets", "target", false),
        "rustup:targets-installed" => rustup_list("rustup:targets-installed", "target", true),
        "rustup:components" => rustup_list("rustup:components", "component", false),
        "rustup:components-installed" => rustup_list("rustup:components-installed", "component", true),
        "asdf:plugins" => asdf_plugins(),
        "asdf:plugins-all" => asdf_plugins_all(),
        "asdf:installed" => asdf_installed(),
        "asdf:versions" => asdf_versions(),
        "asdf:shims" => asdf_shims(),
        "ghostty:themes" => ghostty_themes(),
        "ghostty:fonts" => ghostty_fonts(),
        "opencode:models" => opencode_models(),
        "opencode:sessions" => opencode_sessions(),
        "opencode:agents" => opencode_agents(),
        "dnf:installed" => dnf_installed(),
        "dnf:available" => dnf_available(),
        "dnf:repos" => dnf_repos("dnf:repos", "--all"),
        "dnf:repos-enabled" => dnf_repos("dnf:repos-enabled", "--enabled"),
        "dnf:repos-disabled" => dnf_repos("dnf:repos-disabled", "--disabled"),
        "dnf:history" => dnf_history(),
        "ssh:hosts-colon" => ssh_hosts_colon(),
        "sys:users" => sys_users(),
        "snap:installed" => snaps_installed(),
        "extrepo:available" => extrepo_available(),
        "extrepo:enabled" => extrepo_installed(Some(true)),
        "extrepo:disabled" => extrepo_installed(Some(false)),
        "extrepo:installed" => extrepo_installed(None),
        "online:npm" => npm_registry(),
        "online:crates" => crates_io(),
        "online:dockerhub" => docker_hub(),
        "online:pypi" => pypi_versions(),
        "online:pypi-top" => pypi_top(),
        "online:snap" => snap_store(),
        "cargo:packages" => cargo_packages(),
        "cargo:installed" => cargo_installed(),
        "cargo:features" => cargo_features(),
        "zoxide:dirs" => zoxide_dirs(),
        _ => return None,
    })
}

fn home_abbrev(p: &str) -> String {
    match dirs::home_dir().map(|h| h.to_string_lossy().into_owned()) {
        Some(h) if p == h => "~".into(),
        Some(h) => p.strip_prefix(&format!("{h}/")).map_or_else(|| p.to_string(), |r| format!("~/{r}")),
        None => p.to_string(),
    }
}

/// Directories from zoxide (or rupa/z's `~/.z`), ranked and matched by the
/// tool itself using every keyword typed so far.
pub fn zoxide_dirs() -> Generator {
    gen_fn("zoxide:dirs", |cx| {
        let words: Vec<&str> =
            cx.tokens[1..].iter().map(String::as_str).filter(|t| !t.is_empty() && !t.starts_with('-')).collect();
        let mut argv = vec!["zoxide", "query", "--list", "--"];
        argv.extend(&words);
        let mut paths = cx.lines(&argv);
        if paths.is_empty() && cx.run(&["zoxide", "--version"]).is_none() {
            // rupa/z: `path|rank|time` lines.
            let data = std::env::var("_Z_DATA")
                .map(PathBuf::from)
                .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".z"));
            let mut entries: Vec<(f64, String)> = std::fs::read_to_string(data)
                .unwrap_or_default()
                .lines()
                .filter_map(|l| {
                    let mut p = l.split('|');
                    let path = p.next()?.to_string();
                    let rank = p.next()?.parse().ok()?;
                    let lower = path.to_lowercase();
                    words.iter().all(|w| lower.contains(&w.to_lowercase())).then_some((rank, path))
                })
                .collect();
            entries.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
            paths = entries.into_iter().map(|e| e.1).collect();
        }
        paths.iter().take(40).map(|p| sug(home_abbrev(p)).desc("frecent directory")).collect()
    })
    .trigger(Trigger::Change)
    .unfiltered()
    .ttl(10_000)
}

// ---------------------------------------------------------------- online registries
//
// Like Fig's, these search the package registry as you type (via `curl`),
// with ids starting with `online:` so `behavior.online = false` turns them off.

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn http_json(cx: &GenCtx, url: &str, accept: Option<&str>) -> Option<serde_json::Value> {
    let mut argv = vec!["curl", "-fsSL", "--max-time", "4", "-A", "lig (https://github.com/lig autocomplete)"];
    let header;
    if let Some(a) = accept {
        header = format!("Accept: {a}");
        argv.extend(["-H", header.as_str()]);
    }
    argv.push(url);
    serde_json::from_str(&cx.run(&argv)?).ok()
}

/// Split `name@version` (`@scope/name@1.2` keeps its leading `@`).
fn split_at_version(token: &str) -> Option<(&str, &str)> {
    let start = usize::from(token.starts_with('@'));
    token[start..].find('@').map(|i| (&token[..start + i], &token[start + i + 1..]))
}

/// Newest first, stable releases before pre-releases (`1.2.0` > `1.2.0-rc.1`).
fn sort_versions(list: &mut [(String, String)]) {
    fn key(v: &str) -> (bool, Vec<u64>) {
        let pre = v.contains(|c: char| c.is_ascii_alphabetic());
        let nums =
            v.split(|c: char| !c.is_ascii_digit()).filter(|p| !p.is_empty()).filter_map(|p| p.parse().ok()).collect();
        (pre, nums)
    }
    list.sort_by(|a, b| {
        let (ka, kb) = (key(&a.0), key(&b.0));
        ka.0.cmp(&kb.0).then(kb.1.cmp(&ka.1))
    });
}

/// Exact name, then names starting with the query, then the rest; stable otherwise.
fn rank_by_prefix(q: &str, mut v: Vec<SuggestionDef>) -> Vec<SuggestionDef> {
    let q = q.to_lowercase();
    v.sort_by_key(|s| {
        let n = s.meta.names[0].to_lowercase();
        if n == q {
            0
        } else if n.starts_with(&q) {
            1
        } else {
            2
        }
    });
    v
}

fn version_suggestions(
    name: &str,
    sep: &str,
    typed: &str,
    versions: impl IntoIterator<Item = (String, String)>,
) -> Vec<SuggestionDef> {
    versions
        .into_iter()
        .filter(|(v, _)| v.starts_with(typed))
        .take(100)
        .map(|(v, d)| {
            let mut s = sug(format!("{name}{sep}{v}")).desc(d).icon(Icon::Version);
            s.meta.display_name = Some(v);
            s
        })
        .collect()
}

/// npm registry: package search, or versions/dist-tags after `name@`.
pub fn npm_registry() -> Generator {
    gen_fn("online:npm", |cx| {
        let q = cx.current();
        if q.is_empty() {
            return vec![];
        }
        if let Some((name, typed)) = split_at_version(q) {
            let url = format!("https://registry.npmjs.org/{}", name.replace('/', "%2F"));
            let Some(v) = http_json(cx, &url, Some("application/vnd.npm.install-v1+json")) else { return vec![] };
            let mut list: Vec<(String, String)> = v["dist-tags"]
                .as_object()
                .map(|o| {
                    o.iter().map(|(t, ver)| (t.clone(), format!("tag for {}", ver.as_str().unwrap_or("")))).collect()
                })
                .unwrap_or_default();
            let mut versions: Vec<(String, String)> = v["versions"]
                .as_object()
                .map(|o| o.keys().map(|k| (k.clone(), String::new())).collect())
                .unwrap_or_default();
            sort_versions(&mut versions);
            list.extend(versions);
            return version_suggestions(name, "@", typed, list);
        }
        // npms.io completes prefixes by popularity (`reac` → react) but its
        // version numbers are stale, so only names and descriptions are used.
        let url = format!("https://api.npms.io/v2/search/suggestions?q={}&size=25", urlencode(q));
        if let Some(serde_json::Value::Array(list)) = http_json(cx, &url, None)
            && !list.is_empty()
        {
            let v = list.iter().filter_map(|o| {
                let p = &o["package"];
                Some(sug(p["name"].as_str()?).desc(p["description"].as_str().unwrap_or("")))
            });
            return rank_by_prefix(q, v.collect());
        }
        // Fallback: the registry's own search (matches whole words only).
        let url = format!("https://registry.npmjs.org/-/v1/search?text={}&size=25", urlencode(q));
        let Some(v) = http_json(cx, &url, None) else { return vec![] };
        let list = v["objects"].as_array().into_iter().flatten().filter_map(|o| {
            let p = &o["package"];
            let desc = format!("{} · {}", p["version"].as_str().unwrap_or(""), p["description"].as_str().unwrap_or(""));
            Some(sug(p["name"].as_str()?).desc(desc))
        });
        rank_by_prefix(q, list.collect())
    })
    .trigger(Trigger::Change)
    .unfiltered()
    .ttl(30 * 60_000)
}

/// crates.io: crate search, or versions after `name@`.
pub fn crates_io() -> Generator {
    gen_fn("online:crates", |cx| {
        let q = cx.current();
        if q.is_empty() {
            return vec![];
        }
        if let Some((name, typed)) = q.split_once('@') {
            let url = format!("https://crates.io/api/v1/crates/{}/versions", urlencode(name));
            let Some(v) = http_json(cx, &url, None) else { return vec![] };
            let mut list: Vec<(String, String)> = v["versions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|x| x["yanked"] != true)
                .filter_map(|x| {
                    Some((
                        x["num"].as_str()?.to_string(),
                        x["created_at"].as_str().unwrap_or("").chars().take(10).collect(),
                    ))
                })
                .collect();
            sort_versions(&mut list);
            return version_suggestions(name, "@", typed, list);
        }
        // Relevance finds the exact name; downloads finds the popular ones
        // (`ser` → serde, serde_json).
        let mut out: Vec<(u64, SuggestionDef)> = vec![];
        for sort in ["relevance", "downloads"] {
            let url = format!("https://crates.io/api/v1/crates?q={}&per_page=25&sort={sort}", urlencode(q));
            let Some(v) = http_json(cx, &url, None) else { continue };
            for c in v["crates"].as_array().into_iter().flatten() {
                let Some(name) = c["name"].as_str() else { continue };
                if out.iter().any(|(_, s)| s.meta.names[0] == name) {
                    continue;
                }
                let ver = c["max_stable_version"].as_str().or(c["max_version"].as_str()).unwrap_or("");
                let s = sug(name).desc(format!("{ver} · {}", c["description"].as_str().unwrap_or("").trim()));
                out.push((c["recent_downloads"].as_u64().or(c["downloads"].as_u64()).unwrap_or(0), s));
            }
        }
        // Most downloaded first, then exact/prefix matches ahead (stable sort).
        out.sort_by_key(|x| std::cmp::Reverse(x.0));
        rank_by_prefix(q, out.into_iter().map(|(_, s)| s).collect())
    })
    .trigger(Trigger::Change)
    .unfiltered()
    .ttl(30 * 60_000)
}

/// Docker Hub: repository search, or tags after `image:`.
pub fn docker_hub() -> Generator {
    gen_fn("online:dockerhub", |cx| {
        let q = cx.current();
        if q.is_empty() {
            return vec![];
        }
        if let Some((image, typed)) = q.split_once(':') {
            let repo = if image.contains('/') { image.to_string() } else { format!("library/{image}") };
            let url = format!(
                "https://hub.docker.com/v2/repositories/{repo}/tags?page_size=50&ordering=last_updated&name={}",
                urlencode(typed)
            );
            let Some(v) = http_json(cx, &url, None) else { return vec![] };
            let list = v["results"].as_array().into_iter().flatten().filter_map(|t| {
                Some((
                    t["name"].as_str()?.to_string(),
                    t["last_updated"].as_str().unwrap_or("").chars().take(10).collect(),
                ))
            });
            return version_suggestions(image, ":", typed, list.collect::<Vec<_>>());
        }
        let url = format!("https://hub.docker.com/v2/search/repositories/?query={}&page_size=25", urlencode(q));
        let Some(v) = http_json(cx, &url, None) else { return vec![] };
        let mut repos: Vec<&serde_json::Value> = v["results"].as_array().into_iter().flatten().collect();
        // Official images first, then by stars.
        repos.sort_by_key(|r| (r["is_official"] != true, std::cmp::Reverse(r["star_count"].as_u64().unwrap_or(0))));
        let list = repos.into_iter().filter_map(|r| {
            let official = if r["is_official"] == true { "official, " } else { "" };
            let stars = human_count(r["star_count"].as_u64().unwrap_or(0));
            let desc = format!("{official}{stars} stars · {}", r["short_description"].as_str().unwrap_or(""));
            Some(sug(r["repo_name"].as_str()?).desc(desc).priority(40.0))
        });
        rank_by_prefix(q, list.collect())
    })
    .trigger(Trigger::Change)
    .unfiltered()
    .ttl(30 * 60_000)
}

/// Download `url` into `~/.cache/lig/<file>`, reusing it for `max_age`
/// (and, when offline, however old it is).
fn cached_download(cx: &GenCtx, url: &str, file: &str, max_age: std::time::Duration) -> Option<String> {
    let path = dirs::cache_dir()?.join("lig").join(file);
    let fresh = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age < max_age);
    if !fresh {
        let tmp = path.with_extension("part");
        let _ = std::fs::create_dir_all(path.parent()?);
        let tmp_s = tmp.to_string_lossy().into_owned();
        if cx.run(&["curl", "-fsSL", "--max-time", "8", "-o", &tmp_s, url]).is_some() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
    std::fs::read_to_string(path).ok()
}

fn human_count(n: u64) -> String {
    match n {
        1_000_000_000.. => format!("{:.1}B", n as f64 / 1e9),
        1_000_000.. => format!("{:.1}M", n as f64 / 1e6),
        1_000.. => format!("{:.0}K", n as f64 / 1e3),
        _ => n.to_string(),
    }
}

/// The 15,000 most downloaded PyPI packages (hugovk/top-pypi-packages,
/// refreshed weekly), since PyPI itself can't be searched.
pub fn pypi_top() -> Generator {
    gen_fn("online:pypi-top", |cx| {
        if cx.current().contains("==") {
            return vec![];
        }
        let url = "https://hugovk.github.io/top-pypi-packages/top-pypi-packages.min.json";
        let Some(text) = cached_download(cx, url, "top-pypi-packages.json", std::time::Duration::from_secs(7 * 86400))
        else {
            return vec![];
        };
        let v: serde_json::Value = serde_json::from_str(&text).unwrap_or_default();
        v["rows"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| {
                let downloads = r["download_count"].as_u64().unwrap_or(0);
                Some(
                    sug(r["project"].as_str()?)
                        .desc(format!("{} downloads/month", human_count(downloads)))
                        .priority(51.0),
                )
            })
            .collect()
    })
    .ttl(24 * 3_600_000)
}

/// PyPI has no search API; this offers versions after `name==`.
pub fn pypi_versions() -> Generator {
    gen_fn("online:pypi", |cx| {
        let Some((name, typed)) = cx.current().split_once("==") else { return vec![] };
        let url = format!("https://pypi.org/pypi/{}/json", urlencode(name));
        let Some(v) = http_json(cx, &url, None) else { return vec![] };
        let mut list: Vec<(String, String)> = v["releases"]
            .as_object()
            .map(|o| {
                o.iter()
                    .filter(|(_, files)| files.as_array().is_some_and(|f| !f.is_empty()))
                    .map(|(ver, files)| {
                        (ver.clone(), files[0]["upload_time"].as_str().unwrap_or("").chars().take(10).collect())
                    })
                    .collect()
            })
            .unwrap_or_default();
        sort_versions(&mut list);
        version_suggestions(name, "==", typed, list)
    })
    .trigger(Trigger::Change)
    .ttl(30 * 60_000)
}

/// `snap find`: the Snap Store, searched as you type.
pub fn snap_store() -> Generator {
    gen_fn("online:snap", |cx| {
        let q = cx.current();
        if q.chars().count() < 2 {
            return vec![];
        }
        cx.lines(&["snap", "find", q])
            .iter()
            .skip(1)
            .filter_map(|l| {
                // Name  Version  Publisher  Notes  Summary
                let cols: Vec<&str> = l.split_whitespace().collect();
                let summary = cols.get(4..).map(|c| c.join(" ")).unwrap_or_default();
                Some(sug(*cols.first()?).desc(format!("{} · {summary}", cols.get(1).unwrap_or(&""))))
            })
            .collect()
    })
    .trigger(Trigger::Change)
    .unfiltered()
    .ttl(30 * 60_000)
}

pub fn snaps_installed() -> Generator {
    gen_fn("snap:installed", |cx| {
        cx.lines(&["snap", "list"])
            .iter()
            .skip(1)
            .filter_map(|l| {
                let c: Vec<&str> = l.split_whitespace().collect();
                Some(sug(*c.first()?).desc(format!("{} · {}", c.get(1).unwrap_or(&""), c.get(3).unwrap_or(&""))))
            })
            .collect()
    })
    .ttl(30_000)
}

#[cfg(test)]
mod online_tests {
    use super::*;

    #[test]
    fn helpers() {
        assert_eq!(urlencode("@types/no de"), "%40types%2Fno%20de");
        assert_eq!(split_at_version("react@18"), Some(("react", "18")));
        assert_eq!(split_at_version("@types/node@2"), Some(("@types/node", "2")));
        assert_eq!(split_at_version("@types/node"), None);
        let v = version_suggestions(
            "react",
            "@",
            "18",
            vec![("18.2.0".into(), String::new()), ("17.0.0".into(), String::new())],
        );
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].meta.names[0], "react@18.2.0");
        assert_eq!(v[0].meta.display_name.as_deref(), Some("18.2.0"));
        let mut l: Vec<(String, String)> =
            ["1.9.0", "1.10.0-rc.1", "1.10.0", "0.9.9"].iter().map(|v| (v.to_string(), String::new())).collect();
        sort_versions(&mut l);
        assert_eq!(l.iter().map(|x| x.0.as_str()).collect::<Vec<_>>(), ["1.10.0", "1.9.0", "0.9.9", "1.10.0-rc.1"]);
    }
}

// ---------------------------------------------------------------- extrepo

/// Run a command and cache its stdout in `~/.cache/lig/<file>` for `max_age`
/// (falling back to the stale copy when it fails).
fn cached_output(cx: &GenCtx, argv: &[&str], file: &str, max_age: std::time::Duration) -> Option<String> {
    let path = dirs::cache_dir()?.join("lig").join(file);
    let age = std::fs::metadata(&path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
    if age.is_none_or(|a| a >= max_age)
        && let Some(out) = cx.run(argv).filter(|o| !o.trim().is_empty())
    {
        let _ = std::fs::create_dir_all(path.parent()?);
        let _ = std::fs::write(&path, &out);
        return Some(out);
    }
    std::fs::read_to_string(path).ok()
}

/// `(name, description, policy)` for every repository `extrepo search` knows.
fn extrepo_index(cx: &GenCtx) -> Vec<(String, String, String)> {
    let Some(out) =
        cached_output(cx, &["extrepo", "search"], "extrepo-search.txt", std::time::Duration::from_secs(86400))
    else {
        return vec![];
    };
    let mut repos = vec![];
    let mut lines = out.lines().peekable();
    while let Some(line) = lines.next() {
        let Some(name) = line.strip_prefix("Found ").and_then(|n| n.strip_suffix(':')) else { continue };
        let (mut desc, mut policy) = (String::new(), String::new());
        while let Some(l) = lines.peek() {
            if l.starts_with("Found ") {
                break;
            }
            let l = lines.next().unwrap();
            if let Some(d) = l.strip_prefix("description:") {
                desc = d.trim().to_string();
                if desc == "|" || desc == ">" || desc.is_empty() {
                    // YAML block: the next indented line.
                    desc = lines.peek().map(|n| n.trim().to_string()).unwrap_or_default();
                }
            } else if let Some(p) = l.strip_prefix("policy:") {
                policy = p.trim().to_string();
            }
        }
        repos.push((name.to_string(), desc, policy));
    }
    repos
}

/// Repositories set up on this machine: name → enabled.
fn extrepo_local() -> Vec<(String, bool)> {
    let mut v: Vec<(String, bool)> = std::fs::read_dir("/etc/apt/sources.list.d")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let file = e.file_name().to_string_lossy().into_owned();
            let name = file.strip_prefix("extrepo_")?.strip_suffix(".sources")?.to_string();
            let text = std::fs::read_to_string(e.path()).unwrap_or_default();
            let disabled = text.lines().any(|l| l.trim().eq_ignore_ascii_case("enabled: no"));
            Some((name, !disabled))
        })
        .collect();
    v.sort();
    v
}

/// All repositories in the extrepo index; disabled local ones first.
pub fn extrepo_available() -> Generator {
    gen_fn("extrepo:available", |cx| {
        let local = extrepo_local();
        let mut out: Vec<SuggestionDef> = extrepo_index(cx)
            .into_iter()
            .map(|(name, desc, policy)| {
                let state = local.iter().find(|(n, _)| *n == name).map(|(_, enabled)| *enabled);
                let (prio, tag) = match state {
                    Some(false) => (60.0, "disabled · "),
                    Some(true) => (40.0, "enabled · "),
                    None => (50.0, ""),
                };
                let policy = if policy.is_empty() || policy == "main" { String::new() } else { format!("[{policy}] ") };
                sug(name).desc(format!("{tag}{policy}{desc}")).priority(prio)
            })
            .collect();
        // Local ones the index no longer has.
        for (name, enabled) in local {
            if !out.iter().any(|s| s.meta.names[0] == name) {
                out.push(sug(name).desc(if enabled { "enabled" } else { "disabled" }).priority(55.0));
            }
        }
        out.sort_by(|a, b| a.meta.names[0].cmp(&b.meta.names[0]));
        out
    })
    .ttl(3_600_000)
}

/// Repositories configured here: `Some(true)` enabled only, `Some(false)` disabled only.
pub fn extrepo_installed(which: Option<bool>) -> Generator {
    let id = match which {
        Some(true) => "extrepo:enabled",
        Some(false) => "extrepo:disabled",
        None => "extrepo:installed",
    };
    gen_fn(id, move |_| {
        extrepo_local()
            .into_iter()
            .filter(|(_, enabled)| which.is_none_or(|w| w == *enabled))
            .map(|(name, enabled)| sug(name).desc(if enabled { "enabled" } else { "disabled" }))
            .collect()
    })
}
