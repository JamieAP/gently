"""Local metadata remains private without disturbing SQLite's POSIX locks."""
import importlib.util
import os
from pathlib import Path
import sqlite3
import stat
import tempfile
import unittest
from unittest import mock

spec = importlib.util.spec_from_file_location("collector_supervise", Path(__file__).with_name("collector-supervise.py"))
supervisor = importlib.util.module_from_spec(spec)
spec.loader.exec_module(supervisor)


class CollectorPermissionsTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.repo = Path(self.temp.name)
        (self.repo / 'worker').mkdir()

    def test_existing_d1_permissions_are_hardened_without_releasing_locks(self):
        state = self.repo / 'worker/.wrangler/state'
        state.mkdir(parents=True)
        path = state / 'local.sqlite'
        first = sqlite3.connect(path)
        second = sqlite3.connect(path, timeout=0)
        self.addCleanup(first.close)
        self.addCleanup(second.close)
        first.execute('CREATE TABLE fixture (value INTEGER)')
        first.commit()
        first.execute('BEGIN IMMEDIATE')
        os.chmod(path, 0o644)
        supervisor.private_collector_state(self.repo)
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(state.stat().st_mode), 0o700)
        with self.assertRaises(sqlite3.OperationalError):
            second.execute('INSERT INTO fixture VALUES (1)')

    def test_symlinked_state_is_refused_without_changing_target(self):
        target = self.repo / 'user-file'
        target.write_text('synthetic user data')
        target.chmod(0o644)
        state = self.repo / 'worker/.wrangler'
        state.mkdir()
        (state / 'alias').symlink_to(target)
        with self.assertRaises(RuntimeError):
            supervisor.private_collector_state(self.repo)
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o644)
        self.assertEqual(target.read_text(), 'synthetic user data')

    def test_hardlinked_state_is_refused_without_changing_target(self):
        target = self.repo / 'user-file'
        target.write_text('synthetic user data')
        target.chmod(0o644)
        state = self.repo / 'worker/.wrangler'
        state.mkdir()
        os.link(target, state / 'alias')
        with self.assertRaises(RuntimeError):
            supervisor.private_collector_state(self.repo)
        self.assertEqual(stat.S_IMODE(target.stat().st_mode), 0o644)

    def test_linux_uses_pinned_path_without_unsupported_nofollow_chmod(self):
        root = self.repo / 'worker/.wrangler'
        root.mkdir()
        metadata = root.stat()

        def chmod(path, mode, **kwargs):
            if kwargs.get('follow_symlinks') is False:
                raise NotImplementedError('synthetic old-glibc no-follow limitation')
            self.assertEqual(path, '/proc/self/fd/123')
            self.assertEqual(mode, 0o700)

        with mock.patch.object(supervisor.sys, 'platform', 'linux'), \
                mock.patch.object(supervisor.os, 'O_PATH', 0x200000, create=True), \
                mock.patch.object(supervisor.os, 'open', return_value=123) as opened, \
                mock.patch.object(supervisor.os, 'fstat', return_value=metadata), \
                mock.patch.object(supervisor.os, 'chmod', side_effect=chmod), \
                mock.patch.object(supervisor.os, 'close') as closed:
            supervisor.private_collector_state(self.repo)
        self.assertTrue(opened.call_args.args[1] & 0x200000)
        self.assertTrue(opened.call_args.args[1] & os.O_NOFOLLOW)
        closed.assert_called_once_with(123)


if __name__ == '__main__':
    unittest.main()
