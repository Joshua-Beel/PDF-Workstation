import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const workflow = readFileSync('.github/workflows/ci.yml', 'utf8').replace(/\r\n?/g, '\n');

describe('push and pull request CI workflow', () => {
  it('runs on master pushes and pull requests only, read-only and without secrets', () => {
    expect(workflow).toContain('on:\n  push:\n    branches: [master]\n  pull_request:\n    branches: [master]\n');
    expect(workflow).not.toMatch(/pull_request_target|workflow_run|schedule:|workflow_dispatch/);
    expect(workflow).toContain('\npermissions:\n  contents: read\n');
    expect(workflow.match(/permissions:/g)).toHaveLength(1);
    expect(workflow).not.toMatch(/secrets\.|environment:|id-token|:\s*write\b|GITHUB_TOKEN|github\.token/i);
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow, 'signed-artifact workflow tests read pinned commits from git history').toContain('        with:\n          fetch-depth: 0\n          persist-credentials: false\n');
    expect(workflow).toContain('runs-on: windows-latest');
    expect(workflow).toContain('timeout-minutes: 45');
  });

  it('cancels superseded pull request runs but lets every master push finish', () => {
    expect(workflow).toContain("concurrency:\n  group: ci-${{ github.event.pull_request.number || github.ref }}\n  cancel-in-progress: ${{ github.event_name == 'pull_request' }}\n");
    expect(workflow).not.toContain('cancel-in-progress: true');
  });

  it('never builds installers, signs, uploads or touches releases', () => {
    expect(workflow).not.toMatch(/build-installer|setup-ocr|npm run (?:installer|tauri)|AZURE_|TAURI_SIGNING|artifact-signing|setup-dotnet/i);
    expect(workflow).not.toMatch(/upload-artifact|download-artifact|gh release|release create|tags:|latest\.json|cargo install/i);
  });

  it('runs the signing-free gate in order', () => {
    const ordered = [
      'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1',
      'actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1',
      'dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d',
      'actions/cache/restore@',
      'run: npm ci',
      'run: npm run fixtures',
      'run: ./scripts/setup-pdfium.ps1',
      'Fetch and verify the pinned qpdf for the annotation interchange check',
      'run: npx tsc -b',
      'run: cargo fetch --locked --target x86_64-pc-windows-msvc --manifest-path src-tauri/Cargo.toml',
      'run: node scripts/dependency-notices.mjs --check',
      'run: npm test -- --pool=threads --maxWorkers=1 --no-file-parallelism',
      'run: npm run build',
      'run: cargo test --locked --manifest-path src-tauri/Cargo.toml --bins\n',
      'run: cargo test --locked --manifest-path src-tauri/Cargo.toml --no-run',
      'Keep only compiled dependencies in the Cargo cache',
      'actions/cache/save@',
      'Annotation interchange check with the pinned qpdf (E2)',
      'Recheck tracked source',
    ];
    let position = -1;
    for (const marker of ordered) {
      const next = workflow.indexOf(marker, position + 1);
      expect(next, `missing or out-of-order marker: ${marker}`).toBeGreaterThan(position);
      position = next;
      expect(workflow.indexOf(marker, next + marker.length), `duplicate marker: ${marker}`).toBe(-1);
    }
  });

  it('writes a bounded dependency-only Cargo cache, keyed on toolchain and lockfile, only from master pushes', () => {
    const key = "key: ci-cargo-deps-${{ runner.os }}-${{ steps.toolchain.outputs.cachekey }}-${{ hashFiles('src-tauri/Cargo.lock') }}\n";
    expect(workflow.split(key)).toHaveLength(3);
    expect(workflow).toContain('- id: toolchain\n        uses: dtolnay/rust-toolchain@');
    expect(workflow).not.toContain('restore-keys');
    expect(workflow).not.toContain('github.sha');
    const guard = "if: ${{ success() && github.event_name == 'push' && github.ref == 'refs/heads/master' && steps.cargo-cache.outputs.cache-hit != 'true' }}";
    expect(workflow.split(guard)).toHaveLength(3);
    const prune = workflow.slice(workflow.indexOf('Keep only compiled dependencies'), workflow.indexOf('Save Cargo dependency cache'));
    expect(prune).toContain("'incremental'");
    expect(prune).toContain("-Filter 'pdf?workstation*'");
    expect(prune).toContain("-Filter 'pdf-workstation-*'");
    expect(prune).toContain('CARGO_CACHE_TARGET_BYTES=');
    expect(workflow).not.toMatch(/uses: actions\/cache@/);
    expect(workflow).not.toMatch(/CARGO_TARGET_DIR/);
  });
});

