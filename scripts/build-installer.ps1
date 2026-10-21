param(
    [switch]$AzureSigning,
    [switch]$UnsignedLocal,
    [switch]$ArtifactOnly,
    [string]$OcrSetupRoot,
    [string]$OutputRoot,
    [string]$ExpectedPublisher,
    [string]$SourceRevision
)
$ErrorActionPreference = 'Stop'

function New-AzureSigningConfig {
    param([string]$Endpoint, [string]$Account, [string]$Profile)
    foreach ($entry in @(@('AZURE_SIGNING_ENDPOINT', $Endpoint), @('AZURE_SIGNING_ACCOUNT', $Account), @('AZURE_SIGNING_PROFILE', $Profile))) {
        if ([string]::IsNullOrWhiteSpace($entry[1])) { throw "Missing required signing setting: $($entry[0])" }
    }
    if ($Endpoint -cnotmatch '^https://[a-z0-9-]+\.codesigning\.azure\.net/?$') { throw 'Invalid AZURE_SIGNING_ENDPOINT: use an HTTPS Azure code-signing service endpoint without credentials, query, or custom path.' }
    foreach ($entry in @(@('AZURE_SIGNING_ACCOUNT', $Account), @('AZURE_SIGNING_PROFILE', $Profile))) {
        if ($entry[1] -cnotmatch '^[A-Za-z0-9][A-Za-z0-9-]{0,127}$') { throw "Invalid signing identifier: $($entry[0])" }
    }
    return @{ bundle = @{ windows = @{ signCommand = @{ cmd = 'artifact-signing-cli'; args = @('-e', $Endpoint, '-a', $Account, '-c', $Profile, '-d', 'PDF Workstation', '%1') } } } }
}

function New-AzureOcrSigningConfig {
    param([Parameter(Mandatory = $true)][string]$HelperPath)
    $resolvedHelper = [IO.Path]::GetFullPath($HelperPath)
    if (-not (Test-Path -LiteralPath $resolvedHelper -PathType Leaf)) { throw 'OCR-aware Tauri signing helper is missing.' }
    return @{ bundle = @{ windows = @{ signCommand = @{ cmd = 'powershell.exe'; args = @('-NoProfile', '-NonInteractive', '-ExecutionPolicy', 'Bypass', '-File', $resolvedHelper, '-TauriSign', '-Path', '%1') } } } }
}

function Write-JsonUtf8 {
    param([Parameter(Mandatory = $true)][string]$Path, [Parameter(Mandatory = $true)]$Value)
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path)) | Out-Null
    [IO.File]::WriteAllText($Path, ($Value | ConvertTo-Json -Depth 20), [Text.UTF8Encoding]::new($false))
}

function Assert-NotPublisherSigned {
    param([Parameter(Mandatory = $true)][string]$Path)
    if ((Get-AuthenticodeSignature -LiteralPath $Path).Status -ne 'NotSigned') { throw 'Unsigned-local artifact is not exactly Authenticode NotSigned.' }
}

function Close-ReadLocks {
    param($Locks)
    if ($Locks) { foreach ($lock in $Locks) { $lock.Dispose() } }
}

function Invoke-TauriInstallerBuild {
    param(
        [string]$ConfigPath,
        [switch]$DeleteConfig,
        [Parameter(Mandatory = $true)][scriptblock]$BuildCommand
    )
    try {
        return [int](& $BuildCommand $ConfigPath)
    } finally {
        if ($DeleteConfig -and $ConfigPath -and (Test-Path -LiteralPath $ConfigPath -PathType Leaf)) {
            Remove-Item -LiteralPath $ConfigPath
        }
    }
}

function Assert-InstallerSourceRevision {
    param(
        [Parameter(Mandatory = $true)][string]$ProjectRoot,
        [Parameter(Mandatory = $true)][string]$SourceRevision
    )
    if ($SourceRevision -cnotmatch '^[a-f0-9]{40}$') { throw 'Installer proof requires SourceRevision as the exact lowercase 40-character git HEAD.' }
    $repository = [IO.Path]::GetFullPath($ProjectRoot)
    $headRevision = (& git -C $repository rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $headRevision -cne $SourceRevision) { throw 'Installer proof SourceRevision does not match the current git HEAD.' }
    & git -C $repository diff --quiet --
    if ($LASTEXITCODE -ne 0) { throw 'Installer proof requires a clean tracked working tree.' }
    & git -C $repository diff --cached --quiet --
    if ($LASTEXITCODE -ne 0) { throw 'Installer proof requires a clean tracked index.' }
}

$projectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $projectRoot
. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')

# OCR is packaged only through -OcrSetupRoot. An ambient variable must never
# change what an installer or release build packages.
Remove-Item Env:PDF_WORKSTATION_OCR_SETUP_ROOT -ErrorAction SilentlyContinue

if ($AzureSigning -and $UnsignedLocal) { throw 'AzureSigning and UnsignedLocal cannot be combined.' }
if ($ArtifactOnly -and (-not $AzureSigning -or [string]::IsNullOrWhiteSpace($OcrSetupRoot) -or $UnsignedLocal)) { throw 'ArtifactOnly requires AzureSigning with OcrSetupRoot and cannot be combined with UnsignedLocal.' }
$ReleaseOcr = [bool]($AzureSigning -and $OcrSetupRoot -and -not $ArtifactOnly)
$SignedOcr = [bool]($ArtifactOnly -or $ReleaseOcr)
if ($ExpectedPublisher -and -not $SignedOcr) { throw 'ExpectedPublisher is supported only for Azure-signed OCR.' }
if ($SourceRevision -and -not ($SignedOcr -or $UnsignedLocal)) { throw 'SourceRevision is supported only for explicit installer proof modes.' }
if ($UnsignedLocal -and [string]::IsNullOrWhiteSpace($SourceRevision)) { throw 'UnsignedLocal requires SourceRevision as the exact lowercase 40-character git HEAD.' }
if ($ReleaseOcr -and [string]::IsNullOrWhiteSpace($SourceRevision)) { throw 'Azure-signed release OCR requires SourceRevision as the exact lowercase 40-character git HEAD.' }
if ($SignedOcr -and ([string]::IsNullOrWhiteSpace($ExpectedPublisher) -or $ExpectedPublisher -cnotmatch "^[A-Za-z0-9][A-Za-z0-9 .,&'()/-]{0,127}$")) { throw 'Azure-signed OCR requires a valid explicit ExpectedPublisher.' }
if ($SignedOcr -or $UnsignedLocal) { Assert-InstallerSourceRevision -ProjectRoot $projectRoot -SourceRevision $SourceRevision }
if ($OcrSetupRoot -and -not ($AzureSigning -or $UnsignedLocal)) { throw 'OcrSetupRoot requires AzureSigning or UnsignedLocal.' }
if ($OcrSetupRoot -and [string]::IsNullOrWhiteSpace($OutputRoot)) { throw 'OcrSetupRoot requires a fresh OutputRoot beneath target.' }
if ($UnsignedLocal -and [string]::IsNullOrWhiteSpace($OutputRoot)) { throw 'UnsignedLocal requires a fresh OutputRoot beneath target.' }
if ($OutputRoot -and -not ($UnsignedLocal -or $OcrSetupRoot)) { throw 'OutputRoot is supported only for an explicit unsigned-local or OCR build.' }

$resolvedOutput = $null
$cargoTarget = $null
if ($OutputRoot) {
    if ([Environment]::GetEnvironmentVariable('CARGO_TARGET_DIR')) { throw 'Remove the ambient CARGO_TARGET_DIR before a bounded installer proof.' }
    $resolvedOutput = Resolve-FreshInstallerOutput -ProjectRoot $projectRoot -OutputRoot $OutputRoot
    $cargoTarget = Join-Path $resolvedOutput 'cargo-target'
    $env:CARGO_TARGET_DIR = $cargoTarget
}

if ($UnsignedLocal -or $ArtifactOnly) {
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue
    Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue
}

