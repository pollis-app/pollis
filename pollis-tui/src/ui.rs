//! Rendering: `render` draws the whole frame as a pure function of `&App`.

use ratatui::{
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Paragraph, Wrap},
    Frame,
};

use crate::app::{App, Screen};
use crate::content::fit_width;
use crate::enroll_flow::EnrollChoice;
use crate::home::{visible_window, ComposeTarget, ConvKind, Focus, HomeMode, HomeState};
use crate::timeline;

const SEL_BG: Color = Color::Rgb(45, 45, 52);
const SEL_BG_FOCUSED: Color = Color::Rgb(58, 92, 140);

const ACCENT: Color = Color::Rgb(90, 140, 200);

const SIDEBAR_WIDTH: u16 = 28;

/// The status line and the key-hint bar; hints get their own row so a status
/// message never hides them (#1337).
const HOME_FOOTER_ROWS: u16 = 2;

const INPUT_BAR_ROWS: u16 = 3;

const SPINNER: [&str; 8] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];

pub fn render(frame: &mut Frame, app: &App) {
    let footer = if app.screen == Screen::Home {
        HOME_FOOTER_ROWS
    } else {
        1
    };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(footer),
        ])
        .split(frame.area());

    render_header(frame, chunks[0], app);
    match app.screen {
        Screen::Home => render_home(frame, chunks[1], app),
        Screen::SaveSecretKey => render_save_secret_key(frame, chunks[1], app),
        Screen::EnrollChoice => render_enroll_choice(frame, chunks[1], app),
        Screen::EnrollWaiting => render_enroll_waiting(frame, chunks[1], app),
        Screen::PendingEnrollments => render_pending_enrollments(frame, chunks[1], app),
        Screen::ApproveEnrollment => render_approve_enrollment(frame, chunks[1], app),
        _ => render_auth_body(frame, chunks[1], app),
    }
    render_status(frame, chunks[2], app);
}

/// Shared by the renderer and [`message_viewport`], so the size the key
/// handler scrolls by is the size that is drawn.
struct HomeLayout {
    sidebar: Rect,
    messages: Rect,
    input: Option<Rect>,
}

fn home_layout(area: Rect, home: &HomeState) -> HomeLayout {
    let (body, input) = if home.mode == HomeMode::Navigate {
        (area, None)
    } else {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(1), Constraint::Length(INPUT_BAR_ROWS)])
            .split(area);
        (rows[0], Some(rows[1]))
    };
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Length(SIDEBAR_WIDTH), Constraint::Min(10)])
        .split(body);
    HomeLayout {
        sidebar: cols[0],
        messages: cols[1],
        input,
    }
}

