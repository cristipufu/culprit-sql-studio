import importlib.util
import io
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr, redirect_stdout
from unittest.mock import Mock, patch


sys.path.insert(0, str(Path(__file__).parent))
from replication_fixtures import OwnedNetwork
spec = importlib.util.spec_from_file_location("smoke_providers", Path(__file__).with_name("smoke-providers.py"))
smoke = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = smoke
spec.loader.exec_module(smoke)


class DisposableOwnership(unittest.TestCase):
    def database(self, owner=None):
        database = smoke.Database(smoke.FIXTURES[0])
        database.commands = Mock()
        database.commands.run.return_value = (0, (owner or database.owner) + "\n")
        return database

    def test_cancelled_start_removes_only_its_owned_container_and_volumes(self):
        database = self.database()
        with patch.object(database, "start", side_effect=KeyboardInterrupt), redirect_stdout(io.StringIO()):
            with self.assertRaises(KeyboardInterrupt):
                with database:
                    self.fail("a cancelled start must not enter the context")
        removal = database.commands.run.call_args_list[-1].args[0]
        self.assertEqual(removal, ["docker", "rm", "--force", "--volumes", database.name])
        self.assertIn(smoke.OWNER_LABEL, database.commands.run.call_args_list[0].args[0][3])

    def test_cleanup_refuses_a_container_with_another_runs_label(self):
        database = self.database(owner="another-run")
        with self.assertRaisesRegex(RuntimeError, "ownership label"):
            database.close()
        self.assertEqual(database.commands.run.call_count, 1)

    def test_missing_container_needs_no_cleanup(self):
        database = self.database()
        database.commands.run.return_value = (1, "not found")
        database.close()
        self.assertEqual(database.commands.run.call_count, 1)

    def test_restart_refuses_foreign_or_missing_containers_before_mutation(self):
        for result in [(0, "another-owner\n"), (1, "missing")]:
            database = self.database()
            database.commands.run.return_value = result
            with self.assertRaises(RuntimeError):
                database.restart()
            self.assertEqual(database.commands.run.call_count, 1)

    def test_extension_setup_refuses_foreign_container_before_installing_packages(self):
        database = self.database()
        database.commands.run.side_effect = [(1, "missing control file"), (0, "another-owner\n")]
        with self.assertRaisesRegex(RuntimeError, "ownership label"):
            database.prepare_hypopg()
        self.assertFalse(any("apt-get" in call.args[0] for call in database.commands.run.call_args_list))

    def test_existing_extension_needs_no_package_changes_or_server_restart(self):
        database = self.database()
        database.sql = Mock(return_value=(0, ""))
        database.prepare_hypopg()
        self.assertEqual(database.commands.run.call_count, 1)
        database.sql.assert_called_once_with("CREATE DATABASE culprit_hypopg;", database="postgres")

    def test_installed_server_libraries_are_activated_before_extension_sql(self):
        database = self.database()
        events = []

        def command(args, **_):
            events.append(args)
            if args[1] == "inspect":
                return (0, database.owner)
            return (1, "") if "test" in args else (0, "")

        def sql(statement, **_):
            events.append(statement)
            return (0, "18.7")

        database.commands.run.side_effect = command
        database.wait_until_ready = Mock(side_effect=lambda: events.append("ready"))
        database.sql = Mock(side_effect=sql)
        output = io.StringIO()
        with redirect_stdout(output):
            database.prepare_hypopg()
        restart = events.index(["docker", "restart", database.name])
        installed = next(i for i, event in enumerate(events) if isinstance(event, list) and "install" in event)
        self.assertLess(installed, restart)
        self.assertLess(restart, events.index("ready"))
        self.assertLess(events.index("ready"), events.index("SHOW server_version;"))
        self.assertLess(events.index("SHOW server_version;"), events.index("CREATE DATABASE culprit_hypopg;"))
        self.assertIn("18.7", output.getvalue())

    def test_readiness_deadline_cannot_wait_forever_after_restart(self):
        database = self.database()
        database.commands.run.return_value = (0, "127.0.0.1:55439\n")
        database.sql = Mock(return_value=(1, "starting"))
        with patch.object(smoke.time, "monotonic", side_effect=[0, 1, smoke.STARTUP_BUDGET + 1]), patch.object(smoke.time, "sleep"):
            with self.assertRaisesRegex(TimeoutError, "did not become ready"):
                database.wait_until_ready()
        database.sql.assert_called_once_with("SELECT 1;", database="postgres", check=False, timeout=5)
        self.assertEqual(database.commands.run.call_args_list[-1].args[0], ["docker", "logs", "--tail", "60", database.name])


