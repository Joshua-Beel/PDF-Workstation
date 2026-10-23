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

. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')
. (Join-Path $PSScriptRoot 'installed-persistence.ps1')
. (Join-Path $PSScriptRoot 'signed-check-inputs.ps1')

$script:PersistenceArtifactPins = [ordered]@{
    Repository = 'Joshua-Beel/PDF-Workstation'
    RecordName = 'artifact-verification.json'
    Publisher = 'Joshua Beel'
}

function Initialize-PersistencePins {
    param($Inputs)
    $script:PersistenceArtifactPins['TargetVersion'] = $Inputs.TargetVersion
    $script:PersistenceArtifactPins['SourceRevision'] = $Inputs.SourceRevision
    $script:PersistenceArtifactPins['InstallerName'] = $Inputs.InstallerName
    $script:PersistenceArtifactPins['InstallerBytes'] = $Inputs.InstallerBytes
    $script:PersistenceArtifactPins['InstallerSha256'] = $Inputs.InstallerSha256
    $script:PersistenceArtifactPins['RecordBytes'] = $Inputs.RecordBytes
    $script:PersistenceArtifactPins['RecordSha256'] = $Inputs.RecordSha256
    $script:PersistenceArtifactPins['LeakIds'] = "$($Inputs.ArtifactId)|$($Inputs.ArtifactRunId)"
}

function Assert-PersistenceFileReceipt {
    param([string]$Path,[uint64]$Bytes,[string]$Sha256,[string]$Kind)
    Assert-NoReparseAncestors -Path $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Kind is missing." }
    $item = Get-Item -LiteralPath $Path
    if ([uint64]$item.Length -ne $Bytes -or (Get-ExactSha256 -Path $Path) -cne $Sha256) { throw "$Kind does not match its exact receipt." }
}

