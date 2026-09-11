# Contributing to CST

Thanks for wanting to help. This is a small project, so the process is short.

## Changes go through pull requests

Open a PR rather than pushing to `main`, including for your own small fixes. Describe
what changes for someone *using* CST, not only what changed in the code.

PRs are merged with **merge commits, not squash**. That is deliberate: the release notes
are written from the individual commit messages, and squashing would replace them with a
single PR title. Keep your history readable and it will end up in the changelog; see
below.

## Before you open a PR

```bash
cargo fmt --check
cargo clippy --all-targets
cargo test
```

There is no CI running these yet, so please run them yourself. `cargo clippy` should
introduce no new warnings — the repository currently carries three known ones, and any
beyond those are from your change.

If you touched anything that starts a session, talks to `gh` or `git`, or reads the
config, also run:

```bash
cargo run -- doctor
```

## Commit messages

Commit messages here are longer than most projects'. A message should explain **why** the
change exists — what was wrong, what you considered, and what you decided — not just
restate the diff. If a decision is non-obvious or a trade-off was made, that belongs in
the message, because it is the only place it survives.

The subject line follows `type: imperative summary`, where `type` is one of `feat`,
`fix`, `chore`, `docs`, `refactor`, or `test`. Keep it under about 72 characters and
write it as the effect, not the mechanism.

```
fix: stop the chat cursor flickering in Alacritty

The chat was the only pane using the terminal's real cursor; the terminal and
scratchpad panes paint their own. A real cursor is drawn by the terminal on its
own schedule, so while ratatui wrote a frame it followed every MoveTo in the
diff, darting across the pane before landing back on the child's cursor.
```

This matters more than usual because **release notes are written from these messages.**
A vague message becomes a vague changelog entry, or gets left out.

## Tests

Unit tests live in a `#[cfg(test)] mod tests` block at the bottom of the file they test.
Test names are full sentences that state the behaviour and, where it is not obvious, the
reason it matters:

```rust
#[test]
fn a_missing_optional_dependency_still_leaves_the_report_successful() { … }
```

Prefer a test that would have caught the bug you are fixing over a test that describes
the code you wrote.

## The changelog

`CHANGELOG.md` is written at release time, not per PR — there is no `Unreleased` section
to update, and adding one would conflict on every concurrent PR. Its format is enforced
by a test rather than by convention, so run `cargo test` if you edit it.

## Questions

Open an issue. A question that turns out to be a documentation gap is a useful bug.
