import { describe, expect, it } from 'vitest';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';

const workflow = readFileSync('.github/workflows/default-rehearsal-verification.yml', 'utf8').replace(/\r\n?/g, '\n');
const expanded = readFileSync('scripts/verify-default-expanded-page-tools.ps1', 'utf8').replace(/\r\n?/g, '\n');
const commentsForms = readFileSync('scripts/verify-default-comments-forms.ps1', 'utf8').replace(/\r\n?/g, '\n');
const recovery = readFileSync('scripts/verify-default-recovery.ps1', 'utf8').replace(/\r\n?/g, '\n');
const extraction = readFileSync('scripts/verify-default-artifact.ps1', 'utf8').replace(/\r\n?/g, '\n');
const cwd = process.cwd().replaceAll("'", "''");

function runPowerShell(source: string, timeout = 60_000) {
  const root = `target/default-rehearsal-test-${randomUUID()}`; mkdirSync(root, { recursive: true });
  const path = `${root}/run.ps1`; writeFileSync(path, source, 'utf8');
  try { return spawnSync('pwsh', ['-NoProfile', '-NonInteractive', '-File', path], { encoding: 'utf8', timeout }); }
  finally { rmSync(root, { recursive: true, force: true }); }
}

const importer = String.raw`
function Import-TestFunctions { param([string]$Path,[string[]]$Names)
 $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path $Path),[ref]$tokens,[ref]$errors);if($errors.Count){throw ($errors|Out-String)}
 foreach($name in $Names){$node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true);if(-not$node){throw ('missing '+$name)};Set-Item -Path ('Function:global:'+$name) -Value $node.Body.GetScriptBlock()}
}
function Reject([scriptblock]$Action,[string]$Kind){$bad=$false;try{&$Action}catch{$bad=$true};if(-not$bad){throw ($Kind+' accepted')}}
`;

const receiptFixture = String.raw`
$WorkflowSourceRevision='1'*40;$TargetVersion='0.3.1';$TargetSourceRevision='2'*40;$ExpectedPublisher='Publisher'
$ArtifactRunId='31';$ArtifactId='32';$ArtifactName=('signed-ocr-installer-'+$TargetSourceRevision);$ArtifactBytes='33';$ArtifactDigest=('sha256:'+('a'*64))
$InstallerName='PDF Workstation_0.3.1_x64-setup.exe';$InstallerBytes='10';$InstallerSha256='A'*64
$base=@();for($i=0;$i-lt27;$i++){$base+=[pscustomobject]@{target=('resources/licenses/'+$i+'.txt');bytes=[uint64]1;sha256=('D'*64)}};$base+=[pscustomobject]@{target='resources/pdfium/bin/pdfium.dll';bytes=[uint64]2;sha256=('E'*64);unsignedBytes=[uint64]1;unsignedSha256=('F'*64)}
$ocrId='e'*64;$ocrResources=@([pscustomobject][ordered]@{target="resources/ocr/$ocrId/bin/tesseract.exe";role='engine';bytes=[uint64]3;sha256=('E'*64)})+@(@(@('tessdata/eng.traineddata','model'),@('licenses/Tesseract-Apache-2.0.txt','license'),@('licenses/Leptonica-BSD-2-Clause.txt','license'),@('licenses/eng-fast-Apache-2.0.txt','license'))|ForEach-Object{[pscustomobject][ordered]@{target="resources/ocr/$ocrId/$($_[0])";role=$_[1];bytes=[uint64]4;sha256=('7'*64)}})
$receipt=[pscustomobject][ordered]@{schemaVersion=2;scope='scope';mode='default-artifact-extraction';workflowSourceRevision=$WorkflowSourceRevision;targetVersion=$TargetVersion;targetSourceRevision=$TargetSourceRevision;artifact=[pscustomobject][ordered]@{runId=[uint64]31;id=[uint64]32;name=$ArtifactName;bytes=[uint64]33;digest=$ArtifactDigest};assets=@([pscustomobject][ordered]@{role='installer';name=$InstallerName;bytes=[uint64]10;sha256=$InstallerSha256});packagedApplication=[pscustomobject]@{bytes=[uint64]20;sha256=('9'*64)};baseResources=$base;ocr=[pscustomobject][ordered]@{identity=$ocrId;resources=$ocrResources};signatures=[pscustomobject]@{expectedPublisher=$ExpectedPublisher;installer='Valid';application='Valid';pdfium='Valid';ocrEngine='Valid';trustedTimestampsRequired=$true};verification=[pscustomobject][ordered]@{draft=$false;releaseCandidateArtifact=$true;exactAssetInventory=$true;exactExtractedInventory=$true;targetSourceSixVersionsMatched=$true;sourceBuildExecutableByteProvenanceReconstructed=$false;packagedApplicationIdentityAndVersionMatched=$true;requiredExtractedFiles=40;actualExtractedFiles=40;optionalUninstaller=$false;optionalUninstallerExcluded=$true;ocrBundled=$true;packagedApplicationEmbedsCandidateUpdaterKey=$true;updaterArtifactsAbsent=$true;installedBehaviorVerified=$false}}
function Copy-Receipt {return ($receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json)}
`;

