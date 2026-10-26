//! `~/.config/lig/config.toml`

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

pub fn config_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from("~/.config")).join("lig")
}

pub fn config_path() -> PathBuf {
    config_dir().join("config.toml")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
#[derive(Default)]
pub struct Config {
    pub ui: Ui,
    /// Override any color of the theme.
    pub colors: Colors,
    pub keys: Keys,
    pub behavior: Behavior,
    /// Icon overrides by name: a kind or icon name (`folder`, `git-branch`;
    /// `lig icons` lists them), `"*.rs"` for files by extension, or a file name.
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub icons: BTreeMap<String, IconOverride>,
}

/// An `[icons]` entry: just a glyph (`folder = "📁"`), or a glyph and/or a
/// color (`option = { color = "214" }`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum IconOverride {
    Glyph(String),
    Style(IconStyle),
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IconStyle {
    pub glyph: Option<String>,
    /// `"#rrggbb"`, a 256-color number, or `"default"`.
    pub color: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Ui {
    /// Popup width: `"auto"` (fit the content), columns (`60`), or a
    /// percentage of the terminal (`"50%"`).
    pub width: Size,
    /// Narrowest popup with `width = "auto"`.
    pub min_width: u16,
    /// Widest popup with `width = "auto"`: columns or `"80%"`.
    pub max_width: Size,
    /// How many suggestions are listed at most: rows (`8`), or a percentage
    /// of the terminal height (`"30%"`, border included).
    #[serde(alias = "max_rows")]
    pub height: Size,
    /// Keep at least this many rows (padded), so the popup's size is steadier.
    pub min_height: u16,
    /// Show descriptions.
    pub descriptions: bool,
    /// `"right"`: a column next to each name; `"bottom"`: the selected
    /// item's full description in a frame under the list.
    pub description_position: String,
    /// Lines of the bottom description frame (at most).
    pub description_lines: u16,
    /// `"auto"` (Nerd Font glyphs when the terminal/fonts have them),
    /// `"nerd"`, `"unicode"` or `"none"`.
    pub icons: String,
    /// `"rounded"`, `"square"` or `"none"`.
    pub border: String,
    /// `"compact"`, `"normal"` or `"relaxed"` (most space).
    pub density: String,
    /// `default`, `catppuccin`, `nord`, `dracula`, `gruvbox`, `tokyonight`,
    /// `light` or `terminal` (your terminal's own colors).
    pub theme: String,
    /// The `[colors]` section, kept here for drawing (see [`Config::load`]).
    /// `[ui.colors]` is still read for older configs, but no longer written.
    #[serde(skip_serializing)]
    pub colors: Colors,
}

/// A size: a number of cells, a percentage of the terminal (`"50%"`), or `"auto"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Size {
    Cells(u16),
    Text(String),
}

impl Size {
    /// The size in cells for a terminal dimension of `total`; `None` for
    /// `"auto"` (or anything unrecognised).
    pub fn resolve(&self, total: u16) -> Option<u16> {
        match self {
            Size::Cells(n) => Some(*n),
            Size::Text(t) => {
                let t = t.trim();
                match t.strip_suffix('%') {
                    Some(p) => p.trim().parse::<f32>().ok().map(|p| (f32::from(total) * p / 100.0).round() as u16),
                    None => t.parse().ok(),
                }
            }
        }
    }

    pub fn is_percent(&self) -> bool {
        matches!(self, Size::Text(t) if t.trim().ends_with('%'))
    }
}

/// Colors: a 0-255 palette index, `"#rrggbb"`, or `"default"` (the
/// terminal's own). Unset ones come from the theme.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Colors {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_bg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description_fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selected_description_fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub match_fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub border_bg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollbar_fg: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scrollbar_thumb: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub info_fg: Option<String>,
}

