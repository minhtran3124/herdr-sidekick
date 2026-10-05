# Changelog

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
