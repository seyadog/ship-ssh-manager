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

const ACCENT: Color = Color::Rgb(122, 162, 247);
const FG: Color = Color::Rgb(192, 202, 245);
const MUTED: Color = Color::Rgb(86, 95, 137);
const SEL_BG: Color = Color::Rgb(40, 52, 87);
const DROP_BG: Color = Color::Rgb(58, 82, 60);
const GREEN: Color = Color::Rgb(158, 206, 106);
const AMBER: Color = Color::Rgb(224, 175, 104);
const REMOTE_GREEN: Color = Color::Rgb(187, 154, 247);
const REMOTE_GREEN_BRIGHT: Color = Color::Rgb(208, 184, 255);
const RED: Color = Color::Rgb(247, 118, 142);

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
    app.layout.agents.clear();
    app.layout.toolbar.clear();
    app.layout.modal.clear();

    let [main, status] = RLayout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(area);
    let side_w = 32.min(area.width / 2);
    let [side, right] = RLayout::horizontal([Constraint::Length(side_w), Constraint::Min(1)]).areas(main);
    let [tabbar, content] = RLayout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(right);

    app.layout.sidebar = side;
    app.layout.tabbar = tabbar;
    app.layout.content = content;

    draw_sidebar(f, app, side);
    draw_tabs(f, app, tabbar);
    draw_content(f, app, content);
    draw_status(f, app, status);
    draw_modal(f, app, area);
}

// ---------------------------------------------------------------- sidebar

