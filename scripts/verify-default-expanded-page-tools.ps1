[CmdletBinding()]
param(
 [Parameter(Mandatory=$true)][string]$ProjectRoot,[Parameter(Mandatory=$true)][string]$TargetSourceRoot,[Parameter(Mandatory=$true)][string]$DraftProofRoot,[string]$AssetRoot,[Parameter(Mandatory=$true)][string]$WorkRoot,[Parameter(Mandatory=$true)][string]$OutputRoot,[Parameter(Mandatory=$true)][string]$WebDriverRoot,
 [Parameter(Mandatory=$true)][string]$WorkflowSourceRevision,[string]$DraftProofWorkflowRevision,[string]$DraftProofRunId,[string]$DraftProofWorkflowId,[string]$DraftProofArtifactId,[string]$DraftProofArtifactName,[string]$DraftProofArtifactBytes,[string]$DraftProofArtifactDigest,[string]$DraftProofReceiptBytes,[string]$DraftProofReceiptSha256,
 [Parameter(Mandatory=$true)][string]$TargetVersion,[string]$TargetTag,[Parameter(Mandatory=$true)][string]$TargetSourceRevision,[string]$TargetReleaseId,
 [string]$InstallerAssetId,[Parameter(Mandatory=$true)][string]$InstallerName,[Parameter(Mandatory=$true)][string]$InstallerBytes,[Parameter(Mandatory=$true)][string]$InstallerSha256,
 [string]$SignatureAssetId,[string]$SignatureBytes,[string]$SignatureSha256,[string]$ManifestAssetId,[string]$ManifestBytes,[string]$ManifestSha256,[Parameter(Mandatory=$true)][string]$ExpectedPublisher,
 [ValidateSet('draft','artifact')][string]$ProofSource='draft',[string]$ArtifactRoot,[string]$ArtifactRunId,[string]$ArtifactId,[string]$ArtifactName,[string]$ArtifactBytes,[string]$ArtifactDigest
)
$ErrorActionPreference='Stop';Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'installed-image-page-tools.ps1')
. (Join-Path $PSScriptRoot 'installed-organizer-tools.ps1')
. (Join-Path $PSScriptRoot 'installed-structural-page-tools.ps1')
. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')

function Import-ExactFunctions {
 param([string]$Path,[string[]]$Names)
 $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($Path,[ref]$tokens,[ref]$errors);if($errors.Count){throw 'A trusted helper script did not parse.'}
 foreach($name in $Names){$node=$ast.Find({param($value)$value-is[Management.Automation.Language.FunctionDefinitionAst]-and$value.Name-ceq$name},$true);if(-not$node){throw "Trusted helper is missing: $name"};Set-Item -Path ("Function:script:"+$name) -Value $node.Body.GetScriptBlock()}
}
Import-ExactFunctions (Join-Path $PSScriptRoot 'verify-signed-ocr-upgrade.ps1') @('Assert-ExactProperties','Assert-FileReceipt','Resolve-RunnerPath','Assert-FreshInstallFacts','Get-FreshInstallFacts','Get-ConflictingProcessFacts','Invoke-BoundedSilentInstaller','Get-InstallFacts','Assert-InstallFacts','Assert-ReceiptValue','Assert-ReceiptFileObject','Get-UniqueResource','Convert-RegistryFilePath')
Import-ExactFunctions (Join-Path $PSScriptRoot 'verify-signed-expanded-page-tools.ps1') @('Assert-ExpandedExactProperties','Get-ExpandedReceipt','New-ExpandedSourcePng','New-ExpandedPageSequencePdf','Assert-ExpandedImageResult','Assert-ExpandedOrganizerResult')
Import-ExactFunctions (Join-Path $PSScriptRoot 'verify-default-draft.ps1') @('New-GitHubGetRequest','Assert-GitHubResponseSuccess','Invoke-GitHubJson','ConvertTo-ExactUInt64')

$script:Pins=[ordered]@{ExpectedPublisher=$ExpectedPublisher}
$script:FeaturePaths=@('src/App.tsx','src/CreatePdfDialog.tsx','src/ExportPageImageDialog.tsx','src/Organizer.tsx','src/SplitDialog.tsx','src/CropDialog.tsx','src/CombineDialog.tsx','src/InsertPagesDialog.tsx','src/ReplacePagesDialog.tsx','src/bridge.ts','src-tauri/src/image_pdf.rs','src-tauri/src/page_image.rs','src-tauri/src/split.rs','src-tauri/src/combine.rs','src-tauri/src/service.rs','src-tauri/src/main.rs','src-tauri/tauri.conf.json')

