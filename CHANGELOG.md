# Changelog

## 0.2.1

- **Sidekick: add tab-bar summary** now merges into a config.toml that already has its own `[ui]`
  instead of refusing: it adds its entry to an existing multi-line `tab_bar_right` array, or its
  key to the existing `[ui]` table, and keeps a user-defined `[ui.sidebar.spaces]`. Remove still
  takes out exactly what install added. A one-line `tab_bar_right` or dotted `ui.*` keys are still
  left for you to edit by hand.
- Worktree board: a pane `display_agent` that only mirrors the terminal title (reported so herdr
  shows the task on pane borders) no longer replaces the agent name, so cards stop printing the
  title twice.

## 0.2.0

- Agents panel: each subagent shows its model and reasoning effort (e.g. `opus-5-5 · high`) on
  its list row, in the transcript's title band, and in the split pane's border label. The label
  updates as the model or effort changes; effort is omitted for models that do not record it.
- Panes are matched by the `#id8` in their label rather than the whole label, so the changing
  tag no longer breaks the open set, the grid order or closing a pane. Panes opened before this
  version keep their old label until reopened.

## 0.1.1

- Worktree board: Nerd Font status icons with a `?` legend (`WORKTREES_ICONS=plain` keeps
  Unicode); click buttons on the selected card; `x` hides a worktree (kept in the state dir);
  delete can also remove a merged branch, needs `f` when uncommitted files would be lost, and
  prunes a worktree whose folder is gone.
- Agents panel: no fixed limit on transcript panes. Every running subagent gets one, re-tiled as
  the grid that fits their count; FINISHED shows its 3 newest until expanded, and sections fold.
- `q` closes a panel in its tab only; `Q` hides it everywhere (what `q` did before).
- Panels restart themselves in place when the binary is rebuilt; the action
  **Sidekick: restart panels** does it once for panels started from 0.1.0.
- Fix: narrowing an oversized panel resized the wrong split and shrank panels to a few columns
  over time.
- Fix: an agent pane closing itself while the grid was being re-tiled shrank the main pane.

## 0.1.0

First release. Merges three local plugins into one: Worktree Agents (the worktree board),
Changes (changed files + diff) and Agents (Claude Code subagents).

- One binary (`sidekick <board|changes|diff|agents|view|notify>`) and one layout script, so the
  three panels always land as changes over agents, next to the worktree board.
- macOS and Linux: prebuilt binaries per release, `mkdir` lock where `flock` is missing, `gh`
  looked up on PATH (pyenv shims skipped) instead of a fixed path.
- Open file: a fuzzy path picker over `git ls-files` and a highlighted file viewer, with
  `path:line` jumps; from the changes panel (`f`), an action, or Ctrl+click on a path.
- Syntax highlighting uses the pure-Rust regex engine, so builds need no C toolchain.
