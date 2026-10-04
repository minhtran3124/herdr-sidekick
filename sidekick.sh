#!/usr/bin/env bash
# Places sidekick's side panels; each panel follows the focused pane by itself.
# Usage: sidekick.sh ensure | toggle <panel> | off <panel> | open [PATH[:LINE]]
#        panel = worktrees | changes | agents
#   ensure  (startup / focus / agent-status hooks) open each missing panel in the active tab when it
#           applies, unless hidden: worktrees when the repo has >= WORKTREES_MIN checkouts, changes in
#           a git checkout, agents once a Claude pane in the tab has spawned a subagent
#   toggle  hide that panel everywhere when the active tab shows it, else re-enable and open it here
#   off     hide that panel everywhere and stop auto-opening it (the panel's `q` key)
#   open    file viewer overlay in the focused pane's directory; PATH defaults to the Ctrl+clicked
#           link ($HERDR_PLUGIN_CLICKED_URL), and a click on something that is not a file is ignored
# Layout: [ work panes ][ ± changes / ◈ agents ][ ⎇ worktrees ]
set -euo pipefail

cfg="${HERDR_PLUGIN_CONFIG_DIR:-}/config.env"
if [ -f "$cfg" ]; then set -a; . "$cfg"; set +a; fi

H=${HERDR_BIN_PATH:-herdr}
PLUGIN=${HERDR_PLUGIN_ID:-minhtran3124.sidekick}
STATE=${HERDR_PLUGIN_STATE_DIR:?run from herdr}
CLAUDE_DIR=${CLAUDE_CONFIG_DIR:-$HOME/.claude}
# Pane labels: must match the LABEL consts in src/{worktrees,changes,agents}/mod.rs, and "◇ "
# below must match agents::data::PANE_PREFIX (agent transcript panes).
BOARD="⎇ worktrees" CHANGES="± changes" AGENTS="◈ agents"
MIN=${WORKTREES_MIN:-2}

label_of() { case $1 in worktrees) echo "$BOARD" ;; changes) echo "$CHANGES" ;; agents) echo "$AGENTS" ;; *) return 1 ;; esac; }
entry_of() { case $1 in worktrees) echo board ;; *) echo "$1" ;; esac; }
width_of() { case $1 in worktrees) echo "${WORKTREES_WIDTH:-38}" ;; changes) echo "${CHANGES_WIDTH:-44}" ;; agents) echo "${AGENTS_WIDTH:-44}" ;; esac; }
off_flag() { echo "$STATE/disabled-$1"; }

# One run holds the lock, so herdr's state only changes when this script changes it: read
# `pane list` / `workspace list` once and again only after opening a pane (refresh). Helpers
# run in $(...) subshells, which cannot fill a cache, hence the explicit refresh.
PANES='{}' WORKSPACES='{}'
refresh() {
  PANES=$("$H" pane list) || PANES='{}'
  WORKSPACES=$("$H" workspace list) || WORKSPACES='{}'
}

# "<workspace_id> <active_tab_id>" of the focused workspace.
active() {
  jq -r '.result.workspaces[] | select(.focused) | "\(.workspace_id) \(.active_tab_id)"' <<<"$WORKSPACES"
}

in_tab() {
  jq -r --arg t "$1" --arg l "$2" '.result.panes[] | select(.tab_id == $t and .label == $l) | .pane_id' <<<"$PANES"
}

is_side() { # jq filter: side panels and agent panes, which panels never split or follow
  echo '(.label // "") as $l | ($l == "'"$BOARD"'" or $l == "'"$CHANGES"'" or $l == "'"$AGENTS"'" or ($l | startswith("◇ ")))'
}

