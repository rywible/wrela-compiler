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
    def test_a_step_and_a_program_never_share_a_log(self):
        self.assertNotEqual(check.log_name("browser-types"), check.log_name("browser::types"))
        self.assertEqual(check.log_name("wrela-tests::suite"), "wrela-tests__suite")

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
                patch.object(check, 'program_name', side_effect=str), \
                patch.object(check, 'call'), \
                patch.object(check, 'listed', return_value=['test']), \
                patch.object(check, 'step') as step, \
                patch.object(check, 'rust_run', return_value=1) as run, \
                patch.dict(os.environ, {'WRELA_BLESS': '1', 'WRELA_RUN_ONLY': 'missing',
                                        'WRELA_CONST_CACHE': '0',
                                        'WRELA_VALIDATOR': '/elsewhere/wrela'}), \
                contextlib.redirect_stdout(io.StringIO()):
            cwd = Path.cwd()
            try:
                self.assertEqual(check.main([]), 0)
                self.assertEqual([c.args[0] for c in step.call_args_list], list(check.STEPS))
                self.assertEqual([c.args[0] for c in run.call_args_list], ['one', 'two'])
                self.assertFalse(set(check.CONTROLS) & os.environ.keys())
                # The agent test's references are checked with the compiler built here.
                self.assertNotIn('WRELA_VALIDATOR', os.environ)
            finally:
                os.chdir(cwd)

    def test_a_failure_neither_stops_nor_hides_the_other_checks(self):
        def failing_step(name, *args):
            if name in ('clippy', 'rustfmt', 'browser'):
                raise subprocess.CalledProcessError(1, [name], 'step output for ' + name)

        def failing_run(program, out):
            if program == 'one':
                out.write('program output for one')
                raise ValueError('one failed')
            return 2
        output = io.StringIO()
        with tempfile.TemporaryDirectory() as temp, \
                patch.object(check, 'ROOT', Path(temp)), \
                patch.object(check, 'build_tests', return_value=['one', 'two', 'three']), \
                patch.object(check, 'program_name', side_effect=str), \
                patch.object(check, 'call'), \
                patch.object(check, 'listed', return_value=['test']), \
                patch.object(check, 'step', side_effect=failing_step) as step, \
                patch.object(check, 'rust_run', side_effect=failing_run) as run, \
                contextlib.redirect_stdout(output):
            cwd = Path.cwd()
            try:
                self.assertEqual(check.main([]), 1)
            finally:
                os.chdir(cwd)
        self.assertEqual([c.args[0] for c in step.call_args_list], list(check.STEPS))
        self.assertEqual([c.args[0] for c in run.call_args_list], ['one', 'two', 'three'])
        text = output.getvalue()
        for shown in ['step output for clippy', 'step output for rustfmt', 'step output for browser',
                      'program output for one', 'one failed', '4 of 13 checks failed']:
            self.assertIn(shown, text)
        self.assertIn('all checks failed', text)

    def test_tests_run_two_to_six_at_a_time(self):
        for cores, expected in [(None, 2), (1, 2), (4, 2), (10, 5), (12, 6), (64, 6)]:
            with patch.object(os, 'cpu_count', return_value=cores):
                self.assertEqual(check.threads(), expected)

    def test_tools_run_in_their_own_process(self):
        with patch.object(check, 'call') as run:
            check.step('tools', Path('wrela'), Path('logs'))
        self.assertEqual(run.call_args.args[0], [sys.executable, 'tools/runner.py', '--tools-tests'])
        self.assertEqual(run.call_args.kwargs['cwd'], check.ROOT)
        self.assertEqual(run.call_args.kwargs['env']['WRELA_BIN'], 'wrela')

    def test_wrela_checks_rebuild_the_normal_cli_before_using_it(self):
        original = Path.cwd()
        try:
            for arguments in ([], ['wrela::compiler/std::alpha'], ['tools']):
                with self.subTest(arguments=arguments), tempfile.TemporaryDirectory() as temporary:
                    root = Path(temporary).resolve()
                    target = root / 'target'
                    executable = target / 'release/wrela'
                    executable.parent.mkdir(parents=True)
                    executable.write_text('stale CLI')

                    def build(command, **kwargs):
                        self.assertEqual(command, ['cargo', 'build', '-q', '--release', '-p', 'wrela', '--bin', 'wrela'])
                        executable.write_text('current CLI')

                    def selected(name, cli, logs, *rest):
                        self.assertEqual(cli, executable)
                        self.assertEqual(cli.read_text(), 'current CLI')

                    with patch.object(check, 'ROOT', root), \
                            patch.dict(os.environ, {'CARGO_TARGET_DIR': str(target)}), \
                            patch.object(check, 'build_tests', return_value=['one']), \
                            patch.object(check, 'program_name', side_effect=str), \
                            patch.object(check, 'call', side_effect=build) as build_cli, \
                            patch.object(check, 'step', side_effect=selected), \
                            patch.object(check, 'wrela_test', side_effect=selected), \
                            patch.object(check, 'listed', return_value=['test']), \
                            patch.object(check, 'rust_run', return_value=1), \
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
names = json.loads(os.environ.get('FIXTURE_NAMES', '["alpha", "alphabet", "gpu"]'))
if '--list' in sys.argv:
    print('\\n'.join(n + ': test' for n in names))
    sys.exit(0)
