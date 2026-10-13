#!/usr/bin/env bash

set -euo pipefail

# Prints one `plugin_id<TAB>version<TAB>commit` row per plugin component a
# release trigger publishes.
#
# A trigger created by `xtask release-publish-tags` lists every plugin
# component in its signed tag body as `component <tag> <commit>` so one
# trigger can publish several lines of one plugin, each built from its own
# commit (own lockfile, own SDK) by the trigger's xtask. A trigger without
# such lines publishes the plugin tags that point at the release commit.
#
# Every row is checked against the repository: the component tag must be
# annotated, point at the listed commit, and that commit must be an ancestor
# of the release commit. With VERIFY=true the tag must also be
# signature-verified by GitHub (gh api), as the trigger tag itself is.
release_plugin_components() {
  local trigger_tag="${1:-}"
  local release_sha="${2:?release commit is required}"
  local repository="${3:?repository is required}"
  local verify="${4:-false}"
  local prefix="${SCRYER_OFFICIAL_PLUGIN_RELEASE_TAG_PREFIX:-plugins-v3}"

  if [[ ! "${release_sha}" =~ ^[0-9a-f]{40}$ ]]; then
    printf 'Release commit must be a full sha: %s\n' "${release_sha}" >&2
    return 1
  fi

  local -a rows=()
  local line tag sha extra
  if [[ -n "${trigger_tag}" ]]; then
    while IFS= read -r line; do
      [[ "${line}" == component\ * ]] || continue
      read -r _ tag sha extra <<<"${line}"
      if [[ -z "${tag:-}" || -z "${sha:-}" || -n "${extra:-}" ]]; then
        printf 'Malformed component line in %s: %s\n' "${trigger_tag}" "${line}" >&2
        return 1
      fi
      rows+=("${tag}"$'\t'"${sha}")
    done < <(git tag -l --format='%(contents:body)' "${trigger_tag}")
  fi
  if (( ${#rows[@]} == 0 )); then
    while IFS= read -r tag; do
      [[ -n "${tag}" ]] || continue
      rows+=("${tag}"$'\t'"${release_sha}")
    done < <(git tag --points-at "${release_sha}" | grep -E "^${prefix}/[^/]+/v.+$" | sort || true)
  fi

  local row spec plugin_id version tag_object resolved verified
  local seen=$'\n'
  local -a output=()
  for row in ${rows[@]+"${rows[@]}"}; do
    tag="${row%%$'\t'*}"
    sha="${row#*$'\t'}"
    if [[ "${tag}" != "${prefix}"/*/v* ]]; then
      printf 'Component tag is not a plugin release tag: %s\n' "${tag}" >&2
      return 1
    fi
    spec="${tag#"${prefix}"/}"
    plugin_id="${spec%%/*}"
    version="${spec#*/v}"
    if [[ ! "${plugin_id}" =~ ^[a-z0-9][a-z0-9-]*$ || -z "${version}" || "${version}" == */* ]]; then
      printf 'Component tag carries an unsafe id or version: %s\n' "${tag}" >&2
      return 1
    fi
    if [[ ! "${sha}" =~ ^[0-9a-f]{40}$ ]]; then
      printf 'Component %s lists an invalid commit: %s\n' "${tag}" "${sha}" >&2
      return 1
    fi
    if [[ "${seen}" == *$'\n'"${tag}"$'\n'* ]]; then
      printf 'Component %s is listed twice\n' "${tag}" >&2
      return 1
    fi
    seen+="${tag}"$'\n'
    if ! tag_object="$(git rev-parse --verify --quiet "refs/tags/${tag}^{tag}")"; then
      printf 'Component %s is not an annotated tag\n' "${tag}" >&2
      return 1
    fi
    resolved="$(git rev-parse --verify --quiet "refs/tags/${tag}^{commit}")"
    if [[ "${resolved}" != "${sha}" ]]; then
      printf 'Component %s points at %s, not the listed %s\n' "${tag}" "${resolved}" "${sha}" >&2
      return 1
    fi
    if ! git merge-base --is-ancestor "${sha}" "${release_sha}"; then
      printf 'Component %s commit %s is not an ancestor of the release commit %s\n' "${tag}" "${sha}" "${release_sha}" >&2
      return 1
    fi
    if [[ "${verify}" == true ]]; then
      verified="$(gh api "repos/${repository}/git/tags/${tag_object}" --jq '.verification.verified')"
      if [[ "${verified}" != true ]]; then
        printf 'Component %s is not a GitHub-verified signed tag\n' "${tag}" >&2
        return 1
      fi
    fi
    output+=("${plugin_id}"$'\t'"${version}"$'\t'"${sha}")
  done
  # Nothing is printed until every row has passed, so a consumer never sees
  # a partial plan.
  for row in ${output[@]+"${output[@]}"}; do
    printf '%s\n' "${row}"
  done
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  release_plugin_components "$@"
fi
