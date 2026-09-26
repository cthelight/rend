//! Rendering: the drives panel, table of contents, rip progress, the
//! keybinds help window, the tags editor, and the status bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Cell, Clear, Gauge, Paragraph, Row, Table, Wrap};

use rend_core::Toc;

use crate::app::{App, Drive, EditField, Focus, Hover, MetaEdit, Regions, TrackState};

/// The width of the vertical drives panel on the left.
const DRIVES_WIDTH: u16 = 38;
/// The lines a drives panel entry occupies: the name, then the album.
const DRIVE_ENTRY_LINES: u16 = 2;
/// The TOC table's fixed-width columns: `#`, start, length, and state.
const TOC_FIXED_WIDTH: u16 = 4 + 8 + 6 + 18;
/// The width of the per-track progress bar in the state column.
const STATE_BAR: usize = 10;

/// Draws the whole screen and updates the mouse hit-test regions.
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Min(4),
        Constraint::Length(5),
        Constraint::Length(1),
    ])
    .split(area);
    // Drives on the left, the selected disc's TOC and metadata on the right.
    let main = Layout::horizontal([Constraint::Length(DRIVES_WIDTH), Constraint::Min(10)])
        .split(chunks[0]);

    let mut regions = Regions::default();
    draw_drives(f, app, main[0], &mut regions);
    draw_toc(f, app, main[1], &mut regions);
    draw_rip(f, app, chunks[1], &mut regions);
    draw_status(f, app, chunks[2]);
    app.regions = regions;
    if app.help {
        draw_help(f);
    }
    if app.url_modal {
        if let Some(url) = app.disc_register_url() {
            draw_register_url(f, &url);
        }
    }
    let summary = app.match_status();
    if let Some(edit) = app.editing.as_mut() {
        draw_meta_edit(f, summary.as_deref().unwrap_or(""), edit);
    }
}

fn panel_block(title: String, focused: bool) -> Block<'static> {
    let style = if focused {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(style)
        .title(Span::styled(title, style))
}

/// A message centered in a bordered panel's inner area.
fn draw_centered(f: &mut Frame, block: Block<'static>, area: Rect, msg: &str) {
    let inner = area.inner(Margin::new(1, 1));
    let width = inner.width as usize;
    let msg: String = msg.chars().take(width).collect();
    let pad = width.saturating_sub(msg.chars().count()).div_ceil(2);
    let row = inner.y + inner.height / 2;
    f.render_widget(Paragraph::new("").block(block), area);
    f.render_widget(
        Paragraph::new(format!("{}{}", " ".repeat(pad), msg))
            .style(Style::default().fg(Color::DarkGray)),
        Rect::new(inner.x, row, inner.width, 1),
    );
}

fn draw_drives(f: &mut Frame, app: &mut App, area: Rect, regions: &mut Regions) {
    regions.drives = area;
    let block = panel_block(" Drives ".into(), app.focus == Focus::Drives);

    if app.drives.is_empty() {
        draw_centered(f, block, area, "no CD-ROM drives found — try --demo");
        return;
    }

    f.render_widget(Paragraph::new("").block(block), area);
    let inner = area.inner(Margin::new(1, 1));
    let visible = app.drives_visible(area);
    let start = clamp_scroll(app.drives_scroll, app.drive_sel, app.drives.len(), visible);
    app.drives_scroll = start;

    for (row, (i, d)) in app
        .drives
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .enumerate()
    {
        let top = inner.y + row as u16 * DRIVE_ENTRY_LINES;
        let name = Rect::new(inner.x, top, inner.width, 1);
        let info = Rect::new(inner.x, top + 1, inner.width, 1);
        let selected = i == app.drive_sel;
        let hovered = app.hover == Hover::Drive(i);

        if app.drive_ripping(i) {
            // A rip in progress turns the drive's name line into the rip's
            // progress bar, which fills over the name, keeping it readable.
            let (done, total) = app.rip_progress(i);
            let ratio = if total > 0 {
                (done as f64 / total as f64).clamp(0.0, 1.0)
            } else {
                0.0
            };
            f.render_widget(
                Paragraph::new(rip_name_line(d, selected, ratio, inner.width as usize)),
                name,
            );
        } else {
            f.render_widget(
                Paragraph::new(drive_name_line(d, selected, hovered, inner.width as usize)),
                name,
            );
        }

        // Under the name: the disc's album once matched, else the drive's
        // status.
        let (text, color) = match &d.album {
            Some(album) => (
                format!("  {}", truncate_chars(album, (inner.width - 2) as usize)),
                Color::Gray,
            ),
            None => (format!("  {}", d.status), status_color(&d.status)),
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color)))),
            info,
        );
    }
}

