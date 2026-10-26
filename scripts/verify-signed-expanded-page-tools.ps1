[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$SignedArtifactRoot,
    [Parameter(Mandatory = $true)][string]$WorkRoot,
    [Parameter(Mandatory = $true)][string]$OutputRoot,
    [Parameter(Mandatory = $true)][string]$WebDriverRoot,
    [Parameter(Mandatory = $true)][string]$TargetSourceRoot,
    [Parameter(Mandatory = $true)][string]$TargetVersion,
    [Parameter(Mandatory = $true)][string]$SourceRevision,
    [Parameter(Mandatory = $true)][string]$ArtifactRunId,
    [Parameter(Mandatory = $true)][string]$ArtifactId,
    [Parameter(Mandatory = $true)][string]$ArtifactName,
    [Parameter(Mandatory = $true)][string]$ArtifactBytes,
    [Parameter(Mandatory = $true)][string]$ArtifactDigest,
    [Parameter(Mandatory = $true)][string]$InstallerName,
    [Parameter(Mandatory = $true)][string]$InstallerBytes,
    [Parameter(Mandatory = $true)][string]$InstallerSha256,
    [Parameter(Mandatory = $true)][string]$RecordBytes,
    [Parameter(Mandatory = $true)][string]$RecordSha256
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'installed-image-page-tools.ps1')
. (Join-Path $PSScriptRoot 'installed-organizer-tools.ps1')
. (Join-Path $PSScriptRoot 'installed-structural-page-tools.ps1')

$trustedVerifier = Join-Path $PSScriptRoot 'verify-signed-ocr-upgrade.ps1'
$tokens = $null; $parseErrors = $null
$trustedAst = [Management.Automation.Language.Parser]::ParseFile($trustedVerifier,[ref]$tokens,[ref]$parseErrors)
if ($parseErrors.Count -ne 0) { throw 'The trusted signed-installer verifier did not parse.' }
foreach ($definition in @($trustedAst.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] },$true))) { Invoke-Expression $definition.Extent.Text }
. (Join-Path $PSScriptRoot 'signed-check-inputs.ps1')

$script:ExpandedPins = [ordered]@{
    Repository = 'Joshua-Beel/PDF-Workstation'
    ExpectedPublisher = 'Joshua Beel'
    FeaturePaths = @(
        'src/CreatePdfDialog.tsx','src/ExportPageImageDialog.tsx','src/Organizer.tsx','src/SplitDialog.tsx','src/CropDialog.tsx','src/CombineDialog.tsx',
        'src/InsertPagesDialog.tsx','src/ReplacePagesDialog.tsx','src/bridge.ts','src-tauri/src/image_pdf.rs','src-tauri/src/page_image.rs','src-tauri/src/split.rs',
        'src-tauri/src/combine.rs','src-tauri/src/service.rs','src-tauri/src/main.rs','src-tauri/tauri.conf.json'
    )
}
$script:Pins = [ordered]@{
    Repository = $script:ExpandedPins.Repository
    ExpectedPublisher = $script:ExpandedPins.ExpectedPublisher
}

function Initialize-ExpandedPins {
    param($Inputs,[string[]]$BaseResourceTargets)
    Add-SignedPinFields -Pins $script:ExpandedPins -Inputs $Inputs -BaseResourceTargets $BaseResourceTargets
    Add-SignedPinFields -Pins $script:Pins -Inputs $Inputs -BaseResourceTargets $BaseResourceTargets
}

function Assert-ExpandedExactProperties {
    param($Value,[string[]]$Expected,[string]$Kind)
    $actual = @($Value.PSObject.Properties.Name | Sort-Object -CaseSensitive); $wanted = @($Expected | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $wanted.Count -or (Compare-Object $wanted $actual -CaseSensitive)) { throw "$Kind has an unexpected or missing property." }
}

function Resolve-ExpandedRunnerPath {
    param([string]$Path,[switch]$MustExist,[switch]$MustBeFresh)
    if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) { throw 'RUNNER_TEMP is unavailable.' }
    $runner = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\'); $candidate = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if (-not $candidate.StartsWith($runner + '\',[StringComparison]::OrdinalIgnoreCase)) { throw 'Expanded page-tools paths must stay beneath RUNNER_TEMP.' }
    Assert-NoReparseAncestors -Path $candidate
    if ($MustExist -and -not (Test-Path -LiteralPath $candidate)) { throw 'A required expanded page-tools input is missing.' }
    if ($MustBeFresh -and (Test-Path -LiteralPath $candidate)) { throw 'An expanded page-tools output path is not fresh.' }
    return $candidate
}

