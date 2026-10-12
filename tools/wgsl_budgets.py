#!/usr/bin/env python3
"""Build a bounded corpus of real shader capabilities and check emitted source sizes.

Shaders built through the end-to-end must-build helper are also size-checked there. This
corpus exercises the CLI output path without constructing every example or baking a scene.
"""
import gzip
from pathlib import Path
import subprocess
import sys
import tempfile

PACKAGES = ('examples/hello-field', 'compiler/tests/renderer', 'compiler/tests/workgroup',
            'compiler/tests/texels', 'compiler/tests/groups', 'compiler/tests/domains', 'console')


def main(wrela):
    pipelines = 0
    with tempfile.TemporaryDirectory(prefix='wrela-shaders-') as temporary:
        for index, pkg in enumerate(PACKAGES):
            output = Path(temporary) / str(index)
            result = subprocess.run([wrela, 'build', pkg, '-o', str(output)], capture_output=True, text=True)
            if result.returncode:
                sys.exit(result.stdout + result.stderr)
            shaders = list(output.glob('*.wgsl'))
            if not shaders:
                sys.exit(f'{pkg}: no shader pipelines')
            for shader in shaders:
                data = shader.read_bytes()
                budget = 128 * 1024 if pkg == 'examples/hello-field' else 256 * 1024
                if len(data) > budget:
                    sys.exit(f'{pkg}/{shader.name}: {len(data)} bytes over {budget}')
                if pkg == 'examples/hello-field':
                    compressed = len(gzip.compress(data))
                    if compressed > 24 * 1024:
                        sys.exit(f'{pkg}/{shader.name}: {compressed} compressed bytes over {24 * 1024}')
            pipelines += len(shaders)
    print(f'{pipelines} real shader pipelines within size budgets')


if __name__ == '__main__':
    main(sys.argv[1])