fn draw_sidebar(f: &mut Frame, app: &mut App, area: Rect) {
    let focused = app.focus == Focus::Sidebar && app.modal.is_none();
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(if focused { ACCENT } else { MUTED }))
        .title(Span::styled(" ship ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }

    // Action bar
    let bar = Rect::new(inner.x, inner.y, inner.width, 1);
    let in_spaces = app.view == View::Spaces;
    let add_srv = if in_spaces { " + Space " } else { " + Server " };
    let add_dir = if in_spaces { " + Terminal " } else { " + Folder " };
    let w1 = add_srv.width() as u16;
    let w2 = add_dir.width() as u16;
    let edit = " Edit ";
    let w3 = edit.width() as u16;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(add_srv, Style::new().fg(Color::Black).bg(ACCENT)),
            Span::raw(" "),
            Span::styled(add_dir, Style::new().fg(FG).bg(SEL_BG)),
            Span::raw(" "),
            Span::styled(edit, Style::new().fg(FG).bg(SEL_BG)),
        ])),
        bar,
    );
    app.layout.toolbar.push((Rect::new(bar.x, bar.y, w1, 1), Hit::AddServer));
    app.layout.toolbar.push((Rect::new(bar.x + w1 + 1, bar.y, w2, 1), Hit::AddFolder));
    app.layout.toolbar.push((Rect::new(bar.x + w1 + w2 + 2, bar.y, w3, 1), Hit::Edit));

    // View switch
    let views = [
        (" Folders ", View::Folders, Hit::ViewFolders),
        (" Jump hosts ", View::Jump, Hit::ViewJump),
        (" Spaces ", View::Spaces, Hit::ViewSpaces),
    ];
    let mut vx = inner.x;
    let mut spans = vec![];
    for (label, v, hit) in views {
        let w = label.width() as u16;
        let style = if app.view == v {
            Style::new().fg(ACCENT).add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
        } else {
            Style::new().fg(MUTED)
        };
        spans.push(Span::styled(label, style));
        app.layout.toolbar.push((Rect::new(vx, inner.y + 1, w, 1), hit));
        vx += w;
    }
    f.render_widget(Paragraph::new(Line::from(spans)), Rect::new(inner.x, inner.y + 1, inner.width, 1));
    // Terminal entry pinned at the bottom: this computer's shell, or a new terminal in the current space.
    let bottom_h: u16 = if inner.height >= 6 { 1 } else { 0 };
    if bottom_h > 0 {
        let label = if in_spaces { " ⌂ New terminal here " } else { " ⌂ Local terminal " };
        let r = Rect::new(inner.x, inner.y + inner.height - 1, (label.width() as u16).min(inner.width), 1);
        f.render_widget(Paragraph::new(label).style(Style::new().fg(FG).bg(SEL_BG)), r);
        app.layout.toolbar.push((r, Hit::LocalTerm));
    }
    // Keyboard focus on the header: highlight the chosen item.
    if let (Some(i), true) = (app.header, focused && app.modal.is_none()) {
        if let Some(&(r, _)) = app.layout.toolbar.get(i) {
            f.buffer_mut().set_style(r, Style::new().add_modifier(Modifier::REVERSED | Modifier::BOLD));
        }
    }

    // Agents panel: every AI agent running in any terminal, above the terminal entry.
    let agents = app.agent_tabs();
    let shown = agents.len().min(5);
    let panel_h: u16 = if shown > 0 && inner.height >= 12 { shown as u16 + 1 } else { 0 };
    if panel_h > 0 {
        let top = inner.y + inner.height - bottom_h - panel_h;
        let extra = if agents.len() > shown { format!(" +{}", agents.len() - shown) } else { String::new() };
        f.render_widget(
            Paragraph::new(format!("agents{extra}")).style(Style::new().fg(MUTED).add_modifier(Modifier::BOLD)),
            Rect::new(inner.x, top, inner.width, 1),
        );
        for (k, &ti) in agents.iter().take(shown).enumerate() {
            let tab = &app.tabs[ti];
            let Some(info) = tab.session.agent() else { continue };
            let (dot, color) = if tab.attention {
                ("●", GREEN)
            } else if info.working {
                ("●", AMBER)
            } else {
                ("○", MUTED)
            };
            let here = ti == app.active && tab.scope == app.scope && app.focus == Focus::Terminal;
            let room = (inner.width as usize).saturating_sub(3 + info.name.width() + 2);
            let place = fit(&app.tab_place(tab), room);
            let bg = if here { SEL_BG } else { Color::Reset };
            let line = Line::from(vec![
                Span::styled(format!("{dot} "), Style::new().fg(color).bg(bg)),
                Span::styled(info.name.clone(), Style::new().fg(if tab.attention { GREEN } else { FG }).bg(bg)),
                Span::styled(format!("  {place}"), Style::new().fg(MUTED).bg(bg)),
            ]);
            let r = Rect::new(inner.x, top + 1 + k as u16, inner.width, 1);
            f.render_widget(Paragraph::new(line).style(Style::new().bg(bg)), r);
            app.layout.agents.push((r, ti));
        }
    }

    let list_h = inner.height.saturating_sub(3 + bottom_h + panel_h);
    let list = Rect::new(inner.x, inner.y + 3, inner.width, list_h);
    app.layout.list = list;
    if list.height == 0 {
        return;
    }

    let h = list.height as usize;
    if app.selected < app.offset {
        app.offset = app.selected;
    } else if app.selected >= app.offset + h {
        app.offset = app.selected + 1 - h;
    }
    app.offset = app.offset.min(app.rows.len().saturating_sub(1));

    if app.rows.is_empty() {
        let lines: Vec<&str> = if app.view == View::Spaces {
            vec!["No spaces yet.", "A space is a project directory", "with its own terminals.", "Click “+ Space” or press a."]
        } else if app.view == View::Jump && !app.store.servers.is_empty() {
            vec!["No jump hosts yet.", "Edit a server and set “Jump via”: the", "bastion appears here with what is", "reached through it."]
        } else {
            vec!["No servers yet.", "Click “+ Server” or press a."]
        };
        let hint = Paragraph::new(lines.into_iter().map(|l| Line::from(Span::styled(l, Style::new().fg(MUTED)))).collect::<Vec<_>>());
        f.render_widget(hint, list);
        return;
    }

    let width = list.width as usize;
    for (n, row) in app.rows.iter().enumerate().skip(app.offset).take(h) {
        let y = list.y + (n - app.offset) as u16;
        let is_sel = n == app.selected;
        let is_drop = app.drop_hover == Some(n);
        let indent = "  ".repeat(row.depth);
        let mut spans = vec![Span::raw(indent.clone())];
        match row.node {
            NodeId::Space(id) => {
                let name = app.spaces.get(id).map(|s| s.name.as_str()).unwrap_or("?");
                let mine = |t: &&Tab| t.scope == Scope::Space(id);
                let live = app.tabs.iter().filter(mine).any(|t| t.session.exit_code.is_none());
                let attention = app.tabs.iter().filter(mine).any(|t| t.attention);
                let (dot, color) = if attention {
                    ("● ", GREEN)
                } else if live {
                    ("● ", ACCENT)
                } else {
                    ("○ ", MUTED)
                };
                spans.push(Span::styled(dot, Style::new().fg(color)));
                spans.push(Span::styled(fit(name, width.saturating_sub(indent.width() + 3)), Style::new().fg(if attention { GREEN } else { FG })));
                if let Some(b) = app.branches.get(&id) {
                    let room = width.saturating_sub(indent.width() + 2 + name.width() + 2);
                    if room > 3 {
                        spans.push(Span::styled(format!("  {}", fit(b, room)), Style::new().fg(REMOTE_GREEN)));
                    }
                }
            }
            NodeId::Folder(id) => {
                let fo = app.store.folder(id);
                let open = fo.is_some_and(|f| f.expanded);
                let name = fo.map(|f| f.name.as_str()).unwrap_or("?");
                let label = fit(name, width.saturating_sub(indent.width() + 3));
                spans.push(Span::styled(if open { "▾ " } else { "▸ " }, Style::new().fg(AMBER)));
                spans.push(Span::styled(label, Style::new().fg(AMBER).add_modifier(Modifier::BOLD)));
            }
            NodeId::Server(id) => {
                let s = app.store.server(id);
                let name = s.map(|s| s.name.as_str()).unwrap_or("?");
                let live = app.tabs.iter().any(|t| t.server_id == id && t.session.exit_code.is_none());
                let label = fit(name, width.saturating_sub(indent.width() + 3));
                if app.has_children(row.node) {
                    let arrow = if app.is_open(row.node) { "▾ " } else { "▸ " };
                    spans.push(Span::styled(arrow, Style::new().fg(ACCENT)));
                } else {
                    spans.push(Span::styled(
                        if live { "● " } else { "○ " },
                        Style::new().fg(if live { GREEN } else { MUTED }),
                    ));
                }
                spans.push(Span::styled(label, Style::new().fg(if live { GREEN } else { FG })));
                if let Some(s) = s {
                    let room = width.saturating_sub(indent.width() + 2 + name.width() + 2);
                    let via = match (app.view, s.jump.and_then(|j| app.store.server(j))) {
                        (View::Folders, Some(j)) => format!("{} ↪ {}", s.host, j.name),
                        _ => s.host.clone(),
                    };
                    if room > 6 {
                        spans.push(Span::styled(format!("  {}", fit(&via, room)), Style::new().fg(MUTED)));
                    }
                }
            }
        }
        let mut style = Style::new();
        if is_drop {
            style = style.bg(DROP_BG);
        } else if is_sel {
            style = style.bg(SEL_BG);
            if focused {
                style = style.add_modifier(Modifier::BOLD);
            }
        }
        let line = Line::from(spans).style(style);
        f.render_widget(Paragraph::new(line).style(style), Rect::new(list.x, y, list.width, 1));
    }
}

