//! Rendering. One `render` entry point draws the whole frame from `&App` —
//! ratatui's immediate-mode model means the view is a pure function of state.
//! The auth screens (M1) render a centered card; the signed-in Home screen (M2b)
//! renders the three-pane client (sidebar · messages · status).

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, Screen};
use crate::enroll_flow::EnrollChoice;
use crate::home::{visible_window, ConvKind, Focus, HomeMode, HomeState};

/// A solid selection background (no glow — repo rule): reverse-style highlight.
const SEL_BG: Color = Color::Rgb(45, 45, 52);
const SEL_BG_FOCUSED: Color = Color::Rgb(58, 92, 140);

/// The left pane's fixed width, in columns.
const SIDEBAR_WIDTH: u16 = 28;

/// Spinner frames for the sync indicator (solid Braille dots, cycled per refresh).
const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

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
    match app.screen {
        Screen::Home => render_home(frame, chunks[1], app),
        Screen::EnrollChoice => render_enroll_choice(frame, chunks[1], app),
        Screen::EnrollWaiting => render_enroll_waiting(frame, chunks[1], app),
        Screen::PendingEnrollments => render_pending_enrollments(frame, chunks[1], app),
        _ => render_auth_body(frame, chunks[1], app),
    }
    render_status(frame, chunks[2], app);
}

/// The inner height (in rows) of the message pane for a given full-frame area,
/// used by the input loop to page-scroll by whole screens. Mirrors the layout:
/// frame minus header + status (2) minus the message block's top+bottom border
/// (2).
pub fn message_viewport_height(area: Rect) -> usize {
    area.height.saturating_sub(4) as usize
}

fn render_header(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = vec![Span::styled(
        " pollis ",
        Style::default().add_modifier(Modifier::BOLD),
    )];
    if let Some(user) = app.identity() {
        spans.push(Span::styled(
            format!("— {user} "),
            Style::default().fg(Color::Gray),
        ));
    }
    // On Home, name the open conversation and show the sync indicator on the right.
    if app.screen == Screen::Home {
        if let Some(open) = &app.home.open {
            spans.push(Span::styled("· ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                open.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ));
        }
    }
    let left = Paragraph::new(Line::from(spans)).style(Style::default().bg(Color::Rgb(30, 30, 34)));
    frame.render_widget(left, area);

    if app.screen == Screen::Home {
        let indicator = sync_indicator(&app.home);
        let right = Paragraph::new(Line::from(indicator))
            .alignment(Alignment::Right)
            .style(Style::default().bg(Color::Rgb(30, 30, 34)));
        frame.render_widget(right, area);
    }
}

/// The header's right-side sync indicator: a cycling spinner plus a live
/// conversation count. Honest about what's happening (spec §8) without a glow.
fn sync_indicator(home: &HomeState) -> Vec<Span<'static>> {
    let frame = SPINNER[(home.refreshes as usize) % SPINNER.len()];
    let count = home.tree.as_ref().map(|t| t.len()).unwrap_or(0);
    vec![
        Span::styled(format!("{frame} sync "), Style::default().fg(Color::Green)),
        Span::styled(
            format!("· {count} conversations "),
            Style::default().fg(Color::DarkGray),
        ),
    ]
}

/// The three-pane client body: sidebar tree on the left, message list on the
/// right, plus a bottom input bar (compose or create/invite prompt) when the
/// screen is in an input mode — the desktop app's "replace the input bar"
/// pattern, never a modal overlay.
fn render_home(frame: &mut Frame, area: Rect, app: &App) {
    let home = &app.home;
    // Reserve a bottom bar for compose/prompt input when one is active.
    let body = if home.mode == HomeMode::Navigate {
        area
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(3)])
            .split(area);
        render_input_bar(frame, rows[1], app);
        rows[0]
    };

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(SIDEBAR_WIDTH),
            Constraint::Min(10),
        ])
        .split(body);

    render_sidebar(frame, cols[0], home);
    render_messages(frame, cols[1], home);
}

