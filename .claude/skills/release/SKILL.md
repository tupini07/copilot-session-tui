---
name: release
description: Cut a CST release. Use when asked to release, cut a release, publish a new version, bump the version and tag, or ship X.Y.Z. Writes the CHANGELOG.md section from the commit log, bumps Cargo.toml, commits, tags and pushes.
---

# Cutting a CST release

You write the changelog, bump the version, tag and push. The GitHub workflow builds the
binaries and publishes the release, using **your** changelog section as the release body —
if the section is missing the release fails, so the writing is not optional.

There are two points where you stop and ask. Everything else is yours to do.

The mechanical steps are scripts in `scripts/` next to this file. Run them with the Bash
tool and read their summaries instead of running the underlying commands one by one —
every raw command output you read costs context, and the judgment this skill actually
needs from you is in steps 3, 4 and 6, not in shepherding git.

## 1. Preflight — abort before creating anything

```bash
bash .claude/skills/release/scripts/preflight.sh
```

One verdict line per check; `BLOCK` lines mean a release cannot be cut yet, and the last
two lines give you the previous tag and the size of the span. If everything passes,
nothing was created and you move on. If something blocks, the script has told you *what*;
here is what to do about it:

- **Modified tracked files.** They are changes the tag will not contain, so what CI builds
  is not what the working tree says. Untracked files are reported but never block — they
  cannot end up in the tag either way; a check a repository can never satisfy is one
  people learn to step over, which is worse than no check. Do not silently work around
  modified files and do not just give up: show the user what is modified and offer to
  stash it with
  `git stash push --include-untracked --message "parked for the vX.Y.Z release" -- <paths>`.
  **Ask before stashing** — it moves the user's work out of the tree, and a stash is not
  a commit: it survives neither a fresh clone nor `git stash clear`. Say so, and say
  `git stash pop` brings it back.
- **Out of sync with origin.** Unpushed commits would be in the tag but not in what others
  see; unpulled ones mean the changelog span is wrong and the release would miss whatever
  landed on origin. When origin has moved, integrating it is the fix, not a reason to
  abort — the script already previewed the merge read-only. "Merges cleanly" means
  *textually* clean: two branches that both added a variant to the same enum will merge
  without complaint and still need the full verify run, because the thing being released
  is the combination and nobody has ever compiled it before. If it conflicts, stop and
  show the user; do not resolve a contributor's feature on their behalf. Whatever arrived
  also needs its own changelog bullet, and a first-time outside contributor gets the
  thanks line.

## 2. Gather the material

Subjects first — do not pull full bodies for the whole span. This repo's commit messages
are deliberately long, and most subjects already state the user-visible effect:

```bash
git log --no-merges <last-tag>..HEAD --pretty='%h %s'
git log --merges    <last-tag>..HEAD --pretty='%s | %an'   # contributors, for a thanks bullet
```

**`--no-merges` is mandatory.** Merge subjects like `Merge pull request #3 from
fork/branch` say nothing; the contributors' real commits are still in the span — that is
exactly why this repo merges instead of squashing. Never take bullet text from a merge
subject.

Only when a subject leaves the user-visible effect unclear, read that one commit:

```bash
git show -s --format=%B <hash>     # its message body
git show --stat <hash>             # or, failing that, what it touched
```

If the effect is still unclear, leave it out of the notes rather than guessing.

## 3. Propose the version — **stop and ask**

Pre-1.0 rules of thumb: any `feat:` in the span means a minor bump; only `fix:` and
`chore:` means a patch. A change that breaks existing behaviour is still a minor bump
pre-1.0, but its bullet goes first and starts with "Note:".

Tell the user the version you propose and why, and **wait for confirmation**.

## 4. Write the section

Insert directly below the intro paragraph in `CHANGELOG.md`, above the previous version:

```
## vX.Y.Z - YYYY-MM-DD
```

Use today's UTC date. Then the bullets.

### Writing rules

These are the substance of this skill. The format ones are enforced by a test in
`src/changelog.rs`; the style ones are not, so they are on you.

- **3 to 7 bullets.** More than seven candidate changes means merging or dropping some —
  bullets are for humans, the commit log is the archaeology. Fewer than three usually
  means the release is too small to cut; say so.