/// The drive's name line: the selection marker, its path, and its label,
/// which stays dim. A selected entry is reversed across the full row; a
/// hovered one is bold.
fn drive_name_line(d: &Drive, selected: bool, hovered: bool, width: usize) -> Line<'static> {
    let marker = if selected { "●" } else { "○" };
    let name = format!("{marker} {}", d.path);
    let mut label = format!("  {}", d.label);
    if label.chars().count() > width.saturating_sub(name.chars().count()) {
        let keep = width.saturating_sub(name.chars().count()).saturating_sub(1);
        let cut: String = label.chars().take(keep).collect();
        label = format!("{cut}…");
    }
    let base = if selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if hovered {
        Style::default().add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };
    Line::from(vec![
        Span::raw(name),
        Span::styled(label, Style::default().fg(Color::DarkGray)),
    ])
    .style(base)
}

/// The drive's name line while its rip is running: the name, centered, with
/// the progress bar filling over it. The characters the fill passes under
/// take the bar's color as their background and a black foreground, so the
/// name stays readable and the bar stays visible.
fn rip_name_line(d: &Drive, selected: bool, ratio: f64, width: usize) -> Line<'static> {
    let marker = if selected { "●" } else { "○" };
    let name: String = format!("{marker} {} {}", d.path, d.label)
        .chars()
        .take(width)
        .collect();
    let bar = if selected { Color::Cyan } else { Color::Gray };
    let filled = (ratio.clamp(0.0, 1.0) * width as f64).round() as usize;
    let start = (width.saturating_sub(name.chars().count())) / 2;
    let mut cells = vec![None; width];
    for (i, ch) in name.chars().enumerate() {
        cells[start + i] = Some(ch);
    }
    let spans: Vec<Span> = cells
        .iter()
        .enumerate()
        .map(|(x, ch)| match ch {
            Some(ch) if x < filled => {
                Span::styled(ch.to_string(), Style::default().fg(Color::Black).bg(bar))
            }
            Some(ch) => Span::raw(ch.to_string()),
            None if x < filled => Span::styled("█", Style::default().fg(bar)),
            None => Span::styled("░", Style::default().fg(Color::DarkGray)),
        })
        .collect();
    Line::from(spans)
}

fn draw_toc(f: &mut Frame, app: &mut App, area: Rect, regions: &mut Regions) {
    regions.tracks = area;
    let path = app
        .drives
        .get(app.drive_sel)
        .map(|d| d.path.as_str())
        .unwrap_or("?");
    // The album, once matched, headlines the panel; until then the drive's
    // path does.
    let title = match app.toc.as_ref() {
        Some(_) => app
            .album_summary()
            .map(|album| format!(" {album} "))
            .unwrap_or_else(|| format!(" TOC — {path} ")),
        None => {
            let state = if app.loading_toc {
                "reading…"
            } else {
                "no disc"
            };
            if app.drives.is_empty() {
                format!(" TOC — {state} ")
            } else {
                format!(" TOC — {path} ({state}) ")
            }
        }
    };
    let block = panel_block(title, app.focus == Focus::Tracks);

    let Some(toc) = app.toc.as_ref() else {
        let msg = if app.drives.is_empty() {
            "no drive selected — try rend-tui --demo"
        } else if app.loading_toc {
            "reading the table of contents…"
        } else {
            "no disc in the selected drive"
        };
        draw_centered(f, block, area, msg);
        return;
    };

    f.render_widget(Paragraph::new("").block(block), area);
    let inner = area.inner(Margin::new(1, 1));
    let slices = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).split(inner);
    f.render_widget(
        Paragraph::new(toc_subheader(app, toc, inner.width as usize)),
        slices[0],
    );

    let visible = app.tracks_visible(area);
    let start = clamp_scroll(app.tracks_scroll, app.track_sel, toc.tracks.len(), visible);
    app.tracks_scroll = start;

    let titles: std::collections::HashMap<u8, &str> = match app.selected_meta() {
        Some(meta) => toc
            .audio_tracks()
            .enumerate()
            .filter_map(|(i, t)| meta.track(i + 1).map(|tm| (t.number, tm.title.as_str())))
            .collect(),
        None => std::collections::HashMap::new(),
    };

    let header = Row::new(vec!["#", "start", "length", "title", "state"]).style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let title_width = inner.width.saturating_sub(TOC_FIXED_WIDTH) as usize;
    let table_rows: Vec<Row> = toc
        .tracks
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, t)| {
            let selected = i == app.track_sel;
            let base = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if app.hover == Hover::Track(i) {
                Style::default().add_modifier(Modifier::BOLD)
            } else if !t.is_audio() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            };
            let marker = if selected { "▶" } else { " " };
            let title = if t.is_audio() {
                titles.get(&t.number).copied().unwrap_or("—").to_string()
            } else {
                "data track".to_string()
            };
            let (text, color) =
                state_cell(app.selected_rip_state().and_then(|s| s.states.get(i)), 18);
            Row::new(vec![
                Cell::from(format!("{marker}{:>2}", t.number)),
                Cell::from(
                    t.start_msf()
                        .map_or_else(|| "?".to_string(), |m| m.to_string()),
                ),
                Cell::from(fmt_duration(
                    t.frames(toc.end_lba(t.number).unwrap_or(toc.leadout_lba)),
                )),
                Cell::from(truncate_chars(&title, title_width.max(4))),
                Cell::from(Span::styled(text, Style::default().fg(color))),
            ])
            .style(base)
        })
        .collect();

    // The fixed columns keep the state cell readable; the title takes the
    // remaining width.
    let table = Table::new(
        table_rows,
        [
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Length(6),
            Constraint::Min(10),
            Constraint::Length(18),
        ],
    )
    .header(header);
    f.render_widget(table, slices[1]);
}

