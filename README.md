# Sidekick

Side panels for [herdr](https://herdr.dev) when you run coding agents in parallel:

![Sidekick in herdr: a Claude pane, then changed files over its subagents, then the worktree board](docs/screenshot.png)

- **⎇ worktrees**: every git worktree of the repo as a card, with its agents, changed-file count,
  ahead/behind and PR + CI status. Opens when the repo has 2+ worktrees. The selected card has
  click buttons (open, start claude, PR, hide, delete); `x` hides a worktree you do not need to
  see, and `?` explains every icon.
- **± changes**: files changed vs HEAD in the checkout of the pane you are focused on, with +/-
  counts. Click a file for a full-file diff.
- **◈ agents**: the Claude Code subagents of the focused Claude pane, split into NEEDS YOU
  (approval, reported BLOCKED, failed), RUNNING and FINISHED. Each running subagent gets its live
  transcript as a pane next to your Claude pane, opened on its own; your Claude pane and the
  agent panes are re-tiled as one grid of equal cells that fits their count (main + 5 agents →
  3×2), and agent panes close 3s after their agent finishes.
  FINISHED shows the 3 newest until you expand it; click a section header to fold it. herdr
  notifies you when an agent needs you, in any workspace.
- **Open file**: type a path in the repo, fuzzy like Ctrl+P (`aiapi` finds
  `apps/api/app/routers/ai/api.py`), and view it with syntax highlighting. `path:line` jumps to
  the line. Open it with `f` in the changes panel, the action **Sidekick: open file**, or
  Ctrl+click a file path in the terminal.

Panels open by themselves where they apply and follow the pane you work in.

## Install

```sh
herdr plugin install minhtran3124/herdr-sidekick
```

The install downloads a prebuilt binary for macOS (Apple Silicon, Intel) or Linux (x86_64,
arm64). Without one it builds from source, which needs [Rust](https://rustup.rs).

Needs herdr 0.9.1+, `git`, `jq` and `bash`. Optional: `gh` (logged in) for PR status, Claude
Code for the agents panel, a Nerd Font for file icons.

Optional tab-bar summary and sidebar PR row: run the action **Sidekick: add tab-bar summary**.
It adds a managed block to `~/.config/herdr/config.toml` and backs the file up first.

## Keys

Every panel moves with `j`/`k` or `↑`/`↓`, and the mouse wheel scrolls. Viewers (diff, file,
agent pane) also take `d`/`u` or PgDn/PgUp to page and `g`/`G` for top/bottom; `h`/`l` scroll
the diff and file viewers sideways.

### ⎇ Worktrees

| Key | Action |
|---|---|
| `↵` | Open the worktree |
| `⇥` / `space` | Show / hide details |
| `n` | New worktree (type a branch, `↵`) |
| `d` | Delete worktree (see below) |
| `x` | Hide / unhide worktree |
| `H` | Show hidden worktrees |
| `c` | Start Claude in it |
| `o` | Open its PR |
| `y` | Copy its path |
| `/` | Filter (`esc` clears) |
| `?` | Icon legend |
| `r` | Refresh |
| `q` | Close panel in this tab |
| `Q` | Hide panel everywhere |

The selected card also has click buttons: open, start Claude, PR, hide, delete.

### ± Changes

| Key | Action |
|---|---|
| `↵` / `l` | Open diff, or fold / unfold a folder |
| `h` | Fold folder |
| `f` | Open file picker |
| `r` | Refresh |
| `q` | Close panel in this tab |
| `Q` | Hide panel everywhere |

**In the diff**

| Key | Action |
|---|---|
| `]` / `[` | Next / previous file |
| `n` / `p` | Next / previous hunk |
| `e` | Show all lines / only changes |
| `q` / `esc` | Close |

### Open file

**Picker**

| Key | Action |
|---|---|
| *type* | Fuzzy filter; `path:line` jumps to the line |
| `↑` / `↓`, `⇥`, `^N` / `^P` | Select |
| `↵` | Open |
| `^U` | Clear |
| `esc` | Close |

**Viewer**

| Key | Action |
|---|---|
| `:` | Go to line |
| `/` | Find |
| `n` / `N` | Next / previous match |
| `esc` | Back to picker |
| `q` | Close |

### ◈ Agents

| Key | Action |
|---|---|
| `↵` | Open as a pane |
| `v` / `space` | Peek (overlay) |
| `c` | Close all agent panes |
| `o` | Auto-open on / off |
| `a` | Fold / unfold finished |
| *click header* | Fold / unfold that section |
| *click* `+N more` | Show every finished agent |
| `p` | Show agents from before a resume |
| `r` | Refresh |
| `q` | Close panel in this tab |
| `Q` | Hide panel everywhere |

**Agent pane**

| Key | Action |
|---|---|
| *click* | Expand a tool call |
| `o` | Expand all tool calls |
| `t` | Show / hide thinking |
| `G` | Follow new output |
| `q` / `esc` | Close |

`q` on a panel closes it in that tab only; it stays closed there until you toggle it back.
`Q` hides it in every tab and stops it auto-opening. Either way, bring it back with the action
**Sidekick: toggle worktrees / changes / agents**. Toggling a panel that is open hides it
everywhere, like `Q`.

Deleting a worktree (`d` or the card's delete button) asks first:

| Worktree | Choices |
|---|---|
| clean | `y` remove the worktree · `b` also delete its branch · `n` cancel |
| uncommitted files | the prompt names how many will be lost; only `f` removes it (`--force`) |
| folder already gone | runs `git worktree prune` |

The branch is deleted with `git branch -d`, which keeps a branch git does not see as merged (a
squash-merged PR's branch, for one) and says so. Hidden worktrees (`x`) stay hidden across
restarts; the header shows how many.

## Settings

Copy `config.env.example` to `$(herdr plugin config-dir minhtran3124.sidekick)/config.env`.
Widths, the worktree threshold, plain icons, the `gh` path and the Claude config dir can be
changed there.

## Notes

- Agent panes are read-only. A subagent runs inside its parent Claude process, so there is no
  terminal to type into; message it from the parent instead.
- Agent panes are re-tiled with `herdr pane move`, so their transcripts keep running. A grid cell
  never goes below 30×8: past that, the pane of the agent that finished longest ago makes room,
  and an agent with no room stays in the list without a pane.
- The agents panel reads Claude Code's transcripts under `~/.claude/projects/`. Nothing is sent
  anywhere.

## Development

```sh
cargo build --release
herdr plugin link "$PWD"      # link skips [[build]]; target/release/sidekick is used directly
cargo test --release
```

Open panels notice a rebuilt binary within a second and restart themselves in place, layout
untouched. Panels started from a binary older than that feature need the action
**Sidekick: restart panels** once. Edits to `herdr-plugin.toml` need `herdr plugin link` again.

Check a panel without herdr: `sidekick changes --snapshot 44x20`,
`AGENTS_SESSION=<session id> sidekick agents --snapshot 44x30`,
`HERDR_WORKSPACE_ID=<id> sidekick board --snapshot 38x40`.

Release: bump `version` in `herdr-plugin.toml` and `Cargo.toml`, then push a `v<version>` tag.
`.github/workflows/release.yml` builds and attaches the four binaries that the install downloads.