function Assert-ExpandedHostedSource {
    param([string]$ProjectRoot)
    $head = (& git -C $ProjectRoot rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or $env:RUNNER_OS -cne 'Windows' -or
        $env:ImageOS -cne 'win22' -or $env:GITHUB_EVENT_NAME -cne 'workflow_dispatch' -or $env:GITHUB_REF -cne 'refs/heads/master' -or
        $env:GITHUB_REPOSITORY -cne $script:ExpandedPins.Repository -or $env:GITHUB_SHA -cnotmatch '^[a-f0-9]{40}$' -or $head -cne $env:GITHUB_SHA -or
        -not [string]::IsNullOrEmpty($env:GITHUB_TOKEN) -or -not [string]::IsNullOrEmpty($env:GH_TOKEN)) { throw 'Expanded page-tools verification requires the exact clean credential-free hosted Windows manual master run.' }
    & git -C $ProjectRoot diff --quiet --; if ($LASTEXITCODE -ne 0) { throw 'Tracked files changed before expanded page-tools verification.' }
    & git -C $ProjectRoot diff --cached --quiet --; if ($LASTEXITCODE -ne 0) { throw 'The tracked index changed before expanded page-tools verification.' }
}

function Assert-SignedExpandedSourceParity {
    param([string]$ProjectRoot,[string]$CurrentRevision = 'HEAD')
    $signedBlobs = Get-SignedSourceBlobs -ProjectRoot $ProjectRoot -Revision $script:ExpandedPins.SignedSourceRevision -Paths @($script:ExpandedPins.FeaturePaths + 'src/App.tsx')
    foreach ($path in $script:ExpandedPins.FeaturePaths) {
        $current = (& git -C $ProjectRoot rev-parse "${CurrentRevision}:$path").Trim()
        if ($LASTEXITCODE -ne 0 -or $current -cne $signedBlobs[$path]) { throw "An expanded page-tools feature blob does not match the exact signed and dispatched source: $path" }
    }
    $signedShell = $signedBlobs['src/App.tsx']; $currentShell = (& git -C $ProjectRoot rev-parse "${CurrentRevision}:src/App.tsx").Trim()
    $shellSource = (& git -C $ProjectRoot show "$($script:ExpandedPins.SignedSourceRevision):src/App.tsx") -join "`n"
    $markers = @('Combine files','Organize pages','setCombineOpen(true)','setInsertOpen(true)','setReplaceOpen(true)','combineDocuments(first, second)','insertPagesCopy(target, donor, at)','replacePagesCopy(target, donor, start, count)','createPdfFromImage','exportPageImage')
    foreach ($marker in $markers) { if (-not $shellSource.Contains($marker,[StringComparison]::Ordinal)) { throw 'A required expanded page-tools selector marker is absent from the exact signed application shell.' } }
    return [pscustomobject][ordered]@{ exactSignedFeatureBlobsMatched=$true;featureBlobCount=[int]$script:ExpandedPins.FeaturePaths.Count;signedSelectorMarkerCount=$markers.Count;currentApplicationShellMatchesSigned=[bool]($currentShell -ceq $signedShell) }
}

function Get-ExpandedReceipt {
    param([string]$Path)
    $item = Get-Item -LiteralPath $Path -Force
    return [pscustomobject][ordered]@{ bytes=[uint64]$item.Length;sha256=Get-ExactSha256 -Path $Path }
}

