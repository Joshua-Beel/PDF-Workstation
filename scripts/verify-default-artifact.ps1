[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$ProjectRoot,
    [Parameter(Mandatory=$true)][string]$TargetSourceRoot,
    [Parameter(Mandatory=$true)][string]$ArtifactRoot,
    [Parameter(Mandatory=$true)][string]$WorkRoot,
    [Parameter(Mandatory=$true)][string]$OutputRoot,
    [Parameter(Mandatory=$true)][string]$WorkflowSourceRevision,
    [Parameter(Mandatory=$true)][string]$TargetVersion,
    [Parameter(Mandatory=$true)][string]$TargetSourceRevision,
    [Parameter(Mandatory=$true)][string]$ArtifactRunId,
    [Parameter(Mandatory=$true)][string]$ArtifactId,
    [Parameter(Mandatory=$true)][string]$ArtifactName,
    [Parameter(Mandatory=$true)][string]$ArtifactBytes,
    [Parameter(Mandatory=$true)][string]$ArtifactDigest,
    [Parameter(Mandatory=$true)][string]$InstallerName,
    [Parameter(Mandatory=$true)][string]$InstallerBytes,
    [Parameter(Mandatory=$true)][string]$InstallerSha256,
    [Parameter(Mandatory=$true)][string]$ExpectedPublisher
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')

$script:MaximumAssetBytes=[uint64](256MB)

function Import-ArtifactFunctions { param([string]$Path,[string[]]$Names)
 $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($Path,[ref]$tokens,[ref]$errors);if($errors.Count){throw 'A trusted draft-verifier helper did not parse.'}
 foreach($name in $Names){$node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true);if(-not$node){throw "Missing trusted draft-verifier helper: $name"};Set-Item -Path ("Function:script:"+$name) -Value $node.Body.GetScriptBlock()}
}
Import-ArtifactFunctions (Join-Path $PSScriptRoot 'verify-default-draft.ps1') @('ConvertTo-ExactUInt64','Resolve-ProofPath','Assert-ReceiptShape','Invoke-BoundedSevenZip','Assert-BoundedArchiveListing','Invoke-BoundedSevenZipEntry','Get-SevenZipVersionFact','Assert-ReleaseSourceVersion','Assert-PackagedApplicationIdentity','Get-DefaultOcrPlanShape','Assert-DefaultOcrPackage','Expand-DefaultInstaller','Assert-DefaultInventory','Get-UniqueExtractedFile')

function Assert-ArtifactIdentityInputs {
    if($TargetVersion -cnotmatch '^\d+\.\d+\.\d+$'-or $WorkflowSourceRevision -cnotmatch '^[a-f0-9]{40}$'-or $TargetSourceRevision -cnotmatch '^[a-f0-9]{40}$'-or [string]::IsNullOrWhiteSpace($ExpectedPublisher)){throw 'Rehearsal identity inputs are malformed.'}
    if($ArtifactName -cne "signed-ocr-installer-$TargetSourceRevision"){throw 'Signed artifact name is not bound to the rehearsal source revision.'}
    if($ArtifactDigest -cnotmatch '^sha256:[a-f0-9]{64}$'){throw 'Signed artifact digest is malformed.'}
    if($InstallerName -cne "PDF Workstation_${TargetVersion}_x64-setup.exe"){throw 'Installer name is not the exact artifact installer name for this version.'}
}

