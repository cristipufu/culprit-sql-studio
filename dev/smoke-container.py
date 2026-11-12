#!/usr/bin/env python3
"""Package a prebuilt Linux CLI and validate its non-root, offline collector entry points. No cloud calls."""
import argparse
import json
from pathlib import Path
import secrets
import shutil
import struct
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parent.parent
OWNER_LABEL = "dev.culprit.image-smoke"


def platform(binary):
    with binary.open("rb") as stream:
        header = stream.read(64)
    if len(header) < 64 or header[:6] != b"\x7fELF\x02\x01":
        raise ValueError("--binary must be a 64-bit little-endian Linux ELF executable")
    architecture = struct.unpack_from("<H", header, 18)[0]
    platforms = {62: "linux/amd64", 183: "linux/arm64"}
    if architecture not in platforms:
        raise ValueError("The collector image supports Linux amd64 and arm64")
    return platforms[architecture]


def fixture(directory):
    directory.mkdir(mode=0o755)
    workspace = directory / "workspace"
    workspace.mkdir(mode=0o755)
    (workspace / "connections.toml").write_text(
        '[connections.fixture]\nengine="mssql"\nenvironment="development"\n'
        'host="never-contact.invalid"\ndatabase="fixture"\nauth="entra"\n'
    )
    budget = {"page_rows": 250, "max_snapshot_rows": 500, "page_bytes": 262144,
              "total_bytes": 67108864, "max_sql_bytes": 65536, "max_plan_bytes": 131072,
              "max_pages": 256, "max_queries": 256, "statement_timeout_ms": 3000,
              "total_timeout_ms": 15000}
    config = {
        "pipeline": "container-smoke", "shard": "one", "workspace": "/fixture/workspace",
        "state": "/fixture/state", "health_listen": "127.0.0.1:9098",
        "inventory": {"targets": [{"connection": "fixture", "shard": "one", "admission_group": "one",
                                  "schedules": [{"id": "activity", "every_seconds": 60, "jitter_seconds": 0,
                                                 "collection": {"kind": "activity"}, "budget": budget}]}]},
        "credentials": {"fixture": {"kind": "azure", "identity": {"kind": "managed-identity", "client_id": None}}},
        "source_credential": "fixture",
        "recovery": {"kind": "azure-blob", "storage": {"account": "nevercontact", "container": "native",
                                                      "prefix": "one"}, "credential": "fixture"},
        "target": {"kind": "adx", "query": {"endpoint": "https://never-contact.westeurope.kusto.windows.net",
                                            "database": "fixture"},
                   "ready": {"account": "nevercontact", "container": "ready", "prefix": "one"},
                   "credential": "fixture", "ready_credential": "fixture"},
        "limits": {"max_bytes": 1073741824, "max_captures": 256},
    }
    (directory / "collector.json").write_text(json.dumps(config))
    for path in [workspace / "connections.toml", directory / "collector.json"]:
        path.chmod(0o644)
    return directory


def owned_container(name, owner):
    result = subprocess.run(["docker", "inspect", "--format", '{{json .Config.Labels}}', name],
                            capture_output=True, text=True, timeout=15)
    return result.returncode == 0 and json.loads(result.stdout).get(OWNER_LABEL) == owner


def run(binary, licenses, keep_image):
    selected_platform = platform(binary)
    if not licenses.is_file() or licenses.stat().st_size == 0:
        raise ValueError("Generate the release notices with python3 dev/licenses.py first")
    owner = secrets.token_hex(12)
    image = f"culprit-collector-smoke:{owner}"
    containers = []
    built = False
    with tempfile.TemporaryDirectory(prefix="culprit-image-") as temporary:
        root = Path(temporary)
        context = root / "context"
        context.mkdir()
        for source, name in [(binary, "culprit"), (licenses, "THIRD-PARTY-LICENSES.txt"),
                             (ROOT / "LICENSE", "LICENSE"), (ROOT / "deploy/collector/Dockerfile", "Dockerfile")]:
            shutil.copyfile(source, context / name)
        # Bind-mounted data is fabricated, non-secret and readable by the image's UID, never a real workspace.
        root.chmod(0o755)
        mounted = fixture(root / "fixture")
        try:
            subprocess.run(["docker", "build", "--platform", selected_platform, "--label", f"{OWNER_LABEL}={owner}",
                            "--tag", image, str(context)], check=True, timeout=600)
            built = True
            commands = [["--version"], ["collect", "schema", "--format", "json"],
                        ["collect", "service", "validate", "--config", "/fixture/collector.json"]]
            for index, command in enumerate(commands):
                name = f"culprit-image-{owner}-{index}"
                containers.append(name)
                result = subprocess.run([
                    "docker", "run", "--rm", "--name", name, "--label", f"{OWNER_LABEL}={owner}",
                    "--platform", selected_platform, "--network", "none", "--read-only", "--cap-drop", "ALL",
                    "--security-opt", "no-new-privileges", "--pids-limit", "64", "--memory", "512m", "--cpus", "1",
                    "--mount", f"type=bind,source={mounted},target=/fixture,readonly", image, *command,
                ], capture_output=True, text=True, check=False, timeout=60)
                if result.returncode:
                    # Only synthetic offline entry points run here. Preserve loader/runtime diagnostics
                    # before the owned container and image are removed.
                    print(result.stdout, end="")
                    print(result.stderr, end="")
                    result.check_returncode()
                if command[1:2] == ["schema"]:
                    assets = json.loads(result.stdout)
                    if len(assets["routes"]) != 16:
                        raise ValueError("The container must expose all native analytical streams and publication")
                if (mounted / "state").exists():
                    raise ValueError("Offline validation created runtime state")
            print(f"Collector image passed non-root, read-only, network-disabled checks: {selected_platform}")
            if keep_image:
                print(f"Retained image: {image}")
        finally:
            for name in containers:
                if owned_container(name, owner):
                    subprocess.run(["docker", "rm", "--force", name], check=True, timeout=30)
            if built and not keep_image:
                subprocess.run(["docker", "image", "rm", image], check=True, timeout=30)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--licenses", type=Path, default=ROOT / "target/licenses/THIRD-PARTY-LICENSES.txt")
    parser.add_argument("--keep-image", action="store_true")
    args = parser.parse_args()
    run(args.binary.resolve(), args.licenses.resolve(), args.keep_image)


if __name__ == "__main__":
    main()
