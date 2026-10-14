"""Run anime-pack regressions against an explicitly supplied Scryer worktree."""
import argparse
import os
from pathlib import Path
import subprocess
import uuid

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--scryer", type=Path, required=True)
args = parser.parse_args()
host = args.scryer.resolve()
if ".worktrees" not in host.parts or not (host / ".git").is_file():
    parser.error("--scryer must point to an isolated Git worktree under .worktrees")
source = (host / "crates/scryer-rules/src/validation.rs").read_text()
fixture = "fn synthetic_test_input()" + source.split("fn synthetic_test_input()", 1)[1].split("#[cfg(test)]", 1)[0]
root = Path(__file__).resolve().parent
content = (root / "engine_test.rs.txt").read_text().replace("__SYNTHETIC_INPUT__", fixture)
name = "anime_pack_validation_" + uuid.uuid4().hex
target = host / "crates/scryer-rules/tests" / (name + ".rs")
target.parent.mkdir(exist_ok=True)
with target.open("x") as output:
    output.write(content)
env = dict(os.environ, ANIME_PACK=str(root.parents[1] / "anime-scoring.json"), ANIME_CASES=str(root / "cases.json"))
try:
    subprocess.run(["rtk", "proxy", "cargo", "nextest", "run", "-p", "scryer-rules", "--test", name, "--locked", "--retries", "0"], cwd=host, env=env, check=True)
finally:
    # Only remove the temporary test this run created, and preserve concurrent edits.
    if target.read_text() == content:
        target.unlink()
