import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { REV_NEW, REV_OLD, guarded, importFunctions, preamble, runBlocks, runPwsh, signedInputsSnippet } from './signed-check-test-support';

describe('manual signed reading-tools workflow', () => {
  it('is a manual pinned Windows job and uploads only the sanitized record', () => {
    const workflow = readFileSync('.github/workflows/reading-tools-verification.yml', 'utf8');
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('timeout-minutes: 60');
    expect(workflow).toContain('fetch-depth: 0');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('artifact-ids: ${{ inputs.artifact_id }}');
    expect(workflow).toContain('run-id: ${{ inputs.artifact_run_id }}');
    expect(workflow).toContain('./scripts/assert-signed-artifact-metadata.ps1');
    expect(workflow).toContain('./scripts/prepare-default-draft-pdfium.ps1');
    for (const input of ['target_version', 'source_revision', 'artifact_run_id', 'artifact_id', 'artifact_name', 'artifact_bytes', 'artifact_digest', 'installer_name', 'installer_bytes', 'installer_sha256', 'record_bytes', 'record_sha256']) {
      expect(workflow).toContain(`      ${input}: { required: true, type: string }`);
    }
    for (const run of runBlocks(workflow)) expect(run).not.toContain('${{');
    expect(workflow).toMatch(/^permissions:\r?\n  contents: read\r?\n  actions: read\r?\n/m);
    expect(workflow).toContain('./scripts/verify-signed-ocr-upgrade.ps1');
    expect(workflow).toContain('./scripts/verify-signed-reading-tools.ps1');
    const upload = workflow.slice(workflow.indexOf('- name: Upload only the sanitized reading-tools record'));
    expect(upload).toContain('/reading-tools-verification.json');
    expect(upload).not.toMatch(/\.exe|SIGNED_ARTIFACT_ROOT|READING_WORK_ROOT|UPGRADE_OUTPUT_ROOT/);
  });

  it('derives the eleven feature blobs and the application shell from the source revision at run time', () => {
    const verifier = readFileSync('scripts/verify-signed-reading-tools.ps1', 'utf8');
    expect(verifier.match(/'[a-f0-9]{40}'/g)).toBeNull();
    expect(verifier.match(/'[A-F0-9]{64}'/g)).toHaveLength(1);
    expect(verifier).not.toMatch(/\b(?:11005152678|12510717103|39969839829)\b/);
    expect(verifier).toContain('currentApplicationShellMatchesSigned = [bool]($currentShell -ceq $signedShell)');
    expect(verifier).toContain("[string]$script:ReadingVerificationPins.LeakIds");
    const run = (signed: string, current: string) => runPwsh(String.raw`${preamble}
      ${guarded(String.raw`
      ${signedInputsSnippet(signed)}
      ${importFunctions('verify-signed-reading-tools.ps1', '$script:ReadingVerificationPins', ['Assert-SignedReadingSourceParity', 'Initialize-ReadingPins'])}
      Initialize-ReadingPins -Inputs $inputs
      $result = Assert-SignedReadingSourceParity -ProjectRoot '${process.cwd().replaceAll("'", "''")}' -CurrentRevision '${current}'
      if ($result.featureBlobCount -ne 11 -or -not $result.currentApplicationShellMatchesSigned) { throw 'Unexpected parity result.' }
      `)}
    `);
    const control = run(REV_NEW, REV_NEW);
    expect(control.status, control.stderr || control.stdout).toBe(0);
    const moved = run(REV_OLD, REV_NEW);
    expect(moved.status).toBe(1);
    expect(moved.stderr).toContain('A reading feature source blob does not match the exact signed and dispatched source parity contract.');
    const unknown = run('2'.repeat(40), REV_NEW);
    expect(unknown.status).toBe(1);
    expect(unknown.stderr).toContain('The exact signed source commit is unavailable.');
  }, 90_000);

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
