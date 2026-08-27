mod app;
mod config;
mod debug_keys;
mod events;
mod github;
mod host_terminal;
mod input;
mod mux;
mod mux_input;
mod notifications;
mod paste;
mod scratchpad;
#[cfg(feature = "screenshots")]
mod screenshots;
mod session;
mod snippets;
mod terminal_pane;
mod text;
mod theme;
mod ui;
mod updater;
mod windows_terminal;
mod workspace_state;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{
        DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste,
        EnableFocusChange, EnableMouseCapture,
    },
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
#[command(version)]
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

    /// Internal helper used to replace the installed executable without stopping panes
    #[arg(long, value_name = "VERSION", hide = true)]
    install_update_helper: Option<String>,

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
    /// Regenerate the README screenshots from invented sessions
    #[cfg(feature = "screenshots")]
    #[command(hide = true)]
    Screenshots {
        /// Directory to write the SVG files into
        #[arg(default_value = "docs/img")]
        out_dir: PathBuf,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(version) = cli.install_update_helper.as_deref() {
        updater::install_update_helper(version)?;
        return Ok(());
    }
    updater::cleanup_old_update_files();

    // Handle subcommands
    match &cli.command {
        Some(Commands::Init { shell }) => {
            print_shell_init(shell);
            return Ok(());
        }
        Some(Commands::DebugKeys) => {
            return debug_keys::run();
        }
        #[cfg(feature = "screenshots")]
        Some(Commands::Screenshots { out_dir }) => {
            return screenshots::run(out_dir);
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

    let mut user_config = config::load();
    // CLI flags win for this invocation only; they are never written back to disk.
    let mux_on_disk = user_config.mux;
    if cli.mux {
        user_config.mux = true;
    } else if cli.no_mux {
        user_config.mux = false;
    }

    // Direct resume is an exact id and favorites are already ids, so neither path needs
    // to enumerate the complete catalog. Normal startup paints favorites, then discovers
    // the rest in the background.
    let mut session_load_receiver = None;
    let sessions = if let Some(session_id) = cli.session.as_deref() {
        vec![loader::load_session(&copilot_home, session_id)
            .context("Failed to load Copilot session")?
            .with_context(|| format!("Session '{session_id}' was not found"))?]
    } else {
        let favorites = match loader::load_sessions_by_ids(&copilot_home, &user_config.favorites) {
            Ok(sessions) => sessions,
            Err(error) if cli.open_favorites => {
                return Err(error).context("Failed to load favorite Copilot sessions");
            }
            Err(_) => Vec::new(),
        };
        if !cli.open_favorites {
            session_load_receiver = Some(loader::load_sessions_async(copilot_home.clone()));
        }
        favorites
    };
    let startup_session = cli
        .session
        .as_deref()
        .map(|id| resolve_startup_session(&sessions, id))
        .transpose()?;

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
    if let Some(receiver) = session_load_receiver {
        app.begin_session_load(receiver);
    }

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
        EnableBracketedPaste,
        EnableFocusChange
    )?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run app
    let result = run_app(&mut terminal, &mut app);
    // A helper process performs replacement independently, but waiting here ensures a
    // normal exit never abandons the user without the final install result.
    app.wait_for_update_install();

    // Restore terminal
    disable_raw_mode()?;
    terminal
        .backend_mut()
        .write_all(host_terminal::CLEAR_PROGRESS)?;
    terminal.backend_mut().flush()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture,
        DisableBracketedPaste,
        DisableFocusChange,
        SetTitle("")
    )?;
    terminal.show_cursor()?;
    app.terminal.shutdown();

    // Handle result
    result?;

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

fn spawn_config_watcher(
    events: std::sync::mpsc::Sender<mux::MuxEvent>,
    path: PathBuf,
    applied: std::sync::Arc<std::sync::Mutex<Option<config::ConfigRevision>>>,
    interval: std::time::Duration,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        std::thread::sleep(interval);
        let Ok(next) = config::config_revision(&path) else {
            continue;
        };
        if applied
            .lock()
            .ok()
            .is_some_and(|revision| *revision == Some(next))
        {
            continue;
        }
        if events.send(mux::MuxEvent::ConfigChanged).is_err() {
            return;
        }
    })
}

