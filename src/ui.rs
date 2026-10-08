//! Dibujo de la interfaz. Rellena `app.layout` para que el ratón sepa qué hay en cada zona.

use crate::app::*;
use crate::store::{Auth, NodeId};
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
const RED: Color = Color::Rgb(247, 118, 142);

/// Recorta `s` a `w` columnas, añadiendo «…» si no cabe.
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
        .title(Span::styled(" ⛵ ship ", Style::new().fg(ACCENT).add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    f.render_widget(block, area);
    if inner.height < 2 {
        return;
    }

    // Barra de acciones
    let bar = Rect::new(inner.x, inner.y, inner.width, 1);
    let add_srv = " + Servidor ";
    let add_dir = " + Carpeta ";
    let w1 = add_srv.width() as u16;
    let w2 = add_dir.width() as u16;
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(add_srv, Style::new().fg(Color::Black).bg(ACCENT)),
            Span::raw(" "),
            Span::styled(add_dir, Style::new().fg(FG).bg(SEL_BG)),
        ])),
        bar,
    );
    app.layout.toolbar.push((Rect::new(bar.x, bar.y, w1, 1), Hit::AddServer));
    app.layout.toolbar.push((Rect::new(bar.x + w1 + 1, bar.y, w2, 1), Hit::AddFolder));

    let list = Rect::new(inner.x, inner.y + 2, inner.width, inner.height.saturating_sub(2));
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
        let hint = Paragraph::new(vec![
            Line::from(Span::styled("Aún no hay servidores.", Style::new().fg(MUTED))),
            Line::from(Span::styled("Pulsa «+ Servidor» o la tecla a.", Style::new().fg(MUTED))),
        ]);
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
                spans.push(Span::styled(
                    if live { "● " } else { "○ " },
                    Style::new().fg(if live { GREEN } else { MUTED }),
                ));
                spans.push(Span::styled(label, Style::new().fg(FG)));
                if let Some(s) = s {
                    let room = width.saturating_sub(indent.width() + 2 + name.width() + 2);
                    if room > 6 {
                        spans.push(Span::styled(format!("  {}", fit(&s.host, room)), Style::new().fg(MUTED)));
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

// ---------------------------------------------------------------- pestañas

fn draw_tabs(f: &mut Frame, app: &mut App, area: Rect) {
    let mut x = area.x + 1;
    let end = area.x + area.width;
    let mut hits = vec![];
    for (i, tab) in app.tabs.iter().enumerate() {
        let dead = tab.session.exit_code.is_some();
        let title = fit(&tab.title, 20);
        let text = format!(" {} {} ✕ ", if dead { "○" } else { "●" }, title);
        let w = text.width() as u16;
        if x + w > end {
            break;
        }
        let active = i == app.active;
        let dot = if dead { RED } else { GREEN };
        let bg = if active { SEL_BG } else { Color::Reset };
        let fg = if active { FG } else { MUTED };
        let rect = Rect::new(x, area.y, w, 1);
        f.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ", Style::new().bg(bg)),
                Span::styled(if dead { "○ " } else { "● " }, Style::new().fg(dot).bg(bg)),
                Span::styled(
                    title,
                    Style::new().fg(fg).bg(bg).add_modifier(if active { Modifier::BOLD } else { Modifier::empty() }),
                ),
                Span::styled(" ✕ ", Style::new().fg(MUTED).bg(bg)),
            ])),
            rect,
        );
        hits.push(TabHit { rect, close: Rect::new(x + w - 3, area.y, 3, 1) });
        x += w + 1;
    }
    app.layout.tabs = hits;
}

// ---------------------------------------------------------------- contenido

