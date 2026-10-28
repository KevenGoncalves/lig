//! Draws the suggestion popup and restores what was underneath from the vt100 screen model.

use std::io::Write;

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::config::Ui;
use crate::engine::{Kind, Suggestion};
use crate::theme::{self, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rect {
    pub row: u16,
    pub col: u16,
    pub height: u16,
    pub width: u16,
}

/// Top-left, top-right, bottom-left, bottom-right, horizontal, vertical.
type Border = [&'static str; 6];
const ROUNDED: Border = ["╭", "╮", "╰", "╯", "─", "│"];
const SQUARE: Border = ["┌", "┐", "└", "┘", "─", "│"];

#[derive(Clone, Copy)]
struct Style {
    border: Option<Border>,
    pad_l: u16,
    pad_r: u16,
    /// Blank rows above and below the list.
    pad_v: u16,
    /// Between the name and description columns.
    gap: u16,
    icon_gap: u16,
    desc_max: u16,
}

fn style(ui: &Ui) -> Style {
    let border = match ui.border.as_str() {
        "none" => None,
        "square" => Some(SQUARE),
        _ => Some(ROUNDED),
    };
    match ui.density.as_str() {
        "compact" => Style { border, pad_l: 1, pad_r: 1, pad_v: 0, gap: 2, icon_gap: 1, desc_max: 40 },
        "relaxed" => Style { border, pad_l: 3, pad_r: 3, pad_v: 1, gap: 6, icon_gap: 2, desc_max: u16::MAX },
        _ => Style { border, pad_l: 2, pad_r: 2, pad_v: 0, gap: 4, icon_gap: 1, desc_max: u16::MAX },
    }
}

pub struct Layout {
    pub rect: Rect,
    style: Style,
    /// Column widths: icon (with its gap), name, description (0: hidden).
    icon_w: u16,
    name_w: u16,
    desc_w: u16,
    /// Text lines of the bottom description frame (0: none).
    frame_lines: u16,
}

impl Layout {
    fn border_w(&self) -> u16 {
        u16::from(self.style.border.is_some())
    }

    /// Border and padding rows.
    fn chrome_h(&self) -> u16 {
        2 * (self.border_w() + self.style.pad_v)
    }

    /// Rows of the bottom description frame, borders included.
    fn frame_h(&self) -> u16 {
        frame_height(self.frame_lines, self.border_w())
    }

    /// Rows available for suggestions.
    pub fn visible(&self) -> usize {
        (self.rect.height - self.chrome_h() - self.frame_h()) as usize
    }
}

/// Rows of a description frame: a box, or (borderless) a separator line above the text.
fn frame_height(lines: u16, border_w: u16) -> u16 {
    if lines == 0 { 0 } else { lines + if border_w == 1 { 2 } else { 1 } }
}

/// Word-wrap to lines of at most `w` columns.
fn wrap(text: &str, w: usize) -> Vec<String> {
    let mut lines: Vec<String> = vec![];
    let mut cur = String::new();
    for word in text.split_whitespace() {
        let mut word = word.to_string();
        // Hard-split words longer than a line.
        while word.width() > w && w > 0 {
            if !cur.is_empty() {
                lines.push(std::mem::take(&mut cur));
            }
            let mut head = String::new();
            let mut rest = String::new();
            for c in word.chars() {
                if rest.is_empty() && head.width() + c.width().unwrap_or(0) <= w {
                    head.push(c);
                } else {
                    rest.push(c);
                }
            }
            lines.push(head);
            word = rest;
        }
        let extra = if cur.is_empty() { 0 } else { 1 };
        if cur.width() + extra + word.width() > w && !cur.is_empty() {
            lines.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(' ');
        }
        cur.push_str(&word);
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    lines
}

/// The selected item's description as exactly `n` lines of width `w`.
fn frame_text(desc: &str, w: usize, n: usize) -> Vec<String> {
    let mut lines = wrap(desc, w);
    if lines.len() > n {
        lines.truncate(n);
        if let Some(last) = lines.last_mut() {
            *last = fit(&format!("{last} …"), w).trim_end().to_string();
            if !last.ends_with('…') {
                *last = fit(&format!("{last}…"), w).trim_end().to_string();
            }
        }
    }
    lines.resize(n, String::new());
    lines
}

/// Truncate to `w` columns (adding `…` when cut) and pad to exactly `w`.
fn fit(s: &str, w: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    if s.width() <= w {
        out.push_str(s);
        used = s.width();
    } else if w > 0 {
        for c in s.chars() {
            let cw = c.width().unwrap_or(0);
            if used + cw > w - 1 {
                break;
            }
            out.push(c);
            used += cw;
        }
        out.push('…');
        used += 1;
    }
    out.extend(std::iter::repeat_n(' ', w.saturating_sub(used)));
    out
}

/// Rows below the cursor (besides the border) enough to keep the popup below.
const MIN_BELOW: u16 = 4;
/// Preferred text width of the bottom description frame.
const BOTTOM_DESC_W: u16 = 56;
/// A description column narrower than this isn't worth showing.
const MIN_DESC: u16 = 8;

/// Where the popup goes for this screen/cursor, or `None` if it can't fit.
pub fn layout(ui: &Ui, items: &[Suggestion], size: (u16, u16), cursor: (u16, u16), token_width: u16) -> Option<Layout> {
    layout_at(ui, items, size, cursor, cursor.0, token_width)
}

/// Like [`layout`], for input that starts on row `input_top` (a command
/// wrapping over several rows): placed above, the popup goes above all of it.
pub fn layout_at(
    ui: &Ui,
    items: &[Suggestion],
    (rows, cols): (u16, u16),
    (crow, ccol): (u16, u16),
    input_top: u16,
    token_width: u16,
) -> Option<Layout> {
    if items.is_empty() || rows < 3 || cols < 12 {
        return None;
    }
    let style = style(ui);
    let b = u16::from(style.border.is_some());
    // The icon column fits the widest icon shown (emoji from `[icons]` take two columns).
    let icon_w: u16 = if ui.icons == "none" {
        0
    } else {
        let nerd = crate::icons::use_nerd(&ui.icons);
        let widest = items.iter().map(|s| crate::icons::glyph(s.kind, s.icon, &s.label, nerd).0.width()).max();
        widest.unwrap_or(1).clamp(1, 2) as u16 + style.icon_gap
    };
    // Without a border the scrollbar takes its own column.
    let bar = 1 - b;
    let chrome_w = 2 * b + style.pad_l + style.pad_r + icon_w + bar;
    let name_need = (items.iter().map(|s| s.label.width()).max().unwrap_or(1) as u16).clamp(4, 44);
    let bottom = ui.descriptions && ui.description_position == "bottom";
    let longest_desc = items.iter().map(|s| s.description.width()).max().unwrap_or(0) as u16;
    let desc_need = if ui.descriptions && !bottom { longest_desc.min(style.desc_max) } else { 0 };

    // Width: fixed, or fit to the content within `min_width..=max_width`.
    let smallest = (chrome_w + 4).min(cols);
    let max_w = ui.max_width.resolve(cols).unwrap_or(cols).clamp(smallest, cols);
    let mut content_w = chrome_w + name_need + if desc_need > 0 { style.gap + desc_need } else { 0 };
    if bottom {
        content_w = content_w.max(2 * b + style.pad_l + style.pad_r + longest_desc.min(BOTTOM_DESC_W));
    }
    let width = match ui.width.resolve(cols) {
        Some(w) => w,
        None => content_w.clamp(ui.min_width.min(max_w), max_w),
    }
    .clamp(smallest, cols);
    // The name gets space first; the description is dropped when it wouldn't be readable.
    let inner = width - chrome_w;
    let mut name_w = name_need.min(inner);
    let mut desc_w = 0;
    if desc_need > 0 && inner >= name_w + style.gap + MIN_DESC.min(desc_need) {
        desc_w = inner - name_w - style.gap;
    } else {
        name_w = inner;
    }

    // A percent height includes the border; a row count doesn't.
    let chrome_h = 2 * (b + style.pad_v);
    let max_items = match ui.height.resolve(rows) {
        Some(h) if ui.height.is_percent() => h.saturating_sub(chrome_h),
        Some(h) => h,
        None => 8,
    }
    .max(1);
    let mut frame_lines = 0;
    if bottom {
        let text_w = width.saturating_sub(2 * b + style.pad_l + style.pad_r).max(1) as usize;
        let needed = items.iter().map(|s| wrap(&s.description, text_w).len()).max().unwrap_or(0) as u16;
        frame_lines = needed.min(ui.description_lines);
    }
    let frame_h = frame_height(frame_lines, b);
    let want = (items.len() as u16).min(max_items).max(ui.min_height.min(max_items)) + chrome_h + frame_h;
    let below = rows.saturating_sub(crow + 1);
    let above = input_top.min(crow);
    // Prefer below (shrinking, the list scrolls); flip above only when there's hardly any room.
    let (row, height) = if below >= want || below >= want.min(MIN_BELOW + chrome_h) || below >= above {
        (crow + 1, want.min(below))
    } else {
        let h = want.min(above);
        (above - h, h)
    };
    // Not even one suggestion row next to the frame: drop the frame.
    if height <= chrome_h + frame_h {
        frame_lines = 0;
    }
    if height <= chrome_h {
        return None;
    }
    // Align the suggestion text with the start of the token being completed.
    let anchor = ccol.saturating_sub(token_width).saturating_sub(b + style.pad_l + icon_w);
    let col = anchor.min(cols.saturating_sub(width));
    Some(Layout { rect: Rect { row, col, height, width }, style, icon_w, name_w, desc_w, frame_lines })
}

/// The popup as lines of text with SGR codes, each exactly `rect.width` wide.
pub fn render(ui: &Ui, l: &Layout, items: &[Suggestion], selected: usize, scroll: usize) -> Vec<String> {
    let t: Theme = theme::resolve(ui);
    let nerd = crate::icons::use_nerd(&ui.icons);
    let st = l.style;
    let w = l.rect.width as usize;
    let n = items.len();
    let h = l.visible();
    let mut lines = Vec::with_capacity(l.rect.height as usize);

    let (thumb_start, thumb_len) = if n > h {
        let len = (h * h).div_ceil(n).max(1);
        let start = (scroll * h) / n;
        (start.min(h - len), len)
    } else {
        (0, 0)
    };
    let edge = format!("\x1b[0;{};{}m", t.border_bg, t.border_fg);

    if let Some(bd) = st.border {
        lines.push(format!("{edge}{}{}{}\x1b[0m", bd[0], bd[4].repeat(w - 2), bd[1]));
    }
    let blank = |t: &Theme| match st.border {
        Some(bd) => format!("{edge}{}\x1b[0;{}m{}{edge}{}\x1b[0m", bd[5], t.bg, " ".repeat(w - 2), bd[5]),
        None => format!("\x1b[0;{}m{}\x1b[0m", t.bg, " ".repeat(w)),
    };
    for _ in 0..st.pad_v {
        lines.push(blank(&t));
    }
    for i in 0..h {
        let idx = scroll + i;
        let mut line = String::new();
        if let Some(bd) = st.border {
            line.push_str(&format!("{edge}{}", bd[5]));
        }
        let sel = idx == selected;
        let (bg, fg, dfg) = if sel { (&t.sel_bg, &t.sel_fg, &t.sel_desc_fg) } else { (&t.bg, &t.fg, &t.desc_fg) };
        line.push_str(&format!("\x1b[0;{bg};{fg}m{}", " ".repeat(st.pad_l as usize)));
        match items.get(idx) {
            Some(s) => {
                if l.icon_w > 0 {
                    let (glyph, color) = crate::icons::glyph(s.kind, s.icon, &s.label, nerd);
                    let color = if s.kind == Kind::Info { t.info_fg.as_str() } else { color };
                    line.push_str(&format!("\x1b[{color}m{}\x1b[{fg}m", fit(glyph, l.icon_w as usize)));
                }
                let label = fit(&s.label, l.name_w as usize);
                if s.kind == Kind::Info {
                    line.push_str(&format!("\x1b[3;{}m{label}\x1b[23;{fg}m", t.info_fg));
                } else {
                    // The typed prefix, highlighted.
                    let hl = if !s.query.is_empty() && s.label.to_lowercase().starts_with(&s.query.to_lowercase()) {
                        s.query.chars().count().min(label.chars().count())
                    } else {
                        0
                    };
                    let split = label.char_indices().nth(hl).map_or(label.len(), |(b, _)| b);
                    let (a, b) = label.split_at(split);
                    if hl > 0 {
                        let mfg = if sel { fg } else { &t.match_fg };
                        line.push_str(&format!("\x1b[1;{mfg}m{a}\x1b[22;{fg}m{b}"));
                    } else {
                        line.push_str(&label);
                    }
                }
                if l.desc_w > 0 {
                    line.push_str(&format!(
                        "{}\x1b[{dfg}m{}\x1b[{fg}m",
                        " ".repeat(st.gap as usize),
                        fit(&s.description, l.desc_w as usize)
                    ));
                }
            }
            None => {
                let inner = l.icon_w + l.name_w + if l.desc_w > 0 { st.gap + l.desc_w } else { 0 };
                line.push_str(&" ".repeat(inner as usize));
            }
        }
        line.push_str(&" ".repeat(st.pad_r as usize));
        let thumb = thumb_len > 0 && i >= thumb_start && i < thumb_start + thumb_len;
        match st.border {
            Some(_) if thumb => line.push_str(&format!("\x1b[0;{};{}m┃", t.border_bg, t.thumb_fg)),
            Some(bd) => line.push_str(&format!("{edge}{}", bd[5])),
            None if thumb => line.push_str(&format!("\x1b[0;{};{}m▐", t.bg, t.thumb_fg)),
            None if thumb_len > 0 => line.push_str(&format!("\x1b[0;{};{}m▐", t.bg, t.track_fg)),
            None => line.push_str(&format!("\x1b[0;{}m ", t.bg)),
        }
        line.push_str("\x1b[0m");
        lines.push(line);
    }
    for _ in 0..st.pad_v {
        lines.push(blank(&t));
    }
    if let Some(bd) = st.border {
        let counter = if n > h && selected < n { format!(" {}/{} ", selected + 1, n) } else { String::new() };
        let fill = w.saturating_sub(2 + counter.width() + usize::from(!counter.is_empty()));
        let tail =
            if counter.is_empty() { String::new() } else { format!("\x1b[{}m{counter}{edge}{}", t.info_fg, bd[4]) };
        lines.push(format!("{edge}{}{}{tail}{}\x1b[0m", bd[2], bd[4].repeat(fill), bd[3]));
    }
    if l.frame_lines > 0 {
        lines.extend(render_frame(&t, l, items.get(selected)));
    }
    lines
}

fn render_frame(t: &Theme, l: &Layout, item: Option<&Suggestion>) -> Vec<String> {
    let st = l.style;
    let w = l.rect.width as usize;
    let edge = format!("\x1b[0;{};{}m", t.border_bg, t.border_fg);
    let text_w = w - 2 * l.border_w() as usize - (st.pad_l + st.pad_r) as usize;
    let desc = item.map_or("", |s| s.description.as_str());
    let (text, style) = if desc.is_empty() {
        (frame_text("no description", text_w, l.frame_lines as usize), format!("3;{}", t.info_fg))
    } else {
        (frame_text(desc, text_w, l.frame_lines as usize), t.desc_fg.clone())
    };
    let mut out = vec![];
    match st.border {
        Some(bd) => out.push(format!("{edge}{}{}{}\x1b[0m", bd[0], bd[4].repeat(w - 2), bd[1])),
        None => out.push(format!("\x1b[0;{};{}m{}\x1b[0m", t.bg, t.border_fg, "─".repeat(w))),
    }
    for line in text {
        let body = format!(
            "\x1b[0;{};{style}m{}{}{}",
            t.bg,
            " ".repeat(st.pad_l as usize),
            fit(&line, text_w),
            " ".repeat(st.pad_r as usize)
        );
        match st.border {
            Some(bd) => out.push(format!("{edge}{}{body}{edge}{}\x1b[0m", bd[5], bd[5])),
            None => out.push(format!("{body}\x1b[0m")),
        }
    }
    if let Some(bd) = st.border {
        out.push(format!("{edge}{}{}{}\x1b[0m", bd[2], bd[4].repeat(w - 2), bd[3]));
    }
    out
}

/// `lig preview`: a sample popup under a fake prompt, as text.
pub fn preview(ui: &Ui, cols: u16) -> String {
    use crate::spec::Icon;
    let s = |label: &str, kind, icon, desc: &str, query: &str| Suggestion {
        label: label.into(),
        description: desc.into(),
        kind,
        icon,
        insert: String::new(),
        delete: 0,
        back: 0,
        query: query.into(),
    };
    let items = vec![
        s("checkout", Kind::Subcommand, None, "Switch branches or restore working tree files", "ch"),
        s("cherry-pick", Kind::Subcommand, None, "Apply the changes introduced by some existing commits", "ch"),
        s("main", Kind::Arg, Some(Icon::GitBranch), "fix login redirect (2 hours ago)", ""),
        s("--force-with-lease", Kind::Option, None, "Force, but only if the remote is as expected", ""),
        s("src/", Kind::Folder, None, "", ""),
        s("Cargo.toml", Kind::File, None, "modified", ""),
        s("react", Kind::Arg, Some(Icon::Npm), "React is a JavaScript library for building user interfaces", ""),
        s("nginx", Kind::Arg, Some(Icon::Docker), "official · Official build of Nginx", ""),
        s("numpy", Kind::Arg, Some(Icon::Python), "801.6M downloads/month", ""),
    ];
    let prompt = "% git ch";
    let Some(l) = layout(ui, &items, (40, cols), (0, prompt.width() as u16), 2) else { return String::new() };
    let mut out = format!("{prompt}\x1b[7m \x1b[0m\n");
    for line in render(ui, &l, &items, 1, 0) {
        out.push_str(&format!("{}{line}\n", " ".repeat(l.rect.col as usize)));
    }
    out
}

pub fn draw(out: &mut Vec<u8>, ui: &Ui, l: &Layout, items: &[Suggestion], selected: usize, scroll: usize) {
    let r = l.rect;
    for (i, line) in render(ui, l, items, selected, scroll).iter().enumerate() {
        let _ = write!(out, "\x1b[{};{}H{line}", r.row as usize + i + 1, r.col + 1);
    }
}

/// Repaint the screen rows covered by `rect` from the vt100 model.
pub fn erase(out: &mut Vec<u8>, screen: &vt100::Screen, rect: Rect) {
    let (_, cols) = screen.size();
    let range = rect.row..rect.row + rect.height;
    for (i, row) in screen.rows_formatted(0, cols).enumerate() {
        let i = i as u16;
        if range.contains(&i) {
            let _ = write!(out, "\x1b[{};1H\x1b[0m\x1b[2K", i + 1);
            out.extend_from_slice(&row);
        } else if i >= range.end {
            break;
        }
    }
}

/// Put the cursor and SGR state back where the shell left them.
pub fn restore_cursor(out: &mut Vec<u8>, screen: &vt100::Screen) {
    let (r, c) = screen.cursor_position();
    let _ = write!(out, "\x1b[0m\x1b[{};{}H", r + 1, c + 1);
    out.extend_from_slice(&screen.attributes_formatted());
    if !screen.hide_cursor() {
        out.extend_from_slice(b"\x1b[?25h");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<Suggestion> {
        (0..n)
            .map(|i| Suggestion {
                label: format!("item{i}"),
                description: format!("description {i}"),
                kind: Kind::Arg,
                icon: None,
                insert: String::new(),
                delete: 0,
                back: 0,
                query: String::new(),
            })
            .collect()
    }

    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut esc = false;
        for c in s.chars() {
            match (esc, c) {
                (false, '\x1b') => esc = true,
                (true, c) if c.is_ascii_alphabetic() => esc = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    #[test]
    fn placement() {
        let ui = Ui::default();
        let at = |crow| layout(&ui, &items(20), (24, 80), (crow, 4), 0).unwrap().rect;
        assert_eq!((at(2).row, at(2).height), (3, 10));
        assert_eq!((at(16).row, at(16).height), (17, 7));
        assert_eq!((at(23).row, at(23).height), (13, 10));
        let ui = Ui { border: "none".into(), ..Ui::default() };
        let r = layout(&ui, &items(20), (24, 80), (2, 4), 0).unwrap().rect;
        assert_eq!(r.height, 8);
    }

    #[test]
    fn above_a_wrapped_command() {
        let ui = Ui::default();
        let r = layout_at(&ui, &items(20), (24, 80), (23, 4), 21, 0).unwrap().rect;
        assert_eq!(r.row + r.height, 21);
    }

    #[test]
    fn rounded_border_and_counter() {
        let ui = Ui { icons: "none".into(), ..Ui::default() };
        let it = items(20);
        let l = layout(&ui, &it, (24, 80), (2, 10), 0).unwrap();
        let lines: Vec<String> = render(&ui, &l, &it, 2, 0).iter().map(|s| strip(s)).collect();
        assert!(lines[0].starts_with('╭') && lines[0].ends_with('╮'));
        assert!(lines[1].starts_with("│  item0") && lines[1].contains("description 0"));
        let rui = Ui { icons: "none".into(), density: "relaxed".into(), ..Ui::default() };
        let rl = layout(&rui, &it, (24, 80), (2, 10), 0).unwrap();
        let relaxed: Vec<String> = render(&rui, &rl, &it, 0, 0).iter().map(|s| strip(s)).collect();
        assert_eq!(relaxed[1].trim_matches(['│', ' ']), "");
        assert!(relaxed[2].starts_with("│   item0      "));
        assert_eq!(rl.visible(), 8);
        assert!(lines.last().unwrap().starts_with('╰') && lines.last().unwrap().contains(" 3/20 "));
        for line in &lines {
            assert_eq!(line.width(), l.rect.width as usize, "{line:?}");
        }
        for (border, density) in [
            ("square", "compact"),
            ("rounded", "relaxed"),
            ("none", "relaxed"),
            ("none", "compact"),
            ("none", "normal"),
        ] {
            let ui = Ui { border: border.into(), density: density.into(), ..Ui::default() };
            let l = layout(&ui, &it, (24, 80), (2, 10), 0).unwrap();
            for line in render(&ui, &l, &it, 0, 0) {
                assert_eq!(strip(&line).width(), l.rect.width as usize, "{border}/{density}: {line:?}");
            }
        }
    }

    #[test]
    fn width_and_height_settings() {
        use crate::config::Size;
        let it = items(20);
        let base = Ui { icons: "none".into(), ..Ui::default() };
        let ui = Ui { width: Size::Cells(70), ..base.clone() };
        let l = layout(&ui, &it, (24, 100), (2, 10), 0).unwrap();
        assert_eq!(l.rect.width, 70);
        assert!(render(&ui, &l, &it, 0, 0).iter().all(|r| strip(r).width() == 70));
        let ui = Ui { width: Size::Text("50%".into()), ..base.clone() };
        assert_eq!(layout(&ui, &it, (24, 120), (2, 10), 0).unwrap().rect.width, 60);
        let ui = Ui { width: Size::Cells(20), ..base.clone() };
        let l = layout(&ui, &it, (24, 100), (2, 10), 0).unwrap();
        assert!(strip(&render(&ui, &l, &it, 0, 0)[1]).contains("item0") && l.desc_w == 0);
        let ui = Ui { min_width: 60, ..base.clone() };
        assert_eq!(layout(&ui, &it, (24, 100), (2, 10), 0).unwrap().rect.width, 60);
        let ui = Ui { max_width: Size::Text("25%".into()), ..base.clone() };
        assert_eq!(layout(&ui, &it, (24, 100), (2, 10), 0).unwrap().rect.width, 25);
        let ui = Ui { height: Size::Cells(4), ..base.clone() };
        assert_eq!(layout(&ui, &it, (40, 100), (2, 10), 0).unwrap().visible(), 4);
        let ui = Ui { height: Size::Text("50%".into()), ..base.clone() };
        assert_eq!(layout(&ui, &it, (40, 100), (2, 10), 0).unwrap().rect.height, 20);
        let ui = Ui { min_height: 6, ..base.clone() };
        let few = items(2);
        let l = layout(&ui, &few, (40, 100), (2, 10), 0).unwrap();
        assert_eq!(l.visible(), 6);
        assert_eq!(render(&ui, &l, &few, 0, 0).len(), 8);
    }

    #[test]
    fn description_frame_at_the_bottom() {
        let mut it = items(5);
        it[1].description = "Apply the changes introduced by some existing commits to the current branch, \
            creating a new commit for each one"
            .into();
        let ui = Ui { icons: "none".into(), description_position: "bottom".into(), ..Ui::default() };
        let l = layout(&ui, &it, (24, 100), (2, 10), 0).unwrap();
        assert_eq!(l.desc_w, 0);
        assert_eq!(l.frame_lines, 2);
        assert_eq!(l.visible(), 5);
        let lines: Vec<String> = render(&ui, &l, &it, 1, 0).iter().map(|s| strip(s)).collect();
        assert_eq!(lines.len(), l.rect.height as usize);
        for line in &lines {
            assert_eq!(line.width(), l.rect.width as usize, "{line:?}");
        }
        assert!(lines[1].contains("item0") && !lines[1].contains("description"));
        assert!(lines[7].starts_with('╭') && lines[8].contains("Apply the changes"));
        assert!(lines.last().unwrap().starts_with('╰'));
        let ui = Ui { border: "none".into(), ..ui };
        let l = layout(&ui, &it, (24, 100), (2, 10), 0).unwrap();
        let lines: Vec<String> = render(&ui, &l, &it, 0, 0).iter().map(|s| strip(s)).collect();
        assert!(lines[5].starts_with('─') && lines[6].contains("description 0"));
    }

    #[test]
    fn wrapping() {
        assert_eq!(wrap("one two three four", 9), ["one two", "three", "four"]);
        assert_eq!(wrap("abcdefghij", 4), ["abcd", "efgh", "ij"]);
        assert_eq!(frame_text("one two three four", 9, 2), ["one two", "three …"]);
    }

    #[test]
    fn every_theme_resolves() {
        for name in crate::theme::NAMES {
            let ui = Ui { theme: name.to_string(), ..Ui::default() };
            let t = crate::theme::resolve(&ui);
            assert!(!t.bg.is_empty() && !t.sel_bg.is_empty());
        }
    }
}