function New-ExpandedSourcePng {
    param([string]$Path)
    Add-Type -AssemblyName System.Drawing
    $bitmap = [Drawing.Bitmap]::new(64,96,[Drawing.Imaging.PixelFormat]::Format24bppRgb)
    try {
        for ($y=0;$y -lt 96;$y++) { for ($x=0;$x -lt 64;$x++) {
            $color = if ($x -lt 21) { [Drawing.Color]::FromArgb(218,71,64) } elseif ($x -lt 42) { [Drawing.Color]::FromArgb(41,139,98) } else { [Drawing.Color]::FromArgb(49,94,171) }
            if (($x + $y) % 11 -eq 0) { $color = [Drawing.Color]::FromArgb(246,193,70) }
            $bitmap.SetPixel($x,$y,$color)
        } }
        $bitmap.SetResolution(96,96); $bitmap.Save($Path,[Drawing.Imaging.ImageFormat]::Png)
    } finally { $bitmap.Dispose() }
    return Get-ExpandedReceipt -Path $Path
}

function New-ExpandedPageSequencePdf {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)][ValidateSet('TARGET','DONOR')][string]$Kind)
    if (Test-Path -LiteralPath $Path) { throw 'Expanded page-sequence fixture path must be fresh.' }
    Assert-NoReparseAncestors -Path $Path
    $palette = if ($Kind -ceq 'TARGET') { @('0.780 0.180 0.160','0.120 0.480 0.720','0.160 0.620 0.320','0.760 0.460 0.080','0.430 0.250 0.720','0.120 0.620 0.650') } else { @('0.940 0.470 0.120','0.560 0.120 0.200','0.220 0.280 0.720','0.200 0.680 0.520','0.680 0.180 0.580','0.420 0.380 0.100') }
    $encoding = [Text.ASCIIEncoding]::new(); $memory = [IO.MemoryStream]::new(); $offsets = [long[]]::new(16)
    $write = { param([string]$Text) $bytes=$encoding.GetBytes($Text);$memory.Write($bytes,0,$bytes.Length) }.GetNewClosure()
    try {
        & $write "%PDF-1.4`n"
        $objects = @{'1'='<< /Type /Catalog /Pages 2 0 R >>';'2'='<< /Type /Pages /Count 6 /Kids [4 0 R 6 0 R 8 0 R 10 0 R 12 0 R 14 0 R] >>';'3'='<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold >>'}
        for ($page=0;$page -lt 6;$page++) {
            $pageObject=4+$page*2;$contentObject=$pageObject+1;$color=$palette[$page]
            $objects[[string]$pageObject]="<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents $contentObject 0 R >>"
            $content="$color rg 0 0 612 792 re f`nq 1 1 1 RG 8 w 42 42 528 708 re S Q`nBT /F1 46 Tf 1 1 1 rg 72 620 Td ($Kind PAGE $($page+1)) Tj ET`nBT /F1 180 Tf 1 1 1 rg 220 300 Td ($($page+1)) Tj ET`n"
            $length=$encoding.GetByteCount($content);$objects[[string]$contentObject]="<< /Length $length >>`nstream`n$content"+'endstream'
        }
        foreach ($number in 1..15) { $offsets[$number]=$memory.Position;& $write "$number 0 obj`n$($objects[[string]$number])`nendobj`n" }
        $xref=$memory.Position;& $write "xref`n0 16`n0000000000 65535 f `n"
        foreach ($number in 1..15) { & $write (($offsets[$number].ToString('D10',[Globalization.CultureInfo]::InvariantCulture))+" 00000 n `n") }
        & $write "trailer`n<< /Size 16 /Root 1 0 R >>`nstartxref`n$xref`n%%EOF`n"
        [IO.File]::WriteAllBytes($Path,$memory.ToArray())
    } finally { $memory.Dispose() }
    return Get-ExpandedReceipt -Path $Path
}

