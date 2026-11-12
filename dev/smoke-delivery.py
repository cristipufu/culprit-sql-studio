#!/usr/bin/env python3
"""Exercise recovery against an owned, loopback-only Azurite container; never uses Azure credentials."""
import base64
import hashlib
import hmac
import subprocess
import time
import urllib.error
import urllib.request
from datetime import datetime, timezone
from telemetry_fixture import command, test_binary, run_test

IMAGE = "mcr.microsoft.com/azure-storage/azurite@sha256:830430c1da1a2d537e08f3e6764dd1f5ae00cf0346bcaf625b968ec3f0971fd5"
OWNER_LABEL = "dev.culprit.delivery"
ACCOUNT = "culprittest"
KEY = bytes([42]) * 32  # Synthetic emulator account only.
TEST = "tests::azurite_conditional_checkpoint_and_streamed_payload_roundtrip"


def create_container(port):
    path = f"/{ACCOUNT}/recovery"
    date = datetime.now(timezone.utc).strftime("%a, %d %b %Y %H:%M:%S GMT")
    version = "2023-11-03"
    fields = ["PUT", "", "", "", "", "application/octet-stream", "", "", "", "", "", ""]
    canonical = ("\n".join(fields) + f"\nx-ms-date:{date}\nx-ms-version:{version}\n/{ACCOUNT}{path}\nrestype:container")
    signature = base64.b64encode(hmac.new(KEY, canonical.encode(), hashlib.sha256).digest()).decode()
    request = urllib.request.Request(f"http://127.0.0.1:{port}{path}?restype=container", data=b"", method="PUT",
                                    headers={"x-ms-date": date, "x-ms-version": version,
                                             "Content-Type": "application/octet-stream",
                                             "Authorization": f"SharedKey {ACCOUNT}:{signature}"})
    # Explicitly bypass host proxies; this fixture can address loopback only.
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(request, timeout=3) as response:
        if response.status != 201:
            raise RuntimeError("Azurite did not create the private fixture container")


def main():
    binary = test_binary("culprit-blob-azure")  # Compile before starting the disposable service.
    container = command(["docker", "run", "--detach", "--rm", "--label", OWNER_LABEL,
                         "--memory", "512m", "--cpus", "1", "--publish", "127.0.0.1::10000",
                         "--env", f"AZURITE_ACCOUNTS={ACCOUNT}:{base64.b64encode(KEY).decode()}",
                         IMAGE, "azurite-blob", "--blobHost", "0.0.0.0", "--silent", "--disableTelemetry"],
                        timeout=300, text=True, stdout=subprocess.PIPE).stdout.strip()
    try:
        mapping = command(["docker", "port", container, "10000/tcp"], text=True, stdout=subprocess.PIPE).stdout.strip()
        if not mapping.startswith("127.0.0.1:"):
            raise RuntimeError("Fixture must bind loopback only")
        port = int(mapping.rsplit(":", 1)[1])
        deadline = time.monotonic() + 45
        while True:
            try:
                create_container(port)
                break
            except urllib.error.HTTPError:
                raise
            except (urllib.error.URLError, TimeoutError, ConnectionError):
                if time.monotonic() >= deadline:
                    raise
                time.sleep(0.2)
        run_test(binary, TEST, {"CULPRIT_TEST_AZURITE_PORT": str(port)})
    except Exception:
        command(["docker", "logs", container], timeout=10)
        raise
    finally:
        command(["docker", "rm", "--force", container], timeout=30, stdout=subprocess.DEVNULL)


if __name__ == "__main__":
    main()
