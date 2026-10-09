#!/usr/bin/env bash
# Places sidekick's side panels; each panel follows the focused pane by itself.
# Usage: sidekick.sh ensure | toggle <panel> | off <panel> | restart | open [PATH[:LINE]]
#        panel = worktrees | changes | agents
#   ensure  (startup / focus / agent-status hooks) open each missing panel in the active tab when it
#           applies, unless hidden: worktrees when the repo has >= WORKTREES_MIN checkouts, changes in
#           a git checkout, agents once a Claude pane in the tab has spawned a subagent
#   toggle  hide that panel everywhere when the active tab shows it, else re-enable and open it here
#   off     hide that panel everywhere and stop auto-opening it (the panel's `Q` key; `q` closes it in its tab only)
#   restart close and reopen every open panel in every tab, so a rebuilt binary takes effect
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
closed_flag() { echo "$STATE/closed-$1-$2"; } # panel tab: `q` closed it in that tab only

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
  ratio=$(LC_ALL=C awk -v w="$w" -v c="$2" 'BEGIN { printf "%.4f", (w - c) / w }')
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

# A herdr restart restores every pane as a plain shell but keeps its label, so a panel or agent
# pane looks present while nothing runs in it, and its panel never reopens. Close those; the
# panel is then missing and opens again below. A pane just opened still runs `sh …/run.sh`, and
# a herdr without process-info (or one that fails) keeps the pane.
close_dead() { # tab
  local id fg closed=""
  for id in $(jq -r --arg t "$1" ".result.panes[] | select(.tab_id == \$t and ($(is_side))) | .pane_id" <<<"$PANES"); do
    fg=$("$H" pane process-info --pane "$id" 2>/dev/null |
      jq -r '[.result.process_info.foreground_processes[]?.cmdline] | join("\n")') || continue
    [ -n "$fg" ] || continue
    grep -qE 'sidekick|run\.sh' <<<"$fg" && continue
    close_pane "$id"
    closed=1
  done
  [ -z "$closed" ] || refresh
}

