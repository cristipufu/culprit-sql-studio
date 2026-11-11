import tempfile
import unittest
from pathlib import Path

from create_macos_dmg import MEBIBYTE, image_capacity_mib


class DiskImageCapacityTests(unittest.TestCase):
    def test_sparse_binary_is_sized_by_its_logical_length(self):
        with tempfile.TemporaryDirectory() as root:
            source = Path(root)
            with (source / "binary").open("wb") as file:
                file.truncate(512 * MEBIBYTE)
            self.assertGreater(image_capacity_mib(source), 512)

    def test_external_directory_symlink_does_not_include_its_target(self):
        with tempfile.TemporaryDirectory() as root:
            root = Path(root)
            source, external = root / "source", root / "external"
            source.mkdir()
            external.mkdir()
            with (external / "unrelated").open("wb") as file:
                file.truncate(2048 * MEBIBYTE)
            (source / "Applications").symlink_to(external, target_is_directory=True)
            self.assertLess(image_capacity_mib(source), 128)

    def test_large_payload_keeps_proportional_filesystem_headroom(self):
        with tempfile.TemporaryDirectory() as root:
            source = Path(root)
            with (source / "binary").open("wb") as file:
                file.truncate(1024 * MEBIBYTE)
            self.assertGreaterEqual(image_capacity_mib(source), 1280)


if __name__ == "__main__":
    unittest.main()
