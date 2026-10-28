import { describe, expect, it } from 'vitest';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';

const workflowPath = '.github/workflows/ocr-published-installer-verification.yml';
const verifierPath = 'scripts/verify-signed-ocr-upgrade.ps1';
const approvedActionUses = [
  'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1',
  'actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9',
];

function runPwsh(source: string, env: NodeJS.ProcessEnv = process.env) {
  const root = resolve(`target/ocr-published-test-${randomUUID()}`);
  mkdirSync(root, { recursive: true });
  const path = `${root}/run.ps1`;
  writeFileSync(path, source, 'utf8');
  return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path], {
    encoding: 'utf8',
    timeout: 60_000,
    env,
  });
}

function workflowRunBlock(workflow: string, stepName: string) {
  const lines = workflow.split(/\r?\n/);
  const step = lines.findIndex((line) => line.trim() === `- name: ${stepName}`);
  if (step < 0) throw new Error(`Missing workflow step: ${stepName}`);
  const run = lines.findIndex((line, index) => index > step && line.trim() === 'run: |');
  if (run < 0) throw new Error(`Missing run block: ${stepName}`);
  const body: string[] = [];
  for (let index = run + 1; index < lines.length; index += 1) {
    if (/^ {6}- (name:|uses:)/.test(lines[index])) break;
    if (lines[index] === '') break;
    body.push(lines[index].slice(10));
  }
  return `${body.join('\n')}\n`;
}

function stepSlice(workflow: string, stepName: string) {
  const start = workflow.indexOf(`- name: ${stepName}`);
  if (start < 0) throw new Error(`Missing workflow step: ${stepName}`);
  const next = workflow.indexOf('\n      - ', start + 1);
  return workflow.slice(start, next < 0 ? undefined : next);
}

function verifierHarness(names: string[], body: string) {
  return String.raw`
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version Latest
    Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
    . ./scripts/ocr/installer-package.ps1
    . ./scripts/ocr/windows-signing.ps1
    . ./scripts/signed-check-inputs.ps1
    $tokens = $null; $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path ./${verifierPath}).Path, [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw 'Verifier did not parse.' }
    foreach ($name in @(${names.map((name) => `'${name}'`).join(',')})) {
      $function = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name }, $true)
      if (-not $function) { throw ('Missing verifier function: ' + $name) }
      Invoke-Expression $function.Extent.Text
    }
    $script:Pins = [ordered]@{ Repository = 'Joshua-Beel/PDF-Workstation'; ExpectedPublisher = 'Joshua Beel' }
    function Assert-Rejected { param([scriptblock]$Action, [string]$Label) $rejected = $false; try { & $Action | Out-Null } catch { $rejected = $true }; if (-not $rejected) { throw ('Unsafe case accepted: ' + $Label) } }
    ${body}
  `;
}

const publishedInputs = {
  TARGET_VERSION: '0.3.1',
  TARGET_TAG: 'v0.3.1',
  SOURCE_REVISION: '88bff9030ba6c7f00cd93356df3ada8be7335ffb',
  RELEASE_ID: '425998245',
  INSTALLER_ASSET_ID: '682909274',
  INSTALLER_NAME: 'PDF.Workstation_0.3.1_x64-setup.exe',
  INSTALLER_BYTES: '11210976',
  INSTALLER_SHA256: '67C685264ABC094246E95C04629BFFF5D4854BD4FE5B84FEDE8760261A119038',
};

