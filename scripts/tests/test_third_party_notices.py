"""Public CLI tests for third-party notice collection."""

from __future__ import annotations

import importlib.machinery
import importlib.util
import hashlib
import json
from pathlib import Path
import tarfile
import tempfile
import unittest
from unittest.mock import patch


WORKSPACE = Path(__file__).resolve().parents[2]
LOADER = importlib.machinery.SourceFileLoader(
    "third_party_notices", str(WORKSPACE / "scripts/collect-third-party-notices")
)
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None
notices = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(notices)


class FakeCargoCommand:
    """Return a fixed metadata graph without invoking Cargo or the network."""

    def __init__(self, metadata: dict[str, object]) -> None:
        self.metadata = metadata
        self.commands: list[list[str]] = []

    def __call__(self, command: list[str], **_kwargs: object) -> object:
        self.commands.append(command)

        class Result:
            stdout = json.dumps(self.metadata)

        return Result()


class ThirdPartyNoticeTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self._setup_cargo()
        self._setup_npm()
        self._setup_extra()

    def _setup_cargo(self) -> None:
        self.cargo_manifest = self.root / "Cargo.toml"
        self.cargo_manifest.write_text('[package]\nname="app"\n')
        self.crate = self.root / "registry" / "tiny-gpl-1.0"
        self.crate.mkdir(parents=True)
        (self.crate / "Cargo.toml").write_text(
            '[package]\nname="tiny-gpl"\nversion="1.0"\nlicense="GPL-3.0-only"\nlicense-file="COPYING"\n'
        )
        (self.crate / "COPYING").write_text("verbatim GPL text\n")
        (self.crate / "src").mkdir()
        (self.crate / "src/lib.rs").write_text("pub fn source() {}\n")
        (self.crate / "src/source-alias.rs").symlink_to(self.crate / "src/lib.rs")
        (self.crate / "target").mkdir()
        (self.crate / "target/private-build-output").write_text("omit")
        (self.crate / ".git").mkdir()
        (self.crate / ".git/config").write_text("omit")

    def _setup_npm(self) -> None:
        self.npm = self.root / "renderer"
        self._write_npm("@scope/widget", "2.1.0", "MIT", "LICENSE", "actual MIT text\n")
        self._write_npm("nested-only", "3.0.0", None, None, None, nested=True)
        built_manifest = (
            self.npm / "node_modules/@scope/widget/dist/commonjs/package.json"
        )
        built_manifest.parent.mkdir(parents=True)
        built_manifest.write_text('{"type":"commonjs"}')
        (self.npm / "package-lock.json").write_text(
            json.dumps(
                {
                    "lockfileVersion": 3,
                    "packages": {
                        "node_modules/@scope/widget": {
                            "resolved": "https://registry.npmjs.org/widget.tgz"
                        },
                        "node_modules/@scope/widget/node_modules/nested-only": {},
                    },
                }
            )
        )

    def _setup_extra(self) -> None:
        self.extra = self.root / "font reviewed notices"
        self.extra.mkdir()
        (self.extra / "font-notice.txt").write_text("reviewed verbatim\n")

    def _write_npm(
        self,
        name: str,
        version: str,
        license: str | None,
        filename: str | None,
        contents: str | None,
        nested: bool = False,
    ) -> None:
        segments = name.split("/")
        package_root = self.npm / "node_modules" / Path(*segments)
        if nested:
            package_root = self.npm / "node_modules/@scope/widget/node_modules" / name
        package_root.mkdir(parents=True, exist_ok=True)
        manifest = {"name": name, "version": version}
        if license:
            manifest["license"] = license
        (package_root / "package.json").write_text(json.dumps(manifest))
        if filename and contents:
            (package_root / filename).write_text(contents)

    def _metadata(self) -> dict[str, object]:
        app_id, crate_id = (
            "path+file:///tmp/app#app@0.1.0",
            "registry+tiny-gpl#tiny-gpl@1.0",
        )
        return {
            "workspace_root": str(self.root),
            "workspace_members": [app_id],
            "packages": [
                {
                    "id": app_id,
                    "name": "app",
                    "version": "0.1.0",
                    "source": None,
                    "manifest_path": str(self.cargo_manifest),
                },
                {
                    "id": crate_id,
                    "name": "tiny-gpl",
                    "version": "1.0",
                    "source": "registry+https://github.com/rust-lang/crates.io-index",
                    "license": "GPL-3.0-only",
                    "manifest_path": str(self.crate / "Cargo.toml"),
                },
            ],
            "resolve": {
                "nodes": [
                    {"id": app_id, "deps": [{"pkg": crate_id}]},
                    {"id": crate_id, "deps": []},
                ]
            },
        }

    def test_cli_copies_real_text_source_and_extra_and_reports_missing(self) -> None:
        output = self.root / "out"
        fake = FakeCargoCommand(self._metadata())
        arguments = [
            "--cargo-manifest",
            str(self.cargo_manifest),
            "--npm-root",
            str(self.npm),
            "--extra-notices",
            str(self.extra),
            "--output",
            str(output),
        ]
        with patch.object(notices.subprocess, "run", fake):
            result = notices.main(arguments)
        self.assertEqual(result, 1)
        self.assertEqual(len(fake.commands), 1)
        self.assertIn("--locked", fake.commands[0])
        self.assertIn("--filter-platform", fake.commands[0])
        self.assertIn("--all-features", fake.commands[0])
        self._assert_outputs(output)

    def _assert_outputs(self, output: Path) -> None:
        self.assertEqual(
            (output / "packages/cargo/tiny-gpl/1.0/notices/COPYING").read_text(),
            "verbatim GPL text\n",
        )
        archive = output / "sources/cargo/tiny-gpl/1.0.tar.gz"
        with tarfile.open(archive, "r:gz") as source_archive:
            names = source_archive.getnames()
            self.assertEqual(
                source_archive.extractfile("src/lib.rs").read(),
                b"pub fn source() {}\n",
            )
            alias = source_archive.getmember("src/source-alias.rs")
            self.assertTrue(alias.issym())
            self.assertEqual(alias.linkname, "lib.rs")
        self.assertIn("src/lib.rs", names)
        self.assertNotIn("target/private-build-output", names)
        self.assertNotIn(".git/config", names)
        duplicate = self.root / "copy.tar.gz"
        notices.write_source_archive(self.crate, duplicate)
        self.assertEqual(archive.read_bytes(), duplicate.read_bytes())
        self.assertEqual(
            (output / "packages/npm/@scope/widget/2.1.0/notices/LICENSE").read_text(),
            "actual MIT text\n",
        )
        self.assertTrue(
            (
                output
                / "reviewed-extra-notices/reviewed-extra_1_font_reviewed_notices/font-notice.txt"
            ).is_file()
        )
        inventory = json.loads((output / "inventory.json").read_text())
        missing = [
            item["id"] for item in inventory["packages"] if item["missing_license_text"]
        ]
        self.assertEqual(missing, ["npm:nested-only@3.0.0"])
        self.assertNotIn(str(self.root), (output / "inventory.json").read_text())
        self.assertIn("MISSING", (output / "THIRD-PARTY-NOTICES.txt").read_text())

    def test_rejects_existing_output_directory(self) -> None:
        output = self.root / "existing"
        output.mkdir()
        with self.assertRaisesRegex(ValueError, "expected a new directory"):
            notices.validate_inputs(self._arguments(output))

    def _arguments(self, output: Path) -> object:
        import argparse

        return argparse.Namespace(
            cargo_manifest=[self.cargo_manifest],
            npm_root=[],
            extra_notices=[],
            license_overrides=[],
            output=output,
        )

    def test_rejects_notice_symlink_that_escapes_package(self) -> None:
        external = self.root / "private.txt"
        external.write_text("private")
        (self.crate / "LICENSE-EXTERNAL").symlink_to(external)
        with self.assertRaisesRegex(ValueError, "expected a path inside"):
            notices.copy_assets(self.crate, self.root / "out")
        with self.assertRaisesRegex(ValueError, "expected a path inside"):
            notices.write_source_archive(self.crate, self.root / "archive.tar.gz")

    def test_cli_copies_digest_verified_override_with_identity_provenance(self) -> None:
        override_root = self._override_root("actual upstream text\n")
        output = self.root / "override-out"
        result = notices.main(
            [
                "--npm-root",
                str(self.npm),
                "--license-overrides",
                str(override_root),
                "--output",
                str(output),
            ]
        )
        self.assertEqual(result, 0)
        self._assert_override(output)

    def test_rejects_override_digest_and_path_errors(self) -> None:
        directory = self._override_root("actual upstream text\n")
        manifest_path = directory / "manifest.json"
        manifest = json.loads(manifest_path.read_text())
        manifest["npm:nested-only@3.0.0"]["sha256"]["texts/COPYING"] = "0" * 64
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "expected SHA-256"):
            notices.load_license_overrides([directory], notices.npm_records([self.npm]))
        manifest["npm:nested-only@3.0.0"]["sha256"] = {}
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "one sha256 digest"):
            notices.load_license_overrides([directory], notices.npm_records([self.npm]))
        manifest["npm:nested-only@3.0.0"]["files"] = ["../COPYING"]
        manifest["npm:nested-only@3.0.0"]["sha256"] = {"../COPYING": "0" * 64}
        manifest_path.write_text(json.dumps(manifest))
        with self.assertRaisesRegex(ValueError, "normalized relative path"):
            notices.load_license_overrides([directory], notices.npm_records([self.npm]))

    def _override_root(self, contents: str) -> Path:
        directory = self.root / "overrides"
        text = directory / "texts/COPYING"
        text.parent.mkdir(parents=True)
        text.write_text(contents)
        digest = hashlib.sha256(text.read_bytes()).hexdigest()
        manifest = {
            "npm:nested-only@3.0.0": {
                "files": ["texts/COPYING"],
                "source_url": "https://github.com/example/project/blob/0123456789abcdef0123456789abcdef01234567/LICENSE",
                "sha256": {"texts/COPYING": digest},
            }
        }
        (directory / "manifest.json").write_text(json.dumps(manifest))
        return directory

    def _assert_override(self, output: Path) -> None:
        target = (
            output / "packages/npm/nested-only/3.0.0/notices/overrides/texts/COPYING"
        )
        self.assertEqual(target.read_text(), "actual upstream text\n")
        inventory = json.loads((output / "inventory.json").read_text())
        record = next(
            item
            for item in inventory["packages"]
            if item["id"] == "npm:nested-only@3.0.0"
        )
        self.assertFalse(record["missing_license_text"])
        self.assertIn(
            "github.com/example/project/blob/0123456789abcdef0123456789abcdef01234567/LICENSE",
            record["license_override"]["source_url"],
        )


if __name__ == "__main__":
    unittest.main()
