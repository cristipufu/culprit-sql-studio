#!/usr/bin/env python3
"""What the tests exercise: line and function coverage of the backend's tests (LLVM source-based coverage, per crate)
and of the UI's (bun), with the UI files no test loads counted as uncovered, since bun leaves them out.

Needs the llvm-tools rustup component (`rustup component add llvm-tools`). The instrumented build lives in
target/coverage, so the regular build in target/debug stays as it is. Writes target/coverage/rust.lcov,
target/coverage/ui/lcov.info and target/coverage/summary.md, and prints the summary.

    python3 dev/coverage.py            # backend and UI
    python3 dev/coverage.py --ui-only  # just the UI (no Cargo)
"""
import collections
import glob
import json
import os
import re
import subprocess
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
OUT = os.path.join(ROOT, "target", "coverage")
# Test code, generated bindings and dependencies aren't what the tests are meant to cover.
IGNORE = r"(/\.cargo/registry/|/rustc/|/tests/|/tests\.rs$|_tests\.rs$|/examples/)"


def run(cmd, **kw):
    print("$", " ".join(cmd), file=sys.stderr)
    return subprocess.run(cmd, check=True, cwd=kw.pop("cwd", ROOT), **kw)


def rust():
    sysroot = subprocess.check_output(["rustc", "--print", "sysroot"], text=True).strip()
    host = re.search(r"^host: (.+)$", subprocess.check_output(["rustc", "-vV"], text=True), re.M).group(1)
    tools = os.path.join(sysroot, "lib", "rustlib", host, "bin")
    if not os.path.exists(os.path.join(tools, "llvm-cov")):
        sys.exit("llvm-cov not found: rustup component add llvm-tools")
    raw = os.path.join(OUT, "profraw")
    for f in glob.glob(os.path.join(raw, "*.profraw")):
        os.remove(f)
    os.makedirs(raw, exist_ok=True)
    env = dict(
        os.environ,
        CARGO_TARGET_DIR=OUT,
        RUSTFLAGS="-C instrument-coverage",
        LLVM_PROFILE_FILE=os.path.join(raw, "%p-%m.profraw"),
        CULPRIT_KEYCHAIN="off",
    )
    test = ["cargo", "test", "--locked", "--workspace", "--exclude", "culprit-app"]
    run(test, env=env)
    built = subprocess.run(test + ["--no-run", "--message-format=json"], cwd=ROOT, env=env, check=True,
                           capture_output=True, text=True).stdout
    objects = sorted({m["executable"] for m in map(json.loads, built.splitlines())
                      if m.get("reason") == "compiler-artifact" and m.get("executable") and m["profile"]["test"]})
    profdata = os.path.join(OUT, "tests.profdata")
    run([os.path.join(tools, "llvm-profdata"), "merge", "-sparse", *glob.glob(os.path.join(raw, "*.profraw")),
         "-o", profdata])
    # Merged: the raw profiles (one per test process) are only disk space now.
    for f in glob.glob(os.path.join(raw, "*.profraw")):
        os.remove(f)
    args = [f"--instr-profile={profdata}", f"--ignore-filename-regex={IGNORE}"]
    for o in objects[1:]:
        args += ["-object", o]
    llvm_cov = os.path.join(tools, "llvm-cov")
    with open(os.path.join(OUT, "rust.lcov"), "w") as f:
        run([llvm_cov, "export", "-format=lcov", *args, objects[0]], stdout=f)
    exported = json.loads(subprocess.check_output([llvm_cov, "export", "-summary-only", *args, objects[0]], cwd=ROOT))
    by = collections.defaultdict(lambda: [0, 0, 0, 0])
    for file in exported["data"][0]["files"]:
        name = os.path.relpath(file["filename"], ROOT)
        area = "/".join(name.split("/")[:2]) if name.startswith("crates/") else name.split("/")[0]
        s = file["summary"]
        by[area][0] += s["lines"]["covered"]
        by[area][1] += s["lines"]["count"]
        by[area][2] += s["functions"]["covered"]
        by[area][3] += s["functions"]["count"]
    return by


def code_lines(path):
    """Lines of code in a file no test loaded: what it would count if one did, near enough."""
    n, block = 0, False
    for line in open(path, encoding="utf-8"):
        s = line.strip()
        if block:
            block = "*/" not in s
            continue
        if not s or s.startswith("//"):
            continue
        if s.startswith("/*"):
            block = "*/" not in s
            continue
        n += 1
    return n


def ui():
    out = os.path.join(OUT, "ui")
    os.makedirs(out, exist_ok=True)
    run(["bun", "test", "src", "--coverage", "--coverage-reporter=lcov", f"--coverage-dir={out}"],
        cwd=os.path.join(ROOT, "app"), stdout=subprocess.DEVNULL)
    seen = {}
    for record in open(os.path.join(out, "lcov.info")).read().split("end_of_record"):
        source = re.search(r"^SF:(.+)$", record, re.M)
        if source:
            hit = lambda key: int(re.search(rf"^{key}:(\d+)$", record, re.M).group(1))
            seen[os.path.normpath(source.group(1))] = (hit("LH"), hit("LF"), hit("FNH"), hit("FNF"))
    by = collections.defaultdict(lambda: [0, 0, 0, 0, 0, 0])
    app = os.path.join(ROOT, "app")
    for path in glob.glob(os.path.join(app, "src", "**", "*.ts*"), recursive=True):
        rel = os.path.relpath(path, app)
        if "/gen/" in rel or ".test." in rel or rel.startswith("src/test/"):
            continue
        area = "app/" + ("/".join(rel.split("/")[:2]) if rel.count("/") > 1 else "src")
        lh, lf, fh, ff = seen.get(rel, (0, code_lines(path), 0, 0))
        b = by[area]
        b[0] += lh
        b[1] += lf
        b[2] += fh
        b[3] += ff
        b[4] += 1
        b[5] += rel not in seen
    return by


def pct(a, b):
    return f"{100 * a / b:.1f}%" if b else "—"


def main():
    ui_only = "--ui-only" in sys.argv
    lines = ["| Area | Lines | Functions | Files |", "|---|---|---|---|"]
    totals = {}
    if not ui_only:
        backend = rust()
        for area, (lc, lt, fc, ft) in sorted(backend.items()):
            lines.append(f"| {area} | {pct(lc, lt)} of {lt} | {pct(fc, ft)} of {ft} | |")
        totals["Backend"] = [sum(v[i] for v in backend.values()) for i in range(4)]
    front = ui()
    for area, (lc, lt, fc, ft, n, unloaded) in sorted(front.items()):
        functions = f"{pct(fc, ft)} in loaded files" if ft else "—"
        lines.append(f"| {area} | {pct(lc, lt)} of {lt} | {functions} | {n} ({unloaded} no test loads) |")
    totals["UI"] = [sum(v[i] for v in front.values()) for i in range(4)]
    for name, (lc, lt, fc, ft) in totals.items():
        functions = (f"{pct(fc, ft)} in loaded files" if name == "UI" else pct(fc, ft)) if ft else "—"
        lines.append(f"| **{name}** | **{pct(lc, lt)}** of {lt} | {functions} | |")
    summary = "\n".join(
        ["## Coverage", "", *lines, "",
         "UI files no test loads count as uncovered, by their lines of code; bun's own total leaves them out.", ""])
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, "summary.md"), "w") as f:
        f.write(summary)
    print(summary)


if __name__ == "__main__":
    main()