describe('manual published installer OCR verification workflow', () => {
  it('keeps a manual master-only hosted contract with the token scoped to the download step', () => {
    const workflow = readFileSync(workflowPath, 'utf8');
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule|release):/m);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('timeout-minutes: 45');
    expect(workflow).toMatch(/^permissions:\n {2}contents: read\n\n/m);
    expect(workflow).not.toMatch(/contents: write|actions: read|secrets\.|AZURE_|TAURI_SIGNING|gh release/);
    const uses = [...workflow.matchAll(/^\s*-?\s*uses:\s*([^\s#]+)/gm)].map((match) => match[1]);
    expect(uses).toEqual([approvedActionUses[0], approvedActionUses[0], approvedActionUses[1]]);
    expect(workflow).toContain("$env:GITHUB_REF -cne 'refs/heads/master'");
    expect(workflow.match(/persist-credentials: false/g)).toHaveLength(2);
    expect(workflow.match(/github\.token/g)).toHaveLength(1);
    const download = stepSlice(workflow, 'Download the exact published release installer');
    expect(download).toContain('GITHUB_TOKEN: ${{ github.token }}');
    expect(download).toContain('$release.draft -isnot [bool] -or $release.draft');
    expect(download).toContain('$release.prerelease -isnot [bool] -or $release.prerelease');
    expect(download).toContain("('sha256:' + $env:INSTALLER_SHA256.ToLowerInvariant())");
    const verify = stepSlice(workflow, 'Verify fresh silent install and installed OCR');
    expect(verify).not.toMatch(/github\.token|GITHUB_TOKEN|GH_TOKEN/);
    expect(verify).toContain('./scripts/verify-signed-ocr-upgrade.ps1');
    expect(verify).toContain('-PublishedInstaller (Join-Path $env:PUBLISHED_ROOT $env:INSTALLER_NAME)');
    expect(verify).toContain('-PublisherUiMode HostedNonInteractive');
    expect(verify).not.toMatch(/-BaselineInstaller|-SignedArtifactRoot|-ArtifactId/);
    const source = stepSlice(workflow, 'Verify the exact clean published source');
    expect(source).toContain('rev-parse "$($env:TARGET_TAG)^{commit}"');
    const upload = workflow.slice(workflow.indexOf('- name: Upload only the sanitized published verification record'));
    expect(upload).toContain('${{ env.PUBLISHED_OUTPUT_ROOT }}/published-ocr-verification.json');
    expect(upload).not.toMatch(/\.exe|PUBLISHED_ROOT\b|PUBLISHED_WORK_ROOT/);
    expect(workflow.indexOf('Verify the exact clean published source')).toBeLessThan(workflow.indexOf('Prepare exact pinned PDFium resources in the published source'));
  }, 15_000);

  it('executes the exact path initialization block under PowerShell 7', () => {
    const block = workflowRunBlock(readFileSync(workflowPath, 'utf8'), 'Initialize fresh runner paths');
    const root = resolve(`target/ocr-published-init-${randomUUID()}`);
    const runnerTemp = resolve(root, 'runner-temp');
    const githubEnv = resolve(root, 'github-env');
    mkdirSync(runnerTemp, { recursive: true });
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(block, 'utf16le').toString('base64')], {
      encoding: 'utf8',
      timeout: 30_000,
      env: { ...process.env, RUNNER_TEMP: runnerTemp, GITHUB_ENV: githubEnv },
    });
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const bytes = readFileSync(githubEnv);
    expect([...bytes.slice(0, 3)]).not.toEqual([0xef, 0xbb, 0xbf]);
    expect(bytes.toString('utf8').trimEnd().split(/\r?\n/)).toEqual([
      `PUBLISHED_ROOT=${runnerTemp}\\pdf-workstation-published`,
      `PUBLISHED_WORK_ROOT=${runnerTemp}\\pdf-workstation-published-work`,
      `PUBLISHED_OUTPUT_ROOT=${runnerTemp}\\pdf-workstation-published-output`,
      `WEBDRIVER_ROOT=${runnerTemp}\\pdf-workstation-webdriver`,
      `WEBVIEW_PROFILE_ROOT=${runnerTemp}\\pdf-workstation-webview-profile`,
    ]);
  }, 40_000);

  it('downloads only an exact published stable release asset and rejects metadata drift', () => {
    const block = workflowRunBlock(readFileSync(workflowPath, 'utf8'), 'Download the exact published release installer');
    const root = resolve(`target/ocr-published-download-${randomUUID()}`);
    mkdirSync(root, { recursive: true });
    const payload = resolve(root, 'payload.bin');
    writeFileSync(payload, 'published-installer-fixture');
    const harness = String.raw`
      $ErrorActionPreference = 'Stop'
      $payload = '${payload.replaceAll("'", "''")}'
      $sha = (Get-FileHash -LiteralPath $payload -Algorithm SHA256).Hash
      $bytes = [string](Get-Item -LiteralPath $payload).Length
      $block = [IO.File]::ReadAllText('${resolve(root, 'block.ps1').replaceAll("'", "''")}')
      function New-Release {
        [pscustomobject]@{ id = [int64]425998245; tag_name = 'v0.3.1'; draft = $false; prerelease = $false; published_at = '2026-10-27T21:36:00Z'
          assets = @([pscustomobject]@{ id = [int64]682909274; name = 'PDF.Workstation_0.3.1_x64-setup.exe'; size = [int64]$bytes; state = 'uploaded'; digest = ('sha256:' + $sha.ToLowerInvariant()) }) }
      }
      function Invoke-Case {
        param([scriptblock]$Mutate, [string]$Label, [bool]$Expect, [string]$DownloadOverride)
        $script:release = New-Release
        if ($Mutate) { & $Mutate $script:release }
        $script:downloadSource = if ($DownloadOverride) { $DownloadOverride } else { $payload }
        $env:PUBLISHED_ROOT = Join-Path '${root.replaceAll("'", "''")}' ('case-' + [guid]::NewGuid())
        $env:GITHUB_TOKEN = 'fixture-token'
        $env:TARGET_TAG = 'v0.3.1'; $env:RELEASE_ID = '425998245'; $env:INSTALLER_ASSET_ID = '682909274'
        $env:INSTALLER_NAME = 'PDF.Workstation_0.3.1_x64-setup.exe'; $env:INSTALLER_BYTES = $bytes; $env:INSTALLER_SHA256 = $sha
        function global:Invoke-RestMethod { param($Uri, $Headers) if ($Uri -cne 'https://api.github.com/repos/Joshua-Beel/PDF-Workstation/releases/425998245' -or $Headers.Accept -cne 'application/vnd.github+json') { throw 'Unexpected metadata request.' }; return $script:release }
        function global:Invoke-WebRequest { param($Uri, $Headers, $OutFile) if ($Uri -cne 'https://api.github.com/repos/Joshua-Beel/PDF-Workstation/releases/assets/682909274' -or $Headers.Accept -cne 'application/octet-stream') { throw 'Unexpected download request.' }; [IO.File]::Copy($script:downloadSource, $OutFile) }
        $accepted = $true
        try { & ([scriptblock]::Create($block)) } catch { $accepted = $false }
        if ($accepted -ne $Expect) { throw ('Download case outcome changed: ' + $Label) }
      }
      Invoke-Case -Label 'exact' -Expect $true
      Invoke-Case -Label 'draft' -Expect $false -Mutate { param($r) $r.draft = $true }
      Invoke-Case -Label 'prerelease' -Expect $false -Mutate { param($r) $r.prerelease = $true }
      Invoke-Case -Label 'unpublished' -Expect $false -Mutate { param($r) $r.published_at = $null }
      Invoke-Case -Label 'tag' -Expect $false -Mutate { param($r) $r.tag_name = 'v0.3.0' }
      Invoke-Case -Label 'release id' -Expect $false -Mutate { param($r) $r.id = [int64]425998246 }
      Invoke-Case -Label 'name' -Expect $false -Mutate { param($r) $r.assets[0].name = 'PDF Workstation_0.3.1_x64-setup.exe' }
      Invoke-Case -Label 'size' -Expect $false -Mutate { param($r) $r.assets[0].size = [int64]1 }
      Invoke-Case -Label 'digest' -Expect $false -Mutate { param($r) $r.assets[0].digest = ('sha256:' + ('0' * 64)) }
      Invoke-Case -Label 'state' -Expect $false -Mutate { param($r) $r.assets[0].state = 'starter' }
      Invoke-Case -Label 'duplicate' -Expect $false -Mutate { param($r) $r.assets = @($r.assets[0], $r.assets[0]) }
      Invoke-Case -Label 'missing' -Expect $false -Mutate { param($r) $r.assets = @() }
      $tampered = Join-Path '${root.replaceAll("'", "''")}' 'tampered.bin'; [IO.File]::WriteAllText($tampered, 'published-installer-fixturE')
      Invoke-Case -Label 'tampered download' -Expect $false -DownloadOverride $tampered
    `;
    writeFileSync(resolve(root, 'block.ps1'), block, 'utf8');
    const result = runPwsh(harness);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);

  it('binds the existing upgrade invocation and the published invocation to separate parameter sets', () => {
    const result = runPwsh(String.raw`
      $ErrorActionPreference = 'Stop'
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      $command = Get-Command (Resolve-Path ./${verifierPath}).Path
      if (@($command.ParameterSets | Where-Object IsDefault | ForEach-Object Name) -join ',' -cne 'Upgrade') { throw 'Upgrade must stay the default parameter set.' }
      $sets = @{}; foreach ($set in $command.ParameterSets) { $sets[$set.Name] = @($set.Parameters | Where-Object IsMandatory | ForEach-Object Name | Sort-Object) }
      if (@($sets.Keys | Sort-Object) -join ',' -cne 'Published,Upgrade') { throw 'Unexpected parameter sets.' }
      foreach ($name in @('PublishedInstaller','PublishedTag','PublishedReleaseId','PublishedInstallerAssetId','InstallerName','InstallerBytes','InstallerSha256','SourceRevision','TargetVersion','TargetSourceRoot','PublisherUiMode','WebDriverRoot','WebViewProfileRoot','WorkRoot','OutputRoot')) { if ($sets['Published'] -cnotcontains $name) { throw ('Published set lacks ' + $name) } }
      foreach ($name in @('BaselineInstaller','SignedArtifactRoot','ArtifactRunId','ArtifactId','ArtifactName','ArtifactBytes','ArtifactDigest','RecordBytes','RecordSha256')) { if ($sets['Published'] -ccontains $name -or $sets['Upgrade'] -cnotcontains $name) { throw ('Set membership changed for ' + $name) } }
      foreach ($name in @('PublishedInstaller','PublishedTag','PublishedReleaseId','PublishedInstallerAssetId')) { if ($sets['Upgrade'] -ccontains $name) { throw ('Upgrade set gained ' + $name) } }
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const upgrade = readFileSync('.github/workflows/ocr-installer-upgrade.yml', 'utf8');
    expect(upgrade).toContain('-BaselineInstaller');
    expect(upgrade).not.toContain('-PublishedInstaller');
    const verifier = readFileSync(verifierPath, 'utf8');
    const dispatch = verifier.indexOf("if ($PSCmdlet.ParameterSetName -ceq 'Published')");
    expect(dispatch).toBeGreaterThan(verifier.indexOf('Assert-HostedRunnerFacts -Facts $runnerFacts'));
    expect(dispatch).toBeLessThan(verifier.indexOf('$signedInputs = Get-SignedCheckInputs'));
    expect(verifier.match(/'[a-f0-9]{40}'/g)).toBeNull();
    expect(verifier).not.toMatch(/^\s*\$(matches|home)\s*=/im);
  }, 40_000);

  it('accepts the exact published 0.3.1 inputs and rejects malformed ones', () => {
    const result = runPwsh(verifierHarness(['Assert-PublishedInstallerInputs'], String.raw`
      $good = @{ TargetVersion='0.3.1'; PublishedTag='v0.3.1'; SourceRevision='${publishedInputs.SOURCE_REVISION}'; PublishedReleaseId='425998245'; PublishedInstallerAssetId='682909274'; InstallerName='PDF.Workstation_0.3.1_x64-setup.exe'; InstallerBytes='11210976'; InstallerSha256='${publishedInputs.INSTALLER_SHA256}' }
      $value = Assert-PublishedInstallerInputs @good
      if ($value.ReleaseId -ne [uint64]425998245 -or $value.InstallerAssetId -ne [uint64]682909274 -or $value.InstallerBytes -ne [uint64]11210976 -or $value.Tag -cne 'v0.3.1') { throw 'Parsed inputs changed.' }
      foreach ($mutation in @(
        @{TargetVersion='0.3'}, @{TargetVersion='0.3.1-rc1'}, @{PublishedTag='0.3.1'}, @{PublishedTag='v0.3.2'},
        @{SourceRevision='88bff90'}, @{SourceRevision=('A'*40)}, @{PublishedReleaseId='0'}, @{PublishedReleaseId='-1'},
        @{PublishedInstallerAssetId='68290927x'}, @{InstallerName='PDF Workstation_0.3.1_x64-setup.exe'},
        @{InstallerBytes='0'}, @{InstallerSha256=('67c685264abc094246e95c04629bfff5d4854bd4fe5b84fede8760261a119038')}
      )) {
        $copy = $good.Clone(); foreach ($key in $mutation.Keys) { $copy[$key] = $mutation[$key] }
        Assert-Rejected { Assert-PublishedInstallerInputs @copy } (($mutation.Keys | Select-Object -First 1) + '=' + ($mutation.Values | Select-Object -First 1))
      }
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);

  it('derives receipts from the tagged source, OCR pins and engine identity, then passes the shared inventory check', () => {
    const root = resolve(`target/ocr-published-receipts-${randomUUID()}`).replaceAll("'", "''");
    const result = runPwsh(verifierHarness(
      ['Assert-ExactProperties','Assert-ReceiptValue','Get-UniqueResource','Assert-ReceiptFileObject','Assert-ExtractedInstallerInventory','Assert-ExtractedSignedArtifact','Get-PublishedReceiptSets'],
      String.raw`
      $root = '${root}'
      $signature = { param($path) [pscustomobject]@{ Status='Valid'; Publisher='Joshua Beel'; HasTimestamp=$true } }
      function Write-Fixture { param([string]$Path, [string]$Text) [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null; [IO.File]::WriteAllText($Path, $Text, [Text.UTF8Encoding]::new($false)) }
      function New-Case {
        param([string]$Name, [hashtable]$Options = @{})
        $case = Join-Path $root $Name; $source = Join-Path $case 'source'; $extract = Join-Path $case 'extract'
        $engineText = 'signed-engine-fixture'
        Write-Fixture (Join-Path $case 'engine.tmp') $engineText
        $engineSha = Get-ExactSha256 -Path (Join-Path $case 'engine.tmp')
        $identity = if ($Options['Identity']) { $Options['Identity'] } else { $engineSha.ToLowerInvariant() }
        $marker = if ($Options['NoMarker']) { 'no identity here' } else { 'build identity ' + $engineSha }
        Write-Fixture (Join-Path $extract 'pdf-workstation.exe') $marker
        Write-Fixture (Join-Path $source 'resources/welcome.pdf') '%PDF-welcome'
        Write-Fixture (Join-Path $extract 'resources/welcome.pdf') $(if ($Options['WelcomeDrift']) { '%PDF-welcomE' } else { '%PDF-welcome' })
        Write-Fixture (Join-Path $source 'resources/pdfium/bin/pdfium.dll') 'unsigned-pdfium'
        Write-Fixture (Join-Path $extract 'resources/pdfium/bin/pdfium.dll') 'signed-pdfium'
        Write-Fixture (Join-Path $extract "resources/ocr/$identity/bin/tesseract.exe") $engineText
        $pins = [ordered]@{}
        foreach ($entry in @(@('tessdata/eng.traineddata','model-bytes'),@('licenses/Tesseract-Apache-2.0.txt','tesseract-license'),@('licenses/Leptonica-BSD-2-Clause.txt','leptonica-license'),@('licenses/eng-fast-Apache-2.0.txt','model-license'))) {
          $path = Join-Path $extract "resources/ocr/$identity/$($entry[0])"
          Write-Fixture $path $entry[1]
          $pins[$entry[0]] = [pscustomobject]@{ bytes = [uint64](Get-Item -LiteralPath $path).Length; sha256 = Get-ExactSha256 -Path $path }
        }
        if ($Options['ModelDrift']) { Write-Fixture (Join-Path $extract "resources/ocr/$identity/tessdata/eng.traineddata") 'model-bytez' }
        if ($Options['ExtraOcr']) { Write-Fixture (Join-Path $extract "resources/ocr/$identity/bin/extra.dll") 'extra' }
        foreach ($plugin in @('modern-wizard.bmp','nsDialogs.dll','nsis_tauri_utils.dll','NSISdl.dll','StartMenu.dll','System.dll')) { Write-Fixture (Join-Path $extract ('$PLUGINSDIR/' + $plugin)) $plugin }
        $model = $pins['tessdata/eng.traineddata']
        $ocrPins = [pscustomobject]@{
          model = [pscustomobject]@{ data = [pscustomobject]@{ file = 'eng.traineddata'; bytes = $model.bytes; sha256 = $model.sha256 }; license = [pscustomobject]@{ output = 'eng-fast-Apache-2.0.txt'; bytes = $pins['licenses/eng-fast-Apache-2.0.txt'].bytes; sha256 = $pins['licenses/eng-fast-Apache-2.0.txt'].sha256 } }
          tesseract = [pscustomobject]@{ license = [pscustomobject]@{ output = 'Tesseract-Apache-2.0.txt'; bytes = $pins['licenses/Tesseract-Apache-2.0.txt'].bytes; sha256 = $pins['licenses/Tesseract-Apache-2.0.txt'].sha256 } }
          leptonica = [pscustomobject]@{ license = [pscustomobject]@{ output = 'Leptonica-BSD-2-Clause.txt'; bytes = $pins['licenses/Leptonica-BSD-2-Clause.txt'].bytes; sha256 = $pins['licenses/Leptonica-BSD-2-Clause.txt'].sha256 } }
        }
        $base = @(
          [pscustomobject]@{ Source = (Join-Path $source 'resources/welcome.pdf'); Target = 'resources/welcome.pdf'; Receipt = [pscustomobject]@{ Bytes = [uint64](Get-Item -LiteralPath (Join-Path $source 'resources/welcome.pdf')).Length; Sha256 = Get-ExactSha256 -Path (Join-Path $source 'resources/welcome.pdf') } },
          [pscustomobject]@{ Source = (Join-Path $source 'resources/pdfium/bin/pdfium.dll'); Target = 'resources/pdfium/bin/pdfium.dll'; Receipt = [pscustomobject]@{ Bytes = [uint64](Get-Item -LiteralPath (Join-Path $source 'resources/pdfium/bin/pdfium.dll')).Length; Sha256 = Get-ExactSha256 -Path (Join-Path $source 'resources/pdfium/bin/pdfium.dll') } }
        )
        return [pscustomobject]@{ Extract = $extract; Base = $base; Pins = $ocrPins; EngineSha = $engineSha; Files = @(Get-ChildItem -LiteralPath $extract -Recurse -File -Force) }
      }
      $script:pdfiumCalls = 0
      $pdfiumProvider = { param($unsigned, $signed) if (-not $unsigned.EndsWith('pdfium.dll') -or -not $signed.EndsWith('pdfium.dll')) { throw 'PDFium pair changed.' }; $script:pdfiumCalls++ }
      $good = New-Case 'good'
      $published = Get-PublishedReceiptSets -Files $good.Files -ExtractionRoot $good.Extract -BaseEntries $good.Base -OcrPins $good.Pins -SignatureProvider $signature -PdfiumEquivalenceProvider $pdfiumProvider
      if ($published.Identity -cne $good.EngineSha.ToLowerInvariant() -or $published.Sets.Engine.sha256 -cne $good.EngineSha -or @($published.Sets.Ocr).Count -ne 5 -or @($published.Sets.Base).Count -ne 2 -or $script:pdfiumCalls -ne 1) { throw 'Published receipt sets changed shape.' }
      if ($published.Sets.Pdfium.unsignedSha256 -cne $good.Base[1].Receipt.Sha256 -or $published.Sets.Pdfium.sha256 -ceq $good.Base[1].Receipt.Sha256) { throw 'PDFium receipt does not separate signed and unsigned bytes.' }
      $null = Assert-ExtractedSignedArtifact -Files $good.Files -ExtractionRoot $good.Extract -Receipt $published.Receipt -ReceiptSets $published.Sets -SignatureProvider $signature

      foreach ($bad in @(
        @{ Name='identity'; Options=@{ Identity=('a'*64) } }, @{ Name='marker'; Options=@{ NoMarker=$true } },
        @{ Name='model'; Options=@{ ModelDrift=$true } }, @{ Name='welcome'; Options=@{ WelcomeDrift=$true } }, @{ Name='extra'; Options=@{ ExtraOcr=$true } }
      )) {
        $case = New-Case $bad.Name $bad.Options
        Assert-Rejected { Get-PublishedReceiptSets -Files $case.Files -ExtractionRoot $case.Extract -BaseEntries $case.Base -OcrPins $case.Pins -SignatureProvider $signature -PdfiumEquivalenceProvider $pdfiumProvider } $bad.Name
      }
      $unsigned = { param($path) [pscustomobject]@{ Status='NotSigned'; Publisher=''; HasTimestamp=$false } }
      Assert-Rejected { Get-PublishedReceiptSets -Files $good.Files -ExtractionRoot $good.Extract -BaseEntries $good.Base -OcrPins $good.Pins -SignatureProvider $unsigned -PdfiumEquivalenceProvider $pdfiumProvider } 'unsigned'
      $otherPublisher = { param($path) [pscustomobject]@{ Status='Valid'; Publisher='Someone Else'; HasTimestamp=$true } }
      Assert-Rejected { Get-PublishedReceiptSets -Files $good.Files -ExtractionRoot $good.Extract -BaseEntries $good.Base -OcrPins $good.Pins -SignatureProvider $otherPublisher -PdfiumEquivalenceProvider $pdfiumProvider } 'publisher'
      Assert-Rejected { Get-PublishedReceiptSets -Files $good.Files -ExtractionRoot $good.Extract -BaseEntries $good.Base -OcrPins $good.Pins -SignatureProvider $signature -PdfiumEquivalenceProvider { param($u,$s) throw 'not equivalent' } } 'pdfium'
      $badPins = $good.Pins | ConvertTo-Json -Depth 5 | ConvertFrom-Json; $badPins.model.data.file = 'other.traineddata'
      Assert-Rejected { Get-PublishedReceiptSets -Files $good.Files -ExtractionRoot $good.Extract -BaseEntries $good.Base -OcrPins $badPins -SignatureProvider $signature -PdfiumEquivalenceProvider $pdfiumProvider } 'pins'
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);

  it('writes one sanitized published record without recognized text and with explicit non-claims', () => {
    const root = resolve(`target/ocr-published-record-${randomUUID()}`).replaceAll("'", "''");
    const result = runPwsh(verifierHarness(['Assert-ExactProperties','Assert-HostedPublisherEvidence','Write-SanitizedPublishedRecord'], String.raw`
      $root = '${root}'
      $engineSha = ('B' * 64)
      $inputs = [pscustomobject]@{ TargetVersion='0.3.1'; Tag='v0.3.1'; SourceRevision='${publishedInputs.SOURCE_REVISION}'; ReleaseId=[uint64]425998245; InstallerAssetId=[uint64]682909274; InstallerName='PDF.Workstation_0.3.1_x64-setup.exe'; InstallerBytes=[uint64]11210976; InstallerSha256='${publishedInputs.INSTALLER_SHA256}' }
      $published = [pscustomobject]@{
        Identity = $engineSha.ToLowerInvariant()
        Receipt = [pscustomobject]@{ packagedApplication = [pscustomobject]@{ bytes=[uint64]21114152; sha256=('C'*64) } }
        Sets = [pscustomobject]@{ Base=@([pscustomobject]@{target='resources/welcome.pdf';bytes=[uint64]10;sha256=('D'*64)}); Ocr=@([pscustomobject]@{suffix='tessdata/eng.traineddata';role='model';bytes=[uint64]4113088;sha256=('E'*64)},[pscustomobject]@{suffix='bin/tesseract.exe';role='engine';bytes=[uint64]5;sha256=$engineSha}); Engine=[pscustomobject]@{suffix='bin/tesseract.exe';role='engine';bytes=[uint64]5;sha256=$engineSha} }
      }
      function New-Evidence { param($Kind,$Name,$Bytes,$Sha) [pscustomobject][ordered]@{ kind=$Kind; fileName=$Name; bytes=[uint64]$Bytes; sha256=$Sha; publisher='Joshua Beel'; authenticodeStatus='Valid'; trustedTimestampVerified=$true; shellPublisherUiAttempted=$false; shellPublisherUiVerified=$false; notAttemptedReason='github-hosted-noninteractive-session'; screenshotsUsed=$false } }
      $installer = New-Evidence 'installer' $inputs.InstallerName 11210976 $inputs.InstallerSha256
      $application = New-Evidence 'installed-application' 'pdf-workstation.exe' 21114152 ('C'*64)
      $launch = [ordered]@{ sampleName='welcome.pdf'; ocrPage=1; ocrTextUtf8Bytes=120; ocrTextSha256=('F'*64) }
      $smoke = [ordered]@{ matched=$true; actualTextSha256=('A'*64) }
      $recordArgs = @{ WorkflowSourceRevision=('a'*40); Inputs=$inputs; Published=$published; InstalledEngineSmoke=$smoke; InstalledAppLaunch=$launch; InstallerPublisherEvidence=$installer; ApplicationPublisherEvidence=$application }
      $path = Write-SanitizedPublishedRecord -OutputRoot (Join-Path $root 'out') @recordArgs
      $record = Get-Content -LiteralPath $path -Raw -Encoding UTF8 | ConvertFrom-Json
      if ($record.schemaVersion -ne 1 -or $record.mode -cne 'ephemeral-current-user-silent-published-install' -or
          $record.published.releaseId -ne 425998245 -or $record.published.installerAssetId -ne 682909274 -or
          $record.published.ocrEngine.identity -cne $engineSha.ToLowerInvariant() -or $record.published.ocrEngine.signature -cne 'Valid' -or
          $record.verification.baselineUpgradePerformed -or -not $record.verification.applicationCurrentPageOcrCompleted -or
          $record.verification.applicationOcrTextRecorded -or $record.verification.applicationOcrAccuracyVerified -or
          $record.verification.applicationSearchableOcrVerified -or $record.verification.publisherUiAttempted -or $record.verification.inAppUpdaterVerified) {
        throw 'Published record claims changed.'
      }
      $text = [IO.File]::ReadAllText($path)
      if ($text -match '"text"\s*:' -or $text -notmatch 'searchable OCR') { throw 'Published record carries text or drops its scope limits.' }
      Assert-Rejected { Write-SanitizedPublishedRecord -OutputRoot (Join-Path $root 'out') @recordArgs } 'nonfresh output'
      $leak = $recordArgs.Clone(); $leak.InstalledAppLaunch = [ordered]@{ sampleName='C:\Users\runneradmin\welcome.pdf' }
      Assert-Rejected { Write-SanitizedPublishedRecord -OutputRoot (Join-Path $root 'leak') @leak } 'host path'
      $badEvidence = $recordArgs.Clone(); $badEvidence.InstallerPublisherEvidence = New-Evidence 'installer' 'PDF Workstation_0.3.1_x64-setup.exe' 11210976 $inputs.InstallerSha256
      Assert-Rejected { Write-SanitizedPublishedRecord -OutputRoot (Join-Path $root 'evidence') @badEvidence } 'installer evidence name'
      $badIdentity = $recordArgs.Clone(); $badIdentity.Published = $published.PSObject.Copy(); $badIdentity.Published.Identity = ('c'*64)
      Assert-Rejected { Write-SanitizedPublishedRecord -OutputRoot (Join-Path $root 'identity') @badIdentity } 'identity'
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);
});
