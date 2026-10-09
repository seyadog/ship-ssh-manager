//! Drawing code. Fills `app.layout` so the mouse handler knows what is where.

use crate::app::*;
use crate::store::{Auth, NodeId, Store};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Layout as RLayout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Clear, Paragraph},
};
use unicode_width::UnicodeWidthStr;

const REMOTE_GREEN: Color = Color::Rgb(187, 154, 247);
const REMOTE_GREEN_BRIGHT: Color = Color::Rgb(208, 184, 255);
// ---------------------------------------------------------------- theme

/// Two themes. "terminal" (the default) keeps your terminal's background and text colour and paints the rest
/// with soft pastels (Catppuccin Mocha); "classic" has fixed colours of its own. The terminal's ANSI palette
/// is not used for the interface: in many themes those colours are dull.
static TERMINAL_THEME: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(true);

pub fn set_theme(terminal: bool) {
    TERMINAL_THEME.store(terminal, std::sync::atomic::Ordering::Relaxed);
}

pub fn terminal_theme() -> bool {
    TERMINAL_THEME.load(std::sync::atomic::Ordering::Relaxed)
}

/// The blue of the bear: the logo does not follow the theme.
const LOGO_BLUE: Color = Color::Rgb(122, 162, 247);

fn themed(pastel: Color, classic: Color) -> Color {
    if terminal_theme() { pastel } else { classic }
}
fn c_accent() -> Color {
    themed(Color::Rgb(160, 150, 255), Color::Rgb(122, 162, 247)) // periwinkle
}
fn c_fg() -> Color {
    themed(Color::Reset, Color::Rgb(192, 202, 245))
}
fn c_muted() -> Color {
    themed(Color::Rgb(108, 112, 134), Color::Rgb(86, 95, 137))
}
fn c_sel() -> Color {
    themed(Color::Rgb(49, 50, 68), Color::Rgb(40, 52, 87))
}
fn c_drop() -> Color {
    themed(Color::Rgb(49, 72, 60), Color::Rgb(58, 82, 60))
}
fn c_green() -> Color {
    themed(Color::Rgb(130, 230, 130), Color::Rgb(158, 206, 106))
}
fn c_amber() -> Color {
    themed(Color::Rgb(255, 215, 100), Color::Rgb(224, 175, 104))
}
/// Folders: a light, fresh green (the live green is stronger, so the two stay apart).
fn c_folder() -> Color {
    themed(Color::Rgb(178, 240, 170), Color::Rgb(169, 214, 138))
}
fn c_red() -> Color {
    themed(Color::Rgb(255, 110, 145), Color::Rgb(247, 118, 142))
}
/// Text on the accent colour (a button that stands out).
fn badge_accent() -> Style {
    Style::new().fg(Color::Black).bg(c_accent())
}

/// One colour per project, taken from oso's own palette and as far apart from each other as possible.
const PASTELS: [(u8, u8, u8); 8] = [
    (122, 162, 247), // blue
    (247, 118, 142), // red
    (158, 206, 106), // green
    (224, 175, 104), // amber
    (187, 154, 247), // lilac
    (125, 207, 255), // cyan
    (255, 150, 100), // orange
    (115, 218, 180), // teal
];

fn space_color(id: u64, live: bool) -> Color {
    let (r, g, b) = if terminal_theme() { MOCHA[(id as usize) % MOCHA.len()] } else { PASTELS[(id as usize) % PASTELS.len()] };
    // Idle spaces keep their hue but wash out towards grey (fading to black would turn yellows brown).
    let k = if live { 1.0 } else { 0.6 };
    let fade = |c: u8, grey: f32| (c as f32 * k + grey * (1.0 - k)) as u8;
    Color::Rgb(fade(r, 80.0), fade(g, 82.0), fade(b, 100.0))
}

/// The colours of the default theme, soft but lively: blue, pink, green, orange, violet, cyan, yellow, teal.
const MOCHA: [(u8, u8, u8); 8] = [
    (110, 168, 255),
    (255, 110, 145),
    (130, 230, 130),
    (255, 160, 90),
    (190, 130, 255),
    (90, 215, 245),
    (255, 215, 100),
    (80, 225, 185),
];


/// Truncates `s` to `w` columns, adding “…” if it does not fit.
fn fit(s: &str, w: usize) -> String {
    if s.width() <= w {
        return s.to_string();
    }
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        if used + cw + 1 > w {
            break;
        }
        out.push(c);
        used += cw;
    }
    out.push('…');
    out
}

fn centered(w: u16, h: u16, area: Rect) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect::new(area.x + (area.width - w) / 2, area.y + (area.height - h) / 2, w, h)
}

pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    app.layout.tabs.clear();
    app.layout.toolbar.clear();
    app.layout.modal.clear();

    let [main, status] = RLayout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    // One column: Projects on top, Servers below.
    let side_w: u16 = (if area.width >= 110 { 34 } else if area.width >= 80 { 28 } else { 22 }).min(area.width / 2);
    let [side, right] = RLayout::horizontal([Constraint::Length(side_w), Constraint::Min(1)]).areas(main);
    let [tabbar, content] = RLayout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(right);
    // One blank column between the sidebar (or the line between two groups) and the terminal.
    let content = Rect::new(content.x + 1, content.y, content.width.saturating_sub(1), content.height);

    app.layout.sidebar = side;
    app.layout.tabbar = tabbar;
    app.layout.split_area = right;
    app.layout.divider = Rect::default();
    app.layout.panes = [content, Rect::default()];
    app.layout.content = content;

    draw_sidebar(f, app, side);
    if app.is_split() {
        // Two groups of tabs, each with its bar and its terminal, with a line between them to drag.
        let left_w = ((right.width as u32 * app.split_pm as u32 / 1000) as u16).clamp(10, right.width.saturating_sub(11));
        let lrect = Rect::new(right.x, right.y, left_w, right.height);
        let drect = Rect::new(right.x + left_w, right.y, 1, right.height);
        let rrect = Rect::new(drect.x + 1, right.y, right.width.saturating_sub(left_w + 1), right.height);
        let [lbar, lcont] = RLayout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(lrect);
        let [rbar, rcont] = RLayout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(rrect);
        let pad = |r: Rect| Rect::new(r.x + 1, r.y, r.width.saturating_sub(1), r.height);
        let (lcont, rcont) = (pad(lcont), pad(rcont));
        app.layout.divider = drect;
        app.layout.panes = [lcont, rcont];
        let focus_right = app.tabs.get(app.active).is_some_and(|t| t.right);
        app.layout.content = if focus_right { rcont } else { lcont };
        draw_tabs(f, app, lbar, false);
        draw_tabs(f, app, rbar, true);
        let line = Style::new().fg(c_muted());
        for y in drect.y..drect.y + drect.height {
            f.render_widget(Paragraph::new("│").style(line), Rect::new(drect.x, y, 1, 1));
        }
        for right_side in [false, true] {
            let Some(idx) = app.pane_tab(right_side) else { continue };
            let area = if right_side { rcont } else { lcont };
            draw_terminal(f, app, idx, area, right_side == focus_right);
        }
    } else {
        draw_tabs(f, app, tabbar, false);
        draw_content(f, app, content);
    }
    draw_status(f, app, status);
    draw_modal(f, app, area);
}

// ---------------------------------------------------------------- sidebar

/// The sidebar: two columns side by side, Projects and Servers. The one that is the active view takes the
/// keyboard and the selection; the other shows its list and a click makes it the active one. Each can be folded.
fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar && app.modal.is_none();
    app.layout.sidebar = area;
    app.layout.cols = [Rect::default(); 2];
    app.layout.col_geom = [ListGeom::default(); 2];
    app.layout.list = Rect::default();
    app.layout.list_bottom = Rect::default();
    // Projects in the top half, Servers in the bottom half.
    let top_h = area.height / 2;
    let rects = [
        Rect::new(area.x, area.y, area.width, top_h),
        Rect::new(area.x, area.y + top_h, area.width, area.height - top_h),
    ];
    // The titles come first in the toolbar: the keyboard highlight finds them by position (`HEADER`).
    let names = ["Projects", "Servers"];
    for i in 0..2 {
        let title = if app.fold[i] {
            Rect::default()
        } else {
            Rect::new(rects[i].x, rects[i].y, (names[i].width() as u16 + 2).min(rects[i].width.saturating_sub(1)), 1)
        };
        app.layout.toolbar.push((title, HEADER[i]));
    }
    for i in 0..2 {
        draw_column(f, app, i, rects[i], focused);
    }
    // Keyboard focus on a title: highlight it.
    if let (Some(i), true) = (app.header, focused) {
        if let Some(&(r, _)) = app.layout.toolbar.get(i) {
            f.buffer_mut().set_style(r, Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD));
        }
    }
}

