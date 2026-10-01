"""Installer behavior through its entrypoint, with named fake build tools.

Run: PYTHONDONTWRITEBYTECODE=1 python3 -m unittest discover -s scripts/tests -p test_macos_installer.py
"""

from __future__ import annotations

import hashlib
import importlib.machinery
import importlib.util
import json
import os
from pathlib import Path
import plistlib
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch
import zipfile


SCRIPT = Path(__file__).resolve().parents[1] / "build-macos-installer"
LOADER = importlib.machinery.SourceFileLoader("macos_installer", str(SCRIPT))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None
installer = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(installer)


class FakeBuildTools(installer.MacOSInstaller):
    def __init__(self, **configuration: object) -> None:
        super().__init__(**configuration)
        self.calls: list[tuple[str, ...]] = []
        self.notary_status = "Accepted"
        self.linked_library = "/usr/lib/libSystem.B.dylib"
        self.architecture = "arm64"
        self.fail_command = ""
        self.extra_plugin_ids: list[str] = []
        self.omit_canvas = False
        self.image_instructions = ""
        self.prepared_signature_valid = True
        self.include_release_map = False
        self.include_release_source = False
        self.include_release_harness = False

    def preflight(self) -> None:
        self.validate_inputs()
        self.check_native_symbol_tools()

    def command(self, *arguments: str | Path) -> str:
        argv = tuple(str(argument) for argument in arguments)
        self.calls.append(argv)
        command = Path(argv[0]).name
        if command == self.fail_command:
            raise subprocess.CalledProcessError(1, argv)
        if command == "npm" and argv[1:3] == ("run", "build"):
            self.stage_renderer_diagnostics(Path(argv[argv.index("--prefix") + 1]))
        if command == "cargo" and argv[1] == "build":
            self.executable_path.parent.mkdir(parents=True, exist_ok=True)
            self.executable_path.write_bytes(b"compiled lumvise executable")
        if command == "cargo" and argv[1] == "run" and "--verify-release" in argv:
            if not self.prepared_signature_valid:
                raise subprocess.CalledProcessError(2, argv)
        if command == "cargo" and argv[1] == "run" and "--composition" in argv:
            builtins = Path(argv[argv.index("--composition") + 3])
            self.stage_release(builtins, argv[argv.index("--composition") + 1])
        if command == "xcrun" and argv[1:3] == ("dsymutil", "-o"):
            dwarf = Path(argv[3]) / "Contents/Resources/DWARF/lumvise"
            dwarf.parent.mkdir(parents=True)
            dwarf.write_bytes(b"fake dsym dwarf data")
        if command == "sips":
            Path(argv[argv.index("--out") + 1]).write_bytes(b"icon")
        if command == "iconutil":
            Path(argv[-1]).write_bytes(b"icns")
        if command == "codesign" and "--force" in argv and argv[-1].endswith(".app"):
            binary = Path(argv[-1]) / "Contents/MacOS/lumvise"
            binary.write_bytes(binary.read_bytes() + b":signed")
        if command == "hdiutil" and argv[1] == "create":
            source = Path(argv[argv.index("-srcfolder") + 1])
            self.image_instructions = (source / "Install.txt").read_text()
            Path(argv[-1]).write_bytes(b"distribution container")
        return self.command_output(command, argv)

    def command_output(self, command: str, argv: tuple[str, ...]) -> str:
        if command == "lipo":
            return self.architecture + "\n"
        if command == "otool":
            return (
                f"{argv[-1]}:\n\t{self.linked_library} (compatibility version 1.0.0)\n"
            )
        if command == "xcrun" and argv[1:2] == ("--find",):
            return f"/Xcode/Toolchain/usr/bin/{argv[2]}\n"
        if command == "xcrun" and argv[1:3] == ("dwarfdump", "--uuid"):
            return f"UUID: 01234567-89AB-CDEF-0123-456789ABCDEF (arm64) {argv[-1]}\n"
        if "notarytool" in argv:
            return json.dumps(
                {"id": "fixture-submission", "status": self.notary_status}
            )
        return ""

    def stage_renderer_diagnostics(self, renderer: Path) -> None:
        component = "canvas" if "canvas" in renderer.parts else "host"
        output = (
            self.private_workspace / "crates/plugin/builtins/canvas/views/workspace"
            if component == "canvas"
            else self.private_workspace / "crates/frontend-core/renderer/assets"
        )
        output.mkdir(parents=True, exist_ok=True)
        bundle_name = f"{component}.js"
        bundle = b"(()=>{})()\n"
        source_map = b'{"version":3}\n'
        (output / bundle_name).write_bytes(bundle)
        diagnostics = self.diagnostics_dir / component
        diagnostics.mkdir(parents=True, exist_ok=True)
        map_name = f"{bundle_name}.map"
        (diagnostics / map_name).write_bytes(source_map)
        record = {
            "bundle": bundle_name,
            "bundleSha256": hashlib.sha256(bundle).hexdigest(),
            "map": map_name,
            "mapSha256": hashlib.sha256(source_map).hexdigest(),
        }
        (diagnostics / "manifest.json").write_text(
            json.dumps({"component": component, "bundles": [record]})
        )

    def stage_release(self, directory: Path, composition: str) -> None:
        directory.mkdir(parents=True)
        ids = ["builtin.knowledge", "builtin.semantic"]
        if composition == "full":
            ids.append("builtin.assistant")
            if not self.omit_canvas:
                ids.append("builtin.canvas")
        ids.extend(self.extra_plugin_ids)
        index_artifacts = []
        for plugin_id in ids:
            archive_name = f"{plugin_id}.lvp"
            payload = f"plugins/{plugin_id}/{installer.TARGET}/binary"
            manifest = {"plugin_id": plugin_id, "targets": {installer.TARGET: payload}}
            with zipfile.ZipFile(directory / archive_name, "w") as archive:
                archive.writestr("manifest.json", json.dumps(manifest))
                archive.writestr(payload, b"Apple-signed plugin")
                if self.include_release_map:
                    archive.writestr("views/workspace/workspace.js.map", b"private map")
                if self.include_release_source:
                    archive.writestr("src/private.rs", b"private source")
                if self.include_release_harness:
                    archive.writestr("testHarness.js", b"test code")
            index_artifacts.append(
                {"plugin_id": plugin_id, "archive_path": archive_name}
            )
        index = {"composition": composition, "artifacts": index_artifacts}
        (directory / "builtins-release.json").write_text(json.dumps(index))
        (directory / "publisher-trust.json").write_text("{}")
        (directory / "host-capability-grants.json").write_text("{}")


class MacOSInstallerTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(
            prefix="installer tests with spaces "
        )
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.core = self.root / "public-core"
        self.private = self.root / "private-product"
        self.make_workspace(self.core, private=False)
        self.make_workspace(self.private, private=True)
        self.key = self.root / "private release.key"
        self.key.write_bytes(b"a" * 32)
        self.key.chmod(0o600)
        self.output = self.root / "dist/macos"
        self.notices = self.root / "complete-notices"
        self.notices.mkdir()
        (self.notices / "LICENSE").write_text("dependency license\n")
        (self.notices / "inventory.json").write_text(
            json.dumps(
                {
                    "packages": [
                        {
                            "id": "cargo:fixture@1",
                            "missing_license_text": False,
                            "notice_files": ["LICENSE"],
                        }
                    ]
                }
            )
        )

    def make_workspace(self, root: Path, private: bool) -> None:
        (root / "Cargo.toml").parent.mkdir(parents=True, exist_ok=True)
        (root / "Cargo.toml").write_text('[workspace.package]\nversion = "1.2.3"\n')
        (root / "crates/mcp-app-adapter").mkdir(parents=True, exist_ok=True)
        (root / "crates/mcp-app-adapter/Cargo.toml").write_text("[package]\n")
        (root / "crates/plugin/tooling/builtin-release").mkdir(
            parents=True, exist_ok=True
        )
        (root / "crates/plugin/tooling/builtin-release/Cargo.toml").write_text(
            "[package]\n"
        )
        (root / "LICENSE").write_text("community license\n")
        (root / "LICENSES").mkdir(exist_ok=True)
        (root / "LICENSES/LGPL.txt").write_text("license text\n")
        (root / "assets").mkdir(exist_ok=True)
        (root / "assets/lumvise-icon.png").write_bytes(b"public company icon")
        if not private:
            return
        (root / "crates/desktop-product").mkdir(parents=True)
        (root / "crates/desktop-product/Cargo.toml").write_text("[package]\n")
        shell = root / "crates/desktop-shell"
        shell.mkdir(parents=True)
        (shell / "tauri.conf.json").write_text('{"identifier":"com.lumvise.desktop"}')
        for relative in (
            "crates/frontend-core/renderer",
            "crates/plugin/builtins/canvas/renderer",
        ):
            renderer = root / relative
            renderer.mkdir(parents=True)
            (renderer / "package.json").write_text("{}")
        (root / "distribution/third-party-desktop").mkdir(parents=True)
        (root / "distribution/third-party-desktop/OFL.txt").write_text("OFL notice\n")
        (root / "distribution/third-party-desktop/Liberation-license.md").write_text(
            "font license\n"
        )
        (root / "distribution/third-party-desktop/license-provenance.json").write_text(
            '{"source":"reviewed"}\n'
        )
        sources = root / "distribution/third-party-desktop/sources"
        sources.mkdir()
        (sources / "liberation-fonts-1.05.tar.gz").write_bytes(b"source archive")
        (root / "LICENSE").write_text("proprietary license\n")

    def make_builder(
        self,
        edition: str = "community",
        core: Path | None = None,
        private: Path | None = None,
        diagnostics: Path | None = None,
        target_dir: Path | None = None,
        plugin_release: Path | None = None,
        ad_hoc: bool = True,
        third_party_notices: Path | None = None,
    ) -> FakeBuildTools:
        environment = {
            "APPLE_SIGNING_IDENTITY": "Developer ID Application: Fixture (TEAM)",
            "APPLE_NOTARY_PROFILE": "fixture-profile",
            "LUMVISE_DRY_RUN": "1",
            "VITE_LUMVISE_ASSISTANT_E2E": "1",
            "VITE_LUMVISE_BROWSER_CANARY": "1",
            "LUMVISE_RELEASE_DIAGNOSTICS_DIR": "/untrusted/diagnostics",
        }
        with patch.dict(os.environ, environment):
            return FakeBuildTools(
                core_workspace=core or self.core,
                private_workspace=private,
                key=self.key,
                output=self.output,
                ad_hoc=ad_hoc,
                edition=edition,
                diagnostics_dir=diagnostics,
                third_party_notices=third_party_notices,
                target_dir=target_dir,
                plugin_release=plugin_release,
            )

    def test_cli_help_exposes_editions_and_explicit_workspaces(self) -> None:
        result = subprocess.run(
            [str(SCRIPT), "--help"], check=True, capture_output=True, text=True
        )
        self.assertIn("--edition {community,full}", result.stdout)
        self.assertIn("--core-workspace", result.stdout)
        self.assertIn("--private-workspace", result.stdout)
        self.assertIn("--third-party-notices", result.stdout)
        self.assertIn("--target-dir", result.stdout)
        self.assertIn("--plugin-release", result.stdout)
        with patch.object(
            sys,
            "argv",
            [
                str(SCRIPT),
                str(self.key),
                "--edition",
                "community",
                "--ad-hoc",
                "--output-dir",
                str(self.output),
                "--target-dir",
                str(self.root / "release-cache"),
                "--plugin-release",
                str(self.root / "prepared-release"),
            ],
        ):
            arguments = installer.installer_arguments()
        self.assertEqual(arguments.edition, "community")
        self.assertEqual(arguments.core_workspace, installer.WORKSPACE)
        self.assertTrue(arguments.ad_hoc)
        self.assertEqual(arguments.target_dir, self.root / "release-cache")
        self.assertEqual(arguments.plugin_release, self.root / "prepared-release")

    def test_public_icon_png_is_a_required_input(self) -> None:
        build = self.make_builder("community")
        (self.core / "assets/lumvise-icon.png").unlink()

        with self.assertRaisesRegex(
            ValueError, "lumvise-icon.png.*required source input"
        ):
            build.preflight()

    def test_community_uses_public_inputs_only_and_minimal_plugins(self) -> None:
        private_unavailable = self.root / "private-must-not-be-read"
        build = self.make_builder("community", private=private_unavailable)
        self.assertIsNone(build.private_workspace)
        dmg = build.build()

        self.assertEqual(dmg.name, "Lumvise-Community-1.2.3-macos-arm64-adhoc.dmg")
        app = self.output / "Lumvise Community.app"
        contents = app / "Contents"
        metadata = plistlib.loads((contents / "Info.plist").read_bytes())
        self.assertEqual(metadata["CFBundleIdentifier"], "com.lumvise.community")
        self.assertNotIn("NSMicrophoneUsageDescription", metadata)
        self.assertEqual(metadata["CFBundleIconFile"], "Lumvise.icns")
        self.assertTrue((contents / "Resources/Lumvise.icns").is_file())
        self.assertEqual(
            (contents / "Resources/licenses/LICENSE").read_text(), "community license\n"
        )
        self.assertFalse((contents / "Resources/licenses/proprietary-LICENSE").exists())
        configuration = json.loads(build.image_instructions.split("\n", 2)[2])
        server = configuration["mcpServers"]["lumvise"]
        self.assertEqual(
            server["command"],
            "/Applications/Lumvise Community.app/Contents/MacOS/lumvise",
        )
        self.assertEqual(
            server["args"], ["mcp", "--project-root", "/absolute/path/to/project"]
        )
        self.assertFalse(any(call[0] == "npm" for call in build.calls))
        icon_calls = [call for call in build.calls if call[0] == "sips"]
        self.assertEqual(len(icon_calls), 10)
        self.assertTrue(
            all(
                call[4] == str(build.core_workspace / "assets/lumvise-icon.png")
                for call in icon_calls
            )
        )
        self.assertTrue(
            any(call[:3] == ("iconutil", "-c", "icns") for call in build.calls)
        )
        self.assertFalse(
            any(str(private_unavailable) in " ".join(call) for call in build.calls)
        )
        app_build = next(call for call in build.calls if call[:2] == ("cargo", "build"))
        self.assertIn("lumvise-mcp-app-adapter", app_build)
        self.assertIn("native-vector", app_build)
        self.assertIn("--locked", app_build)
        self.assertEqual(
            build.environment["CARGO_TARGET_DIR"], str((self.core / "target").resolve())
        )
        self.assertEqual(build.environment["MACOSX_DEPLOYMENT_TARGET"], "14.0")
        self.assertEqual(
            build.environment["CARGO_PROFILE_RELEASE_DEBUG"], "line-tables-only"
        )
        self.assertEqual(build.target_dir, (self.core / "target").resolve())
        self.assertNotIn("npm", build.required_tools())
        self.assertIn("sips", build.required_tools())
        self.assertIn("iconutil", build.required_tools())
        release = next(call for call in build.calls if call[:2] == ("cargo", "run"))
        self.assertEqual(release.count("--workspace"), 1)
        self.assertIn("minimal", release)
        self.assertIn(str(build.target_dir), release)
        index = json.loads(
            (contents / "Resources/builtins/builtins-release.json").read_text()
        )
        self.assertEqual(
            {entry["plugin_id"] for entry in index["artifacts"]},
            installer.BASE_PLUGIN_IDS,
        )
        self.assertFalse(list(self.output.rglob("*.map")))
        self.assertTrue(
            (build.diagnostics_dir / "native/app/community/Lumvise.dSYM").is_dir()
        )
        self.assertFalse(list(self.output.rglob("*.dSYM")))
        self.assertLess(
            self.call_index(build, ("xcrun", "strip")),
            self.call_index(build, ("codesign", "--force")),
        )

    def test_full_build_uses_two_roots_and_private_product_inputs(self) -> None:
        build = self.make_builder("full", private=self.private)
        dmg = build.build()
        self.assertEqual(dmg.name, "Lumvise-Full-1.2.3-macos-arm64-adhoc.dmg")
        contents = self.output / "Lumvise.app/Contents"
        metadata = plistlib.loads((contents / "Info.plist").read_bytes())
        self.assertEqual(metadata["CFBundleIdentifier"], "com.lumvise.desktop")
        self.assertIn("NSMicrophoneUsageDescription", metadata)
        self.assertEqual(metadata["CFBundleIconFile"], "Lumvise.icns")
        self.assertTrue((contents / "Resources/Lumvise.icns").is_file())
        icon_calls = [call for call in build.calls if call[0] == "sips"]
        self.assertEqual(len(icon_calls), 10)
        self.assertTrue(
            all(
                call[4] == str(build.core_workspace / "assets/lumvise-icon.png")
                for call in icon_calls
            )
        )
        self.assertIn("sips", build.required_tools())
        self.assertIn("iconutil", build.required_tools())
        licenses = contents / "Resources/licenses"
        self.assertEqual(
            (licenses / "proprietary-LICENSE").read_text(), "proprietary license\n"
        )
        self.assertEqual(
            (licenses / "third-party-desktop/OFL.txt").read_text(), "OFL notice\n"
        )
        self.assertEqual(
            (licenses / "third-party-desktop/Liberation-license.md").read_text(),
            "font license\n",
        )
        self.assertEqual(
            (
                licenses / "third-party-desktop/sources/liberation-fonts-1.05.tar.gz"
            ).read_bytes(),
            b"source archive",
        )
        npm_calls = [call for call in build.calls if call[0] == "npm"]
        self.assertEqual(len(npm_calls), 4)
        self.assertEqual(
            {call[call.index("--prefix") + 1] for call in npm_calls},
            {
                str((self.private / "crates/frontend-core/renderer").resolve()),
                str(
                    (self.private / "crates/plugin/builtins/canvas/renderer").resolve()
                ),
            },
        )
        app_build = next(call for call in build.calls if call[:2] == ("cargo", "build"))
        self.assertIn("lumvise-desktop-product", app_build)
        self.assertIn("native-voice,native-vector", app_build)
        self.assertIn(str((self.private / "Cargo.toml").resolve()), app_build)
        release = next(call for call in build.calls if call[:2] == ("cargo", "run"))
        self.assertEqual(release.count("--workspace"), 2)
        self.assertIn(str(self.core.resolve()), release)
        self.assertIn(str(self.private.resolve()), release)
        self.assertIn("full", release)
        self.assertEqual(
            build.environment["CARGO_TARGET_DIR"],
            str((self.private / "target").resolve()),
        )
        self.assertTrue((build.diagnostics_dir / "host/manifest.json").is_file())
        self.assertTrue((build.diagnostics_dir / "canvas/manifest.json").is_file())
        self.assertEqual(
            (
                self.private
                / "crates/frontend-core/renderer/assets/.lumvise-build-mode"
            ).read_text(),
            "production",
        )
        symbols = json.loads(
            (build.diagnostics_dir / "native/app/full/manifest.json").read_text()
        )
        self.assertEqual(symbols["uuid"], "01234567-89ab-cdef-0123-456789abcdef")
        self.assertRegex(symbols["executableSha256"], r"^[0-9a-f]{64}$")
        self.assertEqual(
            symbols["executableSha256"],
            hashlib.sha256(
                (self.output / "Lumvise.app/Contents/MacOS/lumvise").read_bytes()
            ).hexdigest(),
        )
        self.assertLess(
            self.call_index(build, ("xcrun", "strip")),
            self.call_index(build, ("codesign", "--force")),
        )
        self.assertFalse(list(self.output.rglob("*.map")))
        self.assertFalse(list(self.output.rglob("*.dSYM")))

    def test_full_same_workspace_passes_one_workspace_root(self) -> None:
        combined = self.root / "combined"
        self.make_workspace(combined, private=True)
        build = self.make_builder("full", core=combined, private=combined)
        build.build()
        release = next(call for call in build.calls if call[:2] == ("cargo", "run"))
        self.assertEqual(release.count("--workspace"), 1)
        self.assertEqual(
            release[release.index("--workspace") + 1], str(combined.resolve())
        )
        self.assertEqual(
            build.environment["CARGO_TARGET_DIR"], str((combined / "target").resolve())
        )

    def test_full_requires_explicit_private_workspace_before_building(self) -> None:
        build = self.make_builder("full")
        with self.assertRaisesRegex(ValueError, "--private-workspace is required"):
            build.build()
        self.assertFalse(build.calls)

    def test_reviewed_extra_notice_tree_is_copied_in_full(self) -> None:
        notices = self.root / "reviewed-notices"
        (notices / "licenses").mkdir(parents=True)
        (notices / "licenses/dependency.md").write_text("reviewed notice\n")
        (notices / "inventory.json").write_text("metadata\n")
        (notices / "provenance/source.tar.gz").parent.mkdir()
        (notices / "provenance/source.tar.gz").write_bytes(b"source archive")
        build = self.make_builder("community", third_party_notices=notices)
        build.build()
        licenses = self.output / "Lumvise Community.app/Contents/Resources/licenses"
        self.assertEqual(
            (licenses / "third-party-notices/licenses/dependency.md").read_text(),
            "reviewed notice\n",
        )
        self.assertEqual(
            (licenses / "third-party-notices/inventory.json").read_text(), "metadata\n"
        )
        self.assertEqual(
            (licenses / "third-party-notices/provenance/source.tar.gz").read_bytes(),
            b"source archive",
        )
        self.assertTrue(
            build.is_notice_subtree(
                Path(
                    "Contents/Resources/licenses/third-party-notices/licenses/dependency.md"
                )
            )
        )

    def test_prepared_plugin_release_is_verified_and_reused_without_compiling(
        self,
    ) -> None:
        prepared = self.root / "prepared-community"
        self.make_prepared_release(prepared, "minimal")
        target = self.root / "shared-target-cache"
        build = self.make_builder(
            "community", plugin_release=prepared, target_dir=target
        )
        build.build()
        verify = next(
            call
            for call in build.calls
            if call[:2] == ("cargo", "run") and "--verify-release" in call
        )
        self.assertIn("--verify-release", verify)
        self.assertIn(str(self.key.resolve()), verify)
        self.assertIn(str(prepared.resolve()), verify)
        self.assertEqual(verify[-1], installer.TARGET)
        self.assertFalse(
            any(
                call[:2] == ("cargo", "run") and "--composition" in call
                for call in build.calls
            )
        )
        self.assertEqual(build.target_dir, target.resolve())
        self.assertTrue(
            (
                self.output
                / "Lumvise Community.app/Contents/Resources/builtins/builtins-release.json"
            ).is_file()
        )

    def test_prepared_plugin_release_wrong_composition_is_rejected(self) -> None:
        prepared = self.root / "prepared-full"
        self.make_prepared_release(prepared, "full")
        build = self.make_builder("community", plugin_release=prepared)
        with self.assertRaisesRegex(ValueError, "expected nonempty minimal release"):
            build.build()
        self.assertFalse(self.output.exists())

    def test_prepared_plugin_release_signature_failure_stops_before_copy(self) -> None:
        prepared = self.root / "prepared-community"
        self.make_prepared_release(prepared, "minimal")
        build = self.make_builder("community", plugin_release=prepared)
        build.prepared_signature_valid = False
        with self.assertRaises(subprocess.CalledProcessError):
            build.build()
        self.assertFalse(self.output.exists())
        self.assertFalse(any(call[:2] == ("cargo", "build") for call in build.calls))
        self.assertFalse(
            any(
                call[:2] == ("cargo", "run") and "--composition" in call
                for call in build.calls
            )
        )

    def test_legal_notice_subtrees_allow_license_sources_but_other_payload_does_not(
        self,
    ) -> None:
        build = self.make_builder("full", private=self.private)
        app = self.root / "audit.app"
        legal = app / "Contents/Resources/licenses/third-party-notices"
        legal.mkdir(parents=True)
        (legal / "copyright.md").write_text("reviewed license\n")
        (legal / "vendor-source.tar.gz").write_bytes(b"archive")
        build.audit_private_payload_files(app)
        (app / "Contents/Resources/private-source.rs").write_text("source\n")
        with self.assertRaisesRegex(ValueError, "private-source.rs"):
            build.audit_private_payload_files(app)
        (app / "Contents/Resources/private-source.rs").unlink()
        (legal / "leaked.map").write_text("{}")
        with self.assertRaisesRegex(ValueError, "leaked.map"):
            build.audit_private_payload_files(app)

    def test_community_rejects_extra_plugin_and_full_requires_both_private_plugins(
        self,
    ) -> None:
        community = self.make_builder("community")
        community.extra_plugin_ids = ["builtin.canvas"]
        with self.assertRaisesRegex(ValueError, "Community plugin IDs"):
            community.build()
        full = self.make_builder("full", private=self.private)
        full.omit_canvas = True
        with self.assertRaisesRegex(ValueError, "Full plugin IDs"):
            full.build()

    def test_symbols_maps_sources_and_harnesses_never_enter_payload(self) -> None:
        for attribute, expected in (
            ("include_release_map", "forbidden source/map/symbol/test files"),
            ("include_release_source", "source/map/symbol/test files"),
            ("include_release_harness", "source/map/symbol/test files"),
        ):
            with self.subTest(attribute=attribute):
                build = self.make_builder("full", private=self.private)
                setattr(build, attribute, True)
                with self.assertRaisesRegex(ValueError, expected):
                    build.build()
                self.assertFalse(self.output.exists())

    def test_diagnostics_must_be_outside_published_output(self) -> None:
        build = self.make_builder("community", diagnostics=self.output / "diagnostics")
        with self.assertRaisesRegex(ValueError, "outside installer output"):
            build.build()
        self.assertFalse(build.calls)

    def test_release_signing_and_notarization_remain_ordered(self) -> None:
        build = self.make_builder(
            "full", private=self.private, ad_hoc=False, third_party_notices=self.notices
        )
        dmg = build.build()
        uploads = [call for call in build.calls if "notarytool" in call]
        staples = [call for call in build.calls if "staple" in call]
        self.assertEqual(len(uploads), 2)
        self.assertTrue(uploads[0][3].endswith(".zip"))
        self.assertTrue(uploads[1][3].endswith(".dmg"))
        self.assertEqual(len(staples), 2)
        dmg_build = next(
            call for call in build.calls if call[:2] == ("hdiutil", "create")
        )
        self.assertLess(build.calls.index(staples[0]), build.calls.index(dmg_build))
        app_signing = next(call for call in build.calls if "--entitlements" in call)
        self.assertIn("runtime", app_signing)
        self.assertIn("--timestamp", app_signing)
        self.assertNotIn("adhoc", dmg.name)

    def test_rejected_notarization_or_command_never_publishes(self) -> None:
        build = self.make_builder(
            "full", private=self.private, ad_hoc=False, third_party_notices=self.notices
        )
        build.notary_status = "Invalid"
        with self.assertRaisesRegex(
            ValueError, "fixture-submission.*expected Accepted"
        ):
            build.build()
        self.assertFalse(self.output.exists())

    def test_official_build_requires_complete_included_license_texts(self) -> None:
        build = self.make_builder("community", ad_hoc=False)
        with self.assertRaisesRegex(ValueError, "expected --third-party-notices"):
            build.build()
        build.third_party_notices = self.notices
        inventory = self.notices / "inventory.json"
        record = json.loads(inventory.read_text())
        record["packages"][0]["missing_license_text"] = True
        inventory.write_text(json.dumps(record))
        with self.assertRaisesRegex(ValueError, "fixture@1.*expected license text"):
            build.build()
        record["packages"][0]["missing_license_text"] = False
        inventory.write_text(json.dumps(record))
        (self.notices / "LICENSE").unlink()
        with self.assertRaisesRegex(ValueError, "expected included license text"):
            build.build()
        self.assertFalse(build.calls)
        build = self.make_builder("community")
        build.fail_command = "codesign"
        with self.assertRaises(subprocess.CalledProcessError):
            build.build()
        self.assertFalse(self.output.exists())

    def test_missing_credentials_insecure_key_and_bad_architecture_fail(self) -> None:
        build = self.make_builder("full", private=self.private, ad_hoc=False)
        build.identity = "-"
        with self.assertRaisesRegex(ValueError, "Developer ID Application"):
            build.build()
        build = self.make_builder("community")
        self.key.chmod(0o644)
        with self.assertRaisesRegex(ValueError, "mode 600"):
            build.build()
        self.key.chmod(0o600)
        build = self.make_builder("community")
        build.architecture = "x86_64"
        with self.assertRaisesRegex(ValueError, "expected arm64"):
            build.build()
        self.assertFalse(self.output.exists())

    def test_existing_release_is_preserved(self) -> None:
        self.output.mkdir(parents=True)
        old_release = self.output / "old.dmg"
        old_release.write_bytes(b"previous release")
        build = self.make_builder("community")
        with self.assertRaisesRegex(ValueError, "missing or empty directory"):
            build.build()
        self.assertEqual(old_release.read_bytes(), b"previous release")
        self.assertFalse(build.calls)

    @staticmethod
    def call_index(build: FakeBuildTools, prefix: tuple[str, ...]) -> int:
        return next(
            index
            for index, call in enumerate(build.calls)
            if call[: len(prefix)] == prefix
        )

    def make_prepared_release(self, directory: Path, composition: str) -> None:
        build = self.make_builder(
            composition if composition in ("community", "full") else "community"
        )
        build.stage_release(
            directory,
            composition,
        )


if __name__ == "__main__":
    unittest.main()
