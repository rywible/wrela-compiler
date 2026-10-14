"""The one check runs all retained tests or exactly one, and fails closed on bad selection."""
import contextlib
import io
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import check
import runner


class Selection(unittest.TestCase):
    def test_only_zero_or_one_name_is_valid(self):
        self.assertIsNone(check.selection([]))
        self.assertEqual(check.selection(['a::test']), 'a::test')
        for args in (['a', 'b'], ['--long'], ['--full'], ['--fresh'], ['--fmt'],
                     ['--package=x'], ['--nocapture'], ['']):
            with self.subTest(args=args), self.assertRaises(ValueError):
                check.selection(args)

    def test_complete_suite_runs_every_step_and_program_without_memo(self):
        with tempfile.TemporaryDirectory() as temp, \
                patch.object(check, 'ROOT', Path(temp)), \
                patch.object(check, 'build_tests', return_value=['one', 'two']), \
                patch.object(check, 'call'), \
                patch.object(check, 'listed', return_value=['test']), \
                patch.object(check, 'step') as step, \
                patch.object(check, 'rust_run', return_value=1) as run, \
                patch.dict(os.environ, {'WRELA_BLESS': '1', 'WRELA_RUN_ONLY': 'missing',
                                        'WRELA_CONST_CACHE': '0'}), \
                contextlib.redirect_stdout(io.StringIO()):
            cwd = Path.cwd()
            try:
                self.assertEqual(check.main([]), 0)
                self.assertEqual([c.args[0] for c in step.call_args_list], list(check.STEPS))
                self.assertEqual([c.args[0] for c in run.call_args_list], ['one', 'two'])
                self.assertFalse(set(check.CONTROLS) & os.environ.keys())
            finally:
                os.chdir(cwd)

    def test_wrela_checks_rebuild_the_normal_cli_before_using_it(self):
        original = Path.cwd()
        try:
            for arguments in ([], ['wrela::engine::alpha']):
                with self.subTest(arguments=arguments), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary).resolve()
                    target = root / 'target'
                    executable = target / 'release/wrela'
                    executable.parent.mkdir(parents=True)
                    executable.write_text('stale CLI')

                    def build(command, **kwargs):
                        self.assertEqual(command, ['cargo', 'build', '-q', '--release', '-p', 'wrela', '--bin', 'wrela'])
                        executable.write_text('current CLI')

                    def selected(name, cli, logs):
                        self.assertEqual(cli, executable)
                        self.assertEqual(cli.read_text(), 'current CLI')

                    with patch.object(check, 'ROOT', root), \
                            patch.dict(os.environ, {'CARGO_TARGET_DIR': str(target)}), \
                            patch.object(check, 'build_tests', return_value=['one']), \
                            patch.object(check, 'call', side_effect=build) as build_cli, \
                            patch.object(check, 'step', side_effect=selected), \
                            patch.object(check, 'wrela_test', side_effect=selected), \
                            patch.object(check, 'listed', return_value=['test']), \
                            patch.object(check, 'rust_run', return_value=1), \
                            patch.object(check, 'ChromeSession', side_effect=lambda _: contextlib.nullcontext()), \
                            contextlib.redirect_stdout(io.StringIO()):
                        self.assertEqual(check.main(arguments), 0)
                        build_cli.assert_called_once()
        finally:
            os.chdir(original)