$env:PATH = (Join-Path $env:USERPROFILE '.cargo\bin') + ';' + $env:PATH
if ($AzureSigning) {
    foreach ($name in @('AZURE_SIGNING_ENDPOINT', 'AZURE_SIGNING_ACCOUNT', 'AZURE_SIGNING_PROFILE')) {
        $setting = [Environment]::GetEnvironmentVariable($name)
        if ($env:GITHUB_ACTIONS -eq 'true' -and $setting) {
            $mask = $setting.Replace('%', '%25').Replace("`r", '%0D').Replace("`n", '%0A')
            Write-Output "::add-mask::$mask"
        }
    }
    $azureConfig = New-AzureSigningConfig -Endpoint $env:AZURE_SIGNING_ENDPOINT -Account $env:AZURE_SIGNING_ACCOUNT -Profile $env:AZURE_SIGNING_PROFILE
    foreach ($name in @('AZURE_TENANT_ID', 'AZURE_CLIENT_ID', 'AZURE_CLIENT_SECRET')) {
        if (-not [Environment]::GetEnvironmentVariable($name)) { throw "Missing required signing credential: $name" }
    }
    if (-not (Get-Command artifact-signing-cli -ErrorAction SilentlyContinue)) { throw 'Install artifact-signing-cli 0.11.0 before an Azure-signed build.' }
}

# UnsignedLocal and artifact-only OCR intentionally neither read nor require
# either updater key variable. Release builds keep the updater-key behavior.
if (-not $UnsignedLocal -and -not $ArtifactOnly -and -not $env:TAURI_SIGNING_PRIVATE_KEY) {
    $signingFile = Join-Path $env:LOCALAPPDATA 'PDFWorkstation\signing\updater.key'
    if (-not (Test-Path -LiteralPath $signingFile)) { throw 'Set TAURI_SIGNING_PRIVATE_KEY to the release signing key path.' }
    $env:TAURI_SIGNING_PRIVATE_KEY = $signingFile
}