/// The message pane's inner `(width, height)` for a full-frame `area`. Must
/// mirror [`render`]'s layout.
pub fn message_viewport(area: Rect, app: &App) -> (u16, usize) {
    let body = Rect::new(
        area.x,
        area.y + 1,
        area.width,
        area.height.saturating_sub(1 + HOME_FOOTER_ROWS),
    );
    let pane = home_layout(body, &app.home).messages;
    (
        pane.width.saturating_sub(2),
        pane.height.saturating_sub(2) as usize,
    )
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
    if app.screen == Screen::Home {
        if let Some(open) = &app.home.open {
            spans.push(Span::styled("· ", Style::default().fg(Color::DarkGray)));
            spans.push(Span::styled(
                open.name.as_str(),
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

/// Input modes replace the bottom bar (the desktop pattern), never a modal.
fn render_home(frame: &mut Frame, area: Rect, app: &App) {
    let layout = home_layout(area, &app.home);
    if let Some(input) = layout.input {
        render_input_bar(frame, input, app);
    }
    render_sidebar(frame, layout.sidebar, &app.home);
    render_messages(frame, layout.messages, app);
}

/// Focus must be unmistakable since it decides what ↑/↓ and Enter do (#1337).
fn pane_block(title: String, focused: bool) -> Block<'static> {
    if focused {
        Block::default()
            .borders(Borders::ALL)
            .border_type(BorderType::Thick)
            .border_style(Style::default().fg(ACCENT))
            .title(Span::styled(
                title,
                Style::default()
                    .fg(Color::Black)
                    .bg(ACCENT)
                    .add_modifier(Modifier::BOLD),
            ))
    } else {
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title(Span::styled(title, Style::default().fg(Color::Gray)))
    }
}

fn render_input_bar(frame: &mut Frame, area: Rect, app: &App) {
    let home = &app.home;
    let name = home.open.as_ref().map_or("Message", |o| o.name.as_str());
    let in_thread = home.open.as_ref().is_some_and(|o| o.thread.is_some());
    let label = match &home.mode {
        HomeMode::Compose => match &home.compose {
            ComposeTarget::New if in_thread => " Reply in thread ".to_string(),
            ComposeTarget::New => format!(" Message {name} "),
            ComposeTarget::Reply {
                sender, preview, ..
            } => format!(" Reply to {sender}: {preview} "),
            ComposeTarget::Edit { .. } => " Edit your message ".to_string(),
        },
        HomeMode::Prompt(kind) => format!(" {} ", kind.label()),
        HomeMode::ConfirmDelete { .. } => " Delete message ".to_string(),
        HomeMode::Navigate => " Input ".to_string(),
    };
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(ACCENT))
        .title(Span::styled(
            fit_width(&label, area.width.saturating_sub(2) as usize),
            Style::default().add_modifier(Modifier::BOLD),
        ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let line = if let HomeMode::ConfirmDelete { preview, .. } = &home.mode {
        Line::from(vec![
            Span::raw(format!("Delete “{preview}”?  ")),
            Span::styled("y", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" delete · "),
            Span::styled("n", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(" keep"),
        ])
    } else {
        // When the text outgrows the bar, the end (where the caret is) stays
        // in view.
        let shown = app.input.replace('\n', "⏎ ");
        let room = (inner.width as usize).saturating_sub(4);
        let count = shown.chars().count();
        let shown = if count > room {
            let tail: String = shown.chars().skip(count - room.saturating_sub(1)).collect();
            format!("…{tail}")
        } else {
            shown
        };
        Line::from(vec![
            Span::styled("› ", Style::default().fg(Color::Cyan)),
            Span::raw(shown),
            Span::styled("▏", Style::default().add_modifier(Modifier::SLOW_BLINK)),
        ])
    };
    frame.render_widget(Paragraph::new(line), inner);
}

fn render_sidebar(frame: &mut Frame, area: Rect, home: &HomeState) {
    let focused = home.focus == Focus::Sidebar;
    let block = pane_block(" Conversations ".to_string(), focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if home.rows.is_empty() {
        let hint = Paragraph::new("No conversations yet.\nWaiting for sync…\n\nd  start a DM\ng  new group")
            .style(Style::default().fg(Color::DarkGray))
            .wrap(Wrap { trim: true });
        frame.render_widget(hint, inner);
        return;
    }

    let width = inner.width as usize;
    let lines: Vec<Line> = home
        .rows
        .iter()
        .enumerate()
        .map(|(i, row)| {
            // Rows are only depth 0 or 1 (`home::build_sidebar_rows`).
            let indent = if row.depth == 0 { "" } else { "  " };
            let selected = i == home.selected && row.selectable();
            let mut style = Style::default();
            if selected {
                style = style
                    .bg(if focused { SEL_BG_FOCUSED } else { SEL_BG })
                    .add_modifier(Modifier::BOLD);
            } else if !row.selectable() {
                style = style
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD);
            }
            let label = fit_width(&row.label, width.saturating_sub(indent.len()));
            Line::from(vec![Span::styled(indent, style), Span::styled(label, style)])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Existing device: type the verification code shown on the new device (#1096).
///
/// Deliberately shows no code of its own: the typed code is compared against
/// one derived from the ephemeral key this device fetched, so a substituted
/// key fails here.
fn render_approve_enrollment(frame: &mut Frame, area: Rect, app: &App) {
    use pollis_core::commands::device_enrollment::ENROLLMENT_SAS_LEN;

    let card = centered(area, 62, 11);
    let device = app
        .approvals
        .current()
        .map_or("?", |r| r.new_device_id.as_str());

    let typed = app.input.chars().count();
    let field = format!(
        "{}{}",
        app.input,
        "_".repeat(ENROLLMENT_SAS_LEN.saturating_sub(typed))
    );

    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        " Approve device ",
        Style::default().add_modifier(Modifier::BOLD),
    ));
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  Device {device} wants to join this account."),
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  Read the code off THAT device's screen and type it here:",
            Style::default().fg(Color::Gray),
        )),
        Line::from(""),
        Line::from(Span::styled(
            format!("      {field}"),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(Span::styled(
            "  If it does not match, press Esc and reject the request.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

fn render_messages(frame: &mut Frame, area: Rect, app: &App) {
    let home = &app.home;
    let focused = home.focus == Focus::Messages;
    let title = match &home.open {
        Some(o) if o.thread.is_some() => format!(" Thread · {} ", o.name),
        Some(o) => format!(" {} ", o.name),
        None => " Messages ".to_string(),
    };
    let block = pane_block(title, focused);
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
    if open.displayed().is_empty() {
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

    // A bottom-anchored window of display lines (newest at the bottom).
    let blocks = timeline::layout_open(
        open,
        app.signed_in_user_id().unwrap_or(""),
        app.identity().unwrap_or(""),
        inner.width,
    );
    let all: Vec<Line> = blocks.into_iter().flat_map(|b| b.lines).collect();
    let viewport = inner.height as usize;
    let (start, end, top_pad) = visible_window(all.len(), viewport, open.scroll);

    let mut lines: Vec<Line> = Vec::with_capacity(viewport);
    for _ in 0..top_pad {
        lines.push(Line::from(""));
    }
    // The older-history hint goes in the padding, never over a message line.
    if start == 0 && top_pad > 0 && !open.at_beginning && open.thread.is_none() {
        lines[top_pad - 1] = Line::from(Span::styled(
            "  ↑ more history — scroll up to load",
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.extend(all.into_iter().skip(start).take(end - start));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// First-device signup: the Secret Key card (#1337). Shown once; the user
/// saves it, then types it back.
fn render_save_secret_key(frame: &mut Frame, area: Rect, app: &App) {
    let card = centered(area, 72, 15);
    let warn = Style::default().fg(Color::Yellow);
    let gray = Style::default().fg(Color::Gray);
    let key_style = Style::default().add_modifier(Modifier::BOLD);
    let (title, lines) = if app.secret_confirming {
        (
            " Confirm your Secret Key ",
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  Type or paste the key you just saved, to prove you can get it back.",
                    gray,
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  › ", Style::default().fg(Color::Cyan)),
                    Span::raw(app.input.clone()),
                    Span::styled("▏", Style::default().add_modifier(Modifier::SLOW_BLINK)),
                ]),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  Enter", key_style),
                    Span::styled(" confirm   ", gray),
                    Span::styled("Esc", key_style),
                    Span::styled(" show the key again", gray),
                ]),
            ],
        )
    } else {
        let secret = app.pending_secret().unwrap_or("");
        (
            " Your Secret Key ",
            vec![
                Line::from(""),
                Line::from(Span::styled(
                    "  This is the only way to sign in on a new device without one of",
                    gray,
                )),
                Line::from(Span::styled(
                    "  your existing devices. It is shown ONCE and never stored. Lose it",
                    gray,
                )),
                Line::from(Span::styled(
                    "  and every device, and the account cannot be recovered.",
                    warn,
                )),
                Line::from(""),
                Line::from(Span::styled(
                    format!("      {secret}"),
                    Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD),
                )),
                Line::from(""),
                Line::from(Span::styled(
                    "  Put it in a password manager, or print it and lock it away.",
                    gray,
                )),
                Line::from(""),
                Line::from(vec![
                    Span::styled("  s", key_style),
                    Span::styled("      save an Emergency Kit file to the current directory", gray),
                ]),
                Line::from(vec![
                    Span::styled("  Enter", key_style),
                    Span::styled("  I've saved it — continue", gray),
                ]),
            ],
        )
    };
    let block = Block::default().borders(Borders::ALL).title(Span::styled(
        title,
        Style::default().add_modifier(Modifier::BOLD),
    ));
    frame.render_widget(Paragraph::new(lines).block(block), card);
}

fn render_auth_body(frame: &mut Frame, area: Rect, app: &App) {
    let card = centered(area, 60, 9);

    let (title, prompt, value) = match app.screen {
        Screen::Booting => ("Starting up", "Checking for an existing session…", String::new()),
        Screen::Email => ("Sign in", "Email:", app.input.clone()),
        Screen::Otp => ("Verify", "Code:", app.input.clone()),
        Screen::SaveSecretKey => ("Secret Key", "", String::new()),
        Screen::SetPin => ("Set PIN", "New PIN:", mask(&app.input)),
        Screen::Unlock => ("Unlock", "PIN:", mask(&app.input)),
        // Shown plainly: it is long and pasted, so the user must spot typos.
        Screen::RecoverKey => ("Recover", "Secret Key:", app.input.clone()),
        // Rendered by their own functions.
        Screen::EnrollChoice => ("Enroll", "", String::new()),
        Screen::EnrollWaiting => ("Enroll", "", String::new()),
        Screen::PendingEnrollments => ("Enrollments", "", String::new()),
        Screen::ApproveEnrollment => ("Approve", "", String::new()),
        Screen::Home => ("Home", "", String::new()),
        Screen::Fatal => ("Error", "Press any key to exit.", String::new()),
    };

    let mut lines = vec![Line::from(""), Line::from(prompt)];
    if !matches!(
        app.screen,
        Screen::Booting | Screen::Home | Screen::Fatal
    ) {
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

fn render_enroll_waiting(frame: &mut Frame, area: Rect, app: &App) {
    let card = centered(area, 62, 11);
    let code = app
        .enroll_handle
        .as_ref()
        .map_or("", |h| h.verification_code.as_str());

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

/// Existing device: the pending-enrollments list. Rows show no verification
/// code: a DS-supplied code would make the comparison meaningless (#1096); see
/// [`render_approve_enrollment`].
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
                format!("{marker} device {}", r.new_device_id),
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
        spans.push(Span::styled(status.as_str(), Style::default().fg(Color::Gray)));
    }

    if app.screen == Screen::Home {
        let rows = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(area);
        frame.render_widget(Paragraph::new(Line::from(spans)), rows[0]);
        frame.render_widget(
            Paragraph::new(hint_line(home_help(&app.home))).style(Style::default().bg(Color::Rgb(30, 30, 34))),
            rows[1],
        );
        return;
    }

    if spans.is_empty() {
        let help = match app.screen {
            Screen::EnrollChoice => "↑/↓ choose · Enter continue · Ctrl-C quit",
            Screen::EnrollWaiting => "Waiting for approval… · Ctrl-C quit",
            Screen::RecoverKey => "Type/paste your Secret Key · Enter recover · Esc back",
            Screen::PendingEnrollments => "↑/↓ move · a approve · r reject · Esc back",
            Screen::ApproveEnrollment => {
                "Type the code from the new device · Enter approve · Esc back"
            }
            Screen::SaveSecretKey if app.secret_confirming => {
                "Type/paste the key · Enter confirm · Esc show it again"
            }
            Screen::SaveSecretKey => "s save Emergency Kit file · Enter I've saved it",
            _ => "Ctrl-C to quit",
        };
        spans.push(Span::styled(help, Style::default().fg(Color::DarkGray)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Left),
        area,
    );
}

/// `"↑/↓ move · Enter open"` with each key in bold, the rest dim.
fn hint_line(hints: &'static str) -> Line<'static> {
    let mut spans = vec![Span::raw(" ")];
    for (i, part) in hints.split(" · ").enumerate() {
        if i > 0 {
            spans.push(Span::styled(" · ", Style::default().fg(Color::DarkGray)));
        }
        let (key, rest) = part.split_once(' ').unwrap_or((part, ""));
        spans.push(Span::styled(
            key,
            Style::default().fg(Color::White).add_modifier(Modifier::BOLD),
        ));
        if !rest.is_empty() {
            spans.push(Span::styled(
                format!(" {rest}"),
                Style::default().fg(Color::Gray),
            ));
        }
    }
    Line::from(spans)
}

/// The Home key hints for the focused pane and input mode.
pub fn home_help(home: &HomeState) -> &'static str {
    let in_thread = home.open.as_ref().is_some_and(|o| o.thread.is_some());
    match (&home.mode, home.focus) {
        (HomeMode::Navigate, Focus::Sidebar) => {
            "↑/↓ move · Enter open · Tab messages · d DM · g group · c channel · v invite · a accept · E devices · q quit"
        }
        (HomeMode::Navigate, Focus::Messages) if in_thread => {
            "↑/↓ select · r reply · e edit · x delete · + react · R retry · i reply in thread · Esc close thread · q quit"
        }
        (HomeMode::Navigate, Focus::Messages) => {
            "↑/↓ select · r reply · e edit · x delete · + react · R retry · t thread · i write · Esc back · Tab conversations · q quit"
        }
        (HomeMode::Compose, _) => "Enter send · Alt-Enter new line · Esc cancel",
        (HomeMode::Prompt(_), _) => "Enter submit · Esc cancel",
        (HomeMode::ConfirmDelete { .. }, _) => "y delete · n keep",
    }
}

fn mask(s: &str) -> String {
    "•".repeat(s.chars().count())
}

fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    Rect::new(x, y, w, h)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::{OpenConversation, PromptKind};

    #[test]
    fn hints_follow_the_focused_pane_and_mode() {
        let mut home = HomeState::new();
        assert!(home_help(&home).contains("Enter open"));
        assert!(home_help(&home).contains("d DM"));
        home.focus = Focus::Messages;
        for key in ["r reply", "e edit", "x delete", "+ react", "R retry", "t thread"] {
            assert!(home_help(&home).contains(key), "{key}: {}", home_help(&home));
        }
        home.open = Some(OpenConversation {
            thread: Some(crate::home::ThreadView::default()),
            ..Default::default()
        });
        assert!(home_help(&home).contains("Esc close thread"));
        home.mode = HomeMode::Compose;
        assert!(home_help(&home).contains("Alt-Enter new line"));
        home.mode = HomeMode::Prompt(PromptKind::NewGroup);
        assert!(home_help(&home).contains("Enter submit"));
        home.mode = HomeMode::ConfirmDelete {
            message_id: "m".into(),
            preview: "p".into(),
        };
        assert_eq!(home_help(&home), "y delete · n keep");
    }

    #[test]
    fn a_hint_line_keeps_every_key_and_action() {
        let line = hint_line("↑/↓ move · Enter open");
        let text: String = line.spans.iter().map(|s| s.content.as_ref()).collect();
        assert_eq!(text, " ↑/↓ move · Enter open");
        let bold: Vec<&str> = line
            .spans
            .iter()
            .filter(|s| s.style.add_modifier.contains(Modifier::BOLD))
            .map(|s| s.content.as_ref())
            .collect();
        assert_eq!(bold, vec!["↑/↓", "Enter"]);
    }
}
