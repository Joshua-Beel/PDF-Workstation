[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$SignedArtifactRoot,
    [Parameter(Mandatory = $true)][string]$WorkRoot,
    [Parameter(Mandatory = $true)][string]$OutputRoot,
    [Parameter(Mandatory = $true)][string]$WebDriverRoot,
    [Parameter(Mandatory = $true)][string]$WebViewProfileRoot,
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

. (Join-Path $PSScriptRoot 'installed-reading-tools.ps1')
. (Join-Path $PSScriptRoot 'signed-check-inputs.ps1')

$script:ReadingVerificationPins = [ordered]@{
    Repository = 'Joshua-Beel/PDF-Workstation'
    ExpectedPublisher = 'Joshua Beel'
    FeaturePaths = @(
        'src/Viewer.tsx','src/TextLayer.tsx','src/SearchPanel.tsx','src/PasswordDialog.tsx','src/PageText.tsx','src/bridge.ts',
        'src/searchHighlights.ts','src/textHighlightSelection.ts','src-tauri/src/main.rs','src-tauri/src/service.rs','src-tauri/src/text_geometry.rs'
    )
    MaximumFixtureBytes = 2MB
    ProtectedFixtureBytes = [uint64]912
    ProtectedFixtureSha256 = '9BE85022232671CFD796C711E0B8B06847E7020D49AA07FA20DBD6DADE168A37'
}

function Initialize-ReadingPins {
    param($Inputs)
    Add-SignedPinFields -Pins $script:ReadingVerificationPins -Inputs $Inputs
}

function Resolve-ReadingRunnerPath {
    param([Parameter(Mandatory = $true)][string]$Path,[switch]$MustExist,[switch]$MustBeFresh)
    if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) { throw 'RUNNER_TEMP is unavailable.' }
    $runner = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\')
    $candidate = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if (-not $candidate.StartsWith($runner + '\',[StringComparison]::OrdinalIgnoreCase)) { throw 'Reading verification paths must stay beneath RUNNER_TEMP.' }
    Assert-NoReparseAncestors -Path $candidate
    if ($MustExist -and -not (Test-Path -LiteralPath $candidate)) { throw 'A required reading verification input is missing.' }
    if ($MustBeFresh -and (Test-Path -LiteralPath $candidate)) { throw 'A reading verification output path is not fresh.' }
    return $candidate
}

function Assert-ReadingHostedSource {
    param([Parameter(Mandatory = $true)][string]$ProjectRoot)
    $head = (& git -C $ProjectRoot rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or
        $env:RUNNER_OS -cne 'Windows' -or $env:ImageOS -cne 'win22' -or $env:GITHUB_EVENT_NAME -cne 'workflow_dispatch' -or
        $env:GITHUB_REF -cne 'refs/heads/master' -or $env:GITHUB_REPOSITORY -cne $script:ReadingVerificationPins.Repository -or
        $env:GITHUB_SHA -cnotmatch '^[a-f0-9]{40}$' -or $head -cne $env:GITHUB_SHA -or
        -not [string]::IsNullOrEmpty($env:GITHUB_TOKEN) -or -not [string]::IsNullOrEmpty($env:GH_TOKEN)) {
        throw 'Reading verification requires the exact clean credential-free hosted Windows manual master run.'
    }
    & git -C $ProjectRoot diff --quiet --
    if ($LASTEXITCODE -ne 0) { throw 'Tracked files changed before reading verification.' }
    & git -C $ProjectRoot diff --cached --quiet --
    if ($LASTEXITCODE -ne 0) { throw 'The tracked index changed before reading verification.' }
}

function Assert-SignedReadingSourceParity {
    param([Parameter(Mandatory = $true)][string]$ProjectRoot,[string]$CurrentRevision = 'HEAD')
    $signedBlobs = Get-SignedSourceBlobs -ProjectRoot $ProjectRoot -Revision $script:ReadingVerificationPins.SignedSourceRevision -Paths @($script:ReadingVerificationPins.FeaturePaths + 'src/App.tsx')
    $matchedFeatureBlobs = 0
    foreach ($path in $script:ReadingVerificationPins.FeaturePaths) {
        $current = (& git -C $ProjectRoot rev-parse "${CurrentRevision}:$path").Trim()
        if ($LASTEXITCODE -ne 0 -or $current -cne $signedBlobs[$path]) { throw 'A reading feature source blob does not match the exact signed and dispatched source parity contract.' }
        $matchedFeatureBlobs++
    }
    $signedShell = $signedBlobs['src/App.tsx']
    $currentShell = (& git -C $ProjectRoot rev-parse "${CurrentRevision}:src/App.tsx").Trim()
    return [pscustomobject]@{
        currentFeatureParityVerified = [bool]($matchedFeatureBlobs -eq $script:ReadingVerificationPins.FeaturePaths.Count)
        featureBlobCount = [int]$matchedFeatureBlobs
        currentApplicationShellMatchesSigned = [bool]($currentShell -ceq $signedShell)
    }
}

function Get-ReadingFileReceipt {
    param([Parameter(Mandatory = $true)][string]$Path)
    $item = Get-Item -LiteralPath $Path
    return [pscustomobject]@{ bytes = [uint64]$item.Length; sha256 = Get-ExactSha256 -Path $Path }
}

function Get-UniqueInstalledReadingResource {
    param([Parameter(Mandatory = $true)][string]$InstallRoot,[Parameter(Mandatory = $true)][string]$Suffix)
    $shape = $Suffix.Replace('\','/')
    $resourceMatches = @(Get-ChildItem -LiteralPath $InstallRoot -Recurse -File -Force | Where-Object {
        $_.FullName.Replace('\','/').EndsWith('/' + $shape,[StringComparison]::OrdinalIgnoreCase)
    })
    if ($resourceMatches.Count -ne 1 -or ($resourceMatches[0].Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'An installed reading resource was missing, duplicated, or linked.' }
    return $resourceMatches[0]
}

function Join-ReadingByteArrays {
    param([Parameter(Mandatory = $true)][object[]]$Arrays)
    $length = [int](($Arrays | ForEach-Object { $_.Length } | Measure-Object -Sum).Sum)
    $result = [byte[]]::new($length); $offset = 0
    foreach ($array in $Arrays) { [Buffer]::BlockCopy([byte[]]$array,0,$result,$offset,$array.Length); $offset += $array.Length }
    return $result
}

function Get-ReadingMd5 {
    param([Parameter(Mandatory = $true)][byte[]]$Bytes)
    $md5 = [Security.Cryptography.MD5]::Create()
    try { return [byte[]]$md5.ComputeHash($Bytes) } finally { $md5.Dispose() }
}

function Invoke-ReadingRc4 {
    param([Parameter(Mandatory = $true)][byte[]]$Key,[Parameter(Mandatory = $true)][byte[]]$Bytes)
    $state = [byte[]](0..255); $j = 0
    for ($i = 0; $i -lt 256; $i++) {
        $j = ($j + $state[$i] + $Key[$i % $Key.Length]) % 256
        $swap = $state[$i]; $state[$i] = $state[$j]; $state[$j] = $swap
    }
    $result = [byte[]]::new($Bytes.Length); $i = 0; $j = 0
    for ($index = 0; $index -lt $Bytes.Length; $index++) {
        $i = ($i + 1) % 256; $j = ($j + $state[$i]) % 256
        $swap = $state[$i]; $state[$i] = $state[$j]; $state[$j] = $swap
        $result[$index] = $Bytes[$index] -bxor $state[($state[$i] + $state[$j]) % 256]
    }
    return $result
}

function Get-ReadingPasswordPad {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Password)
    $padding = [byte[]](0x28,0xBF,0x4E,0x5E,0x4E,0x75,0x8A,0x41,0x64,0x00,0x4E,0x56,0xFF,0xFA,0x01,0x08,0x2E,0x2E,0x00,0xB6,0xD0,0x68,0x3E,0x80,0x2F,0x0C,0xA9,0xFE,0x64,0x53,0x69,0x7A)
    $passwordBytes = [Text.Encoding]::ASCII.GetBytes($Password)
    $result = [byte[]]::new(32)
    $take = [Math]::Min(32,$passwordBytes.Length)
    [Buffer]::BlockCopy($passwordBytes,0,$result,0,$take)
    if ($take -lt 32) { [Buffer]::BlockCopy($padding,0,$result,$take,32-$take) }
    return $result
}

function New-ProtectedReadingFixture {
    param([Parameter(Mandatory = $true)][string]$Destination)
    $ascii = [Text.Encoding]::ASCII
    $padding = Get-ReadingPasswordPad -Password ''
    $ownerDigest = Get-ReadingMd5 -Bytes (Get-ReadingPasswordPad -Password 'owner password')
    $ownerKey = [byte[]]$ownerDigest[0..4]
    $owner = Invoke-ReadingRc4 -Key $ownerKey -Bytes (Get-ReadingPasswordPad -Password 'test password')
    $fileId = Get-ReadingMd5 -Bytes $ascii.GetBytes('installed-reading-tools-fixture')
    $permissions = [byte[]](0xFC,0xFF,0xFF,0xFF)
    $fileDigest = Get-ReadingMd5 -Bytes (Join-ReadingByteArrays -Arrays @((Get-ReadingPasswordPad -Password 'test password'),$owner,$permissions,$fileId))
    $fileKey = [byte[]]$fileDigest[0..4]
    $user = Invoke-ReadingRc4 -Key $fileKey -Bytes $padding
    $objectDigest = Get-ReadingMd5 -Bytes (Join-ReadingByteArrays -Arrays @($fileKey,[byte[]](5,0,0,0,0)))
    $objectKey = [byte[]]$objectDigest[0..9]
    $content = $ascii.GetBytes("BT /F1 18 Tf 72 700 Td (Protected reading fixture) Tj ET`n")
    $encryptedContent = Invoke-ReadingRc4 -Key $objectKey -Bytes $content
    $hex = { param([byte[]]$value) ([BitConverter]::ToString($value)).Replace('-','') }
    $objects = [ordered]@{
        '1' = '<< /Type /Catalog /Pages 2 0 R >>'
        '2' = '<< /Type /Pages /Kids [4 0 R] /Count 1 >>'
        '3' = '<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>'
        '4' = '<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents 5 0 R >>'
        '6' = "<< /Filter /Standard /V 1 /R 2 /O <$( & $hex $owner )> /U <$( & $hex $user )> /P -4 >>"
    }
    $memory = [IO.MemoryStream]::new()
    $writeAscii = { param([string]$value) $bytes=$ascii.GetBytes($value);$memory.Write($bytes,0,$bytes.Length) }
    try {
        & $writeAscii "%PDF-1.4`n"; $memory.Write([byte[]](0x25,0xE2,0xE3,0xCF,0xD3,0x0A),0,6)
        $offsets = [ordered]@{}
        foreach ($number in 1..6) {
            $offsets[[string]$number] = [int64]$memory.Position
            & $writeAscii "$number 0 obj`n"
            if ($number -eq 5) {
                & $writeAscii "<< /Length $($encryptedContent.Length) >>`nstream`n"
                $memory.Write($encryptedContent,0,$encryptedContent.Length)
                & $writeAscii "`nendstream`n"
            } else { & $writeAscii ($objects[[string]$number] + "`n") }
            & $writeAscii "endobj`n"
        }
        $xref = [int64]$memory.Position
        & $writeAscii "xref`n0 7`n0000000000 65535 f `n"
        foreach ($number in 1..6) { & $writeAscii ($offsets[[string]$number].ToString('0000000000') + " 00000 n `n") }
        $id = & $hex $fileId
        & $writeAscii "trailer`n<< /Size 7 /Root 1 0 R /Encrypt 6 0 R /ID [<$id><$id>] >>`nstartxref`n$xref`n%%EOF`n"
        [IO.File]::WriteAllBytes($Destination,$memory.ToArray())
    } finally { $memory.Dispose() }
    Assert-ReadingFileReceipt -Path $Destination -Bytes $script:ReadingVerificationPins.ProtectedFixtureBytes -Sha256 $script:ReadingVerificationPins.ProtectedFixtureSha256 -Kind 'Deterministic protected reading fixture'
}

function Write-SanitizedReadingRecord {
    param([string]$OutputRoot,[string]$WorkflowSourceRevision,$SourceParity,$PlainReceipt,$ProtectedReceipt,$UiResult)
    [IO.Directory]::CreateDirectory($OutputRoot) | Out-Null
    $record = [ordered]@{
        schemaVersion = 1
        scope = 'Manual installed signed-app reading-tools verification on deterministic bounded fixtures; no general PDF compatibility claim.'
        mode = 'signed-installed-reading-tools-ui'
        workflowSourceRevision = $WorkflowSourceRevision
        signedSourceRevision = $script:ReadingVerificationPins.SignedSourceRevision
        artifactIdentifierIncluded = $false
        sourceParity = $SourceParity
        fixtures = [ordered]@{
            plain = [ordered]@{ bytes=[uint64]$PlainReceipt.bytes;sha256=[string]$PlainReceipt.sha256 }
            protected = [ordered]@{ bytes=[uint64]$ProtectedReceipt.bytes;sha256=[string]$ProtectedReceipt.sha256;deterministicFixedFixture=$true }
        }
        selection = [ordered]@{
            processBoundPickerVerified=[bool]$UiResult.processBoundPickerVerified
            nativeFilenameControlCategory=[string]$UiResult.nativeFilenameControlCategory
            postOpen=[ordered]@{
                documentTabCount=[int]$UiResult.postOpenDocumentTabCount
                activeTabCount=[int]$UiResult.postOpenActiveTabCount
                activeFilenameMatched=[bool]$UiResult.postOpenActiveFilenameMatched
                pageCount=[int]$UiResult.postOpenPageCount
                renderedPageCount=[int]$UiResult.postOpenRenderedPageCount
                imageCount=[int]$UiResult.postOpenImageCount
                loadedImageCount=[int]$UiResult.postOpenLoadedImageCount
                textLayerCount=[int]$UiResult.postOpenTextLayerCount
                pageOneGlyphCount=[int]$UiResult.postOpenPageOneGlyphCount
                pageTextStatusCount=[int]$UiResult.postOpenPageTextStatusCount
            }
            method = 'programmatic-dom-range-plus-real-windows-clipboard'
            textSelectionModeActivated=[bool]$UiResult.textSelectionModeActivated
            programmaticDomSelectionVerified=[bool]$UiResult.programmaticDomSelectionVerified
            geometryInsidePage=[bool]$UiResult.selectionGeometryInsidePage
            clientRectCount=[int]$UiResult.selectionClientRectCount
            utf8Bytes=[int]$UiResult.selectedTextUtf8Bytes
            sha256=[string]$UiResult.selectedTextSha256
            windowsClipboardVerified=[bool]$UiResult.windowsClipboardVerified
            clipboardUtf8Bytes=[int]$UiResult.clipboardUtf8Bytes
            clipboardSha256=[string]$UiResult.clipboardSha256
        }
        search = [ordered]@{
            resultCount=[int]$UiResult.searchResultCount
            navigationVerified=[bool]$UiResult.searchNavigationVerified
            highlightCount=[int]$UiResult.searchHighlightCount
            highlightInsidePage=[bool]$UiResult.searchHighlightInsidePage
            highlightMatchesGlyphGeometry=[bool]$UiResult.searchHighlightMatchesGlyphGeometry
        }
        password = [ordered]@{
            promptVerified=[bool]$UiResult.passwordPromptVerified
            wrongRetryVerified=[bool]$UiResult.wrongPasswordRetryVerified
            inputClearedAfterWrong=[bool]$UiResult.passwordInputClearedAfterWrong
            correctOpenVerified=[bool]$UiResult.correctPasswordOpenVerified
            uiReentryRequiredAfterCloseAndReopen=[bool]$UiResult.passwordReentryRequiredAfterCloseAndReopen
            cancelVerified=[bool]$UiResult.passwordCancelVerified
            promptVerifiedAfterCancel=[bool]$UiResult.postCancelPromptVerified
            passwordStoragePersistenceInspected=$false
        }
        environment = [ordered]@{ profileBinding=[string]$UiResult.profileBinding;nativeDriverVersion=[string]$UiResult.nativeDriverVersion;webViewRuntimeVersion=[string]$UiResult.returnedRuntimeVersion }
        cleanup = [ordered]@{ clipboardClearedAndVerified=$true;sessionDeleted=[bool]$UiResult.sessionDeleted;ownedProcessTreeStopped=[bool]$UiResult.ownedProcessTreeStopped;relevantProcessesRemaining=[int]$UiResult.relevantProcessesRemaining }
    }
    $json = $record | ConvertTo-Json -Depth 8
    if ($json -match ('(?i)([A-Z]:\\|\\Users\\|test password|wrong password|A place for your PDFs|Sample document|GITHUB_TOKEN|GH_TOKEN|' + [string]$script:ReadingVerificationPins.LeakIds + '|raw\.log)')) { throw 'The reading-tools record contains a raw path, text fixture value, password, token, artifact identifier, or raw log reference.' }
    $path = Join-Path $OutputRoot 'reading-tools-verification.json'
    [IO.File]::WriteAllText($path,$json,[Text.UTF8Encoding]::new($false))
    return $path
}

$projectRoot = Split-Path -Parent $PSScriptRoot
Assert-ReadingHostedSource -ProjectRoot $projectRoot
Initialize-ReadingPins -Inputs (Get-SignedCheckInputs -TargetVersion $TargetVersion -SourceRevision $SourceRevision -ArtifactRunId $ArtifactRunId -ArtifactId $ArtifactId -ArtifactName $ArtifactName -ArtifactBytes $ArtifactBytes -ArtifactDigest $ArtifactDigest -InstallerName $InstallerName -InstallerBytes $InstallerBytes -InstallerSha256 $InstallerSha256 -RecordBytes $RecordBytes -RecordSha256 $RecordSha256)
$sourceParity = Assert-SignedReadingSourceParity -ProjectRoot $projectRoot
$signedRoot = Resolve-ReadingRunnerPath -Path $SignedArtifactRoot -MustExist
$work = Resolve-ReadingRunnerPath -Path $WorkRoot -MustBeFresh
$output = Resolve-ReadingRunnerPath -Path $OutputRoot -MustBeFresh
$driverRoot = Resolve-ReadingRunnerPath -Path $WebDriverRoot -MustExist
$profileRoot = Resolve-ReadingRunnerPath -Path $WebViewProfileRoot -MustBeFresh
$settingsRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\')

$recordPath = Join-Path $signedRoot $script:ReadingVerificationPins.SignedRecordName
Assert-ReadingFileReceipt -Path $recordPath -Bytes $script:ReadingVerificationPins.SignedRecordBytes -Sha256 $script:ReadingVerificationPins.SignedRecordSha256 -Kind 'Signed reading artifact record'
$artifactRecord = Get-Content -LiteralPath $recordPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ([string]$artifactRecord.sourceRevision -cne $script:ReadingVerificationPins.SignedSourceRevision -or
    [string]$artifactRecord.packagedApplication.sha256 -cnotmatch '^[A-F0-9]{64}$' -or [uint64]$artifactRecord.packagedApplication.bytes -eq 0) {
    throw 'The signed artifact record is outside the exact reading-tools source contract.'
}
$welcomeRecords = @($artifactRecord.baseResources | Where-Object { [string]$_.target -ceq 'resources/welcome.pdf' })
if ($welcomeRecords.Count -ne 1 -or [string]$welcomeRecords[0].sha256 -cnotmatch '^[A-F0-9]{64}$' -or [uint64]$welcomeRecords[0].bytes -eq 0) { throw 'The signed artifact record has no unique welcome fixture receipt.' }

$installRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'PDF Workstation')).TrimEnd('\')
$application = Get-UniqueInstalledReadingResource -InstallRoot $installRoot -Suffix 'pdf-workstation.exe'
$welcome = Get-UniqueInstalledReadingResource -InstallRoot $installRoot -Suffix 'resources/welcome.pdf'
Assert-ReadingFileReceipt -Path $application.FullName -Bytes ([uint64]$artifactRecord.packagedApplication.bytes) -Sha256 ([string]$artifactRecord.packagedApplication.sha256) -Kind 'Installed signed reading application'
Assert-ReadingFileReceipt -Path $welcome.FullName -Bytes ([uint64]$welcomeRecords[0].bytes) -Sha256 ([string]$welcomeRecords[0].sha256) -Kind 'Installed signed reading fixture'

[IO.Directory]::CreateDirectory($work) | Out-Null
$plainFixture = Join-Path $work 'reading-source.pdf'
$protectedFixture = Join-Path $work 'protected-reading-source.pdf'
[IO.File]::Copy($welcome.FullName,$plainFixture,$false)
$plainReceipt = Get-ReadingFileReceipt -Path $plainFixture
New-ProtectedReadingFixture -Destination $protectedFixture
$protectedReceipt = Get-ReadingFileReceipt -Path $protectedFixture

$uiResult = Invoke-InstalledReadingTools -ApplicationPath $application.FullName -ApplicationReceipt $artifactRecord.packagedApplication -WebDriverRoot $driverRoot -ProfileRoot $profileRoot -SettingsRoot $settingsRoot -PlainFixture $plainFixture -ProtectedFixture $protectedFixture -PlainReceipt $plainReceipt -ProtectedReceipt $protectedReceipt -ExpectedPublisher $script:ReadingVerificationPins.ExpectedPublisher
if (@(Get-LaunchProcessSnapshot).Count -ne 0) { throw 'Reading-tools verification left a relevant process running.' }
Assert-ReadingFileReceipt -Path $plainFixture -Bytes ([uint64]$plainReceipt.bytes) -Sha256 ([string]$plainReceipt.sha256) -Kind 'Final plain reading fixture'
Assert-ReadingFileReceipt -Path $protectedFixture -Bytes ([uint64]$protectedReceipt.bytes) -Sha256 ([string]$protectedReceipt.sha256) -Kind 'Final protected reading fixture'
$record = Write-SanitizedReadingRecord -OutputRoot $output -WorkflowSourceRevision $env:GITHUB_SHA -SourceParity $sourceParity -PlainReceipt $plainReceipt -ProtectedReceipt $protectedReceipt -UiResult $uiResult
Write-Output 'Installed signed reading-tools verification succeeded.'
Write-Output ('Sanitized record SHA-256: ' + (Get-ExactSha256 -Path $record))