$sourceLocks = $null
$derivedLocks = $null
$baseLocks = $null
$ocrPlan = $null
$ocrEntries = $null
$originalEngineReceipt = $null
$originalEngineStatus = $null
try {
    if ($OcrSetupRoot) {
        $originalPlan = Get-VerifiedOcrSetup -ProjectRoot $projectRoot -SetupRoot $OcrSetupRoot
        $originalEngineStatus = (Get-AuthenticodeSignature -LiteralPath ($originalPlan.Files | Where-Object Role -CEQ 'engine').Source).Status.ToString()
        if ($originalEngineStatus -cne 'NotSigned') { throw 'OCR packaging requires an original engine whose Authenticode status is exactly NotSigned.' }
        $sourceLocks = Open-OcrReadLocks -Plan $originalPlan
        $originalPlan = Get-VerifiedOcrSetup -ProjectRoot $projectRoot -SetupRoot $OcrSetupRoot
        $originalEngineReceipt = [pscustomobject]@{ bytes = $originalPlan.Engine.Bytes; sha256 = $originalPlan.Engine.Sha256 }
        if ($SignedOcr) {
            $engineSigner = { param($candidate) Invoke-AzureArtifactSigningCli -Path $candidate }
            $ocrPlan = New-SignedOcrSetup `
                -ProjectRoot $projectRoot `
                -OriginalPlan $originalPlan `
                -OutputRoot $resolvedOutput `
                -Signer $engineSigner `
                -ExpectedPublisher $ExpectedPublisher
            $derivedIdentity = $ocrPlan.Identity
            $derivedManifestSha256 = $ocrPlan.ManifestSha256
            $derivedLocks = Open-OcrReadLocks -Plan $ocrPlan
            $ocrPlan = Get-VerifiedOcrSetup -ProjectRoot $projectRoot -SetupRoot $ocrPlan.Root
            if ($ocrPlan.Identity -cne $derivedIdentity -or $ocrPlan.ManifestSha256 -cne $derivedManifestSha256) {
                throw 'Signed OCR plan changed while acquiring read locks.'
            }
            $env:PDF_WORKSTATION_SIGNED_OCR_ENGINE_PATH = ($ocrPlan.Files | Where-Object Role -CEQ 'engine').Source
            $env:PDF_WORKSTATION_SIGNED_OCR_ENGINE_BYTES = [string]$ocrPlan.Engine.Bytes
            $env:PDF_WORKSTATION_SIGNED_OCR_ENGINE_SHA256 = [string]$ocrPlan.Engine.Sha256
            $env:PDF_WORKSTATION_EXPECTED_PUBLISHER = $ExpectedPublisher
        } else {
            $ocrPlan = $originalPlan
        }
        $ocrEntries = Get-OcrBundleEntries -Plan $ocrPlan
        $env:PDF_WORKSTATION_OCR_SETUP_ROOT = $ocrPlan.Root
    }

    & "$PSScriptRoot/setup-pdfium.ps1" -SkipIfCurrent
    cargo fetch --locked --target x86_64-pc-windows-msvc --manifest-path src-tauri/Cargo.toml
    if ($LASTEXITCODE -ne 0) { throw 'Could not fetch locked dependencies for notice verification.' }
    node scripts/mpl-source-archives.mjs --check
    if ($LASTEXITCODE -ne 0) { throw 'Exact MPL source archives are missing or stale. Regenerate and review them before building an installer.' }
    node scripts/dependency-notices.mjs --check
    if ($LASTEXITCODE -ne 0) { throw 'Dependency notices are missing or stale. Regenerate and review them before building an installer.' }

    $baseResources = if ($resolvedOutput -or $ocrPlan) { Get-BaseBundleResourceMap -ProjectRoot $projectRoot } else { $null }
    if ($baseResources) {
        $unlockedBaseResources = $baseResources
        $baseLocks = Open-PathReadLocks -Paths @($baseResources.Entries | ForEach-Object Source)
        $baseResources = Get-BaseBundleResourceMap -ProjectRoot $projectRoot
        Assert-BaseResourceMapStable -Before $unlockedBaseResources -After $baseResources
        if ($SignedOcr) {
            $baseResources = New-SignableBaseResourcePlan -Base $baseResources -OutputRoot $resolvedOutput
        }
    }
    $resourceMap = $null
    if ($ocrPlan) {
        $resourceMap = Add-OcrBundleResources -Base $baseResources -Entries $ocrEntries -Identity $ocrPlan.Identity
        if ($resourceMap.Count -ne ($baseResources.Count + 5)) { throw 'OCR resource map did not preserve a base-resource bijection.' }
    }
    $signCommand = if ($SignedOcr) {
        (New-AzureOcrSigningConfig -HelperPath (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')).bundle.windows.signCommand
    } elseif ($azureConfig) { $azureConfig.bundle.windows.signCommand } else { $null }
    $override = if ($UnsignedLocal -or $ArtifactOnly -or $signCommand -or $resourceMap) {
        New-InstallerOverrideConfig -SignCommand $signCommand -UnsignedLocal:$UnsignedLocal -ArtifactOnly:$ArtifactOnly -ResourceMap $resourceMap
    } else { $null }

    $configPath = $null
    $deleteConfig = $false
    if ($override) {
        if ($SignedOcr) {
            $configDirectory = Join-Path $projectRoot 'src-tauri/target/signing-config'
            New-Item -ItemType Directory -Path $configDirectory -Force | Out-Null
            $configPath = Join-Path $configDirectory ([Guid]::NewGuid().ToString('N') + '.json')
            $deleteConfig = $true
        } elseif ($resolvedOutput) {
            $configPath = Join-Path $resolvedOutput 'tauri-installer-override.json'
        } else {
            $configDirectory = Join-Path $projectRoot 'src-tauri/target/signing-config'
            New-Item -ItemType Directory -Path $configDirectory -Force | Out-Null
            $configPath = Join-Path $configDirectory ([Guid]::NewGuid().ToString('N') + '.json')
            $deleteConfig = $true
        }
        Write-JsonUtf8 -Path $configPath -Value $override
    }

    $buildCommand = {
        param($path)
        if ($path) { npm.cmd run tauri -- build --ci --bundles nsis --config $path | Out-Host }
        else { npm.cmd run tauri -- build --ci --bundles nsis | Out-Host }
        return [int]$LASTEXITCODE
    }
    $buildExitCode = Invoke-TauriInstallerBuild -ConfigPath $configPath -DeleteConfig:$deleteConfig -BuildCommand $buildCommand
    if ($buildExitCode -ne 0) { throw 'Installer build failed.' }
    if ($SignedOcr) {
        Assert-SignedBasePeSources -Plan $baseResources -ExpectedPublisher $ExpectedPublisher
    }

    $targetRoot = if ($cargoTarget) { $cargoTarget } else { Join-Path $projectRoot 'src-tauri/target' }
    $installerDirectory = Join-Path $targetRoot 'release/bundle/nsis'
    if ($resolvedOutput) {
        $installers = @(Get-ChildItem -LiteralPath $installerDirectory -File | Where-Object Name -Like '*_x64-setup.exe')
        if ($installers.Count -ne 1) { throw 'Expected exactly one x64 NSIS installer in the fresh proof root.' }
        $installer = $installers[0].FullName
    } else {
        $version = (Get-Content -LiteralPath 'src-tauri/tauri.conf.json' -Raw -Encoding UTF8 | ConvertFrom-Json).version
        $installer = Join-Path $installerDirectory "PDF Workstation_${version}_x64-setup.exe"
        if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) { throw 'The current-version x64 NSIS installer is missing.' }
        $installers = @(Get-Item -LiteralPath $installer)
    }

    if ($resolvedOutput) {
        $bundleRoot = Join-Path $targetRoot 'release/bundle'
        $unexpectedUpdaterArtifacts = @(Get-ChildItem -LiteralPath $bundleRoot -Recurse -File | Where-Object { $_.Name -CEQ 'latest.json' -or $_.Extension -CEQ '.sig' })
        if (($UnsignedLocal -or $ArtifactOnly) -and $unexpectedUpdaterArtifacts.Count -ne 0) { throw 'Bounded bundle unexpectedly contains updater signature artifacts.' }
        if ($ReleaseOcr) {
            $releaseVersion = (Get-Content -LiteralPath 'src-tauri/tauri.conf.json' -Raw -Encoding UTF8 | ConvertFrom-Json).version
            $releaseInstallerName = "PDF Workstation_${releaseVersion}_x64-setup.exe"
            if ([IO.Path]::GetFileName($installer) -cne $releaseInstallerName) { throw 'Release OCR bundle installer is not the current-version installer.' }
            $updaterSignature = "$installer.sig"
            if ($unexpectedUpdaterArtifacts.Count -ne 1 -or -not $unexpectedUpdaterArtifacts[0].FullName.Equals([IO.Path]::GetFullPath($updaterSignature), [StringComparison]::OrdinalIgnoreCase) -or $unexpectedUpdaterArtifacts[0].Length -le 0) {
                throw 'Release OCR bundle must contain exactly the installer updater signature and no manifest.'
            }
        }
        $extractor = Get-InstallerExtractor
        $inventoryLines = @(& $extractor.FullName l -slt $installer)
        if ($LASTEXITCODE -ne 0) { throw 'Could not inventory the installer archive.' }
        $inventoryPath = Join-Path $resolvedOutput 'installer-inventory.txt'
        [IO.File]::WriteAllLines($inventoryPath, $inventoryLines, [Text.UTF8Encoding]::new($false))
        $archivePaths = @(Convert-SevenZipInventory -Lines $inventoryLines)
        Assert-ArchiveResourceInventory -ArchivePaths $archivePaths -BaseEntries $baseResources.Entries -OcrPlan $ocrPlan
        $extractionDirectory = Join-Path $resolvedOutput 'extracted-installer'
        if (Test-Path -LiteralPath $extractionDirectory) { throw 'Installer extraction directory already exists.' }
        New-Item -ItemType Directory -Path $extractionDirectory | Out-Null
        & $extractor.FullName x $installer "-o$extractionDirectory" -y
        if ($LASTEXITCODE -ne 0) { throw 'Could not extract the installer for resource verification.' }
        $baseReceipts = @(Assert-ExtractedBaseResources `
            -ExtractionRoot $extractionDirectory `
            -Entries $baseResources.Entries `
            -PublisherSigned:$SignedOcr `
            -ExpectedPublisher $ExpectedPublisher)
        $ocrReceipts = @(Assert-ExtractedOcrPackage -ExtractionRoot $extractionDirectory -Plan $ocrPlan)
        $packagedApplications = @(Get-ChildItem -LiteralPath $extractionDirectory -Recurse -File | Where-Object Name -CEQ 'pdf-workstation.exe')
        if ($packagedApplications.Count -ne 1) { throw 'The installer must contain exactly one application executable.' }
        $packagedApplication = $packagedApplications[0].FullName
        if ($ocrPlan -and -not (Test-ExecutableContainsAscii -Path $packagedApplication -Marker $ocrPlan.Engine.Sha256)) { throw 'Packaged application does not embed the packaged OCR engine identity.' }

        if ($SignedOcr) {
            $installerFacts = Assert-TrustedWindowsSignature -Path $installer -ExpectedPublisher $ExpectedPublisher
            $applicationFacts = Assert-TrustedWindowsSignature -Path $packagedApplication -ExpectedPublisher $ExpectedPublisher
            $installerStatus = [string]$installerFacts.Status
            $applicationStatus = [string]$applicationFacts.Status
        } else {
            Assert-NotPublisherSigned -Path $installer
            Assert-NotPublisherSigned -Path $packagedApplication
            $installerStatus = (Get-AuthenticodeSignature -LiteralPath $installer).Status.ToString()
            $applicationStatus = (Get-AuthenticodeSignature -LiteralPath $packagedApplication).Status.ToString()
        }
        $engineStatus = $null
        if ($ocrPlan) {
            $engineReceipts = @($ocrReceipts | Where-Object path -Like '*/bin/tesseract.exe')
            if ($engineReceipts.Count -ne 1) { throw 'Extracted OCR proof has no unique engine receipt.' }
            $engineReceipt = $engineReceipts[0]
            $extractedEngine = Find-ExtractedResource -Files @(Get-ChildItem -LiteralPath $extractionDirectory -Recurse -File) -Target $engineReceipt.path
            if ($SignedOcr) {
                $engineFacts = Assert-TrustedWindowsSignature -Path $extractedEngine.FullName -ExpectedPublisher $ExpectedPublisher
                $engineStatus = [string]$engineFacts.Status
                Assert-UnsignedWindowsFile -Path ($originalPlan.Files | Where-Object Role -CEQ 'engine').Source
            } else {
                Assert-NotPublisherSigned -Path $extractedEngine.FullName
                $engineStatus = (Get-AuthenticodeSignature -LiteralPath $extractedEngine.FullName).Status.ToString()
            }
        }

        $receipt = [ordered]@{
            schemaVersion = 1
            scope = if ($ReleaseOcr) { 'Azure-signed release installer with OCR, extraction proof only; no install, launch, update, or publication claim.' } elseif ($ArtifactOnly) { 'Artifact-only Azure OCR installer extraction proof; no install, launch, update, workflow, or release claim.' } elseif ($ocrPlan) { 'Unsigned OCR installer extraction proof (the default installer configuration); no install, launch, update, or release claim.' } else { 'Unsigned installer extraction proof without OCR; no install, launch, update, or release claim.' }
            mode = if ($ReleaseOcr) { 'azure-signed-release-ocr' } elseif ($ArtifactOnly) { 'azure-signed-artifact-only-ocr' } elseif ($ocrPlan) { 'unsigned-local-ocr' } else { 'unsigned-local-without-ocr' }
            sourceRevision = $SourceRevision
            installer = @{ path = $installer.Substring($resolvedOutput.Length + 1).Replace('\', '/'); bytes = [uint64]$installers[0].Length; sha256 = Get-ExactSha256 -Path $installer }
            application = @{ path = $packagedApplication.Substring($resolvedOutput.Length + 1).Replace('\', '/'); bytes = [uint64]$packagedApplications[0].Length; sha256 = Get-ExactSha256 -Path $packagedApplication }
            archiveInventory = @{ path = 'installer-inventory.txt'; bytes = [uint64](Get-Item -LiteralPath $inventoryPath).Length; sha256 = Get-ExactSha256 -Path $inventoryPath }
            baseResources = $baseReceipts
            ocr = @{ enabled = [bool]$ocrPlan; identity = if ($ocrPlan) { $ocrPlan.Identity } else { $null }; setupManifestSha256 = if ($ocrPlan) { $ocrPlan.ManifestSha256 } else { $null }; originalEngine = $originalEngineReceipt; packagedResources = $ocrReceipts; licensesArePackagedSidecarsNotNoticeDialogContent = [bool]$ocrPlan }
            signatures = @{ expectedPublisher = if ($SignedOcr) { $ExpectedPublisher } else { $null }; installer = $installerStatus; application = $applicationStatus; engine = $engineStatus; originalEngine = $originalEngineStatus }
            config = if ($ReleaseOcr) { @{ ephemeral = $true; retained = $false; updaterArtifacts = @([IO.Path]::GetFileName($updaterSignature)) } } elseif ($ArtifactOnly) { @{ ephemeral = $true; retained = $false; updaterArtifacts = @() } } else { @{ path = $configPath.Substring($resolvedOutput.Length + 1).Replace('\', '/'); sha256 = Get-ExactSha256 -Path $configPath; updaterArtifacts = @() } }
        }
        Write-JsonUtf8 -Path (Join-Path $resolvedOutput 'installer-verification.json') -Value $receipt
    } elseif ($AzureSigning) {
        & "$PSScriptRoot/verify-windows-signatures.ps1" -Paths @($installer)
        $extractor = Get-Command 7z -ErrorAction Stop
        $verificationDirectory = Join-Path $projectRoot ("src-tauri/target/signature-checks/" + [Guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $verificationDirectory -Force | Out-Null
        & $extractor.Source e $installer "-o$verificationDirectory" -r -y 'pdf-workstation.exe'
        if ($LASTEXITCODE -ne 0) { throw 'Could not extract the packaged application for signature verification.' }
        $packagedApplication = Join-Path $verificationDirectory 'pdf-workstation.exe'
        if (-not (Test-Path -LiteralPath $packagedApplication -PathType Leaf)) { throw 'The installer does not contain the expected application executable.' }
        & "$PSScriptRoot/verify-windows-signatures.ps1" -Paths @($packagedApplication)
    }

    if ($ReleaseOcr) {
        $releaseBundle = Join-Path $projectRoot 'src-tauri/target/release/bundle/nsis'
        if ((Test-Path -LiteralPath $releaseBundle) -and @(Get-ChildItem -LiteralPath $releaseBundle -Force).Count -ne 0) { throw 'The release bundle folder must be empty before the release OCR installer is staged.' }
        New-Item -ItemType Directory -Path $releaseBundle -Force | Out-Null
        foreach ($source in @($installer, $updaterSignature)) {
            $destination = Join-Path $releaseBundle ([IO.Path]::GetFileName($source))
            Copy-Item -LiteralPath $source -Destination $destination
            if ((Get-Item -LiteralPath $destination).Length -ne (Get-Item -LiteralPath $source).Length -or (Get-ExactSha256 -Path $destination) -cne (Get-ExactSha256 -Path $source)) { throw 'Staged release asset differs from the verified build output.' }
        }
        Write-Output ('RELEASE_OCR_IDENTITY identity={0} setupManifestSha256={1} originalEngineBytes={2} originalEngineSha256={3}' -f $ocrPlan.Identity, $ocrPlan.ManifestSha256, $originalEngineReceipt.bytes, $originalEngineReceipt.sha256)
    }
    if (-not $UnsignedLocal -and -not $ArtifactOnly -and ($ReleaseOcr -or -not $OcrSetupRoot)) {
        node scripts/release-manifest.mjs
        if ($LASTEXITCODE -ne 0) { throw 'Update manifest generation failed.' }
    }
} finally {
    Close-ReadLocks -Locks $baseLocks
    Close-ReadLocks -Locks $derivedLocks
    Close-ReadLocks -Locks $sourceLocks
    foreach ($name in @(
        'PDF_WORKSTATION_SIGNED_OCR_ENGINE_PATH',
        'PDF_WORKSTATION_SIGNED_OCR_ENGINE_BYTES',
        'PDF_WORKSTATION_SIGNED_OCR_ENGINE_SHA256',
        'PDF_WORKSTATION_EXPECTED_PUBLISHER'
    )) {
        Remove-Item "Env:$name" -ErrorAction SilentlyContinue
    }
}
