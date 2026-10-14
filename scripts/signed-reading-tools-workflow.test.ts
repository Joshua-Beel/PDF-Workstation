import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

describe('manual signed reading-tools workflow', () => {
  it('is a manual pinned Windows job and uploads only the sanitized record', () => {
    const workflow = readFileSync('.github/workflows/reading-tools-verification.yml', 'utf8');
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('timeout-minutes: 60');
    expect(workflow).toContain('fetch-depth: 0');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain("artifact-ids: '11005152678'");
    expect(workflow).toContain("run-id: '36499724415'");
    expect(workflow).toContain("head_sha -cne '67d238218f4796ba7b8505d072868da0f397174a'");
    expect(workflow).toContain("digest -cne 'sha256:c0e89b5ae821d5875c31855c6596a7d47b7ca052468a026745a5d638e50ca569'");
    expect(workflow).toContain('./scripts/verify-signed-ocr-upgrade.ps1');
    expect(workflow).toContain('./scripts/verify-signed-reading-tools.ps1');
    const upload = workflow.slice(workflow.indexOf('- name: Upload only the sanitized reading-tools record'));
    expect(upload).toContain('/reading-tools-verification.json');
    expect(upload).not.toMatch(/\.exe|SIGNED_ARTIFACT_ROOT|READING_WORK_ROOT|UPGRADE_OUTPUT_ROOT/);
  });

  it('pins the inspected signed blobs and detects current feature and application-shell drift', () => {
    const verifier = readFileSync('scripts/verify-signed-reading-tools.ps1', 'utf8');
    const pins = [
      ['src/Viewer.tsx', '30359386c5d706e408b9911fc24efe7cd166ce39'],
      ['src/TextLayer.tsx', 'e73c8b5b6433439bd651a162e0637145c7c2c73b'],
      ['src/SearchPanel.tsx', 'eb26ef73a1db33141f5395672683b007ccfda691'],
      ['src/PasswordDialog.tsx', 'ff73396e79fd66c0689e4a6a4a9d2e0438bb45f4'],
      ['src/bridge.ts', 'f05fd937ecd72c82cb25c61f62687aebff98f3a3'],
      ['src-tauri/src/service.rs', 'e65e890bfbc57cda8b99787a0f7d909d5a445bf0'],
      ['src-tauri/src/text_geometry.rs', 'c6fb8b84c111d45d44aaa40c7e40d2ccab5b1241'],
    ];
    const changed: string[] = [];
    for (const [path, blob] of pins) {
      expect(verifier).toContain(`'${path}' = '${blob}'`);
      const signed = spawnSync('git', ['rev-parse', `67d238218f4796ba7b8505d072868da0f397174a:${path}`], { encoding: 'utf8' });
      const current = spawnSync('git', ['rev-parse', `HEAD:${path}`], { encoding: 'utf8' });
      expect(signed.status, signed.stderr).toBe(0);
      expect(current.status, current.stderr).toBe(0);
      expect(signed.stdout.trim()).toBe(blob);
      if (current.stdout.trim() !== blob) changed.push(path);
    }
    expect(changed).toContain('src/Viewer.tsx');
    expect(changed.length).toBeGreaterThan(0);
    expect(verifier).toContain("SignedApplicationShellBlob = '35a6d3b5c2dadecf378386f41ffe405da15c8c4c'");
    expect(verifier).toContain('currentApplicationShellMatchesSigned = [bool]($currentShell -ceq $signedShell)');
    const signedShell = spawnSync('git', ['rev-parse', '67d238218f4796ba7b8505d072868da0f397174a:src/App.tsx'], { encoding: 'utf8' });
    const currentShell = spawnSync('git', ['rev-parse', 'HEAD:src/App.tsx'], { encoding: 'utf8' });
    expect(signedShell.stdout.trim()).toBe('35a6d3b5c2dadecf378386f41ffe405da15c8c4c');
    expect(currentShell.stdout.trim()).not.toBe(signedShell.stdout.trim());
  }, 30_000);

  it('keeps artifact credentials step-scoped and the UI verification credential-free', () => {
    const workflow = readFileSync('.github/workflows/reading-tools-verification.yml', 'utf8');
    const verify = workflow.slice(
      workflow.indexOf('- name: Verify installed selection search and password flows'),
      workflow.indexOf('- name: Upload only the sanitized reading-tools record'),
    );
    expect(verify).not.toMatch(/GITHUB_TOKEN|GH_TOKEN|github\.token|secrets\./);
    expect(workflow).not.toMatch(/AZURE_|TAURI_SIGNING|gh release|create.*release/i);
  });
});
