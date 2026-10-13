#!/usr/bin/env bash
# Resolve the central catalog v3 merge base.
#
# Source this file after release-plugin-v3-identity.sh, then call
# resolve_catalog_v3_merge_base EXISTING_DIR REPOSITORY. On success it sets:
#   catalog_asset     file name of the verified master catalog to merge onto,
#                     downloaded to EXISTING_DIR/<catalog_asset>
#   merge_base_asset  file name the live cdn modern redirect currently serves;
#                     the publisher compare-and-sets against it
#
# Supported clients read the modern redirect from the cdn first and from the
# GitHub catalog release second, so both copies must name the same generation
# before anything merges onto either. The legacy redirect ladder is frozen and
# is never read: its rungs are per-stratum projections, and merging onto one
# drops every release the projection filtered out.
#
# Inputs (environment):
#   CF_R2_PUBLIC_BASE_URL            cdn base, e.g. https://cdn.scryer.media/scryer
#   SCRYER_CATALOG_V3_PATH_PREFIX    cdn path of the catalog (default catalog/v3)
#   SCRYER_CATALOG_V3_RELEASE_TAG    GitHub release of the catalog (default catalog/v3)
#   SCRYER_CATALOG_MERGE_BASE_ASSET  operator override: merge onto this signed
#                                    master catalog asset instead
#   CATALOG_V3_RETRY_DELAY           seconds per retry step (default 10)
#
# Callers run with errexit (set -e); any failed download or verification ends
# the step before a merge base is chosen.

readonly CATALOG_V3_MODERN_REDIRECT='catalog-v3.modern.redirect.json'
readonly CATALOG_V3_MODERN_REDIRECT_BUNDLE='catalog-v3.modern.redirect.bundle.json'

catalog_v3_retry() {
  local attempt
  for attempt in 1 2 3 4 5; do
    if "$@"; then
      return 0
    fi
    printf 'attempt %s/5 failed: %s\n' "${attempt}" "$*" >&2
    sleep $(( attempt * ${CATALOG_V3_RETRY_DELAY:-10} ))
  done
  return 1
}

catalog_v3_cdn_base() {
  printf '%s/%s\n' \
    "${CF_R2_PUBLIC_BASE_URL:?CF_R2_PUBLIC_BASE_URL is required}" \
    "${SCRYER_CATALOG_V3_PATH_PREFIX:-catalog/v3}"
}

catalog_v3_fetch() {
  local url="${1:?url is required}"
  local destination="${2:?destination is required}"
  # A peer that connects and then stalls must fail the attempt (and the
  # retry loop above it), not hold the publish slot until the job times out.
  curl -fsSL --proto '=https' --tlsv1.2 --connect-timeout 30 --max-time 600 \
    -o "${destination}" "${url}"
}

catalog_v3_release_download() {
  local tag="${1:?release tag is required}"
  local destination="${2:?destination is required}"
  local repository="${3:?repository is required}"
  shift 3
  gh release download "${tag}" \
    --repo "${repository}" \
    --dir "${destination}" \
    --clobber \
    "$@"
}

catalog_v3_mirror_download() {
  local destination="${1:?destination is required}"
  local repository="${2:?repository is required}"
  shift 2
  catalog_v3_release_download "${SCRYER_CATALOG_V3_RELEASE_TAG:-catalog/v3}" \
    "${destination}" "${repository}" "$@"
}

# Masters published since generation releases began are mirrored on the
# release-trigger-backed generation release that staged them, not on the
# redirect release. Print the tag of the generation release carrying ASSET,
# or nothing when no generation release has it (a pre-generation snapshot).
catalog_v3_generation_release_for_asset() {
  local repository="${1:?repository is required}"
  local asset="${2:?asset is required}"
  local prefix="${SCRYER_OFFICIAL_PLUGIN_RELEASE_TAG_PREFIX:-plugins-v3}/release/"
  gh api --paginate "repos/${repository}/releases?per_page=100" \
    | jq -r --arg asset "${asset}" --arg prefix "${prefix}" \
      '.[] | select(.tag_name | startswith($prefix)) | select(any(.assets[]?; .name == $asset)) | .tag_name' \
    | head -n 1
}

# Print the last (newest) artifact of a redirect as
# <catalog_version> <file name> <url> <mirror url> <signature url> <signature mirror url>
# separated by the ASCII unit separator (0x1f), which no URL may contain.
catalog_v3_redirect_head() {
  python3 -I -c '
import json, sys
from pathlib import Path
from urllib.parse import urlsplit
redirect = json.loads(Path(sys.argv[1]).read_text())
version = redirect.get("catalog_version")
if not isinstance(version, int) or isinstance(version, bool):
    sys.exit("catalog-v3 redirect has no integer catalog_version")
artifacts = redirect.get("artifacts") or []
if not artifacts:
    sys.exit("catalog-v3 redirect does not contain any artifacts")
head = artifacts[-1]
url = head.get("url") or ""
name = urlsplit(url).path.rsplit("/", 1)[-1]
if not name or name in (".", "..") or any(c in name for c in "\\\t\r\n\x1f"):
    sys.exit("catalog-v3 redirect artifact url is not a file: " + repr(url))
mirror = (head.get("mirror_urls") or [""])[0]
signature = head.get("signature_url") or ""
signature_mirror = (head.get("signature_mirror_urls") or [""])[0]
for value in (url, mirror, signature, signature_mirror):
    if any(c in value for c in "\t\r\n\x1f"):
        sys.exit("catalog-v3 redirect contains a control character")
print("\x1f".join([str(version), name, url, mirror, signature, signature_mirror]))
' "$1"
}

