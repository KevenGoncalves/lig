<p align="center">
  <img src="assets/logo.svg" width="140" alt="Lig logo: a terminal prompt with a suggestion popup under the cursor">
</p>

<h1 align="center">Lig</h1>

<p align="center">
  <b>IDE-style autocomplete for your Linux terminal.</b><br>
  Subcommands, options, branches, packages and paths in a popup as you type.<br>
  For zsh, bash and fish, in any terminal. Written in Rust.
</p>

<p align="center">
  <a href="https://github.com/KevenGoncalves/lig/releases/latest"><img src="https://img.shields.io/github/v/release/KevenGoncalves/lig" alt="Latest release"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/KevenGoncalves/lig" alt="MIT license"></a>
</p>

```
❯ git ch
   ╭──────────────────────────────────────────────────────────╮
   │  ▸ checkout       Switch branches or restore working tree │
   │  ▸ cherry-pick    Apply the changes introduced by some …  │
   ╰──────────────────────────────────────────────────────────╯
```

## What is Lig?

Your shell already completes commands when you press `Tab`, but it shows a flat list of
words and only knows what each command's completion script tells it. Lig shows a popup
under the cursor while you type, like the autocomplete in a code editor: what each
subcommand and option does, values that come from your system (branches, containers,
packages), and the next thing that fits on the line.

