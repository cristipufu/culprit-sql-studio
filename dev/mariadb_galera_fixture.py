"""One genuine isolated Galera component; existing Database owns startup, secrets and destruction."""
from contextlib import ExitStack
import re

NATIVE_SELECTION = "telemetry::native_tests::galera::owned_galera_factory_outbox_retains_real_node_queues_totals_and_original_scope_without_cluster_health_claim"
GALERA_OPTIONS = ("--wsrep-provider=/usr/lib/libgalera_smm.so", "--wsrep-on=ON", "--wsrep-cluster-address=gcomm://", "--wsrep-new-cluster", "--binlog-format=ROW")


class MariaDbGalera:
    def __init__(self, database):
        self.database = database
        self.stack = ExitStack()

    def __enter__(self):
        try:
            self.database.options.extend(GALERA_OPTIONS)
            self.stack.enter_context(self.database)
            self.require_owned()
            return self
        except BaseException:
            self.stack.close()
            raise

    def __exit__(self, *exception):
        self.stack.close()

    def require_owned(self):
        database = self.database
        if (database.fixture.name != "mariadb-galera" or database.fixture.image != "mariadb:11.8-ubi"
                or not re.fullmatch(r"[0-9a-f]{24}", database.owner) or not database.owns_container()):
            raise RuntimeError("refusing Galera validation outside the exact disposable owned fixture")
        _, result = database.sql("/* culprit */ SELECT VERSION(),@@hostname,@@GLOBAL.server_uid;", tuples=True, timeout=5)
        values = result.strip().split("\t")
        if (len(values) != 3 or not re.match(r"^11\.8\.[0-9]+-MariaDB", values[0])
                or values[1] != f"culprit-smoke-mysql-{database.owner}"
                or not re.fullmatch(r"[A-Za-z0-9+/]{27}=", values[2])):
            raise RuntimeError("native Galera fixture version, hostname or server UID was not qualified")
        return values[2]

    def verify(self, selected_tests=None):
        expected = [("culprit-mysql", NATIVE_SELECTION)]
        if selected_tests and selected_tests != expected:
            raise RuntimeError("the owned Galera fixture only runs its exact native positive selector")
        uid = self.require_owned()
        self.database.test("culprit-mysql", NATIVE_SELECTION, environment={
            "CULPRIT_MARIADB_GALERA_SINGLE_NODE": "1", "CULPRIT_MARIADB_GALERA_SERVER_UID": uid})
