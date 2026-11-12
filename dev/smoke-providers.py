#!/usr/bin/env python3
"""Run real provider checks in owned, loopback-only, disposable databases."""

import argparse
import json
import os
from pathlib import Path
import re
import secrets
import signal
import subprocess
import sys
import tempfile
import time
from dataclasses import dataclass


ROOT = Path(__file__).resolve().parent.parent
OWNER_LABEL = "dev.culprit.smoke"
STARTUP_BUDGET = 240
COMMAND_BUDGET = 120
CONNECT_BUDGET = 3
TEST_BUDGET = 1200
HEARTBEAT = 30


@dataclass(frozen=True)
class Fixture:
    name: str
    image: str
    engine: str
    port: int
    database: str
    user: str
    memory: str = "1g"
    platform: str | None = None
    options: tuple[str, ...] = ()


FIXTURES = (
    Fixture("postgres", "postgres:18.6", "postgres", 5432, "culprit_smoke", "postgres"),
    Fixture("mysql", "mysql:8.4", "mysql", 3306, "culprit_smoke", "root", "2g"),
    Fixture("mysql-5.7", "mysql:5.7", "mysql", 3306, "culprit_smoke", "root", "2g", "linux/amd64",
            ("--master-info-repository=TABLE", "--relay-log-info-repository=TABLE")),
    *(Fixture(f"mariadb-{version}", f"mariadb:{version}", "mysql", 3306, "culprit_smoke", "root")
      for version in ("10.6", "10.11", "11.8")),
    Fixture("mssql", "mcr.microsoft.com/mssql/server:2022-latest", "mssql", 1433,
            "culprit_smoke", "sa", "3g", "linux/amd64"),
    Fixture("timescale", "timescale/timescaledb@sha256:d971568f9af05e218866181151117f517282f05f299a0e4010dc6a97704f6f2c",
            "postgres", 5432, "culprit_timescale", "postgres"),
    Fixture("yugabyte", "yugabytedb/yugabyte@sha256:e604d9e273a1fc3fccaf6ecbfe6ac6e3b93d2b4f06a075c461e3d73676d183fc",
            "yugabyte", 5433, "culprit_yugabyte", "yugabyte", "6g"),
)