It brings the experience of [Fig](https://fig.io) (now Kiro CLI autocomplete) and
[easy-complete](https://github.com/chen86860/easy-complete) to Linux, as a single binary
that works in any terminal: it draws the popup inside the terminal itself.

- **~740 commands** out of the box: git, docker, kubectl, cargo, npm, aws, gh, terraform,
  systemctl, apt, dnf, asdf, ghostty… (the [withfig/autocomplete](https://github.com/withfig/autocomplete)
  specs converted to JSON, plus hand-written ones, all embedded in the binary).
- **Live values**: git branches, docker containers, systemd units, ssh hosts, make targets,
  npm scripts, zoxide directories, PIDs…
- **Package search as you type**: npm, crates.io, Docker Hub, PyPI and the Snap Store.
- **Any other command** gets its options, subcommands and argument values from its
  `--help` or man page.
- Knows your **aliases, functions and builtins** (`gco ` completes like `git checkout `).
- **zsh, bash and fish**, including fish started inside another shell.
- Works inside **tmux** and **zellij**; zellij panes are restored with the app that ran in them.
- Nerd Font icons, themes, configurable size, and any keys you like.
- Runs locally: no daemon, no account, no telemetry.

## Install

Lig runs on Linux with zsh ≥ 5.3, bash ≥ 4.4 or fish ≥ 4.2.1. Optional: `curl` for online
package search, a [Nerd Font](https://www.nerdfonts.com) for icons.

### Prebuilt binary

```bash
curl -fsSL https://github.com/KevenGoncalves/lig/releases/latest/download/install.sh | sh
```

The script downloads the latest release for your machine (x86_64 or ARM64, the static
build that runs on any Linux), checks it against its sha256, and puts `lig` in
`~/.local/bin`. Set `LIG_INSTALL_DIR` to install elsewhere, or `LIG_VERSION` for an older
release (`… | LIG_VERSION=1.0.6 sh`).

To download it yourself, each [release](https://github.com/KevenGoncalves/lig/releases/latest)
has these archives, each with a `.sha256`:

| Archive | Runs on |
| --- | --- |
| `lig-<version>-x86_64-unknown-linux-musl.tar.gz` | any x86_64 Linux (static binary) |
| `lig-<version>-aarch64-unknown-linux-musl.tar.gz` | any ARM64 Linux (static binary) |
| `lig-<version>-x86_64-unknown-linux-gnu.tar.gz` | x86_64 with glibc 2.35+ (Ubuntu 22.04, Debian 12 and newer) |
| `lig-<version>-aarch64-unknown-linux-gnu.tar.gz` | ARM64 with glibc 2.35+ |

### From source

With [Rust](https://rustup.rs) 1.85 or newer:

```bash
git clone https://github.com/KevenGoncalves/lig.git
cd lig
cargo install --path .
```

### Nix + home-manager

The Nix flake has a [home-manager](https://github.com/nix-community/home-manager) module that
installs Lig, writes its config and sets up your shells. Add the flake to your inputs:

```nix
# flake.nix
inputs.lig = {
  url = "github:KevenGoncalves/lig";
  inputs.nixpkgs.follows = "nixpkgs";
};
```

Then import the module, enable it, and optionally also configure it:

```nix
# home.nix
{ inputs, ... }:
{
  imports = [ inputs.lig.homeModules.default ];

  programs.lig = {
    enable = true;
    settings = { # gets written to ~/.config/lig/config.toml
      ui.theme = "catppuccin";
      behavior.online = false;
    };
  };
}
```

Lig is added to every shell home-manager manages. To leave one out, set its option to
`false`: `programs.lig.enableFishIntegration`, `enableZshIntegration` or
`enableBashIntegration`.

To update Lig, update your Nix flake and rebuild instead of using `lig update`.

### Set up your shell

```bash
lig install            # your $SHELL; or: lig install zsh / bash / fish
```

Open a new terminal, then check the setup with `lig doctor`.

`lig install` adds a small block at the top and one at the bottom of your shell config
(`~/.zshrc`, `~/.bashrc` or `~/.config/fish/config.fish`) and keeps the original as
`<config>.lig-backup`. To set it up by hand instead:

```bash
# first line of ~/.zshrc                 # last line of ~/.zshrc
eval "$(lig init zsh pre)"               eval "$(lig init zsh post)"

# first line of ~/.bashrc                # last line of ~/.bashrc
eval "$(lig init bash pre)"              eval "$(lig init bash post)"
```

```fish
# first line of config.fish
eval (lig init fish pre | string collect)
# last line of config.fish
eval (lig init fish post | string collect)
```

### Update and remove

- **Update**: `lig update` installs the latest release from GitHub over the current
  binary (the static build, checked against its sha256). `lig update --check` only tells
  you whether there's a newer one. Then open a new terminal tab: a tab keeps the version
  it started with.
- **Skip it for one shell**: `LIG_DISABLE=1 zsh`.
- **Remove it**: `lig uninstall` (or `lig uninstall fish` / `bash` / `zsh`).

## Usage

Type as usual and the popup appears with suggestions for what you're typing.

| Key | Action |
| --- | --- |
| `Tab` / `↓` | next suggestion |
| `Shift-Tab` / `↑` | previous suggestion (on the first one: close the popup) |
| `Enter` | accept the suggestion |
| `Esc` | close the popup (`Esc` `Esc` too, then `Enter` runs the line) |
| `Ctrl+Space` | open / close the popup on demand |

What to expect:

- **Command names don't pop up by themselves** (`ls`, `sudo ls`…), so simple commands run
  with a single `Enter`. Press `Ctrl+Space` to see command names, or set
  `complete_commands = true` to always get them.
- **`Enter`** completes the word you're typing (`go bui` → `go build`), or picks the first
  suggestion after a space (`git checkout ` → `git checkout main`). If the word is already
  complete (`git status`), it runs the command. `Enter` `Enter` means "pick, then run".
- **No space is added** after a pick, options included (`--force`). Type the space
  yourself to see what comes next.
- **Files first for commands that open them** (`code`, `vim`, `nvim`, `cat`, `less`,
  `xdg-open`…): their files and folders come before their options. Other commands list
  options first. Add commands with `files_first_commands`.
- **Folders** (`cd`, `mkdir`, …): picking one keeps the popup open with its subfolders.
  The first entries are **↵ Enter** (run the command) and **Back** (one folder up). Path
  options like `cargo install --path` also offer `.`, the current folder.
- **History and the shell's own completion stay yours**: while you browse history with the
  arrows, use `Tab` after closing the popup, or run a tool like fzf's `Ctrl+T`, Lig stays
  out of the way. Typing brings it back.

## Configuration

Settings live in `~/.config/lig/config.toml`. `lig config` prints every option with its
default value. The ones you're most likely to change:

```toml
[ui]
theme = "default"          # catppuccin, nord, dracula, gruvbox, tokyonight, light, terminal
border = "rounded"         # square, none
density = "normal"         # compact, relaxed
icons = "auto"             # nerd, unicode, none
width = "auto"             # or columns (60), or "50%" of the terminal
height = 8                 # max rows, or "30%"
description_position = "right"   # or "bottom": a frame under the list

[colors]                   # override single theme colors: "#rrggbb", 0-255, or "default"
# bg = "#1e1e2e"

[icons]                    # change single icons by name (`lig icons` lists them)
# folder = "📁"                                    # just the glyph
# git-branch = { glyph = "", color = "#f39c12" }  # glyph and color, colors as in [colors]
# option = { color = "214" }                       # just the color
# "*.rs" = "🦀"                                    # files by extension
# "Dockerfile" = "🐳"                              # a file by name

[keys]                     # any key: run `lig keys` to see a key's name
accept = ["enter"]
next = ["tab", "down", "ctrl-n"]
prev = ["shift-tab", "up", "ctrl-p"]
dismiss = ["esc"]
toggle = ["ctrl-space"]

[behavior]
enter_accepts = "auto"     # "after-space", "always", "never"
insert_space = false       # add a space after every pick
complete_commands = false  # pop up for command names too
online = true              # search package registries as you type
disabled_commands = []     # never show the popup for these
files_first_commands = ["code", "nvim", "…"]  # list files before options (editors, viewers…)
```

**Keys** are a key name with optional modifiers: `ctrl-n`, `alt-j`, `shift-up`,
`ctrl-alt-x`, `f5`, `pagedown`, `ctrl-space`. For anything else your terminal sends, use its
escape sequence (`"\\x1b[1;5C"`). Run `lig keys` and press a key to see its name. Keys your
terminal keeps for itself (often Ctrl+Tab) never reach the shell.

**Icons** apply from general to specific: `file = { color = "1" }` colors every file, and
`"*.rs"` then sets the glyph for Rust files only. Unknown names are reported.

**Try styles** without editing the file:

```bash
lig preview --all                                   # every theme
lig preview --theme nord --density compact --border square
lig preview --description bottom --width 60
lig icons                                           # every icon Lig uses, with its name
```

Changes apply to new terminal tabs.

## Where suggestions come from

Looked up in this order:

1. **Your specs** in `~/.config/lig/specs/<command>.json` or `.toml`.
2. **Hand-written specs** in [`specs/lig/`](specs/lig) (git, docker, cargo, npm, systemctl,
   apt, dnf, asdf, kubectl, cd, …), merged with the converted one for the same command when
   there is one.
3. **Converted Fig specs** in [`specs/fig/`](specs/fig).
4. **The command's `--help` or man page**, parsed and cached per executable version:
   options, subcommands, and argument values like `[possible values: a, b]`. `--help` only
   runs for real binaries and Python/Node/Ruby scripts, with no input, no display and a
   2 s timeout; other files only get their man page read. Turn it off with
   `help_parsing = false`.

`lig specs list` shows which source each command uses.

**Online search** (`npm i`, `pnpm add`, `cargo add`, `docker pull`, `pip install`,
`snap install`) sends the package name you're typing to that registry through `curl`, with
results cached for 30 minutes. Turn it off with `online = false`.

### Writing a spec

A spec describes a command's subcommands, options and arguments, in the
[Fig spec format](https://fig.io/docs/reference) (static parts only). Save it as
`~/.config/lig/specs/deploy.toml`:

```toml
name = "deploy"
description = "Deploy the app"

[[options]]
name = ["-e", "--env"]
description = "Target environment"
args = { name = "env", suggestions = ["staging", "production"] }

[[subcommands]]
name = "logs"
description = "Tail the logs of a service"
args = { name = "service", generators = { script = ["deploy", "list-services"] } }
```

Arguments can list fixed `suggestions`, use `template = "filepaths"` or `"folders"`, or run a
command whose output lines become suggestions (`generators = { script = [...] }`). Lig also
has native generators (git branches, docker containers, …) usable with
`generators = [{ builtin = "git:branches" }]`; `lig specs generators` lists them.

Check a spec with `lig complete "deploy -e "`.

## How it works

Lig runs your shell inside a pseudo-terminal (`lig shell`) and passes everything through.
It keeps a model of the screen, draws the popup into the terminal under the cursor, and
restores what was underneath when the popup closes. This is why it works in any terminal,
and on Wayland, where apps can't place floating windows.

Small shell hooks tell Lig what you're typing: zsh reports the line on every redraw, bash's
line is read from the screen after an invisible prompt marker, and fish reports its line
when Lig asks after each key. To accept a suggestion, Lig sends a private key that a shell
function binds to edit the line. Slow sources (commands, network) run in the background, so
the popup never blocks typing.

## zellij

Inside zellij, `lig shell` is the pane's process, so zellij's session resurrection would
bring every pane back as `lig shell` in the folder lig started in. To avoid that, lig
mirrors the foreground app into its own command line and working directory, once a
second. zellij then saves each pane as the app that runs in it (`nvim foo`, `htop`, an
idle shell as a plain pane) with the right folder, and restores it that way.

```bash
lig doctor                  # "[ok  ] zellij mirror: on" inside zellij
LIG_NO_MIRROR=1 zellij      # turn the mirror off; autocomplete still works
```

Outside zellij nothing changes. `LIG_DISABLE=1` turns lig off entirely. Shells, wrappers
(`env`, `nohup`, `timeout`, ...) and commands that are risky to rerun (`sudo`, `rm`, `mv`,
`dd`, `kill`, `git`, ...) are saved as a plain shell.

**Never resurrect with `zellij attach --force-run-commands`.** zellij's resurrect prompt
waits for ENTER in each command pane, and that is the safety net.

Known limitations:

- `pkill -f <app>` inside zellij also matches the lig pane and closes it. `pkill <app>`,
  `killall <app>` and `pgrep -x <app>` don't, because lig's process name stays `lig`.
  `htop` shows the pane as the app.
- A pipeline is saved by its first command (`cat log | less` is saved as `cat log`).
- A terminal started from a zellij pane inherits `$ZELLIJ`, so the mirror runs there too.
  Harmless apart from how `ps` shows lig.
- A restored app runs with the zellij server's `PATH`, like plain zellij.

## Troubleshooting

| Problem | Try |
| --- | --- |
| No popup at all | `lig doctor`: "running inside lig" must be ok. Open a new tab after installing or updating. |
| Wrong or missing suggestions | `lig complete "<the line>"` shows what Lig would suggest. |
| A key binding does nothing | `lig keys` shows whether the key reaches Lig, and its name. |
| A command's suggestions look outdated | `lig specs clear-cache` re-reads `--help` output. |
| Something else | Start a shell with `LIG_LOG=1 zsh` (or `bash` / `fish`) and check `~/.local/state/lig/lig.log`. |

## Limitations

- In bash the line is read from the screen. A cursor inside a long wrapped line at the
  bottom of a full screen can occasionally confuse it, until the next key.
- Fish reserves `SIGUSR1` for line reports and Ctrl+Alt+Shift+F12 for insertion. Fish's own
  `complete` definitions and alias bodies aren't imported, and Starship hasn't been tested
  with fish.
- Specs parsed from `--help` have no live values (like branches).
- About a quarter of the Fig specs' dynamic generators couldn't be converted (they relied on
  JavaScript logic); the options and subcommands around them are kept.

## Development

```bash
cargo test                      # unit tests, including every spec in specs/lig/
cargo build --release
python3 tests/fish_pty.py       # PTY tests: fish, then bash/zsh, tmux, the trial launcher
python3 tests/shell_smoke.py
python3 tests/tmux_placement.py
python3 tests/trial_launcher.py
python3 tests/zellij_mirror.py  # the zellij mirror (needs zsh, not zellij)
tools/update-fig-specs.sh       # regenerate specs/fig/ from withfig/autocomplete
```

The PTY tests need Python 3 and the shells they test; the tmux tests use a private server.
They test `target/release/lig`; set `LIG_TEST_BIN` to use another binary.

```
src/                  the lig binary (session.rs: PTY + popup, engine.rs: completion,
                      popup.rs: drawing, help.rs: --help parser, specs/gens.rs: native generators)
shell/                zsh, bash and fish hooks
specs/lig/            hand-written specs (JSON)
specs/fig/            converted withfig/autocomplete specs (generated)
build.rs              embeds both spec folders, compressed, into the binary
tests/                PTY tests (Python)
tools/figconv/        the Fig → JSON converter (development only)
flake.nix, nix/       the Nix flake and home-manager module
install.sh            the installer attached to every release
.github/workflows/    release builds
assets/               the logo
```

To add a built-in spec, add `specs/lig/<command>.json` and rebuild. To add a new kind of
live value, write a generator in `src/specs/gens.rs` and register its id in `builtin()`.

## Credits and license

Thanks to the contributors:

- [@techwizrd](https://github.com/techwizrd): fish support ([#6](https://github.com/KevenGoncalves/lig/pull/6)).
- [@mawkler](https://github.com/mawkler): the Nix flake and home-manager module
  ([#9](https://github.com/KevenGoncalves/lig/pull/9), [#10](https://github.com/KevenGoncalves/lig/pull/10)),
  telling Ctrl+M from Ctrl+J ([#12](https://github.com/KevenGoncalves/lig/pull/12)), removing the
  popup flicker ([#13](https://github.com/KevenGoncalves/lig/pull/13)), and following the inner
  shell's folder ([#14](https://github.com/KevenGoncalves/lig/pull/14)).
- [@nadelacruz-00](https://github.com/nadelacruz-00): the zellij mirror ([#11](https://github.com/KevenGoncalves/lig/pull/11)).
- [@jeremiasadriano](https://github.com/jeremiasadriano): the `php` and `php artisan` specs ([#16](https://github.com/KevenGoncalves/lig/pull/16)).

The specs in `specs/fig/` come from [withfig/autocomplete](https://github.com/withfig/autocomplete),
© Hercules Labs Inc. and contributors, MIT licensed (see [`specs/fig/LICENSE`](specs/fig/LICENSE)).

Lig is MIT licensed, see [`LICENSE`](LICENSE).
