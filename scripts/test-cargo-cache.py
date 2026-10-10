"""Exercise cache selection, actual Cargo reuse, and cleanup in temporary roots."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time
import unittest

REPOSITORY = Path(__file__).resolve().parents[1]


class CacheTests(unittest.TestCase):
    def test_sharing_and_isolation(self):
        action = (REPOSITORY / '.github/actions/cargo-cache/action.yml').read_text()
        lines = action.split('      run: |\n', 1)[1].splitlines()
        script = []
        for line in lines:
            if line and not line.startswith('        '):
                break
            script.append(line[8:])
        with tempfile.TemporaryDirectory(prefix='thiscord-cache-test-') as directory:
            root = Path(directory)
            def select(job, group='', **overrides):
                output = root / 'env'
                output.write_text('')
                env = dict(os.environ, CACHE_JOB=job, CACHE_GROUP=group,
                           CACHE_REPOSITORY='owner/repo', CACHE_RUNNER='runner-a',
                           CACHE_OS='Windows', CACHE_ARCH='X64',
                           CACHE_ENVIRONMENT='self-hosted', CACHE_PR='', CACHE_DISABLED='',
                           RUNNER_TEMP=str(root / '_temp'),
                           GITHUB_WORKSPACE=str(root / 'workspace'), GITHUB_ENV=str(output))
                env.update(overrides)
                result = subprocess.run(['pwsh', '-NoProfile', '-Command', '\n'.join(script)],
                                        env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                values = dict(line.split('=', 1) for line in output.read_text().splitlines())
                target = Path(values['CARGO_TARGET_DIR'])
                self.assertEqual(target, Path(values['CARGO_BUILD_BUILD_DIR']))
                if values['THISCORD_LOCAL_CACHE'] == 'true':
                    self.assertEqual(2, json.loads((target / 'thiscord-cache.json').read_text())['version'])
                return target

            native = select('audio-windows-latest', 'native')
            self.assertEqual(native, select('desktop-windows-latest', 'native'))
            self.assertEqual(native, select('release-x86_64-pc-windows-msvc', 'native'))
            self.assertNotEqual(native, select('audio', 'native', CACHE_RUNNER='runner-b'))
            self.assertEqual(native, select('audio', 'native', CACHE_PR='123'))
            self.assertEqual(native, select('audio', 'native', CACHE_PR='456'))
            self.assertNotEqual(native, select('audio', 'native', CACHE_REPOSITORY='other/repo'))
            self.assertNotEqual(native, select('audio', 'native', CACHE_OS='Linux'))
            self.assertNotEqual(native, select('audio', 'native', CACHE_ARCH='ARM64'))
            self.assertNotEqual(native, select('release', 'ubuntu24-container'))
            self.assertEqual(native, select('wasm'))
            self.assertEqual(native, select('backend'))
            self.assertEqual(root / 'workspace' / 'target', select('audio', 'native', CACHE_DISABLED='true'))
            self.assertEqual(root / 'workspace' / 'target', select('audio', 'native', CACHE_ENVIRONMENT='github-hosted'))
            if os.name == 'nt':
                self.assertEqual(len(native.name), 32)

    def test_real_cargo_reuses_dependencies_and_preserves_checkout_binaries(self):
        with tempfile.TemporaryDirectory(prefix='thiscord-cargo-reuse-') as directory:
            root = Path(directory)
            dependency = root / 'dependency'
            (dependency / 'src').mkdir(parents=True)
            (dependency / 'Cargo.toml').write_text(
                '[package]\nname="cache-fixture-dep"\nversion="0.1.0"\nedition="2024"\n')
            (dependency / 'src/lib.rs').write_text('pub fn value() -> u32 { 42 }')
            env = {k: v for k, v in os.environ.items() if k not in (
                'CARGO_BUILD_BUILD_DIR', 'CARGO_TARGET_DIR', 'CARGO_BUILD_TARGET_DIR',
                'CARGO_BUILD_TARGET', 'RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER',
                'CARGO_ENCODED_RUSTFLAGS', 'RUSTFLAGS')}
            env['CARGO_HOME'] = str(root / 'cargo-home')
            for name in ('first', 'second'):
                checkout = root / name
                (checkout / 'src').mkdir(parents=True)
                (checkout / '.cargo').mkdir()
                (checkout / '.cargo/config.toml').write_text(
                    (REPOSITORY / '.cargo/config.toml').read_text())
                (checkout / 'rust-toolchain.toml').write_text(
                    (REPOSITORY / 'rust-toolchain.toml').read_text())
                (checkout / 'Cargo.toml').write_text(
                    '[package]\nname="cache-fixture-app"\nversion="0.1.0"\nedition="2024"\n'
                    '[dependencies]\ncache-fixture-dep={path="../dependency"}\n')
                (checkout / 'src/main.rs').write_text(
                    f'fn main() {{ println!("{name} {{}}", cache_fixture_dep::value()); }}')
                result = subprocess.run(['cargo', 'build', '--offline', '-v'],
                                        cwd=checkout, env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                if name == 'second':
                    self.assertIn('Fresh cache-fixture-dep', result.stderr)
            self.assertTrue((root / 'cargo-home/thiscord/debug/deps').is_dir())
            suffix = '.exe' if os.name == 'nt' else ''
            for name in ('first', 'second'):
                binary = root / name / 'target/debug' / ('cache-fixture-app' + suffix)
                self.assertEqual(f'{name} 42', subprocess.check_output([str(binary)], text=True).strip())

    def test_tools_persist_and_isolate_install_options(self):
        action = (REPOSITORY / '.github/actions/cargo-tool/action.yml').read_text()
        script = []
        for line in action.split('      run: |\n', 1)[1].splitlines():
            if line and not line.startswith('        '):
                break
            script.append(line[8:])
        with tempfile.TemporaryDirectory(prefix='thiscord-tools-test-') as directory:
            root = Path(directory)
            def select(**overrides):
                output = root / 'output'
                output.write_text('')
                env = dict(os.environ, RUNNER_TEMP=str(root / '_temp'),
                           TOOL_CRATE='trunk', TOOL_VERSION='0.21.14', TOOL_ARGS='',
                           TOOL_PLATFORM='Linux-X64', TOOL_CHAIN='toolchain-hash',
                           THISCORD_LOCAL_CACHE='true', GITHUB_OUTPUT=str(output),
                           GITHUB_PATH=str(root / 'path'))
                env.update(overrides)
                result = subprocess.run(['pwsh', '-NoProfile', '-Command', '\n'.join(script)],
                                        env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                values = dict(line.split('=', 1) for line in output.read_text().splitlines())
                return Path(values['root'])
            selected = select()
            self.assertEqual(root / 'thiscord-tools', selected.parent)
            self.assertEqual(selected, select(RUNNER_TEMP=str(root / '_other-temp')))
            for key, value in [('TOOL_VERSION', '0.21.15'), ('TOOL_CHAIN', 'new-rust'),
                               ('TOOL_PLATFORM', 'Windows-X64'), ('TOOL_ARGS', '--no-default-features')]:
                self.assertNotEqual(selected, select(**{key: value}))
            self.assertEqual(root / '_temp/thiscord-tools/trunk-0.21.14',
                             select(THISCORD_LOCAL_CACHE='false'))

    def test_editors_share_the_build_cache_and_keep_target_selection(self):
        for name, wasm in [('leptos', True), ('native', False)]:
            settings = json.loads((REPOSITORY / f'thiscord-{name}.code-workspace').read_text())['settings']
            self.assertFalse(settings['rust-analyzer.cargo.targetDir'])
            build = settings['rust-analyzer.cargo.buildScripts.overrideCommand']
            self.assertEqual(build, settings['rust-analyzer.check.overrideCommand'])
            self.assertNotIn('--target-dir', build)
            if wasm:
                self.assertEqual('thiscord-ui', build[build.index('--bin') + 1])
                self.assertEqual('wasm32-unknown-unknown', build[build.index('--target') + 1])
                self.assertNotIn('--workspace', build)
            else:
                self.assertEqual('thiscord-frontend/desktop', build[build.index('--features') + 1])


class PruneTests(unittest.TestCase):
    def invoke(self, root, *args):
        # PowerShell single-quoted literals, never JSON/shell interpolation.
        quote = lambda value: "'" + str(value).replace("'", "''") + "'"
        command = ('& ' + quote(REPOSITORY / 'scripts/prune-cargo-cache.ps1') +
                   ' -CacheRoot ' + quote(root) + ' ' + ' '.join(args) + ' | ConvertTo-Json')
        return subprocess.run(['pwsh', '-NoProfile', '-Command', command],
                              text=True, capture_output=True)

    def test_preview_retention_and_explicit_idle_cleanup(self):
        with tempfile.TemporaryDirectory(prefix='thiscord-prune-test-') as directory:
            root = Path(directory) / 'tc'
            root.mkdir()
            old, recent, current, unrelated = [root / name for name in (
                'a' * 32, 'b' * 64, 'c' * 32, 'not-a-cache')]
            for path in (old, recent, current, unrelated):
                path.mkdir()
                (path / 'data').write_text('cached')
            (current / 'thiscord-cache.json').write_text('{"version":2}')
            past = time.time() - 60 * 86400
            for path in (old, current, unrelated):
                for child in path.iterdir():
                    os.utime(child, (past, past))
                os.utime(path, (past, past))
            result = self.invoke(root)
            self.assertEqual(result.returncode, 0, result.stderr)
            statuses = {Path(r['Path']).name: r['Status'] for r in json.loads(result.stdout)}
            self.assertEqual({old.name: 'Would-remove', recent.name: 'Keep-recent',
                              current.name: 'Keep-current'}, statuses)
            self.assertTrue(old.exists())
            self.assertNotEqual(self.invoke(root, '-Apply').returncode, 0)
            self.assertTrue(old.exists())
            result = self.invoke(root, '-Apply', '-RunnersStopped')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse(old.exists())
            for path in (recent, current, unrelated):
                self.assertTrue(path.exists())
            self.assertNotEqual(self.invoke(directory, '-Apply', '-RunnersStopped').returncode, 0)

    def test_links_cannot_escape_cleanup_root(self):
        with tempfile.TemporaryDirectory(prefix='thiscord-prune-links-') as directory:
            root = Path(directory) / 'thiscord-cargo'
            root.mkdir()
            outside = Path(directory) / 'outside'
            outside.mkdir()
            (outside / 'keep').write_text('keep')
            nested = root / ('b' * 32)
            nested.mkdir()
            for link in (root / ('a' * 32), nested / 'linked'):
                if os.name == 'nt':
                    # Junctions do not need Windows symlink privileges.
                    quote = lambda value: "'" + str(value).replace("'", "''") + "'"
                    result = subprocess.run([
                        'pwsh', '-NoProfile', '-Command',
                        'New-Item -ItemType Junction -Path ' + quote(link) +
                        ' -Target ' + quote(outside)], text=True, capture_output=True)
                    self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                else:
                    link.symlink_to(outside, target_is_directory=True)
            result = self.invoke(root, '-Apply', '-RunnersStopped')
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertTrue((outside / 'keep').exists())
            self.assertTrue(nested.exists())


if __name__ == '__main__':
    unittest.main()
