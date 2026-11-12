"""Shared compilation and process checks for owned telemetry correctness fixtures."""
import json
import os
import subprocess
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent


def command(arguments, *, timeout=120, **kwargs):
    result = subprocess.run(arguments, cwd=ROOT, check=False, timeout=timeout, **kwargs)
    if result.returncode and result.stdout:
        print(result.stdout, end="" if isinstance(result.stdout, str) else None)
    result.check_returncode()
    return result


def test_binary(package):
    build = command(["cargo", "test", "--locked", "-p", package, "--lib", "--no-run", "--message-format=json"],
                    timeout=1200, text=True, stdout=subprocess.PIPE)
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith("{")]
    target = package.replace("-", "_")
    binaries = [item["executable"] for item in artifacts if item.get("reason") == "compiler-artifact"
                and item.get("target", {}).get("name") == target and item.get("executable")]
    if len(binaries) != 1:
        raise RuntimeError(f"Expected one compiled {package} test binary")
    return binaries[0]


def run_test(binary, test, environment):
    result = command([binary, "--ignored", "--exact", test, "--nocapture"], timeout=600, text=True,
                     env={**os.environ, **environment}, stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
    print(result.stdout, end="")
    if "1 passed; 0 failed" not in result.stdout:
        raise RuntimeError(f"The intended native correctness test did not execute: {test}")
