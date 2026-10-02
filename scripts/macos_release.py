"""Own retained macOS release staging and Apple submission receipts.

The installer calls ``MacOSRelease.staging``, ``notarize_app`` and
``notarize_dmg``. Submission persistence and resume checks stay internal here.
"""

from __future__ import annotations

from collections.abc import Iterator
from contextlib import contextmanager
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
from typing import Protocol


class ReleaseCommand(Protocol):
    """Run checked release commands; e.g. ``command('codesign', '--verify', app)``."""

    def __call__(self, *arguments: str | Path) -> str: ...


class MacOSRelease:
    """Retain signed uploads across interruptions, e.g. ``release.notarize_app(app, stage)``."""

    def __init__(
        self, command: ReleaseCommand, identity: str, profile: str, ad_hoc: bool
    ) -> None:
        self.command = command
        self.identity = identity
        self.profile = profile
        self.ad_hoc = ad_hoc

    @contextmanager
    def staging(self, output: Path, resume_from: Path | None) -> Iterator[Path]:
        """Keep failed uploads; e.g. ``with release.staging(output, prior_stage) as stage:``."""
        output.parent.mkdir(parents=True, exist_ok=True)
        staging = self._open_staging(output, resume_from)
        try:
            yield staging
        except BaseException:
            print(
                f"Release staging retained at {staging}; after fixing the error, "
                f"repeat this command with --resume-from {str(staging)!r}.",
                file=sys.stderr,
            )
            raise
        else:
            shutil.rmtree(staging)

    def _open_staging(self, output: Path, resume_from: Path | None) -> Path:
        expected = {
            "output": str(output),
            "identity": self.identity,
            "ad_hoc": self.ad_hoc,
        }
        if resume_from is not None:
            staging = resume_from.resolve()
            actual = json.loads((staging / "installer-stage.json").read_text())
            if actual != expected or not staging.name.startswith(".macos-installer-"):
                raise ValueError(
                    f"resume staging {staging}: {actual!r}; expected {expected!r}"
                )
            return staging
        staging = Path(tempfile.mkdtemp(prefix=".macos-installer-", dir=output.parent))
        self._save_release_record(staging / "installer-stage.json", expected)
        return staging

    def notarize_app(self, app: Path, staging: Path) -> None:
        """Notarize the retained app ZIP; e.g. ``release.notarize_app(app, stage)``."""
        if self.ad_hoc:
            return
        archive = staging / "Lumvise-notarization.zip"
        if not archive.exists():
            self.command("ditto", "-c", "-k", "--keepParent", app, archive)
        self._notarize(archive, app, staging / "app-notarization.json")
        self.command("spctl", "--assess", "--type", "execute", "--verbose=2", app)

    def notarize_dmg(self, dmg: Path, staging: Path) -> None:
        """Sign and notarize a disk image once; e.g. ``release.notarize_dmg(dmg, stage)``."""
        if self.ad_hoc:
            return
        receipt = staging / "dmg-notarization.json"
        if not receipt.exists():
            self.command("codesign", "--sign", self.identity, "--timestamp", dmg)
        self._notarize(dmg, dmg, receipt)
        self.command(
            "spctl",
            "--assess",
            "--type",
            "open",
            "--context",
            "context:primary-signature",
            dmg,
        )

    def _notarize(self, upload: Path, staple: Path, receipt: Path) -> None:
        submission = self._submission(upload, receipt)
        if submission.get("status") not in ("Accepted", "Invalid", "Rejected"):
            submission = self._wait(submission, receipt)
        if submission.get("status") != "Accepted":
            raise ValueError(
                f"notarization {submission!r}; expected Accepted "
                "(retrieve details with xcrun notarytool log <id> --keychain-profile <profile>)"
            )
        self.command("xcrun", "stapler", "staple", staple)
        self.command("xcrun", "stapler", "validate", staple)
        # Stapling a DMG changes its bytes; recognize that verified state on resume.
        submission["stapled_sha256"] = self._upload_digest(upload)
        self._save_release_record(receipt, submission)

    def _submission(self, upload: Path, receipt: Path) -> dict[str, object]:
        digest = self._upload_digest(upload)
        if receipt.exists():
            return self._read_submission(receipt, upload, digest)
        submission = {**self._notary_result("submit", upload), "upload_sha256": digest}
        # Persist the ID before a network-dependent wait can fail or be interrupted.
        self._save_release_record(receipt, submission)
        return submission

    def _wait(self, submission: dict[str, object], receipt: Path) -> dict[str, object]:
        try:
            result = self._notary_result("wait", str(submission["id"]))
        except subprocess.CalledProcessError as error:
            if not error.output:
                raise
            result = self._parse_submission(error.output)
        if result["id"] != submission["id"]:
            raise ValueError(
                f"notarization ID {result['id']!r}; expected {submission['id']!r}"
            )
        updated = {**submission, **result}
        self._save_release_record(receipt, updated)
        return updated

    def _read_submission(
        self, receipt: Path, upload: Path, digest: str
    ) -> dict[str, object]:
        submission = self._parse_submission(receipt.read_text())
        allowed = {submission.get("upload_sha256")}
        if submission.get("status") == "Accepted":
            allowed.add(submission.get("stapled_sha256"))
        if digest not in allowed:
            raise ValueError(
                f"upload {upload} SHA-256 {digest}; expected retained submission bytes {allowed!r}"
            )
        return submission

    def _notary_result(self, operation: str, subject: str | Path) -> dict[str, object]:
        response = self.command(
            "xcrun",
            "notarytool",
            operation,
            subject,
            "--keychain-profile",
            self.profile,
            "--output-format",
            "json",
        )
        return self._parse_submission(response)

    @staticmethod
    def _parse_submission(response: str) -> dict[str, object]:
        value = json.loads(response)
        if (
            not isinstance(value, dict)
            or not isinstance(value.get("id"), str)
            or not value["id"]
        ):
            raise ValueError(
                f"notarization response {value!r}; expected a nonempty submission id"
            )
        return value

    @staticmethod
    def _upload_digest(upload: Path) -> str:
        with upload.open("rb") as stream:
            return hashlib.file_digest(stream, "sha256").hexdigest()

    @staticmethod
    def _save_release_record(path: Path, record: dict[str, object]) -> None:
        temporary = path.with_suffix(".json.tmp")
        temporary.write_text(json.dumps(record, indent=2) + "\n")
        temporary.replace(path)
