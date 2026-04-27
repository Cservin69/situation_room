#!/usr/bin/env bash
# Stockpile delta zip generator — respects .gitignore + only relevant changes

set -euo pipefail

cd "$(dirname "$0")"

DATE=$(date +"%Y-%m-%d")
SHORT_HASH=$(git rev-parse --short HEAD)
COMMIT_MSG=$(git log -1 --pretty=format:%s 2>/dev/null || echo "changes")
MSG_SLUG=$(echo "$COMMIT_MSG" | tr '[:upper:]' '[:lower:]' | tr -cd '[:alnum:]_-' | cut -c1-30)

ZIP_NAME="stockpile_delta_${DATE}_${SHORT_HASH}_${MSG_SLUG}.zip"

# Get only tracked files that changed in the last commit (respects .gitignore naturally)
CHANGED_FILES=$(git diff --name-only --diff-filter=ACMRT HEAD~1 HEAD)

if [ -z "$CHANGED_FILES" ]; then
    echo "No changes detected since last commit."
    exit 0
fi

# Optional: further filter to only meaningful files (you can adjust this)
CHANGED_FILES=$(echo "$CHANGED_FILES" | grep -E '\.(rs|toml|md|sql|json|ts|js)$|^crates/|^config/|^docs/|^apps/|^migrations/|^tests/' || true)

if [ -z "$CHANGED_FILES" ]; then
    echo "No relevant source/config/doc changes detected."
    exit 0
fi

echo "Creating delta zip with $(echo "$CHANGED_FILES" | wc -l | tr -d ' ') files (respects .gitignore)..."
echo "→ $ZIP_NAME"

git archive --format=zip \
    --prefix=code/ \
    HEAD \
    $CHANGED_FILES \
    -o "../${ZIP_NAME}"

echo "✅ Delta zip created: ../${ZIP_NAME}"
echo "   Ready to upload for QA review!"