/// The line under the TOC panel's border: the disc id, track count, and total
/// length on the left; the metadata match state on the right. The match state
/// always keeps its place — the disc facts are what give way when the panel is
/// narrow.
fn toc_subheader(app: &App, toc: &Toc, width: usize) -> Line<'static> {
    let total: u32 = toc
        .tracks
        .iter()
        .filter(|t| t.is_audio())
        .map(|t| t.frames(toc.end_lba(t.number).unwrap_or(toc.leadout_lba)))
        .sum();
    let disc = app
        .disc_id
        .as_deref()
        .map(|id| format!("disc id {id}  "))
        .unwrap_or_default();
    let left = format!(
        "{disc}{} tracks · {}",
        toc.tracks.len(),
        fmt_duration(total)
    );
    let right = app.toc_match_right();
    let budget = if right.is_empty() {
        width
    } else {
        width.saturating_sub(right.chars().count() + 1)
    };
    let left_len = left.chars().count();
    let left: String = if left_len > budget {
        if budget == 0 {
            String::new()
        } else {
            let mut s: String = left.chars().take(budget - 1).collect();
            s.push('…');
            s
        }
    } else {
        left
    };
    let gap = width.saturating_sub(left.chars().count());
    let right: String = right.chars().take(gap.saturating_sub(1)).collect();
    Line::from(vec![
        Span::styled(
            format!(
                "{left}{}",
                " ".repeat(gap.saturating_sub(right.chars().count()))
            ),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(right, Style::default().fg(Color::Gray)),
    ])
}