fn draw_column(f: &mut Frame, app: &mut App, i: usize, area: Rect, focused: bool) {
    let (view, name) = if i == 0 { (View::Spaces, "Projects") } else { (View::Folders, "Servers") };
    let active = app.view == view;
    if app.fold[i] {
        // A thin strip: click it to bring the column back.
        let initial = name.chars().next().unwrap_or(' ').to_string();
        f.render_widget(
            Paragraph::new(vec![Line::raw(""), Line::from(Span::styled(" ›", Style::new().fg(c_accent()))), Line::from(Span::styled(format!(" {initial}"), Style::new().fg(c_muted())))]),
            Rect::new(area.x, area.y, area.width, 3.min(area.height)),
        );
        app.layout.toolbar.push((Rect::new(area.x, area.y, area.width, 3.min(area.height)), Hit::Fold(i)));
        return;
    }
    app.layout.cols[i] = area;
    if area.height < 5 || area.width < 8 {
        return;
    }
    // No frames: the title, the list, a row of actions at the bottom, and a faint line on the right edge.
    let edge = Style::new().fg(themed(Color::Rgb(49, 50, 68), Color::Rgb(44, 50, 74)));
    for y in area.y..area.y + area.height {
        f.render_widget(Paragraph::new("│").style(edge), Rect::new(area.x + area.width - 1, y, 1, 1));
    }
    let title_style = if active && focused {
        Style::new().fg(c_accent()).add_modifier(Modifier::BOLD)
    } else if active {
        Style::new().fg(c_fg()).add_modifier(Modifier::BOLD)
    } else {
        Style::new().fg(c_muted())
    };
    f.render_widget(Paragraph::new(format!("  {name}")).style(title_style), Rect::new(area.x, area.y, area.width - 1, 1));
    // A dot per thing that is open in this column (a project with a live tab, a server with a live session),
    // each in its own colour, at the right end of the title.
    let mut open: Vec<u64> = vec![];
    for t in app.tabs.iter().filter(|t| t.session.exit_code.is_none()) {
        let id = match (i, t.scope) {
            (0, Scope::Space(id)) => id,
            (1, Scope::Ssh) => t.server_id,
            _ => continue,
        };
        if !open.contains(&id) {
            open.push(id);
        }
    }
    let room = (area.width as usize).saturating_sub(name.width() + 7) / 2;
    let dots: Vec<Span> = open.iter().take(room).flat_map(|&id| [Span::styled("●", Style::new().fg(space_color(id, true))), Span::raw(" ")]).collect();
    let dw = (dots.len() as u16).min(area.width.saturating_sub(4));
    if dw > 0 {
        f.render_widget(Paragraph::new(Line::from(dots)), Rect::new(area.x + area.width - 2 - dw, area.y, dw, 1));
    }
    // The list keeps two columns of margin on the left and a little on the right.
    let inner = Rect::new(area.x + 2, area.y + 2, area.width.saturating_sub(5), area.height.saturating_sub(3));
    // The actions of the column, as small padded buttons along the bottom.
    let by = area.y + area.height - 1;
    let actions: [(&str, Hit); 3] = if i == 0 {
        [(" + new ", Hit::NewProject), (" edit ", Hit::Edit), (" term ", Hit::Term)]
    } else {
        [(" + new ", Hit::NewServer), (" edit ", Hit::Edit), (" vault ", Hit::Vault)]
    };
    let mut bx = area.x + 2;
    for (n, (label, hit)) in actions.into_iter().enumerate() {
        let w = label.width() as u16;
        if bx + w > area.x + area.width - 2 {
            break;
        }
        let r = Rect::new(bx, by, w, 1);
        f.render_widget(Paragraph::new(label).style(Style::new().fg(if n == 0 { c_accent() } else { c_fg() }).bg(c_sel())), r);
        app.layout.toolbar.push((r, hit));
        bx += w + 1;
    }
    let list = Rect::new(inner.x, inner.y, inner.width, inner.height.saturating_sub(1));
    if active {
        draw_active_list(f, app, list, focused);
    } else {
        draw_passive_list(f, app, i, view, list);
    }
}

/// Where the parts of a list go in its column: projects as two-line blocks (tighter when there are many),
/// with the folders below (up to half the list); the servers' bastions below, from the middle.
fn list_geom(rows: &[Row], view: View, list: Rect) -> ListGeom {
    let _ = view;
    let header = rows.iter().position(|r| r.node == NodeId::BastionsHeader);
    let split = if list.height >= 6 { header } else { None };
    let (top, bottom) = match split {
        // The part below (folders, bastions) starts right after the top one, with a blank line between, and
        // takes at most half of the column.
        Some(h) => {
            let bottom_lines = ((rows.len() - h) as u16).min(list.height / 2);
            let top_h = (h as u16 + 1).min(list.height - bottom_lines);
            (Rect::new(list.x, list.y, list.width, top_h), Rect::new(list.x, list.y + top_h, list.width, list.height - top_h))
        }
        None => (list, Rect::default()),
    };
    ListGeom { top, bottom, split, row_h: 1, bottom_row_h: 1, top_count: split.unwrap_or(rows.len()) }
}

/// The list of the column that is not the active view. It is laid out exactly like the active one, from
/// the top and without a selection, so that switching column does not rearrange anything; a click activates it.
fn draw_passive_list(f: &mut Frame, app: &mut App, col: usize, view: View, list: Rect) {
    let g = list_geom(&app.other_rows, view, list);
    app.layout.col_geom[col] = g;
    if app.other_rows.is_empty() {
        let text = if view == View::Spaces { "No projects yet." } else { "No servers yet." };
        f.render_widget(Paragraph::new(text).style(Style::new().fg(c_muted())), list);
        return;
    }
    let app: &App = app;
    let rows = &app.other_rows;
    draw_rows(f, app, g.top, 0, g.top_count, 0, g.row_h, false, rows, None, view);
    if let Some(header) = g.split {
        draw_rows(f, app, g.bottom, header, rows.len(), 0, g.bottom_row_h, false, rows, None, view);
    }
}

/// The list of the active view, with its selection and scrolling.
fn draw_active_list(f: &mut Frame, app: &mut App, list: Rect, focused: bool) {
    app.layout.list = list;
    if list.height == 0 {
        return;
    }
    if app.rows.is_empty() {
        let lines: Vec<&str> = if app.view == View::Spaces {
            vec!["No projects yet.", "Press a: a terminal opens;", "run claude, opencode… in it.", "It stays where you leave it (cd)."]
        } else {
            vec!["No servers yet.", "Click “+ Server” or press a."]
        };
        let hint = Paragraph::new(lines.into_iter().map(|l| Line::from(Span::styled(l, Style::new().fg(c_muted())))).collect::<Vec<_>>());
        f.render_widget(hint, list);
        return;
    }
    let g = list_geom(&app.rows, app.view, list);
    let (top, bottom, split, row_h) = (g.top, g.bottom, g.split, g.row_h);
    app.layout.row_h = row_h;
    app.layout.list = top;
    app.layout.list_bottom = bottom;

    // Keep the selected row in view in whichever half it is.
    let top_count = g.top_count;
    let h_top = ((top.height / row_h) as usize).max(1);
    if app.selected < top_count {
        if app.selected < app.offset {
            app.offset = app.selected;
        } else if app.selected >= app.offset + h_top {
            app.offset = app.selected + 1 - h_top;
        }
    }
    app.offset = app.offset.min(top_count.saturating_sub(h_top));
    if let Some(header) = split {
        let h_bottom = ((bottom.height / g.bottom_row_h) as usize).max(1);
        if app.selected >= header {
            let rel = app.selected - header;
            if rel < app.offset_bottom {
                app.offset_bottom = rel;
            } else if rel >= app.offset_bottom + h_bottom {
                app.offset_bottom = rel + 1 - h_bottom;
            }
        }
        app.offset_bottom = app.offset_bottom.min((app.rows.len() - header).saturating_sub(h_bottom));
    }

    let (offset, offset_bottom, rows_len) = (app.offset, app.offset_bottom, app.rows.len());
    let view = app.view;
    let selected = Some(app.selected);
    let rows = std::mem::take(&mut app.rows);
    draw_rows(f, app, top, 0, top_count, offset, row_h, focused, &rows, selected, view);
    if let Some(header) = split {
        draw_rows(f, app, bottom, header, rows_len, offset_bottom, g.bottom_row_h, focused, &rows, selected, view);
    }
    app.rows = rows;
}

