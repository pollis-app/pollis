//! Rendering. One `render` entry point draws the whole frame from `&App` —
//! ratatui's immediate-mode model means the view is a pure function of state.
//! The auth screens (M1) render a centered card; the signed-in Home screen (M2b)
//! renders the three-pane client (sidebar · messages · status + key hints).

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

/// A solid selection background (no glow — repo rule): reverse-style highlight.
const SEL_BG: Color = Color::Rgb(45, 45, 52);
const SEL_BG_FOCUSED: Color = Color::Rgb(58, 92, 140);

/// The accent a focused pane's border and title wear.
const ACCENT: Color = Color::Rgb(90, 140, 200);

/// The left pane's fixed width, in columns.
const SIDEBAR_WIDTH: u16 = 28;

/// Rows under the Home body: the status line and the key-hint bar. The hints
/// get their own row so they are on screen from the first frame of Home — they
/// used to share one line with the status, so the "You're signed in." that
/// greets a new session hid them until the user happened to press Esc (#1337).
const HOME_FOOTER_ROWS: u16 = 2;

/// Height of the compose / prompt / confirm bar.
const INPUT_BAR_ROWS: u16 = 3;

/// Spinner frames for the sync indicator (solid Braille dots, cycled per refresh).
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
            // header
            Constraint::Length(1),
            // body
            Constraint::Min(1),
            // status (+ key hints on Home)
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

/// Where the Home screen's panes go inside its body area. One function for the
/// renderer and [`message_viewport`], so the size the key handler scrolls by
/// is the size that is drawn.
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

/// The message pane's inner `(width, height)` for a full-frame `area` — what
/// the input loop records so page keys jump a screen and selection keeps the
/// highlighted message on screen. Mirrors [`render`]: header (1) and footer
/// off the frame, the input bar when one is open, the sidebar, then the
/// pane's own border.
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
    // On Home, name the open conversation and show the sync indicator on the right.
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
/// right, plus a bottom input bar (compose, prompt or delete confirmation) when
/// the screen is in an input mode — the desktop app's "replace the input bar"
/// pattern, never a modal overlay.
fn render_home(frame: &mut Frame, area: Rect, app: &App) {
    let layout = home_layout(area, &app.home);
    if let Some(input) = layout.input {
        render_input_bar(frame, input, app);
    }
    render_sidebar(frame, layout.sidebar, &app.home);
    render_messages(frame, layout.messages, app);
}

/// A pane's frame. Focus has to be unmistakable — it decides what ↑/↓ and
/// Enter do, and an invisible focus is how a user ended up scrolling the
/// message pane while trying to reach a DM they had just started (#1337) — so
/// the focused pane gets a thick accent border and an accent title bar, and
/// the other a thin dim border. Solid colours only, no glow.
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

/// The bottom input bar: a solid-bordered line labeled with what it's collecting
/// (compose = the conversation, reply, edit; prompt = the action; or the delete
/// question), showing the live buffer with a caret. No glow — a plain accent
/// border.
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
        // A line break shows as ⏎; when the text outgrows the bar, the end —
        // where the caret is — stays in view.
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
            // Rows are only ever depth 0 (group/section) or 1 (child) — see
            // `home::build_sidebar_rows` — so the indent needs no allocation.
            let indent = if row.depth == 0 { "" } else { "  " };
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
            // Cut to the pane with an ellipsis, so a long name says it is long
            // rather than being clipped mid-word.
            let label = fit_width(&row.label, width.saturating_sub(indent.len()));
            Line::from(vec![Span::styled(indent, style), Span::styled(label, style)])
        })
        .collect();

    frame.render_widget(Paragraph::new(lines), inner);
}

/// Existing device: type the verification code shown on the NEW device (#1096).
///
/// Deliberately shows no code of its own — there is none to show, because
/// nothing fetched one. What the user types goes on one side of the comparison
/// and the code Rust derives from the ephemeral key IT fetched goes on the
/// other, so a substituted key fails here instead of passing unnoticed.
fn render_approve_enrollment(frame: &mut Frame, area: Rect, app: &App) {
    use pollis_core::commands::device_enrollment::ENROLLMENT_SAS_LEN;

    let card = centered(area, 62, 11);
    let device = app
        .approvals
        .current()
        .map_or("?", |r| r.new_device_id.as_str());

    let typed = app.input.chars().count();
    // Underscores for the characters still to come, so the width is obvious.
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

    // Lay the displayed messages out at the pane's width, then show a
    // bottom-anchored window of display lines (newest at the bottom).
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
    // A hint that older history is still loadable, in the padding above the
    // oldest loaded message (never in place of a message line).
    if start == 0 && top_pad > 0 && !open.at_beginning && open.thread.is_none() {
        lines[top_pad - 1] = Line::from(Span::styled(
            "  ↑ more history — scroll up to load",
            Style::default().fg(Color::DarkGray),
        ));
    }
    lines.extend(all.into_iter().skip(start).take(end - start));

    frame.render_widget(Paragraph::new(lines), inner);
}

/// First-device signup: the Secret Key / Emergency Kit card (#1337). Shown
/// once; the user saves it (optionally as a file with `s`), then types it back.
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

/// The centered-card auth body (M1 screens).
fn render_auth_body(frame: &mut Frame, area: Rect, app: &App) {
    let card = centered(area, 60, 9);

    let (title, prompt, value) = match app.screen {
        Screen::Booting => ("Starting up", "Checking for an existing session…", String::new()),
        Screen::Email => ("Sign in", "Email:", app.input.clone()),
        Screen::Otp => ("Verify", "Code:", app.input.clone()),
        // Rendered by render_save_secret_key; never shown in a plain card.
        Screen::SaveSecretKey => ("Secret Key", "", String::new()),
        Screen::SetPin => ("Set PIN", "New PIN:", mask(&app.input)),
        Screen::Unlock => ("Unlock", "PIN:", mask(&app.input)),
        // The Secret Key is long and pasted, not memorized — show it plainly so
        // the user can spot a typo before submitting.
        Screen::RecoverKey => ("Recover", "Secret Key:", app.input.clone()),
        // Rendered by their own functions; these arms keep the match exhaustive.
        Screen::EnrollChoice => ("Enroll", "", String::new()),
        Screen::EnrollWaiting => ("Enroll", "", String::new()),
        Screen::PendingEnrollments => ("Enrollments", "", String::new()),
        Screen::ApproveEnrollment => ("Approve", "", String::new()),
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

/// Existing device: the full-screen "Pending device enrollments" list.
///
/// Each row shows the requesting device id and nothing else. It used to show a
/// verification code, fetched from the Delivery Service, and `a` approved with
/// that same value — so the comparison the code exists for was performed by
/// nobody, and a DS that swapped the new device's ephemeral key and its stored
/// code to match passed on one keystroke (#1096). `a` now opens
/// [`render_approve_enrollment`], where the approver types what the NEW device
/// is showing.
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

    // Home: the status on the first row, the key hints ALWAYS on the second.
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

/// The key hints for the Home screen, for the pane that has focus and the
/// input mode — the in-app discovery surface for every Home key.
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::home::{OpenConversation, PromptKind};

    /// The hint bar names the keys of whichever pane has focus.
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

    /// The hint line bolds each key and keeps every word.
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
