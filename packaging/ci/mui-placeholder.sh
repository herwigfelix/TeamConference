#!/usr/bin/env bash
# CI: Platzhalter für RustMUi anlegen. RustMUi (die iOS/Android-Oberfläche)
# liegt nicht in diesem Repo; client/Cargo.toml bindet es nur für iOS/Android
# über den Pfad ../../RustMUi/mui ein. Desktop-Builds kompilieren `mui` nie,
# Cargo muss das Manifest beim Auflösen aber trotzdem lesen können — sonst
# bricht jeder Build mit „failed to load source for dependency `mui`" ab.
# Ein vorhandenes echtes RustMUi bleibt unangetastet.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
DIR="$ROOT/../RustMUi/mui"
if [ -f "$DIR/Cargo.toml" ]; then
    echo "RustMUi vorhanden: $DIR"
    exit 0
fi
mkdir -p "$DIR/src"
cat > "$DIR/Cargo.toml" <<'TOML'
[package]
name = "mui"
version = "0.1.0"
edition = "2021"
publish = false
TOML
: > "$DIR/src/lib.rs"
echo "MUi-Platzhalter angelegt: $DIR"
