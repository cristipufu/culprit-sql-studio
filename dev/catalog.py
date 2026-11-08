#!/usr/bin/env python3
"""Work on a language catalog (locales/<code>.json) without editing JSON by hand.

    python3 dev/catalog.py todo <code> [prefix]     the messages <code> lacks, as JSON {key: English}, to translate
    python3 dev/catalog.py add <code> <file.json>   put the translated {key: message} pairs in <file.json> into the catalog
    python3 dev/catalog.py status                   how much of English each language says
    python3 dev/catalog.py prune <code>             drop messages English no longer has, or whose arguments it changed
    python3 dev/catalog.py stale [<code>] [--check]   report changed English and translations without a source fingerprint
    python3 dev/catalog.py review <code>            record that this catalog was reviewed against today's English
    python3 dev/catalog.py track <code>             recover source fingerprints from translation history, once

`add` refuses a key English doesn't have, so a typo can't slip in; `app/src/lib/catalogs.test.ts` and the Rust catalog
test then check that every message reads and keeps English's arguments. See docs/localization.md.
"""

import json
import hashlib
import subprocess
import sys
from pathlib import Path

LOCALES = Path(__file__).resolve().parent.parent / "locales"


def load(code):
    return json.loads((LOCALES / f"{code}.json").read_text(encoding="utf-8"))


def save(code, catalog):
    text = json.dumps(catalog, indent=2, ensure_ascii=False, sort_keys=True) + "\n"
    (LOCALES / f"{code}.json").write_text(text, encoding="utf-8")


def fingerprint(message):
    return hashlib.sha256(message.encode("utf-8")).hexdigest()


def source_hashes(catalog):
    return catalog["_meta"].setdefault("source_hashes", {})


def stale_messages(english, own):
    sources = own["_meta"].get("source_hashes", {})
    return {key: {"status": "untracked" if key not in sources else "stale", "english": english[key]}
            for key in own if key != "_meta" and key in english and sources.get(key) != fingerprint(english[key])}


def todo(code, prefix=""):
    english, own = load("en"), load(code)
    revisit = stale_messages(english, own)
    missing = {k: v for k, v in english.items() if k != "_meta" and (k not in own or k in revisit) and k.startswith(prefix)}
    missing["_meta"] = {"source_hashes": {key: fingerprint(value) for key, value in missing.items()}}
    print(json.dumps(missing, indent=2, ensure_ascii=False, sort_keys=True))


def add(code, path):
    english, own = load("en"), load(code)
    found = json.loads(Path(path).read_text(encoding="utf-8"))
    if not isinstance(found, dict):
        sys.exit("translation batch must be an object of messages")
    metadata = found.pop("_meta", None)
    unknown = sorted(k for k in found if k not in english or k == "_meta")
    if unknown:
        sys.exit("keys English doesn't have: " + ", ".join(unknown))
    if metadata is not None:
        submitted_sources = metadata.get("source_hashes", {}) if isinstance(metadata, dict) else {}
        if not isinstance(submitted_sources, dict):
            sys.exit("_meta.source_hashes must be an object")
        changed = sorted(key for key in found if submitted_sources.get(key) != fingerprint(english[key]))
        if changed:
            sys.exit("English changed since this batch was exported; review and export again: " + ", ".join(changed))
    bad = sorted(k for k, v in found.items() if not isinstance(v, str) or not v.strip())
    if bad:
        sys.exit("not a message: " + ", ".join(bad))
    mismatched = sorted(k for k, value in found.items() if arguments(value) != arguments(english[k]))
    if mismatched:
        sys.exit("arguments differ from English: " + ", ".join(mismatched))
    own.update(found)
    source_hashes(own).update({key: fingerprint(english[key]) for key in found})
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
    sources = source_hashes(own)
    for key in list(sources):
        if key not in own:
            del sources[key]
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
        revisit = stale_messages(english, own) if path.stem != "en" else {}
        print(f"{path.stem:8} {own['_meta']['english_name']:12} {said:5} of {total} ({100 * said // max(total, 1)}%)"
              + (f", {len(revisit)} need source review" if revisit else ""))


def stale(code=None, check=False):
    english = load("en")
    codes = [code] if code else [p.stem for p in sorted(LOCALES.glob("*.json")) if p.stem != "en"]
    found = {language: stale_messages(english, load(language)) for language in codes}
    found = {language: messages for language, messages in found.items() if messages}
    print(json.dumps(found, indent=2, ensure_ascii=False, sort_keys=True))
    return 1 if check and found else 0


def review(code):
    english, own = load("en"), load(code)
    source_hashes(own).update({key: fingerprint(english[key]) for key in own if key != "_meta" and key in english})
    save(code, own)


def track(code):
    """Recover the English present when each existing translation last changed. This preserves old sources rather
    than pretending an existing translation was reviewed against a later English rewording."""
    def git(*args):
        return subprocess.run(["git", "-C", str(LOCALES.parent), *args], check=True, capture_output=True, text=True).stdout

    commits = git("log", "--reverse", "--format=%H", "--", f"locales/{code}.json").splitlines()
    previous, recovered = {}, {}
    for commit in commits:
        catalog = json.loads(git("show", f"{commit}:locales/{code}.json"))
        changed = {key: value for key, value in catalog.items() if key != "_meta" and value != previous.get(key)}
        if changed:
            english = json.loads(git("show", f"{commit}:locales/en.json"))
            for key, value in changed.items():
                if key in english:
                    recovered[key] = (value, fingerprint(english[key]))
        previous = catalog
    own = load(code)
    sources = source_hashes(own)
    for key, (value, source) in recovered.items():
        if key not in sources and own.get(key) == value:
            sources[key] = source
    save(code, own)


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
    elif args[:1] == ["stale"] and len(args) <= 3:
        rest = [arg for arg in args[1:] if arg != "--check"]
        if len(rest) > 1:
            sys.exit(__doc__)
        sys.exit(stale(rest[0] if rest else None, "--check" in args))
    elif args[:1] == ["review"] and len(args) == 2:
        review(args[1])
    elif args[:1] == ["track"] and len(args) == 2:
        track(args[1])
    else:
        sys.exit(__doc__)
