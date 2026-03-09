#!/bin/bash
set -e

# Usage: ./build.sh [mac|linux|windows]
PLATFORM=$1

if [ -z "$PLATFORM" ]; then
    echo "Usage: ./build.sh [mac|linux|windows]"
    
    # Auto-detect platform
    UNAME=$(uname -s)
    if [ "$UNAME" = "Darwin" ]; then
        PLATFORM="mac"
    elif [ "$UNAME" = "Linux" ]; then
        PLATFORM="linux"
    elif [[ "$UNAME" == MINGW* || "$UNAME" == CYGWIN* ]]; then
        PLATFORM="windows"
    else
        echo "Could not auto-detect platform. Please specify manually."
        exit 1
    fi
    echo "Auto-detected platform as: $PLATFORM"
fi

BIN_DIR="bin"
mkdir -p "$BIN_DIR"

echo "=== Packaging pdf_cutter for $PLATFORM ==="

echo "1. Building Rust CLI..."
cargo build --release

echo "3. Copying binary..."
cp target/release/pdf_cutter "$BIN_DIR/"

echo "4. Cleaning up other platform libraries..."
if [ "$PLATFORM" = "mac" ]; then
    rm -f "$BIN_DIR/libpdfium.so" "$BIN_DIR/pdfium.dll"
elif [ "$PLATFORM" = "linux" ]; then
    rm -f "$BIN_DIR/libpdfium.dylib" "$BIN_DIR/pdfium.dll"
elif [ "$PLATFORM" = "windows" ]; then
    rm -f "$BIN_DIR/libpdfium.dylib" "$BIN_DIR/libpdfium.so"
fi

echo "5. Checking pdfium library..."
PDFIUM_REPO="https://github.com/bblanchon/pdfium-binaries/releases/latest/download"

if [ "$PLATFORM" = "mac" ]; then
    ARCH=$(uname -m)
    if [ "$ARCH" = "arm64" ]; then
        URL="$PDFIUM_REPO/pdfium-mac-arm64.tgz"
    else
        URL="$PDFIUM_REPO/pdfium-mac-x64.tgz"
    fi
    LIB_EXT="dylib"
    FILE="pdfium.tgz"

    if [ -f "$BIN_DIR/libpdfium.$LIB_EXT" ]; then
        echo "libpdfium.$LIB_EXT already exists in $BIN_DIR. Skipping download."
    else
        echo "Downloading libpdfium.$LIB_EXT for Mac..."
        curl -L -o "$FILE" "$URL"
        tar -xzf "$FILE" "lib/libpdfium.$LIB_EXT"
        mv "lib/libpdfium.$LIB_EXT" "$BIN_DIR/"
        rm -rf lib "$FILE"
    fi

elif [ "$PLATFORM" = "linux" ]; then
    URL="$PDFIUM_REPO/pdfium-linux-x64.tgz"
    LIB_EXT="so"
    FILE="pdfium.tgz"

    if [ -f "$BIN_DIR/libpdfium.$LIB_EXT" ]; then
        echo "libpdfium.$LIB_EXT already exists in $BIN_DIR. Skipping download."
    else
        echo "Downloading libpdfium.$LIB_EXT for Linux..."
        curl -L -o "$FILE" "$URL"
        tar -xzf "$FILE" "lib/libpdfium.$LIB_EXT"
        mv "lib/libpdfium.$LIB_EXT" "$BIN_DIR/"
        rm -rf lib "$FILE"
    fi

elif [ "$PLATFORM" = "windows" ]; then
    URL="$PDFIUM_REPO/pdfium-windows-x64.zip"
    FILE="pdfium.zip"

    if [ -f "$BIN_DIR/pdfium.dll" ]; then
        echo "pdfium.dll already exists in $BIN_DIR. Skipping download."
    else
        echo "Downloading pdfium.dll for Windows..."
        # Make sure we add unzip command if not available natively (Windows usually needs powershell)
        curl -L -o "$FILE" "$URL"
        unzip -j "$FILE" "bin/pdfium.dll" -d "$BIN_DIR"
        rm -f "$FILE"
    fi
else
    echo "Unsupported platform specified: $PLATFORM"
    exit 1
fi

echo "=== Done! ==="
echo "The standalone CLI is ready in the './$BIN_DIR' folder."
echo "You can move this folder anywhere and run './$BIN_DIR/pdf_cutter'."
