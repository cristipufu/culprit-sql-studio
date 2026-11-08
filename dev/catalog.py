#!/usr/bin/env python3
"""Work on a language catalog (locales/<code>.json) without editing JSON by hand.

    python3 dev/catalog.py todo <code> [prefix]     the messages <code> lacks, as JSON {key: English}, to translate
    python3 dev/catalog.py add <code> <file.json>   put the translated {key: message} pairs in <file.json> into the catalog
    python3 dev/catalog.py status                   how much of English each language says
    python3 dev/catalog.py prune <code>             drop messages English no longer has, or whose arguments it changed

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


def arguments(message):
    """The arguments a message uses, {name: kind}, as app/src/lib/message-format.ts reads them: `{x}` is text,
    `{x, plural, ...}` and `{x, select, ...}` name their kind; the text of a case is not an argument."""
    found, i = {}, 0

    def body(i):  # text and arguments up to the "}" closing a case (or the end); returns the index after it
        while i < len(message):
            c = message[i]
            if c == "}":
                return i + 1
            if c == "{":
                i = argument(i + 1)
            else:
                i += 1
        return i

    def argument(i):  # just after "{": name, then "}" or ", kind, cases }"
        j = i
        while j < len(message) and message[j] not in ",}":
            j += 1
        name = message[i:j].strip()
        if j >= len(message) or message[j] == "}":
            found.setdefault(name, "text")
            return j + 1
        k = message.index(",", j + 1) if "," in message[j + 1:] else len(message)
        kind = message[j + 1:k].strip()
        found[name] = kind
        i = k + 1
        while i < len(message):
            while i < len(message) and message[i].isspace():
                i += 1
            if i < len(message) and message[i] == "}":
                return i + 1
            while i < len(message) and message[i] != "{":  # the case's selector: one, other, =0, a select value
                i += 1
            i = body(i + 1)
        return i

    body(i)
    return found


def prune(code):
    english, own = load("en"), load(code)
    gone = sorted(k for k in own if k != "_meta" and k not in english)
    changed = sorted(k for k in own if k != "_meta" and k in english and arguments(own[k]) != arguments(english[k]))
    for k in gone + changed:
        del own[k]
    save(code, own)
    print(f"{code}: {len(gone)} messages English no longer has removed" + (": " + ", ".join(gone) if gone else ""))
    print(f"{code}: {len(changed)} messages whose arguments English changed removed, to translate again"
          + (": " + ", ".join(changed) if changed else ""))


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
    elif args[:1] == ["prune"] and len(args) == 2:
        prune(args[1])
    elif args == ["status"]:
        status()
    else:
        sys.exit(__doc__)
