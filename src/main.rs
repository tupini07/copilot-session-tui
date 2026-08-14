mod app;
mod config;
mod debug_keys;
mod events;
mod input;
mod mux;
mod mux_input;
mod scratchpad;
mod session;
mod terminal_pane;
mod text;
mod ui;
mod updater;

use anyhow::Result;
use clap::{Parser, Subcommand};
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Write};
use std::path::PathBuf;

use app::{App, NewSessionRequest};
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

    /// Run sessions as panes inside CST for this invocation, overriding the `mux` config
    #[arg(long, overrides_with = "no_mux")]
    mux: bool,

    /// Launch sessions in the terminal and exit, overriding the `mux` config
    #[arg(long, overrides_with = "mux")]
    no_mux: bool,

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
    /// Report how this terminal delivers key presses (used to pick a mux prefix key)
    #[command(hide = true)]
    DebugKeys,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    // Handle subcommands
    match &cli.command {
        Some(Commands::Init { shell }) => {
            print_shell_init(shell);
            return Ok(());
        }
        Some(Commands::DebugKeys) => {
            return debug_keys::run();
        }
        None => {}
    }

    let copilot_home = cli.copilot_home.unwrap_or_else(loader::copilot_home);

    // Load sessions (may be empty — the TUI still lets you start a new session)
    let sessions = loader::load_sessions(&copilot_home).unwrap_or_default();

    let mut user_config = config::load();
    // CLI flags win for this invocation only; they are never written back to disk.
    let mux_on_disk = user_config.mux;
    if cli.mux {
        user_config.mux = true;
    } else if cli.no_mux {
        user_config.mux = false;
    }

    let mut app = App::new(sessions, user_config);
    app.mux_on_disk = mux_on_disk;

    // Start background update check
    app.update_receiver = Some(updater::check_for_updates_async());

    // Resolving the Copilot binary costs ~400ms; do it off-thread so the first session
    // does not pay for it on the UI thread.
    manager::warm_copilot_lookup();

    // Record the launch directory; auto-filters to its project when enabled
    if let Ok(cwd) = std::env::current_dir() {
        app.set_cwd_context(cwd.to_string_lossy().to_string(), cli.auto_filter);
    }

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(
        stdout,
        EnterAlternateScreen,
        EnableMouseCapture,
        EnableBracketedPaste
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run app
    let result = run_app(&mut terminal, &mut app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste
    )?;
    terminal.show_cursor()?;
    app.terminal.shutdown();

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
    if let Some(request) = app.should_new_session.take() {
        match request {
            NewSessionRequest::Normal { cwd } => {
                eprintln!("Starting new session in {}...", &cwd);
                last_dir = Some(cwd.clone());
                manager::start_new_session(&cwd, &app.config)?;
            }
            NewSessionRequest::Worktree {
                source_project,
                branch,
                config,
            } => {
                let worktree = manager::start_worktree_session(
                    &source_project,
                    &branch,
                    &config,
                    &app.config,
                )?;
                last_dir = Some(worktree.to_string_lossy().to_string());
            }
        }
    }

    // If user quit without entering a session but has an active project filter, use that
    // (skip when the filter is just the project we were launched from — no cd needed)
    if last_dir.is_none() {
        // In mux mode the session ran inside CST, so the last focused pane's directory is
        // the most useful place for the shell wrapper to land.
        last_dir = app.exit_dir.take();
    }
    if last_dir.is_none() {
        if let Some(ref project) = app.project_filter {
            let is_launch_project = app
                .cwd_project
                .as_ref()
                .is_some_and(|p| p.eq_ignore_ascii_case(project));
            if !is_launch_project {
                last_dir = Some(project.clone());
            }
        }
    }

    // Write last directory to file if --last-dir-file was provided
    if let (Some(ref path), Some(ref dir)) = (&cli.last_dir_file, &last_dir) {
        let _ = std::fs::write(path, dir);
    }
    Ok(())
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    // In mux mode a dedicated thread feeds terminal events into the same channel as PTY
    // output, so the loop can wait on both at once instead of polling.
    let terminal_events = app.mux.as_ref().map(|mux| {
        let sender = mux.events.clone();
        std::thread::spawn(move || loop {
            match crossterm::event::read() {
                Ok(event) => {
                    if sender.send(mux::MuxEvent::Term(event)).is_err() {
                        return;
                    }
                }
                Err(_) => return,
            }
        })
    });
    drop(terminal_events);

    loop {
        let size = terminal.size()?;
        let terminal_height = if matches!(app.view, app::View::List) && app.terminal.is_visible() {
            ui::terminal_panel_height(size.height.saturating_sub(3))
        } else {
            0
        };
        update_layout_metrics(app, size.height, terminal_height);

        if let Some(terminal_pane) = app.terminal.active_mut().filter(|_| terminal_height > 0) {
            if let Err(error) = terminal_pane.resize(
                1,
                size.height.saturating_sub(terminal_height + 1),
                terminal_height.saturating_sub(2),
                size.width.saturating_sub(2),
            ) {
                app.status_message = Some(format!("Terminal resize failed: {error}"));
            }
        }

        if app.mux.is_some() {
            // Panes occupy the full screen minus the one-line pane status bar.
            let rows = size.height.saturating_sub(1).max(1);
            let cols = size.width.max(1);
            if app.pane_size != (rows, cols) {
                app.pane_size = (rows, cols);
                if let Some(mux) = app.mux.as_mut() {
                    mux.resize_all(rows, cols);
                }
            }
        }

        input::maybe_load_details(app);
        app.poll_update();

        if let Some(scratchpad) = app.scratchpad.as_mut() {
            if let Err(error) = scratchpad.autosave_if_due() {
                scratchpad.status_message = Some(format!("Autosave failed: {error}"));
            }
        }

        let mut host_sequences = app.terminal.drain_host_sequences();
        host_sequences.append(&mut app.host_sequences);
        if !host_sequences.is_empty() {
            for sequence in host_sequences {
                terminal.backend_mut().write_all(&sequence)?;
            }
            terminal.backend_mut().flush()?;
        }

        terminal.draw(|f| ui::draw(f, app))?;

        // Runs after the draw above so the "creating worktree…" notice is already on
        // screen before this blocks on Git.
        if let Some(pending) = app.pending_worktree.take() {
            input::run_pending_worktree(app, pending);
            continue;
        }

        if app.mux.is_some() {
            pump_mux(app)?;
        } else {
            input::handle_input(app)?;
        }

        if app.should_quit
            || app.should_resume.is_some()
            || app.should_update
            || app.should_new_session.is_some()
        {
            break;
        }
    }

    // Without a daemon, panes are children of this process and must be reaped. Capture the
    // exit directory first — shutdown drops the panes that hold it.
    if let Some(mux) = app.mux.as_mut() {
        app.exit_dir = mux
            .focused_cwd()
            .map(|path| path.to_string_lossy().to_string());
        let _ = mux.shutdown();
    }

    Ok(())
}

