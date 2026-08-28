#!/bin/bash
# Rebuild the debug bundle and refresh the canonical copy in /Applications.
# Note: until dev builds are signed with the trusted "Parle Dev" cert
# (HUMAN_TASKS.md §2), every rebuild orphans the Accessibility grant.
set -euo pipefail
cd "$(dirname "$0")/.."
export PATH="$HOME/.cargo/bin:$PATH"
npm run tauri build -- --debug --bundles app
pkill -x parle 2>/dev/null || true
rm -rf /Applications/Parle.app
cp -R target/debug/bundle/macos/Parle.app /Applications/
open /Applications/Parle.app

# Remove the build-output bundle: otherwise Spotlight/Launchpad/Raycast index
# TWO Parle apps and you can launch the stale one by accident. /Applications
# is the only copy that should ever be visible.
rm -rf target/debug/bundle/macos/Parle.app target/release/bundle/macos/Parle.app

echo "Installed and launched /Applications/Parle.app (build bundle cleaned up)"
