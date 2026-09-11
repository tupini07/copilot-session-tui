# Changelog

What changed in CST, newest first. Written for people using it, not for the code.
Earlier releases: https://github.com/tupini07/copilot-session-tui/releases

## v0.28.0 - 2026-09-04

- Tell an unread tab apart from one waiting on your answer at a glance.
- The chat cursor no longer flickers in Alacritty.

## v0.27.0 - 2026-09-04

- Attached sessions get a real tab bar, so switching between them is one keystroke.
- Favorites can open as panes inside CST instead of as separate terminal tabs.
- Ctrl+J no longer submits the prompt by accident.
- A turn's progress no longer gets lost when Copilot reports its session late.
- Lifecycle hooks are given enough time to run on a loaded machine.

## v0.26.0 - 2026-09-01

- Command dialogs only offer what makes sense where you are.

## v0.25.0 - 2026-09-01

- Pane titles show the session name without Copilot's branding taking up the room.
- Copilot's colours follow the CST theme you picked.
- Lifecycle hooks are refreshed automatically after CST updates itself.

## v0.24.0 - 2026-08-28

- Install CST with a single command on Windows or Linux.
- Press the prefix key to get a menu of what it can do, instead of memorising it.
- Update from the shell with cst update, without opening the TUI.
- Session progress is reported by Copilot itself rather than guessed from its output.
- Read a GitHub discussion in the inspector, alongside issues and pull requests.
- A merged pull request is shown as merged rather than just closed.
