"""Release wiring for the Antigravity OAuth client secrets.

Release builds that compile rpi-antigravity-auth must receive both repository secrets and
must stop before compiling when either is empty. Pull-request and CI test workflows must not
require them. Plain text checks keep this independent of PyYAML on the runners.
"""

import re
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
WORKFLOWS = ROOT / ".github" / "workflows"
ID = "RPI_ANTIGRAVITY_OAUTH_CLIENT_ID"
SECRET = "RPI_ANTIGRAVITY_OAUTH_CLIENT_SECRET"
PASS_ID = "%s: ${{ secrets.%s }}" % (ID, ID)
PASS_SECRET = "%s: ${{ secrets.%s }}" % (SECRET, SECRET)
CHECK_STEP = "Require the Antigravity OAuth client secrets"
RELEASE_BUILDS = {
    # workflow file -> the build step that compiles the extension libraries
    "release-assets.yml": "Build extension libraries and installer",
    "release-packages.yml": "Build only the requested extension libraries",
}


def read(name):
    return (WORKFLOWS / name).read_text(encoding="utf-8")


def job_chunks(text):
    """Split a workflow into its top-level job blocks (two-space indented keys)."""
    return re.split(r"\n  (?=[A-Za-z][\w-]*:\n)", text)


class AntigravityOAuthSecretWiring(unittest.TestCase):
    def test_release_builds_receive_both_secrets_as_environment(self):
        for name, build_step in RELEASE_BUILDS.items():
            with self.subTest(workflow=name):
                text = read(name)
                # The build step's block runs until the next step (six-space "- " item).
                rest = text[text.index(build_step):]
                end = rest.find("\n      - ", 1)
                build = rest if end == -1 else rest[:end]
                self.assertIn(PASS_ID, build)
                self.assertIn(PASS_SECRET, build)

    def test_fail_fast_check_runs_before_the_build_and_names_the_secret(self):
        for name, build_step in RELEASE_BUILDS.items():
            with self.subTest(workflow=name):
                text = read(name)
                self.assertIn(CHECK_STEP, text)
                self.assertLess(text.index(CHECK_STEP), text.index(build_step))
                self.assertIn("::error::", text)
                # The check must read both variables and fail when either is blank.
                self.assertIn("for name in %s %s; do" % (ID, SECRET), text)
                self.assertIn('exit 1', text)

    def test_secrets_are_declared_optional_and_forwarded_through_each_caller(self):
        for name in RELEASE_BUILDS:
            with self.subTest(workflow=name):
                text = read(name)
                declaration = text[text.index("workflow_call:"): text.index("workflow_dispatch:")]
                self.assertIn("secrets:", declaration)
                self.assertIn(ID + ":", declaration)
                self.assertIn(SECRET + ":", declaration)
                self.assertNotIn("required: true", declaration[declaration.index("secrets:"):])

        forwarding = {
            # caller workflow -> (job that calls a release workflow, expected callee)
            "release-cargo.yml": ("assets", "release-assets.yml"),
            "release-unix.yml": ("build", "release-assets.yml"),
            "release-assets.yml": ("backfill", "release-packages.yml"),
        }
        for caller, (job, callee) in forwarding.items():
            with self.subTest(caller=caller):
                chunks = [c for c in job_chunks(read(caller)) if re.match(r"\s*%s:\n" % job, c)]
                self.assertEqual(len(chunks), 1, f"job {job} not found in {caller}")
                self.assertIn(callee, chunks[0])
                self.assertIn(PASS_ID, chunks[0])
                self.assertIn(PASS_SECRET, chunks[0])

    def test_pull_request_and_ci_workflows_do_not_reference_the_secrets(self):
        for path in sorted(WORKFLOWS.glob("*.yml")):
            if path.name.startswith("release-"):
                continue
            with self.subTest(workflow=path.name):
                text = path.read_text(encoding="utf-8")
                self.assertNotIn(ID, text)
                self.assertNotIn(SECRET, text)


if __name__ == "__main__":
    unittest.main()