function Assert-DefaultDraftReceipt {
 param($Receipt)
 Assert-ExactProperties $Receipt @('schemaVersion','scope','mode','workflowSourceRevision','targetVersion','targetTag','targetSourceRevision','releaseId','assets','packagedApplication','baseResources','ocr','signatures','verification') 'Default draft receipt'
 if([int]$Receipt.schemaVersion-ne2-or[string]$Receipt.mode-cne'default-draft-extraction'-or[string]$Receipt.workflowSourceRevision-cne$DraftProofWorkflowRevision-or[string]$Receipt.targetVersion-cne$TargetVersion-or[string]$Receipt.targetTag-cne$TargetTag-or[string]$Receipt.targetSourceRevision-cne$TargetSourceRevision-or[string]$Receipt.releaseId-cne$TargetReleaseId){throw 'Default draft receipt identity mismatch.'}
 $assets=@($Receipt.assets);if($assets.Count-ne3){throw 'Default draft receipt asset count mismatch.'}
 $expected=@{installer=@($InstallerAssetId,$InstallerName,$InstallerBytes,$InstallerSha256);detachedUpdaterSignature=@($SignatureAssetId,"$InstallerName.sig",$SignatureBytes,$SignatureSha256);updaterManifest=@($ManifestAssetId,'latest.json',$ManifestBytes,$ManifestSha256)}
 foreach($asset in $assets){Assert-ExactProperties $asset @('role','id','name','bytes','sha256') 'Draft asset receipt';$pin=$expected[[string]$asset.role];if($null-eq$pin-or[string]$asset.id-cne$pin[0]-or[string]$asset.name-cne$pin[1]-or[string]$asset.bytes-cne$pin[2]-or[string]$asset.sha256-cne$pin[3]){throw 'Draft asset receipt mismatch.'}}
 Assert-ReceiptValue $Receipt.packagedApplication 'Packaged application'
 $base=@($Receipt.baseResources);if($base.Count-ne28){throw 'Default receipt must contain 28 base resources.'}
 $targets=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
 foreach($entry in $base){if(-not$targets.Add([string]$entry.target)-or[string]$entry.target-cnotmatch'^resources/[A-Za-z0-9._/-]+$'-or[string]$entry.target-match'(^|/)ocr(/|$)'){throw 'Default base resource is unsafe, duplicated, or OCR.'};Assert-ReceiptValue $entry 'Base resource'}
 $pdfium=@($base|Where-Object target -CEQ 'resources/pdfium/bin/pdfium.dll');if($pdfium.Count-ne1-or[uint64]$pdfium[0].unsignedBytes-eq0-or[string]$pdfium[0].unsignedSha256-cnotmatch'^[A-F0-9]{64}$'){throw 'Default receipt has no unique signed PDFium source binding.'}
 if([string]$Receipt.signatures.expectedPublisher-cne$ExpectedPublisher-or-not[bool]$Receipt.signatures.trustedTimestampsRequired-or[string]$Receipt.signatures.installer-cne'Valid'-or[string]$Receipt.signatures.application-cne'Valid'-or[string]$Receipt.signatures.pdfium-cne'Valid'-or[string]$Receipt.signatures.ocrEngine-cne'Valid'){throw 'Default receipt signature claim mismatch.'}
 Assert-ExactProperties $Receipt.ocr @('identity','resources') 'Default receipt OCR package'
 $ocrIdentity=[string]$Receipt.ocr.identity;$ocr=@($Receipt.ocr.resources);if($ocrIdentity-cnotmatch'^[a-f0-9]{64}$'-or$ocr.Count-ne5){throw 'Default receipt OCR package identity or count mismatch.'}
 $ocrExpected=@{'bin/tesseract.exe'='engine';'tessdata/eng.traineddata'='model';'licenses/Tesseract-Apache-2.0.txt'='license';'licenses/Leptonica-BSD-2-Clause.txt'='license';'licenses/eng-fast-Apache-2.0.txt'='license'};$ocrSeen=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
 foreach($entry in $ocr){Assert-ExactProperties $entry @('target','role','bytes','sha256') 'Default receipt OCR resource';$prefix="resources/ocr/$ocrIdentity/";$target=[string]$entry.target;if(-not$target.StartsWith($prefix,[StringComparison]::Ordinal)){throw 'Default receipt OCR resource is outside its identity folder.'};$suffix=$target.Substring($prefix.Length);if(-not$ocrExpected.ContainsKey($suffix)-or$ocrExpected[$suffix]-cne[string]$entry.role-or-not$ocrSeen.Add($suffix)-or$targets.Contains($target)){throw 'Default receipt OCR resource set mismatch.'};Assert-ReceiptValue $entry 'OCR resource'}
 $engine=@($ocr|Where-Object role -CEQ 'engine');if($engine.Count-ne1-or([string]$engine[0].sha256).ToLowerInvariant()-cne$ocrIdentity){throw 'Default receipt OCR engine does not match its identity.'}
 if(-not[bool]$Receipt.verification.exactAssetInventory-or-not[bool]$Receipt.verification.exactExtractedInventory-or-not[bool]$Receipt.verification.targetSourceSixVersionsMatched-or-not[bool]$Receipt.verification.packagedApplicationIdentityAndVersionMatched-or[bool]$Receipt.verification.sourceBuildExecutableByteProvenanceReconstructed-or[bool]$Receipt.verification.optionalUninstaller-or-not[bool]$Receipt.verification.optionalUninstallerExcluded-or-not[bool]$Receipt.verification.ocrBundled-or-not[bool]$Receipt.verification.manifestSignatureTextBound-or[bool]$Receipt.verification.detachedUpdaterCryptographyVerified-or[bool]$Receipt.verification.installedBehaviorVerified){throw 'Default receipt verification scope mismatch.'}
 return [pscustomobject]@{Base=$base;Pdfium=$pdfium[0];Ocr=$ocr;OcrEngine=$engine[0]}
}

