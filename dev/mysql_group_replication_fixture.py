"""Single-member Group Replication setup on the existing nonce-owned smoke Database."""

from contextlib import ExitStack
import re
import time
from uuid import UUID, uuid4


POSITIVE_SELECTION = (
    "telemetry::native_tests::group_replication::"
    "owned_group_replication_factory_outbox_preserves_real_member_roles_and_native_transaction_statistics"
)
NEGATIVE_SELECTION = (
    "telemetry::native_tests::group_replication::"
    "owned_plain_mysql_preserves_qualified_unavailable_group_sources_instead_of_healthy_empty"
)


class MySqlGroupReplication:
    """Configure one disposable member; collection still uses the registered native source."""

    SERVER_ID = 201
    LOCAL_ADDRESS = "127.0.0.1:33061"
    CHANNEL = "group_replication_applier"
    SETUP_SECONDS = 60
    SQL_SECONDS = 5
    START_SECONDS = 20
    MAX_ONLINE_OBSERVATIONS = 20
    OBSERVATION_PAUSE = 0.1
    MAX_SQL_DISPATCHES = 2 + MAX_ONLINE_OBSERVATIONS
    MAX_SQL_STATEMENTS = MAX_SQL_DISPATCHES + 2

    def __init__(self, database):
        fixture = database.fixture
        if fixture.name != "mysql-group-replication" or fixture.engine != "mysql" or fixture.database != "culprit_smoke":
            raise ValueError("Group Replication setup requires the dedicated owned MySQL fixture")
        if database.options:
            raise ValueError("the owned single-member profile does not accept conflicting server options")
        self.database = database
        self.stack = ExitStack()
        self.group_id = str(uuid4())
        self.server_uuid = None
        self.version = None
        self.image_id = None
        self.deadline = None
        self.ready = False
        self.sql_dispatches = 0
        self.sql_statements = 0
        self.owner_checks = 0
        self.online_observations = 0
        # MySQL 8.4 defaults already provide ROW/binlog/log_replica_updates. State them explicitly, and
        # do not copy the removed transaction_write_set_extraction setting from older tutorials.
        # https://dev.mysql.com/doc/refman/8.4/en/group-replication-requirements.html
        database.options += [
            "--performance-schema=ON", f"--server-id={self.SERVER_ID}",
            "--log-bin=culprit-group-bin", "--log-replica-updates=ON", "--binlog-format=ROW",
            "--gtid-mode=ON", "--enforce-gtid-consistency=ON", "--plugin-load-add=group_replication.so",
            # The official image initializes the data directory before loading plugins. Allow plugin
            # options during that phase; admit_native_configuration still requires every actual value
            # and an ACTIVE plugin before the one permitted bootstrap.
            # https://dev.mysql.com/doc/refman/8.4/en/install-plugin.html
            f"--loose-group-replication-group-name={self.group_id}", "--loose-group-replication-start-on-boot=OFF",
            "--loose-group-replication-bootstrap-group=OFF", "--loose-group-replication-single-primary-mode=ON",
            "--loose-group-replication-communication-stack=XCOM", "--loose-group-replication-ip-allowlist=127.0.0.1",
            f"--loose-group-replication-local-address={self.LOCAL_ADDRESS}",
            f"--loose-group-replication-group-seeds={self.LOCAL_ADDRESS}",
        ]

    def __enter__(self):
        if self.deadline is not None:
            raise RuntimeError("an owned Group Replication setup cannot be entered twice")
        try:
            self.stack.enter_context(self.database)
            self.prepare()
            return self
        except BaseException:
            self.stack.close()
            raise

    def __exit__(self, *exception):
        # Database.close owns exact nonce-label verification and container+volume retirement. Do not send
        # a speculative STOP command after a failed/unknown START or a cancelled native test.
        self.stack.close()

    def remaining(self, maximum):
        remaining = self.deadline - time.monotonic()
        if remaining <= 0:
            raise TimeoutError("the owned Group Replication setup deadline expired")
        return min(maximum, remaining)

    def require_owned(self):
        database = self.database
        if not re.fullmatch(r"[0-9a-f]{24}", database.owner):
            raise RuntimeError("Group Replication setup requires this run's fixture ownership nonce")
        self.owner_checks += 1
        if not database.owns_container(timeout=self.remaining(self.SQL_SECONDS)):
            raise RuntimeError("the exact owned Group Replication container disappeared")
        if not str(database.port).isdigit() or not 0 < int(database.port) <= 65535:
            raise RuntimeError("the owned Group Replication fixture requires a published loopback port")

    def query(self, statement, *, timeout=None, statements=1):
        if (self.sql_dispatches >= self.MAX_SQL_DISPATCHES
                or self.sql_statements + statements > self.MAX_SQL_STATEMENTS):
            raise RuntimeError("the owned Group Replication setup SQL allowance was exhausted")
        self.require_owned()
        self.sql_dispatches += 1
        self.sql_statements += statements
        _, result = self.database.sql(
            statement, tuples=True, timeout=self.remaining(timeout or self.SQL_SECONDS),
        )
        return result

    @staticmethod
    def fields(result, count):
        rows = result.rstrip("\r\n").splitlines()
        fields = rows[0].split("\t") if len(rows) == 1 else []
        if len(fields) != count:
            raise RuntimeError("the owned Group Replication setup returned an ambiguous native witness")
        return fields

    @staticmethod
    def native_uuid(value):
        try:
            parsed = UUID(value)
        except (ValueError, AttributeError) as error:
            raise RuntimeError("the owned Group Replication member did not expose a native UUID") from error
        if str(parsed) != value.lower() or parsed.int == 0:
            raise RuntimeError("the owned Group Replication member UUID is not canonical and nonzero")
        return str(parsed)

    def prepare(self):
        if self.deadline is not None:
            raise RuntimeError("an owned Group Replication setup cannot bootstrap twice")
        self.deadline = time.monotonic() + self.SETUP_SECONDS
        self.require_owned()
        _, image = self.database.commands.run(
            ["docker", "inspect", "--format", "{{.Image}}", self.database.name],
            timeout=self.remaining(self.SQL_SECONDS),
        )
        self.image_id = image.strip()
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", self.image_id):
            raise RuntimeError("the owned Group Replication fixture did not expose its exact image identity")
        self.admit_native_configuration()
        # Exactly one bootstrap attempt, as documented. A failure/timeout retires this disposable owner;
        # it never retries START, reuses an uncertain member, or reports an unsupported positive as passed.
        # https://dev.mysql.com/doc/refman/8.4/en/group-replication-bootstrap.html
        result = self.query(
            "/* culprit */ SET GLOBAL group_replication_bootstrap_group=ON; "
            "START GROUP_REPLICATION; SET GLOBAL group_replication_bootstrap_group=OFF;",
            timeout=self.START_SECONDS, statements=3,
        )
        if result.strip():
            raise RuntimeError("the owned Group Replication bootstrap returned an unexpected result")
        self.wait_online()

    def admit_native_configuration(self):
        result = self.query(
            "/* culprit */ SELECT @@hostname,VERSION(),@@GLOBAL.server_id,@@GLOBAL.server_uuid,"
            "@@GLOBAL.gtid_mode,@@GLOBAL.enforce_gtid_consistency,@@GLOBAL.log_bin,"
            "@@GLOBAL.log_replica_updates,@@GLOBAL.binlog_format,@@GLOBAL.performance_schema,"
            "@@GLOBAL.group_replication_group_name,@@GLOBAL.group_replication_start_on_boot,"
            "@@GLOBAL.group_replication_bootstrap_group,@@GLOBAL.group_replication_communication_stack,"
            "@@GLOBAL.group_replication_local_address,@@GLOBAL.group_replication_group_seeds,"
            "@@GLOBAL.group_replication_ip_allowlist,@@GLOBAL.group_replication_single_primary_mode,"
            "(SELECT PLUGIN_STATUS FROM information_schema.PLUGINS WHERE PLUGIN_NAME='group_replication');"
        )
        values = self.fields(result, 19)
        hostname, version, server_id, server_uuid = values[:4]
        if hostname != f"culprit-smoke-mysql-{self.database.owner}":
            raise RuntimeError("the native hostname does not match this Group Replication fixture owner")
        if not re.fullmatch(r"8\.4\.[0-9]+(?:-[A-Za-z0-9_.-]+)?", version) or server_id != str(self.SERVER_ID):
            raise RuntimeError("this owned Group Replication profile requires observed MySQL 8.4 and its exact server ID")
        self.server_uuid = self.native_uuid(server_uuid)
        if self.server_uuid == self.group_id:
            raise RuntimeError("the configured group UUID must differ from the original native member UUID")
        expected = [
            "ON", "ON", "1", "1", "ROW", "1", self.group_id, "0", "0", "XCOM",
            self.LOCAL_ADDRESS, self.LOCAL_ADDRESS, "127.0.0.1", "1", "ACTIVE",
        ]
        if values[4:] != expected:
            raise RuntimeError("the owned MySQL native configuration does not match the single-member GR profile")
        self.version = version

    def wait_online(self):
        for _ in range(self.MAX_ONLINE_OBSERVATIONS):
            self.online_observations += 1
            result = self.query(
                "/* culprit */ SELECT @@hostname,@@GLOBAL.server_uuid,@@GLOBAL.group_replication_group_name,"
                "@@GLOBAL.group_replication_bootstrap_group,@@GLOBAL.group_replication_single_primary_mode,"
                "CHANNEL_NAME,MEMBER_ID,MEMBER_STATE,MEMBER_ROLE,MEMBER_VERSION "
                "FROM performance_schema.replication_group_members LIMIT 2;"
            )
            if result.strip():
                values = self.fields(result, 10)
                expected = [f"culprit-smoke-mysql-{self.database.owner}", self.group_id, "0", "1", self.CHANNEL]
                if ([values[0], *values[2:6]] != expected
                        or self.native_uuid(values[1]) != self.server_uuid
                        or self.native_uuid(values[6]) != self.server_uuid):
                    raise RuntimeError("the native Group Replication witness changed its original member/group/channel")
                state, role, version = values[7:]
                if state == "ONLINE":
                    if role != "PRIMARY" or version != self.version:
                        raise RuntimeError("the owned single-member group is not the original native PRIMARY/version")
                    self.ready = True
                    return
                if state not in ("OFFLINE", "RECOVERING"):
                    raise RuntimeError("the owned Group Replication member entered an unsupported native state")
            time.sleep(self.remaining(self.OBSERVATION_PAUSE))
        raise TimeoutError("the owned single-member Group Replication group did not become ONLINE")

    def environment(self):
        if not self.ready:
            raise RuntimeError("native Group Replication acceptance requires a completed original setup witness")
        return {
            "CULPRIT_MYSQL_GROUP_REPLICATION_GROUP_ID": self.group_id,
            "CULPRIT_MYSQL_GROUP_REPLICATION_MEMBER_IDS": self.server_uuid,
        }

    def verify(self, selected_tests=None):
        environment = self.environment()
        print(
            f"  Owned single-member GR: MySQL {self.version}; image {self.image_id}; "
            f"setup batches {self.sql_dispatches}/{self.MAX_SQL_DISPATCHES}; "
            f"statements {self.sql_statements}/{self.MAX_SQL_STATEMENTS}; "
            f"owner checks {self.owner_checks}; native observations {self.online_observations}", flush=True,
        )
        for crate, selection in selected_tests or [("culprit-mysql", POSITIVE_SELECTION)]:
            self.database.test(crate, selection, library=crate != "culprit-engine", environment=environment)