fn draw_content(f: &mut Frame, app: &mut App, area: Rect) {
    if app.tabs.is_empty() {
        return draw_welcome(f, area);
    }
    let focused = app.focus == Focus::Terminal && app.modal.is_none();
    let idx = app.active.min(app.tabs.len() - 1);
    let tab = &mut app.tabs[idx];
    tab.session.resize(area.height, area.width);

    let mut cursor = None;
    let buf = f.buffer_mut();
    tab.session.with_screen(|screen| {
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
    if focused {
        if let (Some((x, y)), None) = (cursor, tab.session.exit_code) {
            f.set_cursor_position(Position::new(x, y));
        }
    }

    if let Some(code) = tab.session.exit_code {
        let msg = match code {
            0 => "Sesión cerrada.".to_string(),
            255 => "Sin conexión (255): mira el error de ssh arriba.".to_string(),
            n => format!("ssh terminó con código {n}."),
        };
        let bar = Rect::new(area.x, area.y + area.height.saturating_sub(1), area.width, 1);
        let text = format!(" {msg}  Enter: reconectar · Alt+W: cerrar ");
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
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}

fn draw_welcome(f: &mut Frame, area: Rect) {
    let logo = ["┏━┓╻ ╻╻┏━┓", "┗━┓┣━┫┃┣━┛", "┗━┛╹ ╹╹╹  "];
    let mut lines: Vec<Line> = logo
        .iter()
        .map(|l| Line::from(Span::styled(*l, Style::new().fg(ACCENT).add_modifier(Modifier::BOLD))))
        .collect();
    lines.push(Line::raw(""));
    for t in [
        "Doble clic en un servidor para abrir una sesión.",
        "Arrastra servidores y carpetas para reorganizarlos.",
        "F6 cambia entre el panel y el terminal.",
    ] {
        lines.push(Line::from(Span::styled(t, Style::new().fg(MUTED))));
    }
    let h = lines.len() as u16;
    let r = centered(area.width, h, area);
    f.render_widget(Paragraph::new(lines).alignment(Alignment::Center), r);
}

// ---------------------------------------------------------------- barra de estado

fn draw_status(f: &mut Frame, app: &App, area: Rect) {
    let text = if let Some((msg, _)) = &app.flash {
        return f.render_widget(Paragraph::new(format!(" {msg}")).style(Style::new().fg(AMBER)), area);
    } else if app.modal.is_some() {
        "Esc cancelar".to_string()
    } else if app.focus == Focus::Terminal {
        "F6 panel · Alt+←/→ pestañas · Alt+Shift+←/→ mover · Alt+W cerrar · F2 renombrar · Shift+PgUp historial"
            .to_string()
    } else {
        "↑↓ mover · Enter abrir · a servidor · f carpeta · e editar · d borrar · Alt+↑↓ ordenar · q salir".to_string()
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
        Modal::Form(form) => draw_form(f, area, form, &mut hits, &mut cursor),
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
                Paragraph::new("Enter aceptar · Esc cancelar").style(Style::new().fg(MUTED)),
                Rect::new(inner.x + 1, inner.y + 2, inner.width.saturating_sub(2), 1),
            );
        }
        Modal::Confirm(c) => {
            let r = centered(56, 6, area);
            f.render_widget(Clear, r);
            let block = modal_block("Confirmar");
            let inner = block.inner(r);
            f.render_widget(block, r);
            f.render_widget(
                Paragraph::new(c.text.as_str()).style(Style::new().fg(FG)).wrap(ratatui::widgets::Wrap { trim: true }),
                Rect::new(inner.x + 1, inner.y + 1, inner.width.saturating_sub(2), 2),
            );
            let by = inner.y + inner.height - 1;
            f.render_widget(
                Paragraph::new(Line::from(vec![button("Sí (y)", true), Span::raw("  "), button("No (n)", false)])),
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

fn draw_form(f: &mut Frame, area: Rect, form: &ServerForm, hits: &mut Vec<(Rect, Hit)>, cursor: &mut Option<Position>) {
    let fields = form.visible();
    let h = fields.len() as u16 + 6;
    let r = centered(66, h, area);
    f.render_widget(Clear, r);
    let title = if form.editing.is_some() { "Editar servidor" } else { "Nuevo servidor" };
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
            F_NAME => ("Nombre", "(por defecto, el host)"),
            F_HOST => ("Host", "ejemplo.com o 10.0.0.5"),
            F_PORT => ("Puerto", ""),
            F_USER => ("Usuario", "(el de ssh por defecto)"),
            F_AUTH => ("Autenticación", ""),
            F_KEY => ("Clave privada", "ruta de la clave"),
            _ => (
                if form.auth == Auth::Password { "Contraseña" } else { "Passphrase" },
                if form.secret_saved {
                    "(guardada en el keyring)"
                } else if form.auth == Auth::Key {
                    "opcional"
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
            _ => &form.secret,
        };
        let shown: String =
            if fi == F_SECRET { "•".repeat(input.value.chars().count()) } else { input.value.clone() };
        let (text, style) = if shown.is_empty() {
            (placeholder.to_string(), Style::new().fg(MUTED).bg(bg))
        } else {
            (shown, Style::new().fg(FG).bg(bg))
        };
        // Desplazamiento horizontal para que el cursor siempre sea visible
        let vis = val.width.saturating_sub(1) as usize;
        let skip = if focused { input.cursor.saturating_sub(vis) } else { 0 };
        let text: String = text.chars().skip(skip).take(val.width as usize).collect();
        f.render_widget(Paragraph::new(text).style(style), val);
        if focused {
            *cursor = Some(Position::new(val.x + (input.cursor - skip) as u16, y));
        }
        if fi == F_KEY {
            let b = Rect::new(val.x + val.width + 1, y, 11, 1);
            f.render_widget(Paragraph::new(Span::styled(" Examinar… ", Style::new().fg(Color::Black).bg(ACCENT))), b);
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
            button("Guardar", true),
            Span::raw("  "),
            button("Cancelar", false),
            Span::styled("  Tab siguiente · Ctrl+O examinar", Style::new().fg(MUTED)),
        ])),
        Rect::new(inner.x + 1, by, inner.width.saturating_sub(2), 1),
    );
    hits.push((Rect::new(inner.x + 1, by, 9, 1), Hit::Save));
    hits.push((Rect::new(inner.x + 12, by, 10, 1), Hit::Cancel));
}

fn draw_picker(f: &mut Frame, area: Rect, p: &mut Picker) -> Rect {
    let r = centered(74, 22, area);
    f.render_widget(Clear, r);
    let title = format!("Elegir clave — {}", fit(&p.dir.to_string_lossy(), 56));
    let block = modal_block(&title);
    let inner = block.inner(r);
    f.render_widget(block, r);
    let list = Rect::new(inner.x + 1, inner.y, inner.width.saturating_sub(2), inner.height.saturating_sub(2));
    let h = list.height as usize;

    // El offset vive en el picker; se ajusta aquí para mantener la selección visible.
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
        let tag = if e.key_like { "  clave privada" } else { "" };
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
        Paragraph::new("Enter abrir/elegir · Backspace subir · . ocultos · ~ home · Esc volver")
            .style(Style::new().fg(MUTED)),
        Rect::new(inner.x + 1, inner.y + inner.height - 1, inner.width.saturating_sub(2), 1),
    );
    Rect::new(list.x, list.y, list.width, list.height)
}
