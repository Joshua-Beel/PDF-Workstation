import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

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
    expect(workflow).toContain('artifact-ids: \'12510717103\'');
    expect(workflow).toContain("run-id: '39969839829'");
    expect(workflow).toContain("[string]$artifact.workflow_run.head_sha -cne '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d'");
    expect(workflow).not.toContain('pull_request:');
    expect(workflow).not.toContain('push:');
  });

  it('binds storage modules and every UI selector to the exact signed source while reporting shell parity separately', () => {
    for (const path of ['src/App.tsx', 'src/preferences.ts', 'src/recentFiles.ts']) {
      expect(workflow).toContain(path);
      expect(verifier).toContain(path);
    }
    expect(workflow).toContain("$signedRevision = '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d'");
    expect(workflow).toContain("$workflowBlob -cne $signedBlob");
    expect(verifier).toContain('Assert-SignedPersistenceSource');
    expect(verifier).toContain('exactSignedStorageModulesMatched = $true');
    expect(verifier).toContain('appShellBlobMatchedSignedSource = [bool]$SourceBinding.appShellBlobMatchedSignedSource');
    expect(verifier).toContain('selectorsBoundToSignedAppSource = [bool]$SourceBinding.selectorsBoundToSignedAppSource');
    expect(verifier).toContain('signedSelectorMarkerCount -ne 18');
    expect(verifier).not.toContain('exactSignedAppShellMatched = $true');
  });

  it('resolves the exact signed storage blobs and selector markers from git history', () => {
    const check = String.raw`
      $tokens=$null;$errors=$null
      $ast=[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\scripts\verify-signed-persistence.ps1',[ref]$tokens,[ref]$errors)
      $function=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq'Assert-SignedPersistenceSource'},$true)
      Invoke-Expression $function.Extent.Text
      $script:PersistenceArtifactPins=[ordered]@{SourceRevision='4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d'}
      $head=(& git rev-parse HEAD).Trim();$result=Assert-SignedPersistenceSource -ProjectRoot '${process.cwd().replaceAll("'", "''")}' -WorkflowRevision $head
      if([int]$result.matchedStorageBlobCount-ne 2-or-not[bool]$result.selectorsBoundToSignedAppSource-or[int]$result.signedSelectorMarkerCount-ne 18){throw 'Signed persistence source binding was incomplete.'}
    `;
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('keeps download credentials out of the installer and persistence verifier step', () => {
    const verifierStep = workflow.slice(workflow.indexOf('- name: Verify signed installed persistence across restarts'), workflow.indexOf('- name: Upload only sanitized persistence verification'));
    expect(verifierStep).not.toMatch(/GITHUB_TOKEN|github\.token|AZURE_|TAURI_SIGNING/);
    expect(verifierStep).toContain('./scripts/verify-signed-persistence.ps1');
    expect(workflow).toContain('./scripts/setup-installed-app-webdriver.ps1 -OutputRoot $env:WEBDRIVER_ROOT');
  });

  it('pins the signed bytes and source, verifies installed trust, and uploads one sanitized record', () => {
    expect(verifier).toContain("SourceRevision = '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d'");
    expect(verifier).toContain("InstallerSha256 = '3DE99F111652727BB59F9C17FF4E3CCA42ED48E9DE5E0614920280FFB0BA9FE5'");
    expect(verifier).toContain("RecordSha256 = '0383DAE8FA04705FA26F63CB3CC4EDE16442129D87F99E7900BD1EF301C4FB5C'");
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
    expect(verifier).toContain('12510717103|39969839829');
    expect(verifier).toContain('Sanitized persistence record contains a path, credential name, artifact identifier, or raw log reference.');
  });
});
