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
        Screen::ApproveEnrollment => render_approve_enrollment(frame, chunks[1], app),
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
                .map_or("Message", |o| o.name.as_str());
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
        Span::raw(app.input.as_str()),
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
            Line::from(vec![
                Span::styled(indent, style),
                Span::styled(row.label.as_str(), style),
            ])
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

    // A run of lost messages (#1312) is one line, as on desktop and mobile. The
    // lines it saves go back on top so the log stays bottom-anchored.
    let body = message_lines(&open.messages[start..end]);
    let saved = (end - start) - body.len();
    let mut lines: Vec<Line> = Vec::with_capacity(viewport);
    for _ in 0..top_pad + saved {
        lines.push(Line::from(""));
    }
    // A hint that older history is still loadable, shown at the very top.
    if start == 0 && !open.at_beginning {
        lines.push(Line::from(Span::styled(
            "  ↑ more history — scroll up to load",
            Style::default().fg(Color::DarkGray),
        )));
    }
    lines.extend(body);

    frame.render_widget(Paragraph::new(lines), inner);
}

/// One line per message, except that a run of consecutive lost-message
/// placeholders (#1312) becomes a single line saying how many.
fn message_lines(messages: &[pollis_core::commands::messages::ChannelMessage]) -> Vec<Line<'_>> {
    let mut out = Vec::with_capacity(messages.len());
    let mut i = 0;
    while i < messages.len() {
        if messages[i].undecryptable {
            let run = messages[i..].iter().take_while(|m| m.undecryptable).count();
            out.push(undecryptable_line(run));
            i += run;
        } else {
            out.push(message_line(&messages[i]));
            i += 1;
        }
    }
    out
}

/// The placeholder line for `count` consecutive lost messages. Nothing
/// readable says who sent them, so no sender is shown.
fn undecryptable_line(count: usize) -> Line<'static> {
    let text = if count == 1 {
        UNDECRYPTABLE_TEXT.to_string()
    } else {
        format!("{count} messages couldn't be decrypted on this device")
    };
    Line::from(vec![
        Span::styled("?", Style::default().fg(Color::DarkGray)),
        Span::raw("  "),
        Span::styled(text, Style::default().fg(Color::Red).add_modifier(Modifier::ITALIC)),
    ])
}

/// Render a single message as `sender  content`, handling the deleted / not-yet-
/// decrypted / edited states honestly.
///
/// Borrows from `m` rather than owning: this runs for every visible message on
/// every frame, and the owned form copied the whole body text each time.
fn message_line(m: &pollis_core::commands::messages::ChannelMessage) -> Line<'_> {
    // A message this device should have been able to read and could not
    // (#1312).
    if m.undecryptable {
        return undecryptable_line(1);
    }
    let sender = m.sender_username.as_deref().unwrap_or(&m.sender_id);
    let sender_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let mut spans = vec![
        Span::styled(sender, sender_style),
        Span::styled("  ", sender_style),
    ];
    if m.deleted_at.is_some() {
        spans.push(Span::styled(
            "(deleted)",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
    } else if let Some(content) = &m.content {
        spans.push(Span::raw(content.as_str()));
        if m.edited_at.is_some() {
            spans.push(Span::raw(" (edited)"));
        }
    } else {
        spans.push(Span::styled(
            "(unable to decrypt)",
            Style::default().fg(Color::Red),
        ));
    }
    Line::from(spans)
}

/// The placeholder line for a lost message — the same words desktop and mobile
/// use.
pub(crate) const UNDECRYPTABLE_TEXT: &str = "This message couldn't be decrypted on this device";

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
    if spans.is_empty() {
        let help = match app.screen {
            Screen::Home => home_help(&app.home.mode),
            Screen::EnrollChoice => "↑/↓ choose · Enter continue · Ctrl-C quit",
            Screen::EnrollWaiting => "Waiting for approval… · Ctrl-C quit",
            Screen::RecoverKey => "Type/paste your Secret Key · Enter recover · Esc back",
            Screen::PendingEnrollments => "↑/↓ move · a approve · r reject · Esc back",
            Screen::ApproveEnrollment => {
                "Type the code from the new device · Enter approve · Esc back"
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use pollis_core::commands::messages::ChannelMessage;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    fn message(undecryptable: bool) -> ChannelMessage {
        ChannelMessage {
            id: "m".to_string(),
            conversation_id: "c".to_string(),
            sender_id: if undecryptable { "sealed" } else { "u" }.to_string(),
            sender_username: None,
            ciphertext: String::new(),
            content: if undecryptable { None } else { Some("hi".to_string()) },
            reply_to_id: None,
            thread_id: None,
            sent_at: "t".to_string(),
            edited_at: None,
            deleted_at: None,
            undecryptable,
        }
    }

    /// A lost message (#1312) renders as the placeholder, with no sender —
    /// never as an empty line or as "(unable to decrypt)" under a fake author.
    #[test]
    fn an_undecryptable_message_renders_the_placeholder() {
        let rendered = text(&message_line(&message(true)));
        assert!(rendered.contains(UNDECRYPTABLE_TEXT), "{rendered}");
        assert!(!rendered.contains("sealed"), "{rendered}");
    }

    /// A run of three is one line that says three; a real message ends it.
    #[test]
    fn a_run_of_lost_messages_is_one_line() {
        let all = vec![message(true), message(true), message(true), message(false), message(true)];
        let lines: Vec<String> = message_lines(&all).iter().map(text).collect();
        assert_eq!(lines.len(), 3, "{lines:?}");
        assert!(lines[0].contains("3 messages couldn't be decrypted"), "{lines:?}");
        assert!(lines[2].contains(UNDECRYPTABLE_TEXT), "{lines:?}");
    }
}
