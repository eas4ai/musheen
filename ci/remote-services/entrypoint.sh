#!/usr/bin/env bash
set -euo pipefail

pids=()

cleanup() {
    rm -f /run/musheen-remote-ready
    if ((${#pids[@]})); then
        kill "${pids[@]}" 2>/dev/null || true
        wait "${pids[@]}" 2>/dev/null || true
    fi
}

trap cleanup EXIT TERM INT

/usr/sbin/vsftpd /etc/vsftpd-ftp.conf &
pids+=("$!")
/usr/sbin/vsftpd /etc/vsftpd-ftps.conf &
pids+=("$!")
/usr/sbin/sshd -D -e -f /etc/ssh/sshd_config &
pids+=("$!")
/usr/sbin/apache2ctl -D FOREGROUND &
pids+=("$!")

for _ in $(seq 1 60); do
    if nc -z 127.0.0.1 2121 \
        && nc -z 127.0.0.1 2990 \
        && nc -z 127.0.0.1 2222 \
        && nc -z 127.0.0.1 8080 \
        && nc -z 127.0.0.1 8443
    then
        touch /run/musheen-remote-ready
        break
    fi
    sleep 0.1
done

test -f /run/musheen-remote-ready
wait -n "${pids[@]}"
