#!/usr/bin/env python3
"""Run the shared recovery contract against an owned S3-compatible service; no AWS account or credentials."""
from contextlib import contextmanager
import importlib.util
from pathlib import Path
import subprocess
import time
import urllib.error
import urllib.request
from telemetry_fixture import command, test_binary, run_test

IMAGE = "rustfs/rustfs@sha256:1cfa82fb394a7c6a4ffc24f5333b52afee608d258503f94b37fcce5cc965c896"
OWNER_LABEL = "dev.culprit.s3-recovery"
TEST = "tests::s3_conditional_checkpoint_and_streamed_payload_roundtrip"


@contextmanager
def fixture():
    container = command([
        "docker", "run", "--detach", "--rm", "--label", OWNER_LABEL, "--memory", "1g", "--cpus", "2",
        "--publish", "127.0.0.1::9000", "--tmpfs", "/data:rw,uid=10001,gid=10001,size=268435456",
        "--env", "RUSTFS_ACCESS_KEY=fixture-access-key", "--env", "RUSTFS_SECRET_KEY=fixture-secret-key",
        "--env", "RUSTFS_CONSOLE_ENABLE=false", "--env", "RUSTFS_UNSAFE_BYPASS_DISK_CHECK=true",
        IMAGE, "/data",
    ], timeout=300, text=True, stdout=subprocess.PIPE).stdout.strip()
    try:
        mapping = command(["docker", "port", container, "9000/tcp"], text=True, stdout=subprocess.PIPE).stdout.strip()
        if not mapping.startswith("127.0.0.1:"):
            raise RuntimeError("S3 fixture must bind loopback only")
        port = int(mapping.rsplit(":", 1)[1])
        opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
        deadline = time.monotonic() + 60
        while True:
            try:
                with opener.open(f"http://127.0.0.1:{port}/health", timeout=3) as response:
                    if response.status == 200:
                        break
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                pass
            if time.monotonic() >= deadline:
                raise RuntimeError("The owned S3 fixture did not become ready")
            time.sleep(0.2)
        yield {"CULPRIT_TEST_S3_PORT": str(port)}
    except Exception:
        command(["docker", "logs", container], timeout=10)
        raise
    finally:
        command(["docker", "rm", "--force", "--volumes", container], timeout=30, stdout=subprocess.DEVNULL)


def main():
    binary = test_binary("culprit-blob-s3")
    specification = importlib.util.spec_from_file_location("clickhouse_fixture", Path(__file__).with_name("smoke-clickhouse.py"))
    clickhouse = importlib.util.module_from_spec(specification)
    specification.loader.exec_module(clickhouse)
    with fixture() as environment:
        run_test(binary, TEST, environment)
        with clickhouse.fixture() as warehouse:
            run_test(binary, "tests::s3_recovery_clickhouse_delivery_and_restart", {**environment, **warehouse})


if __name__ == "__main__":
    main()
