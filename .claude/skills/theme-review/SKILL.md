---
name: theme-review
description: Review CST's colour themes. Use when asked to check, review or audit the themes, when adding or changing a theme, when a colour looks wrong or off-scheme in a session, or after a Copilot CLI update changes how sessions look. Runs a contrast audit over every theme and draws the same screens in each theme with the real binary for looking at.
---

# Reviewing CST's themes

A theme can be wrong in two ways, and each needs its own check:

- **Two colours it puts together cannot be told apart.** A machine can check that, for
  every theme, in a second. That is the audit.
- **It reads, but looks wrong**: a slab of the wrong grey, two palette colours that are
  the same blue, a popup that does not stand out from what is under it. Only looking
  finds that. Those are the sheets.

Both are scripts in `scripts/` next to this file. Run them and read their verdicts; do
not run the cargo commands under them by hand.

## 1. The audit

```bash
bash .claude/skills/theme-review/scripts/audit.sh
```

One line: `audit: ok` or `audit: FAIL` followed by each failing pair as
`Theme: what is 2.31, needs 3.0`. `--table` prints every pair of every theme with its
ratio, for when you want to see how close to the line a theme sits.

The pairs live in `pairs()` in the tests of `src/theme.rs`: text and secondary text on
each of the theme's fills, the selection, every accent on the canvas, diff rows, the
sixteen terminal colours as text, and what Copilot paints on the canvas and on its
panels. A new place where CST puts two theme colours together belongs in that list.

A failure is nearly never fixed by editing a palette. `build()` in `src/theme.rs` moves
a colour only as far as its pairs need (`moved_until`), so a palette keeps its author's
colours wherever they work. If a pair fails, the builder is missing a rule for it.

## 2. The sheets

```bash
bash .claude/skills/theme-review/scripts/sheets.sh            # every theme
bash .claude/skills/theme-review/scripts/sheets.sh nord dracula
```

One line per theme with the folder its four pictures are in. The list of themes comes
from `copilot-session-tui themes`, so a new theme is picked up by itself. Each run takes
about ten seconds a theme.

| Picture | What it shows | Look for |
|---|---|---|
| `1-list.png` | CST's own screen: title bar, list, selection, details, key hints | The selected row readable; the title badges distinct; hints not washed out |
| `2-pane.png` | The stub Copilot's colour card inside a pane | See below |
| `3-picker.png` | A popup over the pane | The popup stands apart from the pane; group headings, the selected row and its markers readable |
| `4-settings.png` | A dense popup: tabs, values, hints | The current tab clear; values and their hints both readable; ON and OFF colours distinct |

Read `2-pane.png` and `3-picker.png` for every theme under review. Read the other two
for a theme that is new or whose palette changed.

### Reading the colour card

- **The two palette rows** are the sixteen terminal colours, as text and as fills. On a
  dark theme 0 is the canvas and may vanish as text; nothing else should. Blue and
  magenta (4, 5) must differ, as must each colour and its bright twin when the scheme
  defines one. Two identical swatches mean a program's output loses a distinction.
- **The grey ramp** fades from the canvas. The near end should melt into the canvas
  and take the theme's tint, not stand as neutral grey blocks on a coloured canvas.
- **The filled box, the inline code and the Sessions tab** are Copilot's panel greys.
  They should look like a step of the theme itself, visible but quiet, with the box's
  half-block edges exactly matching its fill.
- **Everything with a hue** (the link, the blue tab, the string, the `+105`) is
  Copilot's own and passes through untouched. Check it can be read, not that it suits.

## 3. When Copilot changes its colours

The card paints the real Copilot's colours from a table, in `colour_card` in
`tools/e2e/src/stub.rs`, and the audit checks the same colours from
`COPILOT_ON_DARK` and `COPILOT_ON_LIGHT` in `src/theme.rs`. Copilot uses fixed colours
of its own: one set when the terminal reports dark, another for light, and almost
nothing from the terminal's palette. When a Copilot update changes them, both tables
go stale and the review keeps passing against a Copilot that no longer exists.

Refreshing them means running the real Copilot once under a dark and once under a
light theme and reading its colours from the driver's HTML dump. That spends the
user's Copilot usage (about five units a prompt when this was written), so **ask
before doing it**. The steps:

1. A driver script with `env PATH=<the real PATH>` (which drops the stub), a scratch
   `env COPILOT_HOME=...`, `config {"theme": "solarized-dark"}`, then `press n`,
   `press enter`, wait, `press enter` for the folder trust prompt, `paste` one prompt
   asking for inline code, a list, a link and a code block, wait, `dump`.
2. Tally the colours in the dump:
   `grep -o 'background:[^;"]*' 05-answer.html | sort | uniq -c | sort -rn`, and the
   same for `[^-]color:`.
3. Repeat with `solarized-light`. Colours that are the theme's own (its canvas, its
   text) are CST's; the rest are Copilot's.
4. Put the fills in `panels`, the main text colours in `text` and the secondary ones
   in `muted`, in both files, and run the audit.

## 4. Reporting

Say which themes you ran the sheets for and which pictures you looked at, what the
audit said, and anything that looked wrong even though it passed. A theme you did not
look at was not reviewed.