/// Draws rows `from..to` of the list in `area`, the first one shown being `from + offset`.
#[allow(clippy::too_many_arguments)]
fn draw_rows(
    f: &mut Frame,
    app: &App,
    area: Rect,
    from: usize,
    to: usize,
    offset: usize,
    row_h: u16,
    focused: bool,
    rows: &[Row],
    selected: Option<usize>,
    view: View,
) {
    let width = area.width as usize;
    let h = ((area.height / row_h) as usize).max(1);
    let start = from + offset;
    for n in start..to.min(start + h) {
        let row = &rows[n];
        let y = area.y + (n - start) as u16 * row_h;
        let is_sel = selected == Some(n);
        let is_drop = app.drop_hover == Some(n);
        let mut style = Style::new();
        let mut bg = Color::Reset;
        if is_drop {
            bg = c_drop();
        } else if is_sel {
            bg = c_sel();
            if focused {
                style = style.add_modifier(Modifier::BOLD);
            }
        }
        style = style.bg(bg);

        if let NodeId::Space(id) = row.node {
            // A solid two-line block: name, and under it the git branch (or the directory).
            let sp = app.spaces.get(id);
            let name = sp.map(|s| s.name.clone()).unwrap_or_else(|| "?".into());
            let cwd = sp.map(|s| app.tilde(&s.cwd)).unwrap_or_default();
            let mine = |t: &&Tab| t.scope == Scope::Space(id);
            let live = app.tabs.iter().filter(mine).any(|t| t.session.exit_code.is_none());
            let attention = app.tabs.iter().filter(mine).any(|t| t.attention);
            // One line: a coloured spine for the state, the name, and the branch (or directory) on the right.
            // The name turns green when the agent has finished; the spine keeps the space's own colour.
            let color = space_color(id, live);
            let (mut text, tone) = match app.branches.get(&id) {
                Some(b) => (b.clone(), themed(Color::Rgb(190, 140, 255), REMOTE_GREEN)),
                None => (cwd, c_muted()),
            };
            // A filed project that is on top says which folder it belongs to.
            if !row.jump {
                if let Some(fo) = sp.and_then(|s| s.folder).and_then(|f| app.spaces.folder(f)) {
                    text = format!("▸ {}  {}", fo.name, text);
                }
            }
            // The AI agents running in this project (one per tab), each named and coloured by what it is doing.
            let mut agents: Vec<(bool, bool, String)> = app
                .tabs
                .iter()
                .filter(mine)
                .filter_map(|t| t.session.agent().map(|a| (t.attention, a.working, a.name.clone())))
                .collect();
            agents.sort_by_key(|&(att, working, _)| std::cmp::Reverse((att, working)));
            let name_w = name.width().min(width.saturating_sub(3));
            let tags: Vec<(String, Color)> = agents
                .into_iter()
                .map(|(att, working, aname)| {
                    if att {
                        (format!("{aname} ✓"), c_green())
                    } else if working {
                        (format!("{aname} …"), c_amber())
                    } else {
                        (aname, c_muted())
                    }
                })
                .collect();
            // The tags that fit in `room` columns, separated by a space; the later ones are dropped first.
            let fit_tags = |room: usize| -> Vec<(String, Color)> {
                let (mut used, mut out) = (0, vec![]);
                for (t, c) in &tags {
                    let w = t.width() + usize::from(!out.is_empty());
                    if used + w > room {
                        break;
                    }
                    used += w;
                    out.push((t.clone(), *c));
                }
                out
            };
            let tags_width = |v: &[(String, Color)]| v.iter().map(|(t, _)| t.width()).sum::<usize>() + v.len().saturating_sub(1);
            let tag_spans = |v: Vec<(String, Color)>| -> Vec<Span<'static>> {
                let mut out = vec![];
                for (i, (t, c)) in v.into_iter().enumerate() {
                    if i > 0 {
                        out.push(Span::styled(" ", Style::new().bg(bg)));
                    }
                    out.push(Span::styled(t, Style::new().fg(c).bg(bg).add_modifier(Modifier::BOLD)));
                }
                out
            };
            let width = width.saturating_sub(1);
            let mut spans = vec![
                Span::styled(" ", Style::new().bg(bg)),
                Span::styled("▆", Style::new().fg(color).bg(bg)),
                Span::styled(
                    format!(" {}", fit(&name, width.saturating_sub(3))),
                    style.fg(if attention { c_green() } else { color }).add_modifier(Modifier::BOLD),
                ),
            ];
            // The rest of the line, right-aligned: the agent (if any) first, then the branch if it still fits.
            let mut room = width.saturating_sub(2 + name_w + 3);
            let mut right: Vec<Span<'static>> = vec![];
            let shown = fit_tags(room);
            if !shown.is_empty() {
                room -= tags_width(&shown) + 2;
                right.extend(tag_spans(shown));
            }
            if room >= 6 && !text.is_empty() {
                let shown = fit(&text, room);
                right.insert(0, Span::styled(format!("{}  ", shown), Style::new().fg(tone).bg(bg)));
            }
            let used: usize = right.iter().map(|sp| sp.content.width()).sum();
            if used > 0 {
                let pad = width.saturating_sub(2 + name_w + used + 1);
                spans.push(Span::styled(" ".repeat(pad), Style::new().bg(bg)));
                spans.extend(right);
                spans.push(Span::styled(" ", Style::new().bg(bg)));
            }
            f.render_widget(Paragraph::new(Line::from(spans)).style(style), Rect::new(area.x, y, area.width, 1));
            continue;
        }

        let indent = "  ".repeat(row.depth);
        let mut spans = vec![Span::raw(indent.clone())];
        match row.node {
            NodeId::Space(_) => {}
            NodeId::SpaceFolder(id) => {
                // One small line: the folder, how many projects it holds, and whether an agent runs inside.
                let fo = app.spaces.folder(id);
                let open = fo.is_some_and(|f| f.expanded);
                let name = fo.map(|f| f.name.as_str()).unwrap_or("?");
                let inside: Vec<u64> = app.spaces.spaces.iter().filter(|s| s.folder == Some(id)).map(|s| s.id).collect();
                let agents = app.tabs.iter().filter(|t| matches!(t.scope, Scope::Space(s) if inside.contains(&s)));
                let (mut att, mut work) = (false, false);
                for t in agents {
                    att |= t.attention;
                    work |= t.session.agent().is_some_and(|a| a.working);
                }
                let (mark, mc) = if att {
                    (" ✓", c_green())
                } else if work {
                    (" …", c_amber())
                } else {
                    ("", c_muted())
                };
                let count = format!(" ({}){}", inside.len(), mark);
                let label = fit(name, width.saturating_sub(indent.width() + 4 + count.width()));
                spans.push(Span::styled(if open { " ▾  " } else { " ▸  " }, Style::new().fg(c_folder())));
                spans.push(Span::styled(label, Style::new().fg(c_folder()).add_modifier(Modifier::BOLD)));
                spans.push(Span::styled(format!(" ({})", inside.len()), Style::new().fg(c_muted())));
                if !mark.is_empty() {
                    spans.push(Span::styled(mark, Style::new().fg(mc).add_modifier(Modifier::BOLD)));
                }
            }
            NodeId::BastionsHeader => {
                // A fixed label with a rule after it: only the bastions below it fold.
                let title = if view == View::Spaces { "folders" } else { "bastions" };
                spans.push(Span::styled(title, Style::new().fg(c_muted()).add_modifier(Modifier::BOLD)));
                let rest = width.saturating_sub(indent.width() + title.len() + 1);
                spans.push(Span::styled(format!(" {}", "─".repeat(rest)), Style::new().fg(c_muted())));
            }
            NodeId::Folder(id) => {
                let fo = app.store.folder(id);
                let open = fo.is_some_and(|f| f.expanded);
                let name = fo.map(|f| f.name.as_str()).unwrap_or("?");
                let label = fit(name, width.saturating_sub(indent.width() + 5));
                spans.push(Span::styled(if open { " ▾  " } else { " ▸  " }, Style::new().fg(c_folder())));
                spans.push(Span::styled(label, Style::new().fg(c_folder()).add_modifier(Modifier::BOLD)));
            }
            NodeId::Server(id) => {
                let s = app.store.server(id);
                let name = s.map(|s| s.name.as_str()).unwrap_or("?");
                let live = app.tabs.iter().any(|t| t.server_id == id && t.session.exit_code.is_none());
                let label = fit(name, width.saturating_sub(indent.width() + 5));
                if app.has_children(row) {
                    let arrow = if app.is_open(row) { " ▾  " } else { " ▸  " };
                    spans.push(Span::styled(arrow, Style::new().fg(c_accent())));
                } else {
                    spans.push(Span::styled(
                        if live { " ◆  " } else { " ◇  " },
                        Style::new().fg(if live { c_green() } else { c_muted() }),
                    ));
                }
                spans.push(Span::styled(label, Style::new().fg(if live { c_green() } else { c_fg() })));
                if let Some(s) = s {
                    let room = width.saturating_sub(indent.width() + 4 + name.width() + 2);
                    let via = match (row.jump, s.jump.and_then(|j| app.store.server(j))) {
                        (false, Some(j)) => format!("{} ↪ {}", s.host, j.name),
                        _ => s.host.clone(),
                    };
                    if room > 6 {
                        spans.push(Span::styled(format!("  {}", fit(&via, room)), Style::new().fg(c_muted())));
                    }
                }
            }
        }
        let line = Line::from(spans).style(style);
        f.render_widget(Paragraph::new(line).style(style), Rect::new(area.x, y, area.width, 1));
    }
}