class ReplicationOwnership(unittest.TestCase):
    def test_cancelled_ag_failure_diagnostics_still_release_members_and_network(self):
        fixture = next(fixture for fixture in smoke.FIXTURES if fixture.engine == "mssql")
        primary = smoke.Database(smoke.replace(fixture, name="mssql-ag-primary"))
        secondary = smoke.Database(smoke.replace(fixture, name="mssql-ag-secondary"))
        events = []

        def command(args, **_):
            if args[:3] == ["docker", "network", "rm"]:
                events.append("network")
            return (0, primary.owner)

        primary.commands = Mock()
        primary.commands.run.side_effect = command
        for name, database in (("primary", primary), ("secondary", secondary)):
            database.start = Mock()
            database.close = Mock(side_effect=lambda name=name: events.append(name))
        topology = smoke.SqlServerReplication(primary, secondary, smoke.OWNER_LABEL)
        topology.require_cached_images = Mock()
        topology.verify_instance_name = Mock()
        topology.prepare_database = Mock(side_effect=RuntimeError("setup failed"))
        topology.report_failure = Mock(side_effect=KeyboardInterrupt)
        with self.assertRaises(KeyboardInterrupt):
            with topology:
                self.fail("failed setup must not expose a ready topology")
        self.assertEqual(events, ["secondary", "primary", "network"])

    def test_interrupted_network_creation_cleans_only_the_attested_network(self):
        commands = Mock()
        owner = "a" * 24
        commands.run.side_effect = [KeyboardInterrupt, (0, owner + "\n"), (0, "")]
        network = OwnedNetwork(commands, owner, smoke.OWNER_LABEL)
        with self.assertRaises(KeyboardInterrupt):
            with network:
                self.fail("interrupted creation cannot enter the context")
        self.assertEqual(commands.run.call_args_list[-1].args[0], ["docker", "network", "rm", network.name])

    def test_a_foreign_network_is_never_removed(self):
        commands = Mock()
        commands.run.return_value = (0, "another-owner\n")
        network = OwnedNetwork(commands, "a" * 24, smoke.OWNER_LABEL)
        with self.assertRaisesRegex(RuntimeError, "ownership label"):
            network.close()
        self.assertEqual(commands.run.call_count, 1)

    def test_cancelled_standby_start_releases_standby_primary_then_network(self):
        fixture = smoke.Fixture("pair", "postgres:18.6", "postgres", 5432, "culprit_replication", "postgres")
        primary, standby = smoke.Database(fixture), smoke.Database(fixture)
        events = []

        def command(args, **_):
            if args[:3] == ["docker", "network", "rm"]:
                events.append("network")
            return (0, primary.owner)

        primary.commands = Mock()
        primary.commands.run.side_effect = command
        primary.sql = Mock()
        primary.start = Mock()
        primary.close = Mock(side_effect=lambda: events.append("primary"))
        primary.owns_container = Mock(return_value=True)
        standby.start = Mock(side_effect=KeyboardInterrupt)
        standby.close = Mock(side_effect=lambda: events.append("standby"))
        topology = smoke.PostgresReplication(primary, standby, smoke.OWNER_LABEL)
        with self.assertRaises(KeyboardInterrupt):
            with topology:
                self.fail("a failed standby cannot expose a ready topology")
        self.assertEqual(events, ["standby", "primary", "network"])


