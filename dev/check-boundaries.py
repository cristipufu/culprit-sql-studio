#!/usr/bin/env python3
"""Verify resolved build dependencies for the product's independent adapter roles."""
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCES = {"culprit-mssql", "culprit-postgres", "culprit-mysql", "culprit-yugabyte", "culprit-sqlite"}
STORES = {"culprit-store-adx", "culprit-store-clickhouse"}
AUTHENTICATION = {"culprit-auth-azure", "culprit-auth-aws"}
FLEET = STORES | {"culprit-collector", "culprit-blob-azure"}


def dependencies(package):
    result = subprocess.run(
        ["cargo", "tree", "--locked", "--package", package, "--edges", "normal,build",
         "--prefix", "none", "--format", "{p}"], cwd=ROOT, check=True, text=True, capture_output=True)
    return {line.split()[0] for line in result.stdout.splitlines() if line.strip()} - {package}


def main():
    rules = {package: FLEET for package in ["culprit-mcp", "culprit-app"]}
    rules.update({package: FLEET | AUTHENTICATION | {"culprit-engine"} for package in SOURCES})
    rules.update({package: SOURCES | AUTHENTICATION | {"culprit-engine"} for package in STORES | {"culprit-blob-azure"}})
    rules.update({package: FLEET | SOURCES | {"culprit-engine"} for package in AUTHENTICATION})
    rules.update({package: FLEET | SOURCES | AUTHENTICATION | {"culprit-engine"}
                  for package in ["culprit-auth", "culprit-read", "culprit-core", "culprit-checks"]})
    rules["culprit-collector"] = STORES | SOURCES | AUTHENTICATION | {"culprit-engine"}
    for package, forbidden in sorted(rules.items()):
        leaked = dependencies(package) & forbidden
        if leaked:
            raise RuntimeError(f"{package} includes forbidden build dependencies: {', '.join(sorted(leaked))}")
        print(f"{package}: dependency boundary verified")


if __name__ == "__main__":
    main()
