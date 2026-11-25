#!/bin/sh
# Build the Orion site (https://orion.priet.us) from wiki/ and, with
# --upload, publish it.
#
#   tools/wiki/publish.sh            build into wiki/site and check it
#   tools/wiki/publish.sh --serve    preview at http://127.0.0.1:8000 while editing
#   tools/wiki/publish.sh --upload   build, then rsync to the server
#
# The reference pages (configuration, keys, command line) and the screenshots
# are generated from Orion itself, so they always match the code.
# MkDocs Material is installed in a virtualenv under ~/.cache the first time.
set -e
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
export NO_MKDOCS_2_WARNING=1
VENV="${XDG_CACHE_HOME:-$HOME/.cache}/orion-wiki/venv"
DEST="${ORION_WIKI_DEST:-teraflops@mail.priet.us:/srv/sites/orion-site/site/}"

if [ ! -x "$VENV/bin/mkdocs" ]; then
  python3 -m venv "$VENV"
  "$VENV/bin/pip" install -q mkdocs-material pillow
fi

cd "$ROOT"
cargo build -q --release
ORION="$ROOT/target/release/orion"
REF=wiki/docs/reference

# ---- configuration file
{
  echo "# Configuration file"
  echo
  echo 'Orion reads `~/.config/orion/config.toml` (`--config` for another path).'
  echo 'This is the complete example, as printed by `orion --print-config`; every'
  echo 'option is documented in it. Create yours with:'
  echo
  echo '```sh'
  echo 'orion --print-config > ~/.config/orion/config.toml'
  echo '```'
  echo
  echo '```toml'
  "$ORION" --print-config
  echo '```'
} > "$REF/config.md"

# ---- keys
{
  echo "# Keys"
  echo
  echo 'The default key bindings, as printed by `orion --print-keys`. Press `?` in'
  echo 'Orion to see them, and rebind any of them in the configuration file:'
  echo
  echo '```toml'
  echo '[keys]'
  echo 'delete = ["d", "delete"]'
  echo 'quit = ["q", "ctrl-q"]'
  echo '```'
  echo
  "$ORION" --print-keys
  echo
  echo 'The compose panel, the file browser and the attachment list have their own'
  echo 'keys: see [Writing mail](../writing.md) and [Reading mail](../reading.md).'
} > "$REF/keys.md"

# ---- command line
{
  echo "# Command line"
  echo
  for cmd in "" "account" "status" "watch" "sync" "daemon" "contacts"; do
    if [ -z "$cmd" ]; then echo "## orion"; else echo "## orion $cmd"; fi
    echo
    echo '```'
    # shellcheck disable=SC2086
    COLUMNS=100 "$ORION" $cmd --help
    echo '```'
    echo
  done
} > "$REF/cli.md"

# ---- screenshots, from the test with invented data
SHOTS="$(mktemp -d)"
ORION_SHOT_DIR="$SHOTS" cargo test -q -p orion-tui screenshots -- --ignored >/dev/null
for pair in reader:reader thread:conversations compose:compose original:original; do
  src="${pair%%:*}"; dst="${pair##*:}"
  [ -f "$SHOTS/$src.json" ] && "$VENV/bin/python" scripts/render_shot.py "$SHOTS/$src.json" "wiki/docs/img/$dst.png"
done
rm -rf "$SHOTS"
# The front page shows the README's screenshot (a real session, anonymised).
cp docs/screenshot.png wiki/docs/img/screenshot.png

case "$1" in
  --serve) exec "$VENV/bin/mkdocs" serve -f wiki/mkdocs.yml ;;
esac

"$VENV/bin/mkdocs" build --strict -q -f wiki/mkdocs.yml -d site
echo "built wiki/site"

if [ "$1" = "--upload" ]; then
  rsync -rlt --delete --chmod=D755,F644 wiki/site/ "$DEST"
  echo "uploaded to $DEST"
fi