fn draw_rip(f: &mut Frame, app: &mut App, area: Rect, regions: &mut Regions) {
    let rip = app.selected_rip_state();
    let active = rip.is_some_and(|s| s.active);
    let out_display = app
        .selected_rip_out_dir()
        .unwrap_or(app.out_dir.as_path())
        .display()
        .to_string();
    let mut title = format!(" Ripping to {out_display} · {}", app.format.label());
    if app.force {
        title.push_str(" · force on");
    }
    title.push(' ');
    // The panel glows while its drive is actually ripping.
    let block = panel_block(title, active);
    f.render_widget(Paragraph::new("").block(block), area);
    let inner = area.inner(Margin::new(1, 1));
    let slices = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .split(inner);

    let (done, total) = app.rip_totals();
    let ratio = if total > 0 {
        (done as f64 / total as f64).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let pct = done * 100 / total.max(1);
    let label = if active {
        match current_track(rip, app) {
            Some(n) => {
                let count = app.rip_track_count().unwrap_or(0);
                let position = if count > 1 {
                    format!("track {n:02}/{count:02}")
                } else {
                    format!("track {n:02}")
                };
                let name = track_title(app, n)
                    .map(|t| format!(" · {}", truncate_chars(&t, 24)))
                    .unwrap_or_default();
                // `{pct}` is the overall progress across the whole rip.
                format!("{position}{name}  {pct}%")
            }
            None => "starting…".to_string(),
        }
    } else if let Some(summary) = rip.and_then(|s| s.summary.as_ref()) {
        summary.clone()
    } else {
        "idle".to_string()
    };
    let gauge_color = if active { Color::Cyan } else { Color::DarkGray };
    f.render_widget(
        Gauge::default()
            .ratio(ratio)
            .label(label)
            .gauge_style(Style::default().fg(gauge_color)),
        slices[0],
    );

    let speed = rip.map(|s| s.speed).unwrap_or(0.0);
    let stats = if active {
        let eta = if speed > 0.0 {
            total.saturating_sub(done) as f64 / speed
        } else {
            f64::INFINITY
        };
        format!(
            "  {}   ETA {}   {}/{}",
            fmt_rate(speed),
            fmt_eta(eta),
            fmt_bytes(done),
            fmt_bytes(total),
        )
    } else {
        "  select a track to rip it, or rip all the audio tracks".to_string()
    };
    f.render_widget(
        Paragraph::new(stats).style(Style::default().fg(Color::Gray)),
        slices[1],
    );

    // The buttons are clickable, so they carry no keybind hints; the keys
    // still work and are listed in the help window (`?`).
    let can_rip = app.toc.is_some() && !active;
    let can_eject = app.drives.get(app.drive_sel).is_some_and(|d| !d.is_demo()) && !active;
    let buttons: [(&str, bool, Hover); 5] = [
        ("rip selected", can_rip, Hover::RipSelected),
        ("rip all", can_rip, Hover::RipAll),
        ("tags", can_rip, Hover::EditTags),
        ("eject", can_eject, Hover::Eject),
        ("stop", active, Hover::Stop),
    ];
    // Each button occupies its text plus the brackets and padding; the row
    // is centered as a whole.
    let slots: Vec<u16> = buttons
        .iter()
        .map(|(label, _, _)| label.len() as u16 + 6)
        .collect();
    let row_width: u16 = slots.iter().sum::<u16>() + 2 * (buttons.len() - 1) as u16;
    let mut x = slices[2].x + (slices[2].width.saturating_sub(row_width)) / 2;
    for ((label, enabled, hover), slot) in buttons.iter().zip(slots.iter()) {
        let rect = Rect::new(x, slices[2].y, *slot, 1);
        draw_button(f, rect, label, *enabled, app.hover == *hover);
        match hover {
            Hover::RipSelected => regions.rip_selected = rect,
            Hover::RipAll => regions.rip_all = rect,
            Hover::EditTags => regions.edit_tags = rect,
            Hover::Eject => regions.eject = rect,
            Hover::Stop => regions.stop = rect,
            _ => {}
        }
        x += slot + 2;
    }
}

fn draw_status(f: &mut Frame, app: &mut App, area: Rect) {
    // The left carries the selected drive and its state; the right carries
    // the current status message, or a nudge to the help window. The keybind
    // hints live in the help window (`?`).
    let left = match app.drives.get(app.drive_sel) {
        Some(d) => format!(" {} · {} ", d.path, d.status),
        None => " no drives ".to_string(),
    };
    let (right, right_color) = match &app.status {
        Some(status) => (format!(" {status} "), Color::Yellow),
        None => (" ? for keybinds ".to_string(), Color::DarkGray),
    };

    let width = area.width as usize;
    let left_len = left.chars().count();
    let room = width.saturating_sub(left_len + 1);
    let right: String = right.chars().take(room).collect();
    let pad = " ".repeat(room.saturating_sub(right.chars().count()));
    let line = Line::from(vec![
        Span::styled(left, Style::default().fg(Color::DarkGray)),
        Span::raw(pad),
        Span::styled(right, Style::default().fg(right_color)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// The keybinds reference, opened and closed with `?`.
fn draw_help(f: &mut Frame) {
    let area = f.area();
    let width = 54u16.min(area.width.saturating_sub(2));
    let height = 18u16.min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Keybinds · press ? or esc to close ");
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new("").block(block), rect);

    let rows = [
        ("↑/↓  j/k", "move selection"),
        ("tab", "switch panel focus"),
        ("enter", "load disc / rip selected track"),
        ("r", "rip the selected track"),
        ("a", "rip all audio tracks"),
        ("m", "switch metadata match"),
        ("t", "edit the disc's tags"),
        ("u", "show the register disc id URL"),
        ("e", "eject the disc"),
        ("o", "cycle the output format"),
        ("f", "toggle force (overwrite)"),
        ("s", "stop the selected rip"),
        ("?", "show or close this help"),
        ("q  esc", "quit"),
    ];
    let lines: Vec<Line> = rows
        .into_iter()
        .map(|(key, what)| {
            Line::from(vec![
                Span::styled(
                    format!("{key:<10}"),
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!(" {what}")),
            ])
        })
        .chain(std::iter::once(Line::from(Span::raw(""))))
        .chain(std::iter::once(Line::from(Span::styled(
            "click selects · double-click rips · wheel scrolls",
            Style::default().fg(Color::DarkGray),
        ))))
        .collect();
    f.render_widget(Paragraph::new(lines), rect.inner(Margin::new(1, 1)));
}

/// One action button, sized to its slot. The brackets and the colors mark it
/// as clickable rather than plain text: bold cyan brackets when enabled, the
/// whole button inverted on hover, and a flat dark gray when disabled.
fn draw_button(f: &mut Frame, rect: Rect, label: &str, enabled: bool, hovered: bool) {
    let text = format!(" [ {label} ]");
    let pad = " ".repeat((rect.width as usize).saturating_sub(text.chars().count() + 1));
    let spans: Vec<Span> = if enabled && hovered {
        vec![Span::styled(
            format!("{text} {pad}"),
            Style::default().add_modifier(Modifier::REVERSED),
        )]
    } else {
        let bracket = if enabled {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        };
        let body = if enabled {
            Style::default()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        vec![
            Span::styled(" [ ", bracket),
            Span::styled(format!("{label} "), body),
            Span::styled("]", bracket),
            Span::raw(pad),
        ]
    };
    f.render_widget(Paragraph::new(Line::from(spans)), rect);
}

/// The centered modal showing the URL for registering the disc's layout as
/// a MusicBrainz disc id.
fn draw_register_url(f: &mut Frame, url: &str) {
    let area = f.area();
    let width = 90u16.min(area.width.saturating_sub(2));
    let height = 10u16.min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Register disc id · press u or esc to close ");
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new("").block(block), rect);
    let rows = Layout::vertical([Constraint::Length(1), Constraint::Min(1)])
        .split(rect.inner(Margin::new(1, 1)));
    f.render_widget(
        Paragraph::new("Open this URL in a browser to register the disc's layout on MusicBrainz.")
            .style(Style::default().fg(Color::DarkGray)),
        rows[0],
    );
    f.render_widget(
        Paragraph::new(url.to_string())
            .wrap(Wrap { trim: false })
            .style(Style::default().fg(Color::Cyan)),
        rows[1],
    );
}

/// The centered modal for editing the disc's tags.
fn draw_meta_edit(f: &mut Frame, summary: &str, edit: &mut MetaEdit) {
    let area = f.area();
    let width = 54u16.min(area.width.saturating_sub(2));
    let height = 14u16.min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Edit tags ");
    f.render_widget(Clear, rect);
    f.render_widget(Paragraph::new("").block(block), rect);
    let inner = rect.inner(Margin::new(1, 1));
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(1),
        Constraint::Length(1),
    ])
    .split(inner);

    f.render_widget(
        Paragraph::new(summary.to_string()).style(Style::default().fg(Color::DarkGray)),
        rows[0],
    );

    let visible = rows[1].height as usize;
    let start = clamp_scroll(edit.scroll, edit.sel, edit.fields.len(), visible);
    edit.scroll = start;
    let lines: Vec<Line> = edit
        .fields
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, field)| field_line(field, i == edit.sel))
        .collect();
    f.render_widget(Paragraph::new(lines), rows[1]);

    f.render_widget(
        Paragraph::new(" enter save · esc cancel · ↑↓/tab fields · ^c cancel")
            .style(Style::default().fg(Color::DarkGray)),
        rows[2],
    );
}