// ---------------------------------------------------------------- tabs

/// What runs in a tab, shown in the bar so that it can be seen from the other group: the agent and its state
/// (`…` working, `✓` finished and waiting for a look).
fn tab_tag(tab: &crate::app::Tab) -> Option<(String, Color)> {
    let a = tab.session.agent()?;
    Some(if tab.attention {
        (format!("{} ✓", a.name), c_green())
    } else if a.working {
        (format!("{} …", a.name), c_amber())
    } else {
        (a.name, c_muted())
    })
}

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect, right: bool) {
    let end = area.x + area.width;
    // Every open tab of this group; the numbers (Alt+1..9, 0) run through the left group and on into the right one.
    let scoped: Vec<usize> = app.bar_side(right);
    let shown = app.pane_tab(right);
    let first_number = if right { app.bar_side(false).len() } else { 0 };
    let n = scoped.len();
    let mut x = area.x + 1;
    let mut hits = vec![];
    if n > 0 {
        // Shrink titles so that as many tabs as possible fit, then scroll the bar so the active tab is always visible.
        let avail = area.width.saturating_sub(4) as usize;
        let max_title = (avail / n).saturating_sub(9).clamp(6, 20);
        let cell = |p: usize| {
            let tab = &app.tabs[scoped[p]];
            fit(&app.tab_label(tab), max_title).width() + 11 + 1 + tab_tag(tab).map_or(0, |(t, _)| t.width() + 1)
        };
        let active = scoped.iter().position(|&i| Some(i) == shown).unwrap_or(0);
        let mut start = 0;
        while start < active && (start..=active).map(cell).sum::<usize>() > avail {
            start += 1;
        }
        for pos in start..n {
            let i = scoped[pos];
            let tab = &app.tabs[i];
            let dead = tab.session.exit_code.is_some();
            let title = fit(&app.tab_label(tab), max_title);
            let num = match first_number + pos {
                n @ 0..=8 => format!("{} ", n + 1),
                9 => "0 ".to_string(),
                _ => "  ".to_string(),
            };
            let tag = tab_tag(tab);
            let tag_text = tag.as_ref().map(|(t, _)| format!(" {t}")).unwrap_or_default();
            let text = format!("  {num}{} {}{tag_text}  ✕  ", if dead { "○" } else { "●" }, title);
            let w = text.width() as u16;
            if x + w > end {
                break;
            }
            let is_active = Some(i) == shown;
            let dot = if dead {
                c_red()
            } else if tab.attention {
                c_amber()
            } else {
                c_green()
            };
            let bg = if is_active { c_sel() } else { Color::Reset };
            let fg = if is_active { c_fg() } else { c_muted() };
            let rect = Rect::new(x, area.y, w, 1);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  ", Style::new().bg(bg)),
                    Span::styled(num, Style::new().fg(c_muted()).bg(bg)),
                    Span::styled(if dead { "◇ " } else { "◆ " }, Style::new().fg(dot).bg(bg)),
                    Span::styled(
                        title,
                        Style::new().fg(fg).bg(bg).add_modifier(if is_active { Modifier::BOLD } else { Modifier::empty() }),
                    ),
                    Span::styled(tag_text, Style::new().fg(tag.map_or(c_muted(), |(_, c)| c)).bg(bg)),
                    Span::styled("  ✕  ", Style::new().fg(c_muted()).bg(bg)),
                ])),
                rect,
            );
            hits.push(TabHit { idx: i, rect, close: Rect::new(x + w - 5, area.y, 5, 1) });
            x += w + 1;
        }
    }
    // “+” opens a new terminal in this scope.
    if app.scope != Scope::Space(0) && x + 5 <= end {
        let r = Rect::new(x, area.y, 5, 1);
        f.render_widget(Paragraph::new("  +  ").style(Style::new().fg(c_accent())), r);
        app.layout.toolbar.push((r, if right { Hit::NewTabRight } else { Hit::NewTab }));
    }
    if right {
        app.layout.tabs.extend(hits);
    } else {
        app.layout.tabs = hits;
    }
}

// ---------------------------------------------------------------- contenido

fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    if app.blank() {
        return draw_welcome(f, app, area);
    }
    let idx = app.active.min(app.tabs.len() - 1);
    draw_terminal(f, app, idx, area, true);
}

