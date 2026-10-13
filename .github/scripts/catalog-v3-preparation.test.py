#!/usr/bin/env python3
"""Exercise signed preparation recovery without GitHub or publication access."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).with_name("catalog-v3-preparation.sh").resolve()
MOCK = r"""
gh() {
  if [[ "$1" == api ]]; then
    [[ "$MODE" != api_failure ]] || return 1
    printf '%s' "$METADATA"
  elif [[ "$1 $2" == "run download" ]]; then
    [[ "$MODE" != download_failure ]] || return 1
    mkdir -p dist
    cp saved/* dist/
  else
    return 1
  fi
}
source "$SCRIPT" restore dist
"""

class PreparationTests(unittest.TestCase):
    def run_restore(self, *, exists=False, expired=False, attempt=1, mode="ok"):
        with tempfile.TemporaryDirectory(prefix="catalog-preparation-test-") as directory:
            root = Path(directory)
            (root / "saved").mkdir()
            original = b"original signed bytes with the first certificate and timestamp"
            (root / "saved" / "catalog.bundle.zst").write_bytes(original)
            (root / "saved" / "expected-merge-base").write_text("catalog-v3.7.base.min.json.zst")
            result = subprocess.run(
                ["bash", "-c", MOCK], cwd=root,
                env={**os.environ, "SCRIPT": str(SCRIPT), "MODE": mode,
                     "GITHUB_RUN_ID": "123", "GITHUB_REPOSITORY": "example/plugins",
                     "GITHUB_RUN_ATTEMPT": str(attempt), "GITHUB_OUTPUT": str(root / "output"),
                     "METADATA": json.dumps({"artifacts": [{"expired": expired}] if exists else []})},
                capture_output=True, text=True, timeout=15,
            )
            output = (root / "output").read_text() if (root / "output").exists() else ""
            if result.returncode == 0 and exists:
                self.assertEqual((root / "dist" / "catalog.bundle.zst").read_bytes(), original)
                self.assertEqual((root / "dist" / "expected-merge-base").read_text(),
                                 "catalog-v3.7.base.min.json.zst")
            return result, output

    def test_first_attempt_prepares_only_when_no_saved_artifact_exists(self):
        result, output = self.run_restore()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "restored=false\n")

    def test_retry_restores_exact_signature_bytes_and_original_merge_base(self):
        result, output = self.run_restore(exists=True, attempt=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "restored=true\n")

    def test_retry_without_a_saved_artifact_prepares_afresh(self):
        # Nothing is published before the save step, so no artifact means no
        # earlier attempt uploaded anything that a fresh signature could conflict with.
        result, output = self.run_restore(attempt=2)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(output, "restored=false\n")

    def test_expired_retry_artifact_never_allows_resigning(self):
        result, output = self.run_restore(exists=True, expired=True, attempt=2)
        self.assertNotEqual(result.returncode, 0)
        self.assertNotIn("restored=false", output)

    def test_api_and_download_errors_never_allow_resigning(self):
        for mode in ["api_failure", "download_failure"]:
            with self.subTest(mode=mode):
                result, output = self.run_restore(exists=True, attempt=2, mode=mode)
                self.assertNotEqual(result.returncode, 0)
                self.assertNotIn("restored=false", output)

if __name__ == "__main__":
    unittest.main()
