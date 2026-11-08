import importlib.util
import io
import os
from pathlib import Path
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stdout
from unittest.mock import Mock, patch


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


class ChildProcesses(unittest.TestCase):
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
