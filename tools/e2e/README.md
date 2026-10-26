# cst-e2e: drive the real CST binary from a script

`cargo test` checks the cases someone thought of. It cannot tell you that a popup's
text is cut off at its border, that a highlight is invisible, or that Enter never
arrives. This tool runs the real CST in a pseudo-terminal, plays a script of keys,
pastes and mouse events at it, and writes out what the screen showed: as text, as
HTML with the colours, and as a PNG you can look at.

```bash
cargo run --manifest-path tools/e2e/Cargo.toml -- tools/e2e/scripts/smoke.e2e
```

It runs `cargo build` on CST first, so it never drives a stale binary. Each run gets a
fresh directory, `<temp>/cst-e2e/<script name>/` (e.g.
`%TEMP%\cst-e2e\smoke\` on Windows), holding:

- `<dump>.txt`, `<dump>.html` and `<dump>.png` for every `dump` in the script, and
  `failed.*` for the screen at the moment a step failed;
- `home/`, the fake `COPILOT_HOME` with the script's sessions, and `projects/`;
- `cst-home/`, CST's own settings and state for the run (it is started with
  `CST_HOME` pointing here), so nothing it saves reaches your real setup;
- `bin/copilot`, the stub that stands in for the Copilot CLI;
- `raw.bin`, every byte CST wrote to its terminal, for when the escape sequences
  themselves are the question.

The last lines are a verdict: `RESULT: pass`, or `RESULT: FAIL at line N: ...`. The
exit code is 0 for a pass, 1 for a failure, and 2 when the script or the options
could not be read.

**Look at the PNGs**, not only the text. The text is right for checking that something
is there; it cannot show colour, a highlight, or that a line is cut off at a border,
and those are the things users notice.

Options: `--no-build` (drive `target/debug` as it is), `--exe <path>`, `--out <dir>`,
`--no-png`. PNGs need a Chromium-based browser (Edge or Chrome are found where they
are normally installed); set `CST_E2E_BROWSER` to point at another.

## Scripts

One command per line; `#` starts a comment line. Setup commands come first. CST starts
at the first command that drives it, after which setup is refused. The whole script is
checked before anything starts, so a typo fails at once.

### Setup

| Command | Effect |
| --- | --- |
| `size 110x30` | Terminal size, columns x rows. Default 110x30. |
| `session <project> <summary...>` | A Copilot session whose cwd is `projects/<project>`. Ids are `00000001-0000-4000-8000-000000000001`, `...02`, and so on, in order; the first is the most recently used. |
| `project <name>` | An empty folder under `projects/`. |
| `git_project <name>` | The same, with `git init` run in it. |
| `env KEY=VALUE` | An environment variable for CST. |
| `arg <argument>` | A command-line argument for CST, one per line. |
| `config <json>` | CST's settings file for this run, on one line, e.g. `config {"theme": "gruvbox"}`. Without it CST runs on its defaults. |
| `fake_gh` | Put a stub `gh` first on CST's `PATH`, logged in as `cst-e2e`. It answers what thread watching asks about a discussion (who is logged in, a token, `updatedAt`, an empty comment list) and fails anything else. Discussion number 404 does not exist, like a deleted one. Without it CST uses your real `gh`. |
| `watch_thread <url>` | Subscribe the first session to a thread, e.g. `https://github.com/o/r/discussions/7`. Any host works, including one nothing answers on, such as `https://127.0.0.1:9/o/r/issues/1`, to make an issue poll fail. The watcher needs `arg --mux`, and polls a minute apart, so raise `timeout`. |
| `held_reply <url> [throttled]` | A reply on that thread that CST is holding for the first session because it was closed when the reply arrived, as if it had come in while the session was off. It shows in the inbox; opening the session should deliver it. With `throttled`, held instead because the thread had woken the session too often, which opening the session does not release. The stub `copilot` prints every line it is sent as `Fake Copilot received: ...`, so the delivery can be seen. |
| `fake_telegram [private\|forum] [unpaired]` | Serve a stub Telegram Bot API on a loopback port and point CST's settings at it (merged into any `config` line). Paired by default with user `77` in chat `1000` (private) or `-1000` (a group with topics); `unpaired` leaves only the token, for driving the wizard. The stub answers `getMe` as `@cst_e2e_bot`, long-polls `getUpdates`, records every message, topic and reaction, and says the bot is an admin unless `telegram_rights off`. Needs `arg --mux`. |

### Driving

Coordinates are zero-based `COLUMN ROW`, as in the `.txt` dumps. Every input is
followed by a pause (`pace`, default 250 ms), and an input ending in a lone Esc waits
600 ms, because the terminal holds an Esc back to see whether a sequence follows and
reads anything sent too soon as Alt+key.

| Command | Effect |
| --- | --- |
| `press <key> [<key> ...]` | Named keys, each its own write: `enter`, `esc`, `tab`, `backtab`, `backspace`, `space`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `delete`, `f1` to `f12`, `ctrl+<letter>`, `alt+<key>`, or any single character. `press ctrl+b d` is the prefix then `d`. |
| `type <text>` | The text exactly as written, trailing spaces included. On Windows the characters reach the child through ConPTY, which synthesises key events using the active keyboard layout; a character that is a dead key there — the backtick on US-International — is dropped silently. Keep the plain US layout active when a script types such characters. |
| `key <bytes>` | Raw bytes, with `\e`, `\r`, `\n`, `\t`, `\s` (space), `\\` and `\xHH`. |
| `paste <text>` | A bracketed paste. |
| `click [left\|middle\|right] COLUMN ROW` | A mouse click. |
| `click_on [left\|middle\|right] <text>` | A click on the first place `<text>` appears. |
| `scroll up\|down COLUMN ROW`, `scroll_on up\|down <text>` | One wheel notch. |
| `wait <ms>` | Sleep. Prefer `wait_for`. |
| `wait_for <text>` | Until `<text>` is on screen; fails after the timeout. |
| `wait_gone <text>` | Until it is not. |
| `expect <text>`, `expect_not <text>` | Fail unless it is / is not on screen right now. |
| `add_session <project> <summary...>` | Write another session, as `session` does, while CST is running: one started from another terminal. Its id continues the numbering. |
| `delete_session <summary...>` | Delete the folder of the session with that summary, as another CST deleting it would. |
| `github down\|logged-out\|up` | Make the `fake_gh` stub fail every call as `gh` does with no network, or as it does with nobody logged in, or answer again. Only `gh` is affected; an issue poll goes over HTTP and is not. |
| `telegram_say <text>` | A message from the paired user, as a reply to the last message CST sent, or typed into its topic in forum mode. Fails if CST has sent nothing yet. |
| `telegram_say_bare <text>` | The same with no reply and no topic: General, or a bare message in a private chat. |
| `telegram_say_from <user id> <text>` | A message from somebody else, placed like `telegram_say`. CST must ignore it. |
| `telegram_tap <label>` | Tap the inline button with that label under the latest message CST sent that has one, as the paired user. The stub records `tap: <label>`, `button on <id>: <label>` when a message is sent with buttons, and `answered tap: <text>` when CST acknowledges one. |
| `telegram_pair_code` | Send the pairing code CST has armed (read from its `telegram.json`) as `/start <code>`, which is what pressing Start on the deep link does. |
| `telegram_wait <text>`, `telegram_expect <text>`, `telegram_expect_not <text>` | Against what the stub has received, one line per event: `message: …`, `message in <topic>: …`, `topic: <name>`, `renamed topic: …`, `closed topic: …`, `reaction on <id>`, `edited <id>: …`, `incoming from <user>: …`. |
| `telegram_dump <name>` | Write those lines, and every request CST made, to `<name>.telegram.txt`. |
| `telegram_rights on\|off` | Whether the stub says the bot may manage topics. |

The stub `copilot` also takes stage directions, typed into its pane like any prompt, so a
script can act out a turn without a model: `e2e:work <ms> [<text>]` reports a turn over
OSC progress that ends after `<ms>`, writing `<text>` to the session's `events.jsonl` as
an assistant message while it runs (which is what a Telegram notification carries);
`e2e:say <text>` writes such a message without a turn; `e2e:ask <ms> <text> | <choice> | …`
starts a turn and, after `<ms>`, asks the user through the `ask_user` tool as the real
Copilot records it, leaving the turn open until the answer is recorded: by `e2e:answered
<text>`, or by playing the form as CST does for a phone reply (arrows move the highlight
from the first choice, text typed on the Other row is the answer, Enter accepts),
which ends the turn; `e2e:bell` rings the bell. A thread wake-up pointer (`A new comment
arrived on …`) is acted out as a 3 s turn on its own, as the real Copilot would, so a
script can see how CST treats that turn's end. A turn
that ends while the script is back at the list is an attention event, which is what
sends a notification.
| `dump <name>` | Write `<name>.txt`, `.html` and `.png`. |
| `pace <ms>`, `timeout <ms>` | Change the pause after inputs, and the `wait_for` limit (default 10 s). |

Do not send Enter in the same write as typed text (`key hello\r`): ConPTY drops it.
`type hello` then `press enter` is two writes.

## Isolation

CST runs with `CST_HOME` set to the run's `cst-home/`, so its settings, recovery
records, scratchpads and worktree registry all start empty and stay in the run
directory. Saving a setting, a favourite or a project in a script is safe.

The driver still checks that your real `config.json` and recovery folder were left
alone, and fails the run if not. That catches a binary that ignores `CST_HOME`, such
as one built before it existed and passed with `--exe`. Your config is copied to
`config.before.json` in the run directory first, in case it is needed.

What is not isolated:

- **Anything a session itself opens.** Pass `arg --mux` in any script that resumes or
  starts a session, as `smoke.e2e` does, so the session is a pane the driver can see
  rather than a terminal window on the desktop.
- **The update check** reads and writes its cache in the real `~/.copilot`, like any
  CST would.
- Session dates are fixed in January 2026, so how old they read depends on today.

## Changing the driver

It is a tool for working on CST, and it is meant to grow. If a script needs a command
that is not here, add it to `parse_step` and `Driver::drive` in `src/main.rs`, and add
it to the table above, rather than working around its absence in the script. Keep it
a separate crate (it has its own `[workspace]`), so it never slows down or lints into
CST's own build.

`cargo test --manifest-path tools/e2e/Cargo.toml` runs its own tests.