function Resolve-PersistenceRunnerPath {
    param([string]$Path,[string]$RunnerTemp,[switch]$MustExist,[switch]$MustBeFresh)
    $root = [IO.Path]::GetFullPath($RunnerTemp).TrimEnd('\')
    $candidate = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if (-not $candidate.StartsWith($root + '\',[StringComparison]::OrdinalIgnoreCase)) { throw 'Persistence verification paths must stay beneath RUNNER_TEMP.' }
    Assert-NoReparseAncestors -Path $candidate
    if ($MustExist -and -not (Test-Path -LiteralPath $candidate)) { throw 'A required persistence input path is missing.' }
    if ($MustBeFresh -and (Test-Path -LiteralPath $candidate)) { throw 'A persistence output path was not fresh.' }
    return $candidate
}

function Assert-PersistenceHostedRunner {
    param([string]$ProjectRoot)
    $head = (& git -C $ProjectRoot rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0) { throw 'Could not resolve the persistence workflow source revision.' }
    & git -C $ProjectRoot diff --quiet --
    $trackedClean = $LASTEXITCODE -eq 0
    & git -C $ProjectRoot diff --cached --quiet --
    $indexClean = $LASTEXITCODE -eq 0
    $ambientNames = @('AZURE_TENANT_ID','AZURE_CLIENT_ID','AZURE_CLIENT_SECRET','AZURE_SIGNING_ENDPOINT','AZURE_SIGNING_ACCOUNT','AZURE_SIGNING_PROFILE','TAURI_SIGNING_PRIVATE_KEY','TAURI_SIGNING_PRIVATE_KEY_PASSWORD','PDF_WORKSTATION_OCR_SETUP_ROOT','PDF_WORKSTATION_EXPECTED_PUBLISHER')
    $ambientSigning = @($ambientNames | Where-Object { -not [string]::IsNullOrEmpty([Environment]::GetEnvironmentVariable($_)) }).Count -ne 0
    if ($env:GITHUB_ACTIONS -cne 'true' -or $env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or $env:RUNNER_OS -cne 'Windows' -or $env:ImageOS -cne 'win22' -or
        $env:GITHUB_EVENT_NAME -cne 'workflow_dispatch' -or $env:GITHUB_REF -cne 'refs/heads/master' -or $env:GITHUB_REPOSITORY -cne $script:PersistenceArtifactPins.Repository -or
        $env:GITHUB_SHA -cnotmatch '^[a-f0-9]{40}$' -or $head -cne $env:GITHUB_SHA -or -not $trackedClean -or -not $indexClean -or
        -not [string]::IsNullOrEmpty($env:GITHUB_TOKEN) -or -not [string]::IsNullOrEmpty($env:GH_TOKEN) -or $ambientSigning) {
        throw 'Persistence verification is restricted to an exact clean GitHub-hosted Windows 2022 manual master run without credentials in the verifier step.'
    }
    return $head
}

function Assert-SignedPersistenceSource {
    param([string]$ProjectRoot,[string]$WorkflowRevision)
    $signedRevision = $script:PersistenceArtifactPins.SourceRevision
    $matchedStorageBlobs = 0
    foreach ($path in @('src/preferences.ts','src/recentFiles.ts')) {
        $workflowBlob = (& git -C $ProjectRoot rev-parse "${WorkflowRevision}:$path").Trim()
        if ($LASTEXITCODE -ne 0 -or $workflowBlob -cnotmatch '^[a-f0-9]{40,64}$') { throw 'Could not resolve a workflow persistence-storage source blob.' }
        $signedBlob = (& git -C $ProjectRoot rev-parse "${signedRevision}:$path").Trim()
        if ($LASTEXITCODE -ne 0 -or $signedBlob -cne $workflowBlob) { throw "Workflow persistence storage source does not match the signed application: $path" }
        $matchedStorageBlobs++
    }
    $workflowAppBlob = (& git -C $ProjectRoot rev-parse "${WorkflowRevision}:src/App.tsx").Trim()
    if ($LASTEXITCODE -ne 0 -or $workflowAppBlob -cnotmatch '^[a-f0-9]{40,64}$') { throw 'Could not resolve the workflow persistence application shell blob.' }
    $signedAppBlob = (& git -C $ProjectRoot rev-parse "${signedRevision}:src/App.tsx").Trim()
    if ($LASTEXITCODE -ne 0 -or $signedAppBlob -cnotmatch '^[a-f0-9]{40,64}$') { throw 'Could not resolve the signed persistence application shell blob.' }
    $signedAppSource = (& git -C $ProjectRoot show "${signedRevision}:src/App.tsx") -join "`n"
    if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($signedAppSource)) { throw 'Could not read the exact signed persistence application shell source.' }
    $selectorMarkers = @(
        'Explore a sample PDF','aria-label="Home"','label="Toggle theme"','label="Select text on page"','label="Pan document"',
        'label="Collapse all tools"','<h2>Pages</h2>','label="Pages"','label="Fit page"','label="Zoom in"','aria-label="Zoom"',
        'Clear file history',"'Unstar' : 'Star'",'<small>PDF document</small>','<span>{file.pages}</span>',"['Recent', 'Starred']",
        'savePreferences({ dark, zoom, fit, hand, toolsOpen, nav })','saveRecentFiles(recentFiles)'
    )
    foreach ($marker in $selectorMarkers) {
        if ($signedAppSource.IndexOf($marker,[StringComparison]::Ordinal) -lt 0) { throw 'An installed persistence UI selector is not bound to the exact signed application source.' }
    }
    return [pscustomobject]@{
        matchedStorageBlobCount = $matchedStorageBlobs
        appShellBlobMatchedSignedSource = $workflowAppBlob -ceq $signedAppBlob
        selectorsBoundToSignedAppSource = $true
        signedSelectorMarkerCount = $selectorMarkers.Count
    }
}

function Invoke-PersistenceSilentInstaller {
    param([string]$Path,[int]$TimeoutMilliseconds = 240000,[scriptblock]$ProcessProvider)
    if ($ProcessProvider) {
        $result = & $ProcessProvider $Path '/S' $TimeoutMilliseconds
        if ($result.TimedOut -isnot [bool] -or $result.ExitCode -isnot [int] -or $result.TimedOut -or $result.ExitCode -ne 0) { throw 'Signed installer process did not complete successfully.' }
        return
    }
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $Path; $startInfo.ArgumentList.Add('/S'); $startInfo.UseShellExecute = $false; $startInfo.CreateNoWindow = $true; $startInfo.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    $process = [Diagnostics.Process]::new(); $process.StartInfo = $startInfo
    try {
        if (-not $process.Start()) { throw 'Signed installer process did not start.' }
        if (-not $process.WaitForExit($TimeoutMilliseconds)) { try { $process.Kill($true) } catch { }; throw 'Signed installer process exceeded its bounded timeout.' }
        if ($process.ExitCode -ne 0) { throw 'Signed installer process returned a nonzero exit code.' }
    } finally { $process.Dispose() }
}

function Assert-PersistenceFreshHost {
    param([string]$InstallRoot,[string]$SettingsRoot,[string]$RegistryPath,[string]$InstallerPath)
    if ((Test-Path -LiteralPath $InstallRoot) -or (Test-Path -LiteralPath $SettingsRoot) -or (Test-Path -LiteralPath $RegistryPath) -or @(Get-LaunchProcessSnapshot).Count -ne 0) { throw 'The hosted runner is not fresh for persistence installation.' }
    foreach ($process in @(Get-Process -ErrorAction SilentlyContinue)) {
        if ($process.ProcessName.Equals('pdf-workstation',[StringComparison]::OrdinalIgnoreCase)) { throw 'The hosted runner already has an application process.' }
        try { if ([string]$process.Path -and ([string]$process.Path).Equals([IO.Path]::GetFullPath($InstallerPath),[StringComparison]::OrdinalIgnoreCase)) { throw 'The hosted runner already has the installer process.' } } catch [Management.Automation.RuntimeException] { throw } catch { }
    }
}

function Get-PersistenceInstalledApplication {
    param([string]$RegistryPath,[string]$InstallRoot,$ApplicationReceipt)
    if (-not (Test-Path -LiteralPath $RegistryPath)) { throw 'The installed application registry entry is missing.' }
    $registry = Get-ItemProperty -LiteralPath $RegistryPath
    $registeredRoot = [IO.Path]::GetFullPath(([string]$registry.InstallLocation).Trim().Trim('"')).TrimEnd('\')
    $uninstall = [IO.Path]::GetFullPath(([string]$registry.UninstallString).Trim().Trim('"')).TrimEnd('\')
    $application = Join-Path $InstallRoot 'pdf-workstation.exe'
    if ([string]$registry.DisplayName -cne 'PDF Workstation' -or [string]$registry.DisplayVersion -cne $script:PersistenceArtifactPins.TargetVersion -or
        -not $registeredRoot.Equals($InstallRoot,[StringComparison]::OrdinalIgnoreCase) -or -not $uninstall.Equals((Join-Path $InstallRoot 'uninstall.exe'),[StringComparison]::OrdinalIgnoreCase)) {
        throw 'Installed application registry facts do not match the exact signed version.'
    }
    Assert-PersistenceFileReceipt -Path $application -Bytes ([uint64]$ApplicationReceipt.bytes) -Sha256 ([string]$ApplicationReceipt.sha256) -Kind 'Installed signed application'
    $signature = Get-WindowsSignatureFacts -Path $application
    if ([string]$signature.Status -cne 'Valid' -or [string]$signature.Publisher -cne $script:PersistenceArtifactPins.Publisher -or -not [bool]$signature.HasTimestamp) { throw 'Installed application signature is not the expected trusted timestamped publisher signature.' }
    return $application
}

function Write-SanitizedPersistenceRecord {
    param([string]$OutputRoot,[string]$WorkflowRevision,$Receipt,$Persistence,$SourceBinding)
    if (Test-Path -LiteralPath $OutputRoot) { throw 'Persistence output root must be fresh.' }
    [IO.Directory]::CreateDirectory($OutputRoot) | Out-Null
    $record = [ordered]@{
        schemaVersion = 1
        scope = 'Ephemeral GitHub-hosted verification of signed installed application reading preferences, recent packaged sample, star, clear-history persistence, and owned-process cleanup across restarts.'
        workflowSourceRevision = $WorkflowRevision
        sourceBinding = [ordered]@{
            claim = 'Exact signed persistence storage modules; application-shell parity is reported separately; UI selectors are validated against the exact signed application source.'
            exactSignedStorageModulesMatched = $true
            matchedStorageBlobCount = [int]$SourceBinding.matchedStorageBlobCount
            appShellBlobMatchedSignedSource = [bool]$SourceBinding.appShellBlobMatchedSignedSource
            selectorsBoundToSignedAppSource = [bool]$SourceBinding.selectorsBoundToSignedAppSource
            signedSelectorMarkerCount = [int]$SourceBinding.signedSelectorMarkerCount
        }
        signedArtifact = [ordered]@{
            sourceRevision = $script:PersistenceArtifactPins.SourceRevision
            installer = [ordered]@{ fileName = $script:PersistenceArtifactPins.InstallerName; bytes = $script:PersistenceArtifactPins.InstallerBytes; sha256 = $script:PersistenceArtifactPins.InstallerSha256 }
            application = [ordered]@{ bytes = [uint64]$Receipt.packagedApplication.bytes; sha256 = [string]$Receipt.packagedApplication.sha256 }
            publisher = $script:PersistenceArtifactPins.Publisher
        }
        persistence = $Persistence
        verification = [ordered]@{
            installedSignedApplication = $true
            applicationProcessStarted = $true
            realPreferencesVerified = $true
            recentFilesVerified = $true
            starsVerified = $true
            clearHistoryVerified = $true
            sameProfileAcrossRestarts = $true
            settingsSentinelPreserved = $true
            pathScopeVerified = $true
            launchProcessCleanupVerified = $true
            nativeWindowVisualVerified = $false
            nativeFilePickerVerified = $false
            userPdfVerified = $false
        }
    }
    $json = $record | ConvertTo-Json -Depth 12
    if ($json -match ('(?i)([A-Z]:\\|\\Users\\|runneradmin|RUNNER_TEMP|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|' + [string]$script:PersistenceArtifactPins.LeakIds + '|raw\.log)')) { throw 'Sanitized persistence record contains a path, credential name, artifact identifier, or raw log reference.' }
    $path = Join-Path $OutputRoot 'persistence-verification.json'
    [IO.File]::WriteAllText($path,$json,[Text.UTF8Encoding]::new($false))
    $files = @(Get-ChildItem -LiteralPath $OutputRoot -File -Force)
    if ($files.Count -ne 1 -or -not $files[0].FullName.Equals($path,[StringComparison]::OrdinalIgnoreCase)) { throw 'Persistence output must contain exactly one sanitized record.' }
    return $path
}

$projectRoot = Split-Path -Parent $PSScriptRoot
Initialize-PersistencePins -Inputs (Get-SignedCheckInputs -TargetVersion $TargetVersion -SourceRevision $SourceRevision -ArtifactRunId $ArtifactRunId -ArtifactId $ArtifactId -ArtifactName $ArtifactName -ArtifactBytes $ArtifactBytes -ArtifactDigest $ArtifactDigest -InstallerName $InstallerName -InstallerBytes $InstallerBytes -InstallerSha256 $InstallerSha256 -RecordBytes $RecordBytes -RecordSha256 $RecordSha256)
$workflowRevision = Assert-PersistenceHostedRunner -ProjectRoot $projectRoot
$sourceBinding = Assert-SignedPersistenceSource -ProjectRoot $projectRoot -WorkflowRevision $workflowRevision
if ([int]$sourceBinding.matchedStorageBlobCount -ne 2 -or [int]$sourceBinding.signedSelectorMarkerCount -ne 18 -or -not [bool]$sourceBinding.selectorsBoundToSignedAppSource) { throw 'The signed persistence source binding is incomplete.' }
$runnerTemp = [string]$env:RUNNER_TEMP
if ([string]::IsNullOrWhiteSpace($runnerTemp)) { throw 'RUNNER_TEMP is unavailable.' }
$signedRoot = Resolve-PersistenceRunnerPath -Path $SignedArtifactRoot -RunnerTemp $runnerTemp -MustExist
$work = Resolve-PersistenceRunnerPath -Path $WorkRoot -RunnerTemp $runnerTemp -MustBeFresh
$output = Resolve-PersistenceRunnerPath -Path $OutputRoot -RunnerTemp $runnerTemp -MustBeFresh
$driverRoot = Resolve-PersistenceRunnerPath -Path $WebDriverRoot -RunnerTemp $runnerTemp -MustExist
$profileRoot = Resolve-PersistenceRunnerPath -Path $WebViewProfileRoot -RunnerTemp $runnerTemp -MustBeFresh

$installer = Join-Path $signedRoot $script:PersistenceArtifactPins.InstallerName
$artifactRecord = Join-Path $signedRoot $script:PersistenceArtifactPins.RecordName
$signedFiles = @(Get-ChildItem -LiteralPath $signedRoot -Recurse -File -Force)
if ($signedFiles.Count -ne 2) { throw 'Downloaded signed artifact must contain exactly the installer and sanitized record.' }
Assert-PersistenceFileReceipt -Path $installer -Bytes $script:PersistenceArtifactPins.InstallerBytes -Sha256 $script:PersistenceArtifactPins.InstallerSha256 -Kind 'Signed installer'
Assert-PersistenceFileReceipt -Path $artifactRecord -Bytes $script:PersistenceArtifactPins.RecordBytes -Sha256 $script:PersistenceArtifactPins.RecordSha256 -Kind 'Signed artifact record'
$null = Assert-TrustedWindowsSignature -Path $installer -ExpectedPublisher $script:PersistenceArtifactPins.Publisher
$recordText = Get-Content -LiteralPath $artifactRecord -Raw -Encoding UTF8
if ($recordText -match '(?i)([A-Z]:\\|\\Users\\|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log)') { throw 'Downloaded signed artifact record is not sanitized.' }
$receipt = $recordText | ConvertFrom-Json
if ([int]$receipt.schemaVersion -ne 1 -or [string]$receipt.sourceRevision -cne $script:PersistenceArtifactPins.SourceRevision -or [string]$receipt.installer.fileName -cne $script:PersistenceArtifactPins.InstallerName -or [uint64]$receipt.installer.bytes -ne $script:PersistenceArtifactPins.InstallerBytes -or [string]$receipt.installer.sha256 -cne $script:PersistenceArtifactPins.InstallerSha256) { throw 'Signed artifact record does not bind the exact signed source and installer.' }

$driverReceiptPath = Join-Path $driverRoot 'webdriver-receipt.json'
$driverReceipt = Get-Content -LiteralPath $driverReceiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
Assert-WebDriverReceipt -Receipt $driverReceipt
Assert-PersistenceFileReceipt -Path (Join-Path $driverRoot 'tauri-driver-install/bin/tauri-driver.exe') -Bytes ([uint64]$driverReceipt.tauriDriver.bytes) -Sha256 ([string]$driverReceipt.tauriDriver.sha256) -Kind 'Pinned tauri-driver'
$edgeDriverPath = Join-Path $driverRoot 'edge-driver/msedgedriver.exe'
Assert-PersistenceFileReceipt -Path $edgeDriverPath -Bytes ([uint64]$driverReceipt.edgeDriver.bytes) -Sha256 ([string]$driverReceipt.edgeDriver.sha256) -Kind 'Trusted EdgeDriver'
$null = Assert-TrustedWindowsSignature -Path $edgeDriverPath -ExpectedPublisher $script:LaunchPins.EdgePublisher

$installRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'PDF Workstation')).TrimEnd('\')
$settingsRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\')
$registryPath = 'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDF Workstation'
Assert-PersistenceFreshHost -InstallRoot $installRoot -SettingsRoot $settingsRoot -RegistryPath $registryPath -InstallerPath $installer
[IO.Directory]::CreateDirectory($work) | Out-Null
Invoke-PersistenceSilentInstaller -Path $installer
if (@(Get-LaunchProcessSnapshot).Count -ne 0) { throw 'Signed silent installation left an application or WebDriver process.' }
$application = Get-PersistenceInstalledApplication -RegistryPath $registryPath -InstallRoot $installRoot -ApplicationReceipt $receipt.packagedApplication

$welcomeReceipt = @($receipt.baseResources | Where-Object { [string]$_.target -ceq 'resources/welcome.pdf' })
if ($welcomeReceipt.Count -ne 1) { throw 'Signed artifact record does not contain one packaged welcome sample.' }
$welcome = Join-Path $installRoot 'resources/welcome.pdf'
Assert-PersistenceFileReceipt -Path $welcome -Bytes ([uint64]$welcomeReceipt[0].bytes) -Sha256 ([string]$welcomeReceipt[0].sha256) -Kind 'Installed welcome sample'

[IO.Directory]::CreateDirectory($settingsRoot) | Out-Null
$sentinel = Join-Path $settingsRoot 'persistence-sentinel.json'
[IO.File]::WriteAllText($sentinel,'{"kind":"ephemeral-persistence-sentinel","version":1}',[Text.UTF8Encoding]::new($false))
$sentinelItem = Get-Item -LiteralPath $sentinel
$sentinelSha = Get-ExactSha256 -Path $sentinel
[IO.Directory]::CreateDirectory($profileRoot) | Out-Null

$persistence = Invoke-InstalledPersistenceVerification -ApplicationPath $application -WebDriverRoot $driverRoot -ProfileRoot $profileRoot -SettingsRoot $settingsRoot -SentinelPath $sentinel -SentinelBytes ([uint64]$sentinelItem.Length) -SentinelSha256 $sentinelSha -ExpectedEdgeDriverVersion ([string]$driverReceipt.edgeDriver.version) -ExpectedRuntimeVersion ([string]$driverReceipt.webView2RuntimeVersion)
Assert-PersistenceSentinel -Path $sentinel -Bytes ([uint64]$sentinelItem.Length) -Sha256 $sentinelSha
$proof = Write-SanitizedPersistenceRecord -OutputRoot $output -WorkflowRevision $workflowRevision -Receipt $receipt -Persistence $persistence -SourceBinding $sourceBinding
Write-Output 'Ephemeral signed installed persistence verification succeeded.'
Write-Output ('Sanitized record SHA-256: ' + (Get-ExactSha256 -Path $proof))
