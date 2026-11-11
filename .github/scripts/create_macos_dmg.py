#!/usr/bin/env python3
"""Create an installer DMG with capacity based on logical file lengths, including sparse binaries."""

import argparse
import os
from pathlib import Path
import shutil
import stat
import subprocess

MEBIBYTE = 1024 * 1024
MINIMUM_HEADROOM = 64 * MEBIBYTE
ENTRY_ALLOWANCE = 4096


def image_capacity_mib(source: Path) -> int:
    """Do not follow symlinks: Applications and framework links retain their own directory entries."""
    pending = [source]
    content_bytes = 0
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                metadata = entry.stat(follow_symlinks=False)
                content_bytes += ENTRY_ALLOWANCE
                if stat.S_ISDIR(metadata.st_mode):
                    pending.append(Path(entry.path))
                elif stat.S_ISREG(metadata.st_mode):
                    content_bytes += metadata.st_size
                elif not stat.S_ISLNK(metadata.st_mode):
                    raise ValueError(f"Unsupported disk-image source entry: {entry.path}")
    headroom = max(MINIMUM_HEADROOM, (content_bytes + 3) // 4)
    return (content_bytes + headroom + MEBIBYTE - 1) // MEBIBYTE


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--volume-name", default="Culprit SQL Studio")
    args = parser.parse_args()
    capacity = image_capacity_mib(args.source)
    free = shutil.disk_usage(args.output.resolve().parent).free // MEBIBYTE
    print(f"DMG capacity: {capacity} MiB; host filesystem free: {free} MiB", flush=True)
    subprocess.run(["hdiutil", "create", "-volname", args.volume_name, "-srcfolder", str(args.source),
                    "-size", f"{capacity}m", "-fs", "HFS+", "-ov", "-format", "UDZO", str(args.output)], check=True)


if __name__ == "__main__":
    main()