// ---------------------------------------------------------------- tabs

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let end = area.x + area.width;
    let scoped = app.scoped();
    let n = scoped.len();
    let mut x = area.x + 1;
    let mut hits = vec![];
    if n > 0 {
        // Shrink titles so that as many tabs as possible fit, then scroll the bar so the active tab is always visible.
        let avail = area.width.saturating_sub(4) as usize;
        let max_title = (avail / n).saturating_sub(9).clamp(6, 20);
        let cell = |p: usize| fit(&app.tabs[scoped[p]].title, max_title).width() + 8 + 1;
        let active = scoped.iter().position(|&g| g == app.active).unwrap_or(0);
        let mut start = 0;
        while start < active && (start..=active).map(cell).sum::<usize>() > avail {
            start += 1;
        }
        for pos in start..n {
            let i = scoped[pos];
            let tab = &app.tabs[i];
            let dead = tab.session.exit_code.is_some();
            let title = fit(&tab.title, max_title);
            let num = if pos < 9 { format!("{} ", pos + 1) } else { "  ".to_string() };
            let text = format!(" {num}{} {} ✕ ", if dead { "○" } else { "●" }, title);
            let w = text.width() as u16;
            if x + w > end {
                break;
            }
            let is_active = i == app.active;
            let dot = if dead {
                RED
            } else if tab.attention {
                AMBER
            } else {
                GREEN
            };
            let bg = if is_active { SEL_BG } else { Color::Reset };
            let fg = if is_active { FG } else { MUTED };
            let rect = Rect::new(x, area.y, w, 1);
            f.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled(" ", Style::new().bg(bg)),
                    Span::styled(num, Style::new().fg(MUTED).bg(bg)),
                    Span::styled(if dead { "○ " } else { "● " }, Style::new().fg(dot).bg(bg)),
                    Span::styled(
                        title,
                        Style::new().fg(fg).bg(bg).add_modifier(if is_active { Modifier::BOLD } else { Modifier::empty() }),
                    ),
                    Span::styled(" ✕ ", Style::new().fg(MUTED).bg(bg)),
                ])),
                rect,
            );
            hits.push(TabHit { idx: i, rect, close: Rect::new(x + w - 3, area.y, 3, 1) });
            x += w + 1;
        }
    }
    // “+” opens a new terminal in this scope.
    if app.scope != Scope::Space(0) && x + 3 <= end {
        let r = Rect::new(x, area.y, 3, 1);
        f.render_widget(Paragraph::new(" + ").style(Style::new().fg(ACCENT)), r);
        app.layout.toolbar.push((r, Hit::NewTab));
    }
    app.layout.tabs = hits;
}

