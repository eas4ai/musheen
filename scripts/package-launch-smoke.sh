#!/usr/bin/env bash
set -euo pipefail

runtime_dir=$(mktemp -d)
trap 'rm -r -- "$runtime_dir"' EXIT
export XDG_RUNTIME_DIR="$runtime_dir"
export VK_DRIVER_FILES=/usr/share/vulkan/icd.d/lvp_icd.json

xvfb-run -a -s '-screen 0 1280x800x24' dbus-run-session -- bash -c '
    set -euo pipefail
    /usr/bin/musheen /tmp &
    app=$!
    trap '\''kill "$app" 2>/dev/null || true; wait "$app" 2>/dev/null || true'\'' EXIT

    for ((attempt = 0; attempt < 60; attempt++)); do
        if xwininfo -root -tree | grep -F "Musheen" >/dev/null; then
            echo "native graphical launch passed"
            exit 0
        fi
        if ! kill -0 "$app" 2>/dev/null; then
            wait "$app" || true
            echo "Musheen exited before opening a window" >&2
            exit 1
        fi
        sleep 0.5
    done
    echo "Musheen did not open a window within 30 seconds" >&2
    exit 1
'