function Assert-ProofSourceParameters {
 $draftOnly=@('AssetRoot','DraftProofWorkflowRevision','DraftProofRunId','DraftProofWorkflowId','DraftProofArtifactId','DraftProofArtifactName','DraftProofArtifactBytes','DraftProofArtifactDigest','DraftProofReceiptBytes','DraftProofReceiptSha256','TargetTag','TargetReleaseId','InstallerAssetId','SignatureAssetId','SignatureBytes','SignatureSha256','ManifestAssetId','ManifestBytes','ManifestSha256')
 $artifactOnly=@('ArtifactRoot','ArtifactRunId','ArtifactId','ArtifactName','ArtifactBytes','ArtifactDigest')
 if($ProofSource-cne'draft'-and$ProofSource-cne'artifact'){throw 'Proof source must be exactly draft or artifact.'}
 foreach($name in $draftOnly){$present=-not[string]::IsNullOrWhiteSpace([string](Get-Variable -Name $name -ValueOnly));if(($ProofSource-ceq'draft')-ne$present){throw "Parameter $name is required for draft verification and forbidden for the pre-tag artifact rehearsal."}}
 foreach($name in $artifactOnly){$present=-not[string]::IsNullOrWhiteSpace([string](Get-Variable -Name $name -ValueOnly));if(($ProofSource-ceq'artifact')-ne$present){throw "Parameter $name is required for the pre-tag artifact rehearsal and forbidden for draft verification."}}
}

function Assert-DefaultArtifactReceipt {
 param($Receipt)
 Assert-ExactProperties $Receipt @('schemaVersion','scope','mode','workflowSourceRevision','targetVersion','targetSourceRevision','artifact','assets','packagedApplication','baseResources','ocr','signatures','verification') 'Default artifact receipt'
 if([int]$Receipt.schemaVersion-ne2-or[string]$Receipt.mode-cne'default-artifact-extraction'-or[string]$Receipt.workflowSourceRevision-cne$WorkflowSourceRevision-or[string]$Receipt.targetVersion-cne$TargetVersion-or[string]$Receipt.targetSourceRevision-cne$TargetSourceRevision){throw 'Default artifact receipt identity mismatch.'}
 if($ArtifactName-cne"signed-ocr-installer-$TargetSourceRevision"-or$ArtifactDigest-cnotmatch'^sha256:[a-f0-9]{64}$'-or$InstallerName-cne"PDF Workstation_${TargetVersion}_x64-setup.exe"){throw 'Rehearsal artifact identity input is malformed.'}
 Assert-ExactProperties $Receipt.artifact @('runId','id','name','bytes','digest') 'Default artifact identity'
 if([string]$Receipt.artifact.runId-cne[string](ConvertTo-ExactUInt64 $ArtifactRunId 'Artifact run id')-or[string]$Receipt.artifact.id-cne[string](ConvertTo-ExactUInt64 $ArtifactId 'Artifact id')-or[string]$Receipt.artifact.name-cne$ArtifactName-or[string]$Receipt.artifact.bytes-cne[string](ConvertTo-ExactUInt64 $ArtifactBytes 'Artifact bytes')-or[string]$Receipt.artifact.digest-cne$ArtifactDigest){throw 'Default artifact receipt is not bound to the pinned artifact.'}
 $assets=@($Receipt.assets);if($assets.Count-ne1){throw 'Default artifact receipt must hold exactly the installer.'}
 Assert-ExactProperties $assets[0] @('role','name','bytes','sha256') 'Artifact installer receipt'
 if([string]$assets[0].role-cne'installer'-or[string]$assets[0].name-cne$InstallerName-or[string]$assets[0].bytes-cne$InstallerBytes-or[string]$assets[0].sha256-cne$InstallerSha256){throw 'Artifact installer receipt mismatch.'}
 Assert-ReceiptValue $Receipt.packagedApplication 'Packaged application'
 $base=@($Receipt.baseResources);if($base.Count-ne28){throw 'Default receipt must contain 28 base resources.'}
 $targets=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
 foreach($entry in $base){if(-not$targets.Add([string]$entry.target)-or[string]$entry.target-cnotmatch'^resources/[A-Za-z0-9._/-]+$'-or[string]$entry.target-match'(^|/)ocr(/|$)'){throw 'Default base resource is unsafe, duplicated, or OCR.'};Assert-ReceiptValue $entry 'Base resource'}
 $pdfium=@($base|Where-Object target -CEQ 'resources/pdfium/bin/pdfium.dll');if($pdfium.Count-ne1-or[uint64]$pdfium[0].unsignedBytes-eq0-or[string]$pdfium[0].unsignedSha256-cnotmatch'^[A-F0-9]{64}$'){throw 'Default receipt has no unique signed PDFium source binding.'}
 if([string]$Receipt.signatures.expectedPublisher-cne$ExpectedPublisher-or-not[bool]$Receipt.signatures.trustedTimestampsRequired-or[string]$Receipt.signatures.installer-cne'Valid'-or[string]$Receipt.signatures.application-cne'Valid'-or[string]$Receipt.signatures.pdfium-cne'Valid'-or[string]$Receipt.signatures.ocrEngine-cne'Valid'){throw 'Default receipt signature claim mismatch.'}
 Assert-ExactProperties $Receipt.ocr @('identity','resources') 'Default receipt OCR package'
 $ocrIdentity=[string]$Receipt.ocr.identity;$ocr=@($Receipt.ocr.resources);if($ocrIdentity-cnotmatch'^[a-f0-9]{64}$'-or$ocr.Count-ne5){throw 'Default receipt OCR package identity or count mismatch.'}
 $ocrExpected=@{'bin/tesseract.exe'='engine';'tessdata/eng.traineddata'='model';'licenses/Tesseract-Apache-2.0.txt'='license';'licenses/Leptonica-BSD-2-Clause.txt'='license';'licenses/eng-fast-Apache-2.0.txt'='license'};$ocrSeen=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
 foreach($entry in $ocr){Assert-ExactProperties $entry @('target','role','bytes','sha256') 'Default receipt OCR resource';$prefix="resources/ocr/$ocrIdentity/";$target=[string]$entry.target;if(-not$target.StartsWith($prefix,[StringComparison]::Ordinal)){throw 'Default receipt OCR resource is outside its identity folder.'};$suffix=$target.Substring($prefix.Length);if(-not$ocrExpected.ContainsKey($suffix)-or$ocrExpected[$suffix]-cne[string]$entry.role-or-not$ocrSeen.Add($suffix)-or$targets.Contains($target)){throw 'Default receipt OCR resource set mismatch.'};Assert-ReceiptValue $entry 'OCR resource'}
 $engine=@($ocr|Where-Object role -CEQ 'engine');if($engine.Count-ne1-or([string]$engine[0].sha256).ToLowerInvariant()-cne$ocrIdentity){throw 'Default receipt OCR engine does not match its identity.'}
 $v=$Receipt.verification
 if([bool]$v.draft-or-not[bool]$v.releaseCandidateArtifact-or-not[bool]$v.exactAssetInventory-or-not[bool]$v.exactExtractedInventory-or-not[bool]$v.targetSourceSixVersionsMatched-or-not[bool]$v.packagedApplicationIdentityAndVersionMatched-or[bool]$v.sourceBuildExecutableByteProvenanceReconstructed-or[bool]$v.optionalUninstaller-or-not[bool]$v.optionalUninstallerExcluded-or-not[bool]$v.ocrBundled-or-not[bool]$v.packagedApplicationEmbedsCandidateUpdaterKey-or-not[bool]$v.updaterArtifactsAbsent-or[bool]$v.installedBehaviorVerified){throw 'Default artifact receipt verification scope mismatch.'}
 return [pscustomobject]@{Base=$base;Pdfium=$pdfium[0];Ocr=$ocr;OcrEngine=$engine[0]}
}