// ---------------------------------------------------------------- contenido

fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    if app.scoped().is_empty() {
        return draw_welcome(f, app, area);
    }
    let focused = app.focus == Focus::Terminal && app.modal.is_none();
    let idx = app.active.min(app.tabs.len() - 1);
    let tab = &mut app.tabs[idx];
    tab.session.resize(area.height, area.width);

    let mut cursor = None;
    let mut blank = false;
    let buf = f.buffer_mut();
    tab.session.with_screen(|screen| {
        blank = screen.contents().trim().is_empty();
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
    // ssh prints nothing while it waits for an unreachable host: say what is going on.
    let waited = tab.session.started.elapsed().as_secs();
    if blank && tab.session.exit_code.is_none() && waited >= 2 {
        let target = app.store.server(tab.server_id).map(|s| format!("{}:{}", s.host, s.port)).unwrap_or_default();
        let msg = format!("Connecting to {target}… {waited}s (gives up after 15s; Alt+W closes this tab)");
        let w = (msg.width() as u16).min(area.width);
        f.render_widget(Paragraph::new(msg).style(Style::new().fg(MUTED)), Rect::new(area.x, area.y, w, 1));
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
            Paragraph::new(fit(text, area.width as usize)).style(Style::new().fg(Color::Black).bg(ACCENT)),
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
                AMBER
            } else {
                RED
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

fn draw_welcome(f: &mut Frame, app: &App, area: Rect) {
    let logo = ["┏━┓╻ ╻╻┏━┓", "┗━┓┣━┫┃┣━┛", "┗━┛╹ ╹╹╹  "];
    let mut lines: Vec<Line> = logo
        .iter()
        .map(|l| Line::from(Span::styled(*l, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))))
        .collect();
    lines.push(Line::raw(""));
    let hints: Vec<String> = match app.scope {
        Scope::Ssh => vec![
            "Double-click a server to open a session.".into(),
            "Drag servers and folders to reorganize them.".into(),
            "F6 switches between the panel and the terminal.".into(),
        ],
        Scope::Space(0) => vec!["No spaces yet.".into(), "Press a (or “+ Space”) and give it a project directory.".into()],
        Scope::Space(id) => {
            let sp = app.spaces.get(id);
            vec![
                sp.map(|s| s.name.clone()).unwrap_or_default(),
                sp.map(|s| s.cwd.clone()).unwrap_or_default(),
                "Press Enter or t to open a terminal here.".into(),
                "Run claude, opencode… in it: it shows under “agents”.".into(),
            ]
        }
    };
    for t in hints {
        lines.push(Line::from(Span::styled(t, Style::new().fg(MUTED))));
    }
    let h = lines.len() as u16;
    let r = centered(area.width, h, area);
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), r);
}