class TimescaleJobsOwnership(unittest.TestCase):
    def database(self):
        fixture = next(fixture for fixture in smoke.FIXTURES if fixture.name == "timescale")
        database = smoke.Database(fixture)
        database.commands = Mock()
        database.commands.run.return_value = (0, database.owner + "\n")
        database.sql = Mock(return_value=(0, ""))
        return database

    def test_only_dedicated_timescale_and_plain_postgres_start_with_the_exact_owned_nonce(self):
        for fixture in smoke.FIXTURES:
            database = smoke.Database(fixture)
            environment, command = database.configuration()
            markers = [argument for argument in command if argument.startswith("cluster_name=")]
            expected = [f"cluster_name=culprit_owned_{database.owner}"] if fixture.name in ("timescale", "postgres") else []
            with self.subTest(fixture=fixture.name):
                self.assertEqual(markers, expected)
                if fixture.engine == "postgres":
                    self.assertEqual(environment["POSTGRES_DB"], fixture.database)
                    self.assertIn("track_io_timing=on", command)
                if fixture.name == "timescale":
                    self.assertIn("shared_preload_libraries=timescaledb,pg_stat_statements", command)

    def test_owned_setup_runs_the_exact_fixture_script_without_new_variables_or_credentials(self):
        database = self.database()
        script = "-- exact bounded native job fixture setup\n"
        with patch.object(smoke.Path, "read_text", autospec=True, return_value=script) as read:
            database.prepare_timescale_jobs()
        self.assertEqual(read.call_args.args[0], smoke.ROOT / "dev" / "timescale" / "jobs.sql")
        database.sql.assert_called_once_with(script)
        self.assertEqual(database.commands.run.call_count, 1)
        inspection = database.commands.run.call_args.args[0]
        self.assertEqual(inspection[:2], ["docker", "inspect"])
        self.assertEqual(inspection[-1], database.name)
        self.assertIn(smoke.OWNER_LABEL, inspection[3])

    def test_missing_or_foreign_container_refuses_setup_before_reading_or_running_sql(self):
        for result in [(1, "missing"), (0, "another-owner\n")]:
            database = self.database()
            database.commands.run.return_value = result
            with self.subTest(result=result), patch.object(smoke.Path, "read_text") as read:
                with self.assertRaises(RuntimeError):
                    database.prepare_timescale_jobs()
                read.assert_not_called()
                database.sql.assert_not_called()
                self.assertEqual(database.commands.run.call_count, 1)

    def test_an_invalid_nonce_or_other_fixture_cannot_reach_job_setup(self):
        invalid_nonce = self.database()
        invalid_nonce.owner = "not-a-native-owner;SELECT 1"
        other_fixture = smoke.Database(smoke.FIXTURES[0])
        other_fixture.commands = Mock()
        other_fixture.sql = Mock()
        for database in (invalid_nonce, other_fixture):
            with self.subTest(fixture=database.fixture.name), patch.object(smoke.Path, "read_text") as read:
                with self.assertRaises(RuntimeError):
                    database.prepare_timescale_jobs()
                read.assert_not_called()
                database.sql.assert_not_called()
                database.commands.run.assert_not_called()

    def test_timescale_seed_creates_the_limited_role_before_owned_job_setup(self):
        database = self.database()
        scripts = {"seed.sql": "-- base Timescale seed", "workload.sql": "-- base Timescale workload",
                   "jobs.sql": "-- bounded owned jobs"}
        with patch.object(smoke.Path, "read_text", autospec=True,
                          side_effect=lambda path: scripts[path.name]):
            database.seed()
        statements = [call.args[0] for call in database.sql.call_args_list]
        reader = next(index for index, statement in enumerate(statements) if "CREATE ROLE smoke_reader" in statement)
        jobs = statements.index(scripts["jobs.sql"])
        self.assertGreater(jobs, reader)
        self.assertEqual(statements.count(scripts["jobs.sql"]), 1)
        self.assertEqual(statements[:2], [scripts["seed.sql"], scripts["workload.sql"]])

    def test_cancelled_job_setup_removes_only_the_same_owned_container_and_volumes(self):
        database = self.database()
        database.start = Mock()
        database.sql.side_effect = KeyboardInterrupt
        with patch.object(smoke.Path, "read_text", return_value="-- owned jobs"), redirect_stdout(io.StringIO()):
            with self.assertRaises(KeyboardInterrupt):
                with database:
                    database.prepare_timescale_jobs()
        database.sql.assert_called_once()
        removal = database.commands.run.call_args_list[-1].args[0]
        self.assertEqual(removal, ["docker", "rm", "--force", "--volumes", database.name])
        inspections = [call for call in database.commands.run.call_args_list if call.args[0][:2] == ["docker", "inspect"]]
        self.assertEqual(len(inspections), 2)


