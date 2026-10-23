import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';
import { REV_NEW, REV_OLD, guarded, importFunctions, preamble, runBlocks, runPwsh, signedInputsSnippet } from './signed-check-test-support';

const workflow = readFileSync('.github/workflows/expanded-page-tools-verification.yml', 'utf8');
const verifier = readFileSync('scripts/verify-signed-expanded-page-tools.ps1', 'utf8');

function runPowerShell(script: string) {
  return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', script], { cwd: process.cwd(), encoding: 'utf8', timeout: 30_000 });
}

describe('signed installed expanded page-tools workflow', () => {
  it('is manual-only, hosted, bounded, clean-source-bound, and credential-scoped', () => {
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('timeout-minutes: 90');
    expect(workflow).toContain('contents: read');
    expect(workflow).toContain('actions: read');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('fetch-depth: 0');
    expect(workflow).toContain('git diff --quiet --');
    expect(workflow).toContain('git diff --cached --quiet --');
    expect(workflow).toContain('actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683');
    expect(workflow).toContain('actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093');
    expect(workflow).toContain('actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02');
    expect(workflow).not.toMatch(/uses:\s*actions\/(?:checkout|download-artifact|upload-artifact)@v\d+/);
    const verify = workflow.slice(workflow.indexOf('- name: Verify signed installed expanded page tools'), workflow.indexOf('- name: Upload only the sanitized expanded page-tools record'));
    expect(verify).toContain('./scripts/verify-signed-expanded-page-tools.ps1');
    expect(verify).not.toMatch(/GITHUB_TOKEN|github\.token|AZURE_|TAURI_SIGNING/);
  });

  it('takes the artifact from dispatch inputs and derives all sixteen directly owned feature blobs at run time', () => {
    expect(workflow).toContain('artifact-ids: ${{ inputs.artifact_id }}');
    expect(workflow).toContain('run-id: ${{ inputs.artifact_run_id }}');
    expect(workflow).toContain('./scripts/assert-signed-artifact-metadata.ps1');
    expect(workflow).toContain('./scripts/prepare-default-draft-pdfium.ps1');
    expect(workflow).toContain('ref: ${{ inputs.source_revision }}');
    for (const run of runBlocks(workflow)) expect(run).not.toContain('${{');
    expect(verifier.match(/'[a-f0-9]{40}'/g)).toBeNull();
    expect(verifier).not.toMatch(/\b(?:11005152678|12510717103|39969839829)\b/);
    expect(verifier).toContain('featureBlobCount -ne 16');
    expect(verifier).toContain('signedSelectorMarkerCount -ne 10');
    const control = runPwsh(String.raw`${preamble}
      ${signedInputsSnippet()}
      ${importFunctions('verify-signed-expanded-page-tools.ps1', '$script:ExpandedPins', ['Assert-SignedExpandedSourceParity', 'Initialize-ExpandedPins'])}
      $script:Pins = [ordered]@{}
      Initialize-ExpandedPins -Inputs $inputs
      $result = Assert-SignedExpandedSourceParity -ProjectRoot '${process.cwd().replaceAll("'", "''")}' -CurrentRevision '${REV_NEW}'
      if ($result.featureBlobCount -ne 16 -or $result.signedSelectorMarkerCount -ne 10 -or -not $result.currentApplicationShellMatchesSigned) { throw 'Unexpected parity result.' }
      if ($script:ExpandedPins.LeakIds -cne '12510717103|39969839829') { throw 'Leak ids must come from the inputs.' }
    `);
    expect(control.status, control.stderr).toBe(0);
  });

  it('fails closed when current feature source differs from the source revision that was signed', () => {
    const run = (signed: string, current: string) => runPwsh(String.raw`${preamble}
      ${guarded(String.raw`
      ${signedInputsSnippet(signed)}
      ${importFunctions('verify-signed-expanded-page-tools.ps1', '$script:ExpandedPins', ['Assert-SignedExpandedSourceParity', 'Initialize-ExpandedPins'])}
      $script:Pins = [ordered]@{}
      Initialize-ExpandedPins -Inputs $inputs
      Assert-SignedExpandedSourceParity -ProjectRoot '${process.cwd().replaceAll("'", "''")}' -CurrentRevision '${current}' | Out-Null
      `)}
    `);
    const moved = run(REV_OLD, REV_NEW);
    expect(moved.status).toBe(1);
    expect(moved.stderr).toContain('An expanded page-tools feature blob does not match the exact signed and dispatched source:');
    const moved2 = run(REV_NEW, REV_OLD);
    expect(moved2.status).toBe(1);
    expect(moved2.stderr).toContain('An expanded page-tools feature blob does not match the exact signed and dispatched source:');
    const unknown = run('1'.repeat(40), REV_NEW);
    expect(unknown.status).toBe(1);
    expect(unknown.stderr).toContain('The exact signed source commit is unavailable.');
    expect(verifier).toContain('currentApplicationShellMatchesSigned');
    expect(verifier).not.toContain('exactSignedApplicationShellMatched=$true');
  }, 15_000);

  it('imports and invokes all three independent installed-app verifiers with fresh profiles', () => {
    for (const script of ['installed-image-page-tools.ps1', 'installed-organizer-tools.ps1', 'installed-structural-page-tools.ps1']) expect(verifier).toContain(script);
    for (const entry of ['Invoke-InstalledImagePageTools', 'Invoke-InstalledOrganizerTools', 'Invoke-InstalledStructuralPageTools']) expect(verifier).toContain(entry);
    for (const profile of ['image-profile', 'organizer-profile', 'structural-profile']) expect(verifier).toContain(profile);
    expect(verifier).toContain("'upgrade-sentinel.json'");
    expect(verifier).toContain("'organizer-source.pdf'");
    expect(verifier).toContain("'structural-donor.pdf'");
    expect(verifier).toContain("'image-source.png'");
    expect(verifier).toContain('[Drawing.Bitmap]::new(64,96');
    expect(verifier).toContain('$x -lt 21');
    expect(verifier).toContain('$x -lt 42');
    expect(verifier).toContain("New-ExpandedPageSequencePdf -Path $organizerFixture -Kind TARGET");
    expect(verifier).toContain("New-ExpandedPageSequencePdf -Path $donorFixture -Kind DONOR");
    expect(verifier).toContain('-PdfiumPath $resources.Pdfium.FullName -PdfiumReceipt $receiptSets.Pdfium');
    expect(verifier).toContain('-FirstFixturePath $organizerFixture -FirstFixtureReceipt $organizerFixtureReceipt');
    expect(verifier).toContain('-SecondFixturePath $donorFixture -SecondFixtureReceipt $donorFixtureReceipt');
  });

  it('uses trusted install receipts and writes one path-free sanitized record', () => {
    for (const marker of [
      'Assert-SignedArtifactReceipt -Receipt $receipt', 'Invoke-BoundedSilentInstaller -Path $installer', 'Assert-InstallFacts -Facts $installed',
      'Assert-InstalledSignedResources', 'Assert-TrustedWindowsSignature -Path $installer', "'expanded-page-tools-verification.json'",
      'Assert-ExpandedImageResult -Result $image', 'Assert-ExpandedOrganizerResult -Result $organizer', 'Assert-StructuralResult -Result $structural',
      'splitOutputPageCounts', 'sourcePageFingerprintSha256', 'splitPageFingerprintSha256', 'splitPageFingerprintOrderVerified',
    ]) expect(verifier).toContain(marker);
    expect(verifier).toContain("[string]$script:ExpandedPins.LeakIds");
    expect(workflow).toContain('${{ env.EXPANDED_OUTPUT_ROOT }}/expanded-page-tools-verification.json');
    expect(workflow).not.toContain('created-from-image.pdf\n');
    expect(workflow).not.toContain('combined-output.pdf\n');
  });

  it('builds two distinct six-page Letter fixtures and cleans its temporary proof directory', () => {
    const result = runPowerShell(String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      $path='${process.cwd().replaceAll("'", "''")}\scripts\verify-signed-expanded-page-tools.ps1';$tokens=$null;$errors=$null
      $ast=[Management.Automation.Language.Parser]::ParseFile($path,[ref]$tokens,[ref]$errors);if($errors.Count){throw ($errors|Out-String)}
      $definition=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq'New-ExpandedPageSequencePdf'},$true);Invoke-Expression $definition.Extent.Text
      function Assert-NoReparseAncestors{param($Path)}
      function Get-ExpandedReceipt{param($Path)$item=Get-Item -LiteralPath $Path;[pscustomobject][ordered]@{bytes=[uint64]$item.Length;sha256=(Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash}}
      $root=Join-Path $env:TEMP ('smacrobat-expanded-fixture-test-'+[guid]::NewGuid().ToString('N'));[IO.Directory]::CreateDirectory($root)|Out-Null
      try{
        $target=Join-Path $root 'organizer-source.pdf';$donor=Join-Path $root 'structural-donor.pdf'
        $targetReceipt=New-ExpandedPageSequencePdf -Path $target -Kind TARGET;$donorReceipt=New-ExpandedPageSequencePdf -Path $donor -Kind DONOR
        if($targetReceipt.bytes-eq0-or$donorReceipt.bytes-eq0-or$targetReceipt.sha256-ceq$donorReceipt.sha256){throw 'Generated fixture receipts were empty or equal.'}
        $targetText=[Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($target));$donorText=[Text.Encoding]::ASCII.GetString([IO.File]::ReadAllBytes($donor))
        if(([regex]::Matches($targetText,'/Type /Page ')).Count-ne6-or([regex]::Matches($donorText,'/Type /Page ')).Count-ne6-or([regex]::Matches($targetText,'/MediaBox \[0 0 612 792\]')).Count-ne6-or([regex]::Matches($donorText,'/MediaBox \[0 0 612 792\]')).Count-ne6){throw 'Generated fixture page topology was not exact.'}
        foreach($page in 1..6){if(-not$targetText.Contains("(TARGET PAGE $page)")-or-not$donorText.Contains("(DONOR PAGE $page)")){throw 'Generated fixture page identities were incomplete.'}}
      }finally{[IO.Directory]::Delete($root,$true)}
      if(Test-Path -LiteralPath $root){throw 'Generated fixture test directory was not cleaned.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('parses every entry point', () => {
    const result = runPowerShell(String.raw`
      foreach($file in @('scripts/installed-image-page-tools.ps1','scripts/installed-organizer-tools.ps1','scripts/installed-structural-page-tools.ps1','scripts/installed-structural-pdfium-proof.ps1','scripts/verify-signed-expanded-page-tools.ps1')){
        $tokens=$null;$errors=$null;[Management.Automation.Language.Parser]::ParseFile((Join-Path '${process.cwd().replaceAll("'", "''")}' $file),[ref]$tokens,[ref]$errors)|Out-Null
        if($errors.Count){throw ($file+' did not parse: '+($errors|Out-String))}
      }
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });
});
