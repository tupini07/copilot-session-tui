mod app;
mod events;
mod input;
mod session;
mod ui;
mod updater;

use anyhow::Result;
use clap::Parser;
use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io;
use std::path::PathBuf;

use app::App;
use session::loader;
use session::manager;

#[derive(Parser)]
#[command(name = "copilot-session-tui")]
#[command(about = "A TUI for managing GitHub Copilot CLI sessions")]
struct Cli {
    /// Path to the Copilot config directory (default: ~/.copilot)
    #[arg(long)]
    copilot_home: Option<PathBuf>,

    /// Auto-filter to sessions from the current directory
    #[arg(long, default_value = "true")]
    auto_filter: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    let copilot_home = cli
        .copilot_home
        .unwrap_or_else(loader::copilot_home);

    // Load sessions
    let sessions = loader::load_sessions(&copilot_home)?;

    if sessions.is_empty() {
        eprintln!("No Copilot sessions found in {}", copilot_home.display());
        return Ok(());
    }

    let mut app = App::new(sessions);

    // Start background update check
    app.update_receiver = Some(updater::check_for_updates_async());

    // Auto-filter to current directory if enabled
    if cli.auto_filter {
        if let Ok(cwd) = std::env::current_dir() {
            let cwd_str = cwd.to_string_lossy().to_string();
            // Resolve project root (handles worktrees) for matching
            let resolved = loader::resolve_project_root_pub(&cwd_str);
            if app.unique_projects.iter().any(|p| p.eq_ignore_ascii_case(&resolved)) {
                app.set_project_filter(Some(resolved));
            }
        }
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run app
    let result = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    // Handle result
    result?;

    // Perform update if requested (after terminal is restored)
    if app.should_update {
        eprintln!("Updating copilot-session-tui...");
        if let Err(e) = updater::perform_update() {
            eprintln!("Update failed: {}", e);
        }
        return Ok(());
    }

    // Resume session if requested
    if let Some((session_id, cwd)) = app.should_resume {
        eprintln!("Resuming session {} in {}...", &session_id[..8], &cwd);
        manager::resume_session(&session_id, &cwd)?;
    }

    Ok(())
}

fn run_app(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> Result<()> {
    loop {
        // Update visible rows based on terminal size
        let size = terminal.size()?;
        // visible_rows must match the session_list take() count:
        // inner height = total height - 6 (title + borders + status)
        // each item = 2 lines normally, 1 line when project filter is active
        let lines_per_item = if app.project_filter.is_some() { 1 } else { 2 };
        app.visible_rows = (size.height as usize).saturating_sub(6) / lines_per_item;

        // Load details for selected session
        input::maybe_load_details(app);

        // Poll for update check results
        app.poll_update();

        // Draw
        terminal.draw(|f| ui::draw(f, app))?;

        // Handle input
        input::handle_input(app)?;

        if app.should_quit || app.should_resume.is_some() || app.should_update {
            break;
        }
    }

    Ok(())
}