describe('pinned qpdf annotation interchange check (E2, SM7 C-2)', () => {
  const step = (name: string) => {
    const start = workflow.indexOf(`      - name: ${name}\n`);
    expect(start, name).toBeGreaterThan(-1);
    const end = workflow.indexOf('\n      - ', start + 1);
    return workflow.slice(start, end === -1 ? undefined : end);
  };

  it('fetches qpdf fresh into the runner temp folder and takes QPDF_EXE only from the printed line', () => {
    const fetch = step('Fetch and verify the pinned qpdf for the annotation interchange check');
    expect(fetch).toContain('id: qpdf\n');
    expect(fetch).toContain("$root = Join-Path $env:RUNNER_TEMP 'qpdf'");
    expect(fetch).toContain('-File ./scripts/setup-qpdf.ps1 -WorkRoot $root)');
    expect(fetch).not.toMatch(/ZipCache|ZipPath|RecordFileHashes|PinsPath/);
    expect(fetch).toContain('if ($LASTEXITCODE -ne 0) { throw');
    expect(fetch).toContain("StartsWith('QPDF_EXE=')");
    expect(fetch).toContain('if ($found.Count -ne 1) { throw');
    expect(fetch).toContain('"exe=$exe" | Out-File -LiteralPath $env:GITHUB_OUTPUT');
    expect(fetch).not.toContain('GITHUB_ENV');
    expect(workflow).not.toContain('GITHUB_ENV');
    expect(workflow.match(/QPDF_EXE: /g), 'only the E2 step receives QPDF_EXE').toHaveLength(1);
  });

  it('runs exactly the ignored E2 test and fails unless that one test passed', () => {
    const e2 = step('Annotation interchange check with the pinned qpdf (E2)');
    expect(e2).toContain('QPDF_EXE: ${{ steps.qpdf.outputs.exe }}');
    expect(e2).toContain("if (-not $env:QPDF_EXE) { throw");
    expect(e2).toContain('--bins annotations::qpdf_check::tests::qpdf_reads_the_interchange_fixtures_when_provided -- --ignored --exact');
    expect(e2).toContain('if ($code -ne 0) { throw');
    expect(e2).toContain("'^test result: ok\\. 1 passed; 0 failed; 0 ignored;'");
    expect(e2).toContain('.Count -ne 1) { throw');
    expect(e2).not.toMatch(/continue-on-error|if:/);
  });

  it('runs qpdf only after the master-only Cargo cache save, so nothing it touches can enter that cache (SM7 L-1)', () => {
    const save = workflow.indexOf('      - name: Save Cargo dependency cache\n');
    const e2 = workflow.indexOf('      - name: Annotation interchange check with the pinned qpdf (E2)\n');
    expect(save).toBeGreaterThan(-1);
    expect(e2).toBeGreaterThan(save);
    expect(workflow.indexOf('QPDF_EXE: ', 0)).toBeGreaterThan(save);
  });

  it('is wired into ci.yml only: no other workflow mentions qpdf', () => {
    const others = readdirSync('.github/workflows').filter((name) => /\.ya?ml$/.test(name) && name !== 'ci.yml');
    expect(others).toEqual(expect.arrayContaining(['release.yml', 'ocr-signed-artifact.yml']));
    for (const name of others) {
      expect(readFileSync(join('.github/workflows', name), 'utf8'), name).not.toMatch(/qpdf/i);
    }
  });

  it('ships nothing from qpdf: not in the bundle resources or the notices inventory', () => {
    expect(readFileSync('src-tauri/tauri.conf.json', 'utf8')).not.toMatch(/qpdf/i);
    expect(readFileSync('src-tauri/resources/third-party-licenses/inventory.json', 'utf8')).not.toMatch(/qpdf/i);
    expect(readFileSync('src-tauri/resources/third-party-licenses/THIRD-PARTY-NOTICES.txt', 'utf8')).not.toMatch(/qpdf/i);
  });
});
