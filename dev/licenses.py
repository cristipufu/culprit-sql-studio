#!/usr/bin/env python3
"""The third-party notices Culprit ships with: the Rust crates in the CLI, the MCP server and the desktop app on the
platforms Culprit is released for, and the packages in the desktop app's interface, each with its license text.

Writes target/licenses/THIRD-PARTY-LICENSES.txt. The release puts it in the desktop app, in each CLI archive and among
the release's files (for the bare CLI binaries `culprit update` downloads). CI writes it on every push, so a dependency
whose license can't be read fails there rather than on release day.

The licenses accepted are deny.toml's, so one file says what may ship: a crate or UI package under any other license
fails here as it fails `cargo deny check`. Needs cargo-about (`cargo install cargo-about --locked --features cli`; CI
installs its release binary) and the UI's packages (`cd app && bun install`).

    python3 dev/licenses.py
"""
import json
import os
import re
import subprocess
import sys
import tomllib

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
APP = os.path.join(ROOT, "app")
OUT = os.path.join(ROOT, "target", "licenses")
# The platforms the release builds for (.github/workflows/release.yml): a crate only another platform uses isn't shipped.
TARGETS = ["aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu"]
RULE = "-" * 100


def run(cmd):
    print("$", " ".join(cmd), file=sys.stderr)
    subprocess.run(cmd, check=True, cwd=ROOT)


def policy():
    """deny.toml's licenses: those any dependency may use, and those allowed for named crates only."""
    with open(os.path.join(ROOT, "deny.toml"), "rb") as f:
        licenses = tomllib.load(f)["licenses"]
    return licenses["allow"], {e["crate"]: e["allow"] for e in licenses.get("exceptions", [])}


def crates(allow, exceptions):
    """The license names by SPDX id, and (license name, crates using it, text) for each distinct license text among
    the shipped crates."""
    config = [
        f"accepted = {json.dumps(allow)}",
        f"targets = {json.dumps(TARGETS)}",
        "ignore-build-dependencies = true",
        "ignore-dev-dependencies = true",
        # The workspace's own crates (publish = false) are Culprit, under its own LICENSE.
        "private = { ignore = true }",
    ]
    for name, licenses in sorted(exceptions.items()):
        config += [f"[{name}]", f"accepted = {json.dumps(licenses)}"]
    os.makedirs(OUT, exist_ok=True)
    about = os.path.join(OUT, "about.toml")
    with open(about, "w") as f:
        f.write("\n".join(config) + "\n")
    found = os.path.join(OUT, "crates.json")
    # Every platform's crate sources, so the read below needs no network.
    run(["cargo", "fetch", "--locked"])
    run(["cargo", "about", "generate", "--workspace", "--locked", "--offline", "-c", about, "--format", "json",
         "-o", found])
    with open(found) as f:
        licenses = json.load(f)["licenses"]
    names = {lic["id"]: lic["name"] for lic in licenses}
    return names, [(lic["name"], sorted({f"{u['crate']['name']} {u['crate']['version']}" for u in lic["used_by"]}),
                    lic["text"]) for lic in licenses]


def allowed(expression, allow):
    """The license, first in `allow`'s order, under which an SPDX expression of ORs of ANDs may be used, or None. An
    alternative this can't read (nested parentheses, a WITH clause `allow` doesn't list whole) counts as not allowed."""
    alternatives = [[p.strip() for p in re.split(r"\s+AND\s+", alternative)]
                    for alternative in re.split(r"\s+OR\s+", expression.strip().removeprefix("(").removesuffix(")"))]
    usable = [parts[0] for parts in alternatives if all(p in allow for p in parts)]
    return min(usable, key=allow.index, default=None)