if '--worker' in sys.argv: sys.exit(0)
args = sys.argv[1:]
selected = [a for a in args[args.index('--exact') + 1:] if not a.startswith('--')] \\
    if '--exact' in args else names
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
            self.assertEqual(check.rust_run(program, names=[name]), 1)
        got = json.loads((self.root / 'record').read_text())
        self.assertEqual(got['selected'], ['alpha'])
        self.assertIn('--exact', got['args'])
        self.assertFalse(Path(got['scratch']).exists())

    def test_all_tests_include_gpu_and_never_have_skip_arguments(self):
        self.assertEqual(check.rust_run(self.program), 3)
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
                check.rust_run(self.program)
            got = json.loads((self.root / 'record').read_text())
            self.assertTrue(Path(got['scratch']).exists())

    def test_a_group_is_the_tests_under_a_path_in_one_program(self):
        names = ['alpha', 'renderer::a', 'renderer::b', 'renderer_extra::a', 'outer::renderer::a']
        with patch.dict(os.environ, {'FIXTURE_NAMES': json.dumps(names)}):
            for prefix in ['renderer::', 'wrela-fixture::suite::renderer::']:
                program, tests = check.group([self.program], prefix)
                self.assertEqual(tests, ['renderer::a', 'renderer::b'])
                self.assertEqual(check.rust_run(program, names=tests), 2)
                got = json.loads((self.root / 'record').read_text())
                self.assertEqual(got['selected'], ['renderer::a', 'renderer::b'])
                self.assertIn('--exact', got['args'])
                self.assertNotIn('--nocapture', got['args'])
            self.assertEqual(check.group([self.program], 'wrela-fixture::suite::')[1], names)
            for prefix in ['missing::', 'renderer::c::']:
                with self.assertRaisesRegex(ValueError, 'none has any'):
                    check.group([self.program], prefix)
            other = {**self.program, 'target': {'name': 'other', 'kind': ['test']}}
            with self.assertRaisesRegex(ValueError, '2 have them'):
                check.group([self.program, other], 'renderer::')
            self.assertEqual(check.group([self.program, other], 'wrela-fixture::other::renderer::')[0], other)

    def test_replaced_executable_can_still_spawn_its_worker(self):
        with patch.dict(os.environ, {'REPLACE_EXE': '1', 'ORIGINAL_EXE': str(self.exe)}):
            self.assertEqual(check.rust_run(self.program), 3)


