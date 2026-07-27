#!/usr/bin/env bash
set -euo pipefail

project_root="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"

if ! command -v serena >/dev/null 2>&1; then
    printf 'serena is not available in PATH\n' >&2
    exit 127
fi

exec serena start-mcp-server \
    --context "$project_root/.serena/chatgpt-readonly-context.yml" \
    --project "$project_root" \
    --enable-web-dashboard false \
    --open-web-dashboard false