class MssqlFixtureIdentity(unittest.TestCase):
    def ordinary_fixture(self):
        return next(fixture for fixture in smoke.FIXTURES if fixture.name == "mssql")

    def test_common_and_agent_use_a_supported_machine_with_full_native_database_and_docker_nonces(self):
        original = self.ordinary_fixture()
        owner = "a" * 24
        for fixture_type in (smoke.Database, smoke.AgentDatabase):
            with self.subTest(fixture_type=fixture_type.__name__):
                with patch.object(smoke.secrets, "token_hex", side_effect=[owner, "b" * 32, "c" * 32]):
                    database = fixture_type(original)
                self.assertEqual(database.hostname, "m" + owner[:14])
                self.assertEqual(len(database.hostname), 15)
                self.assertEqual(database.fixture.database, "culprit_smoke_" + owner)
                self.assertTrue(database.name.endswith(owner))
                self.assertEqual(database.owner, owner)
                self.assertEqual(database.test_environment()["CULPRIT_SMOKE_DATABASE"], database.fixture.database)
                self.assertEqual(database.test_environment()["CULPRIT_SMOKE_OWNER"], owner)
                self.assertEqual(original.database, "culprit_smoke", "the immutable fixture recipe is not rewritten")

    def test_other_provider_fixture_names_and_native_databases_are_unchanged(self):
        for fixture in smoke.FIXTURES:
            if fixture.engine == "mssql":
                continue
            database = smoke.Database(fixture)
            self.assertEqual(database.fixture, fixture)
            self.assertEqual(database.hostname, f"culprit-smoke-{fixture.engine}-{database.owner}")

    def test_native_machine_limit_keeps_the_full_owner_label_and_exact_cleanup_target(self):
        database = smoke.Database(self.ordinary_fixture())
        database.commands = Mock()
        database.commands.run.side_effect = lambda args, **_: (0, database.owner if args[:2] == ["docker", "inspect"] else "")
        database.wait_until_ready = Mock()
        with redirect_stdout(io.StringIO()):
            with database:
                self.assertEqual(database.commands.run.call_count, 3)
        commands = [call.args[0] for call in database.commands.run.call_args_list]
        create = next(command for command in commands if command[:2] == ["docker", "create"])
        self.assertEqual(create[create.index("--hostname") + 1], database.hostname)
        self.assertEqual(create[create.index("--label") + 1], f"{smoke.OWNER_LABEL}={database.owner}")
        self.assertEqual(create[create.index("--name") + 1], database.name)
        self.assertEqual(commands[-1], ["docker", "rm", "--force", "--volumes", database.name])

    def test_missing_or_foreign_mssql_ownership_still_refuses_removal(self):
        for status, actual in ((1, "missing"), (0, "foreign-full-owner")):
            database = smoke.Database(self.ordinary_fixture())
            database.commands = Mock()
            database.commands.run.return_value = (status, actual)
            if status:
                database.close()
            else:
                with self.assertRaisesRegex(RuntimeError, "ownership label"):
                    database.close()
            self.assertEqual(database.commands.run.call_count, 1)

    def test_ag_constructor_requires_its_exact_common_nonce_database_before_any_setup(self):
        database = smoke.Database(self.ordinary_fixture())
        original = database.fixture
        foreign_owner = ("a" if database.owner[0] != "a" else "b") + database.owner[1:]
        for wrong_name in ("culprit_smoke", "culprit_smoke_" + database.owner[:14], "culprit_smoke_" + foreign_owner):
            database.fixture = smoke.replace(original, database=wrong_name)
            with self.subTest(wrong_name=wrong_name), self.assertRaisesRegex(ValueError, "unstarted owned"):
                smoke.SqlServerReplication(database, smoke.Database(self.ordinary_fixture()), smoke.OWNER_LABEL)
        database.fixture = original

    def test_existing_ag_members_keep_their_own_short_machines_and_both_full_database_nonces(self):
        primary = smoke.Database(smoke.replace(self.ordinary_fixture(), name="mssql-ag-primary"))
        secondary = smoke.Database(smoke.replace(self.ordinary_fixture(), name="mssql-ag-secondary"))
        topology = smoke.SqlServerReplication(primary, secondary, smoke.OWNER_LABEL)
        self.assertEqual(primary.hostname, "p" + primary.owner[:14])
        self.assertEqual(secondary.hostname, "s" + secondary.owner[:14])
        expected = f"culprit_smoke_{primary.owner}_{secondary.owner}"
        self.assertEqual(topology.database_name, expected)
        self.assertEqual(primary.fixture.database, expected)
        self.assertEqual(secondary.fixture.database, expected)
        self.assertEqual(topology.name, "culprit_ag_" + primary.owner)


