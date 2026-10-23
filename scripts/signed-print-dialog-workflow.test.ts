import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

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
    expect(workflow).toContain("artifact-ids: '12510717103'");
    expect(workflow).toContain("run-id: '39969839829'");
    expect(workflow).toContain("$signed = '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d'");
    expect(workflow).toContain("[uint64]$artifact.size_in_bytes -ne 11182480");
    expect(workflow).toContain("sha256:3e5c1003b553e3ed6d2f14bdd8f0b7b6baf56c7aacf55856e875f19ba270a689");
    expect(workflow.match(/^ {12}'(?:src|src-tauri)\/.+'$/gm)).toHaveLength(8);
    expect(workflow).toContain('$currentBlob -cne $signedBlob');
    expect(workflow).toContain('SIGNED_PRINT_SHELL path=src/App.tsx matched=');
    expect(workflow).toContain('fetch-depth: 0');
  });

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
    expect(verifier).toMatch(/12510717103\|39969839829/);
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
    expect(verifier).toContain("$json -match '(?i)([A-Z]:\\\\|\\\\Users\\\\|");
  });
});
