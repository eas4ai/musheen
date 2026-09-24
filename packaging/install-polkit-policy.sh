#!/bin/sh
set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repository_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
broker_binary=${MUSHEEN_BROKER_BINARY:-"$repository_root/target/release/musheen-broker"}
destination_root=${DESTDIR:-}

install -D -m 0755 \
    "$broker_binary" \
    "$destination_root/usr/lib/musheen/musheen-broker"
install -D -m 0644 \
    "$script_dir/polkit/org.musheen.Musheen.policy" \
    "$destination_root/usr/share/polkit-1/actions/org.musheen.Musheen.policy"
