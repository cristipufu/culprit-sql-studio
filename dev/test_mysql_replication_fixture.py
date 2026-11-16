"""Pure ownership/configuration tests; no Docker or native database is contacted."""
from types import SimpleNamespace
from pathlib import Path
import sys
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).parent))
from mysql_replication_fixture import MySqlMovingReplication


class MovingReplicationOwnership(unittest.TestCase):
    def database(self, owner, server_id):
        database = MagicMock()
        database.owner = owner
        database.name = f"culprit-smoke-moving-{owner}"
        database.password = f"independent-{owner}"
        database.port = str(30000 + server_id)
        database.fixture = SimpleNamespace(engine="mysql", database="culprit_smoke")
        database.options = []
        database.container_options = []
        database.commands = SimpleNamespace(passwords=[], run=MagicMock())
        database.owns_container.return_value = True
        database.__enter__.return_value = database
        uuid = f"{owner[:8]}-1111-2222-3333-444444444444"

        def sql(statement, **_):
            if statement == "/* culprit */ SELECT @@hostname;":
                return (0, f"culprit-smoke-mysql-{owner}\n")
            if "VERSION()" in statement:
                return (0, f"8.4.8\t{server_id}\t{uuid}\n")
            if "SHOW BINARY LOG STATUS" in statement:
                return (0, "culprit-moving-bin.000001\t9007199254740993\t\t\t\n")
            return (0, "")

        database.sql.side_effect = sql
        return database

    def topology(self):
        return MySqlMovingReplication(self.database("a" * 24, 101), self.database("b" * 24, 102), "test.culprit.owner")

    def test_passwords_stay_independent_and_only_owned_network_endpoints_enter_setup(self):
        topology = self.topology()
        with patch.object(topology, "wait_for_table"):
            topology.prepare()
        source_password = topology.source.password
        self.assertNotEqual(source_password, topology.replica.password)
        self.assertNotEqual(source_password, topology.replication_password)
        self.assertIn("--server-id=101", topology.source.options)
        self.assertIn("--server-id=102", topology.replica.options)
        statements = [call.args[0] for call in topology.replica.sql.call_args_list]
        change = next(sql for sql in statements if "CHANGE REPLICATION SOURCE" in sql)
        self.assertIn(f"SOURCE_HOST='{topology.source.name}'", change)
        self.assertIn("GET_SOURCE_PUBLIC_KEY=1", change)
        self.assertIn("SOURCE_LOG_POS=9007199254740993", change)
        self.assertNotIn("mysql_native_password", change)
        environment = topology.environment()
        self.assertEqual(environment["CULPRIT_MYSQL_MOVING_SOURCE_PASSWORD"], source_password)
        self.assertNotIn(topology.replica.password, environment.values())
        self.assertNotIn(topology.replication_password, environment.values())

    def test_partial_admission_cancellation_closes_composed_owners_in_reverse_order(self):
        topology = self.topology()
        events = []
        topology.network = MagicMock()
        topology.network.__enter__.return_value = topology.network
        topology.network.__exit__.side_effect = lambda *_: events.append("network")
        topology.source.__exit__.side_effect = lambda *_: events.append("source")
        topology.replica.__exit__.side_effect = lambda *_: events.append("replica")
        with patch.object(topology, "prepare", side_effect=KeyboardInterrupt):
            with self.assertRaises(KeyboardInterrupt):
                with topology:
                    self.fail("cancelled setup cannot enter the native test body")
        self.assertEqual(events, ["replica", "source", "network"])
        topology.source.sql.assert_not_called()
        topology.replica.sql.assert_not_called()

    def test_missing_or_foreign_owner_prevents_mutation_on_either_node(self):
        topology = self.topology()
        topology.replica.owns_container.return_value = False
        with self.assertRaisesRegex(RuntimeError, "owned container"):
            topology.mutate(topology.source, "CREATE TABLE must_not_run(id INT)")
        self.assertFalse(any("must_not_run" in call.args[0] for call in topology.source.sql.call_args_list))
        topology.replica.sql.assert_not_called()

    def test_native_hostname_mismatch_refuses_setup_before_account_or_channel_creation(self):
        topology = self.topology()
        topology.source.sql.return_value = (0, "another-native-owner\n")
        topology.source.sql.side_effect = None
        with self.assertRaisesRegex(RuntimeError, "native hostname"):
            topology.prepare()
        self.assertEqual(topology.source.sql.call_count, 1)
        topology.replica.sql.assert_not_called()

    def test_exact_native_file_coordinates_reject_injection_overflow_and_ambiguous_rows(self):
        self.assertEqual(MySqlMovingReplication.coordinates("bin.000001\t9007199254740993\t\n"), ("bin.000001", 9007199254740993))
        for malformed in ["bin'; DROP\t4", "bin.000001\t18446744073709551616", "bin.000001\t4\nbin.000002\t4", "bin.000001\t04"]:
            with self.assertRaises(RuntimeError):
                MySqlMovingReplication.coordinates(malformed)


if __name__ == "__main__":
    unittest.main()
