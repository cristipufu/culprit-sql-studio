"""Optional, disposable SQL Server 2022 read-scale AG; never a production cost approval.

The caller supplies two unstarted smoke Database supervisors and their ownership-label key.
No host directories are mounted. Endpoint private keys and FULL/log backups stay inside the
containers; only bounded public CERTENCODED bytes cross between the two local instances.

Native setup follows Microsoft's Linux read-scale and certificate-endpoint documentation:
https://learn.microsoft.com/en-us/sql/linux/business-continuity/availability-groups/configure-read-scale
https://learn.microsoft.com/en-us/sql/database-engine/database-mirroring/use-certificates-for-a-database-mirroring-endpoint-transact-sql
https://learn.microsoft.com/en-us/sql/t-sql/statements/create-certificate-transact-sql
"""

from contextlib import ExitStack
from dataclasses import replace
import os
from pathlib import Path
import re
import secrets
import time

from replication_fixtures import OwnedNetwork


SQL_SERVER_IMAGE = "mcr.microsoft.com/mssql/server:2022-latest"
DATABASE_NAME = "culprit_smoke"
ENDPOINT_NAME = "culprit_ag_endpoint"
ENDPOINT_PORT = 5022
CERTIFICATE_NAME = "culprit_ag_local_certificate"
MAX_CERTIFICATE_BYTES = 16 * 1024
CERTIFICATE_CHUNK_HEX = 48
CERTIFICATE_CHUNK_RADIX = 32
SEEDING_BUDGET = 120
READINESS_QUERY_BUDGET = 5
POLL_INTERVAL = 0.2
VALUE_PREFIX = "culprit_fixture_value:"
CERTIFICATE_PREFIX = "culprit_fixture_certificate:"
CERTIFICATE_LINE_WIDTH = len(CERTIFICATE_PREFIX) + CERTIFICATE_CHUNK_HEX


def sql_string(value):
    return "N'" + value.replace("'", "''") + "'"


def member_hostname(database):
    return database.hostname


def prefixed_values(output, prefix):
    return [line.strip()[len(prefix):] for line in output.splitlines() if line.strip().startswith(prefix)]


def require_unstarted_member(database):
    fixture = database.fixture
    if (fixture.engine != "mssql" or fixture.image != SQL_SERVER_IMAGE
            or fixture.database != f"{DATABASE_NAME}_{database.owner}" or fixture.user != "sa"
            or fixture.platform != "linux/amd64" or database.port is not None
            or database.container_options or database.options or database.published_ports() != (1433,)
            or not re.fullmatch(r"[0-9a-f]{24}", database.owner)):
        raise ValueError("AG setup requires an unstarted owned SQL Server 2022 Linux Developer smoke member")


def enable_hadr(database, network):
    environment, command = database.configuration()
    if command or set(environment) - {"ACCEPT_EULA", "MSSQL_PID", "MSSQL_SA_PASSWORD", "MSSQL_ENABLE_HADR"}:
        raise ValueError("AG setup requires the standard owned SQL Server container launch policy")
    environment = {**environment, "MSSQL_ENABLE_HADR": "1", "MSSQL_PID": "Developer"}
    database.launch_configuration = (environment, command)
    database.container_options += ["--network", network.name, "--network-alias", member_hostname(database)]


