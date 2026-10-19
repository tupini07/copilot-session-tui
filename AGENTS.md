# Working on CST as an agent

Instructions for coding agents working in this repository. Humans should read
`CONTRIBUTING.md`, which this does not repeat in full.

## Do not commit to `main`

Work on a branch and open a pull request, even for a one-line fix. If you were asked to
"commit" something, that means committing to a branch and opening a PR unless the person
explicitly said to push to `main`.

PRs are merged with **merge commits, not squash**, because the release notes are built
from individual commit messages.

## Commit messages are the changelog's source material

This repository has unusually long commit messages, and that is on purpose. Release notes
are generated from `git log --no-merges <last-tag>..HEAD`, so a message that only restates
the diff produces a useless changelog entry.

Write the **why**: what was wrong, what you ruled out, what you decided and what it cost.
Subject line is `type: imperative effect` — `feat`, `fix`, `chore`, `docs`, `refactor`,
or `test`. Describe the effect on someone using CST, not the mechanism.

Do not add an `Unreleased` section to `CHANGELOG.md`. It is written at release time by
`.claude/skills/release`.

## Before you say you are done

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
```

All three must pass. Clippy carries three pre-existing warnings; anything beyond those is
yours. Do not report work as complete without running these — and report the real result,
including failures.

### Then drive the real binary until it works and looks right

For anything a user sees or touches, passing tests are not the finish line. Unit tests
check the cases you thought of; they cannot tell you that a popup's error pushes its key
hints out of sight, that a message renders past the edge of the screen, or that Enter
never arrives. All three were found this way, after `cargo test` was green.

So build it, drive the whole flow in the actual binary, read the screens, fix what you
find, and drive it again. Repeat until it works *and* looks right at an ordinary size
such as 110×30. Take the paths a user would, including errors, cancelling and a restart,
not only the happy one. When you report, say which paths you drove and which you did not.

CST is a TUI, so this takes some setup:

- `cargo test` does not rebuild the binary. Run `cargo build` first, or you are testing
  an old build.
- Run it in a pseudo-terminal and read the screen back as text. A scratch crate using
  `portable-pty` and `vt100` does it: write keys to the master, and dump the screen cell
  by cell, because flattened `contents()` misplaces wide characters. On Windows, ConPTY
  shows nothing until its opening `ESC[6n` is answered with `ESC[1;1R`. Send Enter as a
  write of its own after a short pause; bundled with typed text it is lost. SGR mouse
  sequences (`ESC[<0;col;rowM`, then `m`) arrive as real clicks.
- Isolate the sessions: point `COPILOT_HOME` at a scratch directory holding
  `session-state/<id>/workspace.yaml` (`id`, `cwd`, `summary`, `created_at`), and put a
  stub `copilot` first on `PATH` that answers `--version`, exits on `plugin`, and
  otherwise prints its arguments and working directory and idles.
- **The config is not isolated.** `config_path()` uses `dirs::config_dir()`, which
  ignores environment variables on Windows, so a test that saves anything writes the
  user's real `config.json` — and every other running CST reloads it within a second.
  Back the file up first, and before restoring it check that only what your test touched
  differs, so you do not undo a change the user made meanwhile. Or stop short of the step
  that saves.
- Opening a session writes a recovery record to `workspaces/<pid>-*.json` under CST's
  local data directory (`%LOCALAPPDATA%\copilot-session-tui` on Windows). Delete the ones
  your test instances left, or the user's next CST offers to recover fake sessions.
- Do not screenshot the desktop to check how something looks; you would capture the
  user's other windows. Read the screen back from the pseudo-terminal instead.

## Conventions that are enforced, not merely preferred

- **Tests** live in `#[cfg(test)] mod tests` at the bottom of the file they test, and are
  named as full sentences stating the behaviour and why it matters.
- **`CHANGELOG.md` format** is checked by a test in `src/changelog.rs`. It must stay flat
  `- ` bullets under `## vX.Y.Z - YYYY-MM-DD` headings, with no inline markdown at all —
  CST renders it as plain text, so backticks and links show up literally.
- **Comments explain why, not what.** A comment restating the line below it will be
  removed in review.

## If you are working on `src/threads/`

Two rules there are security properties, not preferences, and both are held up by tests
that name them. If a change makes one of those tests fail, the change is wrong.

- **A wake-up carries a link and never comment text.** Comment bodies are written by
  anyone who can reach the thread, and a session may be running with `--yolo` in a real
  repository. `threads::judge` is the single exception and pays for it: no tools, no
  repository, an empty working directory.
- **A comment from a login that is not ours never starts anything.** It is ignored, and
  the user is not asked about it either. Every CST agent posts as the same account, so a
  different author is by definition somebody outside. The only way to let one in is the
  `thread_trusted_authors` list, which the user writes out.

A third thing, recorded because it looks like an obvious optimisation and is a dead end:
**do not poll `GET /notifications`.** One conditional request covering every subscription
is cheaper than one per thread, and it cannot work — GitHub never notifies you about your
own activity, and every CST agent comments as the same account. The inbox would ring only
for comments by other people, which is exactly the case CST refuses to act on
automatically. This was built that way first and found by a live test; the module doc in
`doorbell.rs` has the detail.

## Useful context

- `cargo run -- doctor` reports the state of every external dependency; run it after
  touching session launching, `gh`, `git`, or config loading.
- `vendor/edtui` is a vendored dependency with local patches. Changes there are
  deliberate; read the surrounding comments before altering them.
- The repository is a Copilot CLI session manager, so it is often run *inside* one of its
  own sessions. Be careful with anything that kills processes or rewrites config.
