#!/usr/bin/env python3
"""Functional Kusto correctness on an owned x86 Linux emulator. No credentials, cloud access or benchmarks."""
import json
import platform
import subprocess
import time
import urllib.error
import urllib.request
from telemetry_fixture import command, test_binary, run_test

IMAGE = "mcr.microsoft.com/azuredataexplorer/kustainer-linux@sha256:983a27314b6dfa91a950bdf8fb4c6c2b8ce0d61fecf7decbcf370b3fdbfaa3a7"
TEST = "emulator::functional_contract_and_canonical_sets"
OWNER_LABEL = "dev.culprit.kusto"


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
    binary = test_binary("culprit-store-adx")
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
        run_test(binary, TEST, {"CULPRIT_TEST_KUSTO_PORT": str(port)})
    except Exception:
        command(["docker", "logs", container], timeout=10)
        raise
    finally:
        command(["docker", "rm", "--force", container], timeout=30, stdout=subprocess.DEVNULL)


if __name__ == "__main__":
    main()
