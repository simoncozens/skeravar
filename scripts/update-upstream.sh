#!/usr/bin/env bash
set -euo pipefail
UPSTREAM_CLONE=/tmp/skera-upstream-bridge

# Clone or update the bridge
if [ -d "$UPSTREAM_CLONE" ]; then
    git -C "$UPSTREAM_CLONE" fetch origin
    git -C "$UPSTREAM_CLONE" checkout main
else
    git clone https://github.com/googlefonts/fontations "$UPSTREAM_CLONE"
fi

# Re-filter the upstream clone
git -C "$UPSTREAM_CLONE" filter-repo --subdirectory-filter skera/ --force

# Merge into our repo
git remote add upstream-bridge "$UPSTREAM_CLONE" 2>/dev/null || true
git fetch upstream-bridge
git merge upstream-bridge/main --no-edit