function Read-DefaultArtifactRehearsal { param([string]$ProofRoot,[string]$ArtifactPath)
 $files=@(Get-ChildItem -LiteralPath $ProofRoot -Recurse -Force);if($files.Count-ne1-or$files[0].PSIsContainer-or$files[0].Name-cne'default-artifact-verification.json'-or$files[0].Length-gt1MB){throw 'The rehearsal proof root must hold exactly one artifact extraction receipt.'}
 $text=Get-Content -LiteralPath $files[0].FullName -Raw -Encoding UTF8;if($text-match'(?i)([A-Z]:\\|\\Users\\|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log)'){throw 'Rehearsal artifact receipt is not sanitized.'}
 $receipt=$text|ConvertFrom-Json;$sets=Assert-DefaultArtifactReceipt $receipt
 $exported=@(Get-ChildItem -LiteralPath $ArtifactPath -Recurse -Force);if($exported.Count-ne2-or@($exported|Where-Object{$_.PSIsContainer}).Count-ne0){throw 'Signed artifact must contain exactly the installer and its verification record.'}
 $installer=Join-Path $ArtifactPath $InstallerName;[uint64]$bytes=0;if(-not[uint64]::TryParse($InstallerBytes,[ref]$bytes)){throw 'Rehearsal installer byte input is invalid.'};Assert-FileReceipt $installer $bytes $InstallerSha256 'Rehearsal installer'
 $binding=[ordered]@{source='ocr-signed-artifact';artifactRunId=[uint64]$receipt.artifact.runId;artifactId=[uint64]$receipt.artifact.id;artifactName=[string]$receipt.artifact.name;artifactBytes=[uint64]$receipt.artifact.bytes;artifactDigest=[string]$receipt.artifact.digest;installer=[ordered]@{bytes=$bytes;sha256=$InstallerSha256};extractionReceipt=[ordered]@{bytes=[uint64]$files[0].Length;sha256=Get-ExactSha256 $files[0].FullName};draftVerified=$false;releaseVerified=$false;updaterVerified=$false}
 return [pscustomobject]@{Receipt=$receipt;Sets=$sets;Installer=$installer;Binding=$binding}
}

function Assert-IndependentReceiptParity { param($Pinned,$Independent)
 foreach($name in @('targetVersion','targetTag','targetSourceRevision','releaseId')){if([string]$Pinned.$name-cne[string]$Independent.$name){throw 'Independent draft identity differs from Step 1.'}}
 foreach($name in @('assets','packagedApplication','baseResources','signatures')){if(($Pinned.$name|ConvertTo-Json -Depth 20 -Compress)-cne($Independent.$name|ConvertTo-Json -Depth 20 -Compress)){throw 'Independent draft proof differs from Step 1.'}}
}