class NativeSelection(unittest.TestCase):
    def test_agent_launch_is_explicit_and_its_tests_are_excluded_from_plain_mssql(self):
        fixture = next(fixture for fixture in smoke.FIXTURES if fixture.name == "mssql")
        plain = smoke.Database(fixture)
        agent = smoke.AgentDatabase(smoke.replace(fixture, name="mssql-agent"))
        self.assertNotIn("MSSQL_AGENT_ENABLED", plain.configuration()[0])
        self.assertEqual(agent.configuration()[0]["MSSQL_AGENT_ENABLED"], "true")
        self.assertEqual(agent.configuration()[0]["MSSQL_SA_PASSWORD"], agent.password)
        plain.test_environment = Mock(return_value={})
        strict_holder = "an_owned_native_page_task_holder_requires_the_reported_pointer_and_exact_current_request"
        for selection, expected in [
            ("native_tests::", ["telemetry::replication::native_tests::", "jobs::", strict_holder]),
            ("jobs::native_tests::", ["telemetry::replication::native_tests::", strict_holder]),
        ]:
            with self.subTest(selection=selection), patch.object(smoke, "run_tests") as run_tests:
                plain.test("culprit-mssql", selection)
                self.assertEqual(run_tests.call_args.kwargs["skip"], expected)

    def test_mssql_broad_selection_runs_compatibility_once_and_exact_selection_preserves_strict_qualification(self):
        database = smoke.Database(next(fixture for fixture in smoke.FIXTURES if fixture.name == "mssql"))
        database.test_environment = Mock(return_value={})
        prefix = "activity::page_targets::native_tests::holders::"
        strict = "an_owned_native_page_task_holder_requires_the_reported_pointer_and_exact_current_request"
        compatibility = "owned_native_page_tasks_preserve_original_receipts_and_truthful_holder_refusals"
        native_tests = [prefix + strict, prefix + compatibility]
        for selection, expected in [
            ("", [prefix + compatibility]),
            ("native_tests::", [prefix + compatibility]),
            (prefix, [prefix + compatibility]),
            (prefix + compatibility, [prefix + compatibility]),
            (strict, [prefix + strict]),
            (prefix + strict, [prefix + strict]),
        ]:
            with self.subTest(selection=selection), patch.object(smoke, "run_tests") as run_tests:
                database.test("culprit-mssql", selection)
                run_tests.assert_called_once()
                skips = run_tests.call_args.kwargs["skip"]
                selected = [test for test in native_tests if selection in test and not any(skip in test for skip in skips)]
                self.assertEqual(selected, expected)
        database.test = Mock()
        database.verify()
        ordinary = [call for call in database.test.call_args_list if call.args == ("culprit-mssql", "native_tests::")]
        self.assertEqual(len(ordinary), 1, "default verification owns one broad native run")

    def test_broad_selection_excludes_optional_topologies_without_their_environment(self):
        database = smoke.Database(smoke.FIXTURES[1])
        database.test_environment = Mock(return_value={})
        for selection in ("telemetry::native_tests::", "native_tests::"):
            with self.subTest(selection=selection), patch.object(smoke, "run_tests") as run_tests:
                database.test("culprit-mysql", selection)
                self.assertEqual(run_tests.call_args.kwargs["skip"], [
                    "telemetry::native_tests::replication::",
                    "telemetry::native_tests::replication_movement::",
                ])

    def test_explicit_topology_selection_runs_only_that_topology(self):
        database = smoke.Database(smoke.FIXTURES[1])
        database.test_environment = Mock(return_value={})
        with patch.object(smoke, "run_tests") as run_tests:
            database.test("culprit-mysql", "telemetry::native_tests::replication_movement::")
        self.assertEqual(run_tests.call_args.kwargs["skip"], ["telemetry::native_tests::replication::"])

    def test_generic_postgres_selections_skip_optional_native_fixtures(self):
        for name in ("postgres", "timescale"):
            database = smoke.Database(next(fixture for fixture in smoke.FIXTURES if fixture.name == name))
            database.test_environment = Mock(return_value={})
            for selection in ("telemetry::native_tests::", "native_tests::", ""):
                with self.subTest(fixture=name, selection=selection), patch.object(smoke, "run_tests") as run_tests:
                    database.test("culprit-postgres", selection)
                    self.assertEqual(run_tests.call_args.kwargs["skip"], [
                        "telemetry::native_tests::replication::", "jobs::", "maintenance_waits::",
                    ])

    def test_explicit_short_and_qualified_jobs_filters_run_without_unrelated_optional_fixtures(self):
        database = smoke.Database(next(fixture for fixture in smoke.FIXTURES if fixture.name == "timescale"))
        database.test_environment = Mock(return_value={})
        for selection in ("jobs::compatibility::", "telemetry::native_tests::jobs::limits::"):
            with self.subTest(selection=selection), patch.object(smoke, "run_tests") as run_tests:
                database.test("culprit-postgres", selection)
                self.assertEqual(run_tests.call_args.kwargs["skip"], [
                    "telemetry::native_tests::replication::", "maintenance_waits::",
                ])

    def test_explicit_maintenance_filters_do_not_start_unrelated_optional_fixtures(self):
        database = smoke.Database(next(fixture for fixture in smoke.FIXTURES if fixture.name == "postgres"))
        database.test_environment = Mock(return_value={})
        for selection in ("maintenance_waits::", "telemetry::native_tests::maintenance_waits::"):
            with self.subTest(selection=selection), patch.object(smoke, "run_tests") as run_tests:
                database.test("culprit-postgres", selection)
                self.assertEqual(run_tests.call_args.kwargs["skip"], [
                    "telemetry::native_tests::replication::", "jobs::",
                ])

    def test_default_verification_keeps_installed_and_absent_job_cases_on_their_own_fixtures(self):
        for fixture in smoke.FIXTURES:
            database = smoke.Database(fixture)
            database.test = Mock()
            database.verify()
            selections = [call.args[1] for call in database.test.call_args_list if "jobs::" in call.args[1]]
            expected = {
                "timescale": ["telemetry::native_tests::jobs::compatibility::", "telemetry::native_tests::jobs::limits::"],
                "postgres": ["telemetry::native_tests::jobs::absent::"],
            }.get(fixture.name, [])
            with self.subTest(fixture=fixture.name):
                self.assertEqual(selections, expected)

    def test_explicit_fixture_selection_does_not_add_a_second_default_jobs_run(self):
        database = smoke.Database(next(fixture for fixture in smoke.FIXTURES if fixture.name == "timescale"))
        database.test = Mock()
        database.verify([("culprit-postgres", "jobs::limits::")])
        database.test.assert_called_once_with("culprit-postgres", "jobs::limits::", library=True, skip=None)


    def test_mssql_default_verification_runs_the_exact_owned_history_route_once_on_the_same_fixture(self):
        database = smoke.Database(next(fixture for fixture in smoke.FIXTURES if fixture.name == "mssql"))
        database.test = Mock()
        history = "telemetry::native_history::owned_boxed_history_keeps_native_acquisition_separate_from_data_identity_and_replays_canonically"
        self.assertNotIn("native_tests::", history)
        database.verify()
        selections = [call.args for call in database.test.call_args_list]
        self.assertEqual(selections.count(("culprit-mssql", "native_tests::")), 1)
        self.assertEqual(selections.count(("culprit-mssql", history)), 1)
        self.assertEqual(selections[-2:], [("culprit-mssql", "native_tests::"), ("culprit-mssql", history)])
        database.test.reset_mock()
        database.verify([("culprit-mssql", history)])
        database.test.assert_called_once_with("culprit-mssql", history, library=True, skip=None)
        database.test.reset_mock()
        database.verify([("culprit-mssql", "native_tests::")])
        database.test.assert_called_once_with("culprit-mssql", "native_tests::", library=True, skip=None)

    def test_agent_metadata_success_cannot_hide_a_failed_history_selection(self):
        database = smoke.AgentDatabase(smoke.replace(
            next(fixture for fixture in smoke.FIXTURES if fixture.name == "mssql"), name="mssql-agent"))
        metadata = "jobs::native_tests::owned_agent_metadata_source_roundtrips_without_creating_or_executing_jobs"
        history = "jobs::native_tests::owned_agent_history_keeps_real_failure_success_and_current_definition_separate_on_the_same_reader"
        database.test = Mock(side_effect=[None, RuntimeError("controlled incomplete Agent execution")])
        with self.assertRaisesRegex(RuntimeError, "incomplete Agent execution"):
            database.verify([("culprit-mssql", metadata), ("culprit-mssql", history)])
        self.assertEqual([call.args[1] for call in database.test.call_args_list], [metadata, history])