fn update_layout_metrics(app: &mut App, height: u16, terminal_height: u16) {
    // visible_rows must match the session_list take() count:
    // inner height = total height - 6 (title + borders + status)
    // each item = 2 lines normally, 1 line when project filter is active
    let lines_per_item = if app.project_filter.is_some() { 1 } else { 2 };
    app.visible_rows =
        (height as usize).saturating_sub(6 + terminal_height as usize) / lines_per_item;

    // Project popup visible rows: popup is ~25-80% height, minus borders (2), search (1), separator (1)
    let popup_percent = 80u16
        .min(25u16.max(
            (((app.unique_projects.len() + 6).min(20) as f32 / height as f32) * 100.0) as u16,
        ));
    let popup_height = (height as usize * popup_percent as usize) / 100;
    app.project_visible_rows = popup_height.saturating_sub(4); // borders + search + separator
}

/// Wait for the next terminal or PTY event and apply it.
///
/// Output arriving in many small chunks is drained in one go so a chatty child cannot
/// force a repaint per chunk.
fn pump_mux(app: &mut App) -> Result<()> {
    let Some(mux) = app.mux.as_ref() else {
        return Ok(());
    };

    // A blank pane is showing the startup spinner, which needs frequent repaints; an
    // established pane can idle until something actually happens.
    let animating = mux
        .panes
        .iter()
        .any(|pane| pane.is_running() && pane.is_blank());
    let timeout = if animating { 100 } else { 250 };

    let first = match mux
        .receiver
        .recv_timeout(std::time::Duration::from_millis(timeout))
    {
        Ok(event) => event,
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(()),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            app.should_quit = true;
            return Ok(());
        }
    };

    let mut pending = vec![first];
    while let Ok(event) = app.mux.as_ref().expect("mux present").receiver.try_recv() {
        pending.push(event);
        if pending.len() >= 256 {
            break;
        }
    }

    for event in pending {
        match event {
            mux::MuxEvent::Term(event) => apply_terminal_event(app, event)?,
            other => {
                mux_input::handle_mux_event(app, other);
            }
        }
    }

    Ok(())
}

fn apply_terminal_event(app: &mut App, event: crossterm::event::Event) -> Result<()> {
    match app.view {
        app::View::Attached(_) => {
            mux_input::handle_attached_event(app, event);
            Ok(())
        }
        app::View::List => input::handle_terminal_event(app, event),
    }
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