/// One editor line: the label and the value, the focused line highlighted,
/// the cursor as the reversed character under it.
fn field_line(field: &EditField, selected: bool) -> Line<'static> {
    let before = field.value[..field.cursor].to_string();
    let after = &field.value[field.cursor..];
    let cursor_char = after.chars().next().unwrap_or(' ');
    let rest: String = after.chars().skip(1).collect();
    let label = Span::styled(
        format!("{:<12} ", field.label),
        if selected {
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::DarkGray)
        },
    );
    let value = if selected {
        vec![
            Span::raw(before),
            Span::styled(
                cursor_char.to_string(),
                Style::default().add_modifier(Modifier::REVERSED),
            ),
            Span::raw(rest),
        ]
    } else {
        vec![Span::raw(format!("{before}{cursor_char}{rest}"))]
    };
    let mut spans = vec![label];
    spans.extend(value);
    Line::from(spans)
}

fn current_track(rip: Option<&crate::app::RipState>, app: &App) -> Option<u8> {
    let idx = rip?
        .states
        .iter()
        .position(|s| matches!(s, TrackState::Ripping { .. }))?;
    app.toc.as_ref()?.tracks.get(idx).map(|t| t.number)
}

/// The looked-up title of the track with the given number, if any.
fn track_title(app: &App, number: u8) -> Option<String> {
    let toc = app.toc.as_ref()?;
    let ordinal = toc.audio_tracks().position(|t| t.number == number)?;
    app.selected_meta()?
        .track(ordinal + 1)
        .map(|t| t.title.clone())
}