class ChildProcesses(unittest.TestCase):
    def test_sql_diagnostics_remain_visible_without_corrupting_native_rows(self):
        diagnostics = io.StringIO()
        with redirect_stderr(diagnostics):
            status, output = smoke.Commands([]).run(
                [sys.executable, "-c", "import sys; print('11.8.9-MariaDB'); print('client warning', file=sys.stderr)"],
                separate_stderr=True)
        self.assertEqual(status, 0)
        self.assertEqual(output, "11.8.9-MariaDB\n")
        self.assertIn("client warning", diagnostics.getvalue())

    def test_separate_failure_diagnostics_are_preserved_and_redacted(self):
        password = "simulated-test-password"
        with redirect_stdout(io.StringIO()), self.assertRaises(RuntimeError):
            status, output = smoke.Commands([password]).run(
                [sys.executable, "-c", f"import sys; print({password!r}, file=sys.stderr); raise SystemExit(7)"],
                separate_stderr=True)
        diagnostics = io.StringIO()
        with redirect_stdout(diagnostics):
            status, output = smoke.Commands([password]).run(
                [sys.executable, "-c", f"import sys; print({password!r}, file=sys.stderr); raise SystemExit(7)"],
                separate_stderr=True, check=False)
        self.assertEqual(status, 7)
        self.assertEqual(output, "[redacted]\n")
        self.assertNotIn(password, diagnostics.getvalue())

    def test_failures_never_print_disposable_passwords(self):
        password = "simulated-test-password"
        output = io.StringIO()
        with redirect_stdout(output), self.assertRaises(RuntimeError):
            smoke.Commands([password]).run([sys.executable, "-c", f"print({password!r}); raise SystemExit(7)"])
        self.assertNotIn(password, output.getvalue())
        self.assertIn("[redacted]", output.getvalue())

    @unittest.skipUnless(os.name == "posix", "the Docker runner uses POSIX process groups")
    def test_deadline_terminates_and_reaps_its_process_and_child(self):
        with tempfile.TemporaryDirectory() as directory:
            pids = Path(directory) / "pids"
            program = """
import os, signal, subprocess, sys, time
child = subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(60)'])
def stopped(*_):
    child.wait(timeout=2)
    raise SystemExit(0)
signal.signal(signal.SIGTERM, stopped)
with open(sys.argv[1], 'w') as output:
    output.write(f'{os.getpid()} {child.pid}')
time.sleep(60)
"""
            launch = smoke.subprocess.Popen

            def ready_process(*args, **kwargs):
                process = launch(*args, **kwargs)
                deadline = time.monotonic() + 30
                while not pids.exists():
                    if process.poll() is not None or time.monotonic() >= deadline:
                        smoke.Commands.stop(process)
                        self.fail("the child fixture did not start")
                    time.sleep(0.02)
                return process

            # The assertion exercises cancellation after child ownership is established, independently of OS startup.
            with patch.object(smoke.subprocess, "Popen", side_effect=ready_process):
                with redirect_stdout(io.StringIO()), self.assertRaises(TimeoutError):
                    smoke.Commands([]).run([sys.executable, "-c", program, str(pids)], timeout=0.1)
            for pid in map(int, pids.read_text().split()):
                with self.assertRaises(ProcessLookupError):
                    os.kill(pid, 0)


    @unittest.skipUnless(os.name == "posix", "the existing supervisor uses POSIX process groups")
    def test_outer_termination_reaps_active_work_before_exact_owned_agent_cleanup(self):
        for foreign in (False, True):
            with self.subTest(foreign=foreign), tempfile.TemporaryDirectory() as directory:
                ready = Path(directory) / "ready"
                events = Path(directory) / "events"
                program = r"""
import importlib.util, os, signal, sys
from pathlib import Path
sys.path.insert(0, str(Path(sys.argv[1]).parent))
spec = importlib.util.spec_from_file_location('owned_agent_supervisor', sys.argv[1])
smoke = importlib.util.module_from_spec(spec)
sys.modules[spec.name] = smoke
spec.loader.exec_module(smoke)
ready, events = Path(sys.argv[2]), Path(sys.argv[3])
foreign = sys.argv[4] == 'foreign'
def event(value):
    with events.open('a') as output:
        output.write(value + '\n')
fixture = smoke.replace(next(value for value in smoke.FIXTURES if value.name == 'mssql'), name='mssql-agent')
database = smoke.AgentDatabase(fixture)
database.start = lambda: None
class DockerCommands:
    def run(self, args, **kwargs):
        if args[:2] == ['docker', 'inspect']:
            event('inspect-exact-name' if args[-1] == database.name else 'wrong-name')
            return 0, 'foreign' if foreign else database.owner
        if args == ['docker', 'unpause', database.name]:
            event('unpause-exact-name')
            return 0, ''
        if args == ['docker', 'rm', '--force', '--volumes', database.name]:
            event('remove-exact-name-and-volumes')
            return 0, ''
        raise RuntimeError('unexpected Docker operation')
database.commands = DockerCommands()
signal.signal(signal.SIGTERM, smoke.interrupted)
worker = "import os, signal, sys, time; from pathlib import Path; signal.signal(signal.SIGTERM, lambda *_: sys.exit(0)); Path(sys.argv[1]).write_text(str(os.getpid())); time.sleep(60)"
try:
    with database:
        try:
            smoke.Commands([]).run([sys.executable, '-c', worker, str(ready)])
        finally:
            event('active-worker-settled')
except KeyboardInterrupt:
    event('supervisor-interrupted-after-cleanup')
    raise SystemExit(124)
except RuntimeError:
    event('foreign-owner-refused')
    raise SystemExit(125)
"""
                supervisor = smoke.subprocess.Popen(
                    [sys.executable, "-c", program, str(Path(smoke.__file__).resolve()),
                     str(ready), str(events), "foreign" if foreign else "owned"],
                    stdout=smoke.subprocess.PIPE, stderr=smoke.subprocess.PIPE, text=True,
                    start_new_session=True)
                try:
                    deadline = time.monotonic() + 30
                    while not ready.exists():
                        if supervisor.poll() is not None or time.monotonic() >= deadline:
                            self.fail("the supervised active worker did not establish ownership")
                        time.sleep(0.02)
                    supervisor.send_signal(smoke.signal.SIGTERM)
                    output, diagnostics = supervisor.communicate(timeout=10)
                    self.assertEqual(supervisor.returncode, 125 if foreign else 124, diagnostics)
                    self.assertNotIn("Traceback", diagnostics)
                    expected = ["active-worker-settled", "inspect-exact-name"]
                    expected += (["foreign-owner-refused"] if foreign else [
                        "unpause-exact-name", "remove-exact-name-and-volumes", "supervisor-interrupted-after-cleanup"])
                    self.assertEqual(events.read_text().splitlines(), expected)
                    with self.assertRaises(ProcessLookupError):
                        os.kill(int(ready.read_text()), 0)
                    self.assertEqual(output.count("Removed owned mssql-agent"), 0 if foreign else 1)
                finally:
                    smoke.Commands.stop(supervisor)
                    for stream in (supervisor.stdout, supervisor.stderr):
                        stream.close()


