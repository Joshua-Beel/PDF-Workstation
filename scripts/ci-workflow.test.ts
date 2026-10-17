import { readFileSync } from 'node:fs';
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
      'actions/checkout@11d5960a326750d5838078e36cf38b85af677262',
      'actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020',
      'dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d',
      'actions/cache/restore@',
      'run: npm ci',
      'run: npm run fixtures',
      'run: ./scripts/setup-pdfium.ps1',
      'run: npx tsc -b',
      'run: cargo fetch --locked --target x86_64-pc-windows-msvc --manifest-path src-tauri/Cargo.toml',
      'run: node scripts/dependency-notices.mjs --check',
      'run: npm test -- --pool=threads --maxWorkers=1 --no-file-parallelism',
      'run: npm run build',
      'run: cargo test --locked --manifest-path src-tauri/Cargo.toml --bins',
      'Keep only compiled dependencies in the Cargo cache',
      'actions/cache/save@',
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
