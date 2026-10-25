"""Recorded M-VIRTUAL-B cohort counts must age: no lost tests, no pinned ignores."""
import importlib.util
import pathlib
import tempfile
import unittest


SCRIPT = pathlib.Path(__file__).with_name("refresh-m-virtual-b2-evidence.py")


def load():
    spec = importlib.util.spec_from_file_location("refresh_m_virtual_b2_evidence", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def cohort_log(counts, ignored=None, failed=None):
    ignored = ignored or {}
    failed = failed or {}
    lines = []
    for name in ("rpi-antigravity-auth", "rpi-cursor-sdk", "rpi-multiprovider"):
        lines.append(f"     Running unittests src/lib.rs (/target/debug/deps/{name.replace('-', '_')}-0011223344)")
        lines.append(f"test result: ok. {counts[name]} passed; {failed.get(name, 0)} failed; {ignored.get(name, 0)} ignored; 0 measured; 0 filtered out")
    for name in ("rpi-antigravity-auth", "rpi-cursor-sdk", "rpi-multiprovider"):
        lines.append(f"   Doc-tests {name.replace('-', '_')}")
        lines.append("test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out")
    return "\n".join(lines) + "\n"


class CohortCountTests(unittest.TestCase):
    def run_cohort(self, text):
        module = load()
        with tempfile.TemporaryDirectory() as directory:
            log = pathlib.Path(directory) / "test-195.log"
            log.write_text(text, encoding="utf-8")
            return module.cohort(log)

    def test_counts_above_the_recorded_floor_are_accepted(self):
        counts = self.run_cohort(cohort_log({"rpi-antigravity-auth": 101, "rpi-cursor-sdk": 68, "rpi-multiprovider": 99},
                                            ignored={"rpi-antigravity-auth": 1, "rpi-cursor-sdk": 3, "rpi-multiprovider": 0}))
        self.assertEqual([counts[name]["passed"] for name in ("rpi-antigravity-auth", "rpi-cursor-sdk", "rpi-multiprovider")],
                         [101, 68, 99])

    def test_ignored_counts_are_not_pinned(self):
        module = load()
        text = cohort_log({"rpi-antigravity-auth": 97, "rpi-cursor-sdk": 64, "rpi-multiprovider": 97},
                          ignored={"rpi-antigravity-auth": 0, "rpi-cursor-sdk": 0, "rpi-multiprovider": 12})
        counts = self.run_cohort(text)
        self.assertEqual(counts["rpi-multiprovider"]["ignored"], 12)

    def test_a_lost_test_is_reported(self):
        text = cohort_log({"rpi-antigravity-auth": 97, "rpi-cursor-sdk": 62, "rpi-multiprovider": 97})
        with self.assertRaises(AssertionError) as raised:
            self.run_cohort(text)
        self.assertIn("fewer than the 64", str(raised.exception))

    def test_a_failing_test_is_reported(self):
        text = cohort_log({"rpi-antigravity-auth": 97, "rpi-cursor-sdk": 64, "rpi-multiprovider": 97},
                          failed={"rpi-cursor-sdk": 1})
        with self.assertRaises(AssertionError):
            self.run_cohort(text)


class BaseCommitTests(unittest.TestCase):
    def test_an_absent_base_commit_is_explained(self):
        module = load()
        with self.assertRaises(SystemExit) as raised:
            module.base_commit("0" * 40, module.ROOT)
        message = str(raised.exception)
        self.assertIn("shallow clone", message)
        self.assertIn("--base", message)

    def test_the_recorded_base_commit_resolves(self):
        module = load()
        self.assertEqual(module.base_commit(module.BASE, module.ROOT), module.BASE)


if __name__ == "__main__":
    unittest.main()