function Assert-Step1ArtifactMetadata {
 param([string]$Token)
 $runId=ConvertTo-ExactUInt64 $DraftProofRunId 'Step 1 run id';$workflowId=ConvertTo-ExactUInt64 $DraftProofWorkflowId 'Step 1 workflow id';$artifactId=ConvertTo-ExactUInt64 $DraftProofArtifactId 'Step 1 artifact id';$artifactBytes=ConvertTo-ExactUInt64 $DraftProofArtifactBytes 'Step 1 artifact bytes'
 if($DraftProofWorkflowRevision-cnotmatch'^[a-f0-9]{40}$'-or$DraftProofArtifactName-cne"default-draft-verification-$TargetSourceRevision"-or$DraftProofArtifactDigest-cnotmatch'^sha256:[a-f0-9]{64}$'){throw 'Step 1 artifact identity input is malformed.'}
 $artifact=Invoke-GitHubJson "https://api.github.com/repos/Joshua-Beel/PDF-Workstation/actions/artifacts/$artifactId" $Token
 if([uint64]$artifact.id-ne$artifactId-or[string]$artifact.name-cne$DraftProofArtifactName-or[uint64]$artifact.size_in_bytes-ne$artifactBytes-or[string]$artifact.digest-cne$DraftProofArtifactDigest-or[uint64]$artifact.workflow_run.id-ne$runId-or[string]$artifact.workflow_run.head_sha-cne$DraftProofWorkflowRevision-or[bool]$artifact.expired){throw 'Step 1 artifact metadata mismatch.'}
 $run=Invoke-GitHubJson "https://api.github.com/repos/Joshua-Beel/PDF-Workstation/actions/runs/$runId" $Token
 if([uint64]$run.id-ne$runId-or[uint64]$run.workflow_id-ne$workflowId-or[string]$run.name-cne'Manual default draft verification'-or[string]$run.event-cne'workflow_dispatch'-or[string]$run.status-cne'completed'-or[string]$run.conclusion-cne'success'-or[string]$run.head_branch-cne'master'-or[string]$run.head_sha-cne$DraftProofWorkflowRevision-or[string]$run.path-cne'.github/workflows/default-draft-verification.yml'){throw 'Step 1 workflow run identity mismatch.'}
}

function Assert-FeatureSourceParity {
 foreach($path in $script:FeaturePaths){$target=(& git -C $ProjectRoot rev-parse "$TargetSourceRevision`:$path").Trim();$current=(& git -C $ProjectRoot rev-parse "HEAD`:$path").Trim();if($LASTEXITCODE-ne0-or$target-cne$current){throw "Installed page-tool source parity failed: $path"}}
 $source=(& git -C $ProjectRoot show "$TargetSourceRevision`:src/App.tsx")-join"`n";$markers=@('Combine files','Organize pages','setCombineOpen(true)','setInsertOpen(true)','setReplaceOpen(true)','combineDocuments(first, second)','insertPagesCopy(target, donor, at)','replacePagesCopy(target, donor, start, count)','createPdfFromImage','exportPageImage');foreach($marker in $markers){if(-not$source.Contains($marker,[StringComparison]::Ordinal)){throw 'Target application shell lacks a page-tool marker.'}}
 return [ordered]@{exactFeatureBlobsMatched=$true;featureBlobCount=$script:FeaturePaths.Count;selectorMarkerCount=$markers.Count}
}

function Assert-InstalledDefaultResources { param([string]$InstallRoot,$Receipt,$Sets)
 $files=@(Get-ChildItem -LiteralPath $InstallRoot -Recurse -File -Force);$root=[IO.Path]::GetFullPath($InstallRoot).TrimEnd('\');$actual=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase);foreach($file in $files){$path=[IO.Path]::GetFullPath($file.FullName);Assert-NoReparseAncestors $path;if(-not$path.StartsWith($root+'\',[StringComparison]::OrdinalIgnoreCase)-or-not$actual.Add($path.Substring($root.Length+1).Replace('\','/'))){throw 'Installed inventory escaped, duplicated, or case-collided.'}}
 $required=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase);$null=$required.Add('pdf-workstation.exe');$null=$required.Add('uninstall.exe');foreach($entry in $Sets.Base){$null=$required.Add([string]$entry.target);$file=Get-UniqueResource $files ([string]$entry.target) 'Installed base resource';Assert-ReceiptFileObject $file $entry 'Installed base resource'};foreach($entry in $Sets.Ocr){$null=$required.Add([string]$entry.target);$file=Get-UniqueResource $files ([string]$entry.target) 'Installed OCR resource';Assert-ReceiptFileObject $file $entry 'Installed OCR resource'}
 foreach($path in $required){if(-not$actual.Contains($path)){throw 'Installed inventory is incomplete.'}};foreach($path in $actual){if(-not$required.Contains($path)){throw 'Installed inventory contains an unexpected file.'}}
 $app=Get-UniqueResource $files 'pdf-workstation.exe' 'Installed application';Assert-ReceiptFileObject $app $Receipt.packagedApplication 'Installed application';$pdfium=Get-UniqueResource $files 'resources/pdfium/bin/pdfium.dll' 'Installed PDFium';$welcome=Get-UniqueResource $files 'resources/welcome.pdf' 'Installed welcome sample'
 $null=Assert-TrustedWindowsSignature $app.FullName -ExpectedPublisher $ExpectedPublisher;$null=Assert-TrustedWindowsSignature $pdfium.FullName -ExpectedPublisher $ExpectedPublisher;$engine=Get-UniqueResource $files ([string]$Sets.OcrEngine.target) 'Installed OCR engine';$null=Assert-TrustedWindowsSignature $engine.FullName -ExpectedPublisher $ExpectedPublisher
 if(@($files|Where-Object{$_.Name.Equals('latest.json',[StringComparison]::OrdinalIgnoreCase)-or$_.Extension.Equals('.sig',[StringComparison]::OrdinalIgnoreCase)}).Count){throw 'Installed default application contains updater payloads.'}
 return [pscustomobject]@{Application=$app;Pdfium=$pdfium;Welcome=$welcome}
}