function Assert-ExpandedImageResult {
    param($Result)
    Assert-ExpandedExactProperties $Result @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','sourcePickerControlCategory','createSaveDialogVerified','createdPdf','exportSaveDialogVerified','exportedImage','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') 'Installed image page-tools result'
    Assert-ExpandedExactProperties $Result.createdPdf @('bytes','sha256','pages','widthPoints','heightPoints','workspaceOpened') 'Created PDF result'
    Assert-ExpandedExactProperties $Result.exportedImage @('bytes','sha256','format','width','height','dpi','uiCompleted') 'Exported image result'
    if ([string]$Result.nativeDriverVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or [string]$Result.returnedRuntimeVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or
        [string]$Result.profileBinding -cnotmatch '^(session-capability|owned-webview)-(requested-profile|requested-ebwebview)$' -or
        -not [bool]$Result.createSaveDialogVerified -or -not [bool]$Result.exportSaveDialogVerified -or [string]$Result.sourcePickerControlCategory -notin @('edit-1001','edit-1148') -or
        [uint64]$Result.createdPdf.bytes -eq 0 -or [string]$Result.createdPdf.sha256 -cnotmatch '^[A-F0-9]{64}$' -or [int]$Result.createdPdf.pages -ne 1 -or [double]$Result.createdPdf.widthPoints -ne 612 -or [double]$Result.createdPdf.heightPoints -ne 792 -or -not [bool]$Result.createdPdf.workspaceOpened -or
        [uint64]$Result.exportedImage.bytes -eq 0 -or [string]$Result.exportedImage.sha256 -cnotmatch '^[A-F0-9]{64}$' -or [string]$Result.exportedImage.format -cne 'png' -or
        [int]$Result.exportedImage.width -ne 1275 -or [int]$Result.exportedImage.height -ne 1650 -or [int]$Result.exportedImage.dpi -ne 150 -or -not [bool]$Result.exportedImage.uiCompleted -or
        -not [bool]$Result.sessionDeleted -or -not [bool]$Result.ownedProcessTreeStopped -or [int]$Result.relevantProcessesRemaining -ne 0) { throw 'Installed image page-tools result did not prove the exact bounded output and cleanup contract.' }
}

function Assert-ExpandedOrganizerResult {
    param($Result)
    Assert-ExpandedExactProperties $Result @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','processBoundOpenPickerVerified','openFilenameControlCategory','organizerWorkspaceVerified','cropInteractionVerified','croppedPageWidth','croppedPageHeight','resetCropInteractionVerified','restoredPageWidth','restoredPageHeight','processBoundSplitPickerVerified','splitFilenameControlCategory','splitOutputFileCount','splitOutputBytes','splitOutputPageCounts','sourcePageFingerprintSha256','splitPageFingerprintSha256','splitPageFingerprintOrderVerified','splitFolderCreatedVerified','sourceFixturePreserved','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') 'Installed organizer-tools result'
    $sourceFingerprints = [string[]]@($Result.sourcePageFingerprintSha256); $splitFingerprints = [string[]]@($Result.splitPageFingerprintSha256); $splitCounts = [int[]]@($Result.splitOutputPageCounts)
    if ([string]$Result.nativeDriverVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or [string]$Result.returnedRuntimeVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or
        [string]$Result.profileBinding -cnotmatch '^(session-capability|owned-webview)-(requested-profile|requested-ebwebview)$' -or
        -not [bool]$Result.processBoundOpenPickerVerified -or [string]$Result.openFilenameControlCategory -notin @('edit-1001','edit-1148') -or -not [bool]$Result.organizerWorkspaceVerified -or
        -not [bool]$Result.cropInteractionVerified -or -not [bool]$Result.resetCropInteractionVerified -or -not [bool]$Result.processBoundSplitPickerVerified -or
        [int]$Result.croppedPageWidth -ne 540 -or [int]$Result.croppedPageHeight -ne 720 -or [int]$Result.restoredPageWidth -ne 612 -or [int]$Result.restoredPageHeight -ne 792 -or
        [string]$Result.splitFilenameControlCategory -cne 'edit-1152' -or [int]$Result.splitOutputFileCount -ne 3 -or [uint64]$Result.splitOutputBytes -eq 0 -or [uint64]$Result.splitOutputBytes -gt 768MB -or
        $splitCounts.Count -ne 3 -or ($splitCounts -join ',') -cne '2,2,2' -or $sourceFingerprints.Count -ne 6 -or $splitFingerprints.Count -ne 6 -or
        @($sourceFingerprints | Where-Object { [string]$_ -cnotmatch '^[A-F0-9]{64}$' }).Count -ne 0 -or @($sourceFingerprints | Sort-Object -Unique).Count -ne 6 -or
        -not [bool]$Result.splitPageFingerprintOrderVerified -or (Compare-Object $sourceFingerprints $splitFingerprints -SyncWindow 0 -CaseSensitive) -or
        -not [bool]$Result.splitFolderCreatedVerified -or -not [bool]$Result.sourceFixturePreserved -or -not [bool]$Result.sessionDeleted -or
        -not [bool]$Result.ownedProcessTreeStopped -or [int]$Result.relevantProcessesRemaining -ne 0) { throw 'Installed organizer-tools result did not prove the exact crop, split, source, and cleanup contract.' }
}

