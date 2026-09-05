//! Rendering: the drives panel, table of contents, rip progress, the
//! keybinds help window, the tags editor, and the status bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Gauge, Paragraph, Row, Table};

use crate::app::{App, Drive, EditField, Focus, Hover, MetaEdit, Regions, TrackState};

/// The width of the vertical drives panel on the left.
const DRIVES_WIDTH: u16 = 40;
/// The lines a drives panel entry occupies: the name, then the album.
const DRIVE_ENTRY_LINES: u16 = 2;

/// Draws the whole screen and updates the mouse hit-test regions.
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let chunks = Layout::vertical([
        Constraint::Min(4),
        Constraint::Length(6),
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
    let summary = app.match_status();
    if let Some(edit) = app.editing.as_mut() {
        draw_meta_edit(f, summary.as_deref().unwrap_or(""), edit);
    }
}

fn panel_block(title: String, focused: bool) -> Block<'static> {
    let border = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::bordered().border_style(border).title(title)
}

fn draw_drives(f: &mut Frame, app: &mut App, area: Rect, regions: &mut Regions) {
    regions.drives = area;
    let block = panel_block(" Drives ".into(), app.focus == Focus::Drives);

    if app.drives.is_empty() {
        f.render_widget(
            Paragraph::new("no CD-ROM devices found — try rend-tui --demo")
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    }

    f.render_widget(Paragraph::new("").block(block), area);
    let inner = area.inner(Margin::new(1, 1));
    let visible = (inner.height as usize) / DRIVE_ENTRY_LINES as usize;
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
        let label = drive_name(d, selected, hovered, inner.width as usize);

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
            let style = if selected {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if hovered {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            f.render_widget(Paragraph::new(label).style(style), name);
        }

        // Under the name: the disc's album once matched, else the drive's
        // status.
        let (text, color) = match &d.album {
            Some(album) => (format!(" {album}"), Color::Gray),
            None => (format!(" {}", d.status), status_color(&d.status)),
        };
        f.render_widget(
            Paragraph::new(Line::from(Span::styled(text, Style::default().fg(color)))),
            info,
        );
    }
}

/// The drive's name line: the selection marker, its path, and its label,
/// padded to the full width so a selected (reversed) entry spans the row.
fn drive_name(d: &Drive, selected: bool, hovered: bool, width: usize) -> String {
    let marker = if selected { "●" } else { "○" };
    let name = format!("{marker} {} {}", d.path, d.label);
    let name: String = name.chars().take(width).collect();
    let pad = width.saturating_sub(name.chars().count());
    if pad > 0 && (selected || hovered) {
        format!("{name}{}", " ".repeat(pad))
    } else {
        name
    }
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
    let meta = app.match_status().map(|s| format!(" · {s}"));
    let title = match app.toc.as_ref() {
        Some(_) => {
            let id = app
                .disc_id
                .as_deref()
                .map(|s| format!(" · disc id {s}"))
                .unwrap_or_default();
            format!(" TOC — {path}{id}{} ", meta.unwrap_or_default())
        }
        None => {
            let state = if app.loading_toc {
                "loading…"
            } else {
                "no disc"
            };
            format!(" TOC — {path} ({state}) ")
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
        f.render_widget(
            Paragraph::new(msg)
                .style(Style::default().fg(Color::DarkGray))
                .block(block),
            area,
        );
        return;
    };

    let visible = app.visible(area);
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

    let header = Row::new(vec!["#", "type", "start", "length", "title", "state"]).style(
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );

    let rows: Vec<Row> = toc
        .tracks
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, t)| {
            let style = if i == app.track_sel {
                Style::default().add_modifier(Modifier::REVERSED)
            } else if app.hover == Hover::Track(i) {
                Style::default().add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let marker = if i == app.track_sel { "▶" } else { " " };
            let (text, color) = state_cell(app.selected_rip_state().and_then(|s| s.states.get(i)));
            Row::new(vec![
                Cell::from(format!("{marker}{:>2}", t.number)),
                Cell::from(t.kind.to_string()),
                Cell::from(
                    t.start_msf()
                        .map_or_else(|| "?".to_string(), |m| m.to_string()),
                ),
                Cell::from(fmt_duration(
                    t.frames(toc.end_lba(t.number).unwrap_or(toc.leadout_lba)),
                )),
                Cell::from(titles.get(&t.number).copied().unwrap_or("—")),
                Cell::from(Span::styled(text, Style::default().fg(color))),
            ])
            .style(style)
        })
        .collect();

    // The panel now shares the width with the drives panel, so the title
    // takes the slack and the rip state column stays readable.
    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(7),
            Constraint::Length(9),
            Constraint::Length(7),
            Constraint::Min(8),
            Constraint::Length(18),
        ],
    )
    .header(header)
    .block(block);
    f.render_widget(table, area);
}