/// The bottom input bar: a solid-bordered line labeled with what it's collecting
/// (compose = the conversation; prompt = the create/invite action), showing the
/// live buffer with a caret. No glow — a plain accent border.
fn render_input_bar(frame: &mut Frame, area: Rect, app: &App) {
    let label = match &app.home.mode {
        HomeMode::Compose => {
            let name = app
                .home
                .open
                .as_ref()
                .map(|o| o.name.clone())
                .unwrap_or_else(|| "Message".to_string());
            format!(" Message {name} ")
        }
        HomeMode::Prompt(kind) => format!(" {} ", kind.label()),
        HomeMode::Navigate => " Input ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(90, 140, 200)))
        .title(Span::styled(label, Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let line = Line::from(vec![
        Span::styled("› ", Style::default().fg(Color::Cyan)),
        Span::raw(app.input.clone()),
        Span::styled("▏", Style::default().add_modifier(Modifier::SLOW_BLINK)),
    ]);
    frame.render_widget(Paragraph::new(line), inner);
}

fn render_sidebar(frame: &mut Frame, area: Rect, home: &HomeState) {
    let focused = home.focus == Focus::Sidebar;
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(Span::styled(
            " Conversations ",
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if home.rows.is_empty() {
        let hint = Paragraph::new("No conversations yet.\nWaiting for sync…")
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true });
        frame.render_widget(hint, inner);
        return;
    }

    let lines: Vec<Line> = home
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            let indent = "  ".repeat(row.depth as usize);
            let selected = i == home.selected && row.selectable();
            let mut style = Style::default();
            if selected {
                style = style
                    .bg(if focused { SEL_BG_FOCUSED } else { SEL_BG })
                    .add_modifier(Modifier::BOLD);
            } else if !row.selectable() {
                // Section headers are dim and non-interactive.
                style = style
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD);
            }
            Line::from(Span::styled(format!("{indent}{}", row.label), style))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_messages(frame: &mut Frame, area: Rect, home: &HomeState) {
    let focused = home.focus == Focus::Messages;
    let title = home
        .open
        .as_ref()
        .map(|o| format!(" {} ", o.name))
        .unwrap_or_else(|| " Messages ".to_string());
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(border_style(focused))
        .title(Span::styled(title, Style::default().add_modifier(Modifier::BOLD)));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let Some(open) = &home.open else {
        let hint = Paragraph::new("Select a conversation on the left (↑/↓, Enter).")
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true });
        frame.render_widget(hint, inner);
        return;
    };

    if open.loading && open.messages.is_empty() {
        frame.render_widget(
            Paragraph::new("Loading…").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }
    if open.messages.is_empty() {
        let msg = match open.kind {
            Some(ConvKind::DmRequest) => "Pending request — no messages yet.",
            _ => "No messages yet.",
        };
        frame.render_widget(
            Paragraph::new(msg).style(Style::default().fg(Color::DarkGray)),
            inner,
        );
        return;
    }

    // Bottom-anchored window over the message buffer (newest at the bottom).
    let viewport = inner.height as usize;
    let (start, end, top_pad) = visible_window(open.messages.len(), viewport, open.scroll);

    let mut lines: Vec<Line> = Vec::with_capacity(viewport);
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    // A hint that older history is still loadable, shown at the very top.
    if start == 0 && !open.at_beginning {
        lines.push(Line::from(Span::styled(
            "  ↑ more history — scroll up to load",
            Style::default().fg(Color::DarkGray),
        )));
    }
    for m in &open.messages[start..end] {
        lines.push(message_line(m));
    }

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Render a single message as `sender  content`, handling the deleted / not-yet-
/// decrypted / edited states honestly.
fn message_line(m: &pollis_core::commands::messages::ChannelMessage) -> Line<'static> {
    let sender = m
        .sender_username
        .clone()
        .unwrap_or_else(|| m.sender_id.clone());
    let (body, body_style) = if m.deleted_at.is_some() {
        (
            "(deleted)".to_string(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )
    } else if let Some(content) = &m.content {
        let edited = if m.edited_at.is_some() { " (edited)" } else { "" };
        (format!("{content}{edited}"), Style::default())
    } else {
        (
            "(unable to decrypt)".to_string(),
            Style::default().fg(Color::Red),
        )
    };
    Line::from(vec![
        Span::styled(
            format!("{sender}  "),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(body, body_style),
    ])
}

/// A focused pane gets a solid accent border; an unfocused one a muted border.
fn border_style(focused: bool) -> Style {
    if focused {
        Style::default().fg(Color::Rgb(90, 140, 200))
    } else {
        Style::default().fg(Color::DarkGray)
    }
}

/// The centered-card auth body (M1 screens).
fn render_auth_body(frame: &mut Frame, area: Rect, app: &App) {
    let card = centered(area, 60, 9);

    let (title, prompt, value) = match app.screen {
        Screen::Booting => ("Starting up", "Checking for an existing session…", String::new()),
        Screen::Email => ("Sign in", "Email:", app.input.clone()),
        Screen::Otp => ("Verify", "Code:", app.input.clone()),
        Screen::SetPin => ("Set PIN", "New PIN:", mask(&app.input)),
        Screen::Unlock => ("Unlock", "PIN:", mask(&app.input)),
        // The Secret Key is long and pasted, not memorized — show it plainly so
        // the user can spot a typo before submitting.
        Screen::RecoverKey => ("Recover", "Secret Key:", app.input.clone()),
        // Rendered by their own functions; these arms keep the match exhaustive.
        Screen::EnrollChoice => ("Enroll", "", String::new()),
        Screen::EnrollWaiting => ("Enroll", "", String::new()),
        Screen::PendingEnrollments => ("Enrollments", "", String::new()),
        // Home is rendered by render_home; Fatal shows a simple message.
        Screen::Home => ("Home", "", String::new()),
        Screen::Fatal => ("Error", "Press any key to exit.", String::new()),
    };

    let mut lines = vec![Line::from(""), Line::from(prompt)];
    if !matches!(
        app.screen,
        Screen::Booting | Screen::Home | Screen::Fatal
    ) {
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

/// Second device: the enroll-choice screen — a centered card listing the two
/// paths (sibling approval / Secret-Key recovery) with the highlighted one shown
/// selected. No modal; a plain bordered card like the other auth screens.
fn render_enroll_choice(frame: &mut Frame, area: Rect, app: &App) {
    let card = centered(area, 62, 9);
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  This account already exists on another device.",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
    ];
    for choice in EnrollChoice::ALL {
        let selected = choice == app.enroll_choice;
        let marker = if selected { "›" } else { " " };
        let mut style = Style::default();
        if selected {
            style = style.bg(SEL_BG_FOCUSED).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(
            format!("  {marker} {}", choice.label()),
            style,
        )));
    }

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Add this device ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

/// Second device: the waiting-for-approval screen — the verification code shown
/// big and clear, with a prompt to enter it on the existing device.
fn render_enroll_waiting(frame: &mut Frame, area: Rect, app: &App) {
    let card = centered(area, 62, 11);
    let code = app
        .enroll_handle
        .as_ref()
        .map(|h| h.verification_code.clone())
        .unwrap_or_default();

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "  Enter this code on your other device to approve:",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("      {code}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            if app.enroll_stopped {
                "  Not approved."
            } else {
                "  Waiting for approval…"
            },
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Enroll this device ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

/// Existing device: the full-screen "Pending device enrollments" list. Each row
/// shows the requesting device id and its verification code so the user can
/// confirm the code matches the new device's screen before approving.
fn render_pending_enrollments(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Pending device enrollments ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let requests = &app.approvals.requests;
    if requests.is_empty() {
        let hint = Paragraph::new("No devices are waiting for approval.\n\nEsc to go back.")
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true });
        frame.render_widget(hint, inner);
        return;
    }

    let lines: Vec<Line> = requests
        .iter()
        .enumerate()
        .map(|(i, r)| {
            let selected = i == app.approvals.selected;
            let mut style = Style::default();
            if selected {
                style = style.bg(SEL_BG_FOCUSED).add_modifier(Modifier::BOLD);
            }
            let marker = if selected { "›" } else { " " };
            Line::from(Span::styled(
                format!(
                    "{marker} device {}   code {}",
                    r.new_device_id, r.verification_code
                ),
                style,
            ))
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let mut spans = Vec::new();
    if app.busy {
        spans.push(Span::styled(" working… ", Style::default().fg(Color::Yellow)));
    }
    if let Some(status) = &app.status {
        spans.push(Span::styled(status.clone(), Style::default().fg(Color::Gray)));
    }
    if spans.is_empty() {
        let help = match app.screen {
            Screen::Home => home_help(&app.home.mode),
            Screen::EnrollChoice => "↑/↓ choose · Enter continue · Ctrl-C quit",
            Screen::EnrollWaiting => "Waiting for approval… · Ctrl-C quit",
            Screen::RecoverKey => "Type/paste your Secret Key · Enter recover · Esc back",
            Screen::PendingEnrollments => "↑/↓ move · a approve · r reject · Esc back",
            _ => "Ctrl-C to quit",
        };
        spans.push(Span::styled(help, Style::default().fg(Color::DarkGray)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
}

/// The status-bar key hints for the Home screen, per input mode — the in-app
/// discovery surface for compose / accept / create / invite / quit.
fn home_help(mode: &HomeMode) -> &'static str {
    match mode {
        HomeMode::Navigate => {
            "↑/↓ move · Tab pane · Enter open · i compose · a accept · g group · c channel · d DM · v invite · E enroll · q quit"
        }
        HomeMode::Compose => "Type · Enter send · Esc cancel",
        HomeMode::Prompt(_) => "Type · Enter submit · Esc cancel",
    }
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