# Download and verify the cdn copy of the modern redirect into DIR/cdn.
catalog_v3_read_cdn_redirect() {
  local existing_dir="${1:?existing dir is required}"
  local repository="${2:?repository is required}"
  local cdn_base
  cdn_base="$(catalog_v3_cdn_base)"
  mkdir -p "${existing_dir}/cdn"
  catalog_v3_retry catalog_v3_fetch \
    "${cdn_base}/${CATALOG_V3_MODERN_REDIRECT}" \
    "${existing_dir}/cdn/${CATALOG_V3_MODERN_REDIRECT}"
  catalog_v3_retry catalog_v3_fetch \
    "${cdn_base}/${CATALOG_V3_MODERN_REDIRECT_BUNDLE}" \
    "${existing_dir}/cdn/${CATALOG_V3_MODERN_REDIRECT_BUNDLE}"
  test -s "${existing_dir}/cdn/${CATALOG_V3_MODERN_REDIRECT}"
  verify_release_blob_tag_bound \
    "${existing_dir}/cdn/${CATALOG_V3_MODERN_REDIRECT}" \
    "${existing_dir}/cdn/${CATALOG_V3_MODERN_REDIRECT_BUNDLE}" \
    "${repository}"
}

# Download and verify the GitHub release copy of the modern redirect into DIR/mirror.
catalog_v3_read_mirror_redirect() {
  local existing_dir="${1:?existing dir is required}"
  local repository="${2:?repository is required}"
  mkdir -p "${existing_dir}/mirror"
  catalog_v3_retry catalog_v3_mirror_download "${existing_dir}/mirror" "${repository}" \
    --pattern "${CATALOG_V3_MODERN_REDIRECT}" \
    --pattern "${CATALOG_V3_MODERN_REDIRECT_BUNDLE}"
  test -s "${existing_dir}/mirror/${CATALOG_V3_MODERN_REDIRECT}"
  verify_release_blob_tag_bound \
    "${existing_dir}/mirror/${CATALOG_V3_MODERN_REDIRECT}" \
    "${existing_dir}/mirror/${CATALOG_V3_MODERN_REDIRECT_BUNDLE}" \
    "${repository}"
}

# Verify DIR/ASSET against its DIR/ASSET.bundle.zst companion (decompressed
# beside it) as a release-trigger-bound signature for REPOSITORY.
catalog_v3_verify_catalog_asset() {
  local existing_dir="${1:?existing dir is required}"
  local asset="${2:?asset is required}"
  local repository="${3:?repository is required}"
  zstd -dc "${existing_dir}/${asset}.bundle.zst" > "${existing_dir}/${asset}.bundle" &&
    verify_release_blob_tag_bound \
      "${existing_dir}/${asset}" \
      "${existing_dir}/${asset}.bundle" \
      "${repository}"
}