/// The per-track state cell: a dash when idle, a small progress bar while
/// the track is being read, and the outcome once it has finished.
fn state_cell(state: Option<&TrackState>, width: usize) -> (String, Color) {
    match state {
        None | Some(TrackState::Idle) => ("—".to_string(), Color::DarkGray),
        Some(TrackState::Ripping {
            bytes_done,
            bytes_total,
        }) => {
            let pct = *bytes_done * 100 / (*bytes_total).max(1);
            (
                format!(
                    "{} {pct:3}%",
                    state_bar(*bytes_done, *bytes_total, STATE_BAR)
                ),
                Color::Cyan,
            )
        }
        Some(TrackState::Done { bytes }) => (format!("✓ done {}", fmt_bytes(*bytes)), Color::Green),
        Some(TrackState::Failed(e)) => (
            format!("✗ {}", truncate_chars(e, width.saturating_sub(2))),
            Color::Red,
        ),
        Some(TrackState::Skipped(r)) => (
            format!("↷ {}", truncate_chars(r, width.saturating_sub(2))),
            Color::DarkGray,
        ),
    }
}

/// A small `█░` progress bar of the given width.
fn state_bar(done: u64, total: u64, width: usize) -> String {
    let filled = if total > 0 {
        ((done as f64 / total as f64) * width as f64).round() as usize
    } else {
        0
    }
    .min(width);
    format!("{}{}", "█".repeat(filled), "░".repeat(width - filled))
}

/// Truncates a string to at most `width` characters, marking the cut with
/// an ellipsis.
fn truncate_chars(s: &str, width: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if width == 0 || chars.len() <= width {
        return s.to_string();
    }
    let mut out: String = chars[..width - 1].iter().collect();
    out.push('…');
    out
}

fn status_color(status: &str) -> Color {
    match status {
        "disc present" => Color::Green,
        "no disc" | "tray open" => Color::Yellow,
        "not ready" => Color::Red,
        _ => Color::Gray,
    }
}

fn clamp_scroll(scroll: usize, sel: usize, count: usize, visible: usize) -> usize {
    if visible == 0 || count == 0 {
        return 0;
    }
    if sel < scroll {
        sel
    } else if sel >= scroll + visible {
        sel + 1 - visible
    } else {
        scroll
    }
}

/// Formats a frame count as `m:ss`.
pub fn fmt_duration(frames: u32) -> String {
    let secs = frames / rend_core::FRAMES_PER_SECOND;
    format!("{:1}:{:02}", secs / 60, secs % 60)
}

/// Formats a byte count with binary units.
pub fn fmt_bytes(n: u64) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    let n = n as f64;
    if n < KB {
        format!("{n:.0} B")
    } else if n < MB {
        format!("{:.1} KB", n / KB)
    } else if n < GB {
        format!("{:.1} MB", n / MB)
    } else {
        format!("{:.2} GB", n / GB)
    }
}

/// Formats a transfer rate in bytes/second.
pub fn fmt_rate(bps: f64) -> String {
    format!("{}/s", fmt_bytes(bps as u64))
}

