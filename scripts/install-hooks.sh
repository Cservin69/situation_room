#!/usr/bin/env bash
# Install git hooks for local security checks.
set -euo pipefail
cd "$(dirname "$0")/.."

mkdir -p .git/hooks
cat > .git/hooks/pre-commit << 'HOOK'
#!/usr/bin/env bash
# Block commits containing likely secret patterns.
set -e

if git diff --cached --name-only | xargs -I {} grep -lE '(sk-ant-[A-Za-z0-9_-]{20,}|sk-proj-[A-Za-z0-9_-]{40,}|xai-[A-Za-z0-9]{20,}|AIza[A-Za-z0-9_-]{35}|-----BEGIN [A-Z ]+PRIVATE KEY-----)' 2>/dev/null | grep -v .env.example; then
    echo "ERROR: potential secret detected in staged changes. Commit blocked."
    echo "If this is a false positive, bypass with: git commit --no-verify"
    exit 1
fi

# Run fmt check on staged Rust files
if git diff --cached --name-only | grep -qE '\.rs$'; then
    cargo fmt --all -- --check || { echo "cargo fmt failed"; exit 1; }
fi
HOOK
chmod +x .git/hooks/pre-commit
echo "Installed pre-commit hook: blocks commits containing API-key-like patterns."
