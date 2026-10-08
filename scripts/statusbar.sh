#!/usr/bin/env bash
# Usage: statusbar.sh install | remove
#   install  add a managed block to herdr's config.toml: a tab-bar summary (`sidekick board --status`)
#            and a `$wt_pr` row in the sidebar's Space rows; validate, reload, roll back on failure
#   remove   drop the block, clear the sidebar tokens this plugin published, reload
set -euo pipefail

H=${HERDR_BIN_PATH:-herdr}
ID=${HERDR_PLUGIN_ID:?run from herdr}
ROOT=${HERDR_PLUGIN_ROOT:?run from herdr}
STATE=${HERDR_PLUGIN_STATE_DIR:?run from herdr}
CONF=${HERDR_CONFIG:-$HOME/.config/herdr/config.toml}
RUN=$ROOT/scripts/run.sh
BEGIN="# >>> $ID"
END="# <<< $ID"

say() {
  echo "$1"
  "$H" notification show "Sidekick" --body "$1" >/dev/null 2>&1 || true
}

# Config minus our block (prefix match, so older marker lines with a trailing note still match).
without_block() {
  [ -f "$CONF" ] || return 0
  awk -v b="$BEGIN" -v e="$END" 'index($0, b) == 1 { skip = 1 } !skip { print } index($0, e) == 1 { skip = 0 }' "$CONF"
}

# Write $1 as the new config and restore the previous one if herdr rejects it.
# `.bak-sidekick` is written once, before the plugin's first edit, and never overwritten.
commit() {
  local next=$1 prev
  mkdir -p "$(dirname "$CONF")"
  touch "$CONF"
  prev=$(mktemp)
  cp "$CONF" "$prev"
  [ -f "$CONF.bak-sidekick" ] || cp "$prev" "$CONF.bak-sidekick"
  printf '%s\n' "$next" >"$CONF.tmp.$$" && mv "$CONF.tmp.$$" "$CONF"
  if ! out=$("$H" config check 2>&1); then
    cp "$prev" "$CONF"
    rm -f "$prev"
    say "config rejected, restored previous config.toml: ${out##*$'\n'}"
    exit 1
  fi
  rm -f "$prev"
  "$H" server reload-config >/dev/null 2>&1 || true
}

entry() {
  echo "  { type = \"command\", command = \"sh '$RUN' board --status --state '$STATE'\", interval_seconds = 5, timeout_seconds = 4 },"
}

spaces_rows() {
  echo 'rows = [["state_icon", "workspace"], ["branch", "git_status"], [{ token = "$wt_pr", fg = "#c678dd" }]]'
}

# $1 wrapped in marker lines. Markers sit at column 0 so without_block finds them, even when the
# block is a single entry inside the user's own tab_bar_right array (comments are legal there).
marked() {
  printf '%s\n%s\n%s\n' "$BEGIN (managed by the Sidekick plugin; remove with its statusbar-remove action)" "$1" "$END"
}

# stdin with $INSERT printed after the first line matching the ERE $1.
insert_after() {
  # Both go through ENVIRON: awk -v would eat the backslashes in `\[ui\]`.
  RE=$1 INSERT=$2 awk '{ print } !done && $0 ~ ENVIRON["RE"] { print ENVIRON["INSERT"]; done = 1 }'
}

# The user's config with our lines merged in. TOML forbids defining a table or key twice, so
# instead of appending a second [ui] we add our entry to their tab_bar_right array, or our key
# to their [ui] table, and only append [ui] when they have none.
merged() {
  local rest=$1 out tbr
  tbr=$(grep -E '^[[:space:]]*tab_bar_right[[:space:]]*=' <<<"$rest" || true)
  if [ -n "$tbr" ]; then
    # A multi-line array (`tab_bar_right = [` alone on its line) takes one more entry line.
    if ! grep -Eq '=[[:space:]]*\[[[:space:]]*(#.*)?$' <<<"$tbr"; then
      return 1
    fi
    out=$(insert_after '^[[:space:]]*tab_bar_right[[:space:]]*=' "$(marked "$(entry)")" <<<"$rest")
  elif grep -Eq '^[[:space:]]*\[ui\][[:space:]]*(#.*)?$' <<<"$rest"; then
    out=$(insert_after '^[[:space:]]*\[ui\][[:space:]]*(#.*)?$' \
      "$(marked "tab_bar_right = ["$'\n'"$(entry)"$'\n'"]")" <<<"$rest")
  else
    out="${rest%$'\n'}"$'\n\n'"$(marked "[ui]"$'\n'"tab_bar_right = ["$'\n'"$(entry)"$'\n'"]")"
  fi
  # The PR row needs Space rows of its own; a user-defined [ui.sidebar.spaces] wins.
  if ! grep -Eq '^[[:space:]]*\[ui\.sidebar\.spaces\]' <<<"$rest"; then
    out="${out%$'\n'}"$'\n\n'"$(marked "[ui.sidebar.spaces]"$'\n'"$(spaces_rows)")"
  fi
  printf '%s\n' "$out"
}

install() {
  if ! sh "$RUN" --version >/dev/null 2>&1; then
    say "sidekick binary missing: run 'sh scripts/build.sh' in $ROOT"
    exit 1
  fi
  local rest next
  rest=$(without_block)
  # Root-level dotted keys (`ui.x = ...`) define [ui] in a way no merge can extend.
  if grep -Eq '^[[:space:]]*ui\.' <<<"$rest" || ! next=$(merged "$rest"); then
    say "config.toml defines [ui] in a form Sidekick cannot merge into (dotted ui.* keys or a one-line tab_bar_right); add this entry to tab_bar_right by hand"
    entry
    exit 1
  fi
  commit "${next%$'\n'}"
  if grep -Eq '^[[:space:]]*\[ui\.sidebar\.spaces\]' <<<"$rest"; then
    say "tab-bar summary added; your own [ui.sidebar.spaces] rows are kept, add [{ token = \"\$wt_pr\" }] there for the PR row"
  else
    say "tab-bar summary and sidebar PR row added"
  fi
}

remove() {
  local tokens=$STATE/tokens.json ws
  if [ -f "$tokens" ]; then
    for ws in $(jq -r 'keys[]' "$tokens" 2>/dev/null); do
      "$H" workspace report-metadata "$ws" --source "$ID" --clear-token wt_pr >/dev/null 2>&1 || true
    done
    rm -f "$tokens"
  fi
  if [ -f "$CONF" ] && grep -qF "$BEGIN" "$CONF"; then
    commit "$(without_block)"
  fi
  say "tab-bar summary and sidebar PR row removed"
}

case ${1:-} in
  install) install ;;
  remove) remove ;;
  *) echo "usage: statusbar.sh install|remove" >&2; exit 2 ;;
esac
