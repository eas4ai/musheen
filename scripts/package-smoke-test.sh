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

for size in 48 128 256; do
    icon="$stage/usr/share/icons/hicolor/${size}x${size}/apps/org.musheen.Musheen.png"
    if [[ $(file --brief --mime-type "$icon") != image/png ]]; then
        echo "rasterized icon is not a PNG: $icon" >&2
        exit 1
    fi
done

if [[ ! -x "$stage/usr/bin/musheen" || ! -x "$stage/usr/libexec/musheen-broker" ]]; then
    echo "package is missing an executable application or broker" >&2
    exit 1
fi
if [[ -n $(find "$stage" -type f \( -perm /002 -o -perm /6000 \) -print -quit) ]]; then
    echo "package contains a world-writable or setuid/setgid file" >&2
    exit 1
fi

echo "native package staging smoke passed"