- **One line, one idea, at most about 100 characters.** No semicolons joining two changes.
- **Lead with the user-visible effect, never the mechanism.** What can you now do, or what
  now happens. Never name a module, function, struct, PR number or commit hash.
- **A fix describes the symptom that is gone**, not the cause.
- **Never start a bullet with Added, Fixed, Changed or Updated.** That is a category, not
  information.
- **Leave out internal refactors, dependency bumps, and test or CI changes** unless
  observable behaviour changed — in which case describe the behaviour.
- **Breaking or action-required changes come first**, starting with "Note:" or "You must".
- **Plain text only**: no backticks, asterisks, underscores, links, tables, nested lists
  or emoji. CST renders this file as plain text in its What's New screen, so a backtick
  shows up as a literal backtick. Bare URLs only when the user has to visit one.
- **Do not invent.** Every bullet must trace to a commit in the span.
- **One thanks bullet at most**, last, for a first-time outside contributor:
  "Tab dragging contributed by @name."

### Worked example

Three real commits:

```
feat: reach a snippet by number and delete words while editing it
fix: stop the chat cursor flickering in Alacritty
feat: tell an unread tab apart from a blocked one
```

become:

```
- Reach any of the first ten snippets by typing its number.
- The chat cursor no longer flickers in Alacritty.
- Tell an unread tab apart from one waiting on your answer at a glance.
```

### Counter-examples

| Rejected | Why | Accepted |
|---|---|---|
| `Fixed cursor state tracking in draw_chat` | names the mechanism and a function | `The chat cursor no longer flickers in Alacritty.` |
| `Added tab reordering (#42)` | starts with a category, cites a PR | `Drag a session tab to move it, or move it with the keyboard.` |
| `Refactored TextEditor into a shared module` | internal, no behaviour change | omit it |

## 5. Bump and verify

Edit the version in `Cargo.toml`, then:

```bash
cargo build     # NOT cargo update - Cargo.lock is committed and must move with it
bash .claude/skills/release/scripts/verify.sh
```

The script runs `cargo fmt --check`, `cargo clippy --all-targets` and `cargo test`, but
prints verdicts and summaries on success and full detail only for failures — do not run
those commands separately and re-read their raw output. `cargo test` matters here
specifically: `the_changelog_has_a_section_for_the_version_being_built` reads both the
bumped `Cargo.toml` and your new section, so it fails if they disagree. The script ends
with `git diff --stat`; confirm it touches exactly `Cargo.toml`, `Cargo.lock` and
`CHANGELOG.md`.

If a single test fails once under build load, rerun the script before concluding
anything — but report a flake to the user rather than silently absorbing it.

## 6. Show the notes — **stop and ask**

Show the user the rendered section and the diff, and **wait for approval**. This is the
text that becomes the public release page; it is worth ten seconds of a human's time.

## 7. Commit, tag, push, watch

These four stay as explicit commands on purpose: they are the only irreversible part of
the flow, and each one should be visible in the transcript as itself.

```bash
git commit -am "chore: release X.Y.Z"    # no leading v, matching every previous one
git tag vX.Y.Z                           # lightweight, matching every previous one
git push origin main                     # branch first
git push origin vX.Y.Z                   # then the tag
```

**Branch before tag.** The workflow checks out the tag; pushing the tag first can race a
commit that is not on the remote yet.

Then watch the build **in the background** (the Bash tool's `run_in_background`) and
report the release URL when it finishes:

```bash
bash .claude/skills/release/scripts/watch-release.sh vX.Y.Z
```

It blocks until the workflow completes and prints one line of conclusion plus the
published release and assets — or the failing job's log tail if it did not succeed. Do
not use `gh run watch`; it repaints the whole job tree every few seconds.

## Recovery

- **Failed before the tag push:** `git reset --hard origin/main && git tag -d vX.Y.Z`.
- **The `notes` job failed after pushing:** fix `CHANGELOG.md`, commit, then
  `git tag -f vX.Y.Z && git push --force origin vX.Y.Z`. `action-gh-release` updates an
  existing release's body on re-run, so this genuinely repairs it. **Ask before
  force-pushing a tag.**
- **The release is already published and people may have downloaded it:** do not rewrite
  it. Cut a patch release instead.
