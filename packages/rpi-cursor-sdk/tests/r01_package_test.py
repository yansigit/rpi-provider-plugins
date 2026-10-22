"""R01 packaging acceptance: Cursor sidecar assets, no vendored SDK, install/uninstall.

The real host installer is not importable from this repository and a prebuilt
install needs the network, so the install/uninstall roundtrip below is a
scripted model of the published host semantics (`docs/prebuilt-distribution.md`
and the host's `install-assets.md`): strict manifest/archive verification,
transactional staging, registry records with `prebuilt.assets`, uninstall of the
library with its recorded assets, and empty-directory pruning. Every archive is
built by the repository's own packaging module, not a copy of it.
"""

import hashlib
import importlib.util
import json
import pathlib
import shutil
import subprocess
import tempfile
import unittest
import zipfile

PACKAGE = pathlib.Path(__file__).resolve().parents[1]
REPOSITORY = PACKAGE.parents[1]
ASSETS_SCRIPT = REPOSITORY / ".github" / "scripts" / "prebuilt-assets.py"
FIXTURE = PACKAGE / "tests" / "r01-package-assets.json"
EVIDENCE = PACKAGE / "tests" / "r01-package-evidence.json"
SDK_HASHES = PACKAGE / "distribution" / "cursor-sdk-1.0.37.sha256.json"
TARGETS = sorted(
    {
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
        "aarch64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    }
)
FORMAT = {  # target -> (archive suffix, library name)
    "x86_64-pc-windows-msvc": (".zip", "rpi_cursor_sdk.dll"),
    "x86_64-unknown-linux-gnu": (".tar.gz", "librpi_cursor_sdk.so"),
    "aarch64-unknown-linux-gnu": (".tar.gz", "librpi_cursor_sdk.so"),
    "x86_64-apple-darwin": (".tar.gz", "librpi_cursor_sdk.dylib"),
    "aarch64-apple-darwin": (".tar.gz", "librpi_cursor_sdk.dylib"),
}


def load_packaging():
    spec = importlib.util.spec_from_file_location("r01_prebuilt_assets", ASSETS_SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


packaging = load_packaging()


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def node_version():
    try:
        return subprocess.check_output(["node", "--version"], encoding="utf-8").strip()
    except (OSError, subprocess.CalledProcessError):
        return None


class CursorPackageTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.staged = PACKAGE / ".package"
        node = node_version()
        if node is None:
            raise unittest.SkipTest("Node 24 is required to stage the packaged runtime")
        subprocess.run(
            ["node", str(PACKAGE / "scripts" / "stage-package-runtime.mjs"), "--out", str(cls.staged)],
            cwd=PACKAGE,
            check=True,
            capture_output=True,
        )
        cls.manifest = json.loads((cls.staged / "runtime-assets.json").read_text(encoding="utf-8"))
        cls.sdk = json.loads(SDK_HASHES.read_text(encoding="utf-8"))
        cls.forbidden = {item for package in cls.sdk["packages"] for item in package["files"]}
        cls.assets = [
            {
                "path": item["path"],
                "sha256": item["sha256"],
                "bytes": (cls.staged / item["path"]).read_bytes(),
            }
            for item in cls.manifest["assets"]
        ]

    def archive(self, target, assets=None):
        suffix, library_name = FORMAT[target]
        directory = pathlib.Path(self.temp.name)
        library = directory / library_name
        library.write_bytes(f"fixture-{target}-library".encode())
        archive = directory / f"rpi-cursor-sdk-0.1.6-{target}{suffix}"
        packaging.archive_library(library, archive, self.assets if assets is None else assets)
        return archive, library

    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)

    def test_committed_manifest_matches_the_staged_runtime(self):
        committed = json.loads(FIXTURE.read_text(encoding="utf-8"))
        self.assertEqual(committed, self.manifest)
        self.assertEqual([item["sha256"] for item in committed["assets"]], [item["sha256"] for item in self.manifest["assets"]])

    def test_archives_are_strict_deterministic_sidecar_archives(self):
        for target in TARGETS:
            archive, library = self.archive(target)
            with self.subTest(target=target):
                packaging.verify_archive(archive, library, self.assets)
                names, duplicates = packaging.archive_entries(archive)
                self.assertFalse(duplicates)
                self.assertEqual(len(names), len(self.assets) + 1)
                self.assertLessEqual(len(names), 128)
                self.assertEqual(set(names), {library.name, *(item["path"] for item in self.assets)})
                first = archive.read_bytes()
                archive.unlink()
                archive, _ = self.archive(target)
                self.assertEqual(first, archive.read_bytes(), "archives must be byte-reproducible")

    def test_archive_contains_no_sdk_bytes_and_no_node_modules(self):
        for target in TARGETS:
            archive, library = self.archive(target)
            with self.subTest(target=target):
                names, _ = packaging.archive_entries(archive)
                for name in names:
                    self.assertNotIn("node_modules", name.split("/"))
                    self.assertNotIn("@cursor", name.split("/"))
                for item in self.assets:
                    self.assertNotIn(item["sha256"], self.forbidden, f"{item['path']} is a copy of SDK bytes")
                assets = [name for name in names if name != library.name]
                self.assertFalse(any(name.endswith((".node", ".so", ".dylib", ".dll", ".pyd")) for name in assets))
                for item in self.assets:
                    self.assertNotIn(b"Anysphere", item["bytes"], f"{item['path']} carries proprietary SDK notice text")
                runtime = next(item for item in self.assets if item["path"] == "runtime/dist/rpi/runtime.js")
                self.assertIn(b'from "@cursor/sdk"', runtime["bytes"], "the SDK import must stay external")

    def test_manifest_pins_the_external_sdk_and_license(self):
        package = json.loads((PACKAGE / "runtime" / "package.json").read_text(encoding="utf-8"))
        self.assertEqual(package["dependencies"]["@cursor/sdk"], "1.0.37")
        self.assertEqual(package["license"], "MIT")
        self.assertTrue((PACKAGE / "runtime" / "LICENSE").read_bytes().startswith(b"MIT License"))
        lock = json.loads((PACKAGE / "runtime" / "package-lock.json").read_text(encoding="utf-8"))
        self.assertEqual(lock["packages"][""]["dependencies"]["@cursor/sdk"], "1.0.37")
        self.assertFalse(any("node_modules" in item["path"] for item in self.manifest["assets"]))

    def test_sidecar_path_rules_reject_vendored_dependencies_and_unsafe_names(self):
        for path in (
            "runtime/node_modules/@cursor/sdk/index.js",
            "runtime/../escape.js",
            "runtime/.hidden.js",
            "runtime/libcursor.so",
            "runtime\\windows.js",
        ):
            with self.subTest(path=path), self.assertRaises(ValueError):
                packaging.validate_asset_path(path)

    def test_install_and_uninstall_roundtrip_keeps_user_files(self):
        manifest = self.extension_manifest()
        store = pathlib.Path(self.temp.name) / "agent" / "extensions"
        for target in TARGETS:
            archive = pathlib.Path(self.temp.name) / f"rpi-cursor-sdk-0.1.6-{target}{FORMAT[target][0]}"
            with self.subTest(target=target):
                self.install(store, manifest, target, archive)
                self.assertTrue((store / FORMAT[target][1]).is_file())
                self.assertTrue((store / "runtime" / "main.mjs").is_file())
                self.assertTrue((store / "runtime" / "dist" / "rpi" / "peer.js").is_file())
                self.assertTrue((store / "runtime" / "shared" / "cursor-sensitive-text.mjs").is_file())
                self.uninstall(store, "rpi-cursor-sdk")
                self.assertEqual(list(store.iterdir()) if store.exists() else [], [])

    def test_uninstall_removes_only_recorded_assets_and_prunes_empty_directories(self):
        manifest = self.extension_manifest()
        target = "x86_64-unknown-linux-gnu"
        store = pathlib.Path(self.temp.name) / "agent" / "extensions"
        archive = pathlib.Path(self.temp.name) / f"rpi-cursor-sdk-0.1.6-{target}{FORMAT[target][0]}"
        self.install(store, manifest, target, archive)
        (store / "runtime" / "user.txt").write_bytes(b"user")
        (store / "other").mkdir()
        (store / "other" / "keep.txt").write_bytes(b"keep")
        self.uninstall(store, "rpi-cursor-sdk")
        self.assertEqual((store / "runtime" / "user.txt").read_bytes(), b"user")
        self.assertEqual((store / "other" / "keep.txt").read_bytes(), b"keep")
        # Recorded asset subtrees that became empty are pruned; user-authored parents stay.
        self.assertFalse((store / "runtime" / "dist").exists())
        self.assertFalse((store / "runtime" / "shared").exists())
        self.assertTrue((store / "runtime").is_dir())

    def test_tampered_or_undeclared_archive_entries_are_rejected_before_install(self):
        target = "x86_64-unknown-linux-gnu"
        suffix, library_name = FORMAT[target]
        directory = pathlib.Path(self.temp.name)
        library = directory / library_name
        library.write_bytes(b"library")
        archive = directory / "tampered.tar.gz"
        entries = [(library_name, b"library")] + [(item["path"], item["bytes"] + b"tampered") for item in self.assets]
        with archive.open("wb") as raw:
            import gzip
            import tarfile

            with gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, filename="") as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as value:
                    for name, data in entries:
                        packaging._tar_entry(value, name, data)
        with self.assertRaises(ValueError):
            packaging.verify_archive(archive, library, self.assets)

        extra = directory / "extra.tar.gz"
        trailing = [(library_name, b"library")] + [(item["path"], item["bytes"]) for item in self.assets] + [("runtime/extra.js", b"x")]
        with trailing_archive(extra, trailing):
            pass
        with self.assertRaises(ValueError):
            packaging.verify_archive(extra, library, self.assets)

    def test_evidence_records_this_manifest_and_no_unverified_claims(self):
        evidence = json.loads(EVIDENCE.read_text(encoding="utf-8"))
        self.assertEqual(evidence["status"], "pass")
        self.assertEqual(evidence["ledger"], "R01")
        self.assertEqual(evidence["assetManifest"]["sha256"], sha256(FIXTURE.read_bytes()))
        self.assertEqual(evidence["assetManifest"]["assets"], len(self.assets))
        self.assertEqual(evidence["assetManifest"]["entries"], len(self.assets) + 1)
        self.assertEqual(evidence["sdk"]["packages"], [{"name": item["name"], "version": item["version"], "files": len(item["files"])} for item in self.sdk["packages"]])
        self.assertTrue(any(item.startswith("hostBinaryInstallUninstall") for item in evidence["notTested"]))

    # -- scripted host install/uninstall model ---------------------------------
    def extension_manifest(self):
        targets = {}
        for target in TARGETS:
            suffix, _ = FORMAT[target]
            archive = pathlib.Path(self.temp.name) / f"rpi-cursor-sdk-0.1.6-{target}{suffix}"
            if not archive.exists():
                self.archive(target)
            targets[target] = {
                "url": f"https://github.com/pi-rust/rpi-package/releases/download/rpi-cursor-sdk-v0.1.6/{archive.name}",
                "format": "zip" if suffix == ".zip" else "tar.gz",
                "size": archive.stat().st_size,
                "sha256": sha256(archive.read_bytes()),
                "library": {"path": FORMAT[target][1], "sha256": sha256(f"fixture-{target}-library".encode())},
                "assets": [{"path": item["path"], "sha256": item["sha256"]} for item in self.assets],
            }
        return {"schema_version": 1, "name": "rpi-cursor-sdk", "version": "0.1.6",
                "repository": "https://github.com/pi-rust/rpi-package", "tag": "rpi-cursor-sdk-v0.1.6",
                "source_commit": "a" * 40, "sdk": {"version": "0.3.20", "plugin_abi": 4}, "targets": targets}

    def install(self, store, manifest, target, archive):
        distribution = manifest["targets"][target]
        data = archive.read_bytes()
        if len(data) != distribution["size"] or sha256(data) != distribution["sha256"]:
            raise AssertionError("archive does not match the manifest")
        declared = distribution["assets"]
        paths = {item["path"]: item["sha256"] for item in declared}
        library_name = distribution["library"]["path"]
        names, duplicates = packaging.archive_entries(archive)
        if duplicates or set(names) != {library_name, *paths}:
            raise AssertionError("archive is not strict")
        with archive.open("rb") as raw:
            if archive.suffix == ".zip":
                with zipfile.ZipFile(raw) as value:
                    contents = {name: value.read(name) for name in names}
            else:
                import gzip
                import tarfile

                with tarfile.open(fileobj=gzip.GzipFile(fileobj=raw, mode="rb"), mode="r") as value:
                    contents = {name: value.extractfile(name).read() for name in names}
        for name, want in {"@library": distribution["library"]["sha256"], **paths}.items():
            actual = library_name if name == "@library" else name
            if sha256(contents[actual]) != want:
                raise AssertionError(f"hash mismatch for {actual}")
        staging = store / ".rpi-assets-stage-test"
        staging.mkdir(parents=True, exist_ok=True)
        for name, bytes_ in contents.items():
            destination = staging / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_bytes(bytes_)
        # Activation: files move into place; recorded paths are exactly the library plus assets.
        store.mkdir(parents=True, exist_ok=True)
        for name in [library_name, *paths]:
            destination = store / name
            destination.parent.mkdir(parents=True, exist_ok=True)
            shutil.move(staging / name, destination)
        shutil.rmtree(staging)
        record = {"name": manifest["name"], "version": manifest["version"],
                  "source": f"prebuilt:{manifest['repository']}", "artifacts": [library_name],
                  "prebuilt": {"repository": manifest["repository"], "target": target,
                               "manifest_sha256": "b" * 64, "source_commit": manifest["source_commit"],
                               "sdk_version": manifest["sdk"]["version"], "plugin_abi": manifest["sdk"]["plugin_abi"],
                               "archive_sha256": distribution["sha256"],
                               "library_sha256": distribution["library"]["sha256"],
                               "assets": declared}}
        (store.parent / "native-packages.json").write_text(json.dumps([record]), encoding="utf-8")

    def uninstall(self, store, name):
        registry = store.parent / "native-packages.json"
        records = json.loads(registry.read_text(encoding="utf-8"))
        record = next(item for item in records if item["name"] == name)
        removed = [store / path for path in record["artifacts"]]
        removed += [store / item["path"] for item in record.get("prebuilt", {}).get("assets", [])]
        for path in removed:
            path.unlink(missing_ok=True)
        for path in removed:
            parent = path.parent
            while parent != store and parent.is_dir() and not any(parent.iterdir()):
                parent.rmdir()
                parent = parent.parent
        (store.parent / "native-packages.json").unlink(missing_ok=True)


def trailing_archive(path, entries):
    """Write a tar.gz whose entries are exactly `entries` (context manager for readability)."""
    import contextlib
    import gzip
    import tarfile

    @contextlib.contextmanager
    def writer():
        with path.open("wb") as raw:
            with gzip.GzipFile(fileobj=raw, mode="wb", mtime=0, filename="") as compressed:
                with tarfile.open(fileobj=compressed, mode="w") as value:
                    for name, data in entries:
                        packaging._tar_entry(value, name, data)
        yield path

    return writer()


if __name__ == "__main__":
    unittest.main(verbosity=2)
