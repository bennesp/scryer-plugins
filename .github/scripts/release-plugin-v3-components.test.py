#!/usr/bin/env python3
"""Exercise release trigger component selection against a synthetic repository."""

import os
from pathlib import Path
import subprocess
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().parent / "release-plugin-v3-components.sh"

# Isolated synthetic repository only; gh is a shell mock.
MOCK = r'''set -euo pipefail
gh() {
  printf 'gh %s\n' "$*" >> calls
  if [[ "$1 $2" == "api repos/example/plugins/git/tags/"* ]]; then
    local object="${2##*/}"
    if [[ " $UNVERIFIED " == *" $object "* ]]; then printf 'false\n'; else printf 'true\n'; fi
    return 0
  fi
  return 1
}
source "$SCRIPT"
release_plugin_components "$@"
'''


def git(repo, *args):
    return subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@example.test", "-c", "commit.gpgsign=false",
         "-c", "tag.gpgsign=false", *args],
        cwd=repo, check=True, capture_output=True, text=True, timeout=30,
    ).stdout.strip()


class ComponentTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(prefix="release-components-test-")
        self.repo = Path(self.directory.name)
        git(self.repo, "init", "-q", "-b", "main")
        (self.repo / "a").write_text("1")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-q", "-m", "legacy")
        self.legacy = git(self.repo, "rev-parse", "HEAD")
        (self.repo / "a").write_text("2")
        git(self.repo, "commit", "-q", "-am", "head")
        self.head = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "checkout", "-q", "-b", "side", self.legacy)
        (self.repo / "b").write_text("3")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-q", "-m", "side")
        self.side = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "checkout", "-q", "main")
        git(self.repo, "tag", "-a", "-m", "Release", "plugins-v3/newznab/v2.3.3", self.legacy)
        git(self.repo, "tag", "-a", "-m", "Release", "plugins-v3/newznab/v3.1.0", self.head)
        git(self.repo, "tag", "-a", "-m", "Release", "plugins-v3/torznab/v1.0.0", self.side)
        git(self.repo, "tag", "plugins-v3/jimaku/v1.0.0", self.legacy)
        git(self.repo, "tag", "-a", "-m", "Release", "rule-packs/trash/v1.0.0", self.head)

    def tearDown(self):
        self.directory.cleanup()

    def trigger(self, body, name="plugins-v3/release/1700000000-abcdef012345"):
        message = "Release trigger for components\n\n" + body
        git(self.repo, "tag", "-a", "-m", message, name, self.head)
        return name

    def run_components(self, trigger, *, release_sha=None, verify="true", unverified=""):
        result = subprocess.run(
            ["bash", "-c", MOCK, "components", trigger, release_sha or self.head, "example/plugins", verify],
            cwd=self.repo, capture_output=True, text=True, timeout=30,
            env={**os.environ, "SCRIPT": str(SCRIPT), "UNVERIFIED": unverified},
        )
        calls = (self.repo / "calls").read_text() if (self.repo / "calls").exists() else ""
        return result, calls

    def test_listed_components_build_from_their_own_commits(self):
        trigger = self.trigger(
            f"component plugins-v3/newznab/v3.1.0 {self.head}\n"
            f"component plugins-v3/newznab/v2.3.3 {self.legacy}\n"
        )
        result, calls = self.run_components(trigger)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            result.stdout,
            f"newznab\t3.1.0\t{self.head}\nnewznab\t2.3.3\t{self.legacy}\n",
        )
        self.assertEqual(calls.count("gh api repos/example/plugins/git/tags/"), 2)

    def test_triggers_without_component_lines_use_tags_at_the_release_commit(self):
        trigger = self.trigger("")
        result, calls = self.run_components(trigger, verify="false")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"newznab\t3.1.0\t{self.head}\n")
        self.assertEqual(calls, "")

    def test_a_missing_trigger_falls_back_too(self):
        result, _ = self.run_components("", verify="false")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, f"newznab\t3.1.0\t{self.head}\n")

    def test_a_release_without_plugin_tags_prints_nothing(self):
        (self.repo / "c").write_text("4")
        git(self.repo, "add", ".")
        git(self.repo, "commit", "-q", "-m", "packs only")
        packs_only = git(self.repo, "rev-parse", "HEAD")
        git(self.repo, "tag", "-a", "-m", "Release", "rule-packs/trash/v1.1.0", packs_only)
        result, _ = self.run_components("", release_sha=packs_only)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, "")

    def test_lightweight_component_tags_are_refused(self):
        trigger = self.trigger(f"component plugins-v3/jimaku/v1.0.0 {self.legacy}\n")
        result, _ = self.run_components(trigger)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not an annotated tag", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_a_component_listed_at_the_wrong_commit_is_refused(self):
        trigger = self.trigger(f"component plugins-v3/newznab/v2.3.3 {self.head}\n")
        result, _ = self.run_components(trigger)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("points at", result.stderr)

    def test_a_component_outside_the_release_history_is_refused(self):
        trigger = self.trigger(f"component plugins-v3/torznab/v1.0.0 {self.side}\n")
        result, _ = self.run_components(trigger)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not an ancestor", result.stderr)

    def test_an_unverified_component_tag_is_refused_when_verifying(self):
        trigger = self.trigger(f"component plugins-v3/newznab/v2.3.3 {self.legacy}\n")
        tag_object = git(self.repo, "rev-parse", "refs/tags/plugins-v3/newznab/v2.3.3")
        result, _ = self.run_components(trigger, unverified=tag_object)
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("not a GitHub-verified signed tag", result.stderr)
        self.assertEqual(result.stdout, "")

    def test_malformed_and_duplicate_component_lines_are_refused(self):
        for body in [
            f"component plugins-v3/newznab/v2.3.3 {self.legacy} extra\n",
            f"component plugins-v3/newznab/v2.3.3 {self.legacy}\ncomponent plugins-v3/newznab/v2.3.3 {self.legacy}\n",
            f"component rule-packs/trash/v1.0.0 {self.head}\n",
            "component plugins-v3/newznab/v2.3.3 abc\n",
        ]:
            with self.subTest(body=body):
                trigger = self.trigger(body, name=f"plugins-v3/release/t{abs(hash(body))}")
                result, _ = self.run_components(trigger)
                self.assertNotEqual(result.returncode, 0)
                self.assertEqual(result.stdout, "")


if __name__ == "__main__":
    unittest.main()
