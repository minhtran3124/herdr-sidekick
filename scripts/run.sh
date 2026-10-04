#!/bin/sh
# Entrypoint for panes, hooks and the tab-bar command: load the user's config.env, then run
# the sidekick binary (a local cargo build wins over the downloaded one).
cfg="${HERDR_PLUGIN_CONFIG_DIR:-}/config.env"
if [ -f "$cfg" ]; then
  set -a
  . "$cfg"
  set +a
fi
root=${HERDR_PLUGIN_ROOT:-$(cd "$(dirname "$0")/.." && pwd)}
for bin in "$root/target/release/sidekick" "$root/bin/sidekick"; do
  [ -x "$bin" ] && exec "$bin" "$@"
done
echo "sidekick: no binary in $root; run 'sh scripts/build.sh' there" >&2
exit 1