/// Formats a duration in seconds as `m:ss`.
pub fn fmt_eta(secs: f64) -> String {
    if !secs.is_finite() || secs <= 0.0 {
        return "—".to_string();
    }
    let s = secs as u64;
    format!("{:1}:{:02}", s / 60, s % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use rend_core::TrackType;
    use rend_encode::Format;
    use rend_meta::Template;

    /// Renders the app once and returns the whole screen as one string.
    fn rendered(app: &mut App) -> String {
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect()
    }

    /// Renders the app once and returns the screen row by row.
    fn rendered_lines(app: &mut App) -> Vec<String> {
        let width = 100usize;
        let height = 30usize;
        let backend = TestBackend::new(width as u16, height as u16);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| draw(f, app)).unwrap();
        let cells = terminal.backend().buffer().content();
        (0..height)
            .map(|y| {
                cells[y * width..(y + 1) * width]
                    .iter()
                    .map(|c| c.symbol())
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect()
    }

    /// A demo-mode app with a simulated disc loaded.
    fn demo_app(dir: &std::path::Path) -> App {
        App::new(
            None,
            dir.to_path_buf(),
            false,
            Format::Flac,
            Template::default(),
            true,
        )
    }

    #[test]
    fn draws_the_tags_editor_modal() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(
            None,
            dir.path().to_path_buf(),
            false,
            Format::Flac,
            Template::default(),
            true,
        );

        // Without the editor, no modal is drawn.
        let plain = rendered(&mut app);
        assert!(!plain.contains("Edit tags"));

        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        let with_editor = rendered(&mut app);
        assert!(with_editor.contains("Edit tags"));
        // The summary line and the focused field are visible.
        assert!(with_editor.contains("The Demo Band"));
        assert!(with_editor.contains("Demo Album"));
        assert!(with_editor.contains("enter save"));
    }

    #[test]
    fn drives_panel_shows_the_album_below_the_drive_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        let lines = rendered_lines(&mut app);
        // The drive's name line, in the left panel only.
        let name_row = (0..30)
            .find(|y| {
                lines[*y]
                    .chars()
                    .take(DRIVES_WIDTH as usize)
                    .collect::<String>()
                    .contains("/dev/sr-demo")
            })
            .expect("the drive name line");
        // The album sits directly under the name.
        let album_row: String = lines[name_row + 1]
            .chars()
            .take(DRIVES_WIDTH as usize)
            .collect();
        assert!(
            album_row.contains("The Demo Band — Demo Album (2024)"),
            "unexpected album line: {album_row:?}"
        );
    }

    #[test]
    fn u_shows_the_register_disc_id_url() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        let plain = rendered(&mut app);
        assert!(!plain.contains("Register disc id"));

        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::NONE));
        let with_url = rendered(&mut app);
        assert!(with_url.contains("Register disc id"));
        assert!(
            with_url.contains(
                "https://musicbrainz.org/cdtoc/attach?toc=1+5+10500+0+1500+3750+3900+7500"
            )
        );

        // While the URL window is open, other keys do nothing.
        app.handle_key(KeyEvent::new(KeyCode::Char('t'), KeyModifiers::NONE));
        assert!(app.editing.is_none());

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let closed = rendered(&mut app);
        assert!(!closed.contains("Register disc id"));
        assert!(app.running);
    }

    #[test]
    fn help_window_lists_the_keybinds() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        let plain = rendered(&mut app);
        assert!(!plain.contains("Keybinds"));

        app.handle_key(KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE));
        let with_help = rendered(&mut app);
        assert!(with_help.contains("Keybinds"));
        assert!(with_help.contains("rip all audio tracks"));
        assert!(with_help.contains("double-click rips"));

        app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
        let closed = rendered(&mut app);
        assert!(!closed.contains("Keybinds"));
        assert!(app.running);
    }

    #[test]
    fn a_ripping_drive_turns_its_name_into_a_progress_bar() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        app.track_sel = 2;
        app.rip_selected_track();
        assert!(app.drive_ripping(app.drive_sel));
        app.drives[0].label = "CD".into();

        let lines = rendered_lines(&mut app);
        let name_row = (0..30)
            .find(|y| {
                lines[*y]
                    .chars()
                    .take(DRIVES_WIDTH as usize)
                    .collect::<String>()
                    .contains("/dev/sr-demo")
            })
            .expect("the drive name line");
        let line: String = lines[name_row]
            .chars()
            .take(DRIVES_WIDTH as usize)
            .collect();
        // The name is still visible, with the progress bar filling over it.
        assert!(line.contains("/dev/sr-demo"));
        assert!(
            line.contains('█') || line.contains('░'),
            "no bar in: {line:?}"
        );
    }

    #[test]
    fn the_progress_bar_fills_over_the_drive_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());
        app.drives[0].label = "CD".into();
        let drive = &app.drives[0];
        let width = 20usize;
        let line = rip_name_line(drive, false, 0.5, width);
        let spans: Vec<&Span> = line.spans.iter().collect();
        assert_eq!(spans.len(), width);
        let filled = (0.5 * width as f64).round() as usize;
        for (x, span) in spans.iter().enumerate() {
            let over_fill = x < filled;
            let content = span.content.as_ref();
            if content == "█" || content == "░" {
                // Bar blocks only appear where the fill is.
                assert_eq!(content == "█", over_fill, "column {x}");
                continue;
            }
            // A name character: the fill shows through as its background…
            assert_eq!(
                span.style.bg,
                over_fill.then_some(Color::Gray),
                "column {x}"
            );
            // …and black keeps it readable over the bar.
            assert_eq!(
                span.style.fg,
                over_fill.then_some(Color::Black),
                "column {x}"
            );
        }
        // The name spans both sides of the fill, so the bar is visible in
        // the empty cells of the filled half and the unfilled half.
        assert!(spans.iter().take(filled).any(|s| s.content.as_ref() == "█"));
        assert!(spans.iter().skip(filled).any(|s| s.content.as_ref() == "░"));
    }

    #[test]
    fn status_bar_hints_point_at_the_help_window() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        app.status = None;
        let plain = rendered(&mut app);
        assert!(plain.contains("? for keybinds"));

        app.status = Some("something happened".into());
        let with_status = rendered(&mut app);
        assert!(!with_status.contains("? for keybinds"));
        assert!(with_status.contains("something happened"));
    }

    #[test]
    fn rip_panel_buttons_carry_no_keybind_hints() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = demo_app(dir.path());

        let plain = rendered(&mut app);
        // The action row reads like buttons, not keybind hints.
        assert!(plain.contains("[ rip selected ]"));
        assert!(plain.contains("[ rip all ]"));
        assert!(plain.contains("[ stop ]"));
        assert!(!plain.contains("[r]"));
        assert!(!plain.contains("[a]"));
        assert!(!plain.contains("press r"));
    }

    #[test]
    fn bytes() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(512), "512 B");
        assert_eq!(fmt_bytes(1024), "1.0 KB");
        assert_eq!(fmt_bytes((150 * rend_core::FRAME_SIZE) as u64), "344.5 KB");
        assert_eq!(fmt_bytes((1500 * rend_core::FRAME_SIZE) as u64), "3.4 MB");
    }

    #[test]
    fn rate_and_eta() {
        assert_eq!(fmt_rate(1_500_000.0), "1.4 MB/s");
        assert_eq!(fmt_eta(f64::INFINITY), "—");
        assert_eq!(fmt_eta(0.0), "—");
        assert_eq!(fmt_eta(75.0), "1:15");
        assert_eq!(fmt_eta(59.9), "0:59");
    }

    #[test]
    fn duration() {
        assert_eq!(fmt_duration(0), "0:00");
        assert_eq!(fmt_duration(75 * 75), "1:15");
        assert_eq!(fmt_duration(75 * 300), "5:00");
    }

    #[test]
    fn clamp() {
        assert_eq!(clamp_scroll(0, 0, 5, 3), 0);
        assert_eq!(clamp_scroll(0, 4, 5, 3), 2);
        assert_eq!(clamp_scroll(2, 1, 5, 3), 1);
        assert_eq!(clamp_scroll(1, 1, 0, 3), 0);
        assert_eq!(clamp_scroll(1, 1, 5, 0), 0);
    }

    #[test]
    fn track_type_cell() {
        let track = rend_core::Track {
            number: 1,
            kind: TrackType::Audio,
            start_lba: 0,
        };
        assert_eq!(track.kind.to_string(), "audio");
        let data = rend_core::Track {
            number: 2,
            kind: TrackType::Data,
            start_lba: 100,
        };
        assert_eq!(data.kind.to_string(), "data");
    }
}
