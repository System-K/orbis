#!/usr/bin/env bash
# =============================================================================
# Orbis — Release-Packaging (Linux / Git Bash on Windows)
# =============================================================================
# Baut ein Release-Binary und packt alles in einen verteilbaren Ordner.
#
# Nutzung:
#   ./scripts/package.sh               # Release-Build + Packaging
#   ./scripts/package.sh --skip-build  # Nur Packaging (Binary muss existieren)
#
# Ergebnis: release/orbis-<OS>-<ARCH>/
#   ├── orbis(.exe)
#   ├── assets/
#   │   ├── shaders/
#   │   └── textures/
#   ├── README.md
#   └── LICENSE
# =============================================================================

set -euo pipefail

SKIP_BUILD=false
if [[ "${1:-}" == "--skip-build" ]]; then
    SKIP_BUILD=true
fi

# OS und Architektur erkennen
OS="$(uname -s)"
ARCH="$(uname -m)"

case "$OS" in
    Linux*)                           OS_NAME="linux" ;;
    Darwin*)                          OS_NAME="macos" ;;
    MINGW*|MSYS*|CYGWIN*)            OS_NAME="windows" ;;
    *)                                OS_NAME="unknown" ;;
esac

case "$ARCH" in
    x86_64|amd64)  ARCH_NAME="x86_64" ;;
    aarch64|arm64) ARCH_NAME="aarch64" ;;
    *)             ARCH_NAME="$ARCH" ;;
esac

RELEASE_NAME="orbis-${OS_NAME}-${ARCH_NAME}"
RELEASE_DIR="release/${RELEASE_NAME}"
BINARY_NAME="orbis"

if [[ "$OS_NAME" == "windows" ]]; then
    BINARY_NAME="orbis.exe"
fi

echo "=== Orbis Release: ${RELEASE_NAME} ==="

# Build
if [[ "$SKIP_BUILD" == false ]]; then
    echo ">>> cargo build --release"
    cargo build --release
fi

# Binary prüfen
BINARY_SRC="target/release/${BINARY_NAME}"
if [[ ! -f "$BINARY_SRC" ]]; then
    echo "FEHLER: Binary nicht gefunden: $BINARY_SRC"
    echo "Führe zuerst 'cargo build --release' aus."
    exit 1
fi

# Release-Ordner erstellen
echo ">>> Erstelle ${RELEASE_DIR}/"
rm -rf "$RELEASE_DIR"
mkdir -p "$RELEASE_DIR"

# Binary kopieren
cp "$BINARY_SRC" "$RELEASE_DIR/"

# Assets kopieren (Shader + Texturen)
cp -r assets "$RELEASE_DIR/"

# Dokumentation kopieren
cp README.md "$RELEASE_DIR/" 2>/dev/null || echo "(README.md noch nicht vorhanden)"
cp LICENSE "$RELEASE_DIR/" 2>/dev/null || echo "(LICENSE noch nicht vorhanden)"

# Archiv erstellen
echo ">>> Erstelle Archiv..."
cd release
if [[ "$OS_NAME" == "windows" ]]; then
    if command -v 7z &>/dev/null; then
        7z a "${RELEASE_NAME}.zip" "$RELEASE_NAME/" -mx=9
    elif command -v zip &>/dev/null; then
        zip -r "${RELEASE_NAME}.zip" "$RELEASE_NAME/"
    else
        echo "(Kein zip/7z — Ordner ohne Archiv erstellt)"
    fi
else
    tar -czf "${RELEASE_NAME}.tar.gz" "$RELEASE_NAME/"
fi
cd ..

# Statistik
BINARY_SIZE=$(du -h "$RELEASE_DIR/$BINARY_NAME" | cut -f1)
echo ""
echo "=== Fertig ==="
echo "Binary:  ${BINARY_SIZE}"
echo "Ordner:  ${RELEASE_DIR}/"
echo ""
ls -lh "$RELEASE_DIR/"
