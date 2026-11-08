#!/usr/bin/env python3
"""Work on a language catalog (locales/<code>.json) without editing JSON by hand.

    python3 dev/catalog.py todo <code> [prefix]     the messages <code> lacks, as JSON {key: English}, to translate
    python3 dev/catalog.py add <code> <file.json>   put the translated {key: message} pairs in <file.json> into the catalog
    python3 dev/catalog.py status                   how much of English each language says

`add` refuses a key English doesn't have, so a typo can't slip in; `app/src/lib/catalogs.test.ts` and the Rust catalog
test then check that every message reads and keeps English's arguments. See docs/localization.md.
"""

import json
import sys
from pathlib import Path

LOCALES = Path(__file__).resolve().parent.parent / "locales"


def load(code):
    return json.loads((LOCALES / f"{code}.json").read_text(encoding="utf-8"))


def save(code, catalog):
    text = json.dumps(catalog, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    (LOCALES / f"{code}.json").write_text(text, encoding="utf-8")


def todo(code, prefix=""):
    english, own = load("en"), load(code)
    missing = {k: v for k, v in english.items() if k != "_meta" and k not in own and k.startswith(prefix)}
    print(json.dumps(missing, indent=2, ensure_ascii=False, sort_keys=True))


def add(code, path):
    english, own = load("en"), load(code)
    found = json.loads(Path(path).read_text(encoding="utf-8"))
    unknown = sorted(k for k in found if k not in english or k == "_meta")
    if unknown:
        sys.exit("keys English doesn't have: " + ", ".join(unknown))
    bad = sorted(k for k, v in found.items() if not isinstance(v, str) or not v.strip())
    if bad:
        sys.exit("not a message: " + ", ".join(bad))
    own.update(found)
    save(code, own)
    print(f"{code}: {len(found)} messages added or replaced, {len(own) - 1} of {len(english) - 1}")


def status():
    english = load("en")
    total = len(english) - 1
    for path in sorted(LOCALES.glob("*.json")):
        own = json.loads(path.read_text(encoding="utf-8"))
        said = sum(1 for k in own if k != "_meta" and k in english)
        print(f"{path.stem:8} {own['_meta']['english_name']:12} {said:5} of {total} ({100 * said // max(total, 1)}%)")


if __name__ == "__main__":
    args = sys.argv[1:]
    if args[:1] == ["todo"] and len(args) in (2, 3):
        todo(*args[1:])
    elif args[:1] == ["add"] and len(args) == 3:
        add(*args[1:])
    elif args == ["status"]:
        status()
    else:
        sys.exit(__doc__)