impl Colors {
    /// These colors, with `older`'s where these are unset.
    fn or(self, older: Colors) -> Colors {
        Colors {
            bg: self.bg.or(older.bg),
            fg: self.fg.or(older.fg),
            selected_bg: self.selected_bg.or(older.selected_bg),
            selected_fg: self.selected_fg.or(older.selected_fg),
            description_fg: self.description_fg.or(older.description_fg),
            selected_description_fg: self.selected_description_fg.or(older.selected_description_fg),
            match_fg: self.match_fg.or(older.match_fg),
            border_fg: self.border_fg.or(older.border_fg),
            border_bg: self.border_bg.or(older.border_bg),
            scrollbar_fg: self.scrollbar_fg.or(older.scrollbar_fg),
            scrollbar_thumb: self.scrollbar_thumb.or(older.scrollbar_thumb),
            info_fg: self.info_fg.or(older.info_fg),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Keys {
    pub accept: Vec<String>,
    pub next: Vec<String>,
    pub prev: Vec<String>,
    pub dismiss: Vec<String>,
    /// Show the popup now / hide it.
    pub toggle: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Behavior {
    /// Also match suggestions by subsequence (`gco` → `git-checkout`).
    pub fuzzy: bool,
    /// When Enter picks the selected suggestion instead of running the line:
    /// `"auto"` (unless the word typed is already complete: `git status`),
    /// `"after-space"` (only when the line ends in a space), `"always"` or
    /// `"never"`. A selection moved with Tab/arrows is always picked; the
    /// Enter right after a pick runs the line.
    pub enter_accepts: String,
    /// Add a space after an accepted suggestion (`checkout ` vs `checkout`).
    pub insert_space: bool,
    /// List files before options and subcommands.
    pub files_first: bool,
    /// Commands that open files (editors, viewers, players…): their files and
    /// folders come before their options, as if `files_first` were on.
    pub files_first_commands: Vec<String>,
    /// Show suggestions right after a space (empty current token).
    pub show_on_empty_token: bool,
    /// Pop up automatically for the first word (command names). Off by
    /// default so short commands can be typed and run with one Enter; the
    /// toggle key (Ctrl+Space) always shows them.
    pub complete_commands: bool,
    /// Search package registries while you type (npm, crates.io, Docker Hub,
    /// PyPI versions, Snap Store). What you type for the package name is sent
    /// to them.
    pub online: bool,
    /// Kill generator commands after this many milliseconds.
    pub generator_timeout_ms: u64,
    /// Cache generator results for this long when the spec doesn't say.
    pub generator_cache_ms: u64,
    /// Commands for which lig never shows suggestions.
    pub disabled_commands: Vec<String>,
    /// Build specs for other commands from their `--help` output / man page.
    pub help_parsing: bool,
    /// Kill `--help` / `man` after this many milliseconds.
    pub help_timeout_ms: u64,
    /// Never run `<command> --help` for these.
    pub help_skip: Vec<String>,
}

impl Default for Ui {
    fn default() -> Self {
        Ui {
            width: Size::Text("auto".into()),
            min_width: 30,
            max_width: Size::Cells(100),
            height: Size::Cells(8),
            min_height: 0,
            descriptions: true,
            description_position: "right".into(),
            description_lines: 3,
            icons: "auto".into(),
            border: "rounded".into(),
            density: "normal".into(),
            theme: "default".into(),
            colors: Colors::default(),
        }
    }
}

impl Default for Keys {
    fn default() -> Self {
        let v = |a: &[&str]| a.iter().map(|s| s.to_string()).collect();
        Keys {
            accept: v(&["enter"]),
            next: v(&["tab", "down", "ctrl-n"]),
            prev: v(&["shift-tab", "up", "ctrl-p"]),
            dismiss: v(&["esc"]),
            toggle: v(&["ctrl-space"]),
        }
    }
}

/// The default `files_first_commands`.
#[rustfmt::skip]
const FILES_FIRST_COMMANDS: &[&str] = &[
    // Editors
    "code", "code-insiders", "codium", "cursor", "zed", "subl", "atom", "gedit", "gnome-text-editor",
    "kate", "kwrite", "mousepad", "xed", "pluma", "vim", "vi", "nvim", "gvim", "view", "vimdiff", "nano",
    "pico", "micro", "emacs", "emacsclient", "hx", "helix", "kak", "joe", "ne", "mcedit",
    // Viewers and pagers
    "cat", "bat", "batcat", "less", "more", "most", "head", "tail", "glow", "xxd", "hexdump",
    // Opening files and folders
    "open", "xdg-open", "gio", "nautilus", "dolphin", "thunar", "nemo", "pcmanfm",
    // Images, documents and media
    "eog", "feh", "viu", "sxiv", "nsxiv", "imv", "gimp", "inkscape", "evince", "okular", "zathura",
    "xreader", "libreoffice", "soffice", "mpv", "vlc", "totem", "celluloid",
];

impl Default for Behavior {
    fn default() -> Self {
        Behavior {
            fuzzy: true,
            enter_accepts: "auto".into(),
            insert_space: false,
            files_first: false,
            files_first_commands: FILES_FIRST_COMMANDS.iter().map(|c| c.to_string()).collect(),
            show_on_empty_token: true,
            complete_commands: false,
            online: true,
            generator_timeout_ms: 5000,
            generator_cache_ms: 5000,
            disabled_commands: vec![],
            help_parsing: true,
            help_timeout_ms: 2000,
            help_skip: vec![],
        }
    }
}

impl Config {
    /// Load the config file; problems are reported and defaults are used.
    pub fn load() -> Config {
        let path = config_path();
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Config>(&text) {
                Ok(mut c) => {
                    // `[colors]`, over the older `[ui.colors]`; drawing reads them from `ui`.
                    c.ui.colors = c.colors.clone().or(std::mem::take(&mut c.ui.colors));
                    c
                }
                Err(e) => {
                    eprintln!("lig: ignoring {}: {e}", path.display());
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }
}

/// SGR parameters for a color spec, as foreground (`38`) or background (`48`).
pub fn sgr_color(spec: &str, base: u8) -> String {
    let s = spec.trim();
    if let Some(hex) = s.strip_prefix('#')
        && hex.len() == 6
        && let Ok(n) = u32::from_str_radix(hex, 16)
    {
        return format!("{base};2;{};{};{}", n >> 16, (n >> 8) & 0xff, n & 0xff);
    }
    match s.parse::<u8>() {
        Ok(n) => format!("{base};5;{n}"),
        Err(_) => format!("{}", base + 1), // "default": the terminal's color
    }
}
