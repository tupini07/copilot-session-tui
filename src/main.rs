mod app;
mod config;
mod events;
mod input;
mod session;
mod ui;
mod updater;

use anyhow::Result;
use clap::{Parser, Subcommand};
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

    /// Write the session's project directory to this file on exit (for shell cd wrapper)
    #[arg(long)]
    last_dir_file: Option<PathBuf>,

    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Output shell integration script (add to your shell config for auto-cd on exit)
    Init {
        /// Shell type
        #[arg(value_parser = ["bash", "zsh", "powershell"])]
        shell: String,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle subcommands
    if let Some(Commands::Init { shell }) = &cli.command {
        print_shell_init(shell);
        return Ok(());
    }

    let copilot_home = cli
        .copilot_home
        .unwrap_or_else(loader::copilot_home);

    // Load sessions
    let sessions = loader::load_sessions(&copilot_home)?;

    if sessions.is_empty() {
        eprintln!("No Copilot sessions found in {}", copilot_home.display());
        return Ok(());
    }

    let mut app = App::new(sessions, config::load());

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

    // Track the directory to write to --last-dir-file
    let mut last_dir: Option<String> = None;

    // Resume session if requested
    if let Some((session_id, cwd)) = app.should_resume {
        eprintln!("Resuming session {} in {}...", &session_id[..8], &cwd);
        last_dir = Some(cwd.clone());
        manager::resume_session(&session_id, &cwd, &app.config)?;
    }

    // Start new session if requested
    if let Some(cwd) = app.should_new_session {
        eprintln!("Starting new session in {}...", &cwd);
        last_dir = Some(cwd.clone());
        manager::start_new_session(&cwd, &app.config)?;
    }

    // If user quit without entering a session but has an active project filter, use that
    if last_dir.is_none() {
        if let Some(ref project) = app.project_filter {
            last_dir = Some(project.clone());
        }
    }

    // Write last directory to file if --last-dir-file was provided
    if let (Some(ref path), Some(ref dir)) = (&cli.last_dir_file, &last_dir) {
        let _ = std::fs::write(path, dir);
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

        // Project popup visible rows: popup is ~25-80% height, minus borders (2), search (1), separator (1)
        let popup_percent = 80u16.min(25u16.max(
            (((app.unique_projects.len() + 6).min(20) as f32 / size.height as f32) * 100.0) as u16,
        ));
        let popup_height = (size.height as usize * popup_percent as usize) / 100;
        app.project_visible_rows = popup_height.saturating_sub(4); // borders + search + separator

        // Load details for selected session
        input::maybe_load_details(app);

        // Poll for update check results
        app.poll_update();

        // Draw
        terminal.draw(|f| ui::draw(f, app))?;

        // Handle input
        input::handle_input(app)?;

        if app.should_quit || app.should_resume.is_some() || app.should_update || app.should_new_session.is_some() {
            break;
        }
    }

    Ok(())
}

fn print_shell_init(shell: &str) {
    match shell {
        "bash" | "zsh" => {
            print!(
                r#"cst() {{
    local tmpfile
    tmpfile=$(mktemp)
    command copilot-session-tui --last-dir-file="$tmpfile" "$@"
    local last_dir
    last_dir=$(cat "$tmpfile" 2>/dev/null)
    rm -f "$tmpfile"
    if [ -n "$last_dir" ] && [ -d "$last_dir" ]; then
        cd "$last_dir" || true
    fi
}}
"#
            );
        }
        "powershell" => {
            print!(
                r#"function cst {{
    $tmpfile = [System.IO.Path]::GetTempFileName()
    copilot-session-tui --last-dir-file="$tmpfile" @args
    $lastDir = Get-Content $tmpfile -ErrorAction SilentlyContinue
    Remove-Item $tmpfile -ErrorAction SilentlyContinue
    if ($lastDir -and (Test-Path $lastDir)) {{
        Set-Location $lastDir
    }}
}}
"#
            );
        }
        _ => unreachable!(),
    }
}
