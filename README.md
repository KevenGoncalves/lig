<p align="center">
  <img src="assets/logo.svg" width="140" alt="Lig logo: a terminal prompt with a suggestion popup under the cursor">
</p>

<h1 align="center">Lig</h1>

<p align="center">
  <b>IDE-style autocomplete for your Linux or macOS terminal.</b><br>
  Subcommands, options, branches, packages and paths in a popup as you type.<br>
  For zsh, bash, fish, PowerShell and cmd, in any terminal. Written in Rust.
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
[easy-complete](https://github.com/chen86860/easy-complete) to Linux and macOS, as a
single binary that works in any terminal: it draws the popup inside the
terminal itself.

- **~740 commands** out of the box: git, docker, kubectl, cargo, npm, aws, gh, terraform,
  systemctl, apt, dnf, asdf, ghostty… (the [withfig/autocomplete](https://github.com/withfig/autocomplete)
  specs converted to JSON, plus hand-written ones, all embedded in the binary).
- **Live values**: git branches, docker containers, systemd units, ssh hosts, make targets,
  npm scripts, zoxide directories, PIDs…
- **Package search as you type**: npm, crates.io, Docker Hub, PyPI and the Snap Store.
- **Any other command** gets its options, subcommands and argument values from its
  `--help` or man page.
- Knows your **aliases, functions and builtins** (`gco ` completes like `git checkout `),
  and your **environment variables** after `$` (`$HO` → `$HOME`, with its value).
- **zsh, bash and fish**, including fish started inside another shell.
- Works inside **tmux** and **zellij**; zellij panes are restored with the app that ran in them.
- **Over ssh**: `lig ssh` brings the popup to a server without installing anything there,
  and `scp`/`rsync` complete paths on the host.
- Nerd Font icons, themes, configurable size, and any keys you like.
- Runs locally: no daemon, no account, no telemetry.

## Install

Lig runs on Linux and macOS 11+ (Apple Silicon and Intel) with zsh ≥ 5.3, bash ≥ 4.4 or
fish ≥ 4.2.1, and on Windows 10+ with PowerShell or cmd. On macOS, use the bundled zsh or a
current bash or fish (Apple's bash 3.2 is too old). Optional: `curl` for online package
search, a [Nerd Font](https://www.nerdfonts.com) for icons.

Install it one of these ways, then [set up your shell](#set-up-your-shell).

### Install script

```bash
curl -fsSL https://github.com/KevenGoncalves/lig/releases/latest/download/install.sh | sh
```

The script downloads the latest release for your machine (x86_64/ARM64 static Linux, or
Apple Silicon/Intel macOS), verifies its SHA-256 checksum, and puts `lig` in
`~/.local/bin`. Set `LIG_INSTALL_DIR` to install elsewhere, or `LIG_VERSION` for an older
release (`… | LIG_VERSION=1.0.6 sh`).

### Homebrew (macOS and Linux)

```bash
brew install kevengoncalves/tap/lig
```

### Debian, Ubuntu (APT)

```bash
sudo install -d /etc/apt/keyrings
curl -fsSL https://kevengoncalves.github.io/lig/apt/lig.gpg | sudo tee /etc/apt/keyrings/lig.gpg >/dev/null
echo "deb [signed-by=/etc/apt/keyrings/lig.gpg] https://kevengoncalves.github.io/lig/apt stable main" | sudo tee /etc/apt/sources.list.d/lig.list
sudo apt update && sudo apt install lig
```

### Fedora, RHEL (Copr)

```bash
sudo dnf copr enable kevengoncalves/lig
sudo dnf install lig
```

Built from source for Fedora and EPEL 9/10.

### Windows (PowerShell and cmd)

```powershell
irm https://github.com/KevenGoncalves/lig/releases/latest/download/install.ps1 | iex
lig install            # PowerShell 7 (pwsh), or: lig install powershell (Windows PowerShell 5.1)
lig install cmd        # a "Command Prompt (lig)" profile in Windows Terminal
```

The script installs `lig.exe` in `%LOCALAPPDATA%\Programs\lig` and adds it to your `PATH`.
`lig install` adds a line at the top of your PowerShell profile that starts PowerShell inside
lig; scripts (`pwsh -Command`, `-File`) and other hosts (VS Code's extension) start as usual.
cmd has no configuration file, so `lig install cmd` adds a Windows Terminal profile that runs
`lig shell --shell cmd.exe` instead. Any terminal works the same way: run
`lig shell --shell pwsh` (or `powershell`, `cmd`) as its command.

Lig needs Windows 10 1809 or newer (for ConPTY), and PSReadLine, which PowerShell loads by
default. In cmd, lig reads the line off the screen and types the completions as keys.

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

Lig is added to every shell home-manager manages, so you can skip `lig install`. To leave a
shell out, set its option to `false`: `programs.lig.enableFishIntegration`,
`enableZshIntegration` or `enableBashIntegration`.

### From source

With [Rust](https://rustup.rs) 1.85 or newer:

```bash
git clone https://github.com/KevenGoncalves/lig.git
cd lig
cargo install --path .
```

### Manual download

Each [release](https://github.com/KevenGoncalves/lig/releases/latest) has these files, each
with a `.sha256`:

| File | Runs on |
| --- | --- |
| `lig-<version>-aarch64-apple-darwin.tar.gz` | Apple Silicon macOS (unsigned CLI binary) |
| `lig-<version>-x86_64-apple-darwin.tar.gz` | Intel macOS (unsigned CLI binary) |
| `lig-<version>-x86_64-unknown-linux-musl.tar.gz` | any x86_64 Linux (static binary) |
| `lig-<version>-aarch64-unknown-linux-musl.tar.gz` | any ARM64 Linux (static binary) |
| `lig-<version>-x86_64-unknown-linux-gnu.tar.gz` | x86_64 with glibc 2.35+ (Ubuntu 22.04, Debian 12 and newer) |
| `lig-<version>-aarch64-unknown-linux-gnu.tar.gz` | ARM64 with glibc 2.35+ |
| `lig_<version>_amd64.deb`, `lig_<version>_arm64.deb` | Debian, Ubuntu: `sudo apt install ./lig_<version>_amd64.deb` |
| `lig-<version>-1.x86_64.rpm`, `lig-<version>-1.aarch64.rpm` | Fedora, RHEL: `sudo dnf install ./lig-<version>-1.x86_64.rpm` |

The `.deb` and `.rpm` packages hold the static build and put `lig` in `/usr/bin`.

### Set up your shell

```bash
lig install            # your $SHELL; or: lig install zsh / bash / fish
```

Run it as your user, also after installing a package. Open a new terminal, then check the
setup with `lig doctor`.

`lig install` adds a small block at the top and one at the bottom of your shell config
(`~/.zshrc`, `~/.bashrc` or `~/.config/fish/config.fish`) and keeps the original as
`<config>.lig-backup`. On macOS, `lig install bash` also makes your login file
(`~/.bash_profile`) source `~/.bashrc`, because Terminal starts bash as a login shell. To
set it up by hand instead:

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

- **Update**: a lig from a package manager (Homebrew, apt, dnf, Nix) is updated there, and
  `lig update` tells you the command. Otherwise `lig update` installs the latest release
  from GitHub over the current binary, checked against its SHA-256, and
  `lig update --check` only tells you whether there's a newer one. Then open a new terminal
  tab: a tab keeps the version it started with.
- **Skip it for one shell**: `LIG_DISABLE=1 zsh`.
- **Remove it**: `lig uninstall` (or `lig uninstall fish` / `bash` / `zsh`). Run it before
  removing a packaged lig, so your shell config stops calling it.

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
- **`Enter`** completes the word you're typing (`go bui` → `go build`, `source .venv` →
  `source .venv/`), or picks the first suggestion after a space (`git checkout ` →
  `git checkout main`). If the word is already complete (`git status`, `ls -l`), it runs
  the command. `Enter` `Enter` means "pick, then run". To run only once the popup is
  closed, set `enter_accepts = "always"`: `Enter` on a complete word closes the popup.
- **No space is added** after a pick, options included (`--force`). Type the space
  yourself to see what comes next.
- **Files first for commands that open them** (`code`, `vim`, `nvim`, `cat`, `less`,
  `xdg-open`…): their files and folders come before their options. Other commands list
  options first. Add commands with `files_first_commands`.
- **Folders** (`cd`, `mkdir`, …): picking one keeps the popup open with its subfolders.
  The first entries are **↵ Enter** (run the command) and **Back** (one folder up). Path
  options like `cargo install --path` also offer `.`, the current folder.
- **`$` completes exported variables** (`$HO`, `"${HO`), with the value as the description.
  Values of names with `TOKEN`, `SECRET`, `PASS`, `KEY`, `CREDENTIAL`, `PRIVATE` or
  `COOKIE` show as `••••••`.
- **History and the shell's own completion stay yours**: while you browse history with the
  arrows, use `Tab` after closing the popup, or run a tool like fzf's `Ctrl+T`, Lig stays
  out of the way. Typing brings it back.

## Configuration

Settings live in `~/.config/lig/config.toml` (or `$XDG_CONFIG_HOME/lig/config.toml`). On
macOS, `~/Library/Application Support/lig/` is still used if it exists and `~/.config/lig`
doesn't. `lig config` prints every option with its default value. The ones you're most
likely to change:

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
enter_accepts = "auto"     # "always" (run only once the popup is closed), "after-space", "never"
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

1. **Your specs** in `~/.config/lig/specs/<command>.json` or `.toml` (same
   directory as `config.toml`).
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
[Fig spec format](https://fig.io/docs/reference) (static parts only). To change the
suggestions of a command lig already knows, start from its spec:

```bash
lig specs new docker
```

That writes `docker.json` in your specs folder from the built-in spec (or from `--help` for
other commands, or with `--from-help`), and lig uses it from then on. For a new command,
write one yourself in the same folder, e.g. `deploy.toml`:

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

`lig specs check` reports what lig can't use in your specs (misspelled keys, unknown
icons, templates or generators), and `lig complete "deploy -e "` shows the suggestions.

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

On Linux, inside zellij, `lig shell` is the pane's process, so zellij's session
resurrection would bring every pane back as `lig shell` in the folder lig started in. To
avoid that, lig mirrors the foreground app into its own command line and working
directory, once a second. zellij then saves each pane as the app that runs in it
(`nvim foo`, `htop`, an idle shell as a plain pane) with the right folder, and restores
it that way.

On macOS, Lig uses zellij's `action set-resurrect-command` API to supply exact
arguments and working directories without rewriting its process memory. This API
was verified on zellij's development branch (commit `d06f87aca7151e032d0f10befb61f1a6df365a86`)
and is **not available in zellij 0.45.1**. Lig detects the capability at runtime:
older versions still support autocomplete, but macOS session resurrection requires
a zellij build with this API. `lig doctor` reports whether the integration is active,
unsupported, or failing. Use the same default shell in zellij and `$SHELL` so idle
panes restore as plain shells. Excluded commands and commands larger than about 4 KiB
restore as the shell; macOS also falls back for non-UTF-8 arguments. A non-UTF-8 or
unreadable working directory restores a shell instead of the application (at the nearest
UTF-8 ancestor, or the shell's own directory).

```bash
lig doctor                  # "[ok  ] zellij mirror: on, ..." (Linux) or "... explicit resurrection API" (macOS)
LIG_NO_MIRROR=1 zellij      # turn the mirror off; autocomplete still works
```

Outside zellij nothing changes. `LIG_DISABLE=1` turns lig off entirely. Shells, wrappers
(`env`, `nohup`, `timeout`, ...) and commands that are risky to rerun (`sudo`, `rm`, `mv`,
`dd`, `kill`, `git`, ...) are saved as a plain shell.

**Never resurrect with `zellij attach --force-run-commands`.** zellij's resurrect prompt
waits for ENTER in each command pane, and that is the safety net.

Known limitations:

- On Linux, `pkill -f <app>` inside zellij also matches the lig pane and closes it.
  `pkill <app>`, `killall <app>` and `pgrep -x <app>` don't, because lig's process name
  stays `lig` (`htop` still shows the pane as the app).
- A pipeline is saved by its first command (`cat log | less` is saved as `cat log`).
- A terminal started from a zellij pane inherits `$ZELLIJ`, so the mirror runs there too.
  On Linux this is harmless apart from how `ps` shows lig. On macOS that lig, like any lig
  nested in a pane, sees the pane's id and leaves its resurrect command to the outer lig.
- A restored app runs with the zellij server's `PATH`, like plain zellij.

## ssh

**Remote paths.** `scp` and `rsync` complete paths on the host after `host:`
(`scp notes.txt pi:Doc` → `pi:Documents/`). Lig lists them with `ssh host ls`, so it needs
a login that doesn't ask for a password (a key or ssh-agent); otherwise you just get no
suggestions. Without a key, an open connection works too: with this in `~/.ssh/config`,
one `ssh pi` with your password makes the listings work, and fast, for 10 minutes.

```
Host pi
  ControlMaster auto
  ControlPath ~/.ssh/cm-%C
  ControlPersist 10m
```

**The popup on the host.** `lig ssh` logs in like `ssh` and brings the popup along:

```bash
lig ssh admin@pi
```

It asks for your password like `ssh` if the host needs one, and nothing is installed on
the host: Lig sends its hooks with the login, your zsh or bash there loads them from a temp
folder after your own startup files, and removes the folder at its first prompt.
Suggestions are made on your machine; files, `$PATH` commands and generators like git
branches come from the host, over the same connection (no second login). It takes any ssh
options, and is plain `ssh` outside Lig or with a remote command (`lig ssh pi uptime`), so
`alias ssh='lig ssh'` is safe.

On the host:

- The shell must be zsh or bash (others start as usual, without the popup), with `sh`,
  `base64` and `mktemp`, which any Linux has.
- Commands get no suggestions from their `--help`, and generators that read project files
  (npm scripts, make and Cargo targets) don't see the host's files.

## Troubleshooting

| Problem | Try |
| --- | --- |
| No popup at all | `lig doctor`: "running inside lig" must be ok. Open a new tab after installing or updating. |
| Wrong or missing suggestions | `lig complete "<the line>"` shows what Lig would suggest. |
| A key binding does nothing | `lig keys` shows whether the key reaches Lig, and its name. |
| A command's suggestions look outdated | `lig specs clear-cache` re-reads `--help` output. |
| `clear` or `vim` complain about the terminal over plain `ssh` | The host lacks your terminal's terminfo: `infocmp -x xterm-ghostty \| ssh host -- tic -x -` (`lig ssh` falls back to `xterm-256color` by itself). |
| No `scp host:` paths | The host needs a login without a password prompt (a key, ssh-agent, or an open `ControlMaster` connection). |
| Something else | Start a shell with `LIG_LOG=1 zsh` (or `bash` / `fish`) and check `~/.local/state/lig/lig.log` (Linux), or `$TMPDIR/lig/lig.log` (macOS). |

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
python3 tests/zellij_mirror.py  # Linux mirror (needs zsh, not zellij)
python3 tests/zellij_macos.py   # macOS mirror contracts (needs zsh)
python3 tests/install.py        # offline installer/checksum tests
# macOS end-to-end resurrection, with a zellij build supporting the new API:
LIG_TEST_ZELLIJ=/path/to/zellij python3 tests/zellij_resurrection.py
tools/update-fig-specs.sh       # regenerate specs/fig/ from withfig/autocomplete
```

CI runs unit and shell checks on Linux and on Apple Silicon and Intel macOS.
The PTY tests need Python 3 and the shells they test; the tmux tests use a private server.
They test `target/release/lig`; set `LIG_TEST_BIN` to use another binary.

```
src/                  the lig binary (session.rs: PTY + popup, engine.rs: completion,
                      popup.rs: drawing, help.rs: --help parser, specs/gens.rs: native generators,
                      remote.rs: lig ssh, process/: Linux and macOS process queries,
                      mirror.rs / mirror_macos.rs: zellij resurrection)
shell/                zsh, bash and fish hooks; remote.* bootstrap them for `lig ssh`
specs/lig/            hand-written specs (JSON)
specs/fig/            converted withfig/autocomplete specs (generated)
build.rs              embeds both spec folders, compressed, into the binary
tests/                PTY tests (Python)
tools/figconv/        the Fig → JSON converter (development only)
flake.nix, nix/       the Nix flake and home-manager module
install.sh            the installer attached to every release
packaging/            .deb/.rpm, Homebrew, AUR, Copr and APT packaging (see packaging/README.md)
.github/workflows/    CI and release builds
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
- [@ammachado](https://github.com/ammachado): Apple Silicon macOS support and zellij
  resurrection on macOS ([#18](https://github.com/KevenGoncalves/lig/pull/18)).

The specs in `specs/fig/` come from [withfig/autocomplete](https://github.com/withfig/autocomplete),
© Hercules Labs Inc. and contributors, MIT licensed (see [`specs/fig/LICENSE`](specs/fig/LICENSE)).

Lig is MIT licensed, see [`LICENSE`](LICENSE).
