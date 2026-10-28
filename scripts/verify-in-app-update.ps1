param(
    [Parameter(Mandatory = $true)][string]$WorkRoot,
    [Parameter(Mandatory = $true)][string]$OutputRoot,
    [Parameter(Mandatory = $true)][string]$WebDriverRoot,
    [Parameter(Mandatory = $true)][string]$WebViewProfileRoot,
    [Parameter(Mandatory = $true)][ValidateSet('HostedNonInteractive')][string]$PublisherUiMode,
    [Parameter(Mandatory = $true)][string]$WorkflowSourceRevision,
    [Parameter(Mandatory = $true)][string]$TargetVersion,
    [Parameter(Mandatory = $true)][string]$TargetTag,
    [Parameter(Mandatory = $true)][string]$TargetSourceRevision,
    [Parameter(Mandatory = $true)][string]$TargetReleaseId,
    [Parameter(Mandatory = $true)][string]$TargetInstallerAssetId,
    [Parameter(Mandatory = $true)][string]$TargetInstallerName,
    [Parameter(Mandatory = $true)][string]$TargetInstallerBytes,
    [Parameter(Mandatory = $true)][string]$TargetInstallerSha256,
    [Parameter(Mandatory = $true)][string]$TargetSignatureAssetId,
    [Parameter(Mandatory = $true)][string]$TargetSignatureBytes,
    [Parameter(Mandatory = $true)][string]$TargetSignatureSha256,
    [Parameter(Mandatory = $true)][string]$TargetManifestAssetId,
    [Parameter(Mandatory = $true)][string]$TargetManifestBytes,
    [Parameter(Mandatory = $true)][string]$TargetManifestSha256,
    [Parameter(Mandatory = $true)][string]$ExpectedPublisher
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:UpdatePins = [ordered]@{
    Repository = 'Joshua-Beel/PDF-Workstation'
    BaselineVersion = '0.2.0'
    BaselineName = 'PDF.Workstation_0.2.0_x64-setup.exe'
    BaselineBytes = [uint64]6418229
    BaselineSha256 = '17788CB82BB42DEAC35EA197A385A9BAFC8CD331422446A2B834E115A90C4D2E'
    BaselineUrl = 'https://github.com/Joshua-Beel/PDF-Workstation/releases/download/v0.2.0/PDF.Workstation_0.2.0_x64-setup.exe'
    LatestManifestUrl = 'https://github.com/Joshua-Beel/PDF-Workstation/releases/latest/download/latest.json'
    WebDriverPort = 4444
    NativeDriverPort = 9515
    NetworkBytesMaximum = [uint64](128MB)
    JsonBytesMaximum = [uint64](1MB)
    DriverDeadlineSeconds = 45
    UpdateCheckDeadlineSeconds = 45
    FailedDownloadDeadlineSeconds = 135
    InstallDeadlineSeconds = 180
    InstallerDeadlineMilliseconds = 180000
}

function ConvertTo-ExactUInt64 {
    param([Parameter(Mandatory = $true)][string]$Value,[Parameter(Mandatory = $true)][string]$Kind)
    [uint64]$parsed = 0
    if (-not [uint64]::TryParse($Value,[Globalization.NumberStyles]::None,[Globalization.CultureInfo]::InvariantCulture,[ref]$parsed) -or $parsed -eq 0) {
        throw "$Kind must be one positive base-10 integer."
    }
    return $parsed
}

function Get-ExactSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    $stream = [IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try { return [Convert]::ToHexString($algorithm.ComputeHash($stream)) }
        finally { $algorithm.Dispose() }
    } finally { $stream.Dispose() }
}

function Get-TextReceipt {
    param([AllowEmptyString()][string]$Value,[uint64]$MaximumBytes = 65536)
    $bytes = [Text.Encoding]::UTF8.GetBytes($Value)
    if ([uint64]$bytes.Length -gt $MaximumBytes) { throw 'Text receipt exceeds its byte bound.' }
    return [ordered]@{ bytes = [uint64]$bytes.Length; sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes)) }
}

function Assert-NoReparseAncestors {
    param([Parameter(Mandatory = $true)][string]$Path)
    $candidate = [IO.Path]::GetFullPath($Path)
    while ($candidate) {
        if (Test-Path -LiteralPath $candidate) {
            $item = Get-Item -LiteralPath $candidate -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'A verification path traverses a reparse point.' }
        }
        $parent = [IO.Directory]::GetParent($candidate)
        if ($null -eq $parent) { break }
        $candidate = $parent.FullName
    }
}

