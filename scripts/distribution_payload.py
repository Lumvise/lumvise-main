"""Own the shared license and reviewed-notice payload for desktop distributions.

macOS and native package builders call this component so they ship the same
license tree. Example: `DistributionLicensePayload(core, "community").copy_to(resources)`.
"""

from __future__ import annotations

import shutil
from pathlib import Path


class DistributionLicensePayload:
    """Copy the edition's reviewed license files into a package resources directory."""

    def __init__(
        self,
        core_workspace: Path,
        edition: str,
        private_workspace: Path | None = None,
        third_party_notices: Path | None = None,
    ) -> None:
        if edition not in ("community", "full"):
            raise ValueError(f"edition {edition!r}; expected community or full")
        self.core_workspace = core_workspace
        self.edition = edition
        self.private_workspace = private_workspace
        self.third_party_notices = third_party_notices

    def copy_to(self, resources: Path) -> None:
        """Assemble licenses, e.g. `payload.copy_to(package / 'resources')`."""
        licenses = resources / "licenses"
        licenses.mkdir()
        self._copy_public_licenses(licenses)
        if self.edition == "full":
            self._copy_full_licenses(licenses)
        if self.third_party_notices is not None:
            self._copy_reviewed_tree(
                self.third_party_notices,
                licenses / "third-party-notices",
            )

    def _copy_public_licenses(self, licenses: Path) -> None:
        shutil.copy2(self.core_workspace / "LICENSE", licenses / "LICENSE")
        shutil.copy2(
            self.core_workspace / "distribution/AI-RISK-NOTICE.txt",
            licenses / "AI-RISK-NOTICE.txt",
        )
        shutil.copytree(self.core_workspace / "LICENSES", licenses / "LICENSES")

    def _copy_full_licenses(self, licenses: Path) -> None:
        if self.private_workspace is None:
            raise ValueError("--private-workspace is required for Full licenses")
        shutil.copy2(
            self.private_workspace / "LICENSE",
            licenses / "proprietary-LICENSE",
        )
        self._copy_reviewed_tree(
            self.private_workspace / "distribution/third-party-desktop",
            licenses / "third-party-desktop",
        )

    @staticmethod
    def _copy_reviewed_tree(source: Path, destination: Path) -> None:
        if not any(source.rglob("*")):
            raise ValueError(f"notice directory {source}; expected reviewed files")
        shutil.copytree(source, destination)