# Directory of the pane the user works in: the tab's focused pane unless it is a side panel,
# else its first ordinary pane, else the workspace's checkout.
tab_cwd() {
  local tab=$1 ws=$2 cwd
  cwd=$(jq -r --arg t "$tab" "[.result.panes[] | select(.tab_id == \$t and ($(is_side) | not))]
    | (map(select(.focused)) + .)[0] | .foreground_cwd // .cwd // empty" <<<"$PANES")
  [ -n "$cwd" ] || cwd=$(jq -r --arg w "$ws" \
    '.result.workspaces[] | select(.workspace_id == $w) | .worktree.checkout_path // empty' <<<"$WORKSPACES")
  echo "$cwd"
}

# Non-git workspaces make worktree list fail; single-checkout repos are not worth a board.
has_worktrees() {
  local n
  n=$("$H" worktree list --workspace "$1" 2>/dev/null | jq '.result.worktrees | length') || return 1
  ((n >= MIN))
}

# A Claude pane in the tab has a session with at least one subagent on disk.
has_subagents() {
  local sid
  for sid in $(jq -r --arg t "$1" \
    '.result.panes[] | select(.tab_id == $t and .agent_session.agent == "claude") | .agent_session.value' <<<"$PANES"); do
    compgen -G "$CLAUDE_DIR/projects/*/$sid/subagents/agent-*.meta.json" >/dev/null && return 0
    compgen -G "$CLAUDE_DIR/projects/*/$sid/subagents/workflows/*/agent-*.meta.json" >/dev/null && return 0
  done
  return 1
}

width() {
  "$H" pane layout --pane "$1" | jq -er --arg id "$1" '.result.layout.panes[] | select(.pane_id == $id) | .rect.width'
}

# A placeholder shell pane of the final size, split off pane $1 to the right so $1 keeps all but
# $2 columns: $1 is resized once, straight to its final width. `plugin pane open` takes no size,
# and opening 50/50 then resizing made the main pane jump 345 → 173 → 306 → 153 → 262 columns,
# each a full redraw of the agent running in it.
slot() { # pane cols -> placeholder pane id
  local w ratio
  w=$(width "$1") || return 1
  ((w > $2 + 10)) || return 1
  ratio=$(awk -v w="$w" -v c="$2" 'BEGIN { printf "%.4f", (w - c) / w }')
  "$H" pane split --pane "$1" --direction right --ratio "$ratio" --no-focus | jq -er '.result.pane.pane_id'
}

# Opens a panel inside placeholder $3: split it down, then close the placeholder so the panel
# takes its whole space. Only the placeholder's area changes while this happens.
fill() { # panel tab placeholder cwd
  if ! open_pane "$1" "$2" "$3" down "$4" >/dev/null; then
    # A leftover shell is worse than a missing panel, which the next ensure retries.
    "$H" pane close "$3" >/dev/null 2>&1 || true
    return 1
  fi
  "$H" pane close "$3" >/dev/null 2>&1 || true
}

open_pane() { # panel tab target direction cwd -> new pane id
  "$H" plugin pane open --plugin "$PLUGIN" --entrypoint "$(entry_of "$1")" --placement split \
    --target-pane "$3" --direction "$4" --cwd "$5" --no-focus | jq -r '.result.plugin_pane.pane.pane_id'
}

# Work pane to split right for the middle column: the tab's focused ordinary pane, else its first.
work_pane() {
  jq -r --arg t "$1" "[.result.panes[] | select(.tab_id == \$t and ($(is_side) | not))]
    | (map(select(.focused)) + .)[0].pane_id // empty" <<<"$PANES"
}

# One panel into an existing layout (toggle, or a panel that starts applying later).
place() { # panel tab cwd
  local panel=$1 tab=$2 cwd=$3 target other new ph col p
  case $panel in
    worktrees)
      # The board spans the full height right of the middle column. Cutting it from a column
      # pane would make it half height, so the column is rebuilt in one placeholder with it.
      col=""
      for p in changes agents; do [ -n "$(in_tab "$tab" "$(label_of "$p")")" ] && col="$col $p"; done
      if [ -n "$col" ]; then
        for p in $col; do close_in_tab "$tab" "$p"; done
        refresh
        place_all "$tab" "$cwd" worktrees $col
        return 0
      fi
      target=$(work_pane "$tab")
      ;;
    changes | agents)
      other=$(in_tab "$tab" "$([ "$panel" = changes ] && echo "$AGENTS" || echo "$CHANGES")" | head -n1)
      if [ -n "$other" ]; then
        # The middle column exists: share it. It reads changes above agents, so a later changes
        # pane trades places with the agents one.
        new=$(open_pane "$panel" "$tab" "$other" down "$cwd") || return 0
        [ "$panel" = changes ] && { "$H" pane swap --source-pane "$new" --target-pane "$other" >/dev/null 2>&1 || true; }
        refresh
        return 0
      fi
      target=$(work_pane "$tab")
      ;;
  esac
  [ -n "$target" ] || return 0
  if ph=$(slot "$target" "$(width_of "$panel")"); then
    fill "$panel" "$tab" "$ph" "$cwd" || true
  else
    open_pane "$panel" "$tab" "$target" right "$cwd" >/dev/null || true
  fi
  refresh
}

