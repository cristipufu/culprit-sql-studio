#!/usr/bin/env python3
"""Native analytical correctness in an owned loopback ClickHouse fixture. No cloud or production access."""
from contextlib import contextmanager
import subprocess
import time
import urllib.error
import urllib.request
from telemetry_fixture import command, test_binary, run_test

IMAGE = "clickhouse/clickhouse-server@sha256:d426d2f612e1e189c90f9a3f8dcd85a03c6851273833817a296bb327e63aa7a7"
TEST = "functional::functional_contract_and_canonical_sets"


def wait_for_engine(port):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    deadline = time.monotonic() + 120
    while True:
        try:
            request = urllib.request.Request(f"http://127.0.0.1:{port}/", method="POST", data=b"SELECT version()",
                                            headers={"X-ClickHouse-User": "culprit_fixture",
                                                     "X-ClickHouse-Key": "owned-fixture-password"})
            with opener.open(request, timeout=3) as response:
                if response.status == 200 and response.read(256).strip() == b"26.8.36.3":
                    return
        except (urllib.error.URLError, TimeoutError, ConnectionError):
            pass
        if time.monotonic() >= deadline:
            raise RuntimeError("The owned ClickHouse fixture did not become ready")
        time.sleep(0.5)


@contextmanager
def fixture():
    container = command(["docker", "run", "--detach", "--rm", "--label", "dev.culprit.clickhouse",
                         "--memory", "2g", "--cpus", "2", "--publish", "127.0.0.1::8123",
                         "--env", "CLICKHOUSE_DB=culprit_fixture", "--env", "CLICKHOUSE_USER=culprit_fixture",
                         "--env", "CLICKHOUSE_PASSWORD=owned-fixture-password",
                         "--env", "CLICKHOUSE_DEFAULT_ACCESS_MANAGEMENT=1", IMAGE],
                        timeout=600, text=True, stdout=subprocess.PIPE).stdout.strip()
    try:
        mapping = command(["docker", "port", container, "8123/tcp"], text=True, stdout=subprocess.PIPE).stdout.strip()
        if not mapping.startswith("127.0.0.1:"):
            raise RuntimeError("The owned ClickHouse fixture must bind loopback only")
        port = int(mapping.rsplit(":", 1)[1])
        wait_for_engine(port)
        yield {"CULPRIT_TEST_CLICKHOUSE_PORT": str(port)}
    except Exception:
        command(["docker", "logs", container], timeout=10)
        raise
    finally:
        command(["docker", "rm", "--force", "--volumes", container], timeout=30, stdout=subprocess.DEVNULL)


def main():
    binary = test_binary("culprit-store-clickhouse")
    with fixture() as environment:
        run_test(binary, TEST, environment)


if __name__ == "__main__":
    main()
