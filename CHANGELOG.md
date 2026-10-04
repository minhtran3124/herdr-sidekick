# Changelog

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
