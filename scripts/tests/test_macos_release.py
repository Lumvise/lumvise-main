"""Public Apple-boundary tests for retained macOS notarization state."""

from __future__ import annotations

import hashlib
import importlib.machinery
import importlib.util
import json
from pathlib import Path
import subprocess
import tempfile
import unittest


SCRIPT = Path(__file__).resolve().parents[1] / "macos_release.py"
LOADER = importlib.machinery.SourceFileLoader("macos_release", str(SCRIPT))
SPEC = importlib.util.spec_from_loader(LOADER.name, LOADER)
assert SPEC is not None
release_module = importlib.util.module_from_spec(SPEC)
LOADER.exec_module(release_module)


class FakeAppleCommands:
    """Model notarization, stapling, and Gatekeeper without Apple services."""

    def __init__(
        self,
        *,
        wait_error_status: str | None = None,
        fail_spctl_once: bool = False,
    ) -> None:
        self.calls: list[tuple[str, ...]] = []
        self.wait_error_status = wait_error_status
        self.fail_spctl_once = fail_spctl_once

    def __call__(self, *arguments: str | Path) -> str:
        call = tuple(str(argument) for argument in arguments)
        self.calls.append(call)
        if call[:3] == ("xcrun", "notarytool", "submit"):
            return self.submit_response(Path(call[3]))
        if call[:3] == ("xcrun", "notarytool", "wait"):
            return self.wait_response(call)
        if call[0] == "ditto":
            Path(call[-1]).write_bytes(b"fake app upload bytes")
        if call[0] == "codesign" and "--sign" in call:
            self.sign_dmg(Path(call[-1]))
        if call[:2] == ("xcrun", "stapler"):
            self.staple(call[2], Path(call[3]))
        if call[0] == "spctl":
            self.check_spctl(call)
        return ""

    @staticmethod
    def submit_response(upload: Path) -> str:
        suffix = "app" if upload.suffix == ".zip" else "dmg"
        return json.dumps({"id": f"fixture-submission-{suffix}"})

    def wait_response(self, call: tuple[str, ...]) -> str:
        submission_id = call[3]
        result = {"id": submission_id, "status": "Accepted"}
        if self.wait_error_status is not None:
            result["status"] = self.wait_error_status
            raise subprocess.CalledProcessError(1, call, output=json.dumps(result))
        return json.dumps(result)

    @staticmethod
    def sign_dmg(dmg: Path) -> None:
        contents = dmg.read_bytes()
        if not contents.endswith(b" signed"):
            dmg.write_bytes(contents + b" signed")

    @staticmethod
    def staple(operation: str, target: Path) -> None:
        if operation == "staple" and target.is_file():
            contents = target.read_bytes()
            if not contents.endswith(b" ticket"):
                target.write_bytes(contents + b" ticket")

    def check_spctl(self, call: tuple[str, ...]) -> None:
        if self.fail_spctl_once:
            self.fail_spctl_once = False
            raise subprocess.CalledProcessError(1, call)


class MacOSReleaseTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory(prefix="macos release tests ")
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)

    def test_wait_rejection_is_persisted_and_never_retried_or_stapled(self) -> None:
        commands = FakeAppleCommands(wait_error_status="Invalid")
        release = self.make_release(commands)
        staging, app = self.make_app_stage()
        receipt = staging / "app-notarization.json"

        with self.assertRaisesRegex(ValueError, "expected Accepted"):
            release.notarize_app(app, staging)

        stored = json.loads(receipt.read_text())
        self.assertEqual(stored["id"], "fixture-submission-app")
        self.assertEqual(stored["status"], "Invalid")
        self.assertEqual(
            stored["upload_sha256"],
            hashlib.sha256(
                (staging / "Lumvise-notarization.zip").read_bytes()
            ).hexdigest(),
        )
        self.assertEqual(len(self.calls_for(commands, "submit")), 1)
        self.assertEqual(len(self.calls_for(commands, "wait")), 1)
        self.assertFalse(self.calls_for(commands, "staple"))
        self.assertFalse(any(call[0] == "spctl" for call in commands.calls))

        with self.assertRaisesRegex(ValueError, "expected Accepted"):
            release.notarize_app(app, staging)

        self.assertEqual(len(self.calls_for(commands, "submit")), 1)
        self.assertEqual(len(self.calls_for(commands, "wait")), 1)
        self.assertFalse(self.calls_for(commands, "staple"))
        self.assertTrue(app.is_dir())

    def test_stapled_dmg_receipt_resumes_after_gatekeeper_failure(self) -> None:
        commands = FakeAppleCommands(fail_spctl_once=True)
        release = self.make_release(commands)
        staging, dmg = self.make_dmg_stage()
        receipt = staging / "dmg-notarization.json"

        with self.assertRaises(subprocess.CalledProcessError):
            release.notarize_dmg(dmg, staging)

        signed_and_stapled = dmg.read_bytes()
        stored = json.loads(receipt.read_text())
        self.assertEqual(stored["id"], "fixture-submission-dmg")
        self.assertEqual(stored["status"], "Accepted")
        self.assertEqual(
            stored["upload_sha256"],
            hashlib.sha256(b"original dmg signed").hexdigest(),
        )
        self.assertEqual(
            stored["stapled_sha256"], hashlib.sha256(signed_and_stapled).hexdigest()
        )
        self.assertTrue(signed_and_stapled.endswith(b" ticket"))

        resumed = self.make_release(commands)
        resumed.notarize_dmg(dmg, staging)

        self.assertEqual(dmg.read_bytes(), signed_and_stapled)
        self.assertEqual(len(self.calls_for(commands, "submit")), 1)
        self.assertEqual(len(self.calls_for(commands, "wait")), 1)
        self.assertEqual(len(self.signing_calls(commands)), 1)
        self.assertEqual(len(self.calls_for(commands, "spctl")), 2)

    def make_release(self, commands: FakeAppleCommands) -> object:
        return release_module.MacOSRelease(
            commands,
            identity="Developer ID Application: Fixture (TEAM)",
            profile="fixture-profile",
            ad_hoc=False,
        )

    def make_app_stage(self) -> tuple[Path, Path]:
        staging = self.make_stage("app")
        app = staging / "Fixture.app"
        app.mkdir()
        (app / "Contents").mkdir()
        (app / "Contents/Info.plist").write_text("fixture app")
        return staging, app

    def make_dmg_stage(self) -> tuple[Path, Path]:
        staging = self.make_stage("dmg")
        dmg = staging / "Fixture.dmg"
        dmg.write_bytes(b"original dmg")
        return staging, dmg

    def make_stage(self, name: str) -> Path:
        staging = self.root / f".macos-installer-{name}"
        staging.mkdir()
        return staging

    @staticmethod
    def calls_for(commands: FakeAppleCommands, operation: str) -> list[tuple[str, ...]]:
        return [
            call
            for call in commands.calls
            if call[:3] == ("xcrun", "notarytool", operation)
            or (operation == "staple" and call[:3] == ("xcrun", "stapler", "staple"))
            or (operation == "spctl" and call[0] == "spctl")
        ]

    @staticmethod
    def signing_calls(commands: FakeAppleCommands) -> list[tuple[str, ...]]:
        return [
            call
            for call in commands.calls
            if call[0] == "codesign" and "--sign" in call
        ]


if __name__ == "__main__":
    unittest.main()
