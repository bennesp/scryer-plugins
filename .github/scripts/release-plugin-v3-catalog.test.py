#!/usr/bin/env python3
"""Exercise the central catalog merge-base selection without network access."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent / "catalog-v3-merge-base.sh"
CDN = "https://cdn.example.invalid/scryer/catalog/v3"
MIRROR = "https://github.example.invalid/releases/download/catalog%2Fv3%2Fg7"

# Isolated synthetic files only; every external command is a shell mock.
MOCKS = r'''set -euo pipefail
source "$SCRIPT"
cdn_redirect_attempts=0

redirect_json() {
  local version="$1" name="$2"
  printf '{"schema_version":"3","catalog_version":%s,"artifacts":[{"url":"%s/old.json.zst","mirror_urls":[],"signature_url":"%s/old.json.zst.bundle.zst","signature_mirror_urls":[]},{"url":"%s/%s","mirror_urls":["%s/%s"],"signature_url":"%s/%s.bundle.zst","signature_mirror_urls":["%s/%s.bundle.zst"]}]}' \
    "$version" "$CDN" "$CDN" "$CDN" "$name" "$MIRROR" "$name" "$CDN" "$name" "$MIRROR" "$name"
}

curl() {
  local destination="" url=""
  while (( $# > 0 )); do
    case "$1" in
      -o|--proto|--connect-timeout|--max-time) [[ "$1" == -o ]] && destination="$2"; shift 2 ;;
      -*) shift ;;
      *) url="$1"; shift ;;
    esac
  done
  printf 'curl %s\n' "$url" >> calls
  case "$url" in
    "$CDN/catalog-v3.modern.redirect.json")
      cdn_redirect_attempts=$((cdn_redirect_attempts + 1))
      if [[ "$MODE" == cdn_unavailable ]] || [[ "$MODE" == cdn_transient && "$cdn_redirect_attempts" -lt 3 ]]; then
        return 22
      fi
      local version=7 name=full.json.zst
      if [[ "$MODE" == cdn_behind ]]; then version=6; name=older.json.zst; fi
      redirect_json "$version" "$name" > "$destination"
      ;;
    "$CDN/catalog-v3.modern.redirect.bundle.json") printf '{}' > "$destination" ;;
    "$CDN/full.json.zst"|"$CDN/full.json.zst.bundle.zst"|"$CDN/7/catalog-v3.7.repair.min.json.zst"|"$CDN/7/catalog-v3.7.repair.min.json.zst.bundle.zst")
      [[ "$MODE" != catalog_cdn_down && "$MODE" != catalog_unavailable ]] || return 22
      printf 'catalog' > "$destination"
      ;;
    "$MIRROR/full.json.zst"|"$MIRROR/full.json.zst.bundle.zst")
      [[ "$MODE" != catalog_unavailable ]] || return 22
      printf 'catalog' > "$destination"
      ;;
    *) return 22 ;;
  esac
}

gh() {
  printf 'gh %s\n' "$*" >> calls
  local destination=""
  local -a args=("$@")
  local index
  for index in "${!args[@]}"; do
    if [[ "${args[$index]}" == --dir ]]; then destination="${args[$((index + 1))]}"; fi
  done
  case "$*" in
    "api --paginate repos/example/plugins/releases?per_page=100")
      # Two pages, as gh prints them: a generation release carrying the
      # generation-9 master, and the redirect release carrying only redirects.
      if [[ "$MODE" == override_generation ]]; then
        printf '[{"tag_name":"plugins-v3/release/1700000000-abcdef012345","assets":[{"name":"catalog-v3.9.repair.min.json.zst"},{"name":"catalog-v3.9.repair.min.json.zst.bundle.zst"}]}]'
        printf '[{"tag_name":"catalog/v3","assets":[{"name":"catalog-v3.modern.redirect.json"}]}]'
      else
        printf '[{"tag_name":"catalog/v3","assets":[{"name":"catalog-v3.modern.redirect.json"}]}]'
      fi
      ;;
    "release download plugins-v3/release/1700000000-abcdef012345 "*)
      [[ "$MODE" == override_generation ]] || return 1
      printf 'catalog' > "$destination/catalog-v3.9.repair.min.json.zst"
      printf 'sig' > "$destination/catalog-v3.9.repair.min.json.zst.bundle.zst"
      ;;
    "release download catalog/v3 "*catalog-v3.6.snapshot.min.json.zst*)
      [[ "$MODE" == override_snapshot ]] || return 1
      printf 'catalog' > "$destination/catalog-v3.6.snapshot.min.json.zst"
      printf 'sig' > "$destination/catalog-v3.6.snapshot.min.json.zst.bundle.zst"
      ;;
    *catalog-v3.modern.redirect.json*)
      [[ "$MODE" != mirror_unavailable ]] || return 1
      local version=7 name=full.json.zst
      if [[ "$MODE" == mirror_behind || "$MODE" == override ]]; then version=6; name=older.json.zst; fi
      redirect_json "$version" "$name" > "$destination/catalog-v3.modern.redirect.json"
      printf '{}' > "$destination/catalog-v3.modern.redirect.bundle.json"
      ;;
    *) return 1 ;;
  esac
}

sleep() { :; }
verify_release_blob_tag_bound() {
  printf 'verify %s\n' "$1" >> calls
  [[ "$MODE" != invalid_signature ]]
}
zstd() { printf '{}'; }

resolve_catalog_v3_merge_base existing-central-catalog-v3 example/plugins
printf 'ready %s base %s\n' "$catalog_asset" "$merge_base_asset" >> calls
'''


class CatalogMergeBaseTests(unittest.TestCase):
    def run_selection(self, mode, override=""):
        directory = tempfile.mkdtemp(prefix="catalog-merge-base-test-")
        result = subprocess.run(
            ["bash", "-c", MOCKS],
            cwd=directory,
            env={**os.environ, "MODE": mode, "SCRIPT": str(SCRIPT), "CDN": CDN, "MIRROR": MIRROR,
                 "CF_R2_PUBLIC_BASE_URL": "https://cdn.example.invalid/scryer",
                 "SCRYER_CATALOG_V3_PATH_PREFIX": "catalog/v3",
                 "SCRYER_CATALOG_V3_RELEASE_TAG": "catalog/v3",
                 "SCRYER_CATALOG_MERGE_BASE_ASSET": override},
            capture_output=True, text=True, timeout=15,
        )
        calls_path = Path(directory) / "calls"
        calls = calls_path.read_text() if calls_path.exists() else ""
        # The frozen legacy redirect ladder is never a merge-base source.
        self.assertNotIn("catalog-v3.redirect.json", calls)
        self.assertNotIn("catalog-v3.redirect.bundle.json", calls)
        return result, calls

    def assert_refused(self, result, calls):
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("ready ", calls)

    def test_agreeing_redirects_merge_onto_cdn_master(self):
        result, calls = self.run_selection("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ready full.json.zst base full.json.zst", calls)
        self.assertIn(f"curl {CDN}/full.json.zst\n", calls)
        self.assertNotIn(f"curl {MIRROR}/", calls)
        # cdn redirect, mirror redirect, master catalog.
        self.assertEqual(calls.count("verify "), 3)

    def test_transient_cdn_failure_retries(self):
        result, calls = self.run_selection("cdn_transient")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls.count(f"curl {CDN}/catalog-v3.modern.redirect.json\n"), 3)
        self.assertIn("ready full.json.zst", calls)

    def test_unavailable_cdn_redirect_stops_before_preparation(self):
        result, calls = self.run_selection("cdn_unavailable")
        self.assert_refused(result, calls)
        self.assertEqual(calls.count(f"curl {CDN}/catalog-v3.modern.redirect.json\n"), 5)
        self.assertNotIn("gh ", calls)

    def test_unavailable_mirror_redirect_stops_before_preparation(self):
        result, calls = self.run_selection("mirror_unavailable")
        self.assert_refused(result, calls)
        self.assertEqual(calls.count("gh release download"), 5)

    def test_mirror_behind_cdn_is_refused_with_both_generations(self):
        result, calls = self.run_selection("mirror_behind")
        self.assert_refused(result, calls)
        self.assertIn("mirror behind cdn", result.stderr)
        self.assertIn("generation 7 (full.json.zst)", result.stderr)
        self.assertIn("generation 6 (older.json.zst)", result.stderr)
        self.assertIn("SCRYER_CATALOG_MERGE_BASE_ASSET", result.stderr)
        self.assertNotIn("full.json.zst.bundle.zst", calls)

    def test_cdn_behind_mirror_is_refused(self):
        result, calls = self.run_selection("cdn_behind")
        self.assert_refused(result, calls)
        self.assertIn("cdn behind mirror", result.stderr)

    def test_invalid_signature_stops_before_preparation(self):
        result, calls = self.run_selection("invalid_signature")
        self.assert_refused(result, calls)

    def test_cdn_catalog_failure_falls_back_to_redirect_mirror_url(self):
        result, calls = self.run_selection("catalog_cdn_down")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"curl {MIRROR}/full.json.zst\n", calls)
        self.assertIn(f"curl {MIRROR}/full.json.zst.bundle.zst\n", calls)
        self.assertIn("ready full.json.zst base full.json.zst", calls)

    def test_catalog_unavailable_everywhere_stops_before_preparation(self):
        result, calls = self.run_selection("catalog_unavailable")
        self.assert_refused(result, calls)

    def test_operator_override_merges_named_asset_and_keeps_cdn_compare_and_set(self):
        result, calls = self.run_selection("override", override="catalog-v3.7.repair.min.json.zst")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("ready catalog-v3.7.repair.min.json.zst base full.json.zst", calls)
        # The disagreeing mirror copy is not consulted under the override.
        self.assertNotIn("gh ", calls)
        self.assertIn(f"curl {CDN}/7/catalog-v3.7.repair.min.json.zst\n", calls)

    def test_live_override_uses_the_signed_redirect_mirror_when_cdn_blob_is_down(self):
        result, calls = self.run_selection("catalog_cdn_down", override="full.json.zst")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"curl {MIRROR}/full.json.zst\n", calls)
        self.assertNotIn("gh ", calls)
        self.assertIn("ready full.json.zst base full.json.zst", calls)

    def test_historical_override_recovers_from_its_generation_release(self):
        # The R2 copy of a post-migration historical master is gone; its
        # GitHub mirror lives on the trigger-backed generation release.
        result, calls = self.run_selection("override_generation", override="catalog-v3.9.repair.min.json.zst")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn(f"curl {CDN}/9/catalog-v3.9.repair.min.json.zst\n", calls)
        self.assertIn("gh api --paginate repos/example/plugins/releases?per_page=100\n", calls)
        self.assertIn("gh release download plugins-v3/release/1700000000-abcdef012345 ", calls)
        self.assertNotIn("gh release download catalog/v3", calls)
        self.assertIn("ready catalog-v3.9.repair.min.json.zst base full.json.zst", calls)

    def test_historical_override_without_generation_release_uses_the_redirect_release(self):
        # A pre-generation snapshot is only on the redirect release.
        result, calls = self.run_selection("override_snapshot", override="catalog-v3.6.snapshot.min.json.zst")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("gh api --paginate", calls)
        self.assertIn("gh release download catalog/v3 ", calls)
        self.assertIn("ready catalog-v3.6.snapshot.min.json.zst base full.json.zst", calls)

    def test_operator_override_rejects_paths(self):
        result, calls = self.run_selection("override", override="../escape.json.zst")
        self.assert_refused(result, calls)


if __name__ == "__main__":
    unittest.main()