# Panels that should open in the active tab: missing there, not hidden, and applying.
ensure() {
  local ws tab cwd p f t want=""
  read -r ws tab < <(active) || return 0
  [ -n "$tab" ] || return 0
  close_dead "$tab"
  cwd=$(tab_cwd "$tab" "$ws")
  # `q` flags of tabs that no longer exist.
  for f in "$STATE"/closed-*; do
    [ -e "$f" ] || continue
    t=${f##*/closed-}
    t=${t#*-}
    jq -e --arg t "$t" 'any(.result.panes[]; .tab_id == $t)' <<<"$PANES" >/dev/null || rm -f "$f"
  done
  for p in worktrees changes agents; do
    [ -f "$(off_flag "$p")" ] && continue
    [ -f "$(closed_flag "$p" "$tab")" ] && continue
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

# Closing a pane hands its columns to a neighbour, and herdr scales a split's children by ratio,
# so panels drift off their configured width. Set each right-split whose right side is only
# panels back to the sum of those panel columns, outer split first: resizing an outer split
# rescales everything inside it. `pane resize --direction D` grows the pane across its edge on
# side D, adding --amount to that split's ratio: grow the left side right to narrow the panels,
# grow the panels left to widen them.
fit() { # tab
  local wants anchor step pane dir amount
  # Most focus events land in tabs with nothing to fit: skip the layout call there.
  wants=$(for p in worktrees changes agents; do
    for id in $(in_tab "$1" "$(label_of "$p")"); do echo "$id $(width_of "$p")"; done
  done | jq -Rn '[inputs | split(" ") | {(.[0]): (.[1] | tonumber)}] | add // {}')
  [ "$wants" != "{}" ] || return 0
  anchor=$(work_pane "$1")
  [ -n "$anchor" ] || return 0
  for step in 1 2 3 4; do
    read -r pane dir amount < <("$H" pane layout --pane "$anchor" | jq -r --argjson want "$wants" '
      .result.layout as $L
      | def inside($s): .rect.x >= $s.rect.x and .rect.x + .rect.width <= $s.rect.x + $s.rect.width
          and .rect.y >= $s.rect.y and .rect.y + .rect.height <= $s.rect.y + $s.rect.height;
      [$L.splits[] | select(.direction == "right")] | sort_by(-.rect.width)[] as $s
      | [$L.panes[] | select(inside($s))] as $in
      | ($s.rect.x + $s.rect.width * $s.ratio) as $b
      | ([$in[] | select(.rect.x >= $b - 1) | .rect.x] | min) as $rx
      | [$in[] | select(.rect.x >= $rx)] as $right
      | select(all($right[]; $want[.pane_id] != null))
      | ([$right | group_by(.rect.x)[] | map($want[.pane_id]) | max] | add) as $goal
      | ($s.rect.x + $s.rect.width - $rx) as $cur
      | ([$in[] | select(.rect.x + .rect.width == $rx) | .pane_id][0]) as $left
      | select($left != null and $goal < $s.rect.width - 10 and ($cur - $goal | fabs) > 1)
      | if $cur > $goal then "\($left) right" else "\($right[0].pane_id) left" end
        + " \(($cur - $goal | fabs) / $s.rect.width)"' | head -n1) || return 0
    [ -n "${pane:-}" ] || return 0
    "$H" pane resize --pane "$pane" --direction "$dir" --amount "$amount" >/dev/null 2>&1 || return 0
    pane=""
  done
}

# A running panel keeps the binary it started with: reopen each tab's panels in place. Panels
# built since 2026-10-05 re-exec themselves on a rebuild; this is for older ones. Agent panes
# (◇) park in a hidden tab meanwhile, else the panels would be cut off the work pane and land
# between it and the agents; they come back right of the work pane and the agents panel
# re-grids them.
restart() {
  local tab ws cwd p present agents park id work first
  for tab in $(jq -r "[.result.panes[] | select($(is_side) and (.label | startswith(\"◇ \") | not)) | .tab_id] | unique[]" <<<"$PANES"); do
    present=""
    for p in worktrees changes agents; do
      [ -n "$(in_tab "$tab" "$(label_of "$p")")" ] && present="$present $p"
    done
    ws=$(jq -r --arg t "$tab" '[.result.panes[] | select(.tab_id == $t)][0].workspace_id' <<<"$PANES")
    cwd=$(tab_cwd "$tab" "$ws")
    agents=$(jq -r --arg t "$tab" '.result.panes[] | select(.tab_id == $t and ((.label // "") | startswith("◇ "))) | .pane_id' <<<"$PANES")
    park=""
    for id in $agents; do
      if [ -z "$park" ]; then
        park=$("$H" pane move "$id" --new-tab --label sidekick-park --no-focus | jq -r '.result.move_result.pane.tab_id // empty')
      else
        "$H" pane move "$id" --tab "$park" --split down --no-focus >/dev/null
      fi
    done
    refresh
    for p in $present; do close_in_tab "$tab" "$p"; done
    refresh
    place_all "$tab" "${cwd:-$HOME}" $present
    refresh
    fit "$tab" || true
    if [ -n "$park" ]; then
      work=$(work_pane "$tab") first=""
      for id in $agents; do
        if [ -z "$first" ]; then
          "$H" pane move "$id" --tab "$tab" --target-pane "$work" --split right --ratio 0.5 --no-focus >/dev/null
          first=$id
        else
          "$H" pane move "$id" --tab "$tab" --target-pane "$first" --split down --no-focus >/dev/null
        fi
      done
      refresh
    fi
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
  restart) restart ;;
  toggle | off)
    label_of "$panel" >/dev/null || { echo "usage: sidekick.sh $cmd worktrees|changes|agents" >&2; exit 2; }
    read -r _ tab < <(active) || true
    if [ "$cmd" = off ] || [ -n "$(in_tab "${tab:-}" "$(label_of "$panel")")" ]; then
      touch "$(off_flag "$panel")"
      close_all "$panel"
    else
      rm -f "$(off_flag "$panel")" "$(closed_flag "$panel" "${tab:-}")"
      force_open "$panel"
    fi
    refresh
    [ -n "${tab:-}" ] && { fit "$tab" || true; }
    ;;
  *) echo "usage: sidekick.sh ensure | toggle <panel> | off <panel> | restart | open [PATH[:LINE]]" >&2; exit 2 ;;
esac
