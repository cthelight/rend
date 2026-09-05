//! Rendering: drives, table of contents, rip progress, and the status bar.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, Gauge, List, ListItem, Paragraph, Row, Table};

use crate::app::{App, Focus, Hover, Regions, TrackState};

/// Draws the whole screen and updates the mouse hit-test regions.
pub fn draw(f: &mut Frame, app: &mut App) {
    let area = f.area();
    let drives_height = 2 + app.drives.len().min(8) as u16;
    let chunks = Layout::vertical([
        Constraint::Length(drives_height),
        Constraint::Min(4),
        Constraint::Length(6),
        Constraint::Length(1),
    ])
    .split(area);

    let mut regions = Regions::default();
    draw_drives(f, app, chunks[0], &mut regions);
    draw_toc(f, app, chunks[1], &mut regions);
    draw_rip(f, app, chunks[2], &mut regions);
    draw_status(f, app, chunks[3]);
    app.regions = regions;
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

    let visible = app.visible(area);
    let start = clamp_scroll(app.drives_scroll, app.drive_sel, app.drives.len(), visible);
    app.drives_scroll = start;

    let items: Vec<ListItem> = app
        .drives
        .iter()
        .enumerate()
        .skip(start)
        .take(visible)
        .map(|(i, d)| {
            let (style, marker) = if i == app.drive_sel {
                (Style::default().add_modifier(Modifier::REVERSED), "●")
            } else if app.hover == Hover::Drive(i) {
                (Style::default().add_modifier(Modifier::BOLD), "○")
            } else {
                (Style::default(), "○")
            };
            let rip = if app.drive_ripping(i) {
                Span::styled("  ▶", Style::default().fg(Color::Yellow))
            } else {
                Span::raw("")
            };
            ListItem::new(Line::from(vec![
                Span::raw(format!(" {marker} {:<14} {:<34.34} ", d.path, d.label)),
                Span::styled(
                    d.status.clone(),
                    Style::default().fg(status_color(&d.status)),
                ),
                rip,
            ]))
            .style(style)
        })
        .collect();

    f.render_widget(List::new(items).block(block), area);
}

fn draw_toc(f: &mut Frame, app: &mut App, area: Rect, regions: &mut Regions) {
    regions.tracks = area;
    let path = app
        .drives
        .get(app.drive_sel)
        .map(|d| d.path.as_str())
        .unwrap_or("?");
    let title = match app.toc.as_ref() {
        Some(_) => {
            let id = app
                .disc_id
                .as_deref()
                .map(|s| format!(" · disc id {s}"))
                .unwrap_or_default();
            format!(" TOC — {path}{id} ")
        }
        None => format!(" TOC — {path} (no disc) "),
    };
    let block = panel_block(title, app.focus == Focus::Tracks);

    let Some(toc) = app.toc.as_ref() else {
        let msg = if app.drives.is_empty() {
            "no drive selected — try rend-tui --demo"
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

    let header = Row::new(vec!["#", "type", "start", "length", "state"]).style(
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
                Cell::from(Span::styled(text, Style::default().fg(color))),
            ])
            .style(style)
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Length(8),
            Constraint::Length(10),
            Constraint::Length(8),
            Constraint::Min(10),
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
            Some(n) => format!("track {n:02}  {pct}%"),
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
        "  select a track and press r, or a for all audio tracks".to_string()
    };
    f.render_widget(
        Paragraph::new(stats).style(Style::default().fg(Color::Gray)),
        rows[1],
    );

    let btns = Layout::horizontal([
        Constraint::Length(18),
        Constraint::Length(14),
        Constraint::Length(12),
        Constraint::Length(12),
    ])
    .split(rows[2]);

    let can_rip = app.toc.is_some() && !active;
    let can_eject = app.drives.get(app.drive_sel).is_some_and(|d| !d.is_demo()) && !active;
    draw_button(
        f,
        btns[0],
        "[r] rip selected",
        can_rip,
        app.hover == Hover::RipSelected,
    );
    draw_button(
        f,
        btns[1],
        "[a] rip all",
        can_rip,
        app.hover == Hover::RipAll,
    );
    draw_button(
        f,
        btns[2],
        "[e] eject",
        can_eject,
        app.hover == Hover::Eject,
    );
    draw_button(f, btns[3], "[s] stop", active, app.hover == Hover::Stop);
    regions.rip_selected = btns[0];
    regions.rip_all = btns[1];
    regions.eject = btns[2];
    regions.stop = btns[3];
}

fn draw_status(f: &mut Frame, app: &mut App, area: Rect) {
    let left = match app.drives.get(app.drive_sel) {
        Some(d) => format!(" {} · {} ", d.path, d.status),
        None => " no drives ".to_string(),
    };
    let (right, color) = match &app.status {
        Some(status) => (format!(" {status} "), Color::Yellow),
        None if app.drive_ripping(app.drive_sel) => (
            " [s] stop   [o] format   [f] force   [q] quit ".to_string(),
            Color::Gray,
        ),
        None => (
            " [↑↓] move · [tab] focus · [enter] activate · [r] rip · [a] all · [e] eject · [o] format · [f] force · [q] quit "
                .to_string(),
            Color::Gray,
        ),
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

fn draw_button(f: &mut Frame, rect: Rect, label: &str, enabled: bool, hovered: bool) {
    let mut style = if enabled {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    if hovered && enabled {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(format!(" {label} "), style))),
        rect,
    );
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
    use rend_core::TrackType;

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
