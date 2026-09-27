#!/usr/bin/env bash
set -euo pipefail

repository_root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
scratch_base=${MUSHEEN_SCRATCH_BASE:-/tmp}
if [[ ! -d "$scratch_base" ]]; then
    echo "package scratch base does not exist: $scratch_base" >&2
    exit 1
fi
stage=$(mktemp -d -p "$scratch_base" musheen-package-XXXXXX)
trap 'rm -r -- "$stage"' EXIT

DESTDIR="$stage" "$repository_root/packaging/install-app.sh"
desktop-file-validate "$stage/usr/share/applications/org.musheen.Musheen.desktop"
appstreamcli validate --no-net \
    "$stage/usr/share/metainfo/org.musheen.Musheen.metainfo.xml"

private_service="$stage/usr/share/dbus-1/services/org.musheen.Musheen.service"
if [[ ! -f "$private_service" ]] \
    || ! grep -Fxq 'Name=org.musheen.Musheen' "$private_service" \
    || ! grep -Fxq 'Exec=/usr/bin/musheen' "$private_service"; then
    echo "package is missing Musheen D-Bus activation" >&2
    exit 1
fi
portal_service="$stage/usr/share/dbus-1/services/org.freedesktop.impl.portal.desktop.musheen.service"
if [[ ! -f "$stage/usr/share/xdg-desktop-portal/portals/musheen.portal" ]] \
    || [[ ! -f "$portal_service" ]] \
    || ! grep -Fxq 'Exec=/usr/bin/musheen --portal-backend' "$portal_service"; then
    echo "package is missing the portal backend's files" >&2
    exit 1
fi
if [[ -e "$stage/usr/share/dbus-1/services/org.freedesktop.FileManager1.service" ]]; then
    echo "package must not replace another file manager's D-Bus activation" >&2
    exit 1
fi

for size in 48 128 256; do
    icon="$stage/usr/share/icons/hicolor/${size}x${size}/apps/org.musheen.Musheen.png"
    if [[ $(file --brief --mime-type "$icon") != image/png ]]; then
        echo "rasterized icon is not a PNG: $icon" >&2
        exit 1
    fi
done

for executable in \
    "$stage/usr/bin/musheen" \
    "$stage/usr/bin/musheen-archive-worker" \
    "$stage/usr/bin/musheen-thumbnail-worker" \
    "$stage/usr/lib/musheen/musheen-broker"; do
    if [[ ! -x "$executable" ]]; then
        echo "package is missing executable: $executable" >&2
        exit 1
    fi
done
if [[ -n $(find "$stage" -type f \( -perm /002 -o -perm /6000 \) -print -quit) ]]; then
    echo "package contains a world-writable or setuid/setgid file" >&2
    exit 1
fi

echo "native package staging smoke passed"