# Fetch ASSET and its .bundle.zst into DIR, trying each "<url> <signature url>"
# pair in order; a pair with an empty url is skipped. A pair only counts once
# the downloaded bytes verify, so a corrupt or truncated copy on one source
# falls through to the next instead of failing the run later.
catalog_v3_fetch_catalog_pair() {
  local existing_dir="${1:?existing dir is required}"
  local asset="${2:?asset is required}"
  local repository="${3:?repository is required}"
  shift 3
  while (( $# >= 2 )); do
    local url="$1" signature_url="$2"
    shift 2
    [ -n "${url}" ] && [ -n "${signature_url}" ] || continue
    if catalog_v3_retry catalog_v3_fetch "${url}" "${existing_dir}/${asset}" &&
      catalog_v3_retry catalog_v3_fetch "${signature_url}" "${existing_dir}/${asset}.bundle.zst"; then
      if catalog_v3_verify_catalog_asset "${existing_dir}" "${asset}" "${repository}"; then
        return 0
      fi
      printf 'catalog downloaded from %s failed signature verification; trying the next source\n' "${url}" >&2
      rm -f "${existing_dir}/${asset}" "${existing_dir}/${asset}.bundle.zst" "${existing_dir}/${asset}.bundle"
      continue
    fi
    printf 'catalog download from %s failed; trying the next source\n' "${url}" >&2
  done
  return 1
}

resolve_catalog_v3_merge_base() {
  local existing_dir="${1:?existing dir is required}"
  local repository="${2:?repository is required}"
  local cdn_version cdn_name cdn_url cdn_mirror cdn_signature cdn_signature_mirror
  mkdir -p "${existing_dir}"

  catalog_v3_read_cdn_redirect "${existing_dir}" "${repository}"
  IFS=$'\x1f' read -r cdn_version cdn_name cdn_url cdn_mirror cdn_signature cdn_signature_mirror \
    < <(catalog_v3_redirect_head "${existing_dir}/cdn/${CATALOG_V3_MODERN_REDIRECT}")
  [ -n "${cdn_name:-}" ]
  # shellcheck disable=SC2034 # consumed by the sourcing step
  merge_base_asset="${cdn_name}"

  if [ -n "${SCRYER_CATALOG_MERGE_BASE_ASSET:-}" ]; then
    # Operator repair: merge onto a named, signed master catalog instead of
    # the one the redirects serve. The compare-and-set still guards against
    # the cdn redirect moving underneath this run. The mirror copy is not
    # consulted, because repairing a disagreement is what the override is for.
    printf '::warning::merging onto operator-selected catalog asset %s (cdn redirect serves %s, generation %s)\n' \
      "${SCRYER_CATALOG_MERGE_BASE_ASSET}" "${cdn_name}" "${cdn_version}"
    catalog_asset="${SCRYER_CATALOG_MERGE_BASE_ASSET}"
    case "${catalog_asset}" in
      */*|.|..|*[[:space:]]*|*[*?[]*)
        printf 'SCRYER_CATALOG_MERGE_BASE_ASSET must be a bare asset file name without glob characters; got %s\n' "${catalog_asset}" >&2
        return 1
        ;;
    esac
    local cdn_base override_url override_signature
    cdn_base="$(catalog_v3_cdn_base)"
    if [ "${catalog_asset}" = "${cdn_name}" ]; then
      # The live signed redirect carries both the generation directory and
      # the actual release tag, including release-trigger-backed generations.
      catalog_v3_fetch_catalog_pair "${existing_dir}" "${catalog_asset}" "${repository}" \
        "${cdn_url}" "${cdn_signature}" \
        "${cdn_mirror}" "${cdn_signature_mirror}"
    else
      # Historical overrides keep their immutable generation path on R2.
      # Older snapshots can additionally be recovered from catalog/v3.
      if [[ "${catalog_asset}" =~ ^catalog-v3\.([0-9]+)\. ]]; then
        override_url="${cdn_base}/${BASH_REMATCH[1]}/${catalog_asset}"
      else
        override_url="${cdn_base}/${catalog_asset}"
      fi
      override_signature="${override_url}.bundle.zst"
      if ! catalog_v3_fetch_catalog_pair "${existing_dir}" "${catalog_asset}" "${repository}" \
        "${override_url}" "${override_signature}"; then
        # The GitHub copy lives on the generation release that staged the
        # asset; only pre-generation snapshots sit on the redirect release.
        local generation_tag
        generation_tag="$(catalog_v3_retry catalog_v3_generation_release_for_asset \
          "${repository}" "${catalog_asset}")"
        if [ -n "${generation_tag}" ]; then
          printf 'recovering %s from generation release %s\n' "${catalog_asset}" "${generation_tag}" >&2
          catalog_v3_retry catalog_v3_release_download "${generation_tag}" \
            "${existing_dir}" "${repository}" \
            --pattern "${catalog_asset}" \
            --pattern "${catalog_asset}.bundle.zst"
        else
          catalog_v3_retry catalog_v3_mirror_download "${existing_dir}" "${repository}" \
            --pattern "${catalog_asset}" \
            --pattern "${catalog_asset}.bundle.zst"
        fi
        catalog_v3_verify_catalog_asset "${existing_dir}" "${catalog_asset}" "${repository}"
      fi
    fi
  else
    local mirror_version mirror_name
    catalog_v3_read_mirror_redirect "${existing_dir}" "${repository}"
    IFS=$'\x1f' read -r mirror_version mirror_name _ \
      < <(catalog_v3_redirect_head "${existing_dir}/mirror/${CATALOG_V3_MODERN_REDIRECT}")
    [ -n "${mirror_name:-}" ]
    if [ "${cdn_version}" != "${mirror_version}" ] || [ "${cdn_name}" != "${mirror_name}" ]; then
      local direction="cdn and mirror disagree"
      if (( mirror_version < cdn_version )); then
        direction="mirror behind cdn"
      elif (( cdn_version < mirror_version )); then
        direction="cdn behind mirror"
      fi
      printf '::error::catalog v3 modern redirect: %s: cdn serves generation %s (%s), %s release serves generation %s (%s). Publish refuses to merge onto either. If a publish failed in its last step (the GitHub redirect upload), rerun that run to finish it; otherwise bring the two back in line, or set SCRYER_CATALOG_MERGE_BASE_ASSET to the signed master catalog to merge onto.\n' \
        "${direction}" "${cdn_version}" "${cdn_name}" \
        "${SCRYER_CATALOG_V3_RELEASE_TAG:-catalog/v3}" "${mirror_version}" "${mirror_name}" >&2
      return 1
    fi
    catalog_asset="${cdn_name}"
    catalog_v3_fetch_catalog_pair "${existing_dir}" "${catalog_asset}" "${repository}" \
      "${cdn_url}" "${cdn_signature}" \
      "${cdn_mirror}" "${cdn_signature_mirror}"
  fi
}
