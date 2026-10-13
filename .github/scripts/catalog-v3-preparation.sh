#!/usr/bin/env bash
# Restore the exact signed preparation from this workflow run before any
# merge-base lookup or signing. A rerun must never regenerate uploaded bundles.
set -euo pipefail

[[ "${1:-}" == restore ]] || { echo "usage: $0 restore DIST" >&2; exit 2; }
dist="${2:?dist is required}"
name="signed-catalog-preparation-${GITHUB_RUN_ID:?}"
metadata="$(gh api --method GET "repos/${GITHUB_REPOSITORY:?}/actions/runs/${GITHUB_RUN_ID}/artifacts" -f name="${name}")"
state="$(printf '%s' "${metadata}" | python3 -c '
import json, sys
items = json.load(sys.stdin)["artifacts"]
if len(items) > 1:
    sys.exit("multiple signed preparations for this run")
print("expired" if items and items[0]["expired"] else "present" if items else "absent")
')"
case "${state}" in
  present)
    gh run download "${GITHUB_RUN_ID}" --repo "${GITHUB_REPOSITORY}" --name "${name}" --dir "${dist}"
    echo "restored=true" >> "${GITHUB_OUTPUT:?}"
    ;;
  absent)
    # The preparation is saved before anything is published, so an absent
    # artifact means no earlier attempt of this run uploaded a byte. Signing
    # afresh is safe, on a first attempt and on a rerun alike.
    echo "restored=false" >> "${GITHUB_OUTPUT:?}"
    ;;
  *)
    echo "Saved signed preparation expired; explicit publication recovery is required." >&2
    exit 1
    ;;
esac