def license_file(package, chosen):
    """The package's license text: the file named for the license chosen when it offers a choice (LICENSE-MIT),
    otherwise its LICENSE (or LICENCE, COPYING) file."""
    files = sorted(f for f in os.listdir(package)
                   if re.match(r"(?i)(licen[cs]e|copying)", f) and not f.lower().endswith(".spdx")
                   and os.path.isfile(os.path.join(package, f)))
    named = [f for f in files if chosen.lower() in f.lower()]
    pick = (named or files or [None])[0]
    if pick is None:
        return None
    with open(os.path.join(package, pick), encoding="utf-8") as f:
        return f.read()


def ui_packages(allow, names):
    """(license, packages using it, text) for each distinct license text among the packages the UI bundles: the app's
    dependencies and theirs, as Node resolves them, peer dependencies included."""
    def resolve(name, start):
        here = start
        while True:
            path = os.path.join(here, "node_modules", name)
            if os.path.isdir(path):
                return path
            if os.path.samefile(here, APP):
                return None
            here = os.path.dirname(here)

    with open(os.path.join(APP, "package.json")) as f:
        pending = [(name, APP) for name in json.load(f).get("dependencies", {})]
    seen, problems = {}, []
    while pending:
        name, start = pending.pop()
        path = resolve(name, start)
        if path is None:
            problems.append(f"{name} isn't installed: cd app && bun install")
            continue
        path = os.path.realpath(path)
        if path in seen:
            continue
        with open(os.path.join(path, "package.json")) as f:
            manifest = json.load(f)
        seen[path] = manifest
        optional = manifest.get("peerDependenciesMeta", {})
        pending += [(dep, path) for dep in manifest.get("dependencies", {})]
        pending += [(dep, path) for dep in manifest.get("peerDependencies", {}) if not optional.get(dep, {}).get("optional")]

    groups = {}
    for path, manifest in seen.items():
        package = f"{manifest['name']} {manifest['version']}"
        expression = manifest.get("license") or ""
        chosen = allowed(expression, allow) if expression else None
        if chosen is None:
            problems.append(f"{package}: license {expression or '(none)'!r} isn't allowed by deny.toml")
            continue
        text = license_file(path, chosen)
        if text is None:
            problems.append(f"{package}: no license file to ship")
            continue
        groups.setdefault((chosen, text), []).append(package)
    if problems:
        sys.exit("\n".join(problems))
    return [(names.get(spdx, spdx), sorted(packages), text) for (spdx, text), packages in groups.items()]


def used_by(users):
    """"Used by: a 1.0, b 2.0, ..." wrapped to the rule's width, never inside a "name version" entry."""
    lines, line = [], "Used by:"
    for i, user in enumerate(users):
        entry = user + ("," if i < len(users) - 1 else "")
        if len(line) + 1 + len(entry) > len(RULE):
            lines.append(line)
            line = entry
        else:
            line += " " + entry
    return [*lines, line]


def section(title, groups):
    lines = [title.upper(), "=" * len(title), ""]
    for name, users, text in sorted(groups, key=lambda g: (g[0], g[1][0])):
        body = "\n".join(line.rstrip() for line in text.replace("\r\n", "\n").strip("\n").splitlines())
        lines += [RULE, name, *used_by(users), "", body, ""]
    return lines


def main():
    allow, exceptions = policy()
    names, rust = crates(allow, exceptions)
    ui = ui_packages(allow, names)
    n_crates = len({c for _, users, _ in rust for c in users})
    n_packages = sum(len(users) for _, users, _ in ui)
    lines = [
        "Third-party software in Culprit",
        "",
        "Culprit is MIT licensed (see LICENSE). The desktop app, the CLI and the MCP server include the open-source",
        "software below, used under the licenses that follow.",
        "",
        f"- {n_crates} Rust crates, used to build Culprit for macOS, Windows and Linux",
        f"- {n_packages} packages in the desktop app's interface",
        "",
        *section("Rust crates", rust),
        *section("Interface packages", ui),
    ]
    path = os.path.join(OUT, "THIRD-PARTY-LICENSES.txt")
    with open(path, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines).rstrip("\n") + "\n")
    print(f"{os.path.relpath(path, ROOT)}: {n_crates} crates, {n_packages} interface packages")


if __name__ == "__main__":
    main()