/// The terminal of tab `idx` in `area`. Only the group that has the focus shows the cursor and the selection.
fn draw_terminal(f: &mut Frame, app: &mut App, idx: usize, area: Rect, in_focus: bool) {
    let focused = in_focus && app.focus == Focus::Terminal && app.modal.is_none();
    let tab = &mut app.tabs[idx];
    tab.session.resize(area.height, area.width);

    let mut cursor = None;
    let mut blank = false;
    let mut scrolled = 0usize;
    let buf = f.buffer_mut();
    tab.session.with_screen(|screen| {
        blank = screen.contents().trim().is_empty();
        scrolled = screen.scrollback();
        for row in 0..area.height {
            for col in 0..area.width {
                let Some(cell) = screen.cell(row, col) else { continue };
                if cell.is_wide_continuation() {
                    continue;
                }
                let out = &mut buf[(area.x + col, area.y + row)];
                let s = cell.contents().to_string();
                out.set_symbol(if s.is_empty() { " " } else { &s });
                let mut style = Style::new().fg(map_color(cell.fgcolor())).bg(map_color(cell.bgcolor()));
                if cell.bold() {
                    style = style.add_modifier(Modifier::BOLD);
                }
                if cell.italic() {
                    style = style.add_modifier(Modifier::ITALIC);
                }
                if cell.underline() {
                    style = style.add_modifier(Modifier::UNDERLINED);
                }
                if cell.inverse() {
                    style = style.add_modifier(Modifier::REVERSED);
                }
                out.set_style(style);
            }
        }
        if !screen.hide_cursor() && screen.scrollback() == 0 {
            let (r, c) = screen.cursor_position();
            cursor = Some((area.x + c, area.y + r));
        }
    });
    // Selected text, highlighted.
    if let Some(sel) = app.selection.filter(|_| in_focus) {
        let ((r1, c1), (r2, c2)) = sel.ordered();
        let last_row = area.height.saturating_sub(1);
        let last_col = area.width.saturating_sub(1);
        for row in r1.min(last_row)..=r2.min(last_row) {
            let from = if row == r1 { c1 } else { 0 };
            let to = if row == r2 { c2 } else { last_col };
            for col in from.min(last_col)..=to.min(last_col) {
                let cell = &mut f.buffer_mut()[(area.x + col, area.y + row)];
                cell.set_style(cell.style().add_modifier(Modifier::REVERSED));
            }
        }
    }
    // ssh prints nothing while it waits for an unreachable host: say what is going on.
    let waited = tab.session.started.elapsed().as_secs();
    if blank && tab.session.exit_code.is_none() && waited >= 2 {
        let target = app.store.server(tab.server_id).map(|s| format!("{}:{}", s.host, s.port)).unwrap_or_default();
        let msg = format!("Connecting to {target}… {waited}s (gives up after 15s; Alt+W closes this tab)");
        let w = (msg.width() as u16).min(area.width);
        f.render_widget(Paragraph::new(msg).style(Style::new().fg(c_muted())), Rect::new(area.x, area.y, w, 1));
    }
    if scrolled > 0 {
        let msg = format!(" ↑ {scrolled} lines up · Shift+End or type: back to live ");
        let w = (msg.width() as u16).min(area.width);
        f.render_widget(
            Paragraph::new(msg).style(Style::new().fg(Color::Black).bg(c_amber())),
            Rect::new(area.x + area.width - w, area.y, w, 1),
        );
    }
    if focused {
        if let (Some((x, y)), None) = (cursor, tab.session.exit_code) {
            f.set_cursor_position(Position::new(x, y));
        }
    }

    if tab.session.exit_code.is_none()
        && tab.session.sudo_prompt()
        && app.store.server(tab.server_id).is_some_and(|s| s.has_sudo)
    {
        let bar = Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1);
        let text = " sudo is asking for a password · Alt+P: fill it from the vault · or just type ";
        f.render_widget(
            Paragraph::new(fit(text, area.width as usize)).style(badge_accent()),
            bar,
        );
    }

    if let Some(code) = tab.session.exit_code {
        let msg = match code {
            0 => "Session closed.".to_string(),
            255 => "No connection (255): see the ssh error above.".to_string(),
            n if tab.server_id == LOCAL => format!("Shell exited with code {n}."),
            n => format!("ssh exited with code {n}."),
        };
        let bar = Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1);
        let text = format!(" {msg}  Enter: reconnect · Alt+W: close ");
        f.render_widget(
            Paragraph::new(fit(&text, area.width as usize)).style(Style::new().fg(Color::Black).bg(if code == 0 {
                c_amber()
            } else {
                c_red()
            })),
            bar,
        );
    }
}

fn map_color(c: vt100::Color) -> Color {
    match c {
        vt100::Color::Default => Color::Reset,
        // Remote sessions show ANSI green as lilac, so they never look like the local shell.
        vt100::Color::Idx(2) => REMOTE_GREEN,
        vt100::Color::Idx(10) => REMOTE_GREEN_BRIGHT,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

/// The bear, as a grid of pixels: `#` fur, `o` ears, `i` inside of the ears, `E` eyes (`w` their glint),
/// `s` snout, `n` nose. Two pixel rows make one terminal row (half blocks), so it is 22 columns by 8 lines.
const BEAR: [&str; 16] = [
    "    oooo      oooo    ",
    "   oooooo    oooooo   ",
    "   ooiioo####ooiioo   ",
    "  ##################  ",
    "  ##################  ",
    "  ####wE######wE####  ",
    "  ####EE######EE####  ",
    "  ####EE######EE####  ",
    "  ######ssssss######  ",
    "  #####ssssssss#####  ",
    "  #####ssnnnnss#####  ",
    "   ####sssnnsss####   ",
    "   ####ssssssss####   ",
    "    ##############    ",
    "     ############     ",
    "       ########       ",
];

/// The name in the same pixels, with the s bigger than the two o's: o (4 wide), s (6 wide), o.
const WORDMARK: [&str; 8] = [
    "     .####     ",
    "     #....     ",
    "     #....     ",
    "     .###.     ",
    ".##. ....# .##.",
    "#..# ....# #..#",
    "#..# ....# #..#",
    ".##. ####. .##.",
];

/// Two pixel rows make one terminal row (half blocks). `color` says what a pixel's character is painted with
/// (`None`: nothing).
fn pixel_lines(grid: &[&str], color: impl Fn(char) -> Option<Color>) -> Vec<Line<'static>> {
    let width = grid.iter().map(|r| r.chars().count()).max().unwrap_or(0);
    let at = |row: usize, col: usize| grid.get(row).and_then(|r| r.chars().nth(col)).and_then(&color);
    (0..grid.len().div_ceil(2))
        .map(|line| {
            let spans: Vec<Span<'static>> = (0..width)
                .map(|col| match (at(line * 2, col), at(line * 2 + 1, col)) {
                    (None, None) => Span::raw(" "),
                    (Some(t), None) => Span::styled("▀", Style::new().fg(t)),
                    (None, Some(b)) => Span::styled("▄", Style::new().fg(b)),
                    (Some(t), Some(b)) => Span::styled("▀", Style::new().fg(t).bg(b)),
                })
                .collect();
            Line::from(spans)
        })
        .collect()
}

fn bear_lines() -> Vec<Line<'static>> {
    pixel_lines(&BEAR, |c| match c {
        '#' => Some(LOGO_BLUE),
        'o' => Some(Color::Rgb(187, 154, 247)),
        'i' => Some(Color::Rgb(240, 198, 240)),
        'E' | 'n' => Some(Color::Rgb(20, 21, 30)),
        'w' => Some(Color::Rgb(255, 255, 255)),
        's' => Some(Color::Rgb(214, 224, 255)),
        _ => None,
    })
}

fn wordmark_lines() -> Vec<Line<'static>> {
    pixel_lines(&WORDMARK, |c| (c == '#').then_some(LOGO_BLUE))
}

fn draw_welcome(f: &mut Frame, app: &App, area: Rect) {
    let mut lines: Vec<Line> = bear_lines();
    lines.push(Line::raw(""));
    lines.extend(wordmark_lines());
    lines.push(Line::raw(""));
    let hints: Vec<String> = match app.scope {
        Scope::Ssh => vec![
            "Double-click a server to open a session.".into(),
            "Drag servers and folders to reorganize them.".into(),
            "Alt+Q switches between the panel and the terminal.".into(),
        ],
        Scope::Space(0) => vec![
            "No projects yet.".into(),
            "Press a (or “+ Project”): a terminal opens. Move around with cd and mkdir:".into(),
            "it stays in the last directory you leave it in.".into(),
        ],
        Scope::Space(id) => {
            let sp = app.spaces.get(id);
            vec![
                sp.map(|s| s.name.clone()).unwrap_or_default(),
                sp.map(|s| s.cwd.clone()).unwrap_or_default(),
                "Press Enter or t to open a terminal here.".into(),
                "Run claude, opencode… in it: it shows under “running”.".into(),
            ]
        }
    };
    for t in hints {
        lines.push(Line::from(Span::styled(t, Style::new().fg(c_muted()))));
    }
    let h = lines.len() as u16;
    let r = centered(area.width, h, area);
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), r);
}

// ---------------------------------------------------------------- status bar

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let text = if let Some((msg, _)) = &app.flash {
        return f.render_widget(Paragraph::new(format!(" {msg}")).style(Style::new().fg(c_amber())), area);
    } else if app.modal.is_some() {
        "Esc cancel"
    } else if app.focus == Focus::Terminal {
        "F6 next area · Alt+K open · Ctrl+N new tab · Alt+←/→ tabs · Alt+W close"
    } else if app.header.is_some() {
        "←→ choose column · Esc back to the list"
    } else if app.view == View::Spaces {
        "↑↓ move · Enter open · a add · e rename · d delete · Tab servers · F6 next area · / search · q quit"
    } else {
        "↑↓ move · Enter open · a add · e edit · d delete · Tab projects · F6 next area · / search · q quit"
    };
    f.render_widget(Paragraph::new(format!(" {text}")).style(Style::new().fg(c_muted())), area);
}

