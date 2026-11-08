#!/usr/bin/env python3
"""Three-way merge for a language catalog (locales/<code>.json), key by key.

Git calls it as the `catalog` merge driver (.gitattributes): `python3 dev/merge-catalog.py %O %A %B` with the common
ancestor, ours (rewritten in place with the result) and theirs. Keys added or changed on either side are kept; a key
both sides changed to different messages is a conflict, reported, and the merge fails so a person decides.
"""

import json
import sys


def load(path):
    try:
        with open(path, encoding="utf-8") as f:
            text = f.read()
        return json.loads(text) if text.strip() else {}
    except FileNotFoundError:
        return {}


def merge(base, ours, theirs, prefix=""):
    merged = dict(ours)
    conflicts = []
    for key in set(base) | set(ours) | set(theirs):
        b, o, t = base.get(key), ours.get(key), theirs.get(key)
        if o == t or t == b:
            continue  # Same on both sides, or theirs didn't change it: ours stands.
        if o == b:
            if t is None:
                merged.pop(key, None)  # Theirs removed it, ours didn't touch it.
            else:
                merged[key] = t  # Only theirs changed it.
            continue
        if isinstance(o, dict) and isinstance(t, dict) and (b is None or isinstance(b, dict)):
            merged[key], nested = merge(b or {}, o, t, f"{prefix}{key}.")
            conflicts.extend(nested)
        else:
            conflicts.append(f"{prefix}{key}")
    return merged, conflicts


def main(base_path, ours_path, theirs_path):
    merged, conflicts = merge(load(base_path), load(ours_path), load(theirs_path))
    with open(ours_path, "w", encoding="utf-8") as f:
        f.write(json.dumps(merged, indent=2, ensure_ascii=False, sort_keys=True) + "\n")
    if conflicts:
        sys.stderr.write("catalog keys changed differently on both sides: " + ", ".join(sorted(conflicts)) + "\n")
        return 1
    return 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:4]))
