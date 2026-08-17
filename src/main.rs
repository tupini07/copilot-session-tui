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
mod windows_terminal;
mod workspace_state;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture},
    execute,
    terminal::{
        disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen, SetTitle,
    },
};
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

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

    /// Open an exact session directly instead of showing the session list
    #[arg(long, value_name = "ID", conflicts_with = "open_favorites")]
    session: Option<String>,

    /// Open each inactive favorite in a Windows Terminal tab
    #[arg(long, conflicts_with = "session")]
    open_favorites: bool,

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

    let copilot_home = cli
        .copilot_home
        .clone()
        .unwrap_or_else(loader::copilot_home);
    // The path controls both CST's session loader and the Copilot process it resumes.
    // Generated favorite tabs receive an explicit path, so export it before any child
    // processes or background threads are started.
    if cli.copilot_home.is_some() {
        std::env::set_var("COPILOT_HOME", &copilot_home);
    }

    // Normal picker startup tolerates an absent session directory so users can still
    // create their first session. Targeted commands need the load error to be explicit.
    let sessions = match loader::load_sessions(&copilot_home) {
        Ok(sessions) => sessions,
        Err(error) if cli.session.is_some() || cli.open_favorites => {
            return Err(error).context("Failed to load Copilot sessions");
        }
        Err(_) => Vec::new(),
    };
    let startup_session = cli
        .session
        .as_deref()
        .map(|id| resolve_startup_session(&sessions, id))
        .transpose()?;

    let mut user_config = config::load();
    // CLI flags win for this invocation only; they are never written back to disk.
    let mux_on_disk = user_config.mux;
    if cli.mux {
        user_config.mux = true;
    } else if cli.no_mux {
        user_config.mux = false;
    }

    if cli.open_favorites {
        let mux_override = if cli.mux {
            Some(true)
        } else if cli.no_mux {
            Some(false)
        } else {
            None
        };
        return windows_terminal::open_favorites(
            &sessions,
            &user_config,
            &copilot_home,
            mux_override,
        );
    }

    if let Some(session) = startup_session.as_ref().filter(|_| !user_config.mux) {
        eprintln!(
            "Resuming session {} in {}...",
            short_session_id(&session.id),
            session.cwd
        );
        manager::resume_session(&session.id, &session.cwd, &user_config)?;
        if let Some(path) = &cli.last_dir_file {
            let _ = std::fs::write(path, &session.cwd);
        }
        return Ok(());
    }

    let mut app = App::new(sessions, user_config);
    app.mux_on_disk = mux_on_disk;
    app.copilot_home = copilot_home;

    // Start background update check
    app.update_receiver = Some(updater::check_for_updates_async());

    // Resolving the Copilot binary costs ~400ms; do it off-thread so the first session
    // does not pay for it on the UI thread.
    manager::warm_copilot_lookup();

    // A direct session should detach into its own project list even when invoked from
    // elsewhere. Normal startup continues to use the process launch directory.
    let cwd_context = startup_session
        .as_ref()
        .map(|session| existing_or_current_dir(&session.cwd))
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|cwd| cwd.to_string_lossy().to_string())
        });
    if let Some(cwd) = cwd_context {
        app.set_cwd_context(cwd, cli.auto_filter);
    }

    if let Some(session) = startup_session {
        let cwd = existing_or_current_dir(&session.cwd);
        app.attach_session(&session.id, &cwd, session.title)?;
        mux_input::sync_workspace_panels(&mut app);
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
        DisableBracketedPaste,
        SetTitle("")
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
        eprintln!(
            "Resuming session {} in {}...",
            short_session_id(&session_id),
            &cwd
        );
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartupSession {
    id: String,
    cwd: String,
    title: String,
}

fn resolve_startup_session(sessions: &[session::Session], id: &str) -> Result<StartupSession> {
    let session = sessions
        .iter()
        .find(|session| session.id == id)
        .with_context(|| format!("Session '{id}' was not found"))?;
    if session.is_active {
        anyhow::bail!("Session '{}' is already active", session.display_name());
    }
    Ok(StartupSession {
        id: session.id.clone(),
        cwd: session.cwd.clone(),
        title: session.display_name().to_string(),
    })
}

fn short_session_id(id: &str) -> &str {
    id.get(..8).unwrap_or(id)
}

fn existing_or_current_dir(cwd: &str) -> String {
    if Path::new(cwd).is_dir() {
        return cwd.to_string();
    }
    std::env::current_dir()
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_else(|_| cwd.to_string())
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
    let mut terminal_title = String::new();

    loop {
        app.collapse_stopped_terminals();

        let desired_title = match app.view {
            app::View::Attached(_) => app
                .mux
                .as_ref()
                .and_then(|mux| mux.focused_pane())
                .map(|pane| pane.title.as_str())
                .unwrap_or("Copilot Session Manager"),
            app::View::List => "Copilot Session Manager",
        };
        if terminal_title != desired_title {
            terminal_title.clear();
            terminal_title.push_str(desired_title);
            execute!(terminal.backend_mut(), SetTitle(&terminal_title))?;
        }

        let size = terminal.size()?;
        let attached_layout = matches!(app.view, app::View::Attached(_)).then(|| {
            ui::attached_layout(
                ratatui::layout::Rect::new(0, 0, size.width, size.height),
                app.attached_scratchpad_visible(),
                app.attached_terminal_visible(),
            )
        });
        update_layout_metrics(app, size.height);

        if let Some(layout) = attached_layout {
            app.workspace_areas = app::WorkspaceAreas {
                chat: layout.chat,
                scratchpad: layout.scratchpad,
                terminal: layout.terminal,
            };
            if let (Some(area), Some(terminal_pane)) = (layout.terminal, app.terminal.active_mut())
            {
                if let Err(error) = terminal_pane.resize(
                    area.x.saturating_add(1),
                    area.y.saturating_add(1),
                    area.height.saturating_sub(2),
                    area.width.saturating_sub(2),
                ) {
                    app.status_message = Some(format!("Terminal resize failed: {error}"));
                }
            }

            let chat = layout.chat.inner(ratatui::layout::Margin {
                horizontal: 1,
                vertical: 1,
            });
            let rows = chat.height.max(1);
            let cols = chat.width.max(1);
            if app.pane_size != (rows, cols) || app.pane_origin != (chat.x, chat.y) {
                app.pane_size = (rows, cols);
                app.pane_origin = (chat.x, chat.y);
                if let Some(mux) = app.mux.as_mut() {
                    mux.resize_all_at(chat.x, chat.y, rows, cols);
                }
            }
        } else {
            if app.mux.is_some() {
                let rows = size.height.saturating_sub(1).max(1);
                let cols = size.width.max(1);
                if app.pane_size != (rows, cols) || app.pane_origin != (0, 0) {
                    app.pane_size = (rows, cols);
                    app.pane_origin = (0, 0);
                    if let Some(mux) = app.mux.as_mut() {
                        mux.resize_all(rows, cols);
                    }
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

    if let Some(scratchpad) = app.scratchpad.as_mut() {
        scratchpad.save()?;
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

fn update_layout_metrics(app: &mut App, height: u16) {
    // visible_rows must match the session_list take() count:
    // inner height = total height - 6 (title + borders + status)
    // each item = 2 lines normally, 1 line when project filter is active
    let lines_per_item = if app.project_filter.is_some() { 1 } else { 2 };
    app.visible_rows = (height as usize).saturating_sub(6) / lines_per_item;

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

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn session(id: &str, name: &str, active: bool) -> session::Session {
        session::Session {
            id: id.to_string(),
            cwd: r"C:\work\project".to_string(),
            project_root: r"C:\work\project".to_string(),
            summary: Some(name.to_string()),
            created_at: None,
            updated_at: Some(Utc.timestamp_opt(1, 0).unwrap()),
            is_active: active,
            dir_path: PathBuf::new(),
            edited_files: Vec::new(),
            last_user_message: None,
            turn_count: 0,
            tool_call_count: 0,
        }
    }

    #[test]
    fn direct_session_options_parse_and_conflict() {
        let direct = Cli::try_parse_from(["cst", "--session", "session-1", "--mux"]).unwrap();
        assert_eq!(direct.session.as_deref(), Some("session-1"));
        assert!(direct.mux);

        let favorites = Cli::try_parse_from(["cst", "--open-favorites", "--no-mux"]).unwrap();
        assert!(favorites.open_favorites);
        assert!(favorites.no_mux);

        assert!(
            Cli::try_parse_from(["cst", "--session", "session-1", "--open-favorites"]).is_err()
        );
    }

    #[test]
    fn startup_session_requires_an_exact_inactive_match() {
        let sessions = vec![
            session("session-1", "First", false),
            session("session-2", "Second", true),
        ];

        assert_eq!(
            resolve_startup_session(&sessions, "session-1").unwrap(),
            StartupSession {
                id: "session-1".to_string(),
                cwd: r"C:\work\project".to_string(),
                title: "First".to_string(),
            }
        );
        assert!(resolve_startup_session(&sessions, "session").is_err());
        assert!(resolve_startup_session(&sessions, "session-2")
            .unwrap_err()
            .to_string()
            .contains("already active"));
    }

    #[test]
    fn short_session_ids_do_not_panic() {
        assert_eq!(short_session_id("short"), "short");
        assert_eq!(short_session_id("123456789"), "12345678");
    }

    #[test]
    fn direct_mux_startup_falls_back_when_saved_directory_is_missing() {
        let directory = tempfile::tempdir().unwrap();
        assert_eq!(
            existing_or_current_dir(&directory.path().to_string_lossy()),
            directory.path().to_string_lossy()
        );

        let fallback = existing_or_current_dir(r"Z:\definitely\missing\cst-session");
        assert!(Path::new(&fallback).is_dir());
    }
}
