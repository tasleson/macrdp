#!/bin/bash
set -e

BUNDLE_ID="com.macrdp.app"
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
APP_BUNDLE="$SCRIPT_DIR/src-tauri/target/release/bundle/macos/macrdp.app"

echo "==> Building macrdp-ui..."
cd "$SCRIPT_DIR"
npm run tauri build -- --no-bundle 2>/dev/null || cargo build --release --manifest-path src-tauri/Cargo.toml

# Bundle if needed
if command -v cargo-tauri &>/dev/null; then
    cd "$SCRIPT_DIR"
    npm run tauri build 2>/dev/null || true
fi

# Re-sign .app with stable identifier (preserves TCC permissions across rebuilds)
if [ -d "$APP_BUNDLE" ]; then
    echo "==> Re-signing $APP_BUNDLE with identifier $BUNDLE_ID"
    codesign --force --sign - --identifier "$BUNDLE_ID" "$APP_BUNDLE"
    echo "==> Done. Identifier:"
    codesign -d --verbose=1 "$APP_BUNDLE" 2>&1 | grep Identifier
else
    echo "==> No .app bundle found at $APP_BUNDLE"
    echo "    Sign manually: codesign --force --sign - --identifier $BUNDLE_ID /path/to/macrdp.app"
fi