function Write-SanitizedExpandedRecord {
    param([string]$Root,[string]$WorkflowRevision,$SourceBinding,$Receipt,$Image,$Organizer,$Structural)
    if (Test-Path -LiteralPath $Root) { throw 'Expanded page-tools output root must be fresh.' }
    [IO.Directory]::CreateDirectory($Root) | Out-Null
    $record = [ordered]@{
        schemaVersion=1
        scope='Hosted signed installed Windows verification of image-PDF creation, page-image export, crop/reset, split folder selection, and Combine/Insert/Replace native Save dialogs using controlled fixtures.'
        workflowSourceRevision=$WorkflowRevision;signedSourceRevision=$script:ExpandedPins.SignedSourceRevision
        sourceBinding=$SourceBinding
        signedArtifact=[ordered]@{application=[ordered]@{bytes=[uint64]$Receipt.packagedApplication.bytes;sha256=[string]$Receipt.packagedApplication.sha256;publisher=$script:ExpandedPins.ExpectedPublisher;trustedTimestamp=$true}}
        imagePageTools=$Image;organizerTools=$Organizer;structuralPageTools=$Structural
    }
    $json = $record | ConvertTo-Json -Depth 14
    if ($json -match ('(?i)([A-Z]:\\|\\Users\\|runneradmin|RUNNER_TEMP|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|' + [string]$script:ExpandedPins.LeakIds + '|raw\.log|\.pdf-workstation)')) { throw 'Sanitized expanded page-tools record contains a host path, credential identifier, artifact identifier, or raw evidence reference.' }
    $path = Join-Path $Root 'expanded-page-tools-verification.json'; [IO.File]::WriteAllText($path,$json,[Text.UTF8Encoding]::new($false))
    $files = @(Get-ChildItem -LiteralPath $Root -File -Force); if ($files.Count -ne 1 -or -not $files[0].FullName.Equals($path,[StringComparison]::OrdinalIgnoreCase)) { throw 'Expanded page-tools output root must contain exactly one sanitized JSON record.' }
    return $path
}