// ---------------------------------------------------------------- status bar

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let text = if let Some((msg, _)) = &app.flash {
        return f.render_widget(Paragraph::new(format!(" {msg}")).style(Style::new().fg(AMBER)), area);
    } else if app.modal.is_some() {
        "Esc cancel".to_string()
    } else if app.focus == Focus::Terminal {
        "F6 panel · Alt+←/→ tabs · Alt+Shift+←/→ move · Alt+W close · F2 rename · Shift+PgUp scrollback".to_string()
    } else if app.header.is_some() {
        "←→ choose · Enter activate · ↓ back to the list · Esc list".to_string()
    } else if app.view == View::Spaces && app.header.is_none() {
        "↑↓ choose · Enter open · a new space · e rename · d delete · t terminal · Alt+n next agent · q quit".to_string()
    } else if app.header.is_some() {
        "←→ choose · Enter activate · ↓ back to the list · Esc list".to_string()
    } else {
        "↑↓ move · ↑ at the top for the menu · 1-9 go to tab · t local terminal · → in · ← out · Enter open · v view · a server · f folder · e edit · d delete · c close tab · p passwords · q quit"
            .to_string()
    };
    f.render_widget(Paragraph::new(format!(" {text}")).style(Style::new().fg(MUTED)), area);
}

// ---------------------------------------------------------------- modales

fn modal_block(title: &str) -> Block<'_> {
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Span::styled(format!(" {title} "), Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)))
}