// ---------------------------------------------------------------- modales

fn modal_block(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(c_accent()))
        .title(Span::styled(format!(" {title} "), Style::new().fg(c_accent()).add_modifier(Modifier::BOLD)))
}

fn button(label: &str, primary: bool) -> Span<'static> {
    let style = if primary { badge_accent() } else { Style::new().fg(c_fg()).bg(c_sel()) };
    Span::styled(format!(" {label} "), style)
}

fn draw_modal(f: &mut Frame, app: &mut App, area: Rect) {
    let Some(modal) = app.modal.as_mut() else { return };
    let mut hits: Vec<(Rect, Hit)> = vec![];
    let mut picker_list = Rect::default();
    let mut cursor: Option<Position> = None;

    match modal {
        Modal::SecretEdit(e) => cursor = Some(draw_secret_edit(f, area, e)),
        Modal::Unlock(u) => cursor = Some(draw_unlock(f, area, u)),
        Modal::Master(m) => cursor = Some(draw_master(f, area, m)),
        Modal::Vault(v) => draw_vault(f, area, v),
        Modal::Form(form) => draw_form(f, area, form, &app.store, &mut hits, &mut cursor),
        Modal::Picker(p, _) => picker_list = draw_picker(f, area, p),
        Modal::Prompt(p) => {
            let r = centered(52, 5, area);
            f.render_widget(Clear, r);
            let block = modal_block(&p.title);
            let inner = block.inner(r);
            f.render_widget(block, r);
            let field = Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1);
            f.render_widget(Paragraph::new(p.input.value.as_str()).style(Style::new().fg(c_fg()).bg(c_sel())), field);
            cursor = Some(Position::new(field.x + p.input.cursor as u16, field.y));
            f.render_widget(
                Paragraph::new("Enter accept · Esc cancel").style(Style::new().fg(c_muted())),
                Rect::new(inner.x + 1, inner.y + 2, inner.width.saturating_sub(2), 1),
            );
        }
        Modal::Search(q) => {
            let shown = q.hits.len().min(8);
            let r = centered(64, 4 + shown.max(1) as u16, area);
            f.render_widget(Clear, r);
            let block = modal_block("Open");
            let inner = block.inner(r);
            f.render_widget(block, r);
            let field = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), 1);
            f.render_widget(Paragraph::new(q.input.value.as_str()).style(Style::new().fg(c_fg()).bg(c_sel())), field);
            cursor = Some(Position::new(field.x + q.input.cursor as u16, field.y));
            if q.hits.is_empty() {
                f.render_widget(Paragraph::new("Nothing matches.").style(Style::new().fg(c_muted())), Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 1));
            }
            // A window of the matches that keeps the selected one in view.
            let first = q.selected.saturating_sub(shown.saturating_sub(1));
            for (k, &i) in q.hits.iter().skip(first).take(shown).enumerate() {
                let item = &q.items[i];
                let sel = first + k == q.selected;
                let bg = if sel { c_sel() } else { Color::Reset };
                let w = inner.width.saturating_sub(2) as usize;
                let detail_w = item.detail.width().min(w / 2);
                let name = fit(&item.label, w.saturating_sub(detail_w + 4));
                let pad = w.saturating_sub(2 + name.width() + detail_w);
                let mark = match item.target {
                    SearchTarget::Space(_) => "▌ ",
                    SearchTarget::Server(_) => "▤ ",
                    SearchTarget::Local => "› ",
                };
                let row = Rect::new(inner.x + 1, inner.y + 1 + k as u16, w as u16, 1);
                f.render_widget(
                    Paragraph::new(Line::from(vec![
                        Span::styled(mark, Style::new().fg(c_accent()).bg(bg)),
                        Span::styled(name, Style::new().fg(c_fg()).bg(bg).add_modifier(if sel { Modifier::BOLD } else { Modifier::empty() })),
                        Span::styled(" ".repeat(pad), Style::new().bg(bg)),
                        Span::styled(fit(&item.detail, detail_w), Style::new().fg(c_muted()).bg(bg)),
                    ])),
                    row,
                );
                hits.push((row, Hit::Field(first + k)));
            }
            f.render_widget(
                Paragraph::new("↑↓ choose · Enter open · Esc cancel").style(Style::new().fg(c_muted())),
                Rect::new(inner.x + 1, inner.y + inner.height - 1, inner.width.saturating_sub(2), 1),
            );
        }
        Modal::Confirm(c) => {
            let r = centered(56, 6, area);
            f.render_widget(Clear, r);
            let block = modal_block("Confirm");
            let inner = block.inner(r);
            f.render_widget(block, r);
            f.render_widget(
                Paragraph::new(c.text.as_str()).style(Style::new().fg(c_fg())).wrap(ratatui::widgets::Wrap { trim: true }),
                Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 2),
            );
            let by = inner.y + inner.height - 1;
            f.render_widget(
                Paragraph::new(Line::from(vec![button("Yes (y)", true), Span::raw("  "), button("No (n)", false)])),
                Rect::new(inner.x + 1, by, inner.width - 2, 1),
            );
            hits.push((Rect::new(inner.x + 1, by, 8, 1), Hit::Yes));
            hits.push((Rect::new(inner.x + 11, by, 8, 1), Hit::No));
        }
    }
    app.layout.modal = hits;
    app.layout.picker_list = picker_list;
    if let Some(p) = cursor {
        f.set_cursor_position(p);
    }
}