fn run_app(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>, app: &mut App) -> Result<()> {
    let config_path = config::config_path();
    // The watcher compares against the revision actually applied by App, so a quick
    // A -> B -> A reversion cannot be hidden by a stale watcher-local baseline.
    app.request_config_reload();
    let _config_watcher = app.mux.as_ref().map(|mux| {
        spawn_config_watcher(
            mux.events.clone(),
            config_path.clone(),
            app.config_revision_handle(),
            std::time::Duration::from_secs(1),
        )
    });
    let mut last_non_mux_config_check = std::time::Instant::now();
    // In mux mode a dedicated thread feeds terminal events into the same channel as PTY
    // output, so the loop can wait on both at once instead of polling. Reading through
    // `paste` rebuilds the paste events Windows delivers as bare keystrokes; because the
    // thread does nothing but read, a queued burst can only have come from a paste.
    let terminal_events = app.mux.as_ref().map(|mux| {
        let sender = mux.events.clone();
        std::thread::spawn(move || loop {
            match paste::read_events() {
                Ok(events) => {
                    for event in events {
                        if sender.send(mux::MuxEvent::Term(event)).is_err() {
                            return;
                        }
                    }
                }
                Err(_) => return,
            }
        })
    });
    drop(terminal_events);
    let mut terminal_title = String::new();
    let mut repaint = true;
    let mut last_paint = std::time::Instant::now();

    loop {
        repaint |= app.poll_config_reload();
        if app.mux.is_none()
            && last_non_mux_config_check.elapsed() >= std::time::Duration::from_secs(1)
        {
            if let Ok(next) = config::config_revision(&config_path) {
                if !app.config_revision_is_applied(next) {
                    repaint |= app.request_config_reload();
                }
            }
            last_non_mux_config_check = std::time::Instant::now();
        }
        app.collapse_stopped_terminals();

        let desired_title = desired_terminal_title(app);
        if terminal_title != desired_title {
            terminal_title.clear();
            terminal_title.push_str(&desired_title);
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
        app.poll_session_load();
        app.poll_update();
        app.poll_notifications();
        app.poll_github();
        // Covers every route into the Files tab — keys, mouse, or opening
        // straight onto it — rather than each one separately.
        if app.github_files_tab_active() {
            app.ensure_github_patches();
        }
        if matches!(app.view, app::View::Attached(_)) {
            app.refresh_github_references();
        }

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

        if mux_paint_due(app, repaint, last_paint) {
            terminal.draw(|f| ui::draw(f, app))?;
            last_paint = std::time::Instant::now();
        }

        // Runs after the draw above so the "creating worktree…" notice is already on
        // screen before this blocks on Git.
        if let Some(pending) = app.pending_worktree.take() {
            input::run_pending_worktree(app, pending);
            continue;
        }

        if app.mux.is_some() {
            repaint = pump_mux(app)?;
        } else {
            input::handle_input(app)?;
            repaint = true;
        }

        if exit_waits_for_update(app) {
            app.status_message =
                Some("Finishing the update before leaving; running sessions stay open...".into());
            continue;
        }
        let exit_requested =
            app.should_quit || app.should_resume.is_some() || app.should_new_session.is_some();
        if exit_requested && app.exit_waits_for_notifications() {
            continue;
        }

        if app.should_quit {
            if app.exit_dir.is_none() {
                app.exit_dir = app
                    .mux
                    .as_ref()
                    .and_then(|mux| mux.focused_cwd())
                    .map(|path| path.to_string_lossy().to_string());
            }
            let shutdown = app
                .mux
                .as_mut()
                .map(|mux| mux.shutdown())
                .transpose();
            if let Err(error) = shutdown {
                app.should_quit = false;
                app.cancel_notification_drain();
                app.detach();
                app.status_message = Some(format!("Cannot quit yet: {error}"));
                continue;
            }
            break;
        }

        if app.should_resume.is_some() || app.should_new_session.is_some() {
            break;
        }
    }

    if let Some(scratchpad) = app.scratchpad.as_mut() {
        scratchpad.save()?;
    }

    // Without a daemon, panes are children of this process and must be reaped. Capture the
    // exit directory first — shutdown drops the panes that hold it.
    if let Some(mux) = app.mux.as_mut() {
        if app.exit_dir.is_none() {
            app.exit_dir = mux
                .focused_cwd()
                .map(|path| path.to_string_lossy().to_string());
        }
        if !mux.panes.is_empty() {
            mux.shutdown()?;
        }
    }

    Ok(())
}

fn update_layout_metrics(app: &mut App, height: u16) {
    // visible_rows must match the session_list take() count:
    // inner height = total height - 6 (title + borders + status), minus the group
    // headers, which the list asks the app for so the two cannot disagree.
    let lines_per_item = if app.project_filter.is_some() { 1 } else { 2 };
    app.visible_rows = (height as usize)
        .saturating_sub(6)
        .saturating_sub(app.list_header_lines())
        / lines_per_item;

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
fn pump_mux(app: &mut App) -> Result<bool> {
    let Some(mux) = app.mux.as_ref() else {
        return Ok(false);
    };

    // A blank pane is showing the startup spinner, which needs frequent repaints; an
    // established pane can idle until something actually happens.
    let animating = mux_animating(app);
    let timeout = mux_wait_timeout(app, animating);

    let first = match mux
        .receiver
        .recv_timeout(std::time::Duration::from_millis(timeout))
    {
        Ok(event) => event,
        // Maintenance, animations, and background work use this timeout as their
        // redraw clock.
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return Ok(true),
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            app.should_quit = true;
            return Ok(true);
        }
    };

    let mut pending = vec![first];
    while let Ok(event) = app.mux.as_ref().expect("mux present").receiver.try_recv() {
        pending.push(event);
        if pending.len() >= 256 {
            break;
        }
    }

    let mut repaint = false;
    for event in pending {
        match event {
            mux::MuxEvent::Term(event) => {
                repaint |= terminal_event_needs_repaint(app, &event);
                apply_terminal_event(app, event)?;
            }
            other => {
                repaint |= mux_input::handle_mux_event(app, other);
            }
        }
    }

    Ok(repaint)
}

fn mux_animating(app: &App) -> bool {
    app.mux.as_ref().is_some_and(|mux| {
        mux.panes
            .iter()
            .any(|pane| pane.is_running() && pane.is_blank())
    }) || app.github_loading()
        || app.sessions_loading()
}

fn mux_paint_due(app: &App, repaint: bool, last_paint: std::time::Instant) -> bool {
    repaint
        || (app.mux.is_some()
            && last_paint.elapsed()
                >= std::time::Duration::from_millis(mux_wait_timeout(app, mux_animating(app))))
}

/// Ordinary chat input changes the child, not CST's current frame. Waiting for the
/// child's output avoids rendering the stale pre-echo screen while holding its parser.
fn terminal_event_needs_repaint(app: &App, event: &crossterm::event::Event) -> bool {
    if !matches!(app.view, app::View::Attached(_))
        || app.workspace_focus != app::WorkspaceFocus::Chat
        || app.confirm_quit
        || app.github_inspector.is_some()
        || app.snippet_modal.is_some()
        || app.workspace_help.is_some()
        || !app.terminal_focused
    {
        return true;
    }

    let Some(mux) = app.mux.as_ref() else {
        return true;
    };
    let pane_is_ready = mux
        .focused_pane()
        .is_some_and(|pane| pane.is_running() && !pane.needs_attention());
    if !pane_is_ready {
        return true;
    }

    match event {
        crossterm::event::Event::Paste(_) => false,
        crossterm::event::Event::Key(key) if key.kind == crossterm::event::KeyEventKind::Press => {
            app.update_notice.is_some()
                || mux.prefix_state != mux::PrefixState::Idle
                || mux.prefix.matches(key)
        }
        crossterm::event::Event::Key(_) => false,
        _ => true,
    }
}

fn mux_wait_timeout(app: &App, animating: bool) -> u64 {
    if animating || app.attached_terminal_visible() || app.detail_load_pending() {
        100
    } else if app.background_work_pending() {
        250
    } else {
        // Input and PTY output wake the channel immediately. This is only the idle
        // maintenance cadence; redrawing every 250 ms across many CST instances was
        // consuming a meaningful fraction of a CPU core for no visible change.
        5_000
    }
}

fn desired_terminal_title(app: &App) -> String {
    match app.view {
        app::View::Attached(_) => {
            let title = app
                .focused_pane_title()
                .unwrap_or_else(|| "Copilot Session Manager".to_string());
            if app.any_pane_needs_attention() && !title.starts_with("? ") {
                format!("? {title}")
            } else {
                title
            }
        }
        app::View::List if app.any_pane_needs_attention() => {
            "? Copilot Session Manager".to_string()
        }
        app::View::List => "Copilot Session Manager".to_string(),
    }
}

fn exit_waits_for_update(app: &App) -> bool {
    app.update_installing()
        && (app.should_quit || app.should_resume.is_some() || app.should_new_session.is_some())
}

fn apply_terminal_event(app: &mut App, event: crossterm::event::Event) -> Result<()> {
    match &event {
        crossterm::event::Event::FocusGained => {
            app.terminal_focused = true;
            if matches!(app.view, app::View::Attached(_)) {
                app.acknowledge_focused_pane();
            }
            return Ok(());
        }
        crossterm::event::Event::FocusLost => {
            app.terminal_focused = false;
            return Ok(());
        }
        crossterm::event::Event::Key(crossterm::event::KeyEvent {
            kind: crossterm::event::KeyEventKind::Press,
            ..
        })
        | crossterm::event::Event::Paste(_)
        | crossterm::event::Event::Mouse(crossterm::event::MouseEvent {
            kind: crossterm::event::MouseEventKind::Down(_),
            ..
        }) => {
            app.terminal_focused = true;
            if matches!(app.view, app::View::Attached(_)) {
                app.acknowledge_focused_pane();
            }
        }
        _ => {}
    }
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
    use crate::mux::{Pane, PaneSpec};

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
            details_parsed_len: 0,
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
    fn version_flag_reports_the_crate_version() {
        // Asking a binary what it is should not require byte-scanning it.
        let Err(error) = Cli::try_parse_from(["cst", "--version"]) else {
            panic!("--version should short-circuit parsing");
        };
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayVersion);
        assert!(error.to_string().contains(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn idle_mux_waits_for_events_instead_of_redrawing_four_times_per_second() {
        let mut app = App::new(Vec::new(), config::UserConfig::default());
        assert_eq!(mux_wait_timeout(&app, false), 5_000);
        assert_eq!(mux_wait_timeout(&app, true), 100);

        let (_sender, receiver) = std::sync::mpsc::channel();
        app.begin_session_load(receiver);
        assert_eq!(mux_wait_timeout(&app, false), 250);

        app.detail_pending = Some(("large-session".to_string(), std::time::Instant::now()));
        assert_eq!(mux_wait_timeout(&app, false), 100);
    }

    #[test]
    fn config_watcher_wakes_the_mux_only_after_content_changes() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("config.json");
        std::fs::write(&path, r#"{"model":"first"}"#).unwrap();
        let initial = config::config_revision(&path).unwrap();
        let applied = std::sync::Arc::new(std::sync::Mutex::new(Some(initial)));
        let (sender, receiver) = std::sync::mpsc::channel();
        let watcher = spawn_config_watcher(
            sender,
            path.clone(),
            std::sync::Arc::clone(&applied),
            std::time::Duration::from_millis(5),
        );

        assert!(receiver
            .recv_timeout(std::time::Duration::from_millis(30))
            .is_err());
        std::fs::write(&path, r#"{"model":"second"}"#).unwrap();
        assert!(matches!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(mux::MuxEvent::ConfigChanged)
        ));
        let second = config::config_revision(&path).unwrap();
        *applied.lock().unwrap() = Some(second);
        std::thread::sleep(std::time::Duration::from_millis(20));
        while receiver.try_recv().is_ok() {}

        std::fs::write(&path, r#"{"model":"first"}"#).unwrap();
        assert!(matches!(
            receiver.recv_timeout(std::time::Duration::from_secs(1)),
            Ok(mux::MuxEvent::ConfigChanged)
        ));

        drop(receiver);
        std::fs::write(&path, r#"{"model":"third"}"#).unwrap();
        watcher.join().unwrap();
    }

    #[test]
    fn repaint_deadline_bounds_staleness_when_non_visual_events_keep_arriving() {
        let config = config::UserConfig {
            mux: true,
            ..config::UserConfig::default()
        };
        let mut app = App::new(Vec::new(), config);
        assert!(mux_paint_due(
            &app,
            false,
            std::time::Instant::now() - std::time::Duration::from_secs(6)
        ));

        app.detail_pending = Some(("large-session".to_string(), std::time::Instant::now()));
        assert!(mux_paint_due(
            &app,
            false,
            std::time::Instant::now() - std::time::Duration::from_millis(150)
        ));
        assert!(mux_paint_due(&app, true, std::time::Instant::now()));
    }

    #[test]
    fn ordinary_chat_input_waits_for_the_child_before_repainting() {
        let config = config::UserConfig {
            mux: true,
            ..config::UserConfig::default()
        };
        let mut app = App::new(Vec::new(), config);
        let events = app.mux.as_ref().unwrap().events.clone();
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/c".to_string(), "ping -n 30 127.0.0.1 >nul".to_string()],
            )
        } else {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "sleep 30".to_string()],
            )
        };
        let pane = Pane::spawn(
            PaneSpec {
                id: 1,
                title: "Latency test".to_string(),
                cwd: std::env::temp_dir(),
                session_id: "latency-test".to_string(),
                program,
                args,
                events_path: None,
            },
            24,
            80,
            events,
        )
        .unwrap();
        app.mux.as_mut().unwrap().push(pane);
        app.view = app::View::Attached(1);
        app.terminal_focused = true;

        let character = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('a'),
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!terminal_event_needs_repaint(&app, &character));
        assert!(!terminal_event_needs_repaint(
            &app,
            &crossterm::event::Event::Paste("large prompt".to_string())
        ));

        let prefix = crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Char('b'),
            crossterm::event::KeyModifiers::CONTROL,
        ));
        assert!(terminal_event_needs_repaint(&app, &prefix));

        app.update_notice = Some("Update ready".to_string());
        assert!(terminal_event_needs_repaint(&app, &character));
    }

    #[test]
    fn outer_title_carries_attention_in_attached_and_list_views() {
        let config = config::UserConfig {
            mux: true,
            ..config::UserConfig::default()
        };
        let mut app = App::new(Vec::new(), config);
        let events = app.mux.as_ref().unwrap().events.clone();
        let (program, args) = if cfg!(windows) {
            (
                "cmd.exe".to_string(),
                vec!["/c".to_string(), "ping -n 30 127.0.0.1 >nul".to_string()],
            )
        } else {
            (
                "/bin/sh".to_string(),
                vec!["-c".to_string(), "sleep 30".to_string()],
            )
        };
        let mut pane = Pane::spawn(
            PaneSpec {
                id: 1,
                title: "Plan review".to_string(),
                cwd: std::env::temp_dir(),
                session_id: "attention-title".to_string(),
                program,
                args,
                events_path: None,
            },
            24,
            80,
            events,
        )
        .unwrap();
        pane.feed_synthetic(b"\x1b]9;4;3;0\x1b\\\x1b]9;4;0;0\x1b\\");
        pane.refresh_from_callbacks(false);
        app.mux.as_mut().unwrap().push(pane);
        app.view = app::View::Attached(1);

        assert_eq!(desired_terminal_title(&app), "? Plan review");
        app.terminal_focused = false;
        apply_terminal_event(&mut app, crossterm::event::Event::FocusGained).unwrap();
        assert_eq!(desired_terminal_title(&app), "Plan review");

        app.terminal_focused = false;
        app.mux
            .as_mut()
            .unwrap()
            .pane_mut(1)
            .unwrap()
            .feed_synthetic(b"\x1b]9;4;3;0\x1b\\\x1b]9;4;0;0\x1b\\");
        app.mux
            .as_mut()
            .unwrap()
            .pane_mut(1)
            .unwrap()
            .refresh_from_callbacks(false);
        app.view = app::View::List;
        assert_eq!(desired_terminal_title(&app), "? Copilot Session Manager");
        apply_terminal_event(&mut app, crossterm::event::Event::FocusGained).unwrap();
        assert_eq!(
            desired_terminal_title(&app),
            "? Copilot Session Manager",
            "focusing the list must not acknowledge a pane that was not viewed"
        );

        let _ = app.mux.as_mut().unwrap().shutdown();
    }

    #[test]
    fn newly_created_app_is_unattended_until_focus_or_input_arrives() {
        let mut app = App::new(Vec::new(), config::UserConfig::default());
        assert!(!app.terminal_focused);

        apply_terminal_event(
            &mut app,
            crossterm::event::Event::Key(crossterm::event::KeyEvent::new(
                crossterm::event::KeyCode::Char('x'),
                crossterm::event::KeyModifiers::NONE,
            )),
        )
        .unwrap();

        assert!(app.terminal_focused);
    }

    #[test]
    fn natural_exit_waits_for_background_installation() {
        let mut app = App::new(Vec::new(), config::UserConfig::default());
        let (_sender, receiver) = std::sync::mpsc::channel();
        app.update_install_receiver = Some(receiver);
        app.should_quit = true;

        assert!(exit_waits_for_update(&app));

        app.update_install_receiver = None;
        assert!(!exit_waits_for_update(&app));
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
