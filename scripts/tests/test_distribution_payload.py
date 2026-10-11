"""Shared distribution license-payload behavior with real temporary trees."""

from __future__ import annotations

import importlib.util
from pathlib import Path
import tempfile
import unittest


MODULE_PATH = Path(__file__).resolve().parents[1] / "distribution_payload.py"
SPEC = importlib.util.spec_from_file_location("distribution_payload", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
distribution_payload = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(distribution_payload)
DistributionLicensePayload = distribution_payload.DistributionLicensePayload


class DistributionPayloadFixture:
    def __init__(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.core = self.root / "core"
        self._write_core()

    def close(self) -> None:
        self.temporary.cleanup()

    def _write_core(self) -> None:
        (self.core / "LICENSES").mkdir(parents=True)
        (self.core / "distribution").mkdir()
        (self.core / "LICENSE").write_text("community license\n")
        (self.core / "LICENSES/MIT.txt").write_text("third-party license\n")
        (self.core / "distribution/AI-RISK-NOTICE.txt").write_text("AI notice\n")

    def private_workspace(self, *, reviewed_notices: bool = True) -> Path:
        private = self.root / "private"
        private.mkdir()
        (private / "LICENSE").write_text("proprietary license\n")
        if reviewed_notices:
            notices = private / "distribution/third-party-desktop"
            notices.mkdir(parents=True)
            (notices / "font-license.txt").write_text("font notice\n")
        return private

    def resources(self, name: str) -> Path:
        resources = self.root / name
        resources.mkdir()
        return resources


def relative_files(root: Path) -> list[str]:
    return sorted(
        path.relative_to(root).as_posix() for path in root.rglob("*") if path.is_file()
    )


class DistributionLicensePayloadTests(unittest.TestCase):
    def setUp(self) -> None:
        self.fixture = DistributionPayloadFixture()
        self.addCleanup(self.fixture.close)

    def test_unknown_edition_cannot_silently_ship_community_licenses(self) -> None:
        with self.assertRaisesRegex(
            ValueError, "edition 'ful'; expected community or full"
        ):
            DistributionLicensePayload(self.fixture.core, "ful")

    def test_community_copies_only_public_license_payload(self) -> None:
        resources = self.fixture.resources("community-resources")
        DistributionLicensePayload(self.fixture.core, "community").copy_to(resources)
        licenses = resources / "licenses"

        self.assertEqual(
            relative_files(licenses),
            ["AI-RISK-NOTICE.txt", "LICENSE", "LICENSES/MIT.txt"],
        )
        self.assertEqual((licenses / "LICENSE").read_text(), "community license\n")

    def test_full_copies_proprietary_license_and_reviewed_desktop_notices(self) -> None:
        private = self.fixture.private_workspace()
        resources = self.fixture.resources("full-resources")
        DistributionLicensePayload(
            self.fixture.core, "full", private_workspace=private
        ).copy_to(resources)
        licenses = resources / "licenses"

        self.assertEqual(
            relative_files(licenses),
            [
                "AI-RISK-NOTICE.txt",
                "LICENSE",
                "LICENSES/MIT.txt",
                "proprietary-LICENSE",
                "third-party-desktop/font-license.txt",
            ],
        )
        self.assertEqual(
            (licenses / "proprietary-LICENSE").read_text(), "proprietary license\n"
        )

    def test_full_requires_private_workspace(self) -> None:
        resources = self.fixture.resources("missing-private-resources")

        with self.assertRaisesRegex(ValueError, "--private-workspace is required"):
            DistributionLicensePayload(self.fixture.core, "full").copy_to(resources)

    def test_full_requires_nonempty_reviewed_desktop_notices(self) -> None:
        private = self.fixture.private_workspace(reviewed_notices=False)
        resources = self.fixture.resources("missing-notices-resources")

        with self.assertRaisesRegex(ValueError, "expected reviewed files"):
            DistributionLicensePayload(
                self.fixture.core, "full", private_workspace=private
            ).copy_to(resources)

    def test_optional_third_party_notice_tree_is_copied_verbatim(self) -> None:
        notices = self.fixture.root / "reviewed-notices"
        (notices / "licenses/nested").mkdir(parents=True)
        (notices / "inventory.json").write_text('{"source":"reviewed"}\n')
        (notices / "licenses/nested/dependency.txt").write_text("notice\n")
        resources = self.fixture.resources("optional-notices-resources")

        DistributionLicensePayload(
            self.fixture.core,
            "community",
            third_party_notices=notices,
        ).copy_to(resources)

        copied = resources / "licenses/third-party-notices"
        self.assertEqual(
            relative_files(copied),
            ["inventory.json", "licenses/nested/dependency.txt"],
        )

    def test_optional_but_empty_notice_tree_is_rejected(self) -> None:
        notices = self.fixture.root / "empty-notices"
        notices.mkdir()
        resources = self.fixture.resources("empty-notices-resources")

        with self.assertRaisesRegex(ValueError, "expected reviewed files"):
            DistributionLicensePayload(
                self.fixture.core,
                "community",
                third_party_notices=notices,
            ).copy_to(resources)


if __name__ == "__main__":
    unittest.main()