function Resolve-RunnerPath {
    param([string]$Path,[string]$RunnerTemp,[switch]$MustExist,[switch]$MustBeFresh)
    $root = [IO.Path]::GetFullPath($RunnerTemp).TrimEnd('\')
    $candidate = [IO.Path]::GetFullPath($Path).TrimEnd('\')
    if (-not $candidate.StartsWith($root + '\',[StringComparison]::OrdinalIgnoreCase)) { throw 'Verification path is outside RUNNER_TEMP.' }
    Assert-NoReparseAncestors -Path $candidate
    if ($MustExist -and -not (Test-Path -LiteralPath $candidate)) { throw 'Required verification path is absent.' }
    if ($MustBeFresh -and (Test-Path -LiteralPath $candidate)) { throw 'Verification output path must be fresh.' }
    return $candidate
}

function Assert-HostedRunner {
    if ([string]$env:GITHUB_ACTIONS -cne 'true' -or [string]$env:RUNNER_ENVIRONMENT -cne 'github-hosted' -or
        [string]$env:GITHUB_REPOSITORY -cne $script:UpdatePins.Repository -or [string]$env:ImageOS -cne 'win22' -or
        [string]$env:GITHUB_EVENT_NAME -cne 'workflow_dispatch' -or [string]$env:GITHUB_REF -cne 'refs/heads/master') {
        throw 'Published in-app update verification requires the exact manual GitHub-hosted Windows 2022 context.'
    }
}

function Assert-FileReceipt {
    param([string]$Path,[uint64]$Bytes,[string]$Sha256,[string]$Kind)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf) -or $Sha256 -cnotmatch '^[A-F0-9]{64}$') { throw "$Kind receipt is malformed." }
    Assert-NoReparseAncestors -Path $Path
    $item = Get-Item -LiteralPath $Path
    if ([uint64]$item.Length -ne $Bytes -or (Get-ExactSha256 -Path $Path) -cne $Sha256) { throw "$Kind does not match its exact receipt." }
}

function Get-WindowsSignatureFacts {
    param([string]$Path,[scriptblock]$SignatureProvider)
    $signature = if ($SignatureProvider) { & $SignatureProvider $Path } else { Get-AuthenticodeSignature -LiteralPath $Path }
    $publisher = if ($signature.PSObject.Properties['Publisher']) { [string]$signature.Publisher } elseif ($signature.SignerCertificate) { $signature.SignerCertificate.GetNameInfo([Security.Cryptography.X509Certificates.X509NameType]::SimpleName,$false) } else { '' }
    $timestamp = if ($signature.PSObject.Properties['HasTimestamp']) { [bool]$signature.HasTimestamp } else { $null -ne $signature.TimeStamperCertificate }
    return [pscustomobject]@{ Status = [string]$signature.Status; Publisher = $publisher; HasTimestamp = $timestamp }
}

function Assert-TrustedWindowsSignature {
    param([string]$Path,[string]$ExpectedPublisher,[scriptblock]$SignatureProvider)
    $facts = Get-WindowsSignatureFacts -Path $Path -SignatureProvider $SignatureProvider
    if ([string]$facts.Status -cne 'Valid' -or [string]$facts.Publisher -cne $ExpectedPublisher -or -not [bool]$facts.HasTimestamp) {
        throw 'Executable lacks the exact trusted timestamped publisher signature.'
    }
    return $facts
}

function Invoke-PublicDownload {
    param([string]$Uri,[string]$Destination,[uint64]$MaximumBytes = $script:UpdatePins.NetworkBytesMaximum)
    $parsed = [Uri]$Uri
    if ($parsed.Scheme -cne 'https' -or $parsed.UserInfo) { throw 'Public download must use credential-free HTTPS.' }
    $handler = [Net.Http.HttpClientHandler]::new(); $handler.AllowAutoRedirect = $true
    $client = [Net.Http.HttpClient]::new($handler); $client.Timeout = [TimeSpan]::FromSeconds(90)
    $client.DefaultRequestHeaders.UserAgent.ParseAdd('PDF-Workstation-published-update-verifier/1')
    $response = $null
    try {
        $response = $client.GetAsync($parsed,[Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        if (-not $response.IsSuccessStatusCode) { throw "Public HTTPS download failed with status $([int]$response.StatusCode)." }
        $length = $response.Content.Headers.ContentLength
        if ($null -ne $length -and ([uint64]$length -eq 0 -or [uint64]$length -gt $MaximumBytes)) { throw 'Public download content length is outside its bound.' }
        $stream = $response.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $file = [IO.File]::Open($Destination,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
        try {
            $buffer = [byte[]]::new(65536); [uint64]$total = 0
            while (($read = $stream.Read($buffer,0,$buffer.Length)) -gt 0) {
                $total += [uint64]$read
                if ($total -gt $MaximumBytes) { throw 'Public download exceeded its byte bound.' }
                $file.Write($buffer,0,$read)
            }
            if ($total -eq 0) { throw 'Public download was empty.' }
        } finally { $file.Dispose(); $stream.Dispose() }
        return [string]$response.RequestMessage.RequestUri.AbsoluteUri
    } finally {
        if ($null -ne $response) { $response.Dispose() }
        $client.Dispose(); $handler.Dispose()
    }
}

function Read-BoundedJson {
    param([string]$Path)
    $item = Get-Item -LiteralPath $Path
    if ([uint64]$item.Length -eq 0 -or [uint64]$item.Length -gt $script:UpdatePins.JsonBytesMaximum) { throw 'JSON input is outside its byte bound.' }
    try { return Get-Content -LiteralPath $Path -Raw -Encoding UTF8 | ConvertFrom-Json }
    catch { throw 'JSON input is malformed.' }
}

function Assert-ExactAsset {
    param($Release,[uint64]$Id,[string]$Name,[uint64]$Bytes,[string]$Sha256)
    $found = @($Release.assets | Where-Object { [uint64]$_.id -eq $Id })
    if ($found.Count -ne 1) { throw 'Target release asset id is absent or ambiguous.' }
    $asset = $found[0]
    $assetUri = [Uri]([string]$asset.browser_download_url)
    $expectedPath = "/$($script:UpdatePins.Repository)/releases/download/$TargetTag/$Name"
    if ([string]$asset.name -cne $Name -or [uint64]$asset.size -ne $Bytes -or [string]$asset.state -cne 'uploaded' -or
        [string]$asset.digest -cne ('sha256:' + $Sha256.ToLowerInvariant()) -or $assetUri.Scheme -cne 'https' -or
        $assetUri.DnsSafeHost -cne 'github.com' -or $assetUri.UserInfo -or $assetUri.Query -or $assetUri.Fragment -or
        [Uri]::UnescapeDataString($assetUri.AbsolutePath) -cne $expectedPath) {
        throw 'Target release asset metadata does not match its exact receipt.'
    }
    return $asset
}

function Invoke-BoundedProcess {
    param([string]$Path,[string[]]$Arguments = @(),[int]$TimeoutMilliseconds = 180000,[switch]$ReturnProcess)
    $start = [Diagnostics.ProcessStartInfo]::new()
    $start.FileName = $Path; $start.UseShellExecute = $false; $start.CreateNoWindow = $true
    $start.WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden
    foreach ($argument in $Arguments) { $null = $start.ArgumentList.Add($argument) }
    $process = [Diagnostics.Process]::new(); $process.StartInfo = $start
    if (-not $process.Start()) { throw 'Bounded process did not start.' }
    if ($ReturnProcess) { return $process }
    try {
        if (-not $process.WaitForExit($TimeoutMilliseconds)) { $process.Kill($true); $process.WaitForExit(); throw 'Bounded process timed out.' }
        if ($process.ExitCode -ne 0) { throw "Bounded process failed with exit code $($process.ExitCode)." }
    } finally { $process.Dispose() }
}

function Expand-TargetInstaller {
    param([string]$Installer,[string]$Destination)
    if (Test-Path -LiteralPath $Destination) { throw 'Installer extraction destination must be fresh.' }
    [IO.Directory]::CreateDirectory($Destination) | Out-Null
    Assert-NoReparseAncestors -Path $Destination
    $sevenZip = (Get-Command 7z.exe -CommandType Application -ErrorAction Stop).Source
    Invoke-BoundedProcess -Path $sevenZip -Arguments @('x','-y','-bd','-bb0',"-o$Destination",$Installer) -TimeoutMilliseconds 90000
    $root = [IO.Path]::GetFullPath($Destination).TrimEnd('\') + '\'
    $files = @(Get-ChildItem -LiteralPath $Destination -Recurse -File -Force)
    if ($files.Count -lt 3 -or $files.Count -gt 512) { throw 'Extracted installer inventory is outside its bound.' }
    foreach ($file in $files) {
        if (-not $file.FullName.StartsWith($root,[StringComparison]::OrdinalIgnoreCase) -or ($file.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Extracted installer inventory escaped its owned root.' }
    }
    $apps = @($files | Where-Object { $_.Name.Equals('pdf-workstation.exe',[StringComparison]::OrdinalIgnoreCase) })
    if ($apps.Count -ne 1) { throw 'Extracted installer application is absent or ambiguous.' }
    return $apps[0]
}

function Get-InstallFacts {
    param([string]$InstallRoot,[string]$RegistryPath)
    if (-not (Test-Path -LiteralPath $RegistryPath)) { throw 'Installed application registry record is absent.' }
    $record = Get-ItemProperty -LiteralPath $RegistryPath
    $app = Join-Path $InstallRoot 'pdf-workstation.exe'
    if (-not (Test-Path -LiteralPath $app -PathType Leaf)) { throw 'Installed application executable is absent.' }
    $item = Get-Item -LiteralPath $app; $signature = Get-WindowsSignatureFacts -Path $app
    return [pscustomobject]@{
        displayName = [string]$record.DisplayName; displayVersion = [string]$record.DisplayVersion; installLocation = [string]$record.InstallLocation
        appPath = $app; appBytes = [uint64]$item.Length; appSha256 = Get-ExactSha256 -Path $app
        fileVersion = [string]$item.VersionInfo.FileVersion; productVersion = [string]$item.VersionInfo.ProductVersion
        signatureStatus = [string]$signature.Status; publisher = [string]$signature.Publisher; hasTimestamp = [bool]$signature.HasTimestamp
    }
}

function Get-InstallLocationForm {
    param([AllowEmptyString()][string]$Value)
    if ($Value.Length -ge 3 -and $Value.StartsWith('"',[StringComparison]::Ordinal) -and $Value.EndsWith('"',[StringComparison]::Ordinal) -and
        $Value.IndexOf('"',1) -eq ($Value.Length - 1)) { return 'quoted' }
    if ($Value.Length -gt 0 -and $Value.IndexOf('"') -lt 0) { return 'unquoted' }
    return 'other'
}

function Test-InstallLocation {
    param([AllowEmptyString()][string]$Value,[string]$InstallRoot)
    # Tauri's NSIS template writes InstallLocation as "$INSTDIR" with literal quotes.
    if ((Get-InstallLocationForm -Value $Value) -cne 'quoted') { return $false }
    $inner = $Value.Substring(1,$Value.Length - 2)
    if ($inner -cne $inner.Trim() -or -not [IO.Path]::IsPathFullyQualified($inner)) { return $false }
    return [IO.Path]::GetFullPath($inner).TrimEnd('\').Equals([IO.Path]::GetFullPath($InstallRoot).TrimEnd('\'),[StringComparison]::OrdinalIgnoreCase)
}

function Get-SafeFactValue {
    param([AllowEmptyString()][string]$Value)
    if ($Value -cmatch '^[A-Za-z0-9.+-]{1,32}$') { return $Value }
    if ($Value.Length -eq 0) { return '<empty>' }
    return '<unprintable>'
}

function Get-InstallFactMismatches {
    param($Facts,[string]$Version,[string]$InstallRoot,$ApplicationReceipt,[string]$Publisher,[string]$SignatureStatus)
    $fields = [Collections.Generic.List[string]]::new()
    if ([string]$Facts.displayName -cne 'PDF Workstation') { $fields.Add('displayName') }
    if ([string]$Facts.displayVersion -cne $Version) { $fields.Add('displayVersion(actual=' + (Get-SafeFactValue ([string]$Facts.displayVersion)) + ')') }
    if (-not (Test-InstallLocation -Value ([string]$Facts.installLocation) -InstallRoot $InstallRoot)) { $fields.Add('installLocation(form=' + (Get-InstallLocationForm -Value ([string]$Facts.installLocation)) + ')') }
    if ([uint64]$Facts.appBytes -ne [uint64]$ApplicationReceipt.bytes) { $fields.Add('appBytes') }
    if ([string]$Facts.appSha256 -cne [string]$ApplicationReceipt.sha256) { $fields.Add('appSha256') }
    if ([string]$Facts.fileVersion -cne $Version) { $fields.Add('fileVersion(actual=' + (Get-SafeFactValue ([string]$Facts.fileVersion)) + ')') }
    if ([string]$Facts.productVersion -cne $Version) { $fields.Add('productVersion(actual=' + (Get-SafeFactValue ([string]$Facts.productVersion)) + ')') }
    if ([string]$Facts.signatureStatus -cne $SignatureStatus) { $fields.Add('signatureStatus(actual=' + (Get-SafeFactValue ([string]$Facts.signatureStatus)) + ')') }
    return @($fields)
}

function Assert-InstallFacts {
    param($Facts,[string]$Version,[string]$InstallRoot,$ApplicationReceipt,[string]$Publisher,[string]$SignatureStatus)
    $mismatches = @(Get-InstallFactMismatches -Facts $Facts -Version $Version -InstallRoot $InstallRoot -ApplicationReceipt $ApplicationReceipt -Publisher $Publisher -SignatureStatus $SignatureStatus)
    if ($mismatches.Count -ne 0) {
        throw ('Installed application facts do not match the exact expected version and receipt: ' + ($mismatches -join ', ') + '.')
    }
    if ($SignatureStatus -ceq 'Valid' -and ([string]$Facts.publisher -cne $Publisher -or -not [bool]$Facts.hasTimestamp)) {
        $fields = @(); if ([string]$Facts.publisher -cne $Publisher) { $fields += 'publisher' }; if (-not [bool]$Facts.hasTimestamp) { $fields += 'hasTimestamp' }
        throw ('Installed application publisher signature facts are incomplete: ' + ($fields -join ', ') + '.')
    }
}

function Get-AppProcesses {
    param([string]$ApplicationPath)
    $canonical = [IO.Path]::GetFullPath($ApplicationPath); $found = [Collections.Generic.List[object]]::new()
    foreach ($process in @(Get-Process -Name 'pdf-workstation' -ErrorAction SilentlyContinue)) {
        try {
            if ([IO.Path]::GetFullPath([string]$process.Path).Equals($canonical,[StringComparison]::OrdinalIgnoreCase)) {
                $found.Add([pscustomobject]@{ Id = [int]$process.Id; StartTimeUtc = $process.StartTime.ToUniversalTime(); StartTicks = [long]$process.StartTime.ToUniversalTime().Ticks })
            }
        } catch { }
    }
    return @($found)
}

function Stop-ExactProcesses {
    param([string]$ApplicationPath)
    foreach ($entry in @(Get-AppProcesses -ApplicationPath $ApplicationPath)) {
        try {
            $process = [Diagnostics.Process]::GetProcessById([int]$entry.Id)
            $samePath = [IO.Path]::GetFullPath([string]$process.MainModule.FileName).Equals([IO.Path]::GetFullPath($ApplicationPath),[StringComparison]::OrdinalIgnoreCase)
            $sameStart = [long]$process.StartTime.ToUniversalTime().Ticks -eq [long]$entry.StartTicks
            if ($samePath -and $sameStart) { $process.Kill($true); $null = $process.WaitForExit(10000) }
            $process.Dispose()
        } catch { }
    }
}

function Invoke-LoopbackJson {
    param([ValidateSet('GET','POST','DELETE')][string]$Method,[string]$Path,$Body,[datetime]$Deadline)
    $remaining = [int][Math]::Floor(($Deadline - [datetime]::UtcNow).TotalSeconds)
    if ($remaining -le 0) { throw 'WebDriver deadline expired.' }
    $parameters = @{ Method = $Method; Uri = "http://127.0.0.1:$($script:UpdatePins.WebDriverPort)$Path"; TimeoutSec = [Math]::Max(1,[Math]::Min(10,$remaining)); NoProxy = $true }
    if ($null -ne $Body) { $parameters.ContentType = 'application/json'; $parameters.Body = ($Body | ConvertTo-Json -Depth 12 -Compress) }
    return Invoke-RestMethod @parameters
}

function Start-WebDriverSession {
    param([string]$ApplicationPath,[string]$TauriDriverPath,[string]$EdgeDriverPath,[string]$ProfileRoot)
    if (Test-Path -LiteralPath $ProfileRoot) { throw 'WebDriver profile root must be fresh.' }
    [IO.Directory]::CreateDirectory($ProfileRoot) | Out-Null
    $deadline = [datetime]::UtcNow.AddSeconds($script:UpdatePins.DriverDeadlineSeconds)
    $driver = Invoke-BoundedProcess -Path $TauriDriverPath -Arguments @("--port=$($script:UpdatePins.WebDriverPort)","--native-port=$($script:UpdatePins.NativeDriverPort)","--native-driver=$EdgeDriverPath") -ReturnProcess
    try {
        $ready = $false
        do {
            try { $status = Invoke-LoopbackJson -Method GET -Path '/status' -Deadline $deadline; $ready = [bool]$status.value.ready } catch { }
            if ($driver.HasExited) { throw 'Pinned tauri-driver exited before readiness.' }
            if (-not $ready) { Start-Sleep -Milliseconds 200 }
        } while (-not $ready -and [datetime]::UtcNow -lt $deadline)
        if (-not $ready) { throw 'Pinned tauri-driver did not become ready.' }
        $body = [ordered]@{ capabilities = [ordered]@{ alwaysMatch = [ordered]@{
            browserName = 'wry'
            'tauri:options' = [ordered]@{ application = $ApplicationPath; args = @(); webviewOptions = [ordered]@{ userDataFolder = $ProfileRoot } }
        } } }
        $session = Invoke-LoopbackJson -Method POST -Path '/session' -Body $body -Deadline $deadline
        $id = [string]$session.value.sessionId
        if ($id -cnotmatch '^[A-Za-z0-9-]+$') { throw 'WebDriver session identifier is malformed.' }
        return [pscustomobject]@{ Driver = $driver; SessionId = $id }
    } catch {
        try { if (-not $driver.HasExited) { $driver.Kill($true); $null = $driver.WaitForExit(10000) } } catch { }
        $driver.Dispose()
        throw
    }
}

function Invoke-WebDriverScript {
    param([string]$SessionId,[string]$Script,[datetime]$Deadline)
    if ($SessionId -cnotmatch '^[A-Za-z0-9-]+$' -or $Script.Length -gt 8192) { throw 'WebDriver script is outside its bounded contract.' }
    $response = Invoke-LoopbackJson -Method POST -Path "/session/$SessionId/execute/sync" -Body ([ordered]@{ script = $Script; args = @() }) -Deadline $Deadline
    return $response.value
}

function Wait-WebDriverValue {
    param([string]$SessionId,[string]$Script,[datetime]$Deadline,[scriptblock]$Predicate,[string]$Kind)
    do {
        $value = Invoke-WebDriverScript -SessionId $SessionId -Script $Script -Deadline $Deadline
        if (& $Predicate $value) { return $value }
        Start-Sleep -Milliseconds 150
    } while ([datetime]::UtcNow -lt $Deadline)
    throw "$Kind did not become true before its deadline."
}

function Stop-WebDriverSession {
    param($Session)
    if ($null -eq $Session) { return }
    $deadline = [datetime]::UtcNow.AddSeconds(10)
    try { $null = Invoke-LoopbackJson -Method DELETE -Path "/session/$($Session.SessionId)" -Deadline $deadline } catch { }
    try { if (-not $Session.Driver.HasExited) { $Session.Driver.Kill($true); $null = $Session.Driver.WaitForExit(10000) } } catch { }
    $Session.Driver.Dispose()
}

function Open-UpdatesDialog {
    param([string]$SessionId,[datetime]$Deadline)
    $homeScript = @'
return {ready:document.readyState==='complete',title:document.title,menu:[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Menu').length};
'@
    $homeUi = Wait-WebDriverValue -SessionId $SessionId -Deadline $Deadline -Kind 'Installed home UI' -Script $homeScript -Predicate { param($v) [bool]$v.ready -and [string]$v.title -ceq 'PDF Workstation' -and [int]$v.menu -eq 1 }
    $menuScript = @'
const m=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Menu'&&x.getAttribute('aria-expanded')==='false');if(m.length===1)m[0].click();return m.length===1;
'@
    if (-not (Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script $menuScript)) { throw 'Installed Menu control was unavailable.' }
    $updateScript = @'
const b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Check for updates…'&&!x.disabled);if(b.length===1)b[0].click();return b.length===1;
'@
    if (-not (Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script $updateScript)) { throw 'Installed update control was unavailable.' }
    return $homeUi
}

function Wait-AvailableUpdate {
    param([string]$SessionId,[datetime]$Deadline,[string]$Version,[string]$Notes)
    $script = @'
const d=document.querySelector('dialog[aria-labelledby="updates-title"]');const ps=d?[...d.querySelectorAll('p')].map(x=>x.textContent.trim()):[];const install=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Install update and restart'):[];return {dialog:!!d,installed:ps.find(x=>x.startsWith('Installed version: '))||'',status:d?.querySelector('[role="status"]')?.textContent.trim()||'',notes:d?.querySelector('pre')?.textContent||'',installCount:install.length,installEnabled:install.length===1&&!install[0].disabled,alerts:d?.querySelectorAll('[role="alert"]').length||0};
'@
    return Wait-WebDriverValue -SessionId $SessionId -Script $script -Deadline $Deadline -Kind 'Published available update UI' -Predicate {
        param($v)
        [bool]$v.dialog -and [string]$v.installed -ceq 'Installed version: 0.2.0' -and [string]$v.status -ceq "Version $Version is available." -and
        [string]$v.notes -ceq $Notes -and [int]$v.installCount -eq 1 -and [bool]$v.installEnabled -and [int]$v.alerts -eq 0
    }
}

function Invoke-InstallButton {
    param([string]$SessionId,[datetime]$Deadline)
    $script = @'
const d=document.querySelector('dialog[aria-labelledby="updates-title"]'),b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Install update and restart'&&!x.disabled):[];if(b.length===1)b[0].click();return b.length===1;
'@
    return Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script $script
}

function Wait-InstallFailure {
    param([string]$SessionId,[datetime]$Deadline)
    $script = @'
const d=document.querySelector('dialog[aria-labelledby="updates-title"]');return {status:d?.querySelector('[role="status"]')?.textContent.trim()||'',alert:d?.querySelector('[role="alert"]')?.textContent.trim()||'',retry:[...d?.querySelectorAll('button')||[]].filter(x=>x.textContent.trim()==='Install update and restart'&&!x.disabled).length};
'@
    return Wait-WebDriverValue -SessionId $SessionId -Script $script -Deadline $Deadline -Kind 'Blocked updater download failure' -Predicate {
        param($v)
        [string]$v.status -ceq 'Your installed version is unchanged. You can retry.' -and [string]$v.alert -clike 'Update failed:*' -and [int]$v.retry -eq 1
    }
}

function Wait-CurrentVersionUi {
    param([string]$SessionId,[datetime]$Deadline,[string]$Version)
    $script = @'
const d=document.querySelector('dialog[aria-labelledby="updates-title"]'),ps=d?[...d.querySelectorAll('p')].map(x=>x.textContent.trim()):[];return {dialog:!!d,installed:ps.find(x=>x.startsWith('Installed version: '))||'',status:d?.querySelector('[role="status"]')?.textContent.trim()||'',install:[...d?.querySelectorAll('button')||[]].filter(x=>x.textContent.trim()==='Install update and restart').length,alerts:d?.querySelectorAll('[role="alert"]').length||0};
'@
    return Wait-WebDriverValue -SessionId $SessionId -Script $script -Deadline $Deadline -Kind 'Installed current-version update UI' -Predicate {
        param($v)
        [bool]$v.dialog -and [string]$v.installed -ceq "Installed version: $Version" -and [string]$v.status -ceq 'You have the latest released version.' -and
        [int]$v.install -eq 0 -and [int]$v.alerts -eq 0
    }
}

function Invoke-SilentInstaller {
    param([string]$Path)
    Invoke-BoundedProcess -Path $Path -Arguments @('/S') -TimeoutMilliseconds $script:UpdatePins.InstallerDeadlineMilliseconds
}

function Assert-HostedPublisherUiEvidence {
    param($Value,[string]$Kind,[string]$FileName,[uint64]$Bytes,[string]$Sha256,[string]$Publisher)
    $expected = @('kind','fileName','bytes','sha256','publisher','authenticodeStatus','trustedTimestampVerified',
        'shellPublisherUiAttempted','shellPublisherUiVerified','notAttemptedReason','screenshotsUsed') | Sort-Object -CaseSensitive
    $actual = @($Value.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $expected.Count -or (Compare-Object $expected $actual -CaseSensitive) -or
        [string]$Value.kind -cne $Kind -or [string]$Value.fileName -cne $FileName -or
        [uint64]$Value.bytes -ne $Bytes -or [string]$Value.sha256 -cne $Sha256 -or
        [string]$Value.publisher -cne $Publisher -or [string]$Value.authenticodeStatus -cne 'Valid' -or
        $Value.trustedTimestampVerified -isnot [bool] -or -not $Value.trustedTimestampVerified -or
        $Value.shellPublisherUiAttempted -isnot [bool] -or $Value.shellPublisherUiAttempted -or
        $Value.shellPublisherUiVerified -isnot [bool] -or $Value.shellPublisherUiVerified -or
        [string]$Value.notAttemptedReason -cne 'github-hosted-noninteractive-session' -or
        $Value.screenshotsUsed -isnot [bool] -or $Value.screenshotsUsed) {
        throw 'Hosted publisher evidence receipt is incomplete or malformed.'
    }
}

function Assert-Sentinel {
    param([string]$Path,[uint64]$Bytes,[string]$Sha256)
    Assert-FileReceipt -Path $Path -Bytes $Bytes -Sha256 $Sha256 -Kind 'Upgrade sentinel'
}

if ($WorkflowSourceRevision -cnotmatch '^[a-f0-9]{40}$' -or $TargetSourceRevision -cnotmatch '^[a-f0-9]{40}$' -or
    $TargetVersion -cnotmatch '^\d+\.\d+\.\d+$' -or $TargetTag -cne "v$TargetVersion" -or
    ([version]$TargetVersion -le [version]$script:UpdatePins.BaselineVersion) -or
    # Tauri emits a spaced local filename; GitHub's release asset and generated manifest use its dotted form.
    $TargetInstallerName -cnotmatch "^PDF(?:\.| )Workstation_$([regex]::Escape($TargetVersion))_x64-setup\.exe$" -or
    $TargetInstallerSha256 -cnotmatch '^[A-F0-9]{64}$' -or $TargetSignatureSha256 -cnotmatch '^[A-F0-9]{64}$' -or
    $TargetManifestSha256 -cnotmatch '^[A-F0-9]{64}$' -or [string]::IsNullOrWhiteSpace($ExpectedPublisher) -or
    $ExpectedPublisher -cnotmatch "^[A-Za-z0-9][A-Za-z0-9 .,&'()/-]{0,127}$") {
    throw 'Published target identity inputs are malformed.'
}
$targetReleaseIdValue = ConvertTo-ExactUInt64 -Value $TargetReleaseId -Kind 'Target release id'
$targetInstallerAssetIdValue = ConvertTo-ExactUInt64 -Value $TargetInstallerAssetId -Kind 'Target installer asset id'
$targetInstallerBytesValue = ConvertTo-ExactUInt64 -Value $TargetInstallerBytes -Kind 'Target installer bytes'
$targetSignatureAssetIdValue = ConvertTo-ExactUInt64 -Value $TargetSignatureAssetId -Kind 'Target signature asset id'
$targetSignatureBytesValue = ConvertTo-ExactUInt64 -Value $TargetSignatureBytes -Kind 'Target signature bytes'
$targetManifestAssetIdValue = ConvertTo-ExactUInt64 -Value $TargetManifestAssetId -Kind 'Target manifest asset id'
$targetManifestBytesValue = ConvertTo-ExactUInt64 -Value $TargetManifestBytes -Kind 'Target manifest bytes'

Assert-HostedRunner
if ($PublisherUiMode -cne 'HostedNonInteractive') {
    throw 'GitHub-hosted in-app update verification requires exact HostedNonInteractive publisher UI mode.'
}
$runnerTemp = [string]$env:RUNNER_TEMP
if ([string]::IsNullOrWhiteSpace($runnerTemp)) { throw 'RUNNER_TEMP is unavailable.' }
$work = Resolve-RunnerPath -Path $WorkRoot -RunnerTemp $runnerTemp -MustBeFresh
$output = Resolve-RunnerPath -Path $OutputRoot -RunnerTemp $runnerTemp -MustBeFresh
$driverRoot = Resolve-RunnerPath -Path $WebDriverRoot -RunnerTemp $runnerTemp -MustExist
$profileRoot = Resolve-RunnerPath -Path $WebViewProfileRoot -RunnerTemp $runnerTemp -MustBeFresh
[IO.Directory]::CreateDirectory($work) | Out-Null
Assert-NoReparseAncestors -Path $work

$downloads = Join-Path $work 'downloads'
[IO.Directory]::CreateDirectory($downloads) | Out-Null
$metadataPath = Join-Path $downloads 'release.json'
$latestMetadataPath = Join-Path $downloads 'latest-release.json'
$baselinePath = Join-Path $downloads $script:UpdatePins.BaselineName
$targetInstallerPath = Join-Path $downloads $TargetInstallerName
$targetSignatureName = "$TargetInstallerName.sig"
$targetSignaturePath = Join-Path $downloads $targetSignatureName
$targetManifestPath = Join-Path $downloads 'latest.json'
$null = Invoke-PublicDownload -Uri "https://api.github.com/repos/$($script:UpdatePins.Repository)/releases/tags/$TargetTag" -Destination $metadataPath -MaximumBytes $script:UpdatePins.JsonBytesMaximum
$null = Invoke-PublicDownload -Uri "https://api.github.com/repos/$($script:UpdatePins.Repository)/releases/latest" -Destination $latestMetadataPath -MaximumBytes $script:UpdatePins.JsonBytesMaximum
$release = Read-BoundedJson -Path $metadataPath
$latestRelease = Read-BoundedJson -Path $latestMetadataPath
if ([uint64]$release.id -ne $targetReleaseIdValue -or [uint64]$latestRelease.id -ne $targetReleaseIdValue -or
    [string]$release.tag_name -cne $TargetTag -or [bool]$release.draft -or [bool]$release.prerelease -or
    [string]::IsNullOrWhiteSpace([string]$release.published_at)) {
    throw 'Target is not the exact latest published stable release.'
}
$installerAsset = Assert-ExactAsset -Release $release -Id $targetInstallerAssetIdValue -Name $TargetInstallerName -Bytes $targetInstallerBytesValue -Sha256 $TargetInstallerSha256
$signatureAsset = Assert-ExactAsset -Release $release -Id $targetSignatureAssetIdValue -Name $targetSignatureName -Bytes $targetSignatureBytesValue -Sha256 $TargetSignatureSha256
$manifestAsset = Assert-ExactAsset -Release $release -Id $targetManifestAssetIdValue -Name 'latest.json' -Bytes $targetManifestBytesValue -Sha256 $TargetManifestSha256
$null = Invoke-PublicDownload -Uri $script:UpdatePins.BaselineUrl -Destination $baselinePath
$null = Invoke-PublicDownload -Uri ([string]$installerAsset.browser_download_url) -Destination $targetInstallerPath
$null = Invoke-PublicDownload -Uri ([string]$signatureAsset.browser_download_url) -Destination $targetSignaturePath -MaximumBytes 65536
$null = Invoke-PublicDownload -Uri $script:UpdatePins.LatestManifestUrl -Destination $targetManifestPath -MaximumBytes $script:UpdatePins.JsonBytesMaximum
Assert-FileReceipt -Path $baselinePath -Bytes $script:UpdatePins.BaselineBytes -Sha256 $script:UpdatePins.BaselineSha256 -Kind 'Published 0.2.0 baseline installer'
Assert-FileReceipt -Path $targetInstallerPath -Bytes $targetInstallerBytesValue -Sha256 $TargetInstallerSha256 -Kind 'Published target installer'
Assert-FileReceipt -Path $targetSignaturePath -Bytes $targetSignatureBytesValue -Sha256 $TargetSignatureSha256 -Kind 'Published updater signature'
Assert-FileReceipt -Path $targetManifestPath -Bytes $targetManifestBytesValue -Sha256 $TargetManifestSha256 -Kind 'Published latest manifest'
if ((Get-WindowsSignatureFacts -Path $baselinePath).Status -cne 'NotSigned') { throw 'Published 0.2.0 baseline installer is not exactly Authenticode NotSigned.' }
$null = Assert-TrustedWindowsSignature -Path $targetInstallerPath -ExpectedPublisher $ExpectedPublisher
$manifest = Read-BoundedJson -Path $targetManifestPath
$platform = $manifest.platforms.'windows-x86_64'
$signatureText = (Get-Content -LiteralPath $targetSignaturePath -Raw -Encoding UTF8).Trim()
if ([string]$manifest.version -cne $TargetVersion -or [string]::IsNullOrWhiteSpace([string]$manifest.notes) -or
    [string]$platform.signature -cne $signatureText -or [string]$platform.url -cne [string]$installerAsset.browser_download_url) {
    throw 'Public latest.json does not bind the exact target version, installer, notes, and detached signature.'
}
$manifestNotesReceipt = Get-TextReceipt -Value ([string]$manifest.notes)

$extractedApp = Expand-TargetInstaller -Installer $targetInstallerPath -Destination (Join-Path $work 'target-extracted')
$targetApplicationReceipt = [ordered]@{ bytes = [uint64]$extractedApp.Length; sha256 = Get-ExactSha256 -Path $extractedApp.FullName }
$null = Assert-TrustedWindowsSignature -Path $extractedApp.FullName -ExpectedPublisher $ExpectedPublisher
$projectRoot = Split-Path -Parent $PSScriptRoot
. (Join-Path $PSScriptRoot 'installed-publisher-ui.ps1')
$installerUiReceipt = [pscustomobject]([ordered]@{ bytes = $targetInstallerBytesValue; sha256 = $TargetInstallerSha256 })
$installerPublisherUi = New-HostedPublisherUiNotAttemptedEvidence -Path $targetInstallerPath -Receipt $installerUiReceipt -Kind installer -ExpectedPublisher $ExpectedPublisher

$installRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'PDF Workstation')).TrimEnd('\')
$settingsRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\')
$registryPath = 'Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDF Workstation'
if ((Test-Path -LiteralPath $installRoot) -or (Test-Path -LiteralPath $settingsRoot) -or (Test-Path -LiteralPath $registryPath)) {
    throw 'Hosted runner is not fresh for the current-user installation.'
}
Invoke-SilentInstaller -Path $baselinePath
$baselineFacts = Get-InstallFacts -InstallRoot $installRoot -RegistryPath $registryPath
$baselineReceipt = [ordered]@{ bytes = [uint64]$baselineFacts.appBytes; sha256 = [string]$baselineFacts.appSha256 }
Assert-InstallFacts -Facts $baselineFacts -Version $script:UpdatePins.BaselineVersion -InstallRoot $installRoot -ApplicationReceipt $baselineReceipt -Publisher '' -SignatureStatus 'NotSigned'

[IO.Directory]::CreateDirectory($settingsRoot) | Out-Null
$settingsSentinel = Join-Path $settingsRoot 'upgrade-sentinel.json'
[IO.File]::WriteAllText($settingsSentinel,'{"kind":"published-in-app-update-sentinel","version":1}',[Text.UTF8Encoding]::new($false))
$settingsSentinelReceipt = [ordered]@{ bytes = [uint64](Get-Item -LiteralPath $settingsSentinel).Length; sha256 = Get-ExactSha256 -Path $settingsSentinel }
$documentSentinel = Join-Path $work 'document-sentinel.pdf'
[IO.File]::Copy((Join-Path $projectRoot 'src-tauri/tests/fixtures/reportlab-plain-fields.pdf'),$documentSentinel,$false)
$documentSentinelReceipt = [ordered]@{ bytes = [uint64](Get-Item -LiteralPath $documentSentinel).Length; sha256 = Get-ExactSha256 -Path $documentSentinel }

$driverReceipt = Read-BoundedJson -Path (Join-Path $driverRoot 'webdriver-receipt.json')
$tauriDriver = Join-Path $driverRoot 'tauri-driver-install/bin/tauri-driver.exe'
$edgeDriver = Join-Path $driverRoot 'edge-driver/msedgedriver.exe'
Assert-FileReceipt -Path $tauriDriver -Bytes ([uint64]$driverReceipt.tauriDriver.bytes) -Sha256 ([string]$driverReceipt.tauriDriver.sha256) -Kind 'Pinned tauri-driver'
Assert-FileReceipt -Path $edgeDriver -Bytes ([uint64]$driverReceipt.edgeDriver.bytes) -Sha256 ([string]$driverReceipt.edgeDriver.sha256) -Kind 'Pinned EdgeDriver'

$session = $null
$postSession = $null
$firewallName = $null
$automaticRelaunch = $null
try {
    $session = Start-WebDriverSession -ApplicationPath ([string]$baselineFacts.appPath) -TauriDriverPath $tauriDriver -EdgeDriverPath $edgeDriver -ProfileRoot (Join-Path $profileRoot 'baseline')
    $checkDeadline = [datetime]::UtcNow.AddSeconds($script:UpdatePins.UpdateCheckDeadlineSeconds)
    $null = Open-UpdatesDialog -SessionId $session.SessionId -Deadline $checkDeadline
    $available = Wait-AvailableUpdate -SessionId $session.SessionId -Deadline $checkDeadline -Version $TargetVersion -Notes ([string]$manifest.notes)
    $baselineProcesses = @(Get-AppProcesses -ApplicationPath ([string]$baselineFacts.appPath))
    if ($baselineProcesses.Count -ne 1) { throw 'WebDriver did not own exactly one installed baseline process.' }
    $baselineProcessId = [int]$baselineProcesses[0].Id

    $firewallName = 'PDF Workstation updater cutoff ' + [Guid]::NewGuid().ToString('N')
    $null = New-NetFirewallRule -DisplayName $firewallName -Direction Outbound -Action Block -Program ([string]$baselineFacts.appPath) -Profile Any
    if (-not (Invoke-InstallButton -SessionId $session.SessionId -Deadline ([datetime]::UtcNow.AddSeconds(10)))) { throw 'First in-app install action was unavailable.' }
    $failed = Wait-InstallFailure -SessionId $session.SessionId -Deadline ([datetime]::UtcNow.AddSeconds($script:UpdatePins.FailedDownloadDeadlineSeconds))
    $unchanged = Get-InstallFacts -InstallRoot $installRoot -RegistryPath $registryPath
    Assert-InstallFacts -Facts $unchanged -Version $script:UpdatePins.BaselineVersion -InstallRoot $installRoot -ApplicationReceipt $baselineReceipt -Publisher '' -SignatureStatus 'NotSigned'
    Assert-Sentinel -Path $settingsSentinel -Bytes $settingsSentinelReceipt.bytes -Sha256 $settingsSentinelReceipt.sha256
    Assert-Sentinel -Path $documentSentinel -Bytes $documentSentinelReceipt.bytes -Sha256 $documentSentinelReceipt.sha256
    Remove-NetFirewallRule -DisplayName $firewallName -ErrorAction Stop
    if ($null -ne (Get-NetFirewallRule -DisplayName $firewallName -ErrorAction SilentlyContinue)) { throw 'Updater cutoff firewall rule remained after explicit cleanup.' }
    $firewallName = $null

    $retryStarted = [datetime]::UtcNow
    if (-not (Invoke-InstallButton -SessionId $session.SessionId -Deadline $retryStarted.AddSeconds(10))) { throw 'Retry in-app install action was unavailable.' }
    $sawDownloadState = $false
    $sessionClosedForInstall = $false
    $installDeadline = $retryStarted.AddSeconds($script:UpdatePins.InstallDeadlineSeconds)
    $statusScript = @'
const d=document.querySelector('dialog[aria-labelledby="updates-title"]');return d?.querySelector('[role="status"]')?.textContent.trim()||'';
'@
    do {
        try {
            $state = Invoke-WebDriverScript -SessionId $session.SessionId -Deadline $installDeadline -Script $statusScript
            if ([string]$state -clike 'Downloading signed update*' -or [string]$state -clike 'Downloading update:*' -or
                [string]$state -clike 'Downloaded *' -or [string]$state -ceq 'Verifying update and starting installer…') { $sawDownloadState = $true }
        } catch { $sessionClosedForInstall = $true }
        if (Test-Path -LiteralPath $registryPath) {
            try { if ([string](Get-ItemProperty -LiteralPath $registryPath).DisplayVersion -ceq $TargetVersion) { break } } catch { }
        }
        Start-Sleep -Milliseconds 150
    } while ([datetime]::UtcNow -lt $installDeadline)
    $upgraded = Get-InstallFacts -InstallRoot $installRoot -RegistryPath $registryPath
    Assert-InstallFacts -Facts $upgraded -Version $TargetVersion -InstallRoot $installRoot -ApplicationReceipt $targetApplicationReceipt -Publisher $ExpectedPublisher -SignatureStatus 'Valid'
    Assert-Sentinel -Path $settingsSentinel -Bytes $settingsSentinelReceipt.bytes -Sha256 $settingsSentinelReceipt.sha256
    Assert-Sentinel -Path $documentSentinel -Bytes $documentSentinelReceipt.bytes -Sha256 $documentSentinelReceipt.sha256
    if (-not $sawDownloadState) { throw 'The retry never exposed a real updater download or verification state.' }
    try {
        $null = Invoke-WebDriverScript -SessionId $session.SessionId -Deadline ([datetime]::UtcNow.AddSeconds(5)) -Script 'return document.title;'
        throw 'The baseline WebDriver session survived after the updater replaced and exited the baseline application.'
    } catch {
        if ($_.Exception.Message -like 'The baseline WebDriver session survived*') { throw }
        $sessionClosedForInstall = $true
    }
    try {
        $old = [Diagnostics.Process]::GetProcessById($baselineProcessId)
        $oldStartMatches = [long]$old.StartTime.ToUniversalTime().Ticks -eq [long]$baselineProcesses[0].StartTicks
        if ($oldStartMatches -and -not $old.HasExited) { throw 'The exact baseline application process did not exit for updater installation.' }
        $old.Dispose()
    } catch [ArgumentException] { }
    if (-not $sessionClosedForInstall) { throw 'Updater installation did not close the baseline WebDriver session.' }
    do {
        $relaunches = @(Get-AppProcesses -ApplicationPath ([string]$upgraded.appPath) | Where-Object { [int]$_.Id -ne $baselineProcessId -and $_.StartTimeUtc -ge $retryStarted.AddSeconds(-2) })
        if ($relaunches.Count -eq 1) { $automaticRelaunch = $relaunches[0]; break }
        if ($relaunches.Count -gt 1) { throw 'Automatic updater relaunch was ambiguous.' }
        Start-Sleep -Milliseconds 200
    } while ([datetime]::UtcNow -lt $installDeadline)
    if ($null -eq $automaticRelaunch) { throw 'The updater did not automatically relaunch the exact installed target application.' }
    Stop-ExactProcesses -ApplicationPath ([string]$upgraded.appPath)
    Stop-WebDriverSession -Session $session
    $session = $null

    $postSession = Start-WebDriverSession -ApplicationPath ([string]$upgraded.appPath) -TauriDriverPath $tauriDriver -EdgeDriverPath $edgeDriver -ProfileRoot (Join-Path $profileRoot 'target')
    $postDeadline = [datetime]::UtcNow.AddSeconds($script:UpdatePins.UpdateCheckDeadlineSeconds)
    $null = Open-UpdatesDialog -SessionId $postSession.SessionId -Deadline $postDeadline
    $current = Wait-CurrentVersionUi -SessionId $postSession.SessionId -Deadline $postDeadline -Version $TargetVersion
    Stop-WebDriverSession -Session $postSession
    $postSession = $null
    Stop-ExactProcesses -ApplicationPath ([string]$upgraded.appPath)

    $applicationPublisherUi = New-HostedPublisherUiNotAttemptedEvidence -Path ([string]$upgraded.appPath) -Receipt ([pscustomobject]$targetApplicationReceipt) -Kind installed-application -ExpectedPublisher $ExpectedPublisher

    # Model one bounded installer-stage damage state without claiming NSIS transactionality:
    # quarantine the exact installed application, prove it is absent, then use only the
    # exact signed standalone installer to restore it.
    $quarantineRoot = Join-Path $work 'repair-quarantine'
    [IO.Directory]::CreateDirectory($quarantineRoot) | Out-Null
    Assert-NoReparseAncestors -Path $quarantineRoot
    $quarantineApp = Join-Path $quarantineRoot 'pdf-workstation.exe'
    if (Test-Path -LiteralPath $quarantineApp) { throw 'Repair quarantine target must be fresh.' }
    Assert-FileReceipt -Path ([string]$upgraded.appPath) -Bytes ([uint64]$targetApplicationReceipt.bytes) -Sha256 ([string]$targetApplicationReceipt.sha256) -Kind 'Pre-damage installed application'
    [IO.File]::Move([string]$upgraded.appPath,$quarantineApp,$false)
    if (Test-Path -LiteralPath ([string]$upgraded.appPath)) { throw 'Controlled damage did not remove the installed application executable.' }
    Assert-FileReceipt -Path $quarantineApp -Bytes ([uint64]$targetApplicationReceipt.bytes) -Sha256 ([string]$targetApplicationReceipt.sha256) -Kind 'Quarantined application'
    $null = Assert-TrustedWindowsSignature -Path $quarantineApp -ExpectedPublisher $ExpectedPublisher
    if (@(Get-ChildItem -LiteralPath $quarantineRoot -File -Force).Count -ne 1) { throw 'Repair quarantine contains an unexpected file inventory.' }
    Invoke-SilentInstaller -Path $targetInstallerPath
    $repaired = Get-InstallFacts -InstallRoot $installRoot -RegistryPath $registryPath
    Assert-InstallFacts -Facts $repaired -Version $TargetVersion -InstallRoot $installRoot -ApplicationReceipt $targetApplicationReceipt -Publisher $ExpectedPublisher -SignatureStatus 'Valid'
    Assert-Sentinel -Path $settingsSentinel -Bytes $settingsSentinelReceipt.bytes -Sha256 $settingsSentinelReceipt.sha256
    Assert-Sentinel -Path $documentSentinel -Bytes $documentSentinelReceipt.bytes -Sha256 $documentSentinelReceipt.sha256
    Assert-FileReceipt -Path $quarantineApp -Bytes ([uint64]$targetApplicationReceipt.bytes) -Sha256 ([string]$targetApplicationReceipt.sha256) -Kind 'Isolated quarantined application'
    if (@(Get-ChildItem -LiteralPath $quarantineRoot -File -Force).Count -ne 1) { throw 'Repair changed the isolated quarantine inventory.' }
    Stop-ExactProcesses -ApplicationPath ([string]$repaired.appPath)
    if (@(Get-AppProcesses -ApplicationPath ([string]$repaired.appPath)).Count -ne 0) { throw 'Relevant installed application processes remain after verification.' }

    Assert-HostedPublisherUiEvidence -Value $installerPublisherUi -Kind 'installer' -FileName $TargetInstallerName -Bytes $targetInstallerBytesValue -Sha256 $TargetInstallerSha256 -Publisher $ExpectedPublisher
    Assert-HostedPublisherUiEvidence -Value $applicationPublisherUi -Kind 'installed-application' -FileName 'pdf-workstation.exe' -Bytes ([uint64]$targetApplicationReceipt.bytes) -Sha256 ([string]$targetApplicationReceipt.sha256) -Publisher $ExpectedPublisher
    [IO.Directory]::CreateDirectory($output) | Out-Null
    $record = [ordered]@{
        schemaVersion = 2
        scope = 'Ephemeral GitHub-hosted proof of the exact published 0.2.0 UI checking the public latest manifest, deterministic pre-download network failure and retry, signed target install, automatic relaunch, current-version recheck, exact Authenticode publisher and trusted-timestamp verification, sentinel preservation, and standalone signed-installer restoration of a controlled missing-application-executable state; the noninteractive hosted session does not attempt or verify Windows Explorer Digital Signatures UI, and partial-byte interruption and installer transactionality are not established.'
        mode = 'published-stable-in-app-update'
        workflowSourceRevision = $WorkflowSourceRevision
        targetSourceRevision = $TargetSourceRevision
        baseline = [ordered]@{
            version = $script:UpdatePins.BaselineVersion
            installer = [ordered]@{ fileName = $script:UpdatePins.BaselineName; bytes = $script:UpdatePins.BaselineBytes; sha256 = $script:UpdatePins.BaselineSha256; signature = 'NotSigned' }
            installedApplication = $baselineReceipt
        }
        release = [ordered]@{
            id = $targetReleaseIdValue; tag = $TargetTag; version = $TargetVersion; draft = $false; prerelease = $false
            installer = [ordered]@{ id = $targetInstallerAssetIdValue; fileName = $TargetInstallerName; bytes = $targetInstallerBytesValue; sha256 = $TargetInstallerSha256 }
            signature = [ordered]@{ id = $targetSignatureAssetIdValue; fileName = $targetSignatureName; bytes = $targetSignatureBytesValue; sha256 = $TargetSignatureSha256 }
            manifest = [ordered]@{ id = $targetManifestAssetIdValue; fileName = 'latest.json'; bytes = $targetManifestBytesValue; sha256 = $TargetManifestSha256; notes = $manifestNotesReceipt }
            publisher = $ExpectedPublisher
        }
        installedTarget = [ordered]@{
            application = $targetApplicationReceipt
            publisherUi = [ordered]@{
                mode = 'hosted-noninteractive'
                attempted = $false
                verified = $false
                reason = 'github-hosted-noninteractive-session'
                screenshotsUsed = $false
                installer = $installerPublisherUi
                installedApplication = $applicationPublisherUi
            }
            recovery = [ordered]@{ damageKind = 'missing-application-executable'; quarantinedReceiptPreserved = $true; restoredApplicationReceiptMatched = $true }
        }
        updater = [ordered]@{
            baselineUiVersion = [string]$available.installed; offeredVersion = [string]$available.status; earlyFailureStatus = [string]$failed.status
            retryDownloadStateObserved = [bool]$sawDownloadState; updaterSessionClosedForInstall = [bool]$sessionClosedForInstall
            automaticRelaunchObserved = $true; targetUiVersion = [string]$current.installed; targetUiStatus = [string]$current.status
        }
        verification = [ordered]@{
            runner = 'github-hosted-windows-2022'; ephemeral = $true
            publicUnauthenticatedReleaseMetadataVerified = $true; publicLatestManifestVerified = $true
            exactPublishedBaselineInstalled = $true; inAppUpdateUiVerified = $true
            deterministicEarlyDownloadCutoffVerified = $true; failedDownloadLeftBaselineUnchanged = $true
            retryThroughSameUiVerified = $true; updaterSignatureVerified = $true; targetPublisherSignatureVerified = $true
            registryVersionUpdated = $true; installedApplicationReceiptMatched = $true; automaticRelaunchVerified = $true
            postUpdateLatestCheckVerified = $true; settingsSentinelPreserved = $true; documentSentinelPreserved = $true
            publisherUiMode = 'hosted-noninteractive'; publisherUiAttempted = $false
            installerShellPublisherUiVerified = $false; installedApplicationShellPublisherUiVerified = $false
            publisherUiScreenshotsUsed = $false; standaloneSignedInstallerRepairVerified = $true
            partialByteDownloadInterruptionVerified = $false; appKillMidDownloadVerified = $false
            installerStageInterruptionVerified = $false; installerTransactionalityVerified = $false
            firewallRuleRemoved = $true; relevantApplicationProcessesRemaining = 0
        }
    }
    $json = $record | ConvertTo-Json -Depth 12
    if ($json -match '(?i)([A-Z]:\\|\\Users\\|runneradmin|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|codesigning\.azure\.net|raw\.log|release-assets\.githubusercontent\.com)') {
        throw 'Sanitized in-app update record contains a host path, credential marker, raw log, or ephemeral download URL.'
    }
    $recordPath = Join-Path $output 'in-app-update-verification.json'
    [IO.File]::WriteAllText($recordPath,$json,[Text.UTF8Encoding]::new($false))
    $outputFiles = @(Get-ChildItem -LiteralPath $output -File -Force)
    if ($outputFiles.Count -ne 1 -or -not $outputFiles[0].FullName.Equals($recordPath,[StringComparison]::OrdinalIgnoreCase)) {
        throw 'Sanitized output must contain exactly one JSON record.'
    }
} finally {
    if ($firewallName) { Remove-NetFirewallRule -DisplayName $firewallName -ErrorAction SilentlyContinue }
    Stop-WebDriverSession -Session $postSession
    Stop-WebDriverSession -Session $session
    try { if (Test-Path -LiteralPath $installRoot) { Stop-ExactProcesses -ApplicationPath (Join-Path $installRoot 'pdf-workstation.exe') } } catch { }
}
