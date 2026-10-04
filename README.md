# Sidekick

Side panels for [herdr](https://herdr.dev) when you run coding agents in parallel:

```
┌──────────────────────────┬─ ± changes ──┬─ ⎇ worktrees ─┐
│                          │ files vs HEAD│ main      ✎5  │
│   your Claude pane       │ click → diff │  ● claude …   │
│                          ├─ ◈ agents ───┤ fix/…  #1309 ✓│
│                          │ NEEDS YOU 1  │ refactor/…    │
│                          │ RUNNING 2    │               │
└──────────────────────────┴──────────────┴───────────────┘
```

- **⎇ worktrees**: every git worktree of the repo as a card, with its agents, changed-file count,
  ahead/behind and PR + CI status. Opens when the repo has 2+ worktrees.
- **± changes**: files changed vs HEAD in the checkout of the pane you are focused on, with +/-
  counts. Click a file for a full-file diff.
- **◈ agents**: the Claude Code subagents of the focused Claude pane, split into NEEDS YOU
  (approval, reported BLOCKED, failed), RUNNING and FINISHED. Click one to open its live
  transcript as a pane next to your Claude pane (up to 4, as a 2×2 grid). New subagents open on
  their own. herdr notifies you when one needs you, in any workspace.
- **Open file**: type a path in the repo, fuzzy like Ctrl+P (`aiapi` finds
  `apps/api/app/routers/ai/api.py`), and view it with syntax highlighting. `path:line` jumps to
  the line. Open it with `f` in the changes panel, the action **Sidekick: open file**, or
  Ctrl+click a file path in the terminal.

Panels open by themselves where they apply and follow the pane you work in.

## Install

```sh
herdr plugin install <owner>/herdr-sidekick
```

The install downloads a prebuilt binary for macOS (Apple Silicon, Intel) or Linux (x86_64,
arm64). Without one it builds from source, which needs [Rust](https://rustup.rs).

Needs herdr 0.9.1+, `git`, `jq` and `bash`. Optional: `gh` (logged in) for PR status, Claude
Code for the agents panel, a Nerd Font for file icons.

Optional tab-bar summary and sidebar PR row: run the action **Sidekick: add tab-bar summary**.
It adds a managed block to `~/.config/herdr/config.toml` and backs the file up first.

## Keys

| Panel | Keys |
|---|---|
| worktrees | `↵` open · `⇥` info · `n` new · `d` delete · `c` start claude · `o` open PR · `y` copy path · `/` find · `q` hide |
| changes | `↵` diff · `f` open file · `r` refresh · `q` hide; in the diff `]` / `[` next/prev file, `q` close |
| open file | type to filter · `↑`/`↓` select · `↵` open · `^U` clear · `esc` close; in a file `:` go to line · `/` find · `n`/`N` next · `esc` back · `q` close |
| agents | `↵` open pane · `v` peek (overlay) · `c` close agent panes · `o` auto-open on/off · `a` hide finished · `p` show agents from before a resume · `q` hide |
| agent pane | `j`/`k` scroll · click a tool call to expand · `o` expand all · `t` thinking · `G` follow · `q` close |

`q` on a panel hides it in every tab and stops it auto-opening. Bring it back with the action
**Sidekick: toggle worktrees / changes / agents**.

## Settings

Copy `config.env.example` to `$(herdr plugin config-dir minhtran3124.sidekick)/config.env`.
Widths, the worktree threshold, the `gh` path and the Claude config dir can be changed there.

## Notes

- Agent panes are read-only. A subagent runs inside its parent Claude process, so there is no
  terminal to type into; message it from the parent instead.
- The agents panel reads Claude Code's transcripts under `~/.claude/projects/`. Nothing is sent
  anywhere.

## Development

```sh
cargo build --release
herdr plugin link "$PWD"      # link skips [[build]]; target/release/sidekick is used directly
cargo test --release
```

Check a panel without herdr: `sidekick changes --snapshot 44x20`,
`AGENTS_SESSION=<session id> sidekick agents --snapshot 44x30`,
`HERDR_WORKSPACE_ID=<id> sidekick board --snapshot 38x40`.

Release: bump `version` in `herdr-plugin.toml` and `Cargo.toml`, set `REPO` in
`scripts/build.sh`, then push a `v<version>` tag. `.github/workflows/release.yml` builds and
attaches the four binaries that the install downloads.
