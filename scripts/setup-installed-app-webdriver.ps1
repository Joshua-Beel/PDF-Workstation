[CmdletBinding()]
param([Parameter(Mandatory = $true)][string]$OutputRoot)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')

$script:DriverPins = [ordered]@{
    Repository = 'Joshua-Beel/PDF-Workstation'
    TauriDriverVersion = '2.0.6'
    TauriDriverPackageSha256 = '24DC39BD26A65361C1C8E067636BBFF1D9DD7E2FC58FF874EDFBA33ACAB0E6D0'
    TauriDriverPackageUrl = 'https://static.crates.io/crates/tauri-driver/tauri-driver-2.0.6.crate'
    EdgeDriverOrigin = 'https://msedgedriver.microsoft.com'
    EdgePublisher = 'Microsoft Corporation'
    TauriDriverPackageOrigin = 'static.crates.io'
    EdgeDriverDownloadOrigins = @('msedgedriver.microsoft.com','msedgewebdriverstorage.blob.core.windows.net')
    MaximumEdgeArchiveBytes = 32MB
    MaximumEdgeEntries = 16
    MaximumEdgeExpandedBytes = 64MB
}

function Invoke-ExactHttpsDownload {
    param(
        [Parameter(Mandatory = $true)][string]$Uri,
        [Parameter(Mandatory = $true)][string]$Destination,
        [Parameter(Mandatory = $true)][string[]]$AllowedFinalHosts,
        [Parameter(Mandatory = $true)][uint64]$MaximumBytes
    )
    $requested = [Uri]::new($Uri)
    if ($requested.Scheme -cne 'https' -or -not $AllowedFinalHosts.Contains($requested.DnsSafeHost)) {
        throw 'Driver download URI is outside its exact HTTPS origin allowlist.'
    }
    $handler = [Net.Http.HttpClientHandler]::new()
    $handler.AllowAutoRedirect = $true
    $handler.MaxAutomaticRedirections = 3
    $client = [Net.Http.HttpClient]::new($handler)
    $client.Timeout = [TimeSpan]::FromMinutes(2)
    try {
        $response = $client.GetAsync($requested, [Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        try {
            if (-not $response.IsSuccessStatusCode) { throw 'Driver download returned a non-success status.' }
            $final = $response.RequestMessage.RequestUri
            if ($final.Scheme -cne 'https' -or -not $AllowedFinalHosts.Contains($final.DnsSafeHost)) {
                throw 'Driver download redirected outside its exact HTTPS origin allowlist.'
            }
            if ($response.Content.Headers.ContentLength -gt $MaximumBytes) { throw 'Driver download exceeds its declared byte limit.' }
            $input = $response.Content.ReadAsStream()
            $output = [IO.FileStream]::new($Destination, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
            try {
                $buffer = [byte[]]::new(65536)
                [uint64]$total = 0
                while (($read = $input.Read($buffer, 0, $buffer.Length)) -gt 0) {
                    $total += [uint64]$read
                    if ($total -gt $MaximumBytes) { throw 'Driver download exceeds its live byte limit.' }
                    $output.Write($buffer, 0, $read)
                }
                if ($total -eq 0) { throw 'Driver download was empty.' }
            } finally { $output.Dispose(); $input.Dispose() }
            return $final
        } finally { $response.Dispose() }
    } finally { $client.Dispose(); $handler.Dispose() }
}

function Resolve-FreshDriverRoot {
    param([Parameter(Mandatory = $true)][string]$Path)
    if ([string]::IsNullOrWhiteSpace($env:RUNNER_TEMP)) { throw 'RUNNER_TEMP is unavailable.' }
    $runner = [IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\')
    $candidate = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if (-not $candidate.StartsWith($runner + '\', [StringComparison]::OrdinalIgnoreCase)) {
        throw 'WebDriver setup must stay beneath RUNNER_TEMP.'
    }
    Assert-NoReparseAncestors -Path $candidate
    if (Test-Path -LiteralPath $candidate) { throw 'WebDriver setup root must be fresh.' }
    return $candidate
}

function Assert-ExactHostedDriverSetup {
    $head = (& git -C (Split-Path -Parent $PSScriptRoot) rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or
        $env:GITHUB_ACTIONS -cne 'true' -or
        $env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or
        $env:RUNNER_OS -cne 'Windows' -or
        $env:ImageOS -cne 'win22' -or
        $env:GITHUB_EVENT_NAME -cne 'workflow_dispatch' -or
        $env:GITHUB_REF -cne 'refs/heads/master' -or
        $env:GITHUB_REPOSITORY -cne $script:DriverPins.Repository -or
        $env:GITHUB_SHA -cnotmatch '^[a-f0-9]{40}$' -or
        $head -cne $env:GITHUB_SHA) {
        throw 'WebDriver setup is restricted to the exact GitHub-hosted Windows 2022 manual master run.'
    }
    & git -C (Split-Path -Parent $PSScriptRoot) diff --quiet --
    if ($LASTEXITCODE -ne 0) { throw 'Tracked working-tree files changed before WebDriver setup.' }
    & git -C (Split-Path -Parent $PSScriptRoot) diff --cached --quiet --
    if ($LASTEXITCODE -ne 0) { throw 'The tracked index changed before WebDriver setup.' }
}

function Get-ExactWebView2RuntimeVersion {
    $paths = @(
        'Registry::HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}',
        'Registry::HKEY_CURRENT_USER\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}'
    )
    $versions = @($paths | Where-Object { Test-Path -LiteralPath $_ } | ForEach-Object {
        [string](Get-ItemPropertyValue -LiteralPath $_ -Name 'pv' -ErrorAction Stop)
    } | Where-Object { $_ -match '^\d+\.\d+\.\d+\.\d+$' -and $_ -cne '0.0.0.0' } | Sort-Object -Unique)
    if ($versions.Count -ne 1) { throw 'The hosted runner must expose one exact WebView2 Evergreen Runtime version.' }
    return $versions[0]
}

function Assert-SafeCrateEntries {
    param([Parameter(Mandatory = $true)][string[]]$Entries)
    $prefix = "tauri-driver-$($script:DriverPins.TauriDriverVersion)/"
    if ($Entries.Count -lt 3 -or $Entries.Count -gt 256) { throw 'The tauri-driver crate has an unexpected entry count.' }
    foreach ($entry in $Entries) {
        $shape = $entry.Replace('\','/')
        if (-not $shape.StartsWith($prefix, [StringComparison]::Ordinal) -or
            $shape.Contains('../') -or $shape.Contains('/./') -or $shape.StartsWith('/') -or $shape.Contains(':') -or
            $shape -match '/\.cargo/config(?:\.toml)?$') {
            throw 'The tauri-driver crate contains an unsafe archive path.'
        }
    }
    foreach ($required in @("${prefix}Cargo.toml", "${prefix}Cargo.lock", "${prefix}src/main.rs")) {
        if (-not $Entries.Contains($required)) { throw 'The tauri-driver crate is missing a required locked-build input.' }
    }
}

function Assert-ExtractedCrateTree {
    param([Parameter(Mandatory = $true)][string]$Source)
    $sourcePrefix = [IO.Path]::GetFullPath($Source).TrimEnd('\') + '\'
    foreach ($entry in @(Get-ChildItem -LiteralPath $Source -Force -Recurse)) {
        $canonical = [IO.Path]::GetFullPath($entry.FullName)
        if (-not $canonical.StartsWith($sourcePrefix, [StringComparison]::OrdinalIgnoreCase) -or ($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'The extracted tauri-driver source contains a link or escaped path.'
        }
    }
}

function Invoke-IsolatedCargoInstall {
    param(
        [Parameter(Mandatory = $true)][string]$Cargo,
        [Parameter(Mandatory = $true)][string]$Source,
        [Parameter(Mandatory = $true)][string]$InstallRoot,
        [Parameter(Mandatory = $true)][string]$CargoHome,
        [scriptblock]$ProcessProvider
    )
    if (Test-Path -LiteralPath $CargoHome) { throw 'Isolated Cargo home must be fresh.' }
    [IO.Directory]::CreateDirectory($CargoHome) | Out-Null
    Assert-NoReparseAncestors -Path $CargoHome
    $priorCargoHome = $env:CARGO_HOME
    try {
        $env:CARGO_HOME = $CargoHome
        if ((Test-Path -LiteralPath (Join-Path $CargoHome 'config')) -or (Test-Path -LiteralPath (Join-Path $CargoHome 'config.toml'))) {
            throw 'Isolated Cargo home contains a source replacement configuration.'
        }
        Push-Location -LiteralPath $Source
        try {
            if ($ProcessProvider) { $exitCode = & $ProcessProvider $Cargo @('install','--path','.','--locked','--root',$InstallRoot) }
            else { & $Cargo install --path . --locked --root $InstallRoot; $exitCode = $LASTEXITCODE }
            if ([int]$exitCode -ne 0) { throw 'The pinned tauri-driver locked build failed.' }
        } finally { Pop-Location }
    } finally { $env:CARGO_HOME = $priorCargoHome }
}

function Expand-ExactEdgeDriver {
    param(
        [Parameter(Mandatory = $true)][string]$Archive,
        [Parameter(Mandatory = $true)][string]$Destination
    )
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        $entries = @($zip.Entries)
        if ($entries.Count -lt 1 -or $entries.Count -gt $script:DriverPins.MaximumEdgeEntries) {
            throw 'The EdgeDriver archive has an unexpected entry count.'
        }
        [uint64]$expanded = 0
        $driverEntries = @()
        foreach ($entry in $entries) {
            $shape = $entry.FullName.Replace('\','/')
            if ([string]::IsNullOrWhiteSpace($shape) -or $shape.StartsWith('/') -or $shape.Contains('../') -or
                $shape.Contains('/./') -or $shape.Contains(':') -or [IO.Path]::IsPathRooted($shape)) {
                throw 'The EdgeDriver archive contains an unsafe path.'
            }
            if ([uint64]$entry.Length -gt [uint64]$script:DriverPins.MaximumEdgeArchiveBytes) {
                throw 'An EdgeDriver archive entry exceeds its byte limit.'
            }
            $expanded += [uint64]$entry.Length
            if ($expanded -gt [uint64]$script:DriverPins.MaximumEdgeExpandedBytes) {
                throw 'The EdgeDriver archive exceeds its expanded byte limit.'
            }
            if ($shape.Equals('msedgedriver.exe', [StringComparison]::Ordinal)) { $driverEntries += $entry }
        }
        if ($driverEntries.Count -ne 1) { throw 'The EdgeDriver archive must contain one canonical root msedgedriver.exe.' }
        [IO.Directory]::CreateDirectory($Destination) | Out-Null
        Assert-NoReparseAncestors -Path $Destination
        $target = Join-Path $Destination 'msedgedriver.exe'
        $input = $driverEntries[0].Open()
        $output = [IO.FileStream]::new($target, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write, [IO.FileShare]::None)
        try { $input.CopyTo($output) } finally { $output.Dispose(); $input.Dispose() }
        return $target
    } finally {
        $zip.Dispose()
    }
}

Assert-ExactHostedDriverSetup
$root = Resolve-FreshDriverRoot -Path $OutputRoot
[IO.Directory]::CreateDirectory($root) | Out-Null
Assert-NoReparseAncestors -Path $root

$crate = Join-Path $root "tauri-driver-$($script:DriverPins.TauriDriverVersion).crate"
$crateFinalUri = Invoke-ExactHttpsDownload -Uri $script:DriverPins.TauriDriverPackageUrl -Destination $crate -AllowedFinalHosts @($script:DriverPins.TauriDriverPackageOrigin) -MaximumBytes 2MB
$crateItem = Get-Item -LiteralPath $crate
$crateSha = Get-ExactSha256 -Path $crate
if ($crateItem.Length -eq 0 -or $crateSha -cne $script:DriverPins.TauriDriverPackageSha256) {
    throw 'The official tauri-driver crate does not match its pinned crates.io index SHA-256.'
}
$tar = (Get-Command 'C:\Windows\System32\tar.exe' -CommandType Application -ErrorAction Stop).Source
$entries = @(& $tar -tzf $crate)
if ($LASTEXITCODE -ne 0) { throw 'The pinned tauri-driver crate inventory failed.' }
Assert-SafeCrateEntries -Entries $entries
$typedEntries = @(& $tar -tvzf $crate)
if ($LASTEXITCODE -ne 0 -or @($typedEntries | Where-Object { $_ -and $_[0] -notin @('-','d') }).Count -ne 0) {
    throw 'The pinned tauri-driver crate contains a link or unsupported archive entry type.'
}
$sourceRoot = Join-Path $root 'crate-source'
[IO.Directory]::CreateDirectory($sourceRoot) | Out-Null
& $tar -xzf $crate -C $sourceRoot
if ($LASTEXITCODE -ne 0) { throw 'The pinned tauri-driver crate extraction failed.' }
$source = Join-Path $sourceRoot "tauri-driver-$($script:DriverPins.TauriDriverVersion)"
Assert-NoReparseAncestors -Path $source
if (-not (Test-Path -LiteralPath (Join-Path $source 'Cargo.lock') -PathType Leaf)) { throw 'The pinned tauri-driver crate has no included lockfile.' }
Assert-ExtractedCrateTree -Source $source
$install = Join-Path $root 'tauri-driver-install'
$cargo = (Get-Command cargo.exe -CommandType Application -ErrorAction Stop).Source
$rustc = (Get-Command rustc.exe -CommandType Application -ErrorAction Stop).Source
$cargoVersion = (& $cargo --version).Trim()
if ($LASTEXITCODE -ne 0 -or $cargoVersion -cnotmatch '^cargo \d+\.\d+\.\d+ ') { throw 'Cargo version could not be bound.' }
$rustcVersion = (& $rustc --version).Trim()
if ($LASTEXITCODE -ne 0 -or $rustcVersion -cnotmatch '^rustc \d+\.\d+\.\d+ ') { throw 'Rust compiler version could not be bound.' }
$cargoHome = Join-Path $root 'cargo-home'
Invoke-IsolatedCargoInstall -Cargo $cargo -Source $source -InstallRoot $install -CargoHome $cargoHome
$tauriDriver = Join-Path $install 'bin/tauri-driver.exe'
Assert-NoReparseAncestors -Path $tauriDriver
if (-not (Test-Path -LiteralPath $tauriDriver -PathType Leaf)) { throw 'The locked tauri-driver build produced no executable.' }
$tauriItem = Get-Item -LiteralPath $tauriDriver

$runtimeVersion = Get-ExactWebView2RuntimeVersion
$edgeArchive = Join-Path $root "edgedriver-$runtimeVersion-win64.zip"
$edgeUri = "$($script:DriverPins.EdgeDriverOrigin)/$runtimeVersion/edgedriver_win64.zip"
$edgeFinalUri = Invoke-ExactHttpsDownload -Uri $edgeUri -Destination $edgeArchive -AllowedFinalHosts $script:DriverPins.EdgeDriverDownloadOrigins -MaximumBytes $script:DriverPins.MaximumEdgeArchiveBytes
$edgeArchiveItem = Get-Item -LiteralPath $edgeArchive
if ($edgeArchiveItem.Length -eq 0 -or $edgeArchiveItem.Length -gt $script:DriverPins.MaximumEdgeArchiveBytes) {
    throw 'The exact EdgeDriver archive has an invalid byte length.'
}
$edgeDriver = Expand-ExactEdgeDriver -Archive $edgeArchive -Destination (Join-Path $root 'edge-driver')
$edgeItem = Get-Item -LiteralPath $edgeDriver
$edgeVersion = [string]$edgeItem.VersionInfo.FileVersion
$runtimeParts = $runtimeVersion.Split('.')
$edgeParts = $edgeVersion.Split('.')
if ($runtimeParts.Count -ne 4 -or $edgeParts.Count -ne 4 -or ($runtimeParts[0..2] -join '.') -cne ($edgeParts[0..2] -join '.')) {
    throw 'EdgeDriver does not match the first three WebView2 Runtime version components.'
}
$edgeSignature = Get-WindowsSignatureFacts -Path $edgeDriver
if ([string]$edgeSignature.Status -cne 'Valid' -or [string]$edgeSignature.Publisher -cne $script:DriverPins.EdgePublisher -or -not [bool]$edgeSignature.HasTimestamp) {
    throw 'EdgeDriver does not have the expected trusted timestamped Microsoft signature.'
}

$receipt = [ordered]@{
    schemaVersion = 1
    tauriDriverSource = [ordered]@{ version = $script:DriverPins.TauriDriverVersion; origin = $crateFinalUri.DnsSafeHost; bytes = [uint64]$crateItem.Length; sha256 = $crateSha }
    tauriDriver = [ordered]@{ version = $script:DriverPins.TauriDriverVersion; cargoVersion = $cargoVersion; rustcVersion = $rustcVersion; bytes = [uint64]$tauriItem.Length; sha256 = Get-ExactSha256 -Path $tauriDriver }
    webView2RuntimeVersion = $runtimeVersion
    edgeDriverArchive = [ordered]@{ origin = $edgeFinalUri.DnsSafeHost; bytes = [uint64]$edgeArchiveItem.Length; sha256 = Get-ExactSha256 -Path $edgeArchive }
    edgeDriver = [ordered]@{ version = $edgeVersion; bytes = [uint64]$edgeItem.Length; sha256 = Get-ExactSha256 -Path $edgeDriver; signatureStatus = 'Valid'; publisher = $script:DriverPins.EdgePublisher; hasTimestamp = $true }
}
$receiptPath = Join-Path $root 'webdriver-receipt.json'
[IO.File]::WriteAllText($receiptPath, ($receipt | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
Write-Output 'Pinned hosted WebDriver setup succeeded.'
Write-Output ('Receipt SHA-256: ' + (Get-ExactSha256 -Path $receiptPath))
