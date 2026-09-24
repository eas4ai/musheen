# syntax=docker/dockerfile:1.7
FROM archlinux:base-devel AS build

RUN pacman -Syu --needed --noconfirm \
      appstream adwaita-fonts cargo dbus desktop-file-utils file fontconfig freetype2 git hicolor-icon-theme jq \
      libarchive librsvg libxcb libxkbcommon libxkbcommon-x11 \
      namcap pkgconf polkit python rust smbclient udisks2 vulkan-icd-loader wayland \
    && pacman -Scc --noconfirm

RUN useradd --create-home --uid 1000 builder
COPY . /work/musheen-0.1.0
COPY packaging/arch/PKGBUILD /work/PKGBUILD
RUN tar -C /work -czf /work/musheen-0.1.0.tar.gz musheen-0.1.0 \
    && digest=$(sha256sum /work/musheen-0.1.0.tar.gz | cut -d ' ' -f 1) \
    && sed -i "s/__SOURCE_SHA256__/$digest/" /work/PKGBUILD \
    && chown -R builder:builder /work

USER builder
WORKDIR /work
ENV CARGO_BUILD_JOBS=8 CARGO_INCREMENTAL=0 \
    CARGO_HOME=/home/builder/.cargo CARGO_TARGET_DIR=/work/target \
    RUST_TEST_THREADS=1 RUST_MIN_STACK=16777216
RUN --mount=type=cache,target=/home/builder/.cargo,uid=1000,gid=1000 \
    --mount=type=cache,target=/work/target,uid=1000,gid=1000 \
    makepkg --noconfirm

USER root
RUN namcap /work/musheen-*.pkg.tar.zst \
    && install -d /export \
    && cp /work/musheen-*.pkg.tar.zst /export/

FROM archlinux:base-devel AS runtime-check
COPY --from=build /export/ /export/
COPY scripts/package-launch-smoke.sh /usr/local/bin/package-launch-smoke.sh
RUN pacman -Syu --noconfirm xorg-server-xvfb xorg-xwininfo vulkan-swrast \
    && pacman -U --noconfirm /export/musheen-*.pkg.tar.zst \
    && test -x /usr/bin/musheen \
    && test -x /usr/bin/musheen-archive-worker \
    && test -x /usr/bin/musheen-thumbnail-worker \
    && test -x /usr/lib/musheen/musheen-broker \
    && test -f /usr/share/applications/org.musheen.Musheen.desktop \
    && ! ldd /usr/bin/musheen | grep -q 'not found' \
    && ! ldd /usr/bin/musheen-archive-worker | grep -q 'not found' \
    && ! ldd /usr/bin/musheen-thumbnail-worker | grep -q 'not found' \
    && ! ldd /usr/lib/musheen/musheen-broker | grep -q 'not found' \
    && useradd --create-home smoke \
    && timeout --signal=TERM --kill-after=5s 45s \
       runuser --user smoke -- bash /usr/local/bin/package-launch-smoke.sh \
    && pacman -Rns --noconfirm musheen \
    && test ! -e /usr/bin/musheen \
    && test ! -e /usr/bin/musheen-archive-worker \
    && test ! -e /usr/bin/musheen-thumbnail-worker \
    && test ! -e /usr/lib/musheen/musheen-broker

FROM scratch AS artifact
COPY --from=runtime-check /export/ /