function Assert-ArtifactExport { param([string]$Root,[uint64]$Bytes,[string]$Sha256)
    $files=@(Get-ChildItem -LiteralPath $Root -Recurse -Force)
    if(@($files|Where-Object{$_.PSIsContainer}).Count -ne 0-or $files.Count -ne 2){throw 'Signed artifact must contain exactly the installer and its verification record.'}
    $installer=Join-Path $Root $InstallerName;$recordPath=Join-Path $Root 'artifact-verification.json'
    foreach($path in @($installer,$recordPath)){if(-not (Test-Path -LiteralPath $path -PathType Leaf)){throw 'Signed artifact file set is not exact.'};Assert-NoReparseAncestors -Path $path}
    if([uint64](Get-Item -LiteralPath $installer).Length -ne $Bytes-or (Get-ExactSha256 $installer)-cne $Sha256){throw 'Signed artifact installer differs from its pinned receipt.'}
    if((Get-Item -LiteralPath $recordPath).Length -gt 1MB){throw 'Signed artifact record is oversized.'}
    try{$record=Get-Content -LiteralPath $recordPath -Raw -Encoding UTF8|ConvertFrom-Json}catch{throw 'Signed artifact record is malformed.'}
    if([int]$record.schemaVersion -ne 1-or [string]$record.mode -cne 'azure-signed-artifact-only-ocr'-or [string]$record.sourceRevision -cne $TargetSourceRevision-or [string]$record.installer.fileName -cne $InstallerName-or [uint64]$record.installer.bytes -ne $Bytes-or [string]$record.installer.sha256 -cne $Sha256-or [string]$record.signatures.expectedPublisher -cne $ExpectedPublisher-or @($record.verification.updaterArtifacts).Count -ne 0){throw 'Signed artifact record is not bound to this source, installer and publisher.'}
    return $installer
}

