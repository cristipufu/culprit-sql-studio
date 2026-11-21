"""Pure setup/lifecycle controls. These tests never contact Docker or a native database."""

from pathlib import Path
from types import SimpleNamespace
import sys
import unittest
from unittest.mock import MagicMock, patch

sys.path.insert(0, str(Path(__file__).parent))
from mysql_group_replication_fixture import MySqlGroupReplication, POSITIVE_SELECTION


class GroupReplicationOwnership(unittest.TestCase):
    OWNER = "a" * 24
    NATIVE_UUID = "bbbbbbbb-1111-2222-3333-444444444444"

    def profile(self):
        database = MagicMock()
        database.owner = self.OWNER
        database.name = f"culprit-smoke-mysql-group-replication-{self.OWNER}"
        database.port = "34061"
        database.fixture = SimpleNamespace(name="mysql-group-replication", engine="mysql", database="culprit_smoke")
        database.options = []
        database.commands = SimpleNamespace(run=MagicMock(return_value=(0, "sha256:" + "c" * 64 + "\n")))
        database.owns_container.return_value = True
        database.__enter__.return_value = database
        profile = MySqlGroupReplication(database)

        def sql(statement, **_):
            if "VERSION()" in statement:
                return (0, "\t".join(self.configuration(profile)) + "\n")
            if "FROM performance_schema.replication_group_members" in statement:
                return (0, "\t".join(self.member(profile)) + "\n")
            return (0, "")

        database.sql.side_effect = sql
        return profile

    def configuration(self, profile):
        return [
            f"culprit-smoke-mysql-{self.OWNER}", "8.4.8", "201", self.NATIVE_UUID,
            "ON", "ON", "1", "1", "ROW", "1", profile.group_id, "0", "0", "XCOM",
            profile.LOCAL_ADDRESS, profile.LOCAL_ADDRESS, "127.0.0.1", "1", "ACTIVE",
        ]

    def member(self, profile):
        return [
            f"culprit-smoke-mysql-{self.OWNER}", self.NATIVE_UUID, profile.group_id, "0", "1",
            profile.CHANNEL, self.NATIVE_UUID, "ONLINE", "PRIMARY", "8.4.8",
        ]

    @staticmethod
    def starts(profile):
        return [call for call in profile.database.sql.call_args_list if "START GROUP_REPLICATION" in call.args[0]]

    def test_single_native_member_enters_existing_factory_selection_with_original_uuid(self):
        profile = self.profile()
        with profile:
            profile.verify()
            self.assertEqual(profile.sql_dispatches, 3)
            self.assertEqual(profile.sql_statements, 5)
            self.assertEqual(profile.online_observations, 1)
            self.assertEqual(profile.image_id, "sha256:" + "c" * 64)
            self.assertEqual(profile.environment(), {
                "CULPRIT_MYSQL_GROUP_REPLICATION_GROUP_ID": profile.group_id,
                "CULPRIT_MYSQL_GROUP_REPLICATION_MEMBER_IDS": self.NATIVE_UUID,
            })
        self.assertEqual(len(self.starts(profile)), 1)
        profile.database.test.assert_called_once_with(
            "culprit-mysql", POSITIVE_SELECTION, library=True, environment=profile.environment(),
        )
        profile.database.__exit__.assert_called_once()

    def test_plugin_options_allow_initialization_while_core_requirements_remain_strict(self):
        profile = self.profile()
        options = profile.database.options
        self.assertIn("--loose-group-replication-start-on-boot=OFF", options)
        self.assertIn("--loose-group-replication-bootstrap-group=OFF", options)
        self.assertIn("--loose-group-replication-local-address=127.0.0.1:33061", options)
        self.assertIn("--loose-group-replication-ip-allowlist=127.0.0.1", options)
        self.assertEqual(sum(value.startswith("--loose-group-replication-") for value in options), 8)
        self.assertFalse(any(value.startswith("--group-replication-") for value in options))
        self.assertIn("--gtid-mode=ON", options)
        self.assertIn("--enforce-gtid-consistency=ON", options)
        self.assertIn("--plugin-load-add=group_replication.so", options)
        self.assertFalse(any("transaction-write-set-extraction" in value for value in options))
        self.assertFalse(any("mysql-native-password" in value for value in options))
        profile.database.sql.assert_not_called()
        profile.database.commands.run.assert_not_called()

    def test_foreign_or_missing_container_nonce_prevents_every_native_mutation(self):
        for change in ("label", "nonce", "port"):
            with self.subTest(change=change):
                profile = self.profile()
                if change == "label":
                    profile.database.owns_container.return_value = False
                elif change == "nonce":
                    profile.database.owner = "not-owned"
                else:
                    profile.database.port = "3306/foreign"
                with self.assertRaises(RuntimeError):
                    with profile:
                        self.fail("a foreign fixture cannot reach native acceptance")
                profile.database.sql.assert_not_called()
                profile.database.__exit__.assert_called_once()

    def test_native_hostname_and_unsupported_version_refuse_before_bootstrap(self):
        for position, value in ((0, "foreign"), (1, "8.0.41"), (1, "11.8.5-MariaDB"), (2, "1")):
            with self.subTest(position=position, value=value):
                profile = self.profile()
                row = self.configuration(profile)
                row[position] = value
                profile.database.sql.side_effect = None
                profile.database.sql.return_value = (0, "\t".join(row))
                with self.assertRaises(RuntimeError):
                    with profile:
                        self.fail("a foreign or unsupported native server cannot bootstrap")
                self.assertEqual(self.starts(profile), [])
                profile.database.__exit__.assert_called_once()

    def test_missing_plugin_or_mismatched_native_configuration_never_skip_passes(self):
        for position, value in ((4, "OFF"), (9, "0"), (10, self.NATIVE_UUID), (11, "1"),
                                (12, "1"), (13, "MYSQL"), (16, "AUTOMATIC"), (18, "NULL")):
            with self.subTest(position=position, value=value):
                profile = self.profile()
                row = self.configuration(profile)
                row[position] = value
                profile.database.sql.side_effect = None
                profile.database.sql.return_value = (0, "\t".join(row))
                with self.assertRaises(RuntimeError):
                    with profile:
                        self.fail("native setup qualification cannot be replaced by an option label")
                self.assertEqual(self.starts(profile), [])
                self.assertFalse(profile.ready)
                profile.database.__exit__.assert_called_once()

    def test_foreign_member_group_channel_and_wrong_native_roles_refuse_acceptance(self):
        for position, value in ((0, "foreign"), (1, "cccccccc-1111-2222-3333-444444444444"),
                                (2, "cccccccc-1111-2222-3333-444444444444"), (3, "1"), (4, "0"),
                                (5, "other_channel"), (6, "cccccccc-1111-2222-3333-444444444444"),
                                (7, "ERROR"), (8, "SECONDARY"), (9, "8.4.7")):
            with self.subTest(position=position, value=value):
                profile = self.profile()
                original = profile.database.sql.side_effect

                def changed(statement, **kwargs):
                    if "FROM performance_schema.replication_group_members" in statement:
                        row = self.member(profile)
                        row[position] = value
                        return (0, "\t".join(row))
                    return original(statement, **kwargs)

                profile.database.sql.side_effect = changed
                with self.assertRaises(RuntimeError):
                    with profile:
                        self.fail("only the original single ONLINE PRIMARY may reach the source test")
                self.assertEqual(len(self.starts(profile)), 1)
                profile.database.test.assert_not_called()
                profile.database.__exit__.assert_called_once()

    def test_initial_offline_empty_role_is_pending_until_the_same_native_member_is_online(self):
        profile = self.profile()
        original = profile.database.sql.side_effect
        observations = 0

        def joining(statement, **kwargs):
            nonlocal observations
            if "FROM performance_schema.replication_group_members" in statement:
                observations += 1
                row = self.member(profile)
                if observations == 1:
                    row[7:] = ["OFFLINE", "", ""]
                return (0, "\t".join(row) + "\n")
            return original(statement, **kwargs)

        profile.database.sql.side_effect = joining
        with patch("mysql_group_replication_fixture.time.sleep"):
            with profile:
                self.assertTrue(profile.ready)
                self.assertEqual(profile.online_observations, 2)
        self.assertEqual(len(self.starts(profile)), 1)
        profile.database.__exit__.assert_called_once()

    def test_duplicate_member_rows_are_not_collapsed_to_one_native_identity(self):
        profile = self.profile()
        original = profile.database.sql.side_effect

        def duplicate(statement, **kwargs):
            if "FROM performance_schema.replication_group_members" in statement:
                row = "\t".join(self.member(profile))
                return (0, row + "\n" + row + "\n")
            return original(statement, **kwargs)

        profile.database.sql.side_effect = duplicate
        with self.assertRaisesRegex(RuntimeError, "ambiguous"):
            with profile:
                self.fail("two native rows cannot prove a single-member group")
        profile.database.__exit__.assert_called_once()

    def test_bootstrap_timeout_or_cancellation_retires_same_owner_without_retry_or_stop(self):
        for error in (TimeoutError("native START did not complete"), KeyboardInterrupt()):
            with self.subTest(error=type(error).__name__):
                profile = self.profile()
                original = profile.database.sql.side_effect

                def interrupted(statement, **kwargs):
                    if "START GROUP_REPLICATION" in statement:
                        raise error
                    return original(statement, **kwargs)

                profile.database.sql.side_effect = interrupted
                with self.assertRaises(type(error)):
                    with profile:
                        self.fail("an unsettled START cannot reach source acceptance")
                self.assertEqual(profile.sql_dispatches, 2)
                self.assertEqual(len(self.starts(profile)), 1)
                self.assertFalse(any("STOP GROUP_REPLICATION" in call.args[0] for call in profile.database.sql.call_args_list))
                profile.database.__exit__.assert_called_once()

    def test_transitional_member_observations_are_bounded_without_retrying_start(self):
        profile = self.profile()
        original = profile.database.sql.side_effect

        def recovering(statement, **kwargs):
            if "FROM performance_schema.replication_group_members" in statement:
                row = self.member(profile)
                row[7] = "RECOVERING"
                return (0, "\t".join(row))
            return original(statement, **kwargs)

        profile.database.sql.side_effect = recovering
        with patch("mysql_group_replication_fixture.time.sleep"):
            with self.assertRaises(TimeoutError):
                with profile:
                    self.fail("a transitional state is not an ONLINE witness")
        self.assertEqual(profile.online_observations, profile.MAX_ONLINE_OBSERVATIONS)
        self.assertEqual(profile.sql_dispatches, profile.MAX_SQL_DISPATCHES)
        self.assertEqual(profile.sql_statements, profile.MAX_SQL_STATEMENTS)
        self.assertEqual(len(self.starts(profile)), 1)
        profile.database.__exit__.assert_called_once()

    def test_absolute_setup_deadline_prevents_new_sql_instead_of_renewing_allowance(self):
        profile = self.profile()
        with patch("mysql_group_replication_fixture.time.monotonic", side_effect=[0, 61]):
            with self.assertRaises(TimeoutError):
                with profile:
                    self.fail("an exhausted setup cannot begin native SQL")
        self.assertEqual(profile.sql_dispatches, 0)
        profile.database.sql.assert_not_called()
        profile.database.__exit__.assert_called_once()

    def test_test_body_cancellation_retires_the_existing_database_without_cleanup_sql(self):
        profile = self.profile()
        profile.database.test.side_effect = KeyboardInterrupt
        with self.assertRaises(KeyboardInterrupt):
            with profile:
                profile.verify()
        self.assertEqual(profile.sql_dispatches, 3)
        profile.database.__exit__.assert_called_once()

    def test_setup_is_consumed_once_and_cannot_be_entered_again_after_retirement(self):
        profile = self.profile()
        with profile:
            pass
        with self.assertRaisesRegex(RuntimeError, "entered twice"):
            with profile:
                self.fail("a retired fixture is not a second bootstrap")
        profile.database.__enter__.assert_called_once()
        self.assertEqual(len(self.starts(profile)), 1)

    def test_unqualified_setup_cannot_export_expected_member_environment(self):
        profile = self.profile()
        with self.assertRaisesRegex(RuntimeError, "completed original"):
            profile.environment()
        profile.database.sql.assert_not_called()

    def test_malformed_or_zero_native_uuid_and_unverified_image_are_refused(self):
        for value in ("not-a-uuid", "0" * 32, "00000000-0000-0000-0000-000000000000"):
            with self.subTest(value=value):
                with self.assertRaises(RuntimeError):
                    MySqlGroupReplication.native_uuid(value)
        profile = self.profile()
        profile.database.commands.run.return_value = (0, "mysql:8.4")
        with self.assertRaisesRegex(RuntimeError, "image identity"):
            with profile:
                self.fail("a mutable image tag is not its observed immutable image identity")
        profile.database.sql.assert_not_called()
        profile.database.__exit__.assert_called_once()


if __name__ == "__main__":
    unittest.main()