function Invoke-SilentUninstall { param([string]$Path)
 $start=[Diagnostics.ProcessStartInfo]::new();$start.FileName=$Path;$null=$start.ArgumentList.Add('/S');$start.UseShellExecute=$false;$start.CreateNoWindow=$true;$start.WindowStyle='Hidden';$process=[Diagnostics.Process]::new();$process.StartInfo=$start;try{if(-not$process.Start()){throw 'Uninstaller did not start.'};if(-not$process.WaitForExit(240000)){try{$process.Kill($true)}catch{};throw 'Uninstaller timed out.'};if($process.ExitCode-ne0){throw 'Uninstaller failed.'}}finally{$process.Dispose()}
}

function Invoke-OwnedCleanup {
 param([bool]$Installed,[string]$InstallRoot,[string]$SettingsRoot,[scriptblock]$UninstallProvider,[scriptblock]$RemoveProvider)
 $uninstaller=Join-Path $InstallRoot 'uninstall.exe'
 if($Installed){if(-not(Test-Path -LiteralPath $uninstaller)){throw 'Owned installation has no uninstaller for cleanup.'};if($UninstallProvider){&$UninstallProvider $uninstaller}else{Invoke-SilentUninstall $uninstaller}}
 if(Test-Path -LiteralPath $SettingsRoot){Assert-NoReparseAncestors $SettingsRoot;foreach($entry in @(Get-ChildItem -LiteralPath $SettingsRoot -Recurse -Force)){Assert-NoReparseAncestors $entry.FullName};if($RemoveProvider){&$RemoveProvider $SettingsRoot}else{Remove-Item -LiteralPath $SettingsRoot -Recurse -Force}}
}

function Assert-EmptyProcessFacts { param($Facts,[string]$Kind)
 if($Facts.applicationProcessPresent-isnot[bool]-or$Facts.installerProcessPresent-isnot[bool]-or[bool]$Facts.applicationProcessPresent-or[bool]$Facts.installerProcessPresent){throw "$Kind process facts are not exactly empty."}
}

function Assert-OwnedCleanupState { param([string]$RegistryPath,[string]$InstallRoot,[string]$SettingsRoot,$ProcessFacts)
 if((Test-Path -LiteralPath $RegistryPath)-or(Test-Path -LiteralPath $InstallRoot)-or(Test-Path -LiteralPath $SettingsRoot)){throw 'Owned installed state remains after cleanup.'};Assert-EmptyProcessFacts $ProcessFacts 'Final'
}

function Complete-PrimaryAndCleanup { param($Primary,$Cleanup)
 if($null-ne$Primary){if($null-ne$Cleanup){$Primary.Exception.Data['ownedCleanupFailed']=$true};throw $Primary};if($null-ne$Cleanup){throw $Cleanup}
}

function Write-InstalledRecord { param($Source,$Receipt,$Image,$Organizer,$Structural,$BaselineProcesses,$FinalProcesses)
 [IO.Directory]::CreateDirectory($OutputRoot)|Out-Null;$installerReceipt=@($Receipt.assets|Where-Object role -CEQ 'installer')[0];$record=[ordered]@{schemaVersion=1;scope='Installed default image, organizer, and structural page-tool verification with controlled fixtures; the packaged OCR files are receipt-checked but no OCR recognition, updater cryptography, comments, forms, print, or general behavior claim.';workflowSourceRevision=$WorkflowSourceRevision;targetSourceRevision=$TargetSourceRevision;targetVersion=$TargetVersion;sourceBinding=$Source;step1=[ordered]@{workflowSourceRevision=$DraftProofWorkflowRevision;workflowId=[uint64]$DraftProofWorkflowId;runId=[uint64]$DraftProofRunId;artifactId=[uint64]$DraftProofArtifactId;artifactDigest=$DraftProofArtifactDigest;receiptBytes=[uint64]$DraftProofReceiptBytes;receiptSha256=$DraftProofReceiptSha256};draft=[ordered]@{releaseId=[uint64]$Receipt.releaseId;installer=[ordered]@{bytes=[uint64]$installerReceipt.bytes;sha256=[string]$installerReceipt.sha256};independentlyRedownloaded=$true};imagePageTools=$Image;organizerTools=$Organizer;structuralPageTools=$Structural;cleanup=[ordered]@{silentUninstallCompleted=$true;baseline=$BaselineProcesses;final=$FinalProcesses;settingsSentinelPreservedThroughTests=$true;ownedSettingsRemoved=$true};ocrBundled=$true;ocrRecognitionVerified=$false}
 $json=$record|ConvertTo-Json -Depth 20;if($json-match'(?i)([A-Z]:\\|\\Users\\|runneradmin|RUNNER_TEMP|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log|\.pdf-workstation)'){throw 'Installed receipt contains host or credential data.'};[IO.File]::WriteAllText((Join-Path $OutputRoot 'default-expanded-page-tools-verification.json'),$json,[Text.UTF8Encoding]::new($false))
}

