#!/usr/bin/env bash
# Laadt de Capture Browser Bridge (Firefox-variant) tijdelijk in Firefox,
# met automatische herlaad bij bestandswijzigingen.
#
#   ./install.sh                # web-ext run: start Firefox met de extensie
#   ./install.sh --firefox /pad/naar/firefox
#   ./install.sh --no-reload    # geen auto-herlaad, alleen installeren
#
# Vereist Node.js (npm). Installeert web-ext ad-hoc als het ontbreekt:
#   npm install -g web-ext
#
# Auto-herlaad is web-ext's --reload, dat standaard al aan staat; extra
# argumenten gaan 1-op-1 naar web-ext run door.
#
# "Tijdelijk" betekent: verdwijnt bij sluiten. Voor permanente
# installatie is ondertekening via addons.mozilla.org nodig.

set -euo pipefail

EXT_DIR="$(cd "$(dirname "$0")" && pwd)"
FIREFOX_BIN=""

while [ $# -gt 0 ]; do
    case "$1" in
        --firefox) FIREFOX_BIN="$2"; shift 2 ;;
        *) break ;;
    esac
done

if ! command -v web-ext >/dev/null 2>&1; then
    echo "web-ext ontbreekt — installeer ad-hoc..."
    if ! command -v npm >/dev/null 2>&1; then
        echo "Vereist Node.js/npm. Installeer van https://nodejs.org en draai opnieuw." >&2
        exit 1
    fi
    npm install -g web-ext
fi

ARGS=(run --source-dir "$EXT_DIR")
[ -n "$FIREFOX_BIN" ] && ARGS+=(--firefox "$FIREFOX_BIN")

echo "Extensie : $EXT_DIR"
echo "web-ext  : ${ARGS[*]} $*"

exec web-ext "${ARGS[@]}" "$@"