fn button(label: &str, primary: bool) -> Span<'static> {
    let style = if primary { Style::new().fg(Color::Black).bg(ACCENT) } else { Style::new().fg(FG).bg(SEL_BG) };
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
            f.render_widget(Paragraph::new(p.input.value.as_str()).style(Style::new().fg(FG).bg(SEL_BG)), field);
            cursor = Some(Position::new(field.x + p.input.cursor as u16, field.y));
            f.render_widget(
                Paragraph::new("Enter accept · Esc cancel").style(Style::new().fg(MUTED)),
                Rect::new(inner.x + 1, inner.y + 2, inner.width.saturating_sub(2), 1),
            );
        }
        Modal::Confirm(c) => {
            let r = centered(56, 6, area);
            f.render_widget(Clear, r);
            let block = modal_block("Confirm");
            let inner = block.inner(r);
            f.render_widget(block, r);
            f.render_widget(
                Paragraph::new(c.text.as_str()).style(Style::new().fg(FG)).wrap(ratatui::widgets::Wrap { trim: true }),
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
            if focused { Style::new().fg(ACCENT).add_modifier(Modifier::BOLD) } else { Style::new().fg(MUTED) };
        let label_rect = Rect::new(row.x, y, LABEL_W, 1);
        f.render_widget(Paragraph::new(Span::styled(label, label_style)), label_rect);

        let browse_w = if fi == F_KEY { 12 } else { 0 };
        let val = Rect::new(row.x + LABEL_W, y, row.width.saturating_sub(LABEL_W + browse_w), 1);
        let bg = if focused { SEL_BG } else { Color::Reset };
        if fi == F_JUMP {
            let name = form.jump.and_then(|j| store.server(j)).map(|s| s.name.as_str()).unwrap_or("(none)");
            let text = format!("◀ {name} ▶");
            f.render_widget(Paragraph::new(text).style(Style::new().fg(if focused { FG } else { MUTED }).bg(bg)), val);
            continue;
        }
        if fi == F_AUTH {
            let text = format!("◀ {} ▶", form.auth.label());
            f.render_widget(Paragraph::new(text).style(Style::new().fg(if focused { FG } else { MUTED }).bg(bg)), val);
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
            (placeholder.to_string(), Style::new().fg(MUTED).bg(bg))
        } else {
            (shown, Style::new().fg(FG).bg(bg))
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
            f.render_widget(Paragraph::new(Span::styled(" Browse… ", Style::new().fg(Color::Black).bg(ACCENT))), b);
            hits.push((b, Hit::Browse));
        }
    }

    let ey = inner.y + 1 + fields.len() as u16;
    if let Some(e) = &form.error {
        f.render_widget(
            Paragraph::new(format!("✕ {e}")).style(Style::new().fg(RED)),
            Rect::new(inner.x + 1, ey, inner.width.saturating_sub(2), 1),
        );
    }
    let by = inner.y + inner.height - 1;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            button("Save", true),
            Span::raw("  "),
            button("Cancel", false),
            Span::styled("  Tab next · Ctrl+O browse", Style::new().fg(MUTED)),
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
            ("▸ ", Style::new().fg(ACCENT))
        } else if e.key_like {
            ("◆ ", Style::new().fg(GREEN).add_modifier(Modifier::BOLD))
        } else {
            ("  ", Style::new().fg(MUTED))
        };
        let tag = if e.key_like { "  private key" } else { "" };
        let name = fit(&e.name, list.width as usize - 2 - tag.width());
        let mut line_style = Style::new();
        if n == p.selected {
            line_style = line_style.bg(SEL_BG);
        }
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(icon, style),
                Span::styled(name, style),
                Span::styled(tag, Style::new().fg(MUTED)),
            ]))
            .style(line_style),
            Rect::new(list.x, y, list.width, 1),
        );
    }
    f.render_widget(
        Paragraph::new("Enter open/select · Backspace up · . hidden · ~ home · Esc back").style(Style::new().fg(MUTED)),
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
}

// ---------------------------------------------------------------- vault

fn mask(s: &str) -> String {
    "•".repeat(s.chars().count())
}

