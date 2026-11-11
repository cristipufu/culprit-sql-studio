#!/usr/bin/env python3
"""Functional Kusto correctness on an owned x86 Linux emulator. No credentials, cloud access or benchmarks."""
import json
import os
import platform
import subprocess
import time
import urllib.error
import urllib.request
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
IMAGE = "mcr.microsoft.com/azuredataexplorer/kustainer-linux@sha256:983a27314b6dfa91a950bdf8fb4c6c2b8ce0d61fecf7decbcf370b3fdbfaa3a7"
TEST = "adx::emulator::functional_contract_and_canonical_sets"
OWNER_LABEL = "dev.culprit.kusto"


def command(arguments, *, timeout=120, **kwargs):
    result = subprocess.run(arguments, cwd=ROOT, check=False, timeout=timeout, **kwargs)
    if result.returncode and result.stdout:
        print(result.stdout, end="" if isinstance(result.stdout, str) else None)
    result.check_returncode()
    return result


def test_binary():
    result = command(["cargo", "test", "--locked", "-p", "culprit-engine", "--lib", "adx::",
                      "--no-run", "--message-format=json"], timeout=1200, text=True, stdout=subprocess.PIPE)
    artifacts = [json.loads(line) for line in result.stdout.splitlines() if line.startswith("{")]
    binaries = [item["executable"] for item in artifacts if item.get("reason") == "compiler-artifact"
                and item.get("target", {}).get("name") == "culprit_engine" and item.get("executable")]
    if len(binaries) != 1:
        raise RuntimeError("Expected one compiled engine test binary")
    return binaries[0]


def wait_for_engine(port):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    deadline = time.monotonic() + 180
    while True:
        try:
            request = urllib.request.Request(f"http://127.0.0.1:{port}/v1/rest/mgmt", method="POST",
                                            data=b'{"csl":".show cluster"}',
                                            headers={"Content-Type": "application/json"})
            with opener.open(request, timeout=3) as response:
                reply = json.load(response)
                if response.status == 200 and reply.get("Tables") and not reply.get("error"):
                    return
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            pass
        if time.monotonic() >= deadline:
            raise RuntimeError("The owned emulator did not become ready")
        time.sleep(0.5)


def main():
    # Refuse before compilation, image downloads or container creation. ARM emulation cannot establish this gate.
    if platform.system() != "Linux" or platform.machine().lower() not in ("x86_64", "amd64"):
        raise RuntimeError("Kusto correctness requires an x86 Linux runner; ARM is not supported")
    binary = test_binary()
    container = command(["docker", "run", "--detach", "--rm", "--platform", "linux/amd64",
                         "--label", OWNER_LABEL, "--memory", "4g", "--cpus", "2",
                         "--publish", "127.0.0.1::8080", "--env", "ACCEPT_EULA=Y", IMAGE],
                        timeout=600, text=True, stdout=subprocess.PIPE).stdout.strip()
    try:
        mapping = command(["docker", "port", container, "8080/tcp"], text=True, stdout=subprocess.PIPE).stdout.strip()
        if not mapping.startswith("127.0.0.1:"):
            raise RuntimeError("The owned emulator must bind loopback only")
        port = int(mapping.rsplit(":", 1)[1])
        wait_for_engine(port)
        result = command([binary, "--ignored", "--exact", TEST, "--nocapture"], timeout=600, text=True,
                         env={**os.environ, "CULPRIT_TEST_KUSTO_PORT": str(port)},
                         stdout=subprocess.PIPE, stderr=subprocess.STDOUT)
        print(result.stdout, end="")
        if "1 passed; 0 failed" not in result.stdout:
            raise RuntimeError("The intended native Kusto correctness test did not execute")
    except Exception:
        command(["docker", "logs", container], timeout=10)
        raise
    finally:
        command(["docker", "rm", "--force", container], timeout=30, stdout=subprocess.DEVNULL)


if __name__ == "__main__":
    main()
