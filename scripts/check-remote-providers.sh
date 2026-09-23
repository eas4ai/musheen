#!/usr/bin/env bash
set -euo pipefail

repository=$(git rev-parse --show-toplevel)
image=musheen-remote-services:local
container="musheen-remote-ci-$$"
certificate_file=
docker_config=${DOCKER_CONFIG:-${XDG_CONFIG_HOME:-${HOME}/.config}/docker-hub/config}
export DOCKER_CONFIG="$docker_config"

cleanup() {
    docker rm --force "$container" >/dev/null 2>&1 || true
    if [[ -n "$certificate_file" ]]; then
        rm -f "$certificate_file"
    fi
}

trap cleanup EXIT

docker build \
    --file "$repository/ci/remote-services.Dockerfile" \
    --tag "$image" \
    "$repository"

docker run --detach \
    --name "$container" \
    --publish 127.0.0.1:32121:2121 \
    --publish 127.0.0.1:32990:2990 \
    --publish 127.0.0.1:32222:2222 \
    --publish 127.0.0.1:38080:8080 \
    --publish 127.0.0.1:38443:8443 \
    --publish 127.0.0.1:30000-30009:30000-30009 \
    --publish 127.0.0.1:30100-30109:30100-30109 \
    "$image" >/dev/null

for _ in $(seq 1 120); do
    health=$(docker inspect --format '{{.State.Health.Status}}' "$container")
    case "$health" in
        healthy) break ;;
        unhealthy)
            docker logs "$container"
            exit 1
            ;;
    esac
    sleep 0.25
done

test "$(docker inspect --format '{{.State.Health.Status}}' "$container")" = healthy

certificate_file=$(mktemp "${TMPDIR:-/tmp}/musheen-remote-ca.XXXXXX.pem")
docker cp "$container:/etc/ssl/certs/musheen-remote-ca.crt" "$certificate_file" >/dev/null

tls_sha256=$(
    docker exec "$container" \
        openssl x509 -in /etc/ssl/certs/musheen-remote-ci.crt -outform DER \
        | sha256sum \
        | awk '{print $1}'
)
ssh_sha256=$(
    docker exec "$container" \
        awk '{print $2}' /etc/ssh/ssh_host_ed25519_key.pub \
        | base64 --decode \
        | sha256sum \
        | awk '{print $1}'
)

cd "$repository"
MUSHEEN_LIVE_HTTP_URL=http://127.0.0.1:38080/ \
MUSHEEN_LIVE_WEBDAV_URL=https://127.0.0.1:38443/ \
MUSHEEN_LIVE_FTP_ENDPOINT=ftp://127.0.0.1:32121 \
MUSHEEN_LIVE_FTPS_ENDPOINT=ftps://127.0.0.1:32990 \
MUSHEEN_LIVE_SFTP_ENDPOINT=ssh://127.0.0.1:32222 \
MUSHEEN_LIVE_USERNAME=musheen \
MUSHEEN_LIVE_PASSWORD=musheen-pass \
MUSHEEN_LIVE_TLS_SHA256="$tls_sha256" \
MUSHEEN_LIVE_SSH_SHA256="$ssh_sha256" \
MUSHEEN_REMOTE_CONTAINER="$container" \
MUSHEEN_REMOTE_LIVE=1 \
SSL_CERT_FILE="$certificate_file" \
CARGO_BUILD_JOBS=8 \
cargo test -p musheen-desktop --test remote_live_contract --locked -- --nocapture