function Write-RehearsalInstalledRecord { param($Source,$Rehearsal,$Image,$Organizer,$Structural,$BaselineProcesses,$FinalProcesses)
 [IO.Directory]::CreateDirectory($OutputRoot)|Out-Null;$record=[ordered]@{schemaVersion=1;scope='Pre-tag rehearsal: installed default image, organizer, and structural page-tool verification with controlled fixtures, run on a signed release-candidate artifact rather than a release draft; no release, draft, updater, OCR recognition, comments, forms, print, or general behavior claim.';workflowSourceRevision=$WorkflowSourceRevision;targetSourceRevision=$TargetSourceRevision;targetVersion=$TargetVersion;sourceBinding=$Source;rehearsal=$Rehearsal;imagePageTools=$Image;organizerTools=$Organizer;structuralPageTools=$Structural;cleanup=[ordered]@{silentUninstallCompleted=$true;baseline=$BaselineProcesses;final=$FinalProcesses;settingsSentinelPreservedThroughTests=$true;ownedSettingsRemoved=$true};ocrBundled=$true;ocrRecognitionVerified=$false}
 $json=$record|ConvertTo-Json -Depth 20;if($json-match'(?i)([A-Z]:\\|\\Users\\|runneradmin|RUNNER_TEMP|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log|\.pdf-workstation)'){throw 'Installed receipt contains host or credential data.'};[IO.File]::WriteAllText((Join-Path $OutputRoot 'default-expanded-page-tools-rehearsal.json'),$json,[Text.UTF8Encoding]::new($false))
}