# Opens every panel in $3.. that is missing. When the middle column and the board both open,
# one placeholder as wide as both is cut from the work pane and the board is cut from it, so
# the work pane is resized once and the board still spans the full height.
place_all() { # tab cwd panels...
  local tab=$1 cwd=$2 list p first cw bw ph board
  shift 2
  list=" $* "
  if [[ $list == *" changes "* || $list == *" agents "* ]] && [ -z "$(in_tab "$tab" "$CHANGES")$(in_tab "$tab" "$AGENTS")" ]; then
    first=agents
    [[ $list == *" changes "* ]] && first=changes
    cw=$(width_of "$first") bw=0
    [[ $list == *" worktrees "* ]] && bw=$(width_of worktrees)
    if ph=$(slot "$(work_pane "$tab")" $((cw + bw))); then
      if ((bw > 0)) && board=$(slot "$ph" "$bw"); then
        fill worktrees "$tab" "$board" "$cwd" && list=${list/ worktrees / }
      fi
      fill "$first" "$tab" "$ph" "$cwd" || true
      list=${list/ $first / }
      refresh
    fi
  fi
  for p in $list; do place "$p" "$tab" "$cwd" || true; done
}

applies() { # panel ws tab cwd
  case $1 in
    worktrees) has_worktrees "$2" ;;
    changes) git -C "${4:-/nonexistent}" rev-parse >/dev/null 2>&1 ;;
    agents) has_subagents "$3" ;;
  esac
}

# Panels that should open in the active tab: missing there, not hidden, and applying.
ensure() {
  local ws tab cwd p want=""
  read -r ws tab < <(active) || return 0
  [ -n "$tab" ] || return 0
  cwd=$(tab_cwd "$tab" "$ws")
  for p in worktrees changes agents; do
    [ -f "$(off_flag "$p")" ] && continue
    [ -z "$(in_tab "$tab" "$(label_of "$p")")" ] || continue
    # One panel failing to apply (herdr busy) must not keep the others from opening.
    applies "$p" "$ws" "$tab" "$cwd" && want="$want $p"
  done
  [ -n "$want" ] && place_all "$tab" "${cwd:-$HOME}" $want
  fit "$tab"
}

# Toggle on: open the panel here even when it does not apply.
force_open() {
  local ws tab cwd
  read -r ws tab < <(active) || return 0
  [ -n "$tab" ] || return 0
  [ -z "$(in_tab "$tab" "$(label_of "$1")")" ] || return 0
  cwd=$(tab_cwd "$tab" "$ws")
  place "$1" "$tab" "${cwd:-$HOME}"
}

# Re-linking the plugin drops herdr's ownership record, so fall back to a plain pane close.
close_pane() {
  "$H" plugin pane close "$1" >/dev/null 2>&1 || "$H" pane close "$1" >/dev/null 2>&1 || true
}

close_in_tab() { # tab panel
  local id
  for id in $(in_tab "$1" "$(label_of "$2")"); do close_pane "$id"; done
}