class RustChecks(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.root = Path(temp.name)
        self.pkg = self.root / 'compiler/tests'
        self.pkg.mkdir(parents=True)
        (self.pkg / 'Cargo.toml').write_text('[package]\nname = "wrela-fixture"\n')
        self.logs = self.root / 'logs'
        self.logs.mkdir()
        self.exe = self.root / 'fixture'
        self.exe.write_text(f'#!{sys.executable}\n' + '''
import json, os, pathlib, sys
names = ['alpha', 'alphabet', 'gpu']
if '--list' in sys.argv:
    print('\\n'.join(n + ': test' for n in names))
    sys.exit(0)
if '--worker' in sys.argv: sys.exit(0)
args = sys.argv[1:]
selected = [args[args.index('--exact') + 1]] if '--exact' in args else names
if '--include-ignored' not in args: selected.remove('gpu')
pathlib.Path(os.environ['CHECK_RECORD']).write_text(json.dumps({
    'selected': selected, 'args': args,
    'scratch': os.environ['WRELA_TEST_TMPDIR']}))
if os.environ.get('REPLACE_EXE'):
    p = pathlib.Path(os.environ['ORIGINAL_EXE']); p.unlink(); p.write_text('broken')
    import subprocess
    subprocess.run([sys.argv[0], '--worker'], check=True)
mode = os.environ.get('FAIL_MODE', '')
count = 0 if mode in ['zero', 'failure'] else len(selected)
print(f'test result: ok. {count} passed; 0 failed; {1 if mode == "skip" else 0} ignored; 0 measured; 0 filtered out')
if mode == 'failure': sys.exit(1)
''')
        self.exe.chmod(0o755)
        self.program = {'manifest_path': str(self.pkg / 'Cargo.toml'), 'executable': str(self.exe),
                        'target': {'name': 'suite', 'kind': ['test']}}
        env = patch.dict(os.environ, {'CHECK_RECORD': str(self.root / 'record'),
                                     'CARGO_TARGET_DIR': str(self.root / 'cargo')})
        env.start(); self.addCleanup(env.stop)
        scratch = patch.object(runner, 'scratch_root', return_value=str(self.root))
        scratch.start(); self.addCleanup(scratch.stop)

    def test_exact_selection_never_runs_a_substring_match(self):
        program, name = check.matching([self.program], 'alpha')
        with contextlib.redirect_stdout(io.StringIO()):
            self.assertEqual(check.rust_run(program, self.logs, name), 1)
        got = json.loads((self.root / 'record').read_text())
        self.assertEqual(got['selected'], ['alpha'])
        self.assertIn('--exact', got['args'])
        self.assertFalse(Path(got['scratch']).exists())

    def test_all_tests_include_gpu_and_never_have_skip_arguments(self):
        self.assertEqual(check.rust_run(self.program, self.logs), 3)
        got = json.loads((self.root / 'record').read_text())
        self.assertEqual(got['selected'], ['alpha', 'alphabet', 'gpu'])
        self.assertNotIn('--skip', got['args'])

    def test_unknown_and_ambiguous_names_fail_before_running(self):
        with self.assertRaises(ValueError): check.matching([self.program], 'alph')
        other = {**self.program, 'target': {'name': 'other', 'kind': ['test']}}
        with self.assertRaisesRegex(ValueError, '2 matches'):
            check.matching([self.program, other], 'alpha')
        self.assertEqual(check.matching([self.program, other], 'wrela-fixture::other::alpha'),
                         (other, 'alpha'))
        self.assertFalse((self.root / 'record').exists())

    def test_failure_zero_tests_and_skips_are_failures_and_keep_scratch(self):
        for mode in ['failure', 'zero', 'skip']:
            with patch.dict(os.environ, {'FAIL_MODE': mode}), \
                    contextlib.redirect_stdout(io.StringIO()), \
                    self.assertRaises(ValueError):
                check.rust_run(self.program, self.logs)
            got = json.loads((self.root / 'record').read_text())
            self.assertTrue(Path(got['scratch']).exists())

    def test_replaced_executable_can_still_spawn_its_worker(self):
        with patch.dict(os.environ, {'REPLACE_EXE': '1', 'ORIGINAL_EXE': str(self.exe)}):
            self.assertEqual(check.rust_run(self.program, self.logs), 3)


class WrelaSelection(unittest.TestCase):
    def test_exact_results_required_and_package_paths_confined(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'engine').mkdir()
            logs = root / 'logs'; logs.mkdir()
            for tests in [[], [{'name': 'alphabet', 'passed': True}],
                          [{'name': 'alpha', 'passed': True}] * 2,
                          [{'name': 'alpha', 'passed': False}]]:
                with patch.object(check, 'ROOT', root), \
                        patch.object(check, 'call', return_value=subprocess.CompletedProcess(
                            [], 0, json.dumps({'tests': tests}))), self.assertRaises(ValueError):
                    check.wrela_test('wrela::engine::alpha', Path('wrela'), logs)
            with patch.object(check, 'ROOT', root), patch.object(check, 'call') as run:
                for name in ['wrela::../::alpha', 'wrela::engine::', 'wrela::engine::alpha::extra']:
                    with self.assertRaises(ValueError): check.wrela_test(name, Path('wrela'), logs)
                run.assert_not_called()


class BrowserSelection(unittest.TestCase):
    def test_no_tests_and_multiple_tests_fail_exact_selection(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            directory = root / 'runtime/browser/test'
            directory.mkdir(parents=True)
            (directory / 'x.test.ts').write_text('')
            logs = root / 'logs'; logs.mkdir()
            for xml in ['<testsuites/>', '<testsuites><testcase/><testcase/></testsuites>']:
                (logs / 'browser.xml').write_text(xml)
                with patch.object(check, 'ROOT', root), patch.object(check, 'call'), \
                        self.assertRaises(ValueError):
                    check.browser_test('browser::x.test.ts::a', logs)
            with patch.object(check, 'ROOT', root), self.assertRaises(ValueError):
                check.browser_test('browser::../../outside::a', logs)


class BrowserPackages(unittest.TestCase):
    def test_browser_steps_install_the_locked_packages_first(self):
        for name, last in [('browser-types', ['bun', 'run', 'typecheck']),
                           ('browser-dist', ['bun', 'run', 'check-dist']),
                           ('browser-size', ['bun', 'run', 'size'])]:
            with self.subTest(name=name), patch.object(check, 'call') as run:
                check.step(name, Path('wrela'), Path('logs'))
                self.assertEqual([c.args[0] for c in run.call_args_list],
                                 [['bun', 'install', '--frozen-lockfile', '--silent'], last])
        with patch.object(check, 'call') as run, patch.object(check, 'browser_test') as test:
            check.step('browser', Path('wrela'), Path('logs'))
            self.assertEqual(run.call_args.args[0], ['bun', 'install', '--frozen-lockfile', '--silent'])
            test.assert_called_once_with(None, Path('logs'))


if __name__ == '__main__':
    unittest.main()
