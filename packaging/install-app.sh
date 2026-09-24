#!/bin/sh
set -eu

: "${DESTDIR:?set DESTDIR to an isolated package staging directory}"
case "$DESTDIR" in
    /*) ;;
    *) echo "DESTDIR must be an absolute staging directory" >&2; exit 1 ;;
esac
if [ "$DESTDIR" = / ]; then
    echo "DESTDIR must not be the filesystem root" >&2
    exit 1
fi
if [ -L "$DESTDIR" ]; then
    echo "DESTDIR must not be a symlink" >&2
    exit 1
fi
DESTDIR=$(realpath -m -- "$DESTDIR")
if [ "$DESTDIR" = / ]; then
    echo "DESTDIR resolves to the filesystem root" >&2
    exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
app_binary=${MUSHEEN_APP_BINARY:-"$repository_root/target/release/musheen"}
broker_binary=${MUSHEEN_BROKER_BINARY:-"$repository_root/target/release/musheen-broker"}
archive_worker_binary=${MUSHEEN_ARCHIVE_WORKER_BINARY:-"$repository_root/target/release/musheen-archive-worker"}
thumbnail_worker_binary=${MUSHEEN_THUMBNAIL_WORKER_BINARY:-"$repository_root/target/release/musheen-thumbnail-worker"}
icon="$repository_root/assets/icons/musheen.svg"

install -D -m 0755 "$app_binary" "$DESTDIR/usr/bin/musheen"
install -D -m 0755 "$archive_worker_binary" "$DESTDIR/usr/bin/musheen-archive-worker"
install -D -m 0755 "$thumbnail_worker_binary" "$DESTDIR/usr/bin/musheen-thumbnail-worker"
install -D -m 0644 \
    "$script_dir/org.musheen.Musheen.desktop" \
    "$DESTDIR/usr/share/applications/org.musheen.Musheen.desktop"
install -D -m 0644 \
    "$script_dir/org.musheen.Musheen.metainfo.xml" \
    "$DESTDIR/usr/share/metainfo/org.musheen.Musheen.metainfo.xml"
install -D -m 0644 \
    "$script_dir/org.musheen.Musheen.service" \
    "$DESTDIR/usr/share/dbus-1/services/org.musheen.Musheen.service"
install -D -m 0644 \
    "$icon" \
    "$DESTDIR/usr/share/icons/hicolor/scalable/apps/org.musheen.Musheen.svg"

for size in 48 128 256; do
    directory="$DESTDIR/usr/share/icons/hicolor/${size}x${size}/apps"
    install -d -m 0755 "$directory"
    raster="$directory/org.musheen.Musheen.png"
    rsvg-convert -w "$size" -h "$size" -o "$raster" "$icon"
    chmod 0644 "$raster"
done

DESTDIR="$DESTDIR" MUSHEEN_BROKER_BINARY="$broker_binary" \
    "$script_dir/install-polkit-policy.sh"