class SqlServerReplication:
    """A primary and readable secondary, manual failover only, with exact per-member tests."""

    def __init__(self, primary, secondary, owner_label):
        require_unstarted_member(primary)
        require_unstarted_member(secondary)
        if primary.owner == secondary.owner or primary.name == secondary.name:
            raise ValueError("AG members must retain distinct owned container identities")
        self.primary = primary
        self.secondary = secondary
        self.members = (("primary", primary, 2), ("secondary", secondary, 1))
        self.stack = ExitStack()
        self.network = OwnedNetwork(primary.commands, primary.owner, owner_label)
        self.name = f"culprit_ag_{primary.owner}"
        self.database_name = f"{DATABASE_NAME}_{primary.owner}_{secondary.owner}"
        self.native_names = {}
        self.setup_elapsed_seconds = None
        for role, database, _ in self.members:
            # Linux requires physical hostnames of at most fifteen characters. Keep both full owner
            # nonces in the native database name; the Rust admission checks that native ownership proof.
            prefix = {"primary": "p", "secondary": "s"}[role]
            database.hostname = prefix + database.owner[:14]
            database.fixture = replace(database.fixture, database=self.database_name)
            enable_hadr(database, self.network)

    def __enter__(self):
        started = time.monotonic()
        try:
            self.require_cached_images()
            self.stack.enter_context(self.network)
            for role, database, _ in self.members:
                self.stack.enter_context(database)
                self.verify_instance_name(role, database)
            self.prepare_database()
            certificates = {role: self.create_endpoint(database) for role, database, _ in self.members}
            self.trust_peer(self.primary, certificates["secondary"])
            self.trust_peer(self.secondary, certificates["primary"])
            self.create_group()
            self.wait_until_ready()
            self.setup_elapsed_seconds = time.monotonic() - started
            return self
        except BaseException as error:
            try:
                if isinstance(error, Exception):
                    self.report_failure()
            finally:
                self.stack.close()
            raise

    def __exit__(self, *exception):
        self.stack.close()

    def report_failure(self):
        for role, database, _ in self.members:
            try:
                if not database.owns_container():
                    continue
                print(f"Owned AG {role} state and final server messages:", flush=True)
                database.commands.run(["docker", "inspect", "--format",
                                       "{{.State.Status}} OOM={{.State.OOMKilled}} exit={{.State.ExitCode}}",
                                       database.name], timeout=5, check=False, echo=True)
                database.commands.run(["docker", "logs", "--tail", "60", database.name],
                                      timeout=5, check=False, echo=True)
            except (OSError, RuntimeError, TimeoutError) as error:
                print(f"Owned AG {role} diagnostics unavailable: {type(error).__name__}", flush=True)

    def require_cached_images(self):
        for _, database, _ in self.members:
            status, _ = database.commands.run(
                ["docker", "image", "inspect", database.fixture.image, "--format", "{{.Id}}"], check=False)
            if status:
                raise RuntimeError("the optional AG fixture requires the already cached SQL Server 2022 image")

    @staticmethod
    def sql(database, statement, *, selected_database="master", timeout=30):
        if not database.owns_container():
            raise RuntimeError("the exact owned AG member disappeared before native setup or verification")
        return database.sql(statement, database=selected_database, timeout=timeout)

    def value(self, database, expression, *, timeout=30):
        _, output = self.sql(database, f"SET NOCOUNT ON; SELECT {sql_string(VALUE_PREFIX)} + "
                            f"CONVERT(nvarchar(200),({expression}));", timeout=timeout)
        values = prefixed_values(output, VALUE_PREFIX)
        if len(values) != 1:
            raise RuntimeError("the owned AG member did not return one complete native fixture value")
        return values[0]

    def verify_machine(self, database):
        machine = self.value(database, "SERVERPROPERTY('MachineName')")
        if machine != member_hostname(database):
            raise RuntimeError("native SQL MachineName does not match this member's owned physical hostname")
        if self.value(database, "CONCAT(CONVERT(varchar(10),SERVERPROPERTY('ProductMajorVersion')), ':', "
                      "CONVERT(varchar(10),SERVERPROPERTY('EngineEdition')), ':', "
                      "CONVERT(varchar(10),SERVERPROPERTY('IsHadrEnabled')))") != "16:3:1":
            raise RuntimeError("the owned AG member is not HADR-enabled SQL Server 2022 Developer")

    def verify_instance_name(self, role, database):
        self.verify_machine(database)
        actual_name = self.value(database, "@@SERVERNAME")
        if actual_name != member_hostname(database):
            raise RuntimeError(f"the owned AG server name differs from its hostname: {actual_name!r}")
        self.native_names[role] = actual_name

    def prepare_database(self):
        print("Preparing owned AG database and seed backups", flush=True)
        if not self.primary.owns_container():
            raise RuntimeError("the owned primary disappeared before database preparation")
        self.primary.seed()
        self.sql(self.primary, "CREATE TABLE dbo.culprit_ag_marker (owner char(24) NOT NULL PRIMARY KEY); "
                 f"INSERT dbo.culprit_ag_marker VALUES ({sql_string(self.primary.owner)});",
                 selected_database=self.database_name)
        prefix = f"/var/opt/mssql/data/culprit_ag_{self.primary.owner}"
        self.sql(self.primary, f"ALTER DATABASE [{self.database_name}] SET RECOVERY FULL; "
                 f"BACKUP DATABASE [{self.database_name}] TO DISK={sql_string(prefix + '.bak')} "
                 "WITH INIT, COMPRESSION, CHECKSUM; "
                 f"BACKUP LOG [{self.database_name}] TO DISK={sql_string(prefix + '.trn')} "
                 "WITH INIT, COMPRESSION, CHECKSUM;", timeout=60)

    def create_endpoint(self, database):
        print(f"Preparing owned AG endpoint on {database.name}", flush=True)
        password = f"Master!{secrets.token_hex(16)}Aa9"
        database.commands.passwords.append(password)
        self.sql(database, f"CREATE MASTER KEY ENCRYPTION BY PASSWORD={sql_string(password)}; "
                 f"CREATE CERTIFICATE [{CERTIFICATE_NAME}] WITH SUBJECT=N'Culprit owned AG endpoint'; "
                 f"CREATE ENDPOINT [{ENDPOINT_NAME}] STATE=STARTED "
                 f"AS TCP (LISTENER_PORT={ENDPOINT_PORT}, LISTENER_IP=ALL) "
                 f"FOR DATABASE_MIRRORING (AUTHENTICATION=CERTIFICATE [{CERTIFICATE_NAME}], "
                 "ENCRYPTION=REQUIRED ALGORITHM AES, ROLE=ALL);")
        return self.public_certificate(database)

    def public_certificate(self, database):
        # Each declared column and prefix-plus-hex value fits sqlcmd's default 80-column screen.
        # Return ordered public chunks only, never CERTPRIVATEKEY or a host-side private-key file.
        numbers = ",".join(f"({number})" for number in range(CERTIFICATE_CHUNK_RADIX))
        _, output = self.sql(database, f"""SET NOCOUNT ON;
DECLARE @encoded varchar(max)=CONVERT(varchar(max),CERTENCODED(CERT_ID(N'{CERTIFICATE_NAME}')),2);
IF @encoded IS NULL OR DATALENGTH(@encoded)=0 OR DATALENGTH(@encoded)>{MAX_CERTIFICATE_BYTES * 2}
    THROW 50000, 'The owned public endpoint certificate exceeds its bounded fixture policy', 1;
SELECT '{VALUE_PREFIX}' + CONVERT(varchar(10),DATALENGTH(@encoded));
WITH chunks AS (SELECT a.n + {CERTIFICATE_CHUNK_RADIX}*b.n AS n FROM (VALUES {numbers}) a(n) CROSS JOIN (VALUES {numbers}) b(n))
SELECT CONVERT(varchar({CERTIFICATE_LINE_WIDTH}),'{CERTIFICATE_PREFIX}' + SUBSTRING(@encoded,n*{CERTIFICATE_CHUNK_HEX}+1,{CERTIFICATE_CHUNK_HEX}))
FROM chunks WHERE n*{CERTIFICATE_CHUNK_HEX}<DATALENGTH(@encoded) ORDER BY n;
""")
        chunks = prefixed_values(output, CERTIFICATE_PREFIX)
        lengths = prefixed_values(output, VALUE_PREFIX)
        encoded = "".join(chunks)
        if (not chunks or len(encoded) % 2 or len(encoded) > MAX_CERTIFICATE_BYTES * 2
                or lengths != [str(len(encoded))]
                or any(len(chunk) != CERTIFICATE_CHUNK_HEX for chunk in chunks[:-1])
                or not re.fullmatch(r"[0-9A-Fa-f]+", encoded)):
            raise RuntimeError("the owned member returned a truncated or invalid public endpoint certificate")
        return encoded

    def trust_peer(self, database, certificate):
        print(f"Trusting owned AG peer on {database.name}", flush=True)
        password = f"Peer!{secrets.token_hex(16)}Aa9"
        database.commands.passwords.append(password)
        self.sql(database, f"CREATE LOGIN culprit_ag_peer WITH PASSWORD={sql_string(password)}; "
                 "CREATE USER culprit_ag_peer FOR LOGIN culprit_ag_peer; "
                 "CREATE CERTIFICATE culprit_ag_peer_certificate AUTHORIZATION culprit_ag_peer "
                 f"FROM BINARY=0x{certificate}; "
                 f"GRANT CONNECT ON ENDPOINT::[{ENDPOINT_NAME}] TO culprit_ag_peer;")

    def create_group(self):
        replicas = []
        for role, database, _ in self.members:
            endpoint = f"TCP://{member_hostname(database)}:{ENDPOINT_PORT}"
            replicas.append(f"{sql_string(self.native_names[role])} WITH (ENDPOINT_URL={sql_string(endpoint)}, "
                            "AVAILABILITY_MODE=ASYNCHRONOUS_COMMIT, FAILOVER_MODE=MANUAL, "
                            "SEEDING_MODE=AUTOMATIC, SECONDARY_ROLE(ALLOW_CONNECTIONS=ALL))")
        print("Creating owned AG on the primary", flush=True)
        self.sql(self.primary, f"CREATE AVAILABILITY GROUP [{self.name}] WITH (CLUSTER_TYPE=NONE) "
                 "FOR REPLICA ON " + ",".join(replicas) + ";")
        print("Joining owned AG on the secondary", flush=True)
        self.sql(self.secondary, f"ALTER AVAILABILITY GROUP [{self.name}] JOIN WITH (CLUSTER_TYPE=NONE); "
                 f"ALTER AVAILABILITY GROUP [{self.name}] GRANT CREATE ANY DATABASE;")
        print("Adding the owned database to the AG", flush=True)
        self.sql(self.primary, f"ALTER AVAILABILITY GROUP [{self.name}] ADD DATABASE [{self.database_name}];")

    def ready(self, role, database, visible_rows, timeout):
        writable = "READ_WRITE" if role == "primary" else "READ_ONLY"
        expression = f"""CASE WHEN
EXISTS (SELECT 1 FROM sys.availability_groups ag
 JOIN sys.dm_hadr_availability_replica_states ars ON ars.group_id=ag.group_id
 WHERE ag.name={sql_string(self.name)} AND ars.is_local=1 AND ars.role_desc={sql_string(role.upper())})
AND EXISTS (SELECT 1 FROM sys.dm_hadr_database_replica_states drs
 JOIN sys.availability_groups ag ON ag.group_id=drs.group_id
 WHERE ag.name={sql_string(self.name)} AND drs.database_id=DB_ID(N'{self.database_name}')
 AND drs.is_local=1 AND drs.synchronization_state IN (1,2) AND drs.is_suspended=0)
AND (SELECT COUNT(*) FROM sys.dm_hadr_database_replica_states drs
 JOIN sys.availability_groups ag ON ag.group_id=drs.group_id
 WHERE ag.name={sql_string(self.name)} AND drs.database_id=DB_ID(N'{self.database_name}'))={visible_rows}
AND DATABASEPROPERTYEX(N'{self.database_name}','Status')=N'ONLINE'
AND DATABASEPROPERTYEX(N'{self.database_name}','Updateability')={sql_string(writable)}
THEN N'ready' ELSE N'pending' END"""
        return self.value(database, expression, timeout=timeout) == "ready"

    def wait_until_ready(self):
        print("Checking owned AG roles, native visibility and replicated marker", flush=True)
        deadline = time.monotonic() + SEEDING_BUDGET
        while time.monotonic() < deadline:
            ready = True
            for role, database, visible_rows in self.members:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    ready = False
                    break
                ready = self.ready(role, database, visible_rows, min(READINESS_QUERY_BUDGET, remaining)) and ready
            if ready:
                self.verify_marker()
                return
            time.sleep(min(POLL_INTERVAL, max(0, deadline - time.monotonic())))
        raise TimeoutError("the owned AG did not establish both expected native roles, rows and database visibility")

    def verify_marker(self):
        for _, database, _ in self.members:
            _, output = self.sql(database, f"SET NOCOUNT ON; SELECT {sql_string(VALUE_PREFIX)} + owner "
                                 "FROM dbo.culprit_ag_marker;", selected_database=self.database_name)
            owners = prefixed_values(output, VALUE_PREFIX)
            if owners != [self.primary.owner]:
                raise RuntimeError("the readable replica did not retain the exact primary's seeded marker")

    def topology(self):
        return {"kind": "owned-sql-server-2022-ag-pair", "group": self.name, "cluster_type": "NONE",
                "availability_mode": "ASYNCHRONOUS_COMMIT", "failover_mode": "MANUAL",
                "database": self.database_name, "setup_client_elapsed_seconds": self.setup_elapsed_seconds,
                "production_admission": "unqualified", "production_cost_qualified": False,
                "members": [{"role": role, "machine": member_hostname(database),
                             "native_server_name": self.native_names[role], "loopback_port": database.port,
                             "expected_visible_database_rows": rows} for role, database, rows in self.members]}

    def receipt_directories(self):
        root = Path(os.environ["CULPRIT_NATIVE_COST_RECEIPT_DIR"])
        if not root.is_absolute() or not root.is_dir() or root.is_symlink():
            raise ValueError("the AG fixture requires an existing absolute private receipt directory")
        directories = {}
        for role, _, _ in self.members:
            directory = root / f"mssql-ag-{self.primary.owner}-{role}"
            directory.mkdir(mode=0o700)
            directories[role] = directory
        return directories

    def verify(self, selected_tests=None):
        self.wait_until_ready()
        directories = self.receipt_directories()
        tests = selected_tests or [("culprit-mssql", "telemetry::replication::native_tests::"
                                   "owned_replication_profile_measures_the_actual_reader_scope_limits_and_settlement")]
        for role, database, rows in self.members:
            environment = {"CULPRIT_MSSQL_REPLICATION_EXPECT_EXPANDED_AG": "1",
                           "CULPRIT_MSSQL_REPLICATION_EXPECT_ROLE": role,
                           "CULPRIT_MSSQL_REPLICATION_EXPECT_GROUP": self.name,
                           "CULPRIT_MSSQL_REPLICATION_EXPECT_ROWS": str(rows),
                           "CULPRIT_NATIVE_COST_RECEIPT_DIR": str(directories[role])}
            for crate, selection in tests:
                database.test(crate, selection, library=crate != "culprit-engine", environment=environment)
