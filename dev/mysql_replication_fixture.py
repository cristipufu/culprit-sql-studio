"""Optional moving MySQL topology. All mutations belong to disposable, nonce-owned resources."""

from contextlib import ExitStack
import re
import secrets
import time

from replication_fixtures import OwnedNetwork


class MySqlMovingReplication:
    """Compose existing database/network ownership; do not introduce another container supervisor."""

    SOURCE_ID = 101
    REPLICA_ID = 102
    READY_SECONDS = 30

    def __init__(self, source, replica, owner_label):
        if any(database.fixture.engine != "mysql" or database.fixture.database != "culprit_smoke" for database in (source, replica)):
            raise ValueError("moving MySQL fixtures require the explicit owned culprit_smoke database")
        self.source = source
        self.replica = replica
        self.stack = ExitStack()
        self.network = OwnedNetwork(source.commands, source.owner, owner_label)
        self.channel = f"culprit_{source.owner}_moving"
        self.table = f"culprit_moving_{source.owner}"
        self.replication_user = f"crepl_{source.owner[:12]}"
        # Native SOURCE_PASSWORD accepts at most 32 bytes; keep 96 random bits inside that limit.
        self.replication_password = f"Repl!{secrets.token_hex(12)}Aa9"
        self.monitor_user = f"cmonitor_{replica.owner[:12]}"
        self.monitor_password = f"Monitor!{secrets.token_hex(16)}Aa9"
        self.source_uuid = None
        for database in (source, replica):
            database.commands.passwords.extend([self.replication_password, self.monitor_password])
            database.container_options += ["--network", self.network.name]
        source.options += [f"--server-id={self.SOURCE_ID}", "--log-bin=culprit-moving-bin", "--binlog-format=ROW"]
        replica.options += [f"--server-id={self.REPLICA_ID}", "--relay-log=culprit-moving-relay"]

    def __enter__(self):
        try:
            # Register each owner's cleanup before any account, table or replication-channel SQL is issued.
            self.stack.enter_context(self.network)
            self.stack.enter_context(self.source)
            self.stack.enter_context(self.replica)
            self.prepare()
            return self
        except BaseException:
            self.stack.close()
            raise

    def __exit__(self, *exception):
        self.stack.close()

    @staticmethod
    def require_owned(database):
        if not re.fullmatch(r"[0-9a-f]{24}", database.owner) or not database.owns_container():
            raise RuntimeError("refusing moving replication setup without the exact owned container")
        if not str(database.port).isdigit() or not 0 < int(database.port) <= 65535:
            raise RuntimeError("the owned moving fixture requires a published loopback port")
        _, hostname = database.sql("/* culprit */ SELECT @@hostname;", tuples=True, timeout=5)
        if hostname.strip() != f"culprit-smoke-{database.fixture.engine}-{database.owner}":
            raise RuntimeError("the native hostname does not match this moving fixture owner")

    def require_mysql84(self, database, server_id):
        self.require_owned(database)
        _, result = database.sql("/* culprit */ SELECT VERSION(),@@GLOBAL.server_id,@@GLOBAL.server_uuid;", tuples=True, timeout=5)
        fields = result.strip().split("\t")
        if len(fields) != 3 or not re.match(r"^8\.4(?:\.|$)", fields[0]) or fields[1] != str(server_id):
            raise RuntimeError("moving replication currently requires owned MySQL 8.4 with distinct configured server IDs")
        if not re.fullmatch(r"[0-9a-f]{8}(?:-[0-9a-f]{4}){3}-[0-9a-f]{12}", fields[2]):
            raise RuntimeError("the owned MySQL source must expose its native UUID")
        return fields[2]

    def prepare(self):
        self.source_uuid = self.require_mysql84(self.source, self.SOURCE_ID)
        replica_uuid = self.require_mysql84(self.replica, self.REPLICA_ID)
        if replica_uuid == self.source_uuid:
            raise RuntimeError("moving source and replica must have different native UUIDs")
        # The generated account is private to this disposable source. No mysql_native_password downgrade.
        self.mutate(self.source, f"/* culprit */ CREATE USER '{self.replication_user}'@'%' IDENTIFIED BY '{self.replication_password}'; "
                        f"GRANT REPLICATION SLAVE ON *.* TO '{self.replication_user}'@'%';")
        _, status = self.source.sql("/* culprit */ SHOW BINARY LOG STATUS;", tuples=True, timeout=5)
        log_file, position = self.coordinates(status)
        # MySQL 8.4 caching_sha2_password needs TLS or RSA exchange. This isolated owned network uses the
        # documented public-key option, never an application/source credential or a production auth change:
        # https://dev.mysql.com/doc/refman/8.4/en/replication-howto-slaveinit.html
        self.mutate(self.replica, f"/* culprit */ CHANGE REPLICATION SOURCE TO SOURCE_HOST='{self.source.name}',SOURCE_PORT=3306,"
                         f"SOURCE_USER='{self.replication_user}',SOURCE_PASSWORD='{self.replication_password}',"
                         f"SOURCE_LOG_FILE='{log_file}',SOURCE_LOG_POS={position},GET_SOURCE_PUBLIC_KEY=1 "
                         f"FOR CHANNEL '{self.channel}'; START REPLICA FOR CHANNEL '{self.channel}';")
        self.mutate(self.source, f"/* culprit */ CREATE TABLE {self.table}(id INT PRIMARY KEY,value INT NOT NULL);")
        self.wait_for_table()
        self.require_owned(self.replica)
        self.mutate(self.replica, f"/* culprit */ CREATE USER '{self.monitor_user}'@'%' IDENTIFIED BY '{self.monitor_password}'; "
                         f"GRANT REPLICATION CLIENT ON *.* TO '{self.monitor_user}'@'%'; "
                         f"GRANT SELECT ON {self.replica.fixture.database}.{self.table} TO '{self.monitor_user}'@'%';")

    def mutate(self, database, statement):
        self.require_owned(self.source)
        self.require_owned(self.replica)
        database.sql(statement, timeout=10)

    @staticmethod
    def coordinates(status):
        lines = status.strip().splitlines()
        fields = lines[0].split("\t") if len(lines) == 1 else []
        if len(fields) < 2 or not re.fullmatch(r"[A-Za-z0-9_.-]{1,255}", fields[0]):
            raise RuntimeError("the owned source did not expose one bounded native binary-log filename")
        if not re.fullmatch(r"(?:0|[1-9][0-9]{0,19})", fields[1]) or int(fields[1]) > 2**64 - 1:
            raise RuntimeError("the owned source did not expose an exact binary-log position")
        return fields[0], int(fields[1])

    def wait_for_table(self):
        deadline = time.monotonic() + self.READY_SECONDS
        while time.monotonic() < deadline:
            self.require_owned(self.replica)
            status, result = self.replica.sql(f"/* culprit */ SELECT COUNT(*) FROM {self.table};", tuples=True, check=False, timeout=5)
            if status == 0 and result.strip() == "0":
                return
            time.sleep(0.2)
        raise TimeoutError("the owned replica did not apply its identified empty fixture table")

    def environment(self):
        return {
            "CULPRIT_MYSQL_MOVING_SOURCE_PORT": self.source.port,
            "CULPRIT_MYSQL_MOVING_REPLICA_PORT": self.replica.port,
            "CULPRIT_MYSQL_MOVING_SOURCE_OWNER": self.source.owner,
            "CULPRIT_MYSQL_MOVING_REPLICA_OWNER": self.replica.owner,
            "CULPRIT_MYSQL_MOVING_SOURCE_PASSWORD": self.source.password,
            "CULPRIT_MYSQL_MOVING_MONITOR_USER": self.monitor_user,
            "CULPRIT_MYSQL_MOVING_MONITOR_PASSWORD": self.monitor_password,
            "CULPRIT_MYSQL_MOVING_CHANNEL": self.channel,
            "CULPRIT_MYSQL_MOVING_TABLE": self.table,
            "CULPRIT_MYSQL_MOVING_SOURCE_HOST": self.source.name,
            "CULPRIT_MYSQL_MOVING_SOURCE_UUID": self.source_uuid,
        }

    def verify(self, selected_tests=None):
        environment = self.environment()
        for crate, selection in selected_tests or [("culprit-mysql", "telemetry::native_tests::replication_movement::")]:
            self.source.test(crate, selection, library=crate != "culprit-engine", environment=environment)
