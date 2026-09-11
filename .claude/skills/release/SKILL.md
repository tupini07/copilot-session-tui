---
name: release
description: Cut a CST release. Use when asked to release, cut a release, publish a new version, bump the version and tag, or ship X.Y.Z. Writes the CHANGELOG.md section from the commit log, bumps Cargo.toml, commits, tags and pushes.
---

# Cutting a CST release

You write the changelog, bump the version, tag and push. The GitHub workflow builds the
binaries and publishes the release, using **your** changelog section as the release body —
if the section is missing the release fails, so the writing is not optional.

There are two points where you stop and ask. Everything else is yours to do.

## 1. Preflight — abort before creating anything

Run all of these first. If any fails, stop and report; nothing has been created yet, so
there is nothing to unwind.

```bash
git rev-parse --abbrev-ref HEAD                  # must be main
git status --porcelain --untracked-files=no      # must be empty - modified tracked files
git status --porcelain                           # untracked files: report, do not block
git fetch origin
git rev-list --count origin/main..HEAD   # must be 0 - unpushed commits
git rev-list --count HEAD..origin/main   # must be 0 - unpulled commits
```

**Be precise about what "dirty" means.** Only *modified tracked* files block a release:
they are changes the tag will not contain, so what CI builds is not what the working tree
says. Untracked files cannot end up in the tag either way, so they are not a problem —
mention them and carry on. A check that a repository can never satisfy is one people
learn to step over, which is worse than no check.

If there *are* modified tracked files, do not silently work around them and do not just
give up. Show the user what is modified and offer to stash it:

```bash
git stash push --include-untracked --message "parked for the vX.Y.Z release" -- <paths>
```

**Ask before stashing** — it moves the user's work out of the tree, and a stash is not a
commit: it survives neither a fresh clone nor `git stash clear`. Say so, and say
`git stash pop` brings it back.

Local `main` must equal `origin/main` **in both directions**. Unpushed commits would be
in the tag but not in what others see; unpulled ones mean you write the changelog against
the wrong span — and the release would be built from a tree missing whatever landed on
origin while you worked. Check this even when you are sure nothing has changed.

When origin *has* moved, integrating it is the fix, not a reason to abort. Preview it
read-only first, so a conflicted merge is something you report rather than something you
are halfway through:

```bash
git merge-tree --write-tree HEAD origin/main   # exit 0 means it merges cleanly
```

Clean means *textually* clean. Two branches that both added a variant to the same enum
or a field to the same struct will merge without complaint and still need the full
`cargo build`, `cargo test` and `cargo clippy` run afterwards, because the thing being
released is the combination and nobody has ever compiled it before. If it conflicts,
stop and show the user; do not resolve a contributor's feature on their behalf without
asking. Whatever arrived also needs its own changelog bullet, and a first-time outside
contributor gets the thanks line.

Then find the previous release:

```bash
git tag --sort=-v:refname | head -1
```

Use this, **not** `git describe --tags --abbrev=0`. PRs are merged with merge commits, and
`describe` follows first-parent history, which can silently pick the wrong tag.

## 2. Gather the material

```bash
git log --no-merges <last-tag>..HEAD --pretty=format:'%h %s%n%b%n---'
```

**`--no-merges` is mandatory.** Without it the span is full of
`Merge pull request #3 from fork/branch` subjects, which say nothing. The contributors'
real commits are still there — that is exactly why this repo merges instead of squashing.

Also check who contributed, for a possible thanks bullet:

```bash
git log --merges <last-tag>..HEAD --pretty='%s | %an'
```

Never take bullet text from a merge subject. If a commit's user-visible effect is not
clear from its message, read its diff (`git show <hash>`) or leave it out.

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

```bash
# edit version in Cargo.toml
cargo build      # NOT cargo update - Cargo.lock is committed and must move with it
cargo fmt --check
cargo clippy --all-targets
cargo test
```

`cargo test` matters here specifically: `the_changelog_has_a_section_for_the_version_being_built`
reads both the bumped `Cargo.toml` and your new section, so it fails if they disagree.

Confirm the diff touches exactly three files:

```bash
git diff --stat     # Cargo.toml, Cargo.lock, CHANGELOG.md
```

## 6. Show the notes — **stop and ask**

Show the user the rendered section and the diff, and **wait for approval**. This is the
text that becomes the public release page; it is worth ten seconds of a human's time.

## 7. Commit, tag, push

```bash
git commit -m "chore: release X.Y.Z"     # no leading v, matching every previous one
git tag vX.Y.Z                           # lightweight, matching every previous one
git push origin main                     # branch first
git push origin vX.Y.Z                   # then the tag
```

**Branch before tag.** The workflow checks out the tag; pushing the tag first can race a
commit that is not on the remote yet.

Then watch it and report the release URL:

```bash
gh run watch
```

## Recovery

- **Failed before the tag push:** `git reset --hard origin/main && git tag -d vX.Y.Z`.
- **The `notes` job failed after pushing:** fix `CHANGELOG.md`, commit, then
  `git tag -f vX.Y.Z && git push --force origin vX.Y.Z`. `action-gh-release` updates an
  existing release's body on re-run, so this genuinely repairs it. **Ask before
  force-pushing a tag.**
- **The release is already published and people may have downloaded it:** do not rewrite
  it. Cut a patch release instead.
