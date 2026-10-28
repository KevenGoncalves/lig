//! Popup color themes: presets plus per-color overrides from `[colors]`.

use crate::config::{Colors, Ui, sgr_color};

/// A preset. Colors are a 0-255 palette index, `#rrggbb`, or `default` (the terminal's own).
struct Palette {
    bg: &'static str,
    fg: &'static str,
    selected_bg: &'static str,
    selected_fg: &'static str,
    description_fg: &'static str,
    selected_description_fg: &'static str,
    match_fg: &'static str,
    border_fg: &'static str,
    border_bg: &'static str,
    scrollbar_fg: &'static str,
    scrollbar_thumb: &'static str,
    info_fg: &'static str,
}

pub const NAMES: &[&str] = &["default", "catppuccin", "nord", "dracula", "gruvbox", "tokyonight", "light", "terminal"];

fn preset(name: &str) -> Palette {
    match name {
        "catppuccin" => Palette {
            bg: "#1e1e2e",
            fg: "#cdd6f4",
            selected_bg: "#45475a",
            selected_fg: "#cdd6f4",
            description_fg: "#7f849c",
            selected_description_fg: "#bac2de",
            match_fg: "#89b4fa",
            border_fg: "#6c7086",
            border_bg: "default",
            scrollbar_fg: "#45475a",
            scrollbar_thumb: "#9399b2",
            info_fg: "#7f849c",
        },
        "nord" => Palette {
            bg: "#2e3440",
            fg: "#d8dee9",
            selected_bg: "#434c5e",
            selected_fg: "#eceff4",
            description_fg: "#7b88a1",
            selected_description_fg: "#d8dee9",
            match_fg: "#88c0d0",
            border_fg: "#4c566a",
            border_bg: "default",
            scrollbar_fg: "#3b4252",
            scrollbar_thumb: "#81a1c1",
            info_fg: "#616e88",
        },
        "dracula" => Palette {
            bg: "#282a36",
            fg: "#f8f8f2",
            selected_bg: "#44475a",
            selected_fg: "#f8f8f2",
            description_fg: "#6272a4",
            selected_description_fg: "#c6c6c6",
            match_fg: "#bd93f9",
            border_fg: "#6272a4",
            border_bg: "default",
            scrollbar_fg: "#44475a",
            scrollbar_thumb: "#bd93f9",
            info_fg: "#6272a4",
        },
        "gruvbox" => Palette {
            bg: "#282828",
            fg: "#ebdbb2",
            selected_bg: "#504945",
            selected_fg: "#fbf1c7",
            description_fg: "#928374",
            selected_description_fg: "#d5c4a1",
            match_fg: "#fabd2f",
            border_fg: "#665c54",
            border_bg: "default",
            scrollbar_fg: "#3c3836",
            scrollbar_thumb: "#a89984",
            info_fg: "#928374",
        },
        "tokyonight" => Palette {
            bg: "#1a1b26",
            fg: "#c0caf5",
            selected_bg: "#283457",
            selected_fg: "#c0caf5",
            description_fg: "#565f89",
            selected_description_fg: "#a9b1d6",
            match_fg: "#7aa2f7",
            border_fg: "#3b4261",
            border_bg: "default",
            scrollbar_fg: "#292e42",
            scrollbar_thumb: "#565f89",
            info_fg: "#565f89",
        },
        "light" => Palette {
            bg: "#f6f8fa",
            fg: "#1f2328",
            selected_bg: "#0969da",
            selected_fg: "#ffffff",
            description_fg: "#656d76",
            selected_description_fg: "#dbe9ff",
            match_fg: "#0550ae",
            border_fg: "#afb8c1",
            border_bg: "default",
            scrollbar_fg: "#eaeef2",
            scrollbar_thumb: "#8c959f",
            info_fg: "#656d76",
        },
        // Follows the terminal's own color scheme (ANSI colors).
        "terminal" => Palette {
            bg: "default",
            fg: "default",
            selected_bg: "8",
            selected_fg: "15",
            description_fg: "8",
            selected_description_fg: "7",
            match_fg: "4",
            border_fg: "8",
            border_bg: "default",
            scrollbar_fg: "0",
            scrollbar_thumb: "7",
            info_fg: "8",
        },
        _ => Palette {
            bg: "235",
            fg: "252",
            selected_bg: "24",
            selected_fg: "231",
            description_fg: "244",
            selected_description_fg: "252",
            match_fg: "117",
            border_fg: "240",
            border_bg: "default",
            scrollbar_fg: "237",
            scrollbar_thumb: "245",
            info_fg: "244",
        },
    }
}

/// SGR parameters ready to use (`48;2;r;g;b`, `38;5;n`, …).
pub struct Theme {
    pub bg: String,
    pub fg: String,
    pub sel_bg: String,
    pub sel_fg: String,
    pub desc_fg: String,
    pub sel_desc_fg: String,
    pub match_fg: String,
    pub border_fg: String,
    pub border_bg: String,
    pub track_fg: String,
    pub thumb_fg: String,
    pub info_fg: String,
}

pub fn resolve(ui: &Ui) -> Theme {
    let p = preset(&ui.theme);
    let c: &Colors = &ui.colors;
    let pick = |o: &Option<String>, d: &str| o.clone().unwrap_or_else(|| d.to_string());
    Theme {
        bg: sgr_color(&pick(&c.bg, p.bg), 48),
        fg: sgr_color(&pick(&c.fg, p.fg), 38),
        sel_bg: sgr_color(&pick(&c.selected_bg, p.selected_bg), 48),
        sel_fg: sgr_color(&pick(&c.selected_fg, p.selected_fg), 38),
        desc_fg: sgr_color(&pick(&c.description_fg, p.description_fg), 38),
        sel_desc_fg: sgr_color(&pick(&c.selected_description_fg, p.selected_description_fg), 38),
        match_fg: sgr_color(&pick(&c.match_fg, p.match_fg), 38),
        border_fg: sgr_color(&pick(&c.border_fg, p.border_fg), 38),
        border_bg: sgr_color(&pick(&c.border_bg, p.border_bg), 48),
        track_fg: sgr_color(&pick(&c.scrollbar_fg, p.scrollbar_fg), 38),
        thumb_fg: sgr_color(&pick(&c.scrollbar_thumb, p.scrollbar_thumb), 38),
        info_fg: sgr_color(&pick(&c.info_fg, p.info_fg), 38),
    }
}