fn draw_rip(f: &mut Frame, app: &mut App, area: Rect, regions: &mut Regions) {
    let rip = app.selected_rip_state();
    let active = rip.is_some_and(|s| s.active);
    let force = if app.force { "force on" } else { "force off" };
    let out_display = app
        .selected_rip_out_dir()
        .unwrap_or(app.out_dir.as_path())
        .display()
        .to_string();
    let title = format!(
        " Ripping to {} · {} · {force} ",
        out_display,
        app.format.label()
    );
    let block = Block::bordered()
        .border_style(Style::default().fg(Color::DarkGray))
        .title(title);
    f.render_widget(Paragraph::new("").block(block), area);
    let inner = area.inner(Margin::new(1, 1));
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(1),
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
                if count > 1 {
                    // `{pct}` is the overall progress across the whole rip.
                    format!("track {n:02}/{count:02}  {pct}%")
                } else {
                    format!("track {n:02}  {pct}%")
                }
            }
            None => "starting…".to_string(),
        }
    } else if let Some(summary) = rip.and_then(|s| s.summary.as_ref()) {
        summary.clone()
    } else {
        "idle".to_string()
    };
    f.render_widget(
        Gauge::default()
            .ratio(ratio)
            .label(label)
            .gauge_style(Style::default().fg(Color::Cyan)),
        rows[0],
    );

    let speed = rip.map(|s| s.speed).unwrap_or(0.0);
    let stats = if active {
        let eta = if speed > 0.0 {
            total.saturating_sub(done) as f64 / speed
        } else {
            f64::INFINITY
        };
        format!(
            "  speed {}   ETA {}   {}/{}",
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
        rows[1],
    );

    // The buttons are clickable, so they carry no keybind hints; the keys
    // still work and are listed in the help window (`?`).
    let btns = Layout::horizontal([
        Constraint::Length(18),
        Constraint::Length(13),
        Constraint::Length(10),
        Constraint::Length(11),
        Constraint::Length(10),
    ])
    .split(rows[2]);

    let can_rip = app.toc.is_some() && !active;
    let can_eject = app.drives.get(app.drive_sel).is_some_and(|d| !d.is_demo()) && !active;
    draw_button(
        f,
        btns[0],
        "rip selected",
        can_rip,
        app.hover == Hover::RipSelected,
    );
    draw_button(f, btns[1], "rip all", can_rip, app.hover == Hover::RipAll);
    draw_button(f, btns[2], "tags", can_rip, app.hover == Hover::EditTags);
    draw_button(f, btns[3], "eject", can_eject, app.hover == Hover::Eject);
    draw_button(f, btns[4], "stop", active, app.hover == Hover::Stop);
    regions.rip_selected = btns[0];
    regions.rip_all = btns[1];
    regions.edit_tags = btns[2];
    regions.eject = btns[3];
    regions.stop = btns[4];
}

