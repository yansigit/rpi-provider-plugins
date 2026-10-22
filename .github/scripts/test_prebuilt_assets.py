import argparse
import hashlib
import importlib.util
import json
import pathlib
import re
import tarfile
import tempfile
import unittest
from unittest import mock
import zipfile

spec = importlib.util.spec_from_file_location("prebuilt_assets", pathlib.Path(__file__).with_name("prebuilt-assets.py"))
assets = importlib.util.module_from_spec(spec)
spec.loader.exec_module(assets)


class PrebuiltAssetsTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = pathlib.Path(self.temporary.name)
        self.tag = "rpi-demo-v1.2.3"
        self.args = argparse.Namespace(output=str(self.root), tags=json.dumps([self.tag]),
                                      source_ref="a" * 40, bundle_tag="rpi-packages-aaaaaaaaaaaa",
                                      repository="pi-rust/rpi-package")
        self.sdk = {"version": "0.3.20", "plugin_abi": 4}
        for target in sorted(assets.TARGETS):
            archive = self.root / f"rpi-demo-1.2.3-{target}.zip"
            library = self.root / "rpi_demo.dll"
            library.write_bytes(target.encode())
            assets.archive_library(library, archive)
            distribution = assets.asset(archive, self.args.repository, self.tag)
            distribution["library"] = {"path": library.name, "sha256": assets.digest(library)}
            assets.write_json(self.root / f"rpi-demo.{target}.json", {
                "schema_version": 1, "name": "rpi-demo", "version": "1.2.3",
                "repository": "https://github.com/pi-rust/rpi-package", "tag": self.tag,
                "source_commit": self.args.source_ref, "sdk": self.sdk,
                "target": target, "distribution": distribution,
            })
            bundle = self.root / f"bundle-{target}.zip"
            bridge = self.root / f"bridge-{target}.zip"
            bundle.write_bytes(b"bundle")
            bridge.write_bytes(b"bridge")
            assets.write_json(self.root / f"bundle.{target}.json", {
                "schema_version": 1, "target": target, "source_commit": self.args.source_ref,
                "tag": self.args.bundle_tag, "packages": [],
                "archive": assets.asset(bundle, self.args.repository, self.args.bundle_tag),
                "acp_bridge": assets.asset(bridge, self.args.repository, self.args.bundle_tag),
            })

    def edit_fragment(self, **changes):
        path = next(self.root.glob("rpi-demo.*.json"))
        fragment = json.loads(path.read_text())
        fragment.update(changes)
        assets.write_json(path, fragment)

    def test_complete_manifest_is_uploaded_after_all_archives(self):
        with mock.patch.object(assets, "release_exists", return_value=True), \
                mock.patch.object(assets, "check_tag_source"), \
                mock.patch.object(assets, "command", return_value='{"assets": []}'), \
                mock.patch.object(assets, "upload") as upload:
            assets.publish(self.args)
        self.assertEqual(upload.call_args_list[0].args[0], self.tag)
        self.assertEqual(len(upload.call_args_list[0].args[1]), 5)
        self.assertEqual(upload.call_args_list[1].args[1][0].name, "rpi-extension.json")
        manifest = json.loads(upload.call_args_list[1].args[1][0].read_text())
        self.assertEqual(set(manifest["targets"]), assets.TARGETS)
        self.assertEqual(manifest["version"], "1.2.3")
        self.assertEqual(manifest["sdk"], self.sdk)

    def test_package_only_publish_needs_no_bundle_and_uploads_manifest_last(self):
        self.args.packages_only = True
        for path in self.root.glob("bundle.*.json"):
            path.unlink()
        with mock.patch.object(assets, "release_exists", return_value=True), \
                mock.patch.object(assets, "check_tag_source"), \
                mock.patch.object(assets, "command", return_value='{"assets": []}'), \
                mock.patch.object(assets, "upload") as upload:
            assets.publish(self.args)
        self.assertEqual(len(upload.call_args_list), 2)
        self.assertEqual(len(upload.call_args_list[0].args[1]), 5)
        self.assertEqual(upload.call_args_list[1].args[1][0].name, "rpi-extension.json")

    def test_package_only_publish_still_requires_all_five_platforms(self):
        self.args.packages_only = True
        next(self.root.glob("rpi-demo.*.json")).unlink()
        self.assert_rejected_before_upload()

    def test_package_only_publish_rejects_empty_selection(self):
        self.args.packages_only = True
        self.args.tags = "[]"
        self.assert_rejected_before_upload()

    def test_package_only_publish_preserves_complete_releases(self):
        self.args.packages_only = True
        with mock.patch.object(assets, "release_exists", return_value=True), \
                mock.patch.object(assets, "check_tag_source"), \
                mock.patch.object(assets, "command", return_value='{"assets": [{"name":"rpi-extension.json"}]}'), \
                mock.patch.object(assets, "upload") as upload:
            assets.publish(self.args)
        upload.assert_not_called()

    def assert_rejected_before_upload(self):
        with mock.patch.object(assets, "upload") as upload:
            with self.assertRaises(ValueError):
                assets.publish(self.args)
            upload.assert_not_called()

    def test_missing_platform_never_publishes_a_partial_manifest(self):
        next(self.root.glob("rpi-demo.*.json")).unlink()
        self.assert_rejected_before_upload()

    def test_mixed_source_commits_are_rejected(self):
        self.edit_fragment(source_commit="b" * 40)
        self.assert_rejected_before_upload()

    def test_mixed_sdk_abi_is_rejected(self):
        self.edit_fragment(sdk={"version": "0.3.20", "plugin_abi": 5})
        self.assert_rejected_before_upload()

    def test_changed_archive_checksum_is_rejected(self):
        next(self.root.glob("rpi-demo-*.zip")).write_bytes(b"corrupted")
        self.assert_rejected_before_upload()

    def test_completed_releases_keep_their_original_assets_on_retry(self):
        with mock.patch.object(assets, "release_exists", return_value=True), \
                mock.patch.object(assets, "check_tag_source"), \
                mock.patch.object(assets, "command", return_value='{"assets": [{"name":"rpi-extension.json"},{"name":"rpi-packages.json"}]}'), \
                mock.patch.object(assets, "upload") as upload:
            assets.publish(self.args)
            upload.assert_not_called()

    def test_individual_archives_have_only_the_declared_library_at_root(self):
        library = self.root / "librpi_demo.so"
        library.write_bytes(b"native library")
        archive = self.root / "extension.tar.gz"
        assets.archive_library(library, archive)
        with tarfile.open(archive) as packaged:
            self.assertEqual(packaged.getnames(), [library.name])
            self.assertEqual(packaged.extractfile(library.name).read(), library.read_bytes())
        zip_archive = next(self.root.glob("rpi-demo-*.zip"))
        with zipfile.ZipFile(zip_archive) as packaged:
            self.assertEqual(packaged.namelist(), ["rpi_demo.dll"])

    def declared_assets(self, paths):
        staging = self.root / "staging"
        records = []
        for path in paths:
            data = f"bytes of {path}".encode()
            target = staging / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(data)
            records.append({"path": path, "sha256": hashlib.sha256(data).hexdigest()})
        (self.root / "assets.json").write_text(json.dumps({
            "schema_version": 1, "package": "rpi-demo", "source": "staging", "assets": records,
        }), encoding="utf-8")
        return {"name": "rpi-demo", "manifest_path": str(self.root / "Cargo.toml"),
                "metadata": {"rpi": {"assets": "assets.json"}}}

    def test_declared_assets_are_strict_deterministic_sidecar_archives(self):
        package = self.declared_assets(["runtime/main.mjs", "runtime/dist/a.js", "runtime/shared/b.mjs"])
        declared = assets.load_declared_assets(package)
        self.assertEqual([item["path"] for item in declared], ["runtime/main.mjs", "runtime/dist/a.js", "runtime/shared/b.mjs"])
        library = self.root / "librpi_demo.so"
        library.write_bytes(b"native library")
        first, second = self.root / "one.tar.gz", self.root / "two.tar.gz"
        assets.archive_library(library, first, declared)
        assets.archive_library(library, second, declared)
        self.assertEqual(first.read_bytes(), second.read_bytes(), "archives must be reproducible")
        assets.verify_archive(first, library, declared)
        names, duplicates = assets.archive_entries(first)
        self.assertFalse(duplicates)
        self.assertEqual(set(names), {library.name, *(item["path"] for item in declared)})

    def test_stale_or_unsafe_asset_declarations_are_rejected(self):
        package = self.declared_assets(["runtime/main.mjs"])
        (self.root / "staging/runtime/main.mjs").write_bytes(b"changed after staging")
        with self.assertRaises(ValueError):
            assets.load_declared_assets(package)
        package = self.declared_assets(["runtime/node_modules/@cursor/sdk/index.js"])
        with self.assertRaises(ValueError):
            assets.load_declared_assets(package)
        package = self.declared_assets(["runtime/libdemo.so"])
        with self.assertRaises(ValueError):
            assets.load_declared_assets(package)

    def test_more_assets_than_the_host_accepts_are_rejected(self):
        package = self.declared_assets([f"runtime/file-{index}.js" for index in range(128)])
        with self.assertRaises(ValueError):
            assets.load_declared_assets(package)

    def test_archive_verification_rejects_undeclared_or_tampered_entries(self):
        package = self.declared_assets(["runtime/main.mjs"])
        declared = assets.load_declared_assets(package)
        library = self.root / "librpi_demo.so"
        library.write_bytes(b"native library")
        archive = self.root / "extension.tar.gz"
        assets.archive_library(library, archive, declared)
        extra = self.root / "extra.tar.gz"
        assets.archive_library(library, extra, declared + [{"path": "runtime/extra.js", "sha256": "0" * 64, "bytes": b"extra"}])
        with self.assertRaises(ValueError):
            assets.verify_archive(extra, library, declared)
        tampered = list(declared)
        tampered[0] = {"path": tampered[0]["path"], "sha256": "0" * 64, "bytes": tampered[0]["bytes"]}
        with self.assertRaises(ValueError):
            assets.verify_archive(archive, library, tampered)

    def test_build_includes_declared_assets_in_the_distribution_manifest(self):
        package = self.declared_assets(["runtime/main.mjs", "runtime/dist/a.js"])
        sdk_root = self.root / "sdk"
        (sdk_root / "src").mkdir(parents=True)
        (sdk_root / "src/lib.rs").write_text("pub const RPI_PLUGIN_ABI_VERSION_UNIFIED: u32 = 4;\n")
        release = self.root / "target/release"
        release.mkdir(parents=True)
        library = release / "librpi_demo.so"
        library.write_bytes(b"native library")
        package.update({
            "id": "demo", "version": "1.2.3", "repository": "https://github.com/pi-rust/rpi-package",
            "publish": [], "targets": [{"name": "rpi_demo", "crate_types": ["cdylib"]}],
        })
        metadata = {
            "workspace_members": ["demo"], "target_directory": str(self.root / "target"),
            "packages": [package, {"id": "sdk", "version": "0.3.20", "manifest_path": str(sdk_root / "Cargo.toml")}],
            "resolve": {"nodes": [{"id": "demo", "deps": [{"name": "rpi_plugin_sdk", "pkg": "sdk"}]}]},
        }
        self.args.packages_only = True
        self.args.target = "x86_64-unknown-linux-gnu"
        with mock.patch.object(assets, "command", side_effect=lambda *args: {
            ("cargo", "metadata", "--format-version", "1", "--locked", "--filter-platform", "x86_64-unknown-linux-gnu"): json.dumps(metadata),
            ("rustc", "-vV"): "host: x86_64-unknown-linux-gnu\n",
            ("git", "rev-parse", "HEAD"): self.args.source_ref,
        }[args]):
            assets.build(self.args)
        fragment = json.loads((self.root / "rpi-demo.x86_64-unknown-linux-gnu.json").read_text())
        declared = fragment["distribution"]["assets"]
        self.assertEqual([item["path"] for item in declared], ["runtime/main.mjs", "runtime/dist/a.js"])
        archive = next(self.root.glob("rpi-demo-1.2.3-x86_64-unknown-linux-gnu.tar.gz"))
        with tarfile.open(archive) as packaged:
            self.assertEqual(set(packaged.getnames()), {"librpi_demo.so", "runtime/main.mjs", "runtime/dist/a.js"})

    def test_workflow_builds_every_target_required_by_the_manifest(self):
        workflow = pathlib.Path(__file__).parents[1] / "workflows/release-assets.yml"
        self.assertEqual(set(re.findall(r"target: ([a-z0-9_-]+)", workflow.read_text())), assets.TARGETS)

    def test_sdk_abi_comes_from_the_resolved_sdk_source(self):
        sdk_root = self.root / "sdk"
        (sdk_root / "src").mkdir(parents=True)
        (sdk_root / "src/lib.rs").write_text("pub const RPI_PLUGIN_ABI_VERSION_UNIFIED: u32 = 4;\n")
        package = {"id": "extension"}
        metadata = {
            "packages": [{"id": "sdk", "version": "0.3.20", "manifest_path": str(sdk_root / "Cargo.toml")}],
            "resolve": {"nodes": [{"id": "extension", "deps": [{"name": "rpi_plugin_sdk", "pkg": "sdk"}]}]},
        }
        self.assertEqual(assets.sdk_identity(package, metadata), self.sdk)
        (sdk_root / "src/lib.rs").write_text("// unknown SDK ABI\n")
        with self.assertRaises(ValueError):
            assets.sdk_identity(package, metadata)

    def test_tag_selection_uses_exact_commit_and_handles_annotated_tags(self):
        tag_spec = importlib.util.spec_from_file_location("release_tags", pathlib.Path(__file__).with_name("select-release-tags.py"))
        selector = importlib.util.module_from_spec(tag_spec)
        tag_spec.loader.exec_module(selector)
        metadata = {"workspace_members": ["demo", "old", "tool", "core", "prebuilt"], "packages": [
            {"id": "demo", "name": "rpi-demo", "version": "1.2.3", "publish": None, "targets": [{"crate_types": ["cdylib"]}]},
            {"id": "old", "name": "rpi-old", "version": "0.1.0", "publish": None, "targets": [{"crate_types": ["cdylib"]}]},
            {"id": "tool", "name": "installer", "version": "0.0.0", "publish": [], "targets": [{"crate_types": ["bin"]}]},
            {"id": "core", "name": "rpi-shared", "version": "0.1.0", "publish": None, "targets": [{"crate_types": ["lib"]}]},
            {"id": "prebuilt", "name": "rpi-prebuilt", "version": "0.2.0", "publish": [],
             "metadata": {"rpi": {"prebuilt": True}}, "targets": [{"crate_types": ["cdylib"]}]},
        ]}
        refs = {"refs/tags/rpi-demo-v1.2.3": "tag-object", "refs/tags/rpi-demo-v1.2.3^{}": self.args.source_ref,
                "refs/tags/rpi-old-v0.1.0": "b" * 40, "refs/tags/installer-v0.0.0": self.args.source_ref,
                "refs/tags/rpi-shared-v0.1.0": self.args.source_ref,
                "refs/tags/rpi-prebuilt-v0.2.0": self.args.source_ref}
        self.assertEqual(selector.select(metadata, refs, self.args.source_ref), [self.tag, "rpi-prebuilt-v0.2.0"])

    def test_published_tag_must_point_to_the_exact_build_commit(self):
        with mock.patch.object(assets, "command", side_effect=[
            json.dumps({"object": {"type": "tag", "sha": "b" * 40}}),
            json.dumps({"object": {"type": "commit", "sha": self.args.source_ref}}),
        ]):
            assets.check_tag_source(self.args.repository, self.tag, self.args.source_ref)
        with mock.patch.object(assets, "command", return_value=json.dumps({"object": {"type": "commit", "sha": "b" * 40}})):
            with self.assertRaises(ValueError):
                assets.check_tag_source(self.args.repository, self.tag, self.args.source_ref)


if __name__ == "__main__":
    unittest.main()
