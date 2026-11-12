#!/usr/bin/env python3
"""Verify resolved build dependencies for the product's independent adapter roles."""
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SOURCES = {"culprit-mssql", "culprit-postgres", "culprit-mysql", "culprit-yugabyte", "culprit-sqlite"}
STORES = {"culprit-store-adx", "culprit-store-clickhouse"}
AUTHENTICATION = {"culprit-auth-azure", "culprit-auth-aws"}
RECOVERY = {"culprit-blob-azure", "culprit-blob-s3"}
CLOUD = {"culprit-cloud"}
FLEET = STORES | RECOVERY | {"culprit-collector"}


def dependencies(package, features=None):
    selection = [] if features is None else ["--no-default-features", "--features", features]
    result = subprocess.run(
        ["cargo", "tree", "--locked", "--package", package, "--edges", "normal,build",
         "--prefix", "none", "--format", "{p}", *selection], cwd=ROOT, check=True, text=True, capture_output=True)
    return {line.split()[0] for line in result.stdout.splitlines() if line.strip()} - {package}


def main():
    rules = {package: FLEET for package in ["culprit-mcp", "culprit-app"]}
    rules.update({package: FLEET | CLOUD | AUTHENTICATION | {"culprit-engine"} for package in SOURCES})
    rules.update({package: SOURCES | CLOUD | AUTHENTICATION | {"culprit-engine"} for package in STORES | RECOVERY})
    rules.update({package: FLEET | CLOUD | SOURCES | {"culprit-engine"} for package in AUTHENTICATION})
    rules.update({package: FLEET | SOURCES | AUTHENTICATION | CLOUD | {"culprit-engine"}
                  for package in ["culprit-auth", "culprit-read", "culprit-core", "culprit-checks"]})
    rules["culprit-collector"] = STORES | RECOVERY | CLOUD | SOURCES | AUTHENTICATION | {"culprit-engine"}
    rules["culprit-cloud"] = FLEET | SOURCES | AUTHENTICATION | {"culprit-engine"}
    for package, forbidden in sorted(rules.items()):
        leaked = dependencies(package) & forbidden
        if leaked:
            raise RuntimeError(f"{package} includes forbidden build dependencies: {', '.join(sorted(leaked))}")
        print(f"{package}: dependency boundary verified")
    for features, forbidden in [
        ("store-clickhouse,recovery-s3", {"culprit-store-adx", "culprit-blob-azure"}),
        ("store-adx,recovery-azure", {"culprit-store-clickhouse", "culprit-blob-s3"}),
    ]:
        leaked = dependencies("culprit-engine", features) & forbidden
        if leaked:
            raise RuntimeError(f"engine [{features}] includes unrelated adapters: {sorted(leaked)}")
        print(f"engine [{features}]: independent adapter selection verified")


if __name__ == "__main__":
    main()