class ClusterOwnership(unittest.TestCase):
    def cluster(self):
        cluster = smoke.YugabyteCluster()
        self.addCleanup(cluster.directory.cleanup)
        cluster.config.write_text("{}")
        cluster.commands = Mock()
        return cluster

    def test_cleanup_refuses_foreign_container_before_compose_down(self):
        cluster = self.cluster()
        cluster.commands.run.side_effect = [(0, "foreign-container\n"), (0, "another-owner\n")]
        with self.assertRaisesRegex(RuntimeError, "ownership label"):
            cluster.close()
        self.assertEqual(cluster.commands.run.call_count, 2)

    def test_interrupted_start_resumes_and_removes_its_own_cluster(self):
        cluster = self.cluster()

        def command(args, **_):
            if args[:2] == ["docker", "ps"]:
                return (0, "owned-container\n")
            if args[:2] == ["docker", "inspect"]:
                return (0, cluster.owner + "\n")
            return (0, "")

        cluster.commands.run.side_effect = command
        with patch.object(cluster, "start", side_effect=KeyboardInterrupt), redirect_stdout(io.StringIO()):
            with self.assertRaises(KeyboardInterrupt):
                with cluster:
                    self.fail("an interrupted start must not enter the fixture")
        calls = [call.args[0] for call in cluster.commands.run.call_args_list]
        self.assertIn(["docker", "unpause", "owned-container"], calls)
        self.assertEqual(calls[-1], cluster.compose("down", "--volumes", "--remove-orphans"))


class TestSelection(unittest.TestCase):
    def test_a_successful_command_with_zero_cases_is_not_validation(self):
        commands = Mock()
        commands.run.return_value = (0, "test result: ok. 0 passed; 0 failed; 4 filtered out")
        with redirect_stdout(io.StringIO()), self.assertRaisesRegex(RuntimeError, "did not execute any"):
            smoke.run_tests(commands, "culprit-yugabyte", "missing-case", {})


if __name__ == "__main__":
    unittest.main()
