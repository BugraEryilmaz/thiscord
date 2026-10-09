"""Execute the composite action's real PowerShell cache selector in a sandbox."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest


class CacheTests(unittest.TestCase):
    def test_sharing_and_isolation(self):
        action = (Path(__file__).resolve().parents[1] /
                  '.github/actions/cargo-cache/action.yml').read_text()
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
                           CACHE_ENVIRONMENT='self-hosted', CACHE_PR='', CACHE_DISABLED='',
                           RUNNER_TEMP=str(root / '_temp'),
                           GITHUB_WORKSPACE=str(root / 'workspace'), GITHUB_ENV=str(output))
                env.update(overrides)
                result = subprocess.run(['pwsh', '-NoProfile', '-Command', '\n'.join(script)],
                                        env=env, text=True, capture_output=True)
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                values = dict(line.split('=', 1) for line in output.read_text().splitlines())
                return Path(values['CARGO_TARGET_DIR'])

            native = select('audio-windows-latest', 'native')
            self.assertEqual(native, select('desktop-windows-latest', 'native'))
            self.assertEqual(native, select('release-x86_64-pc-windows-msvc', 'native'))
            self.assertNotEqual(native, select('audio', 'native', CACHE_RUNNER='runner-b'))
            self.assertNotEqual(native, select('audio', 'native', CACHE_PR='123'))
            self.assertNotEqual(native, select('release', 'ubuntu24-container'))
            self.assertNotEqual(select('wasm'), select('backend'))
            self.assertEqual(root / 'workspace' / 'target', select('audio', 'native', CACHE_DISABLED='true'))
            self.assertEqual(root / 'workspace' / 'target', select('audio', 'native', CACHE_ENVIRONMENT='github-hosted'))
            if os.name == 'nt':
                self.assertEqual(len(native.name), 32)


if __name__ == '__main__':
    unittest.main()