$projectRoot = Split-Path -Parent $PSScriptRoot
Assert-ExpandedHostedSource -ProjectRoot $projectRoot
$signedInputs = Get-SignedCheckInputs -TargetVersion $TargetVersion -SourceRevision $SourceRevision -ArtifactRunId $ArtifactRunId -ArtifactId $ArtifactId -ArtifactName $ArtifactName -ArtifactBytes $ArtifactBytes -ArtifactDigest $ArtifactDigest -InstallerName $InstallerName -InstallerBytes $InstallerBytes -InstallerSha256 $InstallerSha256 -RecordBytes $RecordBytes -RecordSha256 $RecordSha256
Initialize-ExpandedPins -Inputs $signedInputs -BaseResourceTargets (Get-SignedBaseResourceTargets -TargetSourceRoot $TargetSourceRoot -Inputs $signedInputs)
$sourceBinding = Assert-SignedExpandedSourceParity -ProjectRoot $projectRoot
if ([int]$sourceBinding.featureBlobCount -ne 16 -or [int]$sourceBinding.signedSelectorMarkerCount -ne 10) { throw 'Expanded page-tools signed source binding was incomplete.' }
$signedRoot = Resolve-ExpandedRunnerPath -Path $SignedArtifactRoot -MustExist; $work = Resolve-ExpandedRunnerPath -Path $WorkRoot -MustBeFresh
$output = Resolve-ExpandedRunnerPath -Path $OutputRoot -MustBeFresh; $drivers = Resolve-ExpandedRunnerPath -Path $WebDriverRoot -MustExist
$installer = Join-Path $signedRoot $script:ExpandedPins.SignedInstallerName; $recordPath = Join-Path $signedRoot $script:ExpandedPins.SignedRecordName
$signedFiles = @(Get-ChildItem -LiteralPath $signedRoot -Recurse -File -Force)
if ($signedFiles.Count -ne 2) { throw 'Downloaded signed artifact must contain exactly the installer and sanitized record.' }
Assert-FileReceipt -Path $installer -Bytes $script:ExpandedPins.SignedInstallerBytes -Sha256 $script:ExpandedPins.SignedInstallerSha256 -Kind 'Signed expanded page-tools installer'
Assert-FileReceipt -Path $recordPath -Bytes $script:ExpandedPins.SignedRecordBytes -Sha256 $script:ExpandedPins.SignedRecordSha256 -Kind 'Signed expanded page-tools artifact record'
$null = Assert-TrustedWindowsSignature -Path $installer -ExpectedPublisher $script:ExpandedPins.ExpectedPublisher
$recordText = Get-Content -LiteralPath $recordPath -Raw -Encoding UTF8
if ($recordText -match '(?i)([A-Z]:\\|\\Users\\|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log)') { throw 'Downloaded signed artifact record is not sanitized.' }
$receipt = $recordText | ConvertFrom-Json; $receiptSets = Assert-SignedArtifactReceipt -Receipt $receipt
$driverReceipt = Get-Content -LiteralPath (Join-Path $drivers 'webdriver-receipt.json') -Raw -Encoding UTF8 | ConvertFrom-Json; Assert-WebDriverReceipt -Receipt $driverReceipt
$installRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'PDF Workstation')).TrimEnd('\'); $settingsRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\')
$registryPath = 'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDF Workstation'
$fresh = Get-FreshInstallFacts -InstallRoot $installRoot -SettingsRoot $settingsRoot -RegistryPath $registryPath -InstallerPaths @($installer); Assert-FreshInstallFacts -Facts $fresh
[IO.Directory]::CreateDirectory($work) | Out-Null; Assert-NoReparseAncestors -Path $work
Invoke-BoundedSilentInstaller -Path $installer
$installed = Get-InstallFacts -RegistryPath $registryPath -ExpectedInstallRoot $installRoot
Assert-InstallFacts -Facts $installed -ExpectedVersion $script:ExpandedPins.TargetVersion -ExpectedInstallRoot $installRoot -ExpectedApplication $receipt.packagedApplication -ExpectedSignatureStatus 'Valid' -ExpectedPublisher $script:ExpandedPins.ExpectedPublisher -ExpectedTimestamp $true
$resources = Assert-InstalledSignedResources -InstallRoot $installRoot -Receipt $receipt -ReceiptSets $receiptSets
if (Test-Path -LiteralPath $settingsRoot) { throw 'Silent install unexpectedly created the application settings root.' }
[IO.Directory]::CreateDirectory($settingsRoot) | Out-Null

$welcomeReceiptEntry = @($receiptSets.Base | Where-Object { [string]$_.target -ceq 'resources/welcome.pdf' })
if ($welcomeReceiptEntry.Count -ne 1) { throw 'Installed welcome sample receipt is not unique.' }
$welcomeReceipt = [pscustomobject][ordered]@{bytes=[uint64]$welcomeReceiptEntry[0].bytes;sha256=[string]$welcomeReceiptEntry[0].sha256}
$organizerFixture = Join-Path $work 'organizer-source.pdf'; $donorFixture = Join-Path $work 'structural-donor.pdf'
$organizerFixtureReceipt = New-ExpandedPageSequencePdf -Path $organizerFixture -Kind TARGET
$donorFixtureReceipt = New-ExpandedPageSequencePdf -Path $donorFixture -Kind DONOR
$sourcePng = Join-Path $work 'image-source.png'; $sourcePngReceipt = New-ExpandedSourcePng -Path $sourcePng
$splitParent = Join-Path $work 'split-parent'; [IO.Directory]::CreateDirectory($splitParent) | Out-Null

$image = Invoke-InstalledImagePageTools -ApplicationPath $resources.Application.FullName -ApplicationReceipt $receipt.packagedApplication -PdfiumPath $resources.Pdfium.FullName -PdfiumReceipt $receiptSets.Pdfium -WebDriverRoot $drivers -ProfileRoot (Join-Path $work 'image-profile') -SettingsRoot $settingsRoot -SourceImagePath $sourcePng -SourceImageReceipt $sourcePngReceipt -CreatedPdfPath (Join-Path $work 'image-source.pdf') -ExportedPngPath (Join-Path $work 'image-source-page-1.png') -ExpectedPublisher $script:ExpandedPins.ExpectedPublisher
Assert-ExpandedImageResult -Result $image
if (@(Get-ChildItem -LiteralPath $settingsRoot -Force).Count -ne 0) { throw 'Image page-tools escaped its requested profile into application settings.' }
$settingsSentinel = Join-Path $settingsRoot 'upgrade-sentinel.json'; [IO.File]::WriteAllText($settingsSentinel,'{' + '"kind":"expanded-page-tools-sentinel","version":1' + '}',[Text.UTF8Encoding]::new($false)); $settingsSentinelReceipt = Get-ExpandedReceipt -Path $settingsSentinel
$organizer = Invoke-InstalledOrganizerTools -ApplicationPath $resources.Application.FullName -ApplicationReceipt $receipt.packagedApplication -PdfiumPath $resources.Pdfium.FullName -PdfiumReceipt $receiptSets.Pdfium -WebDriverRoot $drivers -ProfileRoot (Join-Path $work 'organizer-profile') -SettingsRoot $settingsRoot -FixturePath $organizerFixture -FixtureReceipt $organizerFixtureReceipt -SplitParentRoot $splitParent -ExpectedPublisher $script:ExpandedPins.ExpectedPublisher
Assert-ExpandedOrganizerResult -Result $organizer
if (@(Get-ChildItem -LiteralPath $settingsRoot -Force).Count -ne 1) { throw 'Organizer tools escaped their requested profile into application settings.' }; Assert-StructuralFileReceipt -Path $settingsSentinel -Receipt $settingsSentinelReceipt -Kind 'Post-organizer settings sentinel'
$structural = Invoke-InstalledStructuralPageTools -ApplicationPath $resources.Application.FullName -ApplicationReceipt $receipt.packagedApplication -PdfiumPath $resources.Pdfium.FullName -PdfiumReceipt $receiptSets.Pdfium -WebDriverRoot $drivers -ProfileRoot (Join-Path $work 'structural-profile') -SettingsRoot $settingsRoot -FirstFixturePath $organizerFixture -FirstFixtureReceipt $organizerFixtureReceipt -SecondFixturePath $donorFixture -SecondFixtureReceipt $donorFixtureReceipt -CombineOutputPath (Join-Path $work 'combined.pdf') -InsertOutputPath (Join-Path $work 'inserted-pages.pdf') -ReplaceOutputPath (Join-Path $work 'replaced-pages.pdf') -ExpectedPublisher $script:ExpandedPins.ExpectedPublisher
Assert-StructuralResult -Result $structural
if (@(Get-ChildItem -LiteralPath $settingsRoot -Force).Count -ne 1) { throw 'Structural page tools escaped their requested profile into application settings.' }; Assert-StructuralFileReceipt -Path $settingsSentinel -Receipt $settingsSentinelReceipt -Kind 'Post-structural settings sentinel'

foreach ($item in @(@($resources.Application.FullName,$receipt.packagedApplication,'Final installed application'),@($resources.Pdfium.FullName,$receiptSets.Pdfium,'Final installed PDFium'),@($resources.Welcome.FullName,$welcomeReceipt,'Final installed welcome'),@($organizerFixture,$organizerFixtureReceipt,'Final organizer and first structural fixture'),@($donorFixture,$donorFixtureReceipt,'Final second structural fixture'),@($sourcePng,$sourcePngReceipt,'Final image fixture'),@($settingsSentinel,$settingsSentinelReceipt,'Final settings sentinel'))) {
    Assert-StructuralFileReceipt -Path ([string]$item[0]) -Receipt $item[1] -Kind ([string]$item[2])
}
$proof = Write-SanitizedExpandedRecord -Root $output -WorkflowRevision ([string]$env:GITHUB_SHA) -SourceBinding $sourceBinding -Receipt $receipt -Image $image -Organizer $organizer -Structural $structural
Write-Output 'Signed installed expanded page-tools verification succeeded.'
Write-Output ('Sanitized record SHA-256: ' + (Get-ExactSha256 -Path $proof))
