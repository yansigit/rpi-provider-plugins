"""The cohort refresher must honour --root and never record lane-absolute paths."""
import importlib.util
import json
import pathlib
import re
import subprocess
import sys
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("refresh-provider-parity-evidence.py")


def load():
    """The script as a module, without running its CLI."""
    spec = importlib.util.spec_from_file_location("refresh_provider_parity_evidence", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class RootBindingTests(unittest.TestCase):
    def test_git_follows_a_rebound_root(self):
        # --root rebinds ROOT in __main__; a default bound at def time would keep
        # asserting against the checkout this file happens to live in.
        module = load()
        with tempfile.TemporaryDirectory() as directory:
            other = pathlib.Path(directory).resolve()
            subprocess.run(["git", "init", "-q", str(other)], check=True, capture_output=True)
            module.ROOT = other
            self.assertEqual(module.git("rev-parse", "--show-toplevel"), str(other))

    def test_report_never_uses_the_module_root(self):
        source = SCRIPT.read_text(encoding="utf-8")
        body = source[source.index("def report("):source.index("\nif __name__")]
        self.assertNotIn("ROOT", body, "report() must use its root argument, not the module global")
        calls = re.findall(r"git\((?:[^()]|\([^()]*\))*\)", body)
        self.assertTrue(calls)
        for call in calls:
            self.assertIn("cwd=", call, f"git call without an explicit checkout: {call}")


class PortablePathTests(unittest.TestCase):
    def test_identity_redacts_the_checkout_and_the_home(self):
        module = load()
        with tempfile.TemporaryDirectory() as directory:
            root = pathlib.Path(directory) / "Projects/lane"
            root.mkdir(parents=True)
            target = root / "packages/p/Cargo.toml"
            target.parent.mkdir(parents=True)
            target.write_text("[]\n", encoding="utf-8")
            recorded = module.identity(target, root)
            self.assertEqual(recorded["path"], "<root>/packages/p/Cargo.toml")

    def test_a_path_outside_the_checkout_is_homed(self):
        module = load()
        root = pathlib.Path("/tmp/lane-root")
        home = module.home_prefixes(root)[-1]
        recorded = module.portable(pathlib.Path(home) / ".targets/g1/debug/lib.so", root)
        self.assertTrue(recorded.startswith("<home>/"), recorded)
        self.assertNotIn(home, recorded)

    def test_recorded_evidence_with_a_raw_home_is_refused(self):
        module = load()
        root = pathlib.Path("/tmp/lane-root")
        home = module.home_prefixes(root)[-1]
        with self.assertRaises(AssertionError):
            module.assert_portable({"worktree": home}, root)
        module.assert_portable({"worktree": "<home>"}, root)


class CommandLineTests(unittest.TestCase):
    def run_script(self, *arguments):
        return subprocess.run([sys.executable, str(SCRIPT), *arguments], capture_output=True, text=True)

    def test_write_and_check_are_mutually_exclusive(self):
        result = self.run_script("--write", "--check")
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("not allowed with argument", result.stderr)

    def test_a_missing_log_directory_is_explained(self):
        with tempfile.TemporaryDirectory() as directory:
            missing = pathlib.Path(directory) / "no-logs"
            result = self.run_script("--root", str(SCRIPT.parents[2]), "--logs", str(missing))
            self.assertEqual(result.returncode, 1, result.stderr)
            self.assertIn("does not exist", result.stderr)
            self.assertNotIn("Traceback", result.stderr)

    def test_an_absent_base_commit_is_explained(self):
        with tempfile.TemporaryDirectory() as directory:
            result = self.run_script("--root", str(SCRIPT.parents[2]), "--logs", directory, "--base", "0" * 12)
        self.assertEqual(result.returncode, 1, result.stderr)
        self.assertIn("base commit", result.stderr)
        self.assertNotIn("Traceback", result.stderr)

    def test_the_report_records_no_absolute_home(self):
        module = load()
        root = module.ROOT
        evidence = json.loads((root / "docs/provider-parity-sdk-cohort.json").read_text(encoding="utf-8"))
        module.assert_portable(evidence, root)


if __name__ == "__main__":
    unittest.main()
