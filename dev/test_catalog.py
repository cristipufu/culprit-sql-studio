import contextlib
import importlib.util
import io
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("catalog", Path(__file__).with_name("catalog.py"))
catalog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog)
merge_spec = importlib.util.spec_from_file_location("merge_catalog", Path(__file__).with_name("merge-catalog.py"))
merge_catalog = importlib.util.module_from_spec(merge_spec)
merge_spec.loader.exec_module(merge_catalog)


class TranslationSources(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        self.root = Path(self.directory.name)
        self.locales = self.root / "locales"
        self.locales.mkdir()
        self.patch = patch.object(catalog, "LOCALES", self.locales)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.write("en", "Read {name}.")
        self.write("fr", "Lire {name}.")

    def write(self, code, message):
        catalog.save(code, {"_meta": {"english_name": code}, "message": message})

    def test_rewording_without_argument_changes_requires_review(self):
        catalog.review("fr")
        self.write("en", "Inspect {name}.")
        stale = catalog.stale_messages(catalog.load("en"), catalog.load("fr"))
        self.assertEqual(stale["message"]["status"], "stale")
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(catalog.stale("fr", check=True), 1)
        catalog.review("fr")
        self.assertEqual(catalog.stale_messages(catalog.load("en"), catalog.load("fr")), {})

    def test_adding_a_translation_tracks_only_the_submitted_messages(self):
        changes = self.root / "translation.json"
        changes.write_text(json.dumps({"message": "Inspecter {name}."}))
        with contextlib.redirect_stdout(io.StringIO()):
            catalog.add("fr", changes)
        self.assertEqual(catalog.stale_messages(catalog.load("en"), catalog.load("fr")), {})
        self.write("en", "Read {table}.")
        with self.assertRaisesRegex(SystemExit, "arguments differ"):
            catalog.add("fr", changes)

    def test_history_keeps_the_original_english_source(self):
        def commit():
            subprocess.run(["git", "-C", str(self.root), "add", "locales"], check=True, capture_output=True)
            subprocess.run(["git", "-C", str(self.root), "-c", "user.name=Catalog test", "-c", "user.email=catalog@example.invalid", "commit", "-m", "Catalog"], check=True, capture_output=True)

        subprocess.run(["git", "init", "-q", str(self.root)], check=True, capture_output=True)
        commit()
        self.write("en", "Inspect {name}.")
        commit()
        catalog.track("fr")
        self.assertEqual(catalog.load("fr")["_meta"]["source_hashes"]["message"], catalog.fingerprint("Read {name}."))
        self.assertEqual(catalog.stale_messages(catalog.load("en"), catalog.load("fr"))["message"]["status"], "stale")

    def test_a_batch_cannot_claim_to_translate_english_changed_after_export(self):
        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            catalog.todo("fr")
        batch = json.loads(output.getvalue())
        batch["message"] = "Lire {name}."
        path = self.root / "translation.json"
        path.write_text(json.dumps(batch))
        self.write("en", "Inspect {name}.")
        with self.assertRaisesRegex(SystemExit, "English changed since this batch was exported"):
            catalog.add("fr", path)
        self.assertEqual(catalog.load("fr")["message"], "Lire {name}.")


class CatalogMerges(unittest.TestCase):
    def test_independent_translation_reviews_merge_alongside_their_messages(self):
        base = {"_meta": {"source_hashes": {}}, "first": "old", "second": "old"}
        ours = {"_meta": {"source_hashes": {"first": "source1"}}, "first": "new1", "second": "old"}
        theirs = {"_meta": {"source_hashes": {"second": "source2"}}, "first": "old", "second": "new2"}
        merged, conflicts = merge_catalog.merge(base, ours, theirs)
        self.assertEqual(conflicts, [])
        self.assertEqual(merged, {"_meta": {"source_hashes": {"first": "source1", "second": "source2"}}, "first": "new1", "second": "new2"})

    def test_conflicting_sources_are_reported_at_the_message_key(self):
        merged, conflicts = merge_catalog.merge(
            {"_meta": {"source_hashes": {"first": "old"}}},
            {"_meta": {"source_hashes": {"first": "one"}}},
            {"_meta": {"source_hashes": {"first": "two"}}})
        self.assertEqual(conflicts, ["_meta.source_hashes.first"])
        self.assertEqual(merged["_meta"]["source_hashes"]["first"], "one")


if __name__ == "__main__":
    unittest.main()
