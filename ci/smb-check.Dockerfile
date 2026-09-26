FROM rust:1.95-bookworm

RUN apt-get update \
    && apt-get install --yes --no-install-recommends \
        libacl1-dev \
        libsmbclient-dev \
        pkg-config \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /work
