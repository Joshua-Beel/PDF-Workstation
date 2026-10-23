import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { REV_NEW, REV_PREFS_OLD, guarded, importFunctions, preamble, runBlocks, runPwsh, signedInputsSnippet } from './signed-check-test-support';

const workflow = readFileSync('.github/workflows/persistence-verification.yml', 'utf8');
const verifier = readFileSync('scripts/verify-signed-persistence.ps1', 'utf8');

function runPowerShell(script: string) {
  return spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
    cwd: process.cwd(), encoding: 'utf8', timeout: 20_000,
  });
}

describe('signed installed persistence workflow', () => {
  it('is manual-only, hosted, clean-source-bound, and exact-artifact-bound', () => {
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('timeout-minutes: 30');
    expect(workflow).toContain("$env:GITHUB_REF -cne 'refs/heads/master'");
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('fetch-depth: 0');
    expect(workflow).toContain('git diff --quiet --');
    expect(workflow).toContain('git diff --cached --quiet --');
    expect(workflow).toContain('artifact-ids: ${{ inputs.artifact_id }}');
    expect(workflow).toContain('run-id: ${{ inputs.artifact_run_id }}');
    expect(workflow).toContain('./scripts/assert-signed-artifact-metadata.ps1');
    for (const input of ['target_version', 'source_revision', 'artifact_run_id', 'artifact_id', 'artifact_name', 'artifact_bytes', 'artifact_digest', 'installer_name', 'installer_bytes', 'installer_sha256', 'record_bytes', 'record_sha256']) {
      expect(workflow).toContain(`      ${input}: { required: true, type: string }`);
    }
    for (const run of runBlocks(workflow)) expect(run).not.toContain('${{');
    expect(workflow).toMatch(/^permissions:\r?\n  contents: read\r?\n  actions: read\r?\n/m);
    expect(workflow).not.toMatch(/secrets\./);
    expect(workflow).not.toContain('pull_request:');
    expect(workflow).not.toContain('push:');
  });

  it('binds storage modules and every UI selector to the exact signed source while reporting shell parity separately', () => {
    for (const path of ['src/App.tsx', 'src/preferences.ts', 'src/recentFiles.ts']) {
      expect(workflow).toContain(path);
      expect(verifier).toContain(path);
    }
    expect(workflow).toContain('$signedRevision = $env:SOURCE_REVISION');
    expect(workflow).toContain("$workflowBlob -cne $signedBlob");
    expect(verifier).toContain('Assert-SignedPersistenceSource');
    expect(verifier).toContain('exactSignedStorageModulesMatched = $true');
    expect(verifier).toContain('appShellBlobMatchedSignedSource = [bool]$SourceBinding.appShellBlobMatchedSignedSource');
    expect(verifier).toContain('selectorsBoundToSignedAppSource = [bool]$SourceBinding.selectorsBoundToSignedAppSource');
    expect(verifier).toContain('signedSelectorMarkerCount -ne 18');
    expect(verifier).not.toContain('exactSignedAppShellMatched = $true');
  });

  it('resolves the exact signed storage blobs and selector markers from git history', () => {
    const run = (workflowRevision: string) => runPwsh(String.raw`${preamble}
      ${guarded(String.raw`
      ${signedInputsSnippet(REV_NEW)}
      ${importFunctions('verify-signed-persistence.ps1', '$script:PersistenceArtifactPins', ['Assert-SignedPersistenceSource', 'Initialize-PersistencePins'])}
      Initialize-PersistencePins -Inputs $inputs
      $result=Assert-SignedPersistenceSource -ProjectRoot '${process.cwd().replaceAll("'", "''")}' -WorkflowRevision '${workflowRevision}'
      if([int]$result.matchedStorageBlobCount-ne 2-or-not[bool]$result.selectorsBoundToSignedAppSource-or[int]$result.signedSelectorMarkerCount-ne 18){throw 'Signed persistence source binding was incomplete.'}
      `)}
    `);
    const control = run(REV_NEW);
    expect(control.status, control.stderr || control.stdout).toBe(0);
    const drifted = run(REV_PREFS_OLD);
    expect(drifted.status).toBe(1);
    expect(drifted.stderr).toContain('Workflow persistence storage source does not match the signed application: src/preferences.ts');
  }, 60_000);

  it('keeps download credentials out of the installer and persistence verifier step', () => {
    const verifierStep = workflow.slice(workflow.indexOf('- name: Verify signed installed persistence across restarts'), workflow.indexOf('- name: Upload only sanitized persistence verification'));
    expect(verifierStep).not.toMatch(/GITHUB_TOKEN|github\.token|AZURE_|TAURI_SIGNING/);
    expect(verifierStep).toContain('./scripts/verify-signed-persistence.ps1');
    expect(workflow).toContain('./scripts/setup-installed-app-webdriver.ps1 -OutputRoot $env:WEBDRIVER_ROOT');
  });

  it('pins the signed bytes and source, verifies installed trust, and uploads one sanitized record', () => {
    expect(verifier).toContain("$script:PersistenceArtifactPins['SourceRevision'] = $Inputs.SourceRevision");
    expect(verifier).toContain("$script:PersistenceArtifactPins['InstallerSha256'] = $Inputs.InstallerSha256");
    expect(verifier).toContain("$script:PersistenceArtifactPins['RecordSha256'] = $Inputs.RecordSha256");
    expect(verifier.match(/'[A-F0-9]{64}'/g)).toBeNull();
    expect(verifier).toContain('Assert-TrustedWindowsSignature -Path $installer');
    expect(verifier).toContain('Get-PersistenceInstalledApplication');
    expect(verifier).toContain('Assert-PersistenceFileReceipt -Path $welcome');
    expect(verifier).toContain('-ExpectedRuntimeVersion ([string]$driverReceipt.webView2RuntimeVersion)');
    expect(verifier).toContain("'persistence-verification.json'");
    expect(workflow).toContain('path: ${{ env.PERSISTENCE_OUTPUT_ROOT }}/persistence-verification.json');
  });

  it('parses both PowerShell entry points and bounds the silent installer', () => {
    const check = String.raw`
      foreach($file in @('scripts/installed-persistence.ps1','scripts/verify-signed-persistence.ps1')){
        $tokens=$null;$errors=$null
        [Management.Automation.Language.Parser]::ParseFile((Join-Path '${process.cwd().replaceAll("'", "''")}' $file),[ref]$tokens,[ref]$errors)|Out-Null
        if($errors.Count){throw ($file+' did not parse.')}
      }
    `;
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    for (const required of ["ArgumentList.Add('/S')", 'WaitForExit($TimeoutMilliseconds)', 'Kill($true)', 'WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden', 'TimedOut -isnot [bool]', 'ExitCode -isnot [int]']) {
      expect(verifier).toContain(required);
    }
  });

  it('marks DOM persistence only and rejects paths, credentials, raw logs, and artifact ids from proof', () => {
    for (const claim of ['realPreferencesVerified = $true', 'recentFilesVerified = $true', 'starsVerified = $true', 'clearHistoryVerified = $true', 'sameProfileAcrossRestarts = $true', 'settingsSentinelPreserved = $true', 'launchProcessCleanupVerified = $true']) {
      expect(verifier).toContain(claim);
    }
    for (const unverified of ['nativeWindowVisualVerified = $false', 'nativeFilePickerVerified = $false', 'userPdfVerified = $false']) {
      expect(verifier).toContain(unverified);
    }
    expect(verifier).toContain("[string]$script:PersistenceArtifactPins.LeakIds");
    expect(verifier).toContain('Sanitized persistence record contains a path, credential name, artifact identifier, or raw log reference.');
  });
});