# Closing a pane hands its columns to a neighbour, so after a panel closes the board or the
# middle column can be left wider than configured. Narrow each back by growing the pane on its
# left. Resize amounts are fractions of the parent right-split holding the panel.
fit() { # tab
  local anchor layout label panel id w parent left want amount
  # Most focus events land in tabs with nothing to fit: skip the layout call there.
  [ -n "$(in_tab "$1" "$BOARD")$(in_tab "$1" "$CHANGES")$(in_tab "$1" "$AGENTS")" ] || return 0
  anchor=$(work_pane "$1")
  [ -n "$anchor" ] || return 0
  layout=$("$H" pane layout --pane "$anchor") || return 0
  for panel in worktrees changes agents; do
    label=$(label_of "$panel")
    id=$(in_tab "$1" "$label" | head -n1)
    [ -n "$id" ] || continue
    want=$(width_of "$panel")
    read -r w parent left < <(jq -r --arg id "$id" '.result.layout as $L
      | ($L.panes[] | select(.pane_id == $id) | .rect) as $r
      | ([$L.splits[] | select(.direction == "right" and .rect.x <= $r.x and .rect.x + .rect.width >= $r.x + $r.width
          and .rect.y <= $r.y and .rect.y + .rect.height >= $r.y + $r.height) | .rect.width] | min) as $p
      | ([$L.panes[] | select(.rect.x + .rect.width == $r.x and .rect.y <= $r.y and .rect.y + .rect.height > $r.y) | .pane_id][0] // "-") as $left
      | "\($r.width) \($p) \($left)"' <<<"$layout") || continue
    [ "$left" != "-" ] && [ "$parent" != null ] && ((w > want + 1)) || continue
    amount=$(awk -v w="$w" -v t="$want" -v p="$parent" 'BEGIN { printf "%.4f", (w - t) / p }')
    "$H" pane resize --pane "$left" --direction right --amount "$amount" >/dev/null 2>&1 || true
    layout=$("$H" pane layout --pane "$anchor") || return 0
  done
}

# `off` and toggling off: the panel in every tab, not just this one.
close_all() {
  local id
  for id in $(jq -r --arg l "$(label_of "$1")" '.result.panes[] | select(.label == $l) | .pane_id' <<<"$PANES"); do
    close_pane "$id"
  done
}

cmd=${1:-} panel=${2:-}

# Opening a file waits for nobody: it changes no panel layout, so it skips the lock.
if [ "$cmd" = open ]; then
  refresh
  target=${2:-${HERDR_PLUGIN_CLICKED_URL:-}}
  read -r ws tab < <(active) || exit 0
  cwd=$(tab_cwd "$tab" "$ws")
  cwd=${cwd:-$HOME}
  if [ -z "${2:-}" ] && [ -n "$target" ]; then
    # The link pattern also matches hosts and versions (127.0.0.1:8080, v1.2): open only files.
    path=$(sed -E 's|^file://||; s/(:[0-9]+){1,2}$//' <<<"$target")
    root=$(git -C "$cwd" rev-parse --show-toplevel 2>/dev/null || echo "$cwd")
    [ -f "$path" ] || [ -f "$cwd/$path" ] || [ -f "$root/$path" ] || exit 0
  fi
  exec "$H" plugin pane open --plugin "$PLUGIN" --entrypoint open --placement overlay --cwd "$cwd" \
    --env "OPEN_PATH=$target" --focus >/dev/null
fi

# Focus and agent-status events arrive in bursts; serialize so a tab never gets two of a panel.
# macOS has no flock(1): mkdir is atomic there, and a stuck lock is ignored after ~5s.
if command -v flock >/dev/null 2>&1; then
  # A burst of ensures needs one run after the current one, not one per event: a second waiter
  # leaves (the run already waiting reads the state when its turn comes). toggle/off always run.
  if [ "$cmd" = ensure ]; then
    exec 8>"$STATE/pending"
    flock -n 8 || exit 0
  fi
  exec 9>"$STATE/lock"
  flock 9
  [ "$cmd" = ensure ] && exec 8>&-
else
  for _ in $(seq 50); do mkdir "$STATE/lock.d" 2>/dev/null && break; sleep 0.1; done
  trap 'rmdir "$STATE/lock.d" 2>/dev/null' EXIT
fi

refresh
case $cmd in
  ensure) ensure || true ;;
  toggle | off)
    label_of "$panel" >/dev/null || { echo "usage: sidekick.sh $cmd worktrees|changes|agents" >&2; exit 2; }
    read -r _ tab < <(active) || true
    if [ "$cmd" = off ] || [ -n "$(in_tab "${tab:-}" "$(label_of "$panel")")" ]; then
      touch "$(off_flag "$panel")"
      close_all "$panel"
    else
      rm -f "$(off_flag "$panel")"
      force_open "$panel"
    fi
    refresh
    [ -n "${tab:-}" ] && { fit "$tab" || true; }
    ;;
  *) echo "usage: sidekick.sh ensure | toggle <panel> | off <panel> | open [PATH[:LINE]]" >&2; exit 2 ;;
esac
