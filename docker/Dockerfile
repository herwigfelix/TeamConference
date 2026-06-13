# TeamConference Server
# Build:  docker build -f docker/Dockerfile -t teamconference-server .
#         (aus dem Projekt-Wurzelverzeichnis)
# Aktuelles stabiles Rust (einige Abhängigkeiten brauchen Edition 2024 → ≥ 1.85)
FROM rust:1 AS builder
WORKDIR /app
COPY server/ .
# Cargo-Caches zwischen Builds wiederverwenden
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/app/target \
    cargo build --release && \
    cp target/release/teamconference-server /usr/local/bin/teamconference-server

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y openssl ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/local/bin/teamconference-server /usr/local/bin/
WORKDIR /
EXPOSE 9500/tcp 9501/udp
VOLUME ["/data", "/certs"]
# Konfiguration komplett über TC_*-Umgebungsvariablen (siehe README bzw.
# docker-compose.yml). Optional kann eine config.toml nach
# /etc/teamconference/config.toml gemountet werden — Umgebungsvariablen
# haben Vorrang; fehlt die Datei, gelten die eingebauten Defaults.
ENTRYPOINT ["teamconference-server", "--config", "/etc/teamconference/config.toml"]