class WrelaSelection(unittest.TestCase):
    def test_a_name_under_the_clis_rust_programs_is_theirs(self):
        programs = [{'name': 'wrela::suite'}, {'name': 'wrela::wrela'}]
        names = ['tool::a', 'tool::b', 'other::c']
        for name, rust in [('wrela::suite::tool::', ['tool::a', 'tool::b']),
                           ('wrela::suite::tool::a', ['tool::a']),
                           ('wrela::compiler/std::alpha', None), ('wrela::suite', None)]:
            with self.subTest(name=name), \
                    patch.object(check, 'build_tests', return_value=programs), \
                    patch.object(check, 'program_name', side_effect=lambda p: p['name']), \
                    patch.object(check, 'listed', side_effect=lambda p: names if p['name'] == 'wrela::suite' else []), \
                    patch.object(check, 'call'), \
                    patch.object(check, 'wrela_test') as wrela, \
                    patch.object(check, 'rust_run', return_value=1) as run, \
                    contextlib.redirect_stdout(io.StringIO()):
                self.assertEqual(check.main([name]), 0)
                if rust:
                    self.assertEqual(run.call_args.args[0], programs[0])
                    self.assertEqual(run.call_args.kwargs['names'], rust)
                    wrela.assert_not_called()
                else:
                    self.assertEqual(wrela.call_args.args[0], name)
                    run.assert_not_called()

    def test_exact_results_required_and_package_paths_confined(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            (root / 'fields').mkdir()
            logs = root / 'logs'; logs.mkdir()
            for tests in [[], [{'name': 'alphabet', 'passed': True}],
                          [{'name': 'alpha', 'passed': True}] * 2,
                          [{'name': 'alpha', 'passed': False}]]:
                with patch.object(check, 'ROOT', root), \
                        patch.object(check, 'call', return_value=subprocess.CompletedProcess(
                            [], 0, json.dumps({'tests': tests}))), self.assertRaises(ValueError):
                    check.wrela_test('wrela::fields::alpha', Path('wrela'), logs)
            with patch.object(check, 'ROOT', root), patch.object(check, 'call') as run:
                for name in ['wrela::../::alpha', 'wrela::::alpha', 'wrela::fields::alpha::extra',
                             'wrela::fields::not-a-name']:
                    with self.assertRaises(ValueError): check.wrela_test(name, Path('wrela'), logs)
                run.assert_not_called()

    def test_a_package_group_runs_every_test_and_fails_on_none_or_any_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            # Resolved, as the checkout's ROOT is: macOS's /var is a link to /private/var.
            root = Path(temporary).resolve()
            (root / 'fields').mkdir()
            logs = root / 'logs'; logs.mkdir()
            def report(tests):
                return subprocess.CompletedProcess([], 0, json.dumps({'tests': tests}))
            passed = [{'name': n, 'passed': True, 'at': 'fields/a.wrela:1'} for n in ['a', 'b']]
            for tests in [[], [*passed, {'name': 'c', 'passed': False, 'at': 'fields/a.wrela:9'}]]:
                with patch.object(check, 'ROOT', root), patch.object(check, 'call', return_value=report(tests)), \
                        self.assertRaises(ValueError):
                    check.wrela_test('wrela::fields::', Path('wrela'), logs)
            with patch.object(check, 'ROOT', root), patch.object(check, 'call', return_value=report(passed)) as run, \
                    contextlib.redirect_stdout(io.StringIO()) as output:
                check.wrela_test('wrela::fields::', Path('wrela'), logs)
            self.assertEqual(run.call_args.args[0], ['wrela', 'test', str(root / 'fields'), '--json'])
            self.assertIn('ok wrela::fields::b', output.getvalue())


class BrowserSelection(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        self.directory = root / 'runtime/browser/test'
        self.directory.mkdir(parents=True)
        (self.directory / 'x.test.ts').write_text('')
        self.logs = root / 'logs'
        self.logs.mkdir()
        at_root = patch.object(check, 'ROOT', root)
        at_root.start(); self.addCleanup(at_root.stop)

    def test_no_tests_and_multiple_tests_fail_exact_selection(self):
        for xml in ['<testsuites/>', '<testsuites><testcase/><testcase/></testsuites>']:
            (self.logs / 'browser.xml').write_text(xml)
            with patch.object(check, 'call'), self.assertRaises(ValueError):
                check.browser_test('browser::x.test.ts::a', self.logs)
        with self.assertRaises(ValueError):
            check.browser_test('browser::../../outside::a', self.logs)

    def test_a_file_group_runs_every_test_in_the_file(self):
        for xml, ok in [('<testsuites><testcase/><testcase/></testsuites>', True),
                        ('<testsuites><testcase/><testcase><skipped/></testcase></testsuites>', False),
                        ('<testsuites/>', False)]:
            (self.logs / 'browser.xml').write_text(xml)
            with patch.object(check, 'call') as run:
                if ok:
                    self.assertEqual(check.browser_test('browser::x.test.ts::', self.logs), 2)
                    self.assertEqual(run.call_args.args[0][-1], str(self.directory / 'x.test.ts'))
                else:
                    with self.assertRaises(ValueError):
                        check.browser_test('browser::x.test.ts::', self.logs)


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
            test.assert_called_once_with(None, Path('logs'), None)


if __name__ == '__main__':
    unittest.main()
