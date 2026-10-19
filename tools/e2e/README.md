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

### Driving

Coordinates are zero-based `COLUMN ROW`, as in the `.txt` dumps. Every input is
followed by a pause (`pace`, default 250 ms), and an input ending in a lone Esc waits
600 ms, because the terminal holds an Esc back to see whether a sequence follows and
reads anything sent too soon as Alt+key.

| Command | Effect |
| --- | --- |
| `press <key> [<key> ...]` | Named keys, each its own write: `enter`, `esc`, `tab`, `backtab`, `backspace`, `space`, `up`, `down`, `left`, `right`, `home`, `end`, `pageup`, `pagedown`, `delete`, `ctrl+<letter>`, `alt+<key>`, or any single character. `press ctrl+b d` is the prefix then `d`. |
| `type <text>` | The text exactly as written, trailing spaces included. |
| `key <bytes>` | Raw bytes, with `\e`, `\r`, `\n`, `\t`, `\s` (space), `\\` and `\xHH`. |
| `paste <text>` | A bracketed paste. |
| `click [left\|middle\|right] COLUMN ROW` | A mouse click. |
| `click_on [left\|middle\|right] <text>` | A click on the first place `<text>` appears. |
| `scroll up\|down COLUMN ROW`, `scroll_on up\|down <text>` | One wheel notch. |
| `wait <ms>` | Sleep. Prefer `wait_for`. |
| `wait_for <text>` | Until `<text>` is on screen; fails after the timeout. |
| `wait_gone <text>` | Until it is not. |
| `expect <text>`, `expect_not <text>` | Fail unless it is / is not on screen right now. |
| `dump <name>` | Write `<name>.txt`, `.html` and `.png`. |
| `pace <ms>`, `timeout <ms>` | Change the pause after inputs, and the `wait_for` limit (default 10 s). |

Do not send Enter in the same write as typed text (`key hello\r`): ConPTY drops it.
`type hello` then `press enter` is two writes.

## What it does not isolate

- **CST's config is your real one.** CST finds `config.json` through the `dirs`
  crate, which ignores environment variables on Windows. A run that saves anything
  (a favourite, a project, a setting) changes the file every running CST reloads.
  The driver copies it to `config.before.json` in the run directory first and prints
  a warning if the run changed it; compare before restoring it, since another CST may
  have saved its own change meanwhile. Better: stop the script short of the save.
- **The config also decides how a session opens.** With the multiplexer off,
  resuming one opens a real terminal window on the desktop. Put `arg --mux` in any
  script that opens a session, as `smoke.e2e` does.
- **Other state under CST's data directory is shared too.** The `R recover workspace`
  hint, for example, reflects your own recovery records. The record this run's CST
  writes is deleted at the end, and the driver fails if it could not be.
- Session dates are fixed in January 2026, so how old they read depends on today.

## Changing the driver

It is a tool for working on CST, and it is meant to grow. If a script needs a command
that is not here, add it to `parse_step` and `Driver::drive` in `src/main.rs`, and add
it to the table above, rather than working around its absence in the script. Keep it
a separate crate (it has its own `[workspace]`), so it never slows down or lints into
CST's own build.

`cargo test --manifest-path tools/e2e/Cargo.toml` runs its own tests.