fn draw_form(
    f: &mut Frame,
    area: Rect,
    form: &ServerForm,
    store: &Store,
    hits: &mut Vec<(Rect, Hit)>,
    cursor: &mut Option<Position>,
) {
    let fields = form.visible();
    let h = fields.len() as u16 + 6;
    let r = centered(66, h, area);
    f.render_widget(Clear, r);
    let title = if form.editing.is_some() { "Edit server" } else { "New server" };
    let block = modal_block(title);
    let inner = block.inner(r);
    f.render_widget(block, r);

    const LABEL_W: u16 = 14;
    for (n, &fi) in fields.iter().enumerate() {
        let y = inner.y + 1 + n as u16;
        let row = Rect::new(inner.x + 1, y, inner.width.saturating_sub(2), 1);
        hits.push((row, Hit::Field(fi)));
        let focused = form.focus == fi;
        let (label, placeholder) = match fi {
            F_NAME => ("Name", "(defaults to the host)"),
            F_HOST => ("Host", "example.com or 10.0.0.5"),
            F_PORT => ("Port", ""),
            F_USER => ("User", "(ssh default)"),
            F_AUTH => ("Auth", ""),
            F_JUMP => ("Jump via", ""),
            F_KEY => ("Private key", "path to the key"),
            F_SUDO => (
                "Sudo password",
                if form.clear_sudo {
                    "(will be removed on save - Ctrl+X to undo)"
                } else if form.sudo_saved {
                    "(saved in the vault - Ctrl+X to remove)"
                } else {
                    "optional - offered when sudo asks"
                },
            ),
            _ => (
                if form.auth == Auth::Password { "Password" } else { "Passphrase" },
                if form.clear_secret {
                    "(will be removed on save - Ctrl+X to undo)"
                } else if form.secret_saved {
                    "(saved in the vault - Ctrl+X to remove)"
                } else if form.auth == Auth::Key {
                    "optional"
                } else {
                    ""
                },
            ),
        };
        let label_style =
            if focused { Style::new().fg(c_accent()).add_modifier(Modifier::BOLD) } else { Style::new().fg(c_muted()) };
        let label_rect = Rect::new(row.x, y, LABEL_W, 1);
        f.render_widget(Paragraph::new(Span::styled(label, label_style)), label_rect);

        let browse_w = if fi == F_KEY { 12 } else { 0 };
        let val = Rect::new(row.x + LABEL_W, y, row.width.saturating_sub(LABEL_W + browse_w), 1);
        let bg = if focused { c_sel() } else { Color::Reset };
        if fi == F_JUMP {
            let name = form.jump.and_then(|j| store.server(j)).map(|s| s.name.as_str()).unwrap_or("(none)");
            let text = format!("◀ {name} ▶");
            f.render_widget(Paragraph::new(text).style(Style::new().fg(if focused { c_fg() } else { c_muted() }).bg(bg)), val);
            continue;
        }
        if fi == F_AUTH {
            let text = format!("◀ {} ▶", form.auth.label());
            f.render_widget(Paragraph::new(text).style(Style::new().fg(if focused { c_fg() } else { c_muted() }).bg(bg)), val);
            continue;
        }
        let input = match fi {
            F_NAME => &form.name,
            F_HOST => &form.host,
            F_PORT => &form.port,
            F_USER => &form.user,
            F_KEY => &form.key,
            F_SUDO => &form.sudo,
            _ => &form.secret,
        };
        let shown: String = if fi == F_SECRET || fi == F_SUDO {
            "•".repeat(input.value.chars().count())
        } else {
            input.value.clone()
        };
        let (text, style) = if shown.is_empty() {
            (placeholder.to_string(), Style::new().fg(c_muted()).bg(bg))
        } else {
            (shown, Style::new().fg(c_fg()).bg(bg))
        };
        // Horizontal scroll so the cursor is always visible
        let vis = val.width.saturating_sub(1) as usize;
        let skip = if focused { input.cursor.saturating_sub(vis) } else { 0 };
        let text: String = text.chars().skip(skip).take(val.width as usize).collect();
        f.render_widget(Paragraph::new(text).style(style), val);
        if focused {
            *cursor = Some(Position::new(val.x + (input.cursor - skip) as u16, y));
        }
        if fi == F_KEY {
            let b = Rect::new(val.x + val.width + 1, y, 11, 1);
            f.render_widget(Paragraph::new(Span::styled(" Browse… ", badge_accent())), b);
            hits.push((b, Hit::Browse));
        }
    }

    let ey = inner.y + 1 + fields.len() as u16;
    if let Some(e) = &form.error {
        f.render_widget(
            Paragraph::new(format!("✕ {e}")).style(Style::new().fg(c_red())),
            Rect::new(inner.x + 1, ey, inner.width.saturating_sub(2), 1),
        );
    }
    let by = inner.y + inner.height - 1;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            button("Save", true),
            Span::raw("  "),
            button("Cancel", false),
            Span::styled("  Tab next · Ctrl+O browse", Style::new().fg(c_muted())),
        ])),
        Rect::new(inner.x + 1, by, inner.width.saturating_sub(2), 1),
    );
    hits.push((Rect::new(inner.x + 1, by, 9, 1), Hit::Save));
    hits.push((Rect::new(inner.x + 12, by, 10, 1), Hit::Cancel));
}

fn draw_picker(f: &mut Frame, area: Rect, p: &mut Picker) -> Rect {
    let r = centered(74, 22, area);
    f.render_widget(Clear, r);
    let title = format!("Choose key — {}", fit(&p.dir.to_string_lossy(), 56));
    let block = modal_block(&title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let list = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), inner.height.saturating_sub(2));
    let h = list.height as usize;

    // The offset lives in the picker; adjust it here to keep the selection visible.
    if p.selected < p.offset {
        p.offset = p.selected;
    } else if p.selected >= p.offset + h {
        p.offset = p.selected + 1 - h;
    }
    let offset = p.offset;
    for (n, e) in p.entries.iter().enumerate().skip(offset).take(h) {
        let y = list.y + (n - offset) as u16;
        let (icon, style) = if e.is_dir {
            ("▸ ", Style::new().fg(c_accent()))
        } else if e.key_like {
            ("◆ ", Style::new().fg(c_green()).add_modifier(Modifier::BOLD))
        } else {
            ("  ", Style::new().fg(c_muted()))
        };
        let tag = if e.key_like { "  private key" } else { "" };
        let name = fit(&e.name, list.width as usize - 2 - tag.width());
        let mut line_style = Style::new();
        if n == p.selected {
            line_style = line_style.bg(c_sel());
        }
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(icon, style),
                Span::styled(name, style),
                Span::styled(tag, Style::new().fg(c_muted())),
            ]))
            .style(line_style),
            Rect::new(list.x, y, list.width, 1),
        );
    }
    f.render_widget(
        Paragraph::new("Enter open/select · Backspace up · . hidden · ~ home · Esc back").style(Style::new().fg(c_muted())),
        Rect::new(inner.x + 1, inner.y + inner.height - 1, inner.width.saturating_sub(2), 1),
    );
    Rect::new(list.x, list.y, list.width, list.height)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ansi_green_is_remapped_but_other_colors_are_not() {
        assert_eq!(map_color(vt100::Color::Idx(2)), REMOTE_GREEN);
        assert_eq!(map_color(vt100::Color::Idx(10)), REMOTE_GREEN_BRIGHT);
        assert_eq!(map_color(vt100::Color::Idx(1)), Color::Indexed(1));
        assert_eq!(map_color(vt100::Color::Default), Color::Reset);
    }

    /// Draws an SSH view with `n` servers at the root plus one bastion with a server behind it.
    fn drawn(n: usize, height: u16) -> (App, Vec<String>) {
        use crate::store::{Server, Store};
        let mut st = Store::default();
        for i in 0..n {
            st.add_server(Server {
                id: 0,
                name: format!("srv{i}"),
                host: format!("h{i}.example"),
                port: 22,
                user: String::new(),
                auth: Auth::Agent,
                key_path: String::new(),
                parent: None,
                jump: None,
                has_secret: false,
                has_sudo: false,
            });
        }
        let ids: Vec<u64> = st.servers.iter().map(|s| s.id).collect();
        if ids.len() > 1 {
            st.move_via(ids[1], Some(ids[0]));
        }
        let vault = crate::vault::Vault::new(std::env::temp_dir().join(format!("ship-ui-vault-{}", std::process::id())));
        let mut app = App::new(st, vault);
        app.rebuild();
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, height)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        let buf = term.backend().buffer().clone();
        let lines = (0..height).map(|y| (0..80).map(|x| buf[(x, y)].symbol()).collect::<String>()).collect();
        (app, lines)
    }

    #[test]
    fn the_bastions_start_right_after_the_servers() {
        for height in [20u16, 30, 41] {
            let (app, lines) = drawn(3, height);
            let (top, bottom) = (app.layout.list, app.layout.list_bottom);
            assert!(bottom.height > 0, "split at {height}");
            let header = app.split_index().unwrap();
            assert_eq!(top.height as usize, header + 1, "the top part holds the servers and one blank line ({height})");
            assert_eq!(bottom.y, top.y + top.height);
            assert!(lines[bottom.y as usize].contains("bastions"), "the title is the first line of the lower part ({height})");
            let title_rows = lines.iter().filter(|l| l.contains("bastions")).count();
            assert_eq!(title_rows, 1);
        }
    }

    #[test]
    fn the_lower_half_scrolls_on_its_own_and_follows_the_selection() {
        let (mut app, _) = drawn(12, 26);
        // Open the bastion so the lower half has more rows than it can show, then go to its last row.
        app.jump_open.insert(app.store.servers[0].id);
        app.rebuild();
        app.selected = app.rows.len() - 1;
        let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(80, 26)).unwrap();
        term.draw(|f| draw(f, &mut app)).unwrap();
        assert_eq!(app.offset, 0, "the upper half did not move");
        let split = app.split_index().unwrap();
        assert!(app.rows.len() > split + 1);
    }

    #[test]
    fn without_bastions_the_list_is_not_split() {
        let (app, lines) = drawn(1, 30);
        assert_eq!(app.layout.list_bottom.height, 0);
        assert!(!lines.iter().any(|l| l.contains("bastions")));
    }
}

// ---------------------------------------------------------------- vault

fn mask(s: &str) -> String {
    "•".repeat(s.chars().count())
}

/// A masked single-line field; returns the cursor position when `focused`.
fn draw_secret_field(f: &mut Frame, rect: Rect, input: &Input, focused: bool) -> Position {
    let bg = if focused { c_sel() } else { Color::Reset };
    let skip = input.cursor.saturating_sub(rect.width.saturating_sub(1) as usize);
    let shown: String = mask(&input.value).chars().skip(skip).collect();
    f.render_widget(Paragraph::new(shown).style(Style::new().fg(c_fg()).bg(bg)), rect);
    Position::new(rect.x + (input.cursor - skip) as u16, rect.y)
}

