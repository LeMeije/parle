#!/bin/bash
# Rebuild the debug bundle and refresh the canonical copy in /Applications.
# Note: until dev builds are signed with the trusted "EchoKey Dev" cert
# (HUMAN_TASKS.md §2), every rebuild orphans the Accessibility grant.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
npm run tauri build -- --debug --bundles app
pkill -x echokey 2>/dev/null || true
rm -rf /Applications/EchoKey.app
cp -R target/debug/bundle/macos/EchoKey.app /Applications/
open /Applications/EchoKey.app
echo "Installed and launched /Applications/EchoKey.app"
