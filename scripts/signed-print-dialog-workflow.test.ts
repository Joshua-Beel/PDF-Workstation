import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { REV_NEW, REV_OLD, guarded, importFunctions, preamble, runBlocks, runPwsh, signedInputsSnippet } from './signed-check-test-support';

describe('manual signed print dialog workflow', () => {
  it('is manual, hosted, bounded, read-only, and preflights before downloads or installation', () => {
    const workflow = readFileSync('.github/workflows/print-dialog-verification.yml', 'utf8');
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('timeout-minutes: 45');
    expect(workflow).toContain('contents: read');
    expect(workflow).toContain('actions: read');
    const preflight = workflow.indexOf('- name: Preflight hosted Windows printers without installing features');
    const drivers = workflow.indexOf('- name: Prepare exact hosted WebDriver inputs');
    const download = workflow.indexOf('- name: Download exact signed application artifact');
    const verify = workflow.indexOf('- name: Verify signed installed native print dialog and output');
    expect(preflight).toBeGreaterThan(0);
    expect(preflight).toBeLessThan(drivers);
    expect(drivers).toBeLessThan(download);
    expect(download).toBeLessThan(verify);
    expect(workflow).not.toMatch(/Enable-WindowsOptionalFeature|Add-WindowsCapability|dism(?:\.exe)?/i);
  });

  it('pins the exact signed artifact and proves eight printing implementation blobs while reporting shell parity separately', () => {
    const workflow = readFileSync('.github/workflows/print-dialog-verification.yml', 'utf8');
    expect(workflow).toContain('artifact-ids: ${{ inputs.artifact_id }}');
    expect(workflow).toContain('run-id: ${{ inputs.artifact_run_id }}');
    expect(workflow).toContain('$signed = $env:SOURCE_REVISION');
    expect(workflow).toContain('./scripts/assert-signed-artifact-metadata.ps1');
    expect(workflow).toContain('./scripts/prepare-default-draft-pdfium.ps1');
    for (const input of ['target_version', 'source_revision', 'artifact_run_id', 'artifact_id', 'artifact_name', 'artifact_bytes', 'artifact_digest', 'installer_name', 'installer_bytes', 'installer_sha256', 'record_bytes', 'record_sha256']) {
      expect(workflow).toContain(`      ${input}: { required: true, type: string }`);
    }
    for (const run of runBlocks(workflow)) expect(run).not.toContain('${{');
    expect(workflow).toMatch(/^permissions:\r?\n  contents: read\r?\n  actions: read\r?\n/m);
    expect(workflow).not.toMatch(/secrets\./);
    expect(workflow.match(/^ {12}'(?:src|src-tauri)\/.+'$/gm)).toHaveLength(8);
    expect(workflow).toContain('$currentBlob -cne $signedBlob');
    expect(workflow).toContain('SIGNED_PRINT_SHELL path=src/App.tsx matched=');
    expect(workflow).toContain('fetch-depth: 0');
  });

  it('compares the eight printing blobs of the dispatched revision with the signed revision and fails closed on drift', () => {
    const run = (workflowRevision: string) => runPwsh(String.raw`${preamble}
      ${guarded(String.raw`
      ${signedInputsSnippet(REV_NEW)}
      ${importFunctions('verify-signed-print-dialog.ps1', '$script:Pins', ['Assert-SignedPrintingSourceBlobs'])}
      Add-SignedPinFields -Pins $script:Pins -Inputs $inputs
      $result = Assert-SignedPrintingSourceBlobs -ProjectRoot '${process.cwd().replaceAll("'", "''")}' -WorkflowRevision '${workflowRevision}'
      if ($result.matchedImplementationBlobCount -ne 8) { throw 'Expected eight printing blobs.' }
      `)}
    `);
    const control = run(REV_NEW);
    expect(control.status, control.stderr || control.stdout).toBe(0);
    const drifted = run(REV_OLD);
    expect(drifted.status).toBe(1);
    expect(drifted.stderr).toContain('Workflow printing source does not match the signed application:');
  }, 60_000);

  it('reuses trusted installer and WebDriver primitives and uploads only sanitized JSON', () => {
    const verifier = readFileSync('scripts/verify-signed-print-dialog.ps1', 'utf8');
    const workflow = readFileSync('.github/workflows/print-dialog-verification.yml', 'utf8');
    expect(verifier).toContain("Join-Path $PSScriptRoot 'verify-signed-ocr-upgrade.ps1'");
    expect(verifier).toContain('FunctionDefinitionAst');
    expect(verifier).toContain('Assert-SignedArtifactReceipt -Receipt $receipt');
    expect(verifier).toContain('Invoke-BoundedSilentInstaller -Path $signedInstaller');
    expect(verifier).toContain('Assert-InstalledSignedResources');
    expect(verifier.indexOf('$printerFacts = Get-PrintCapabilityFacts')).toBeLessThan(verifier.indexOf('Invoke-BoundedSilentInstaller'));
    expect(verifier).toContain("status = 'explicit-unavailable'");
    expect(verifier).toContain('installationAttempted = $false');
    expect(verifier).toContain("'print-dialog-verification.json'");
    expect(verifier).toContain("[string]$script:Pins.LeakIds");
    expect(verifier).not.toMatch(/\b(?:12510717103|39969839829)\b/);
    expect(workflow).toContain('${{ env.PRINT_OUTPUT_ROOT }}/print-dialog-verification.json');
    expect(workflow).not.toContain('printed-welcome-page-1.pdf\n');
  });

  it('keeps the proof schema path-free and records native dialog and output limits', () => {
    const verifier = readFileSync('scripts/verify-signed-print-dialog.ps1', 'utf8');
    expect(verifier).toContain("automation='process-bound-.NET-UIAutomation'");
    expect(verifier).toContain('cancellationResultTransported');
    expect(verifier).toContain('relevantProcessesRemaining');
    expect(verifier).toContain('nativeDialogsRemaining=0');
    expect(verifier).toContain('exactSignedPrintingImplementationSubsetMatched=$true');
    expect(verifier).toContain('appShellBlobMatchedSignedSource');
    expect(verifier).toContain('pdfOutput = $PrintResult.output');
    expect(verifier).toContain("$json -match ('(?i)([A-Z]:\\\\|\\\\Users\\\\|");
  });
});
