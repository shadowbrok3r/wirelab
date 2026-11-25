#!/usr/bin/env bash
# Build a desktop app, pack it as a store bundle and publish it for linux-x86_64.
#
#   AS_URL=https://appstore.example AS_KEY=<admin api key> \
#     scripts/publish-desktop.sh <path/to/bundle.env> ["what changed"]
#
# bundle.env sits beside the app and sets, with paths relative to its own folder:
#   SLUG          the store entry the build belongs to (the app's existing catalog slug)
#   PACKAGE       cargo package to build; its version is the published version
#   BIN           executable name; installs as ~/.cargo/bin/$BIN, where `cargo install` puts it
#   APP_ID        launcher id: share/applications/$APP_ID.desktop and the icon name
#   DESKTOP_FILE  the launcher entry; the store points its Exec at the installed binary
#   ICON_PNG      a 192x192 PNG icon
#   ICON_SVG      optional scalable icon
#   WORKSPACE     the cargo workspace to build in (default: the folder bundle.env is in)
#
# A machine only takes a strictly newer version, so publishing the version already in the
# store is refused; bump the package version. FORCE=1 republishes it anyway.
set -euo pipefail

die() { echo "error: $*" >&2; exit 1; }

[ -n "${AS_URL:-}" ] || die "set AS_URL (e.g. https://appstore.shadowbroker.app)"
[ -n "${AS_KEY:-}" ] || die "set AS_KEY (an admin API key)"
[ $# -ge 1 ] || die "usage: publish-desktop.sh <bundle.env> [notes]"

ENV_FILE="$(realpath "$1")"
NOTES="${2:-}"
HERE="$(dirname "$ENV_FILE")"
TARGET_ID=linux-x86_64
# The proxy in front of the store refuses request bodies over 100 MB.
MAX_BYTES=$((95 * 1024 * 1024))

SLUG="" PACKAGE="" BIN="" APP_ID="" DESKTOP_FILE="" ICON_PNG="" ICON_SVG="" WORKSPACE="."
# shellcheck source=/dev/null
source "$ENV_FILE"
for var in SLUG PACKAGE BIN APP_ID DESKTOP_FILE ICON_PNG; do
    [ -n "${!var}" ] || die "$ENV_FILE sets no $var"
done
WS="$(realpath "$HERE/$WORKSPACE")"
[ -f "$HERE/$DESKTOP_FILE" ] || die "no $HERE/$DESKTOP_FILE"
[ -f "$HERE/$ICON_PNG" ] || die "no $HERE/$ICON_PNG"

echo "building $PACKAGE in $WS"
(cd "$WS" && cargo build --release --locked -p "$PACKAGE")

read -r VERSION TARGET_DIR < <(cd "$WS" && cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys
meta = json.load(sys.stdin)
pkg = [p for p in meta["packages"] if p["name"] == sys.argv[1]]
if not pkg:
    sys.exit(f"no package {sys.argv[1]} in this workspace")
print(pkg[0]["version"], meta["target_directory"])
' "$PACKAGE")
BIN_PATH="$TARGET_DIR/release/$BIN"
[ -x "$BIN_PATH" ] || die "no $BIN_PATH after the build"

PUBLISHED=$(curl -sS -H "x-api-key: $AS_KEY" "$AS_URL/$SLUG/desktop/$TARGET_ID/update.json" \
    | python3 -c 'import json, sys; b = json.load(sys.stdin).get("build") or {}; print(b.get("version", ""))' || true)
if [ -n "$PUBLISHED" ] && [ "${FORCE:-0}" != 1 ]; then
    python3 - "$VERSION" "$PUBLISHED" <<'EOF' || die "$SLUG already has $PUBLISHED published; bump $PACKAGE's version (or FORCE=1)"
import sys
def key(v):
    core = v.split("+")[0].split("-")[0]
    return tuple(int(p) for p in core.split(".")) + (0 if "-" in v.split("+")[0] else 1,)
sys.exit(0 if key(sys.argv[1]) > key(sys.argv[2]) else 1)
EOF
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
mkdir -p "$STAGE/root"
install -Dm755 "$BIN_PATH" "$STAGE/root/bin/$BIN"
install -Dm644 "$HERE/$DESKTOP_FILE" "$STAGE/root/share/applications/$APP_ID.desktop"
install -Dm644 "$HERE/$ICON_PNG" "$STAGE/root/share/icons/hicolor/192x192/apps/$APP_ID.png"
if [ -n "$ICON_SVG" ]; then
    [ -f "$HERE/$ICON_SVG" ] || die "no $HERE/$ICON_SVG"
    install -Dm644 "$HERE/$ICON_SVG" "$STAGE/root/share/icons/hicolor/scalable/apps/$APP_ID.svg"
fi
python3 -c 'import json, sys; print(json.dumps(dict(zip(["slug", "bin", "app_id", "version"], sys.argv[1:]))))' \
    "$SLUG" "$BIN" "$APP_ID" "$VERSION" > "$STAGE/root/manifest.json"
BUNDLE="$STAGE/$SLUG-$VERSION-$TARGET_ID.tar.gz"
tar -C "$STAGE/root" --owner=0 --group=0 -czf "$BUNDLE" manifest.json bin share
SIZE=$(stat -c %s "$BUNDLE")
[ "$SIZE" -le "$MAX_BYTES" ] || die "bundle is $SIZE bytes, over the proxy's limit"

QUERY=$(python3 -c 'import sys, urllib.parse as u; print(u.urlencode(dict(zip(["version", "bin", "app_id", "notes"], sys.argv[1:]))))' \
    "$VERSION" "$BIN" "$APP_ID" "$NOTES")
echo "publishing $SLUG $VERSION ($((SIZE / 1024)) KiB) for $TARGET_ID"
curl -sS --fail-with-body -X POST \
    -H "x-api-key: $AS_KEY" -H "content-type: application/gzip" \
    --data-binary @"$BUNDLE" \
    "$AS_URL/$SLUG/desktop/$TARGET_ID/upload?$QUERY"
echo