Assert-ArtifactIdentityInputs
$runId=ConvertTo-ExactUInt64 $ArtifactRunId 'Artifact run id';$artifactIdValue=ConvertTo-ExactUInt64 $ArtifactId 'Artifact id';$artifactLength=ConvertTo-ExactUInt64 $ArtifactBytes 'Artifact bytes';$installerLength=ConvertTo-ExactUInt64 $InstallerBytes 'Installer bytes'
Assert-ReceiptShape $installerLength $InstallerSha256 'Installer'
$runnerTemp=$env:RUNNER_TEMP;if([string]::IsNullOrWhiteSpace($runnerTemp)){throw 'RUNNER_TEMP is unavailable.'}
$artifactPath=Resolve-ProofPath $ArtifactRoot $runnerTemp;if(-not (Test-Path -LiteralPath $artifactPath -PathType Container)){throw 'Signed artifact root is missing.'}
$workPath=Resolve-ProofPath $WorkRoot $runnerTemp -Fresh;$outputPath=Resolve-ProofPath $OutputRoot $runnerTemp -Fresh
[IO.Directory]::CreateDirectory($workPath)|Out-Null;[IO.Directory]::CreateDirectory($outputPath)|Out-Null
Assert-ReleaseSourceVersion -WorkflowRoot $ProjectRoot -SourceRoot $TargetSourceRoot -Version $TargetVersion
$installerPath=Assert-ArtifactExport $artifactPath $installerLength $InstallerSha256
$null=Assert-TrustedWindowsSignature -Path $installerPath -ExpectedPublisher $ExpectedPublisher
$configuredUpdaterKey=[string](Get-Content -LiteralPath (Join-Path $TargetSourceRoot 'src-tauri/tauri.conf.json') -Raw -Encoding UTF8|ConvertFrom-Json).plugins.updater.pubkey;if($configuredUpdaterKey -cnotmatch '^[A-Za-z0-9+/=]{100,400}$'){throw 'Candidate updater public key is malformed.'}
$null=& (Join-Path $PSScriptRoot 'assert-updater-signing-key.ps1') -SourceRoot $TargetSourceRoot
$base=Get-BaseBundleResourceMap -ProjectRoot $TargetSourceRoot
if([uint64]$base.Count -ne 28){throw 'Default candidate-source resource inventory is not exactly 28 files.'}
$extractionRoot=Join-Path $workPath 'extracted-installer';$files=Expand-DefaultInstaller $installerPath $extractionRoot $base.Entries
$inventory=Assert-DefaultInventory $files $extractionRoot $base.Entries
$application=Get-UniqueExtractedFile $files 'pdf-workstation.exe' 'Packaged application';Assert-PackagedApplicationIdentity -Path $application.FullName -Version $TargetVersion;$null=Assert-TrustedWindowsSignature -Path $application.FullName -ExpectedPublisher $ExpectedPublisher
if(-not (Test-ExecutableContainsAscii -Path $application.FullName -Marker $configuredUpdaterKey)){throw 'Packaged application does not embed the candidate updater public key.'}
$ocrReceipts=@(Assert-DefaultOcrPackage -Files $files -Identity $inventory.ocrIdentity -SourceRoot $TargetSourceRoot -ApplicationPath $application.FullName -ExpectedPublisher $ExpectedPublisher)
$resourceReceipts=@()
foreach($entry in $base.Entries){
    $match=Get-UniqueExtractedFile $files ([string]$entry.Target) 'Base resource'
    if([string]$entry.Target -ceq 'resources/pdfium/bin/pdfium.dll'){
        Assert-SignedPdfiumEquivalent -UnsignedPath $entry.Source -SignedPath $match.FullName -ExpectedPublisher $ExpectedPublisher
        $resourceReceipts+=[ordered]@{target=$entry.Target;bytes=[uint64]$match.Length;sha256=Get-ExactSha256 $match.FullName;unsignedBytes=[uint64]$entry.Receipt.Bytes;unsignedSha256=[string]$entry.Receipt.Sha256}
    }else{
        if([uint64]$match.Length -ne [uint64]$entry.Receipt.Bytes-or (Get-ExactSha256 $match.FullName)-cne [string]$entry.Receipt.Sha256){throw 'Extracted base resource differs from candidate source.'}
        $resourceReceipts+=[ordered]@{target=$entry.Target;bytes=[uint64]$match.Length;sha256=[string]$entry.Receipt.Sha256}
    }
}
$record=[ordered]@{
 schemaVersion=2;scope='Pre-tag rehearsal: read-only extraction proof of a signed artifact-only OCR installer built from the release candidate, including the packaged OCR engine, model and licenses; no release, updater signature, installation, launch, OCR recognition or publication claim.';mode='default-artifact-extraction';workflowSourceRevision=$WorkflowSourceRevision;targetVersion=$TargetVersion;targetSourceRevision=$TargetSourceRevision
 artifact=[ordered]@{runId=$runId;id=$artifactIdValue;name=$ArtifactName;bytes=$artifactLength;digest=$ArtifactDigest}
 assets=@([ordered]@{role='installer';name=$InstallerName;bytes=$installerLength;sha256=$InstallerSha256})
 packagedApplication=[ordered]@{bytes=[uint64]$application.Length;sha256=Get-ExactSha256 $application.FullName}
 baseResources=@($resourceReceipts|Sort-Object target);ocr=[ordered]@{identity=$inventory.ocrIdentity;resources=$ocrReceipts};signatures=[ordered]@{expectedPublisher=$ExpectedPublisher;installer='Valid';application='Valid';pdfium='Valid';ocrEngine='Valid';trustedTimestampsRequired=$true}
 verification=[ordered]@{draft=$false;releaseCandidateArtifact=$true;exactAssetInventory=$true;exactExtractedInventory=$true;targetSourceSixVersionsMatched=$true;packagedApplicationIdentityAndVersionMatched=$true;sourceBuildExecutableByteProvenanceReconstructed=$false;requiredExtractedFiles=$inventory.required;actualExtractedFiles=$inventory.actual;optionalUninstaller=$inventory.optionalUninstaller;optionalUninstallerExcluded=[bool]$script:OptionalUninstallerExcluded;ocrBundled=$true;packagedApplicationEmbedsCandidateUpdaterKey=$true;updaterArtifactsAbsent=$true;installedBehaviorVerified=$false}
}
$json=$record|ConvertTo-Json -Depth 20
if($json -match '(?i)([A-Z]:\\|\\Users\\|runneradmin|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log)'){throw 'Rehearsal receipt contains host or credential data.'}
[IO.File]::WriteAllText((Join-Path $outputPath 'default-artifact-verification.json'),$json,[Text.UTF8Encoding]::new($false))
