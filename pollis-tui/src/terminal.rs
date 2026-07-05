//! Terminal lifecycle: enter the alternate screen + raw mode on construction,
//! restore the user's terminal on drop. The RAII guard guarantees the terminal
//! is restored on *every* exit path — clean quit, `?`-propagated error, or panic
//! — so the user is never left in a broken raw-mode shell.

use std::io::{self, Stdout};

use anyhow::Result;
use crossterm::{
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};

pub type Tui = Terminal<CrosstermBackend<Stdout>>;

/// Owns the terminal's raw/alt-screen state. Dropping it restores the terminal.
pub struct TerminalGuard {
    pub terminal: Tui,
}

impl TerminalGuard {
    pub fn enter() -> Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self { terminal })
    }

    /// Best-effort restore, factored out so both `Drop` and the panic hook can
    /// call it. Ignores errors — there is nothing useful to do if restoring the
    /// terminal itself fails while we are already tearing down.
    pub fn restore() {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        Self::restore();
    }
}