fn draw_unlock(f: &mut Frame, area: Rect, u: &UnlockModal) -> Position {
    let h = if u.creating { 11 } else { 7 };
    let r = centered(60, h, area);
    f.render_widget(Clear, r);
    let block = modal_block(if u.creating { "Create the password vault" } else { "Unlock the vault" });
    let inner = block.inner(r);
    f.render_widget(block, r);
    let w = inner.width.saturating_sub(2);
    let x = inner.x + 1;
    let mut y = inner.y;
    let mut cursor = Position::new(x, y);

    if u.creating {
        f.render_widget(
            Paragraph::new(vec![
                Line::from("Your saved passwords will be encrypted with a master"),
                Line::from("password that is never stored anywhere."),
                Line::from(Span::styled("If you forget it, they cannot be recovered.", Style::new().fg(c_amber()))),
            ])
            .style(Style::new().fg(c_fg())),
            Rect::new(x, y, w, 3),
        );
        y += 4;
    } else {
        y += 1;
    }

    let label_w = 10;
    f.render_widget(Paragraph::new("Master").style(Style::new().fg(c_muted())), Rect::new(x, y, label_w, 1));
    let field = Rect::new(x + label_w, y, w.saturating_sub(label_w), 1);
    let c = draw_secret_field(f, field, &u.input, u.focus == 0);
    if u.focus == 0 {
        cursor = c;
    }
    if u.creating {
        y += 1;
        f.render_widget(Paragraph::new("Repeat").style(Style::new().fg(c_muted())), Rect::new(x, y, label_w, 1));
        let field = Rect::new(x + label_w, y, w.saturating_sub(label_w), 1);
        let c = draw_secret_field(f, field, &u.confirm, u.focus == 1);
        if u.focus == 1 {
            cursor = c;
        }
    }
    y += 1;
    if let Some(e) = &u.error {
        f.render_widget(Paragraph::new(format!("✕ {e}")).style(Style::new().fg(c_red())), Rect::new(x, y, w, 1));
    }
    let hint = if u.creating { "Enter next/create · Esc cancel" } else { "Enter unlock · Esc skip" };
    f.render_widget(Paragraph::new(hint).style(Style::new().fg(c_muted())), Rect::new(x, inner.y + inner.height - 1, w, 1));
    cursor
}

fn draw_master(f: &mut Frame, area: Rect, m: &MasterModal) -> Position {
    let r = centered(60, 8, area);
    f.render_widget(Clear, r);
    let block = modal_block("Change the master password");
    let inner = block.inner(r);
    f.render_widget(block, r);
    let w = inner.width.saturating_sub(2);
    let x = inner.x + 1;
    let label_w = 10;
    let mut cursor = Position::new(x, inner.y + 1);
    for (i, (label, input)) in [("New", &m.input), ("Repeat", &m.confirm)].into_iter().enumerate() {
        let y = inner.y + 1 + i as u16;
        f.render_widget(Paragraph::new(label).style(Style::new().fg(c_muted())), Rect::new(x, y, label_w, 1));
        let c = draw_secret_field(f, Rect::new(x + label_w, y, w.saturating_sub(label_w), 1), input, m.focus == i);
        if m.focus == i {
            cursor = c;
        }
    }
    if let Some(e) = &m.error {
        f.render_widget(Paragraph::new(format!("✕ {e}")).style(Style::new().fg(c_red())), Rect::new(x, inner.y + 3, w, 1));
    }
    f.render_widget(
        Paragraph::new("Enter next/save · Esc cancel").style(Style::new().fg(c_muted())),
        Rect::new(x, inner.y + inner.height - 1, w, 1),
    );
    cursor
}

fn draw_vault(f: &mut Frame, area: Rect, v: &VaultView) {
    let r = centered(78, 20, area);
    f.render_widget(Clear, r);
    let block = modal_block("Password vault");
    let inner = block.inner(r);
    f.render_widget(block, r);
    let w = inner.width.saturating_sub(2) as usize;
    let x = inner.x + 1;

    if v.rows.is_empty() {
        f.render_widget(
            Paragraph::new("Nothing saved yet. Passwords you enter when adding a server appear here.")
                .style(Style::new().fg(c_muted())),
            Rect::new(x, inner.y + 1, w as u16, 1),
        );
    }
    let list_h = inner.height.saturating_sub(3) as usize;
    let top = v.selected.saturating_sub(list_h.saturating_sub(1));
    for (n, row) in v.rows.iter().enumerate().skip(top).take(list_h) {
        let y = inner.y + (n - top) as u16;
        let shown = v.shown.as_ref().filter(|s| s.id == row.id);
        let cell = |present: bool, value: Option<&String>| -> Span<'static> {
            match (present, value) {
                (false, _) => Span::styled("—".to_string(), Style::new().fg(c_muted())),
                (true, Some(s)) => Span::styled(s.clone(), Style::new().fg(c_green()).add_modifier(Modifier::BOLD)),
                (true, None) => Span::styled("••••••••".to_string(), Style::new().fg(c_fg())),
            }
        };
        let name = fit(&row.name, 20);
        let host = fit(&row.host, 22);
        let line = Line::from(vec![
            Span::styled(format!("{name:<21}"), Style::new().fg(c_fg())),
            Span::styled(format!("{host:<23}"), Style::new().fg(c_muted())),
            Span::styled("login ", Style::new().fg(c_muted())),
            cell(row.login, shown.and_then(|s| s.login.as_ref())),
            Span::styled("  sudo ", Style::new().fg(c_muted())),
            cell(row.sudo, shown.and_then(|s| s.sudo.as_ref())),
        ]);
        let style = if n == v.selected { Style::new().bg(c_sel()) } else { Style::new() };
        f.render_widget(Paragraph::new(line).style(style), Rect::new(x, y, w as u16, 1));
        if n == v.selected {
            // Underline the column the c / e / d keys act on.
            let (cx, cw) = if v.col == 0 { (x + 50, 6 + 8) } else { (x + 50 + 14 + 8, 6 + 8) };
            let col = Rect::new(cx.min(x + w as u16), y, cw.min((x + w as u16).saturating_sub(cx)), 1);
            f.buffer_mut().set_style(col, Style::new().add_modifier(Modifier::UNDERLINED));
        }
    }
    f.render_widget(
        Paragraph::new("↑↓ row · ←→ column · r reveal · c copy · e change · d remove · m master · Esc")
            .style(Style::new().fg(c_muted())),
        Rect::new(x, inner.y + inner.height - 1, w as u16, 1),
    );
}

fn draw_secret_edit(f: &mut Frame, area: Rect, e: &SecretEdit) -> Position {
    let r = centered(64, 7, area);
    f.render_widget(Clear, r);
    let title = fit(&e.title, 58);
    let block = modal_block(&title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let x = inner.x + 1;
    let w = inner.width.saturating_sub(2);
    let field = Rect::new(x + 6, inner.y + 1, w.saturating_sub(6), 1);
    f.render_widget(Paragraph::new("New").style(Style::new().fg(c_muted())), Rect::new(x, inner.y + 1, 6, 1));
    let cursor = if e.show {
        let skip = e.input.cursor.saturating_sub(field.width.saturating_sub(1) as usize);
        let shown: String = e.input.value.chars().skip(skip).collect();
        f.render_widget(Paragraph::new(shown).style(Style::new().fg(c_fg()).bg(c_sel())), field);
        Position::new(field.x + (e.input.cursor - skip) as u16, field.y)
    } else {
        draw_secret_field(f, field, &e.input, true)
    };
    if let Some(err) = &e.error {
        f.render_widget(
            Paragraph::new(format!("✕ {err}")).style(Style::new().fg(c_red())),
            Rect::new(x, inner.y + 2, w, 1),
        );
    }
    f.render_widget(
        Paragraph::new("Enter save · Ctrl+T show/hide · Esc cancel").style(Style::new().fg(c_muted())),
        Rect::new(x, inner.y + inner.height - 1, w, 1),
    );
    cursor
}
