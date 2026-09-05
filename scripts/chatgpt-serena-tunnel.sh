#!/usr/bin/env bash
set -euo pipefail

readonly profile="fastiplayer-chatgpt-readonly"
action="${1:-run}"

case "$action" in
    doctor|run) ;;
    *)
        printf 'usage: %s [doctor|run]\n' "${0##*/}" >&2
        exit 2
        ;;
esac

if ! command -v tunnel-client >/dev/null 2>&1; then
    printf 'tunnel-client is not available in PATH\n' >&2
    exit 127
fi

if [[ -z "${CONTROL_PLANE_API_KEY:-}" ]]; then
    read -r -s -p 'OpenAI Runtime API key: ' CONTROL_PLANE_API_KEY
    printf '\n' >&2
    export CONTROL_PLANE_API_KEY
fi

if [[ -z "$CONTROL_PLANE_API_KEY" ]]; then
    printf 'OpenAI Runtime API key must not be empty\n' >&2
    exit 2
fi

if [[ "$action" == "doctor" ]]; then
    exec tunnel-client doctor --profile "$profile" --explain
fi

exec tunnel-client run --profile "$profile"
