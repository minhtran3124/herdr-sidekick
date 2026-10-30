---
state: active        # active | paused | blocked | done
progress: 0          # rough estimate, 0-100
priority: medium     # high | medium | low
updated: 2026-10-29
updated_by: claude   # claude | manual
---

# herdr-sidekick — Status

## Summary
herdr-sidekick, a Rust TUI companion for herdr, is at Release 0.2.7 (2026-10-29). Latest work added the AGENTS_AUTO_OPEN config for subagent split panes and moved notifications to herdr events instead of polling. The working tree is clean.

## Next step
Run 0.2.7 against real herdr sessions to check event-driven notifications and AGENTS_AUTO_OPEN, then pick the next feature or bug. Also check the odd 'Release 0.3.0' commit before 0.2.2.

## Blockers
- (none)

## Recent
- 2026-10-19: created STATUS.md