class Commands:
    def __init__(self, passwords):
        self.passwords = passwords

    def sanitized(self, text):
        for password in self.passwords:
            text = text.replace(password, "[redacted]")
        return text

    @staticmethod
    def stop(process):
        if process.poll() is not None:
            return
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            process.wait()
            return
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(process.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            process.wait()

    def run(self, args, *, input=None, env=None, timeout=COMMAND_BUDGET, check=True, echo=False):
        process = subprocess.Popen(args, cwd=ROOT, env={**os.environ, **(env or {})},
                                   stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                   stderr=subprocess.STDOUT, text=True, start_new_session=True)
        started = time.monotonic()
        pending_input = input
        try:
            while True:
                remaining = timeout - (time.monotonic() - started)
                if remaining <= 0:
                    raise TimeoutError(f"{args[0]} exceeded its {timeout}s budget")
                try:
                    output, _ = process.communicate(pending_input, timeout=min(HEARTBEAT, remaining))
                    break
                except subprocess.TimeoutExpired as progress:
                    pending_input = None
                    if time.monotonic() - started < timeout:
                        partial = progress.output or b""
                        if isinstance(partial, bytes):
                            partial = partial.decode(errors="replace")
                        tests = re.findall(r"^test ([A-Za-z0-9_:]+) \.\.\.", partial, re.MULTILINE)
                        current = f"; {tests[-1]}" if tests else ""
                        print(f"  {args[0]} is still running ({int(time.monotonic() - started)}s{current})", flush=True)
        finally:
            self.stop(process)
            for stream in (process.stdin, process.stdout):
                if stream is not None:
                    stream.close()
        output = self.sanitized(output)
        if echo or (check and process.returncode):
            print(output, end="" if output.endswith("\n") else "\n", flush=True)
        if check and process.returncode:
            raise RuntimeError(f"{args[0]} exited with status {process.returncode}")
        return process.returncode, output


def run_tests(commands, crate, selection, environment, *, library=True, skip=None):
    target = ["--lib"] if library else ["--test", "all"]
    print(f"  Testing {crate} {selection}", flush=True)
    filters = ["--skip", skip] if skip else []
    features = ["--features", "collector"] if crate == "culprit-engine" else []
    _, output = commands.run(["cargo", "test", "--locked", "-p", crate, *features, *target, selection,
                             "--", "--ignored", "--test-threads=1", *filters],
                            env=environment, timeout=TEST_BUDGET, echo=True)
    counts = re.findall(r"test result: ok\. (\d+) passed", output)
    if not counts or sum(map(int, counts)) == 0:
        raise RuntimeError(f"{crate} {selection} did not execute any live tests")


class Database:
    def __init__(self, fixture):
        self.fixture = fixture
        self.owner = secrets.token_hex(12)
        self.name = f"culprit-smoke-{fixture.name}-{self.owner}"
        self.password = f"Smoke!{secrets.token_hex(16)}Aa9"
        self.limited_password = f"Reader!{secrets.token_hex(16)}Aa9"
        self.commands = Commands([self.password, self.limited_password])
        self.port = None

    def __enter__(self):
        try:
            self.start()
            return self
        except BaseException:
            self.close()
            raise

    def __exit__(self, *exception):
        self.close()

    def published_ports(self):
        return (self.fixture.port,)

    def start(self):
        fixture = self.fixture
        print(f"Starting {fixture.name} in {self.name}", flush=True)
        found, _ = self.commands.run(["docker", "image", "inspect", fixture.image, "--format", "{{.Id}}"], check=False)
        if found:
            self.commands.run(["docker", "pull", fixture.image], timeout=600, echo=True)
        args = ["docker", "create", "--pull=never", "--name", self.name,
                "--hostname", self.name, "--label", f"{OWNER_LABEL}={self.owner}",
                "--cpus", "3" if fixture.engine == "yugabyte" else "2", "--memory", fixture.memory]
        for port in self.published_ports():
            args += ["-p", f"127.0.0.1::{port}"]
        if fixture.platform:
            args += ["--platform", fixture.platform]
        environment, command = self.configuration()
        for name in environment:
            args += ["-e", name]
        self.commands.run([*args, fixture.image, *command, *fixture.options], env=environment)
        self.commands.run(["docker", "start", self.name])
        _, port = self.commands.run(["docker", "port", self.name, str(fixture.port)])
        self.port = port.strip().removeprefix("127.0.0.1:")
        if not self.port.isdigit():
            raise RuntimeError("Docker did not publish exactly one loopback port")
        deadline = time.monotonic() + STARTUP_BUDGET
        while time.monotonic() < deadline:
            try:
                status, _ = self.sql("SELECT 1;", database=self.admin_database(), check=False, timeout=5)
            except TimeoutError:
                status = 1
            if status == 0:
                print(f"  {fixture.name} ready at 127.0.0.1:{self.port}", flush=True)
                return
            time.sleep(2)
        self.commands.run(["docker", "logs", "--tail", "60", self.name], check=False, echo=True)
        raise TimeoutError(f"{fixture.name} did not become ready")

    def configuration(self):
        fixture = self.fixture
        if fixture.engine == "postgres":
            preload = "timescaledb,pg_stat_statements" if fixture.name == "timescale" else "pg_stat_statements"
            return ({"POSTGRES_PASSWORD": self.password, "POSTGRES_DB": fixture.database},
                    ["-c", f"shared_preload_libraries={preload}", "-c", "track_io_timing=on"])
        if fixture.engine == "mysql":
            return ({"MYSQL_ROOT_PASSWORD": self.password, "MYSQL_ROOT_HOST": "%", "MYSQL_DATABASE": fixture.database},
                    ["--performance-schema=ON", "--server-id=1"])
        if fixture.engine == "mssql":
            return ({"ACCEPT_EULA": "Y", "MSSQL_PID": "Developer", "MSSQL_SA_PASSWORD": self.password}, [])
        return ({}, ["bin/yugabyted", "start", "--background=false", "--callhome=false", "--ui=false",
                     # Bound native backfill speed so progress tests observe work, even on fast CI hosts.
                     "--tserver_flags=memory_limit_hard_bytes=2147483648,backfill_index_rate_rows_per_sec=1000",
                     "--master_flags=memory_limit_hard_bytes=536870912"])

    def admin_database(self):
        return {"postgres": "postgres", "yugabyte": "yugabyte", "mysql": "mysql", "mssql": "master"}[self.fixture.engine]

    def sql(self, sql, *, database=None, check=True, timeout=COMMAND_BUDGET):
        fixture = self.fixture
        database = database or fixture.database
        env = {}
        args = ["docker", "exec", "-i"]
        if fixture.engine in ("postgres", "yugabyte"):
            binary = "psql" if fixture.engine == "postgres" else "bin/ysqlsh"
            if fixture.engine == "postgres":
                env = {"PGPASSWORD": self.password, "PGCONNECT_TIMEOUT": str(CONNECT_BUDGET)}
                args += ["-e", "PGPASSWORD", "-e", "PGCONNECT_TIMEOUT"]
            host = self.name if fixture.engine == "yugabyte" else "127.0.0.1"
            args += [self.name, binary, "-X", "-v", "ON_ERROR_STOP=1", "-U", fixture.user,
                     "-d", database, "-h", host]
        elif fixture.engine == "mysql":
            env = {"MYSQL_PWD": self.password}
            binary = "mariadb" if fixture.name.startswith("mariadb") else "mysql"
            args += ["-e", "MYSQL_PWD", self.name, binary, "--protocol=TCP", "-h", "127.0.0.1",
                     f"--connect-timeout={CONNECT_BUDGET}", "-u", fixture.user, database]
        else:
            env = {"SQLCMDPASSWORD": self.password}
            args += ["-e", "SQLCMDPASSWORD", self.name, "/opt/mssql-tools18/bin/sqlcmd",
                     "-C", "-S", "localhost", "-U", fixture.user, "-d", database, "-b", "-l", str(CONNECT_BUDGET)]
        return self.commands.run(args, input=sql, env=env, check=check, timeout=timeout)

    def seed(self):
        fixture = self.fixture
        if fixture.engine in ("yugabyte", "mssql"):
            self.sql(f"CREATE DATABASE {fixture.database};", database=self.admin_database())
        if fixture.name in ("timescale", "yugabyte"):
            self.sql((ROOT / "dev" / fixture.name / "seed.sql").read_text())
            self.sql((ROOT / "dev" / fixture.name / "workload.sql").read_text())
        elif fixture.engine == "postgres":
            self.sql("CREATE EXTENSION pg_stat_statements;")
            self.prepare_hypopg()
        if fixture.engine == "mssql":
            self.sql(f"ALTER DATABASE {fixture.database} SET QUERY_STORE=ON (OPERATION_MODE=READ_WRITE, QUERY_CAPTURE_MODE=ALL);")
        schema = "dbo" if fixture.engine == "mssql" else (fixture.database if fixture.engine == "mysql" else "public")
        table = f"{schema}.smoke_rows"
        if fixture.engine == "mysql":
            # Explicit setup on this owned fixture, never collector behavior. MariaDB leaves the current-statement
            # consumer off by default; provider tests separately verify that disabled instrumentation stays unknown.
            self.sql("UPDATE performance_schema.setup_consumers SET ENABLED='YES' WHERE NAME IN "
                     "('global_instrumentation','thread_instrumentation','statements_digest','events_statements_current');")
        values = ",".join(f"({i},{i * 3})" for i in range(1, 101))
        self.sql(f"CREATE TABLE {table} (id int PRIMARY KEY, value int NOT NULL); INSERT INTO {table} VALUES {values};")
        self.create_reader(table)
        self.sql("\n".join(f"SELECT sum(value) FROM {table} WHERE id > {i};" for i in range(20)))
        if fixture.engine == "mssql":
            self.sql("EXEC sys.sp_query_store_flush_db;")

    def prepare_hypopg(self):
        status, _ = self.commands.run(["docker", "exec", self.name, "test", "-f",
                                      "/usr/share/postgresql/18/extension/hypopg.control"], check=False)
        if status:
            # Only the owned fixture gets packages. Provider collection never installs extensions.
            self.commands.run(["docker", "exec", self.name, "apt-get", "update"], timeout=300)
            self.commands.run(["docker", "exec", self.name, "apt-get", "install", "-y", "--no-install-recommends",
                               "postgresql-18-hypopg"], timeout=300)
        self.sql("CREATE DATABASE culprit_hypopg;", database="postgres")

    def create_reader(self, table):
        engine = self.fixture.engine
        if engine in ("postgres", "yugabyte"):
            self.sql(f"CREATE ROLE smoke_reader LOGIN PASSWORD '{self.limited_password}'; "
                     f"GRANT USAGE ON SCHEMA public TO smoke_reader; GRANT SELECT ON {table} TO smoke_reader;")
        elif engine == "mysql":
            self.sql(f"CREATE USER 'smoke_reader'@'%' IDENTIFIED BY '{self.limited_password}'; "
                     f"GRANT SELECT ON {table} TO 'smoke_reader'@'%';")
        else:
            self.sql(f"CREATE LOGIN smoke_reader WITH PASSWORD='{self.limited_password}'; "
                     f"CREATE USER smoke_reader FOR LOGIN smoke_reader; GRANT SELECT ON {table} TO smoke_reader;")

    def test_environment(self):
        fixture = self.fixture
        return {"CULPRIT_KEYCHAIN": "off", "CULPRIT_LANG": "en",
                "CULPRIT_SMOKE_ENGINE": fixture.engine, "CULPRIT_SMOKE_PORT": self.port,
                "CULPRIT_SMOKE_DATABASE": fixture.database, "CULPRIT_SMOKE_USER": fixture.user,
                "CULPRIT_SMOKE_PASSWORD": self.password, "CULPRIT_SMOKE_LIMITED_USER": "smoke_reader",
                "CULPRIT_SMOKE_LIMITED_PASSWORD": self.limited_password,
                "CULPRIT_SMOKE_SCHEMA": "dbo" if fixture.engine == "mssql" else
                    (fixture.database if fixture.engine == "mysql" else "public"),
                "CULPRIT_PREVIEW_PG_PORT": self.port, "CULPRIT_PREVIEW_PG_DATABASE": fixture.database,
                "CULPRIT_PREVIEW_PG_PASSWORD": self.password, "CULPRIT_TIMESCALE_TEST_PORT": self.port,
                "CULPRIT_HYPOPG_PG_TEST_PORT": self.port, "CULPRIT_HYPOPG_PG_TEST_PASSWORD": self.password,
                "CULPRIT_TIMESCALE_TEST_PASSWORD": self.password,
                "CULPRIT_YSQL_TEST_PORT": self.port, "CULPRIT_MYSQL_TEST_PORT": self.port,
                "CULPRIT_MYSQL_TEST_PASSWORD": self.password,
                "CULPRIT_PREVIEW_MSSQL_PORT": self.port, "CULPRIT_PREVIEW_MSSQL_PASSWORD": self.password,
                "CULPRIT_MSSQL_SOURCE_TEST_PORT": self.port, "CULPRIT_MSSQL_SOURCE_TEST_PASSWORD": self.password}

    def test(self, crate, selection, *, library=True, skip=None):
        run_tests(self.commands, crate, selection, self.test_environment(), library=library, skip=skip)

    def verify(self, selected_tests=None):
        if selected_tests:
            for crate, selection in selected_tests:
                self.test(crate, selection, library=crate != "culprit-engine",
                          skip="cluster_tests::" if crate == "culprit-yugabyte" else None)
            return
        self.test("culprit-engine", "live_providers::live_registered_provider", library=False)
        fixture = self.fixture
        if fixture.engine == "postgres":
            self.test("culprit-engine", "live_providers::native_telemetry_", library=False)
            self.test("culprit-postgres", "read::native_tests::")
            self.test("culprit-postgres", "telemetry::native_tests::")
            self.test("culprit-postgres", "wire::tests::live_table_preview")
            if fixture.name == "timescale":
                self.test("culprit-postgres", "timescale::tests::live_")
            else:
                self.test("culprit-postgres", "wire::tests::live_postgres_hypothetical_")
        elif fixture.engine == "mysql":
            self.test("culprit-engine", "live_providers::native_telemetry_", library=False)
            self.test("culprit-mysql", "telemetry::native_tests::")
            self.test("culprit-mysql", "read::native_tests::")
            self.test("culprit-mysql", "preview::tests::live_")
        elif fixture.engine == "mssql":
            self.test("culprit-mssql", "statement::tests::live_")
            self.test("culprit-mssql", "native_tests::")
        else:
            self.test("culprit-engine", "live_providers::native_telemetry_", library=False)
            self.test("culprit-yugabyte", "telemetry::native_tests::")
            self.test("culprit-yugabyte", "tests::live_", skip="cluster_tests::")
            self.test("culprit-postgres", "wire::tests::live_ysql_")
            self.test("culprit-engine", "services::live_native_", library=False)

    def close(self):
        # A cancelled create can succeed before its CLI returns. Verify the exact ownership label even then.
        status, owner = self.commands.run(["docker", "inspect", "--format",
                                          '{{index .Config.Labels "' + OWNER_LABEL + '"}}', self.name], check=False)
        if status:
            return
        if owner.strip() != self.owner:
            raise RuntimeError("refusing to remove a container without this run's ownership label")
        self.commands.run(["docker", "unpause", self.name], check=False)
        self.commands.run(["docker", "rm", "--force", "--volumes", self.name])
        print(f"Removed owned {self.fixture.name} container and anonymous volumes", flush=True)


class SshDatabase(Database):
    def __init__(self):
        super().__init__(Fixture("ssh", "postgres:18.6", "postgres", 5432, "ssh_fixture", "postgres"))
        self.directory = tempfile.TemporaryDirectory(prefix="culprit-ssh-")
        self.files = Path(self.directory.name)
        self.database_password = "fixture-only-password"
        self.commands.passwords.append(self.database_password)

    def published_ports(self):
        return (self.fixture.port, 22)

    def certificates(self):
        def path(name):
            return str(self.files / name)

        self.commands.run(["openssl", "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-sha256",
                           "-keyout", path("ca.key"), "-out", path("ca.pem"), "-days", "1",
                           "-subj", "/CN=Culprit disposable fixture CA", "-addext", "basicConstraints=critical,CA:TRUE"])
        self.commands.run(["openssl", "req", "-new", "-newkey", "rsa:2048", "-nodes", "-sha256",
                           "-keyout", path("server.key"), "-out", path("server.csr"), "-subj", "/CN=localhost"])
        (self.files / "server.ext").write_text("subjectAltName=DNS:localhost\nextendedKeyUsage=serverAuth\nbasicConstraints=CA:FALSE\n")
        self.commands.run(["openssl", "x509", "-req", "-in", path("server.csr"), "-CA", path("ca.pem"),
                           "-CAkey", path("ca.key"), "-CAcreateserial", "-days", "1", "-sha256",
                           "-extfile", path("server.ext"), "-out", path("server.crt")])

    def install_file(self, destination, content, owner="root", mode="600"):
        self.commands.run(["docker", "exec", "-i", self.name, "install", "-m", mode, "-o", owner,
                           "-g", owner, "/dev/stdin", destination], input=content)

    def seed(self):
        self.certificates()
        self.commands.run(["docker", "exec", self.name, "mkdir", "-p", "/tmp/culprit-ssl"])
        self.install_file("/tmp/culprit-ssl/server.key", (self.files / "server.key").read_text(), owner="postgres")
        self.install_file("/tmp/culprit-ssl/server.crt", (self.files / "server.crt").read_text(), owner="postgres")
        self.sql("ALTER SYSTEM SET ssl_cert_file='/tmp/culprit-ssl/server.crt';\n"
                 "ALTER SYSTEM SET ssl_key_file='/tmp/culprit-ssl/server.key';\n"
                 "ALTER SYSTEM SET ssl=on; SELECT pg_reload_conf();\n"
                 f"CREATE ROLE ssh_fixture LOGIN PASSWORD '{self.database_password}';")
        self.start_bastion()

    def start_bastion(self):
        status, _ = self.commands.run(["docker", "exec", self.name, "test", "-x", "/usr/sbin/sshd"], check=False)
        if status:
            self.commands.run(["docker", "exec", self.name, "apt-get", "update"], timeout=300)
            self.commands.run(["docker", "exec", self.name, "apt-get", "install", "-y", "--no-install-recommends",
                               "openssh-server"], timeout=300)
        self.commands.run(["ssh-keygen", "-q", "-t", "ed25519", "-N", "", "-f", str(self.files / "client")])
        self.commands.run(["docker", "exec", self.name, "useradd", "--create-home", "--shell", "/bin/sh", "smoke_tunnel"])
        self.commands.run(["docker", "exec", "-i", self.name, "chpasswd"], input=f"smoke_tunnel:{self.password}\n")
        self.commands.run(["docker", "exec", self.name, "install", "-d", "-m", "700", "-o", "smoke_tunnel",
                           "-g", "smoke_tunnel", "/home/smoke_tunnel/.ssh"])
        self.install_file("/home/smoke_tunnel/.ssh/authorized_keys", (self.files / "client.pub").read_text(), owner="smoke_tunnel")
        self.commands.run(["docker", "exec", self.name, "ssh-keygen", "-A"])
        self.commands.run(["docker", "exec", self.name, "mkdir", "-p", "/run/sshd"])
        self.install_file("/tmp/culprit-sshd.conf", """Port 22
ListenAddress 0.0.0.0
HostKey /etc/ssh/ssh_host_ed25519_key
AuthorizedKeysFile .ssh/authorized_keys
PubkeyAuthentication yes
PasswordAuthentication no
KbdInteractiveAuthentication no
PermitRootLogin no
AllowUsers smoke_tunnel
AllowTcpForwarding local
PermitOpen localhost:5432 127.0.0.1:5432
UsePAM no
PidFile /run/culprit-sshd.pid
""")
        self.commands.run(["docker", "exec", self.name, "/usr/sbin/sshd", "-f", "/tmp/culprit-sshd.conf",
                           "-E", "/tmp/culprit-sshd.log"])
        _, endpoint = self.commands.run(["docker", "port", self.name, "22"])
        port = endpoint.strip().removeprefix("127.0.0.1:")
        if not port.isdigit():
            raise RuntimeError("the SSH fixture must publish exactly one loopback port")
        _, host_key = self.commands.run(["docker", "exec", self.name, "cat", "/etc/ssh/ssh_host_ed25519_key.pub"])
        (self.files / "known_hosts").write_text(f"[127.0.0.1]:{port} {host_key.strip()}\n")
        (self.files / "config").write_text(f"""Host fixture-bastion
    HostName 127.0.0.1
    Port {port}
    User smoke_tunnel
    IdentityFile "{self.files / 'client'}"
    UserKnownHostsFile "{self.files / 'known_hosts'}"
    StrictHostKeyChecking yes
    IdentitiesOnly yes
    BatchMode yes
    HostKeyAlgorithms ssh-ed25519
""")
        self.commands.run(["ssh", "-F", str(self.files / "config"), "fixture-bastion", "true"])

    def verify(self, selected_tests=None):
        environment = {"CULPRIT_KEYCHAIN": "off", "CULPRIT_LANG": "en",
                       "CULPRIT_SSH_LIVE_CONFIG": str(self.files / "config"),
                       "CULPRIT_SSH_LIVE_CA": str(self.files / "ca.pem"),
                       "CULPRIT_SSH_LIVE_PG_PORT": str(self.fixture.port)}
        tests = selected_tests or [("culprit-engine", "ssh::tests::real_openssh_")]
        for crate, selection in tests:
            run_tests(self.commands, crate, selection, environment, library=True)

    def close(self):
        try:
            super().close()
        finally:
            self.directory.cleanup()


class YugabyteCluster:
    def __init__(self):
        self.owner = secrets.token_hex(12)
        self.project = f"culprit-smoke-cluster-{self.owner}"
        self.directory = tempfile.TemporaryDirectory(prefix=self.project + "-")
        self.config = Path(self.directory.name) / "compose.json"
        self.commands = Commands([])
        self.environment = {"CULPRIT_KEYCHAIN": "off", "CULPRIT_LANG": "en",
                            "CULPRIT_YSQL_CLUSTER_TEST": "1", "CULPRIT_YSQL_CLUSTER_PROJECT": self.project,
                            "CULPRIT_YSQL_CLUSTER_OWNER": self.owner}

    def compose(self, *args):
        return ["docker", "compose", "--project-name", self.project, "--file", str(self.config), *args]

    def __enter__(self):
        try:
            self.start()
            return self
        except BaseException:
            if self.config.exists():
                self.commands.run(self.compose("logs", "--no-color", "--tail", "80"), check=False, echo=True)
            self.close()
            raise

    def __exit__(self, *exception):
        self.close()

    def start(self):
        print(f"Starting owned three-node cluster {self.project}", flush=True)
        _, source = self.commands.run(["docker", "compose", "-f", "dev/yugabyte/cluster.compose.yml",
                                       "config", "--format", "json"])
        config = json.loads(source)
        config["name"] = self.project
        for service in config["services"].values():
            service.setdefault("labels", {})[OWNER_LABEL] = self.owner
            for port in service["ports"]:
                port["published"] = "0"
                port["host_ip"] = "127.0.0.1"
        # Compose's resolved default-network name contains the source project's name: give this run its own network.
        config["networks"]["default"]["name"] = self.project + "-network"
        config["networks"]["default"].setdefault("labels", {})[OWNER_LABEL] = self.owner
        self.config.write_text(json.dumps(config))
        self.commands.run(self.compose("up", "--detach", "--wait", "--wait-timeout", str(STARTUP_BUDGET)),
                          timeout=STARTUP_BUDGET + 60, echo=True)
        for index in range(1, 4):
            self.environment[f"CULPRIT_YSQL_CLUSTER_PORT_{index}"] = self.port(f"yb{index}", 5433)
        self.environment["CULPRIT_YSQL_CLUSTER_PORT_POOL"] = self.port("yb3", 5434)
        self.sql("CREATE DATABASE culprit_cluster;", database="yugabyte")
        self.sql((ROOT / "dev/yugabyte/cluster-seed.sql").read_text())

    def port(self, service, port):
        _, published = self.commands.run(self.compose("port", service, str(port)))
        value = published.strip().removeprefix("127.0.0.1:")
        if not value.isdigit():
            raise RuntimeError("the cluster must publish exactly one loopback endpoint per port")
        return value

    def sql(self, sql, *, database="culprit_cluster"):
        self.commands.run(self.compose("exec", "-T", "yb1", "bin/ysqlsh", "-h", "culprit-yb-cluster-1",
                                       "-X", "-v", "ON_ERROR_STOP=1", "-U", "yugabyte", "-d", database), input=sql)

    def verify(self, selected_tests=None):
        for crate, selection in selected_tests or [("culprit-yugabyte", "cluster_tests::")]:
            run_tests(self.commands, crate, selection, self.environment, library=crate != "culprit-engine")

    def close(self):
        try:
            if not self.config.exists():
                return
            _, containers = self.commands.run(["docker", "ps", "--all", "--quiet", "--filter",
                                                f"label=com.docker.compose.project={self.project}"])
            for container in containers.split():
                _, owner = self.commands.run(["docker", "inspect", "--format",
                                              '{{index .Config.Labels "' + OWNER_LABEL + '"}}', container])
                if owner.strip() != self.owner:
                    raise RuntimeError("refusing to remove a cluster container without this run's ownership label")
                self.commands.run(["docker", "unpause", container], check=False)
            self.commands.run(self.compose("down", "--volumes", "--remove-orphans"), echo=True)
            print(f"Removed owned cluster {self.project}", flush=True)
        finally:
            self.directory.cleanup()


def interrupted(*_):
    raise KeyboardInterrupt


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("fixtures", nargs="+", choices=[f.name for f in FIXTURES] + ["yugabyte-cluster", "ssh", "all"])
    parser.add_argument("--test", nargs=2, action="append", metavar=("CRATE", "FILTER"),
                        help="run only this live test selection; repeat for multiple commands in the same fixture")
    args = parser.parse_args()
    selected = FIXTURES if "all" in args.fixtures else [f for f in FIXTURES if f.name in args.fixtures]
    signal.signal(signal.SIGTERM, interrupted)
    for fixture in selected:
        with Database(fixture) as database:
            database.seed()
            database.verify(args.test)
    count = len(selected)
    if "yugabyte-cluster" in args.fixtures or "all" in args.fixtures:
        with YugabyteCluster() as cluster:
            cluster.verify(args.test)
        count += 1
    if "ssh" in args.fixtures or "all" in args.fixtures:
        with SshDatabase() as database:
            database.seed()
            database.verify(args.test)
        count += 1
    print(f"Verified {count} disposable provider fixtures", flush=True)


if __name__ == "__main__":
    try:
        main()
    except (RuntimeError, OSError, KeyboardInterrupt) as error:
        print(str(error) or "Provider smoke run interrupted", file=sys.stderr)
        sys.exit(1)
