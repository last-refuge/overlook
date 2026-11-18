#!/usr/bin/env bash
# Verify the shipped OAuth clients without printing their values.
set -euo pipefail

binary=${1:?path to the Overlook binary}
root=$(cd "$(dirname "$0")/.." && pwd)
microsoft_id=$(sed -nE 's/.*"client_id"[[:space:]]*:[[:space:]]*"([^"]+)".*/\1/p' \
  "$root/crates/overlook-provider/src/microsoft/microsoft-oauth-client.json")

check() {
  local name=$1 value=$2
  if [[ -z "$value" ]] || ! LC_ALL=C grep -aFq -- "$value" "$binary"; then
    printf 'Missing %s in %s\n' "$name" "$binary" >&2
    exit 1
  fi
}

check "Google OAuth client ID" "${OVERLOOK_GOOGLE_CLIENT_ID:-}"
check "Google OAuth desktop client secret" "${OVERLOOK_GOOGLE_CLIENT_SECRET:-}"
check "Microsoft OAuth client ID" "$microsoft_id"
printf 'Verified Google and Microsoft OAuth clients in %s\n' "$binary"
