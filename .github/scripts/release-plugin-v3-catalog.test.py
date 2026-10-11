#!/usr/bin/env python3
"""Exercise the workflow's catalog merge-base selection without network access."""

import os
from pathlib import Path
import subprocess
import tempfile
import textwrap
import unittest

WORKFLOW = Path(__file__).resolve().parents[1] / "workflows/release-plugin-v3.yml"


class CatalogMergeBaseTests(unittest.TestCase):
    def run_selection(self, mode):
        workflow = WORKFLOW.read_text()
        start = workflow.index('          existing_dir="existing-central-catalog-v3"')
        end = workflow.index('          prepare_v3_args+=(--existing-catalog', start)
        block = textwrap.dedent(workflow[start:end])
        # Isolated synthetic files only; all external commands are shell mocks.
        directory = tempfile.mkdtemp(prefix="catalog-merge-base-test-")
        mocks = r'''set -euo pipefail
attempts=0
gh() {
  printf 'download %s\n' "$*" >> calls
  case "$*" in
    *catalog-v3.modern.redirect.json*)
      attempts=$((attempts + 1))
      if [[ "$MODE" == unavailable ]] || [[ "$MODE" == transient && "$attempts" -lt 3 ]]; then
        return 1
      fi
      printf '%s' '{"artifacts":[{"url":"https://example.invalid/full.json.zst"}]}' > "$existing_dir/catalog-v3.modern.redirect.json"
      ;;
    *catalog-v3.redirect.json*)
      printf '%s' '{"artifacts":[{"url":"https://example.invalid/filtered.json.zst"}]}' > "$existing_dir/catalog-v3.redirect.json"
      ;;
    *full.json.zst*)
      [[ "$MODE" != catalog_unavailable ]] || return 1
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
'''
        result = subprocess.run(
            ["bash", "-c", mocks + block + "\nprintf 'ready %s\\n' \"$catalog_asset\" >> calls\n"],
            cwd=directory,
            env={**os.environ, "MODE": mode, "GITHUB_REPOSITORY": "example/plugins",
                 "SCRYER_CATALOG_MERGE_BASE_ASSET": ""},
            capture_output=True, text=True, timeout=15,
        )
        calls = (Path(directory) / "calls").read_text()
        self.assertNotIn("catalog-v3.redirect.json", calls)
        self.assertNotIn("filtered.json.zst", calls)
        return result, calls

    def test_unavailable_modern_catalog_stops_before_preparation(self):
        result, calls = self.run_selection("unavailable")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(sum(line.startswith("download ") for line in calls.splitlines()), 5)
        self.assertNotIn("ready ", calls)

    def test_transient_failure_retries_full_catalog(self):
        result, calls = self.run_selection("transient")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(sum(line.startswith("download ") for line in calls.splitlines()), 4)
        self.assertIn("ready full.json.zst", calls)

    def test_success_uses_verified_full_catalog(self):
        result, calls = self.run_selection("success")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(calls.count("verify "), 2)
        self.assertIn("ready full.json.zst", calls)

    def test_invalid_signature_stops_before_preparation(self):
        result, calls = self.run_selection("invalid_signature")
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("ready ", calls)

    def test_catalog_asset_failure_stops_before_preparation(self):
        result, calls = self.run_selection("catalog_unavailable")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(sum(line.startswith("download ") for line in calls.splitlines()), 6)
        self.assertNotIn("ready ", calls)


if __name__ == "__main__":
    unittest.main()
