#!/usr/bin/env python3
"""Compile Unicode CLDR cardinal rules into shared data for the Rust and browser message engines.

Usage: python3 dev/update-plurals.py <plurals.json>
Source: https://github.com/unicode-org/cldr-json/tree/49.0.0/cldr-json/cldr-core/supplemental
The checked-in result makes adding a language independent of network access and browser ICU versions.
"""

import json
import re
import sys
from pathlib import Path

CATEGORIES = ("zero", "one", "two", "few", "many")


def relation(text):
    match = re.fullmatch(r"([nivwftce])(?: % (\d+))? (!?=) ([\d.,]+)", text)
    if not match:
        raise ValueError(f"Unknown CLDR relation: {text}")
    operand, modulus, operator, values = match.groups()
    ranges = []
    for value in values.split(","):
        ends = [int(part) for part in value.split("..")]
        ranges.append([ends[0], ends[-1]])
    return [operand, int(modulus or 0), operator == "!=", ranges]


def rules(source):
    compiled, languages = [], {}
    for language, categories in sorted(source.items()):
        cases = []
        for category in CATEGORIES:
            text = categories.get(f"pluralRule-count-{category}", "").split("@")[0].strip()
            if text:
                cases.append([category, [[relation(part) for part in conjunction.split(" and ")]
                                         for conjunction in text.split(" or ")]])
        if cases not in compiled:
            compiled.append(cases)
        languages[language] = compiled.index(cases)
    return {"languages": languages, "rules": compiled}


def samples(source):
    """Expected categories come from CLDR's independent samples, not from evaluating our compiled rules.
    Numeric arguments cannot retain visible trailing zeroes or compact-number exponents; omit those samples."""
    result = []
    for language, categories in sorted(source.items()):
        for category, rule in categories.items():
            numbers = set()
            for section in re.findall(r"@(?:integer|decimal)\s+([^@]+)", rule):
                for span in section.strip().split(","):
                    for text in span.strip().split("~"):
                        if not re.fullmatch(r"\d+(?:\.\d+)?", text) or ("." in text and text.endswith("0")):
                            continue
                        numbers.add(float(text))
            result.extend([language, number, category.removeprefix("pluralRule-count-")] for number in sorted(numbers))
    return result


def main(path):
    source = json.loads(Path(path).read_text())["supplemental"]
    result = {"cldrVersion": source["version"]["_cldrVersion"],
              "source": "https://github.com/unicode-org/cldr-json/tree/49.0.0",
              **rules(source["plurals-type-cardinal"])}
    destination = Path(__file__).resolve().parent.parent / "locales/.cldr/cardinals.json"
    destination.parent.mkdir(exist_ok=True)
    destination.write_text(json.dumps(result, separators=(",", ":"), ensure_ascii=False) + "\n")
    cases = samples(source["plurals-type-cardinal"])
    tests = destination.parent.parent / ".tests/plurals.json"
    tests.write_text(json.dumps(cases, separators=(",", ":")) + "\n")
    print(f"CLDR {result['cldrVersion']}: {len(result['languages'])} languages, {len(result['rules'])} shared rule sets")
    print(f"{len(cases)} independent CLDR sample cases")


if __name__ == "__main__":
    main(sys.argv[1])