describe('pre-tag default rehearsal workflow', () => {
  it('is manual, master-only, read-only, secret-free, pinned and fail-closed on the release candidate', () => {
    expect(workflow).toContain('workflow_dispatch:'); expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule|release):/m);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('permissions:\n  contents: read\n  actions: read\n');
    expect(workflow.match(/^\s*permissions:/gm)).toHaveLength(1);
    expect(workflow).not.toMatch(/:\s*write\b/);
    expect(workflow).not.toMatch(/secrets\.|AZURE_|TAURI_SIGNING|artifact-signing|gh release|cargo |npm run build|setup-dotnet|environment:/);
    expect(workflow).toContain("if ($env:GITHUB_REPOSITORY -cne 'Joshua-Beel/PDF-Workstation' -or $env:GITHUB_EVENT_NAME -cne 'workflow_dispatch' -or $env:GITHUB_REF -cne 'refs/heads/master') { throw 'Dispatch only from master.' }");
    expect(workflow).toContain("git merge-base --is-ancestor $env:TARGET_SHA HEAD;if($LASTEXITCODE-ne0){throw 'The release candidate is not on master.'}");
    for (const name of ['check', 'target_version', 'target_source_revision', 'artifact_run_id', 'artifact_id', 'artifact_name', 'artifact_bytes', 'artifact_digest', 'installer_name', 'installer_bytes', 'installer_sha256', 'expected_publisher']) expect(workflow).toMatch(new RegExp(`^      ${name}:`, 'm'));
    expect(workflow).not.toMatch(/target_tag|target_release_id|draft_proof_|signature_asset_id|manifest_asset_id/);
    const uses = [...workflow.matchAll(/^\s*-?\s*uses:\s*([^\s#]+)\s*$/gm)].map(x => x[1]);
    expect(uses).toEqual(['actions/checkout@11d5960a326750d5838078e36cf38b85af677262', 'actions/checkout@11d5960a326750d5838078e36cf38b85af677262', 'actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093', 'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02']);
    expect(workflow.match(/persist-credentials: false/g)).toHaveLength(2);
    expect(workflow).toContain('artifact-ids: ${{ inputs.artifact_id }}'); expect(workflow).toContain('run-id: ${{ inputs.artifact_run_id }}'); expect(workflow).toContain('merge-multiple: true');
    for (const line of workflow.split('\n').filter(l => l.includes('${{ inputs.'))) expect(line, line).toMatch(/^\s+(-\s+)?[A-Za-z_-]+:\s|^\s+env: \{/);
    expect(workflow.match(/GITHUB_TOKEN: \$\{\{ github\.token \}\}/g)).toHaveLength(1);
    const order = ['./scripts/assert-signed-artifact-metadata.ps1', 'actions/download-artifact@', './scripts/prepare-default-draft-pdfium.ps1', './scripts/setup-installed-app-webdriver.ps1', './scripts/verify-default-artifact.ps1', "ProofSource='artifact'", 'Recheck clean tracked sources', 'actions/upload-artifact@'].map(x => workflow.indexOf(x));
    expect(order.every(x => x > 0)).toBe(true); expect([...order].sort((a, b) => a - b)).toEqual(order);
    expect(workflow.indexOf('GITHUB_TOKEN: ${{ github.token }}')).toBeLessThan(workflow.indexOf('./scripts/assert-signed-artifact-metadata.ps1'));
    for (const script of ['verify-default-expanded-page-tools.ps1', 'verify-default-comments-forms.ps1', 'verify-default-recovery.ps1']) expect(workflow).toContain(`./scripts/${script} @common`);
    expect(workflow).toContain('*-rehearsal.json'); expect(workflow).not.toContain('-verification.json\n          if-no');
  });

  it('leaves the three real Step 2 draft workflows on the default draft mode with every draft input', () => {
    for (const path of ['default-expanded-page-tools-verification.yml', 'default-comments-forms-verification.yml', 'default-recovery-verification.yml']) {
      const text = readFileSync(`.github/workflows/${path}`, 'utf8');
      expect(text).not.toMatch(/ProofSource|ArtifactRoot|ArtifactRunId/);
      for (const flag of ['-DraftProofRunId', '-DraftProofReceiptSha256', '-TargetTag', '-TargetReleaseId', '-SignatureAssetId', '-ManifestAssetId']) expect(text).toContain(flag);
    }
    for (const verifier of [expanded, commentsForms, recovery]) {
      expect(verifier).toContain("[ValidateSet('draft','artifact')][string]$ProofSource='draft'");
      expect(verifier.indexOf('Assert-ProofSourceParameters\nif($ProofSource-ceq\'draft\'){')).toBeGreaterThan(0);
    }
    expect(expanded).toContain('Assert-IndependentReceiptParity $receipt $independent');
    expect(expanded).toContain("'default-expanded-page-tools-verification.json'");
    expect(expanded).toContain("'default-expanded-page-tools-rehearsal.json'");
    expect(commentsForms).toContain("'default-comments-forms-verification.json'"); expect(commentsForms).toContain("'default-comments-forms-rehearsal.json'");
    expect(recovery).toContain("'default-recovery-verification.json'"); expect(recovery).toContain("'default-recovery-rehearsal.json'");
    const rehearsalRecord = expanded.slice(expanded.indexOf('function Write-RehearsalInstalledRecord'), expanded.indexOf('\n}\n', expanded.indexOf('function Write-RehearsalInstalledRecord')));
    expect(rehearsalRecord).not.toMatch(/step1=|draft=\[ordered\]|independentlyRedownloaded/);
    expect(rehearsalRecord).toContain("scope='Pre-tag rehearsal:");
  });

  it('fails closed when draft and rehearsal inputs are mixed or missing', () => {
    const check = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest;Set-Location -LiteralPath '${cwd}'
      ${importer}
      Import-TestFunctions ./scripts/verify-default-expanded-page-tools.ps1 @('Assert-ProofSourceParameters')
      $draftNames=@('AssetRoot','DraftProofWorkflowRevision','DraftProofRunId','DraftProofWorkflowId','DraftProofArtifactId','DraftProofArtifactName','DraftProofArtifactBytes','DraftProofArtifactDigest','DraftProofReceiptBytes','DraftProofReceiptSha256','TargetTag','TargetReleaseId','InstallerAssetId','SignatureAssetId','SignatureBytes','SignatureSha256','ManifestAssetId','ManifestBytes','ManifestSha256')
      $artifactNames=@('ArtifactRoot','ArtifactRunId','ArtifactId','ArtifactName','ArtifactBytes','ArtifactDigest')
      function Set-Mode([string]$Mode,[string[]]$Present){foreach($n in $draftNames+$artifactNames){Set-Variable -Scope Global -Name $n -Value $(if($Present-contains$n){'x'}else{''})};Set-Variable -Scope Global -Name ProofSource -Value $Mode}
      Set-Mode 'draft' $draftNames;Assert-ProofSourceParameters
      Set-Mode 'artifact' $artifactNames;Assert-ProofSourceParameters
      foreach($n in $draftNames){Set-Mode 'draft' @($draftNames|Where-Object{$_-cne$n});Reject {Assert-ProofSourceParameters} ('draft without '+$n);Set-Mode 'artifact' ($artifactNames+$n);Reject {Assert-ProofSourceParameters} ('artifact with '+$n)}
      foreach($n in $artifactNames){Set-Mode 'artifact' @($artifactNames|Where-Object{$_-cne$n});Reject {Assert-ProofSourceParameters} ('artifact without '+$n);Set-Mode 'draft' ($draftNames+$n);Reject {Assert-ProofSourceParameters} ('draft with '+$n)}
      Set-Mode 'Draft' $draftNames;Reject {Assert-ProofSourceParameters} 'case-folded mode'
      Write-Output PASS
    `;
    const result = runPowerShell(check); expect(result.status, result.stderr || result.stdout).toBe(0); expect(result.stdout).toContain('PASS');
  });

  it('stops each real verifier before any download, install or launch when a rehearsal carries draft inputs', () => {
    const common = "-ProjectRoot x -TargetSourceRoot x -DraftProofRoot x -WorkRoot x -OutputRoot x -WebDriverRoot x -WorkflowSourceRevision x -TargetVersion 0.3.1 -TargetSourceRevision x -InstallerName x -InstallerBytes 1 -InstallerSha256 x -ExpectedPublisher x -ProofSource artifact -ArtifactRoot x -ArtifactRunId 1 -ArtifactId 1 -ArtifactName x -ArtifactBytes 1 -ArtifactDigest x";
    for (const script of ['verify-default-expanded-page-tools.ps1', 'verify-default-comments-forms.ps1', 'verify-default-recovery.ps1']) {
      const mixed = runPowerShell(`$ErrorActionPreference='Stop';Set-Location -LiteralPath '${cwd}';$env:RUNNER_TEMP=$null;./scripts/${script} ${common} -TargetTag v0.3.1`, 120_000);
      expect(mixed.status, script).not.toBe(0);
      expect((mixed.stderr + mixed.stdout).replace(/\s+/g, ' '), script).toContain('Parameter TargetTag is required for draft verification and forbidden for the pre-tag artifact rehearsal.');
      const draftWithArtifact = runPowerShell(`$ErrorActionPreference='Stop';Set-Location -LiteralPath '${cwd}';./scripts/${script} ${common.replace('-ProofSource artifact', '-ProofSource draft')}`, 120_000);
      expect(draftWithArtifact.status, script).not.toBe(0);
      expect((draftWithArtifact.stderr + draftWithArtifact.stdout).replace(/\s+/g, ' '), script).toMatch(/Parameter \w+ is required for draft verification and forbidden for the pre-tag artifact rehearsal\./);
    }
  }, 400_000);

  it('accepts only an artifact extraction receipt bound to the pinned artifact, installer and candidate', () => {
    const check = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest;Set-Location -LiteralPath '${cwd}'
      ${importer}
      Import-TestFunctions ./scripts/verify-default-expanded-page-tools.ps1 @('Assert-DefaultArtifactReceipt')
      Import-TestFunctions ./scripts/verify-signed-ocr-upgrade.ps1 @('Assert-ExactProperties','Assert-ReceiptValue')
      Import-TestFunctions ./scripts/verify-default-draft.ps1 @('ConvertTo-ExactUInt64')
      ${receiptFixture}
      $sets=Assert-DefaultArtifactReceipt $receipt;if(@($sets.Base).Count-ne28-or@($sets.Ocr).Count-ne5-or$sets.Pdfium.target-cne'resources/pdfium/bin/pdfium.dll'){throw 'sets changed'}
      $mutations=@(
        {param($r)$r.mode='default-draft-extraction'},{param($r)$r.schemaVersion=1},{param($r)$r.workflowSourceRevision='3'*40},{param($r)$r.targetVersion='0.3.0'},{param($r)$r.targetSourceRevision='4'*40},
        {param($r)$r.artifact.runId=99},{param($r)$r.artifact.id=99},{param($r)$r.artifact.bytes=99},{param($r)$r.artifact.digest=('sha256:'+('b'*64))},{param($r)$r.artifact.name='signed-ocr-installer-other'},
        {param($r)$r.assets[0].sha256='B'*64},{param($r)$r.assets[0].bytes=11},{param($r)$r.assets[0].name='other.exe'},{param($r)$r.assets[0].role='updaterManifest'},{param($r)$r.assets+=$r.assets[0]},
        {param($r)$r.assets[0]|Add-Member id 4},{param($r)$r|Add-Member releaseId 3},{param($r)$r.baseResources=@($r.baseResources|Select-Object -Skip 1)},{param($r)$r.baseResources[0].target='resources/ocr/model'},
        {param($r)$r.ocr.resources=@($r.ocr.resources|Select-Object -Skip 1)},{param($r)$r.signatures.installer='NotSigned'},{param($r)$r.signatures.expectedPublisher='Other'},
        {param($r)$r.verification.draft=$true},{param($r)$r.verification.releaseCandidateArtifact=$false},{param($r)$r.verification.updaterArtifactsAbsent=$false},{param($r)$r.verification.packagedApplicationEmbedsCandidateUpdaterKey=$false},{param($r)$r.verification.installedBehaviorVerified=$true},{param($r)$r.verification.ocrBundled=$false}
      )
      $i=0;foreach($m in $mutations){$copy=Copy-Receipt;&$m $copy;Reject {Assert-DefaultArtifactReceipt $copy} ('mutation '+$i);$i++}
      $ArtifactName='signed-ocr-installer-'+('5'*40);Reject {Assert-DefaultArtifactReceipt $receipt} 'artifact name not bound to the candidate'
      $ArtifactName='signed-ocr-installer-'+$TargetSourceRevision;$InstallerName='PDF Workstation_0.3.0_x64-setup.exe';Reject {Assert-DefaultArtifactReceipt $receipt} 'installer name for another version'
      Write-Output PASS
    `;
    const result = runPowerShell(check); expect(result.status, result.stderr || result.stdout).toBe(0); expect(result.stdout).toContain('PASS');
  });

  it('reads the rehearsal proof and the exact downloaded installer, and refuses extra files or a changed installer', () => {
    const check = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest;Set-Location -LiteralPath '${cwd}'
      . ./scripts/ocr/installer-package.ps1
      ${importer}
      Import-TestFunctions ./scripts/verify-default-expanded-page-tools.ps1 @('Assert-DefaultArtifactReceipt','Read-DefaultArtifactRehearsal')
      Import-TestFunctions ./scripts/verify-signed-ocr-upgrade.ps1 @('Assert-ExactProperties','Assert-ReceiptValue','Assert-FileReceipt')
      Import-TestFunctions ./scripts/verify-default-draft.ps1 @('ConvertTo-ExactUInt64')
      ${receiptFixture}
      $root=Join-Path (Get-Location) ('target/rehearsal-read-'+[guid]::NewGuid());$proof=Join-Path $root 'proof';$artifact=Join-Path $root 'artifact';[IO.Directory]::CreateDirectory($proof)|Out-Null;[IO.Directory]::CreateDirectory($artifact)|Out-Null
      try{
        $installer=Join-Path $artifact $InstallerName;[IO.File]::WriteAllBytes($installer,[byte[]](1..64));$InstallerBytes='64';$InstallerSha256=Get-ExactSha256 $installer;$receipt.assets[0].bytes=[uint64]64;$receipt.assets[0].sha256=$InstallerSha256
        [IO.File]::WriteAllText((Join-Path $artifact 'artifact-verification.json'),'{}')
        [IO.File]::WriteAllText((Join-Path $proof 'default-artifact-verification.json'),($receipt|ConvertTo-Json -Depth 20))
        $read=Read-DefaultArtifactRehearsal $proof $artifact
        if($read.Installer-cne$installer-or$read.Binding.source-cne'ocr-signed-artifact'-or$read.Binding.artifactRunId-ne31-or$read.Binding.draftVerified-or$read.Binding.releaseVerified-or$read.Binding.updaterVerified-or$read.Binding.installer.sha256-cne$InstallerSha256){throw 'binding changed'}
        if(($read.Binding|ConvertTo-Json -Depth 5)-match'(?i)[A-Z]:\\|\\Users\\'){throw 'binding leaks a host path'}
        [IO.File]::WriteAllText((Join-Path $artifact 'extra.txt'),'x');Reject {Read-DefaultArtifactRehearsal $proof $artifact} 'extra artifact file';Remove-Item -LiteralPath (Join-Path $artifact 'extra.txt')
        [IO.File]::WriteAllText((Join-Path $proof 'extra.json'),'{}');Reject {Read-DefaultArtifactRehearsal $proof $artifact} 'extra proof file';Remove-Item -LiteralPath (Join-Path $proof 'extra.json')
        [IO.File]::WriteAllBytes($installer,[byte[]](2..65));Reject {Read-DefaultArtifactRehearsal $proof $artifact} 'changed installer';[IO.File]::WriteAllBytes($installer,[byte[]](1..64))
        $leaky=Copy-Receipt;$leaky.scope='C:\Users\runneradmin';[IO.File]::WriteAllText((Join-Path $proof 'default-artifact-verification.json'),($leaky|ConvertTo-Json -Depth 20));Reject {Read-DefaultArtifactRehearsal $proof $artifact} 'unsanitized receipt'
      }finally{Remove-Item -LiteralPath $root -Recurse -Force}
      Write-Output PASS
    `;
    const result = runPowerShell(check); expect(result.status, result.stderr || result.stdout).toBe(0); expect(result.stdout).toContain('PASS');
  });

  it('binds the signed artifact export to its candidate, installer and publisher before extraction', () => {
    expect(extraction).toContain("$null=& (Join-Path $PSScriptRoot 'assert-updater-signing-key.ps1') -SourceRoot $TargetSourceRoot");
    expect(extraction).toContain('Test-ExecutableContainsAscii -Path $application.FullName -Marker $configuredUpdaterKey');
    expect(extraction).toContain("if([uint64]$base.Count -ne 28)");
    expect(extraction).toContain("mode='default-artifact-extraction'"); expect(extraction).toContain('draft=$false;releaseCandidateArtifact=$true');
    expect(extraction).not.toMatch(/GITHUB_TOKEN\b(?!\|)|Invoke-GitHubJson|HttpClient|Invoke-RestMethod|Invoke-WebRequest/);
    const check = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest;Set-Location -LiteralPath '${cwd}'
      . ./scripts/ocr/installer-package.ps1
      ${importer}
      Import-TestFunctions ./scripts/verify-default-artifact.ps1 @('Assert-ArtifactIdentityInputs','Assert-ArtifactExport')
      $TargetVersion='0.3.1';$WorkflowSourceRevision='1'*40;$TargetSourceRevision='2'*40;$ExpectedPublisher='Publisher';$ArtifactName='signed-ocr-installer-'+$TargetSourceRevision;$ArtifactDigest='sha256:'+('a'*64);$InstallerName='PDF Workstation_0.3.1_x64-setup.exe'
      Assert-ArtifactIdentityInputs
      $ArtifactName='signed-ocr-installer-'+('3'*40);Reject {Assert-ArtifactIdentityInputs} 'unbound artifact name';$ArtifactName='signed-ocr-installer-'+$TargetSourceRevision
      $ArtifactDigest='sha256:'+('A'*64);Reject {Assert-ArtifactIdentityInputs} 'uppercase digest';$ArtifactDigest='sha256:'+('a'*64)
      $InstallerName='PDF Workstation_0.3.0_x64-setup.exe';Reject {Assert-ArtifactIdentityInputs} 'other version installer';$InstallerName='PDF Workstation_0.3.1_x64-setup.exe'
      $root=Join-Path (Get-Location) ('target/rehearsal-export-'+[guid]::NewGuid());[IO.Directory]::CreateDirectory($root)|Out-Null
      try{
        $installer=Join-Path $root $InstallerName;[IO.File]::WriteAllBytes($installer,[byte[]](1..32));$sha=Get-ExactSha256 $installer
        $record=[ordered]@{schemaVersion=1;mode='azure-signed-artifact-only-ocr';sourceRevision=$TargetSourceRevision;installer=[ordered]@{fileName=$InstallerName;bytes=32;sha256=$sha};signatures=[ordered]@{expectedPublisher=$ExpectedPublisher};verification=[ordered]@{updaterArtifacts=@()}}
        function Write-Record($Value){[IO.File]::WriteAllText((Join-Path $root 'artifact-verification.json'),($Value|ConvertTo-Json -Depth 10))}
        Write-Record $record;if((Assert-ArtifactExport $root 32 $sha)-cne$installer){throw 'installer path changed'}
        Reject {Assert-ArtifactExport $root 33 $sha} 'byte mismatch';Reject {Assert-ArtifactExport $root 32 ('B'*64)} 'hash mismatch'
        foreach($change in @({param($r)$r.mode='azure-signed-release-ocr'},{param($r)$r.sourceRevision='4'*40},{param($r)$r.installer.fileName='other.exe'},{param($r)$r.signatures.expectedPublisher='Other'},{param($r)$r.verification.updaterArtifacts=@('latest.json')},{param($r)$r.schemaVersion=2})){$copy=$record|ConvertTo-Json -Depth 10|ConvertFrom-Json -AsHashtable;&$change $copy;Write-Record $copy;Reject {Assert-ArtifactExport $root 32 $sha} 'record mutation'}
        Write-Record $record;[IO.File]::WriteAllText((Join-Path $root 'latest.json'),'{}');Reject {Assert-ArtifactExport $root 32 $sha} 'extra updater file';Remove-Item -LiteralPath (Join-Path $root 'latest.json')
        [IO.Directory]::CreateDirectory((Join-Path $root 'nested'))|Out-Null;Reject {Assert-ArtifactExport $root 32 $sha} 'nested folder'
      }finally{Remove-Item -LiteralPath $root -Recurse -Force}
      Write-Output PASS
    `;
    const result = runPowerShell(check); expect(result.status, result.stderr || result.stdout).toBe(0); expect(result.stdout).toContain('PASS');
  });

  it('checks the signed artifact and its run against the pinned identity before download', () => {
    const run = (artifact: string, runJson: string, extra = '') => runPowerShell(String.raw`
      $ErrorActionPreference='Stop';Set-Location -LiteralPath '${cwd}';$env:GITHUB_TOKEN=$null
      $sha='2'*40;$fakeArtifact=${artifact};$fakeRun=${runJson};${extra}
      ./scripts/assert-signed-artifact-metadata.ps1 -TargetSourceRevision $sha -ArtifactRunId 31 -ArtifactId 32 -ArtifactName ('signed-ocr-installer-'+$sha) -ArtifactBytes 33 -ArtifactDigest ('sha256:'+('a'*64)) -JsonProvider {param($Uri)if($Uri-like'*/actions/artifacts/32'){$fakeArtifact}elseif($Uri-like'*/actions/runs/31'){$fakeRun}else{throw ('unexpected '+$Uri)}}
    `);
    const goodArtifact = "[pscustomobject]@{id=32;name=('signed-ocr-installer-'+('2'*40));size_in_bytes=33;digest=('sha256:'+('a'*64));expired=$false;workflow_run=[pscustomobject]@{id=31;head_sha=('2'*40);head_branch='master'}}";
    const goodRun = "[pscustomobject]@{id=31;name='Manual signed OCR artifact validation';event='workflow_dispatch';status='completed';conclusion='success';head_branch='master';head_sha=('2'*40);path='.github/workflows/ocr-signed-artifact.yml';repository=[pscustomobject]@{full_name='Joshua-Beel/PDF-Workstation'}}";
    const ok = run(goodArtifact, goodRun); expect(ok.status, ok.stderr || ok.stdout).toBe(0); expect(ok.stdout).toContain('SIGNED_ARTIFACT_METADATA_VERIFIED');
    for (const extra of ['$fakeArtifact.expired=$true', "$fakeArtifact.digest='sha256:'+('b'*64)", '$fakeArtifact.size_in_bytes=34', "$fakeArtifact.workflow_run.head_sha='3'*40", "$fakeArtifact.workflow_run.head_branch='sm6/x'", '$fakeArtifact.workflow_run.id=30', "$fakeRun.conclusion='failure'", "$fakeRun.path='.github/workflows/release.yml'", "$fakeRun.name='Release'", "$fakeRun.event='push'", "$fakeRun.head_sha='3'*40", "$fakeRun.repository.full_name='someone/fork'"]) {
      const bad = run(goodArtifact, goodRun, extra); expect(bad.status, extra).not.toBe(0); expect(bad.stdout, extra).not.toContain('SIGNED_ARTIFACT_METADATA_VERIFIED');
    }
    const noToken = runPowerShell(`$ErrorActionPreference='Stop';Set-Location -LiteralPath '${cwd}';$env:GITHUB_TOKEN=$null;./scripts/assert-signed-artifact-metadata.ps1 -TargetSourceRevision ('2'*40) -ArtifactRunId 31 -ArtifactId 32 -ArtifactName x -ArtifactBytes 33 -ArtifactDigest x`);
    expect(noToken.status).not.toBe(0); expect(noToken.stderr + noToken.stdout).toContain('GitHub token is unavailable');
  }, 240_000);
});
