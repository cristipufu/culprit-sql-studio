import tempfile
import unittest
from pathlib import Path

from configure_ubuntu_mirror import UBUNTU_MIRROR, configure_mirror


class UbuntuMirrorTests(unittest.TestCase):
    def test_legacy_sources_keep_security_suites_components_and_signing_options(self):
        with tempfile.TemporaryDirectory() as directory:
            apt = Path(directory)
            source = apt / "sources.list"
            original = (
                "deb [signed-by=/ubuntu.gpg] http://azure.archive.ubuntu.com/ubuntu/ jammy main universe\n"
                "deb https://security.ubuntu.com/ubuntu jammy-security main universe\n"
            )
            source.write_text(original)
            self.assertEqual(configure_mirror(apt), 1)
            self.assertEqual(source.read_text(), original.replace(
                "http://azure.archive.ubuntu.com/ubuntu/", UBUNTU_MIRROR
            ).replace("https://security.ubuntu.com/ubuntu", UBUNTU_MIRROR))

    def test_deb822_sources_keep_their_signature_policy_and_all_pockets(self):
        with tempfile.TemporaryDirectory() as directory:
            apt = Path(directory)
            sources = apt / "sources.list.d"
            sources.mkdir()
            source = sources / "ubuntu.sources"
            original = (
                "Types: deb\nURIs: https://archive.ubuntu.com/ubuntu/\n"
                "Suites: resolute resolute-updates resolute-backports resolute-security\n"
                "Components: main universe\nSigned-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg\n"
            )
            source.write_text(original)
            configure_mirror(apt)
            self.assertEqual(source.read_text(), original.replace(
                "https://archive.ubuntu.com/ubuntu/", UBUNTU_MIRROR
            ))

    def test_runner_mirror_lists_change_without_breaking_their_file_references(self):
        with tempfile.TemporaryDirectory() as directory:
            apt = Path(directory)
            source = apt / "sources.list"
            reference = "deb mirror+file:/etc/apt/apt-mirrors.txt jammy main\n"
            source.write_text(reference)
            mirrors = apt / "apt-mirrors.txt"
            mirrors.write_text("http://azure.archive.ubuntu.com/ubuntu\nhttps://archive.ubuntu.com/ubuntu\n")
            self.assertEqual(configure_mirror(apt), 1)
            self.assertEqual(source.read_text(), reference)
            self.assertEqual(mirrors.read_text(), f"{UBUNTU_MIRROR}\n{UBUNTU_MIRROR}\n")

    def test_unrelated_repositories_remain_unchanged_and_reconfiguration_is_idempotent(self):
        with tempfile.TemporaryDirectory() as directory:
            apt = Path(directory)
            source = apt / "sources.list"
            original = "deb https://packages.microsoft.com/ubuntu/26.04/prod resolute main\n"
            source.write_text(original)
            self.assertEqual(configure_mirror(apt), 0)
            self.assertEqual(source.read_text(), original)
            source.write_text(f"deb {UBUNTU_MIRROR} jammy main\n")
            self.assertEqual(configure_mirror(apt), 0)


if __name__ == "__main__":
    unittest.main()
