import { describe, expect, it } from 'vitest';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';

const workflow = readFileSync('.github/workflows/default-expanded-page-tools-verification.yml', 'utf8');
const verifier = readFileSync('scripts/verify-default-expanded-page-tools.ps1', 'utf8');

function runPowerShell(source: string) {
  const root = `target/default-expanded-test-${randomUUID()}`; mkdirSync(root, { recursive: true });
  const path = `${root}/run.ps1`; writeFileSync(path, source, 'utf8');
  try { return spawnSync('pwsh', ['-NoProfile', '-NonInteractive', '-File', path], { encoding: 'utf8', timeout: 30_000 }); }
  finally { rmSync(root, { recursive: true, force: true }); }
}

describe('default installed expanded page-tools workflow', () => {
  it('is manual, pinned, exact-input driven, and has no signing, release, build, or OCR capability', () => {
    expect(workflow).toContain('workflow_dispatch:'); expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('runs-on: windows-2022'); expect(workflow).toContain('contents: read'); expect(workflow).toContain('actions: read'); expect(workflow.match(/contents:\s*write/g)).toHaveLength(1); expect(workflow.indexOf('contents: write')).toBeGreaterThan(workflow.indexOf('verify-installed-page-tools:')); expect(workflow).not.toMatch(/actions:\s*write|id-token:\s*write/);
    for (const name of ['draft_proof_run_id','draft_proof_workflow_id','draft_proof_artifact_id','draft_proof_artifact_name','draft_proof_artifact_bytes','draft_proof_artifact_digest','draft_proof_workflow_revision','draft_proof_receipt_bytes','draft_proof_receipt_sha256','target_release_id','installer_asset_id','installer_bytes','installer_sha256']) expect(workflow).toContain(`${name}:`);
    const uses=[...workflow.matchAll(/^\s*-?\s*uses:\s*([^\s#]+)\s*$/gm)].map(x=>x[1]);
    expect(uses).toEqual(['actions/checkout@11d5960a326750d5838078e36cf38b85af677262','actions/checkout@11d5960a326750d5838078e36cf38b85af677262','actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093','actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02']);
    expect(workflow.match(/persist-credentials: false/g)).toHaveLength(2);
    const pdfiumPrep = "./scripts/prepare-default-draft-pdfium.ps1 -TargetSourceRoot (Resolve-Path target-source).Path -WorkRoot (Join-Path $env:RUNNER_TEMP 'default-installed-pdfium-prep')";
    expect(workflow).toContain(pdfiumPrep);
    expect(workflow.indexOf(pdfiumPrep)).toBeLessThan(workflow.indexOf('./scripts/verify-default-expanded-page-tools.ps1'));
    expect(workflow.match(/git -C target-source diff --cached --quiet --/g)).toHaveLength(2);
    expect(workflow).not.toMatch(/secrets\.|AZURE_|TAURI_SIGNING|artifact-signing|gh release|cargo |npm run build|setup-dotnet/);
  });

  it('reuses unchanged page-tool helpers, reruns Step 1 independently, and limits its claims', () => {
    for (const path of ['installed-image-page-tools.ps1','installed-organizer-tools.ps1','installed-structural-page-tools.ps1','verify-default-draft.ps1']) expect(verifier).toContain(path);
    expect(verifier).toContain('Assert-IndependentReceiptParity $receipt $independent');
    expect(verifier).not.toMatch(/Import-ExactFunctions[^\r\n]+Assert-ReceiptShape/);
    expect(verifier).toContain("featureBlobCount=$script:FeaturePaths.Count");
    expect(verifier).toContain("sourceBuildExecutableByteProvenanceReconstructed");
    expect(verifier).toContain('Invoke-SilentUninstall');
    expect(verifier).toContain('ocrBundled=$true;ocrRecognitionVerified=$false');
    expect(verifier).toContain('the packaged OCR files are receipt-checked but no OCR recognition, updater cryptography, comments, forms, print, or general behavior claim');
    expect(verifier).toContain("$engine=Get-UniqueResource $files ([string]$Sets.OcrEngine.target) 'Installed OCR engine';$null=Assert-TrustedWindowsSignature $engine.FullName -ExpectedPublisher $ExpectedPublisher");
    expect(verifier).toContain("foreach($entry in $Sets.Ocr){$null=$required.Add([string]$entry.target)");
    expect(verifier).not.toContain("TargetVersion-cne'0.2.11'");
    expect(verifier).not.toMatch(/Write-(?:Host|Verbose|Debug)|Invoke-AzureArtifactSigningCli|artifact-signing-cli/);
    expect(verifier).toContain('$env:GITHUB_TOKEN=$null;$env:GH_TOKEN=$null;$token=$null');
    expect(verifier.indexOf('$env:GITHUB_TOKEN=$null')).toBeLessThan(verifier.indexOf('Invoke-BoundedSilentInstaller $installer'));
    expect(verifier).not.toMatch(/HttpMethod\]::(?:Post|Put|Patch|Delete)|Invoke-RestMethod|Invoke-WebRequest|gh\s+(?:release|api)/i);
  });

  it('retains exact imported helper functions after the importer returns', () => {
    const check=String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest;Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path ./scripts/verify-default-expanded-page-tools.ps1),[ref]$tokens,[ref]$errors);if($errors.Count){throw ($errors|Out-String)}
      $node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq'Import-ExactFunctions'},$true);Invoke-Expression $node.Extent.Text
      Import-ExactFunctions (Resolve-Path ./scripts/verify-default-draft.ps1) @('ConvertTo-ExactUInt64','New-GitHubGetRequest','Assert-GitHubResponseSuccess','Invoke-GitHubJson')
      foreach($name in @('ConvertTo-ExactUInt64','New-GitHubGetRequest','Assert-GitHubResponseSuccess','Invoke-GitHubJson')){if(-not(Get-Command $name -CommandType Function -ErrorAction SilentlyContinue)){throw ('Imported helper did not persist: '+$name)}}
      if((ConvertTo-ExactUInt64 '7' 'test')-ne7){throw 'Imported helper did not execute.'}
      Write-Output PASS
    `;
    const result=runPowerShell(check);expect(result.error).toBeUndefined();expect(result.status,result.stderr||result.stdout).toBe(0);expect(result.stdout).toContain('PASS');
  });

  it('executes real receipt and independent-parity guards against stale, OCR, unsigned-scope, and changed redownload proofs', () => {
    const check=String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest;Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/ocr/installer-package.ps1
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path ./scripts/verify-default-expanded-page-tools.ps1),[ref]$tokens,[ref]$errors);if($errors.Count){throw ($errors|Out-String)}
      foreach($name in @('Assert-DefaultDraftReceipt','Assert-IndependentReceiptParity','Assert-Step1ArtifactMetadata','Assert-EmptyProcessFacts','Assert-OwnedCleanupState','Invoke-OwnedCleanup','Complete-PrimaryAndCleanup')){$node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true);if(-not$node){throw ('missing '+$name)};Invoke-Expression $node.Extent.Text}
      function Assert-ExactProperties {param($Value,[string[]]$Expected,[string]$Kind)$a=@($Value.PSObject.Properties.Name|Sort-Object);$e=@($Expected|Sort-Object);if($a.Count-ne$e.Count-or(Compare-Object $e $a)){throw 'shape'}}
      function Assert-ReceiptValue {param($Value,[string]$Kind)if([uint64]$Value.bytes-eq0-or[string]$Value.sha256-cnotmatch'^[A-F0-9]{64}$'){throw 'receipt'}}
      function ConvertTo-ExactUInt64 {param([string]$Value,[string]$Kind)[uint64]$parsed=0;if(-not[uint64]::TryParse($Value,[ref]$parsed)-or$parsed-eq0){throw 'integer'};return $parsed}
      $DraftProofWorkflowRevision='1'*40;$TargetVersion='0.2.11';$TargetTag='v0.2.11';$TargetSourceRevision='2'*40;$TargetReleaseId='3';$InstallerAssetId='4';$InstallerName='PDF.Workstation_0.2.11_x64-setup.exe';$InstallerBytes='10';$InstallerSha256='A'*64;$SignatureAssetId='5';$SignatureBytes='11';$SignatureSha256='B'*64;$ManifestAssetId='6';$ManifestBytes='12';$ManifestSha256='C'*64;$ExpectedPublisher='Publisher'
      $base=@();for($i=0;$i-lt27;$i++){$base+=[pscustomobject]@{target=('resources/licenses/'+$i+'.txt');bytes=[uint64]1;sha256=('D'*64)}};$base+=[pscustomobject]@{target='resources/pdfium/bin/pdfium.dll';bytes=[uint64]2;sha256=('E'*64);unsignedBytes=[uint64]1;unsignedSha256=('F'*64)}
      $ocrId='e'*64;$ocrResources=@([pscustomobject][ordered]@{target="resources/ocr/$ocrId/bin/tesseract.exe";role='engine';bytes=[uint64]3;sha256=('E'*64)})+@(@(@('tessdata/eng.traineddata','model'),@('licenses/Tesseract-Apache-2.0.txt','license'),@('licenses/Leptonica-BSD-2-Clause.txt','license'),@('licenses/eng-fast-Apache-2.0.txt','license'))|ForEach-Object{[pscustomobject][ordered]@{target="resources/ocr/$ocrId/$($_[0])";role=$_[1];bytes=[uint64]4;sha256=('7'*64)}})
      $receipt=[pscustomobject][ordered]@{schemaVersion=2;scope='scope';mode='default-draft-extraction';workflowSourceRevision=$DraftProofWorkflowRevision;targetVersion=$TargetVersion;targetTag=$TargetTag;targetSourceRevision=$TargetSourceRevision;releaseId=[uint64]3;assets=@([pscustomobject]@{role='installer';id=[uint64]4;name=$InstallerName;bytes=[uint64]10;sha256=$InstallerSha256},[pscustomobject]@{role='detachedUpdaterSignature';id=[uint64]5;name=($InstallerName+'.sig');bytes=[uint64]11;sha256=$SignatureSha256},[pscustomobject]@{role='updaterManifest';id=[uint64]6;name='latest.json';bytes=[uint64]12;sha256=$ManifestSha256});packagedApplication=[pscustomobject]@{bytes=[uint64]20;sha256=('9'*64)};baseResources=$base;ocr=[pscustomobject][ordered]@{identity=$ocrId;resources=$ocrResources};signatures=[pscustomobject]@{expectedPublisher=$ExpectedPublisher;installer='Valid';application='Valid';pdfium='Valid';ocrEngine='Valid';trustedTimestampsRequired=$true};verification=[pscustomobject]@{exactAssetInventory=$true;exactExtractedInventory=$true;targetSourceSixVersionsMatched=$true;packagedApplicationIdentityAndVersionMatched=$true;sourceBuildExecutableByteProvenanceReconstructed=$false;optionalUninstaller=$false;optionalUninstallerExcluded=$true;ocrBundled=$true;manifestSignatureTextBound=$true;detachedUpdaterCryptographyVerified=$false;installedBehaviorVerified=$false}}
      function Reject([scriptblock]$Action,[string]$Kind){$bad=$false;try{&$Action}catch{$bad=$true};if(-not$bad){throw ($Kind+' accepted')}}
      $null=Assert-DefaultDraftReceipt $receipt;Assert-IndependentReceiptParity $receipt $receipt
      $stale=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$stale.targetVersion='0.2.10';Reject {Assert-DefaultDraftReceipt $stale} 'stale version'
      $sets=Assert-DefaultDraftReceipt $receipt;if(@($sets.Ocr).Count-ne5-or$sets.OcrEngine.target-cne"resources/ocr/$ocrId/bin/tesseract.exe"){throw 'OCR receipt sets changed'}
      $ocr=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$ocr.baseResources[0].target='resources/ocr/model';Reject {Assert-DefaultDraftReceipt $ocr} 'OCR resource in base'
      $old=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$old.schemaVersion=1;Reject {Assert-DefaultDraftReceipt $old} 'schema 1 receipt'
      $noOcr=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$noOcr.verification.ocrBundled=$false;Reject {Assert-DefaultDraftReceipt $noOcr} 'receipt without OCR'
      $four=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$four.ocr.resources=@($four.ocr.resources|Select-Object -First 4);Reject {Assert-DefaultDraftReceipt $four} 'four OCR resources'
      $engineId=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$engineId.ocr.resources[0].sha256='F'*64;Reject {Assert-DefaultDraftReceipt $engineId} 'engine outside its identity'
      $outside=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$outside.ocr.resources[1].target=('resources/ocr/'+('f'*64)+'/tessdata/eng.traineddata');Reject {Assert-DefaultDraftReceipt $outside} 'OCR resource outside identity folder'
      $role=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$role.ocr.resources[1].role='license';Reject {Assert-DefaultDraftReceipt $role} 'wrong OCR role'
      $engineSig=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$engineSig.signatures.ocrEngine='NotSigned';Reject {Assert-DefaultDraftReceipt $engineSig} 'unsigned OCR engine claim'
      $scope=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$scope.verification.installedBehaviorVerified=$true;Reject {Assert-DefaultDraftReceipt $scope} 'false Step 1 scope'
      $included=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$included.verification.optionalUninstallerExcluded=$false;Reject {Assert-DefaultDraftReceipt $included} 'unexcluded optional uninstaller'
      $changed=$receipt|ConvertTo-Json -Depth 20|ConvertFrom-Json;$changed.packagedApplication.sha256='8'*64;Reject {Assert-IndependentReceiptParity $receipt $changed} 'changed independent proof'
      Assert-EmptyProcessFacts ([pscustomobject]@{applicationProcessPresent=$false;installerProcessPresent=$false}) 'test';Reject {Assert-EmptyProcessFacts ([pscustomobject]@{applicationProcessPresent=$true;installerProcessPresent=$false}) 'test'} 'residual process'
      $owned=Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'owned-settings';[IO.Directory]::CreateDirectory($owned)|Out-Null;[IO.File]::WriteAllText((Join-Path $owned 'sentinel'),'x');$remove={param($path)if($path-cne$owned){throw 'wrong cleanup root'};Remove-Item -LiteralPath $path -Recurse -Force}.GetNewClosure();Invoke-OwnedCleanup $false (Join-Path $owned 'absent-install') $owned -RemoveProvider $remove;if(Test-Path $owned){throw 'owned cleanup did not run'}
      $missingUninstaller=Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'missing-uninstaller';[IO.Directory]::CreateDirectory($missingUninstaller)|Out-Null;Reject {Invoke-OwnedCleanup $true $missingUninstaller (Join-Path $missingUninstaller 'settings')} 'missing uninstaller';Remove-Item -LiteralPath $missingUninstaller -Recurse -Force
      $residual=Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'residual-install';[IO.Directory]::CreateDirectory($residual)|Out-Null;Reject {Assert-OwnedCleanupState (Join-Path $residual 'absent-registry') $residual (Join-Path $residual 'absent-settings') ([pscustomobject]@{applicationProcessPresent=$false;installerProcessPresent=$false})} 'residual install';$primaryResidual=$null;$cleanupResidual=$null;try{throw 'primary residual sentinel'}catch{$primaryResidual=$_};try{Assert-OwnedCleanupState (Join-Path $residual 'absent-registry') $residual (Join-Path $residual 'absent-settings') ([pscustomobject]@{applicationProcessPresent=$true;installerProcessPresent=$false})}catch{$cleanupResidual=$_};try{Complete-PrimaryAndCleanup $primaryResidual $cleanupResidual;throw 'primary residual accepted'}catch{if($_.Exception.Message-cnotmatch'primary residual sentinel'-or-not[bool]$_.Exception.Data['ownedCleanupFailed']){throw 'primary residual failure was not preserved'}};Remove-Item -LiteralPath $residual -Recurse -Force
      $primary=$null;$cleanup=$null;try{throw 'primary sentinel'}catch{$primary=$_};try{throw 'cleanup sentinel'}catch{$cleanup=$_};try{Complete-PrimaryAndCleanup $primary $cleanup;throw 'combined failure accepted'}catch{if($_.Exception.Message-cnotmatch'primary sentinel'-or-not[bool]$_.Exception.Data['ownedCleanupFailed']){throw 'primary failure was not preserved'}}
      $DraftProofRunId='21';$DraftProofWorkflowId='22';$DraftProofArtifactId='23';$DraftProofArtifactBytes='24';$DraftProofArtifactName=('default-draft-verification-'+$TargetSourceRevision);$DraftProofArtifactDigest=('sha256:'+('a'*64));$DraftProofWorkflowRevision='1'*40
      $script:badRun=$false;function Invoke-GitHubJson {param([string]$Uri,[string]$Token)if($Uri-match'/artifacts/') {return [pscustomobject]@{id=[uint64]23;name=$DraftProofArtifactName;size_in_bytes=[uint64]24;digest=$DraftProofArtifactDigest;workflow_run=[pscustomobject]@{id=[uint64]21;head_sha=$DraftProofWorkflowRevision};expired=$false}};return [pscustomobject]@{id=[uint64]21;workflow_id=[uint64]22;name='Manual default draft verification';event='workflow_dispatch';status='completed';conclusion=$(if($script:badRun){'failure'}else{'success'});head_branch='master';head_sha=$DraftProofWorkflowRevision;path='.github/workflows/default-draft-verification.yml'}}
      Assert-Step1ArtifactMetadata 'token';$script:badRun=$true;Reject {Assert-Step1ArtifactMetadata 'token'} 'failed Step 1 run'
      Write-Output PASS
    `;
    const result=runPowerShell(check);expect(result.error).toBeUndefined();expect(result.status,result.stderr||result.stdout).toBe(0);expect(result.stdout).toContain('PASS');
  },20_000);
});
