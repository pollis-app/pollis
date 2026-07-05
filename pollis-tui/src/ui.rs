//! Rendering. One `render` entry point draws the whole frame from `&App` —
//! ratatui's immediate-mode model means the view is a pure function of state.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
    Frame,
};

use crate::app::{App, Screen};

pub fn render(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            // header
            Constraint::Length(1),
            // body
            Constraint::Min(1),
            // status
            Constraint::Length(1),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], app);
    render_body(frame, chunks[1], app);
    render_status(frame, chunks[2], app);
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let ident = app
        .identity()
        .map(|u| format!(" — {u}"))
        .unwrap_or_default();
    let header = Paragraph::new(Line::from(vec![
        Span::styled(" pollis ", Style::default().add_modifier(Modifier::BOLD)),
        Span::styled(ident, Style::default().fg(Color::DarkGray)),
    ]))
    .style(Style::default().bg(Color::Rgb(30, 30, 34)));
    frame.render_widget(header, area);
}

fn render_body(frame: &mut Frame, area: Rect, app: &App) {
    // Center a fixed-width card in the available space.
    let card = centered(area, 60, 9);

    let (title, prompt, value) = match app.screen {
        Screen::Booting => ("Starting up", "Checking for an existing session…", String::new()),
        Screen::Email => ("Sign in", "Email:", app.input.clone()),
        Screen::Otp => ("Verify", "Code:", app.input.clone()),
        Screen::SetPin => ("Set PIN", "New PIN:", mask(&app.input)),
        Screen::Unlock => ("Unlock", "PIN:", mask(&app.input)),
        Screen::Home => (
            "Home",
            "Signed in. Groups, channels and DMs land in M2. Press q to quit.",
            String::new(),
        ),
        Screen::Fatal => ("Error", "Press any key to exit.", String::new()),
    };

    let mut lines = vec![Line::from(""), Line::from(prompt)];
    if !matches!(app.screen, Screen::Booting | Screen::Home | Screen::Fatal) {
        // Input line with a cursor caret.
        lines.push(Line::from(vec![
            Span::styled("  › ", Style::default().fg(Color::Cyan)),
            Span::raw(value),
            Span::styled("▏", Style::default().add_modifier(Modifier::SLOW_BLINK)),
        ]));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(Span::styled(
            format!(" {title} "),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let para = Paragraph::new(lines).block(block);
    frame.render_widget(para, card);
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = Vec::new();
    if app.busy {
        spans.push(Span::styled(
            " working… ",
            Style::default().fg(Color::Yellow),
        ));
    }
    if let Some(status) = &app.status {
        spans.push(Span::styled(
            status.clone(),
            Style::default().fg(Color::Gray),
        ));
    }
    if spans.is_empty() {
        spans.push(Span::styled(
            "Ctrl-C to quit",
            Style::default().fg(Color::DarkGray),
        ));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
}

/// Mask a PIN/secret as bullets so it never renders in the clear.
fn mask(s: &str) -> String {
    "•".repeat(s.chars().count())
}

/// A `w`×`h` rectangle centered inside `area` (clamped to fit).
fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}