fn draw_status(f: &mut Frame, app: &mut App, area: Rect) {
    let left = match app.drives.get(app.drive_sel) {
        Some(d) => format!(" {} · {} ", d.path, d.status),
        None => " no drives ".to_string(),
    };
    // The keybind hints live in the help window now (`?`); the right side
    // carries the current status message, or a nudge to the help window.
    let (right, color) = match &app.status {
        Some(status) => (format!(" {status} "), Color::Yellow),
        None if app.drive_ripping(app.drive_sel) => (String::new(), Color::Gray),
        None => (" ? for keybinds ".to_string(), Color::DarkGray),
    };

    let width = area.width as usize;
    let left_len = left.chars().count();
    let room = width.saturating_sub(left_len + 2);
    let right: String = right.chars().take(room).collect();
    let pad = " ".repeat(room.saturating_sub(right.chars().count()));
    let line = Line::from(vec![
        Span::styled(left, Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {pad} ")),
        Span::styled(right, Style::default().fg(color)),
    ]);
    f.render_widget(Paragraph::new(line), area);
}

/// The keybinds reference, opened and closed with `?`.
fn draw_help(f: &mut Frame) {
    let area = f.area();
    let width = 54u16.min(area.width.saturating_sub(2));
    let height = 17u16.min(area.height.saturating_sub(2));
    let rect = Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    };
    let block = Block::bordered()
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Keybinds · press ? or esc to close ");
    f.render_widget(Paragraph::new("").block(block), rect);

    let rows = [
        ("↑/↓  j/k", "move selection"),
        ("tab", "switch panel focus"),
        ("enter", "load disc / rip selected track"),
        ("r", "rip the selected track"),
        ("a", "rip all audio tracks"),
        ("m", "switch metadata match"),
        ("t", "edit the disc's tags"),
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
            "mouse: click selects · double-click rips · wheel scrolls",
            Style::default().fg(Color::DarkGray),
        ))))
        .collect();
    f.render_widget(Paragraph::new(lines), rect.inner(Margin::new(1, 1)));
}

/// One action button. The brackets and the colors mark it as clickable
/// rather than plain text: bold cyan brackets when enabled, the whole
/// button inverted on hover, and a flat dark gray when disabled.
fn draw_button(f: &mut Frame, rect: Rect, label: &str, enabled: bool, hovered: bool) {
    let spans: Vec<Span> = if enabled && hovered {
        vec![Span::styled(
            format!(" [ {label} ] "),
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
        let text = if enabled {
            Style::default()
        } else {
            Style::default().fg(Color::DarkGray)
        };
        vec![
            Span::styled(" [ ", bracket),
            Span::styled(label.to_string(), text),
            Span::styled(" ] ", bracket),
        ]
    };
    f.render_widget(Paragraph::new(Line::from(spans)), rect);
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
        .border_style(Style::default().fg(Color::Cyan))
        .title(" Edit tags ");
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
        Paragraph::new(" enter save · esc cancel · ↑↓/tab fields · ctrl-c cancel ")
            .style(Style::default().fg(Color::DarkGray)),
        rows[2],
    );
}

/// One editor line: the label, the value, and the cursor as a reversed bar.
fn field_line(field: &EditField, selected: bool) -> Line<'_> {
    let before = &field.value[..field.cursor];
    let after = &field.value[field.cursor..];
    let text = format!("{before}▏{after}");
    let label = format!("{:<12} ", field.label);
    let value = if selected {
        Span::styled(text, Style::default().add_modifier(Modifier::REVERSED))
    } else {
        Span::raw(text)
    };
    Line::from(vec![Span::raw(label), value])
}

fn current_track(rip: Option<&crate::app::RipState>, app: &App) -> Option<u8> {
    let idx = rip?
        .states
        .iter()
        .position(|s| matches!(s, TrackState::Ripping { .. }))?;
    app.toc.as_ref()?.tracks.get(idx).map(|t| t.number)
}

fn state_cell(state: Option<&TrackState>) -> (String, Color) {
    match state {
        None | Some(TrackState::Idle) => ("—".to_string(), Color::DarkGray),
        Some(TrackState::Ripping {
            bytes_done,
            bytes_total,
        }) => {
            let pct = *bytes_done * 100 / (*bytes_total).max(1);
            (
                format!(
                    "{pct:3}%  {}/{}",
                    fmt_bytes(*bytes_done),
                    fmt_bytes(*bytes_total)
                ),
                Color::Yellow,
            )
        }
        Some(TrackState::Done { bytes }) => (format!("done  {}", fmt_bytes(*bytes)), Color::Green),
        Some(TrackState::Failed(e)) => (format!("failed  {e}"), Color::Red),
        Some(TrackState::Skipped(r)) => (format!("skipped  {r}"), Color::DarkGray),
    }
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
