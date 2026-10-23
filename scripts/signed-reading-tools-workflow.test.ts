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
    expect(workflow).toContain("artifact-ids: '12510717103'");
    expect(workflow).toContain("run-id: '39969839829'");
    expect(workflow).toContain("head_sha -cne '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d'");
    expect(workflow).toContain("digest -cne 'sha256:3e5c1003b553e3ed6d2f14bdd8f0b7b6baf56c7aacf55856e875f19ba270a689'");
    expect(workflow).toContain('./scripts/verify-signed-ocr-upgrade.ps1');
    expect(workflow).toContain('./scripts/verify-signed-reading-tools.ps1');
    const upload = workflow.slice(workflow.indexOf('- name: Upload only the sanitized reading-tools record'));
    expect(upload).toContain('/reading-tools-verification.json');
    expect(upload).not.toMatch(/\.exe|SIGNED_ARTIFACT_ROOT|READING_WORK_ROOT|UPGRADE_OUTPUT_ROOT/);
  });

  it('pins the inspected signed blobs and detects current feature and application-shell drift', () => {
    const verifier = readFileSync('scripts/verify-signed-reading-tools.ps1', 'utf8');
    const pins = [
      ['src/Viewer.tsx', '4e9e5464521d2a963eff84f71cc16453acda9940'],
      ['src/TextLayer.tsx', '9984131a916f911d48005979b070f653e4b37215'],
      ['src/SearchPanel.tsx', 'db36f2dd70795cf3f581f255d32eb7ae834c5c59'],
      ['src/PasswordDialog.tsx', '5a06bea211de4be9403f74c2739e2194ce264a77'],
      ['src/bridge.ts', '88707e0cf6b140002845044be3016cb832d78d24'],
      ['src-tauri/src/service.rs', '737a4492b4006eb6272c1b9084dc54f508309037'],
      ['src-tauri/src/text_geometry.rs', '7d54d3fcac02ffd1d873b5fd6b265d9d42b25e4b'],
    ];
    const changed: string[] = [];
    for (const [path, blob] of pins) {
      expect(verifier).toContain(`'${path}' = '${blob}'`);
      const signed = spawnSync('git', ['rev-parse', `4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d:${path}`], { encoding: 'utf8' });
      const current = spawnSync('git', ['rev-parse', `HEAD:${path}`], { encoding: 'utf8' });
      expect(signed.status, signed.stderr).toBe(0);
      expect(current.status, current.stderr).toBe(0);
      expect(signed.stdout.trim()).toBe(blob);
      if (current.stdout.trim() !== blob) changed.push(path);
    }
    expect(verifier).toContain("SignedApplicationShellBlob = '2af97705343967869b4174e2037fec6619b68fe5'");
    expect(verifier).toContain('currentApplicationShellMatchesSigned = [bool]($currentShell -ceq $signedShell)');
    const signedShell = spawnSync('git', ['rev-parse', '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d:src/App.tsx'], { encoding: 'utf8' });
    const currentShell = spawnSync('git', ['rev-parse', 'HEAD:src/App.tsx'], { encoding: 'utf8' });
    expect(signedShell.stdout.trim()).toBe('2af97705343967869b4174e2037fec6619b68fe5');
    expect(currentShell.status, currentShell.stderr).toBe(0);
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
