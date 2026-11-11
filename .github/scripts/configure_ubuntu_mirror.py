#!/usr/bin/env python3
"""Use an independent Ubuntu mirror on ephemeral CI runners, retaining APT's signing policy."""

from pathlib import Path
import re

# Canonical's DNS outage on 2026-11-11 blocked all three default runner mirrors.
# This mirror serves Ubuntu's signed archive, including the security pockets.
UBUNTU_MIRROR = "https://mirrors.edge.kernel.org/ubuntu"
DEFAULT_ARCHIVE = re.compile(
    r"https?://(?:azure\.archive|archive|security)\.ubuntu\.com/ubuntu/?(?=\s|$)"
)


def configure_mirror(apt_directory: Path) -> int:
    sources = [apt_directory / "sources.list"]
    sources.extend(apt_directory.glob("sources.list.d/*.list"))
    sources.extend(apt_directory.glob("sources.list.d/*.sources"))
    # GitHub runner images also use mirror+file URIs in both source formats.
    sources.extend(apt_directory.glob("*mirrors*.txt"))
    changed = 0
    for source in sources:
        if not source.is_file():
            continue
        original = source.read_text()
        updated = DEFAULT_ARCHIVE.sub(UBUNTU_MIRROR, original)
        if updated != original:
            source.write_text(updated)
            changed += 1
    return changed


if __name__ == "__main__":
    changed = configure_mirror(Path("/etc/apt"))
    print(f"Ubuntu mirror: {UBUNTU_MIRROR}; {changed} source files updated; APT signature policy unchanged")