Assert-ProofSourceParameters
if($ProofSource-ceq'draft'){
if($TargetVersion-cnotmatch'^\d+\.\d+\.\d+$'-or$TargetTag-cne('v'+$TargetVersion)){throw 'Target version and stable tag are malformed or inconsistent.'}
$token=$env:GITHUB_TOKEN;if([string]::IsNullOrWhiteSpace($token)){throw 'GitHub token is unavailable for exact proof reacquisition.'};Assert-Step1ArtifactMetadata $token
$proofRoot=Resolve-RunnerPath $DraftProofRoot $env:RUNNER_TEMP -MustExist;$assetRoot=Resolve-RunnerPath $AssetRoot $env:RUNNER_TEMP -MustBeFresh;$work=Resolve-RunnerPath $WorkRoot $env:RUNNER_TEMP -MustBeFresh;$output=Resolve-RunnerPath $OutputRoot $env:RUNNER_TEMP -MustBeFresh;$drivers=Resolve-RunnerPath $WebDriverRoot $env:RUNNER_TEMP -MustExist
$proofFiles=@(Get-ChildItem -LiteralPath $proofRoot -Recurse -File -Force);if($proofFiles.Count-ne1-or$proofFiles[0].Name-cne'default-draft-verification.json'){throw 'Step 1 artifact must contain one exact receipt.'}
[uint64]$proofBytes=0;if(-not[uint64]::TryParse($DraftProofReceiptBytes,[ref]$proofBytes)){throw 'Step 1 receipt byte input is invalid.'};Assert-FileReceipt $proofFiles[0].FullName $proofBytes $DraftProofReceiptSha256 'Step 1 receipt'
$proofText=Get-Content $proofFiles[0].FullName -Raw -Encoding UTF8;if($proofText-match'(?i)([A-Z]:\\|\\Users\\|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log)'){throw 'Step 1 receipt is not sanitized.'};$receipt=$proofText|ConvertFrom-Json;$sets=Assert-DefaultDraftReceipt $receipt
$sourceBinding=Assert-FeatureSourceParity
$independentWork=Join-Path $work 'independent-draft-work';$independentOutput=Join-Path $work 'independent-draft-output'
& (Join-Path $PSScriptRoot 'verify-default-draft.ps1') -ProjectRoot $ProjectRoot -TargetSourceRoot $TargetSourceRoot -AssetRoot $assetRoot -WorkRoot $independentWork -OutputRoot $independentOutput -WorkflowSourceRevision $WorkflowSourceRevision -TargetVersion $TargetVersion -TargetTag $TargetTag -TargetSourceRevision $TargetSourceRevision -TargetReleaseId $TargetReleaseId -InstallerAssetId $InstallerAssetId -InstallerName $InstallerName -InstallerBytes $InstallerBytes -InstallerSha256 $InstallerSha256 -SignatureAssetId $SignatureAssetId -SignatureBytes $SignatureBytes -SignatureSha256 $SignatureSha256 -ManifestAssetId $ManifestAssetId -ManifestBytes $ManifestBytes -ManifestSha256 $ManifestSha256 -ExpectedPublisher $ExpectedPublisher
$independent=Get-Content (Join-Path $independentOutput 'default-draft-verification.json') -Raw -Encoding UTF8|ConvertFrom-Json;Assert-IndependentReceiptParity $receipt $independent
$env:GITHUB_TOKEN=$null;$env:GH_TOKEN=$null;$token=$null
}else{
if($TargetVersion-cnotmatch'^\d+\.\d+\.\d+$'-or$WorkflowSourceRevision-cnotmatch'^[a-f0-9]{40}$'-or$TargetSourceRevision-cnotmatch'^[a-f0-9]{40}$'){throw 'Rehearsal target identity is malformed.'}
$env:GITHUB_TOKEN=$null;$env:GH_TOKEN=$null
$proofRoot=Resolve-RunnerPath $DraftProofRoot $env:RUNNER_TEMP -MustExist;$assetRoot=Resolve-RunnerPath $ArtifactRoot $env:RUNNER_TEMP -MustExist;$work=Resolve-RunnerPath $WorkRoot $env:RUNNER_TEMP -MustBeFresh;$output=Resolve-RunnerPath $OutputRoot $env:RUNNER_TEMP -MustBeFresh;$drivers=Resolve-RunnerPath $WebDriverRoot $env:RUNNER_TEMP -MustExist
$rehearsal=Read-DefaultArtifactRehearsal $proofRoot $assetRoot;$receipt=$rehearsal.Receipt;$sets=$rehearsal.Sets
$sourceBinding=Assert-FeatureSourceParity
}
$installer=Join-Path $assetRoot $InstallerName;$installRoot=[IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'PDF Workstation')).TrimEnd('\');$settingsRoot=[IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\');$registry='Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDF Workstation'
$fresh=Get-FreshInstallFacts $installRoot $settingsRoot $registry @($installer);Assert-FreshInstallFacts $fresh
$baselineProcesses=Get-ConflictingProcessFacts @($installer);Assert-EmptyProcessFacts $baselineProcesses 'Baseline'
[IO.Directory]::CreateDirectory($work)|Out-Null;$installedOk=$false;$primaryFailure=$null;$cleanupFailure=$null;$sentinel=$null;$sentinelReceipt=$null;$finalProcesses=$null
try{
 Invoke-BoundedSilentInstaller $installer;$installedOk=$true;$facts=Get-InstallFacts $registry $installRoot;Assert-InstallFacts $facts $TargetVersion $installRoot $receipt.packagedApplication 'Valid' $ExpectedPublisher $true;$resources=Assert-InstalledDefaultResources $installRoot $receipt $sets
 if(Test-Path $settingsRoot){throw 'Silent install created settings.'};[IO.Directory]::CreateDirectory($settingsRoot)|Out-Null
 $organizerFixture=Join-Path $work 'organizer-source.pdf';$donorFixture=Join-Path $work 'structural-donor.pdf';$organizerReceipt=New-ExpandedPageSequencePdf $organizerFixture TARGET;$donorReceipt=New-ExpandedPageSequencePdf $donorFixture DONOR;$png=Join-Path $work 'image-source.png';$pngReceipt=New-ExpandedSourcePng $png;$split=Join-Path $work 'split-parent';[IO.Directory]::CreateDirectory($split)|Out-Null
 $image=Invoke-InstalledImagePageTools -ApplicationPath $resources.Application.FullName -ApplicationReceipt $receipt.packagedApplication -PdfiumPath $resources.Pdfium.FullName -PdfiumReceipt $sets.Pdfium -WebDriverRoot $drivers -ProfileRoot (Join-Path $work 'image-profile') -SettingsRoot $settingsRoot -SourceImagePath $png -SourceImageReceipt $pngReceipt -CreatedPdfPath (Join-Path $work 'image-source.pdf') -ExportedPngPath (Join-Path $work 'image-source-page-1.png') -ExpectedPublisher $ExpectedPublisher;Assert-ExpandedImageResult $image
 if(@(Get-ChildItem $settingsRoot -Force).Count){throw 'Image tools escaped profile.'};$sentinel=Join-Path $settingsRoot 'page-tools-sentinel.json';[IO.File]::WriteAllText($sentinel,'{"kind":"page-tools-sentinel","version":1}',[Text.UTF8Encoding]::new($false));$sentinelReceipt=Get-ExpandedReceipt $sentinel
 $organizer=Invoke-InstalledOrganizerTools -ApplicationPath $resources.Application.FullName -ApplicationReceipt $receipt.packagedApplication -PdfiumPath $resources.Pdfium.FullName -PdfiumReceipt $sets.Pdfium -WebDriverRoot $drivers -ProfileRoot (Join-Path $work 'organizer-profile') -SettingsRoot $settingsRoot -FixturePath $organizerFixture -FixtureReceipt $organizerReceipt -SplitParentRoot $split -ExpectedPublisher $ExpectedPublisher;Assert-ExpandedOrganizerResult $organizer;Assert-StructuralFileReceipt $sentinel $sentinelReceipt 'Organizer settings sentinel'
 $structural=Invoke-InstalledStructuralPageTools -ApplicationPath $resources.Application.FullName -ApplicationReceipt $receipt.packagedApplication -PdfiumPath $resources.Pdfium.FullName -PdfiumReceipt $sets.Pdfium -WebDriverRoot $drivers -ProfileRoot (Join-Path $work 'structural-profile') -SettingsRoot $settingsRoot -FirstFixturePath $organizerFixture -FirstFixtureReceipt $organizerReceipt -SecondFixturePath $donorFixture -SecondFixtureReceipt $donorReceipt -CombineOutputPath (Join-Path $work 'combined.pdf') -InsertOutputPath (Join-Path $work 'inserted-pages.pdf') -ReplaceOutputPath (Join-Path $work 'replaced-pages.pdf') -ExpectedPublisher $ExpectedPublisher;Assert-StructuralResult $structural;Assert-StructuralFileReceipt $sentinel $sentinelReceipt 'Structural settings sentinel'
 foreach($item in @(@($resources.Application.FullName,$receipt.packagedApplication),@($resources.Pdfium.FullName,$sets.Pdfium),@($organizerFixture,$organizerReceipt),@($donorFixture,$donorReceipt),@($png,$pngReceipt),@($sentinel,$sentinelReceipt))){Assert-StructuralFileReceipt $item[0] $item[1] 'Final preserved input'}
}catch{$primaryFailure=$_}finally{
 try{
 Invoke-OwnedCleanup $installedOk $installRoot $settingsRoot
  $finalProcesses=Get-ConflictingProcessFacts @($installer);Assert-OwnedCleanupState $registry $installRoot $settingsRoot $finalProcesses
 }catch{$cleanupFailure=$_}
}
Complete-PrimaryAndCleanup $primaryFailure $cleanupFailure
if($ProofSource-ceq'draft'){Write-InstalledRecord $sourceBinding $receipt $image $organizer $structural $baselineProcesses $finalProcesses}else{Write-RehearsalInstalledRecord $sourceBinding $rehearsal.Binding $image $organizer $structural $baselineProcesses $finalProcesses}