/// A masked single-line field; returns the cursor position when `focused`.
fn draw_secret_field(f: &mut Frame, rect: Rect, input: &Input, focused: bool) -> Position {
    let bg = if focused { SEL_BG } else { Color::Reset };
    let skip = input.cursor.saturating_sub(rect.width.saturating_sub(1) as usize);
    let shown: String = mask(&input.value).chars().skip(skip).collect();
    f.render_widget(Paragraph::new(shown).style(Style::new().fg(FG).bg(bg)), rect);
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
                Line::from(Span::styled("If you forget it, they cannot be recovered.", Style::new().fg(AMBER))),
            ])
            .style(Style::new().fg(FG)),
            Rect::new(x, y, w, 3),
        );
        y += 4;
    } else {
        y += 1;
    }

    let label_w = 10;
    f.render_widget(Paragraph::new("Master").style(Style::new().fg(MUTED)), Rect::new(x, y, label_w, 1));
    let field = Rect::new(x + label_w, y, w.saturating_sub(label_w), 1);
    let c = draw_secret_field(f, field, &u.input, u.focus == 0);
    if u.focus == 0 {
        cursor = c;
    }
    if u.creating {
        y += 1;
        f.render_widget(Paragraph::new("Repeat").style(Style::new().fg(MUTED)), Rect::new(x, y, label_w, 1));
        let field = Rect::new(x + label_w, y, w.saturating_sub(label_w), 1);
        let c = draw_secret_field(f, field, &u.confirm, u.focus == 1);
        if u.focus == 1 {
            cursor = c;
        }
    }
    y += 1;
    if let Some(e) = &u.error {
        f.render_widget(Paragraph::new(format!("✕ {e}")).style(Style::new().fg(RED)), Rect::new(x, y, w, 1));
    }
    let hint = if u.creating { "Enter next/create · Esc cancel" } else { "Enter unlock · Esc skip" };
    f.render_widget(Paragraph::new(hint).style(Style::new().fg(MUTED)), Rect::new(x, inner.y + inner.height - 1, w, 1));
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
        f.render_widget(Paragraph::new(label).style(Style::new().fg(MUTED)), Rect::new(x, y, label_w, 1));
        let c = draw_secret_field(f, Rect::new(x + label_w, y, w.saturating_sub(label_w), 1), input, m.focus == i);
        if m.focus == i {
            cursor = c;
        }
    }
    if let Some(e) = &m.error {
        f.render_widget(Paragraph::new(format!("✕ {e}")).style(Style::new().fg(RED)), Rect::new(x, inner.y + 3, w, 1));
    }
    f.render_widget(
        Paragraph::new("Enter next/save · Esc cancel").style(Style::new().fg(MUTED)),
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
                .style(Style::new().fg(MUTED)),
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
                (false, _) => Span::styled("—".to_string(), Style::new().fg(MUTED)),
                (true, Some(s)) => Span::styled(s.clone(), Style::new().fg(GREEN).add_modifier(Modifier::BOLD)),
                (true, None) => Span::styled("••••••••".to_string(), Style::new().fg(FG)),
            }
        };
        let name = fit(&row.name, 20);
        let host = fit(&row.host, 22);
        let line = Line::from(vec![
            Span::styled(format!("{name:<21}"), Style::new().fg(FG)),
            Span::styled(format!("{host:<23}"), Style::new().fg(MUTED)),
            Span::styled("login ", Style::new().fg(MUTED)),
            cell(row.login, shown.and_then(|s| s.login.as_ref())),
            Span::styled("  sudo ", Style::new().fg(MUTED)),
            cell(row.sudo, shown.and_then(|s| s.sudo.as_ref())),
        ]);
        let style = if n == v.selected { Style::new().bg(SEL_BG) } else { Style::new() };
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
            .style(Style::new().fg(MUTED)),
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
    f.render_widget(Paragraph::new("New").style(Style::new().fg(MUTED)), Rect::new(x, inner.y + 1, 6, 1));
    let cursor = if e.show {
        let skip = e.input.cursor.saturating_sub(field.width.saturating_sub(1) as usize);
        let shown: String = e.input.value.chars().skip(skip).collect();
        f.render_widget(Paragraph::new(shown).style(Style::new().fg(FG).bg(SEL_BG)), field);
        Position::new(field.x + (e.input.cursor - skip) as u16, field.y)
    } else {
        draw_secret_field(f, field, &e.input, true)
    };
    if let Some(err) = &e.error {
        f.render_widget(
            Paragraph::new(format!("✕ {err}")).style(Style::new().fg(RED)),
            Rect::new(x, inner.y + 2, w, 1),
        );
    }
    f.render_widget(
        Paragraph::new("Enter save · Ctrl+T show/hide · Esc cancel").style(Style::new().fg(MUTED)),
        Rect::new(x, inner.y + inner.height - 1, w, 1),
    );
    cursor
}
