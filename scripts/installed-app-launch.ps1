$script:LaunchPins = [ordered]@{
    TauriDriverVersion = '2.0.6'
    TauriDriverPackageSha256 = '24DC39BD26A65361C1C8E067636BBFF1D9DD7E2FC58FF874EDFBA33ACAB0E6D0'
    EdgePublisher = 'Microsoft Corporation'
    WebDriverPort = 4444
    NativeDriverPort = 4445
    TotalTimeoutMilliseconds = 60000
    RequestTimeoutMilliseconds = 10000
    SessionCreationTimeoutMilliseconds = 30000
    CleanupProcessTimeoutMilliseconds = 30000
    CleanupProcessPollMilliseconds = 100
    OcrUiTextBytesMaximum = 65536
    WebDriverErrorBytesMaximum = 4096
    RequestBytesMaximum = 1MB
    JsonDepthMaximum = 12
    JsonNodesMaximum = 512
    ProcessTimestampToleranceTicks = 10
    OwnedProcessMaximum = 128
    ConsoleHostPublisher = 'Microsoft Windows'
}

function Assert-LaunchExactProperties {
    param($Value,[string[]]$Expected,[string]$Kind)
    $actual = @($Value.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    $wanted = @($Expected | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $wanted.Count -or (Compare-Object $wanted $actual -CaseSensitive)) {
        throw "$Kind has an unexpected or missing property."
    }
}

function Assert-LaunchReceiptValue {
    param($Value,[string]$Kind)
    Assert-LaunchExactProperties -Value $Value -Expected @('bytes','sha256') -Kind $Kind
    if ([uint64]$Value.bytes -eq 0 -or [string]$Value.sha256 -cnotmatch '^[A-F0-9]{64}$') { throw "$Kind has an invalid receipt." }
}

function Assert-WebDriverReceipt {
    param($Receipt)
    Assert-LaunchExactProperties -Value $Receipt -Expected @('schemaVersion','tauriDriverSource','tauriDriver','webView2RuntimeVersion','edgeDriverArchive','edgeDriver') -Kind 'WebDriver receipt'
    if ($Receipt.schemaVersion -ne 1 -or [string]$Receipt.webView2RuntimeVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$') {
        throw 'WebDriver receipt schema or WebView2 version is invalid.'
    }
    Assert-LaunchExactProperties -Value $Receipt.tauriDriverSource -Expected @('version','origin','bytes','sha256') -Kind 'tauri-driver source receipt'
    if ([string]$Receipt.tauriDriverSource.version -cne $script:LaunchPins.TauriDriverVersion -or
        [string]$Receipt.tauriDriverSource.origin -cne 'static.crates.io' -or
        [string]$Receipt.tauriDriverSource.sha256 -cne $script:LaunchPins.TauriDriverPackageSha256 -or
        [uint64]$Receipt.tauriDriverSource.bytes -eq 0) { throw 'tauri-driver source receipt is not pinned.' }
    Assert-LaunchExactProperties -Value $Receipt.tauriDriver -Expected @('version','cargoVersion','rustcVersion','bytes','sha256') -Kind 'tauri-driver executable receipt'
    if ([string]$Receipt.tauriDriver.version -cne $script:LaunchPins.TauriDriverVersion -or [uint64]$Receipt.tauriDriver.bytes -eq 0 -or
        [string]$Receipt.tauriDriver.cargoVersion -cnotmatch '^cargo \d+\.\d+\.\d+ ' -or [string]$Receipt.tauriDriver.rustcVersion -cnotmatch '^rustc \d+\.\d+\.\d+ ' -or
        [string]$Receipt.tauriDriver.sha256 -cnotmatch '^[A-F0-9]{64}$') { throw 'tauri-driver executable receipt is invalid.' }
    Assert-LaunchExactProperties -Value $Receipt.edgeDriverArchive -Expected @('origin','bytes','sha256') -Kind 'EdgeDriver archive receipt'
    if (@('msedgedriver.microsoft.com','msedgewebdriverstorage.blob.core.windows.net') -cnotcontains [string]$Receipt.edgeDriverArchive.origin -or
        [uint64]$Receipt.edgeDriverArchive.bytes -eq 0 -or [string]$Receipt.edgeDriverArchive.sha256 -cnotmatch '^[A-F0-9]{64}$') {
        throw 'EdgeDriver archive receipt is outside its exact Microsoft origin allowlist.'
    }
    Assert-LaunchExactProperties -Value $Receipt.edgeDriver -Expected @('version','bytes','sha256','signatureStatus','publisher','hasTimestamp') -Kind 'EdgeDriver executable receipt'
    $runtimeParts = ([string]$Receipt.webView2RuntimeVersion).Split('.')
    $driverParts = ([string]$Receipt.edgeDriver.version).Split('.')
    if ($driverParts.Count -ne 4 -or ($driverParts[0..2] -join '.') -cne ($runtimeParts[0..2] -join '.') -or
        [uint64]$Receipt.edgeDriver.bytes -eq 0 -or [string]$Receipt.edgeDriver.sha256 -cnotmatch '^[A-F0-9]{64}$' -or
        [string]$Receipt.edgeDriver.signatureStatus -cne 'Valid' -or [string]$Receipt.edgeDriver.publisher -cne $script:LaunchPins.EdgePublisher -or
        $Receipt.edgeDriver.hasTimestamp -isnot [bool] -or -not $Receipt.edgeDriver.hasTimestamp) {
        throw 'EdgeDriver receipt is not trusted or does not match the WebView2 Runtime.'
    }
}

function Assert-BoundedJsonShape {
    param($Value,[int]$Depth = 0,[ref]$Nodes)
    $Nodes.Value++
    if ($Depth -gt $script:LaunchPins.JsonDepthMaximum -or $Nodes.Value -gt $script:LaunchPins.JsonNodesMaximum) {
        throw 'WebDriver JSON exceeded its shape limit.'
    }
    if ($null -eq $Value -or $Value -is [string] -or $Value -is [ValueType]) { return }
    if ($Value -is [Collections.IDictionary]) {
        foreach ($key in $Value.Keys) { Assert-BoundedJsonShape -Value $Value[$key] -Depth ($Depth + 1) -Nodes $Nodes }
        return
    }
    if ($Value -is [Collections.IEnumerable] -and $Value -isnot [Management.Automation.PSCustomObject]) {
        foreach ($item in $Value) { Assert-BoundedJsonShape -Value $item -Depth ($Depth + 1) -Nodes $Nodes }
        return
    }
    foreach ($property in $Value.PSObject.Properties) { Assert-BoundedJsonShape -Value $property.Value -Depth ($Depth + 1) -Nodes $Nodes }
}

function Get-LaunchRemainingMilliseconds {
    param([datetime]$Deadline,[Parameter(Mandatory = $true)][int]$MaximumMilliseconds)
    $remaining = [int][Math]::Floor(($Deadline - [datetime]::UtcNow).TotalMilliseconds)
    if ($remaining -le 0) { throw 'Installed application launch smoke exceeded its total deadline.' }
    if ($MaximumMilliseconds -le 0 -or $MaximumMilliseconds -gt $script:LaunchPins.SessionCreationTimeoutMilliseconds) {
        throw 'Loopback request timeout maximum is outside the bounded launch contract.'
    }
    return [Math]::Min($remaining, $MaximumMilliseconds)
}

function Get-LoopbackRequestTimeoutMilliseconds {
    param([string]$Method,[string]$Path,[int]$Port,[datetime]$Deadline)
    $maximum = if ($Port -eq $script:LaunchPins.WebDriverPort -and $Method -ceq 'POST' -and $Path -ceq '/session') {
        $script:LaunchPins.SessionCreationTimeoutMilliseconds
    } else {
        $script:LaunchPins.RequestTimeoutMilliseconds
    }
    return Get-LaunchRemainingMilliseconds -Deadline $Deadline -MaximumMilliseconds $maximum
}

function Read-BoundedLoopbackResponseText {
    param([Parameter(Mandatory = $true)][IO.Stream]$Stream,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $memory = [IO.MemoryStream]::new()
    try {
        $buffer = [byte[]]::new(8192)
        while ($true) {
            $remaining = Get-LaunchRemainingMilliseconds -Deadline $Deadline -MaximumMilliseconds $script:LaunchPins.SessionCreationTimeoutMilliseconds
            $cancellation = [Threading.CancellationTokenSource]::new()
            try {
                $pending = $Stream.ReadAsync($buffer,0,$buffer.Length,$cancellation.Token)
                if (-not $pending.Wait($remaining)) { $cancellation.Cancel(); throw 'WebDriver response read exceeded its deadline.' }
                $null = Get-LaunchRemainingMilliseconds -Deadline $Deadline -MaximumMilliseconds $script:LaunchPins.SessionCreationTimeoutMilliseconds
                $read = $pending.GetAwaiter().GetResult()
            } finally { $cancellation.Dispose() }
            if ($read -eq 0) { break }
            if ($memory.Length + $read -gt $script:LaunchPins.RequestBytesMaximum) { throw 'WebDriver response exceeded its live byte limit.' }
            $memory.Write($buffer,0,$read)
        }
        return [Text.UTF8Encoding]::new($false,$true).GetString($memory.ToArray())
    } finally { $memory.Dispose() }
}

function Invoke-BoundedLoopbackJson {
    param(
        [ValidateSet('GET','POST','DELETE')][string]$Method,
        [Parameter(Mandatory = $true)][string]$Path,
        $Body,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [int]$Port = $script:LaunchPins.WebDriverPort
    )
    $webDriverRequest = $Port -eq $script:LaunchPins.WebDriverPort -and $Path -cmatch '^/(status|session(?:/[-A-Za-z0-9]+(?:/execute/sync)?)?)$'
    $nativeStatusRequest = $Port -eq $script:LaunchPins.NativeDriverPort -and $Method -ceq 'GET' -and $Path -ceq '/status' -and $null -eq $Body
    if (-not $webDriverRequest -and -not $nativeStatusRequest) {
        throw 'WebDriver requests are restricted to the fixed loopback endpoint and command allowlist.'
    }
    $uri = [Uri]::new("http://127.0.0.1:$Port$Path")
    if (-not $uri.IsLoopback -or $uri.Scheme -cne 'http') { throw 'WebDriver URI is not fixed loopback HTTP.' }
    $timeout = Get-LoopbackRequestTimeoutMilliseconds -Method $Method -Path $Path -Port $Port -Deadline $Deadline
    $requestDeadline = [datetime]::UtcNow.AddMilliseconds($timeout)
    if ($requestDeadline -gt $Deadline) { $requestDeadline = $Deadline }
    $request = [Net.HttpWebRequest]::CreateHttp($uri)
    $request.Method = $Method
    $request.AllowAutoRedirect = $false
    $request.Proxy = $null
    $request.Timeout = $timeout
    $request.ReadWriteTimeout = $timeout
    $request.ContentType = 'application/json; charset=utf-8'
    if ($null -ne $Body) {
        $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($Body | ConvertTo-Json -Depth 12 -Compress))
        if ($bytes.Length -gt $script:LaunchPins.RequestBytesMaximum) { throw 'WebDriver request exceeded its byte limit.' }
        $request.ContentLength = $bytes.Length
        $requestStream = $request.GetRequestStream()
        try { $requestStream.Write($bytes, 0, $bytes.Length) } finally { $requestStream.Dispose() }
    }
    $response = $null
    try {
        $remaining = Get-LaunchRemainingMilliseconds -Deadline $requestDeadline -MaximumMilliseconds $timeout
        $pendingResponse = $request.GetResponseAsync()
        $completed = $false
        try { $completed = $pendingResponse.Wait($remaining) } catch {
            $failure = $_.Exception; while ($null -ne $failure -and $failure -isnot [Net.WebException]) { $failure = $failure.InnerException }
            if ($null -eq $failure -or $failure.Response -isnot [Net.HttpWebResponse]) { throw }
            $errorResponse = [Net.HttpWebResponse]$failure.Response
            try {
                $bodyText = ''
                if ($errorResponse.ContentLength -le $script:LaunchPins.RequestBytesMaximum) {
                    $errorStream = $errorResponse.GetResponseStream()
                    try { $bodyText = Read-BoundedLoopbackResponseText -Stream $errorStream -Deadline $requestDeadline } catch { $bodyText = '' } finally { $errorStream.Dispose() }
                }
                $sanitized = ConvertTo-SanitizedWebDriverError -StatusCode ([int]$errorResponse.StatusCode) -BodyText $bodyText
            } finally { $errorResponse.Dispose() }
            [Console]::Out.WriteLine(('WEBDRIVER_ERROR_DIAG status={0} error={1} message={2}' -f $sanitized.status,$sanitized.error,$sanitized.message))
            throw ('WebDriver returned HTTP {0} ({1}): {2}' -f $sanitized.status,$sanitized.error,$sanitized.message)
        }
        if (-not $completed) { throw 'WebDriver response headers exceeded their deadline.' }
        $response = [Net.HttpWebResponse]$pendingResponse.GetAwaiter().GetResult()
        $null = Get-LaunchRemainingMilliseconds -Deadline $requestDeadline -MaximumMilliseconds $timeout
        if ([int]$response.StatusCode -ne 200) { throw 'WebDriver returned an unexpected HTTP status.' }
        if ($response.ContentLength -gt $script:LaunchPins.RequestBytesMaximum) { throw 'WebDriver response exceeded its declared byte limit.' }
        $stream = $response.GetResponseStream()
        try {
            $text = Read-BoundedLoopbackResponseText -Stream $stream -Deadline $requestDeadline
        } finally { $request.Abort(); $stream.Dispose() }
    } finally {
        $request.Abort()
        if ($null -ne $response) { $response.Dispose() }
    }
    if ([string]::IsNullOrWhiteSpace($text)) { throw 'WebDriver returned an empty JSON response.' }
    $value = $text | ConvertFrom-Json
    $nodes = 0
    Assert-BoundedJsonShape -Value $value -Nodes ([ref]$nodes)
    $null = Get-LaunchRemainingMilliseconds -Deadline $requestDeadline -MaximumMilliseconds $timeout
    return $value
}

function Assert-NativeDriverStatus {
    param($Status,[Parameter(Mandatory = $true)][string]$ExpectedVersion)
    if ($ExpectedVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or
        $null -eq $Status.PSObject.Properties['value'] -or
        $null -eq $Status.value.PSObject.Properties['ready'] -or $Status.value.ready -isnot [bool] -or -not $Status.value.ready -or
        $null -eq $Status.value.PSObject.Properties['build'] -or
        $null -eq $Status.value.build.PSObject.Properties['version']) {
        throw 'Native EdgeDriver status is incomplete.'
    }
    $reported = ([string]$Status.value.build.version -split '\s+')[0]
    if ($reported -cne $ExpectedVersion) { throw 'The live native EdgeDriver version does not match its trusted receipt.' }
    return $reported
}

function Wait-NativeDriverStatus {
    param([Parameter(Mandatory = $true)][string]$ExpectedVersion,[Parameter(Mandatory = $true)][datetime]$Deadline,[Parameter(Mandatory = $true)]$TauriDriver,[scriptblock]$StatusProvider)
    do {
        try {
            $status = if ($StatusProvider) { & $StatusProvider $Deadline } else { Invoke-BoundedLoopbackJson -Method GET -Path '/status' -Port $script:LaunchPins.NativeDriverPort -Deadline $Deadline }
        } catch {
            if ($TauriDriver.HasExited) { throw 'Pinned tauri-driver exited before the native EdgeDriver became ready.' }
            Start-Sleep -Milliseconds 200
            continue
        }
        return Assert-NativeDriverStatus -Status $status -ExpectedVersion $ExpectedVersion
    } while ([datetime]::UtcNow -lt $Deadline)
    throw 'Native EdgeDriver did not become ready before the shared launch deadline.'
}

function Get-ExactProfileBinding {
    param([Parameter(Mandatory = $true)][string]$Candidate,[Parameter(Mandatory = $true)][string]$RequestedProfile,[Parameter(Mandatory = $true)][string]$SettingsRoot)
    $actual = [IO.Path]::GetFullPath($Candidate).TrimEnd('\')
    $requested = [IO.Path]::GetFullPath($RequestedProfile).TrimEnd('\')
    $settings = [IO.Path]::GetFullPath($SettingsRoot).TrimEnd('\')
    $settingsWebView = [IO.Path]::GetFullPath((Join-Path $settings 'EBWebView')).TrimEnd('\')
    if ($actual.Equals($requested,[StringComparison]::OrdinalIgnoreCase)) { return 'requested-profile' }
    if ($actual.Equals($settingsWebView,[StringComparison]::OrdinalIgnoreCase)) { return 'tauri-app-settings-ebwebview' }
    $requestedWebView = [IO.Path]::GetFullPath((Join-Path $requested 'EBWebView')).TrimEnd('\')
    if ($actual.Equals($requestedWebView,[StringComparison]::OrdinalIgnoreCase) -and [IO.Path]::GetFileName($actual) -ceq 'EBWebView') { return 'requested-ebwebview' }
    $relation = if ($actual.Equals($settings,[StringComparison]::OrdinalIgnoreCase)) { 'exact-settings-root' }
        elseif ($actual.StartsWith($settings + '\',[StringComparison]::OrdinalIgnoreCase)) { 'settings-root-other' }
        elseif ($actual.StartsWith($requested + '\',[StringComparison]::OrdinalIgnoreCase)) { 'requested-root-other' }
        elseif ($env:RUNNER_TEMP -and $actual.StartsWith([IO.Path]::GetFullPath($env:RUNNER_TEMP).TrimEnd('\') + '\',[StringComparison]::OrdinalIgnoreCase)) { 'runner-temp-other' }
        else { 'outside-known-roots' }
    throw "Owned WebView2 profile relation '$relation' is outside the exact controlled profile contract."
}

function Get-OwnedProfileBinding {
    param([object[]]$Owned,[Parameter(Mandatory = $true)][string]$RequestedProfile,[Parameter(Mandatory = $true)][string]$SettingsRoot)
    $webViews = @($Owned | Where-Object { [IO.Path]::GetFileName([string]$_.Path).Equals('msedgewebview2.exe',[StringComparison]::OrdinalIgnoreCase) })
    if ($webViews.Count -eq 0) { throw 'No owned WebView2 process was available to bind the fresh profile.' }
    $matched = 0
    $binding = $null
    foreach ($process in $webViews) {
        $commandLine = [string]$process.CommandLine
        $userDataSwitches = [regex]::Matches($commandLine, '(?i)(?:^|\s)"?--user-data-dir=(?:"(?<quoted>[^"]+)"|(?<plain>[^\s"]+))"?(?=\s|$)')
        if ($userDataSwitches.Count -gt 1) { throw 'An owned WebView2 process has multiple user-data-dir switches.' }
        if ($userDataSwitches.Count -eq 1) {
            $candidate = if ($userDataSwitches[0].Groups['quoted'].Success) { $userDataSwitches[0].Groups['quoted'].Value } else { $userDataSwitches[0].Groups['plain'].Value }
            $candidateBinding = Get-ExactProfileBinding -Candidate $candidate -RequestedProfile $RequestedProfile -SettingsRoot $SettingsRoot
            if ($null -ne $binding -and $binding -cne $candidateBinding) { throw 'Owned WebView2 processes used inconsistent controlled profiles.' }
            $binding = $candidateBinding
            $matched++
        }
    }
    if ($matched -eq 0) { throw 'No owned WebView2 command line bound the fresh profile path.' }
    return "owned-webview-$binding"
}

function Assert-TrustedConsoleHostTopology {
    param(
        [object[]]$Owned,
        [Parameter(Mandatory = $true)][int]$RootProcessId,
        [switch]$AllowAbsent,
        [string]$SystemDirectory,
        [scriptblock]$SignatureProvider,
        [scriptblock]$VersionInfoProvider,
        [string]$AdditionalTrustedParentPath,
        [ValidateSet('ocr-engine')][string]$AdditionalTrustedParentCategory
    )
    $items = @($Owned)
    if ($script:LaunchPins.OwnedProcessMaximum -ne 128 -or $items.Count -gt $script:LaunchPins.OwnedProcessMaximum) { throw 'The captured launch inventory is outside its process-count bound.' }
    $hasAdditionalParentPath = -not [string]::IsNullOrWhiteSpace($AdditionalTrustedParentPath)
    $hasAdditionalParentCategory = -not [string]::IsNullOrWhiteSpace($AdditionalTrustedParentCategory)
    if ($hasAdditionalParentPath -ne $hasAdditionalParentCategory -or
        ($hasAdditionalParentPath -and (-not $AllowAbsent -or $AdditionalTrustedParentCategory -cne 'ocr-engine' -or -not [IO.Path]::GetFileName($AdditionalTrustedParentPath).Equals('tesseract.exe',[StringComparison]::OrdinalIgnoreCase)))) {
        throw 'The additional trusted console-host parent configuration is invalid.'
    }
    $expectedAdditionalParentPath = if ($hasAdditionalParentPath) { [IO.Path]::GetFullPath($AdditionalTrustedParentPath) } else { '' }
    $consoleHosts = @($items | Where-Object { [IO.Path]::GetFileName([string]$_.Path).Equals('conhost.exe',[StringComparison]::OrdinalIgnoreCase) })
    if ($AllowAbsent -and $consoleHosts.Count -eq 0) { return 0 }
    if ([string]::IsNullOrWhiteSpace($SystemDirectory)) { $SystemDirectory = [Environment]::GetFolderPath([Environment+SpecialFolder]::System) }
    $expectedPath = [IO.Path]::GetFullPath((Join-Path ([IO.Path]::GetFullPath($SystemDirectory).TrimEnd('\')) 'conhost.exe'))
    if (@($consoleHosts | Where-Object { -not ([IO.Path]::GetFullPath([string]$_.Path)).Equals($expectedPath,[StringComparison]::OrdinalIgnoreCase) }).Count -ne 0) {
        throw 'A captured Windows console host did not use the canonical System32 executable.'
    }
    $ownedIds = [Collections.Generic.HashSet[int]]::new()
    $webViewIds = [Collections.Generic.HashSet[int]]::new()
    $additionalParentIds = [Collections.Generic.HashSet[int]]::new()
    foreach ($process in $items) {
        $processIdProperty = $process.PSObject.Properties['ProcessId']
        $processId = 0
        if ($null -eq $processIdProperty -or -not [int]::TryParse([string]$processIdProperty.Value,[ref]$processId) -or $processId -le 0 -or -not $ownedIds.Add($processId)) {
            throw 'The captured console-host topology has an invalid process identity.'
        }
        if ([IO.Path]::GetFileName([string]$process.Path).Equals('msedgewebview2.exe',[StringComparison]::OrdinalIgnoreCase)) { $null = $webViewIds.Add($processId) }
        if ($hasAdditionalParentPath -and [IO.Path]::GetFileName([string]$process.Path).Equals('tesseract.exe',[StringComparison]::OrdinalIgnoreCase) -and
            ([IO.Path]::GetFullPath([string]$process.Path)).Equals($expectedAdditionalParentPath,[StringComparison]::OrdinalIgnoreCase)) {
            $null = $additionalParentIds.Add($processId)
        }
    }
    $rootParents = 0
    $webViewParents = 0
    $additionalParents = 0
    foreach ($consoleHost in $consoleHosts) {
        if ([int]$consoleHost.ProcessId -eq $RootProcessId) { throw 'The captured console-host topology has an invalid process identity.' }
        $parentProperty = $consoleHost.PSObject.Properties['ParentProcessId']
        $parentProcessId = 0
        if ($null -eq $parentProperty -or -not [int]::TryParse([string]$parentProperty.Value,[ref]$parentProcessId) -or $parentProcessId -le 0) {
            throw 'The captured console-host topology has an invalid parent identity.'
        }
        if ($parentProcessId -eq $RootProcessId) { $rootParents++ }
        elseif ($webViewIds.Contains($parentProcessId)) { $webViewParents++ }
        elseif ($additionalParentIds.Contains($parentProcessId)) { $additionalParents++ }
        else { throw 'A captured Windows console host was outside the trusted captured parent topology.' }
    }
    if ($rootParents + $webViewParents + $additionalParents -ne $consoleHosts.Count -or
        (-not $AllowAbsent -and ($rootParents -lt 1 -or $webViewParents -lt 1))) {
        throw 'The captured Windows console-host parent categories do not match the trusted topology.'
    }
    $null = Assert-NoReparseAncestors -Path $expectedPath
    $null = Assert-TrustedWindowsSignature -Path $expectedPath -SignatureProvider $SignatureProvider -ExpectedPublisher $script:LaunchPins.ConsoleHostPublisher
    $version = if ($VersionInfoProvider) { & $VersionInfoProvider $expectedPath } else { (Get-Item -LiteralPath $expectedPath -ErrorAction Stop).VersionInfo }
    $expectedProduct = 'Microsoft' + [char]0x00AE + ' Windows' + [char]0x00AE + ' Operating System'
    if ([string]$version.CompanyName -cne 'Microsoft Corporation' -or [string]$version.ProductName -cne $expectedProduct -or
        [string]$version.InternalName -cne 'ConHost' -or [string]$version.FileDescription -cne 'Console Window Host') {
        throw 'The canonical Windows console host has an unexpected product identity.'
    }
    return [int]$consoleHosts.Count
}

function Assert-OwnedLaunchExecutables {
    param(
        [object[]]$Owned,
        [Parameter(Mandatory = $true)][string]$ApplicationPath,
        [Parameter(Mandatory = $true)][string]$EdgeDriverPath,
        [Parameter(Mandatory = $true)][string]$OcrEnginePath,
        [int]$RootProcessId = 0,
        [string]$SystemDirectory,
        [scriptblock]$ConsoleHostSignatureProvider,
        [scriptblock]$ConsoleHostVersionInfoProvider
    )
    $applications = @($Owned | Where-Object { [IO.Path]::GetFileName([string]$_.Path).Equals('pdf-workstation.exe',[StringComparison]::OrdinalIgnoreCase) })
    if ($applications.Count -ne 1 -or -not ([IO.Path]::GetFullPath([string]$applications[0].Path)).Equals([IO.Path]::GetFullPath($ApplicationPath),[StringComparison]::OrdinalIgnoreCase)) {
        throw 'WebDriver did not launch exactly the installed signed application.'
    }
    $nativeDrivers = @($Owned | Where-Object { [IO.Path]::GetFileName([string]$_.Path).Equals('msedgedriver.exe',[StringComparison]::OrdinalIgnoreCase) })
    if ($nativeDrivers.Count -ne 1 -or -not ([IO.Path]::GetFullPath([string]$nativeDrivers[0].Path)).Equals([IO.Path]::GetFullPath($EdgeDriverPath),[StringComparison]::OrdinalIgnoreCase)) {
        throw 'tauri-driver did not own exactly the trusted native EdgeDriver executable.'
    }
    $ocrEngines = @($Owned | Where-Object { [IO.Path]::GetFileName([string]$_.Path).Equals('tesseract.exe',[StringComparison]::OrdinalIgnoreCase) })
    if (@($ocrEngines | Where-Object { -not ([IO.Path]::GetFullPath([string]$_.Path)).Equals([IO.Path]::GetFullPath($OcrEnginePath),[StringComparison]::OrdinalIgnoreCase) }).Count -ne 0) {
        throw 'An owned OCR process did not use the exact trusted installed engine.'
    }
    $unknown = @($Owned | Where-Object {
        $leaf = [IO.Path]::GetFileName([string]$_.Path)
        -not $leaf.Equals('pdf-workstation.exe',[StringComparison]::OrdinalIgnoreCase) -and
        -not $leaf.Equals('msedgedriver.exe',[StringComparison]::OrdinalIgnoreCase) -and
        -not $leaf.Equals('msedgewebview2.exe',[StringComparison]::OrdinalIgnoreCase) -and
        -not $leaf.Equals('tesseract.exe',[StringComparison]::OrdinalIgnoreCase) -and
        -not $leaf.Equals('conhost.exe',[StringComparison]::OrdinalIgnoreCase)
    })
    if ($unknown.Count -ne 0) { throw 'The owned launch tree contains an unexpected executable.' }
    $null = Assert-TrustedConsoleHostTopology -Owned $Owned -RootProcessId $RootProcessId -AllowAbsent -SystemDirectory $SystemDirectory -SignatureProvider $ConsoleHostSignatureProvider -VersionInfoProvider $ConsoleHostVersionInfoProvider -AdditionalTrustedParentPath $OcrEnginePath -AdditionalTrustedParentCategory 'ocr-engine'
}

function Get-UniqueOwnedLaunchProcesses {
    param([object[]]$Processes)
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $unique = [Collections.Generic.List[object]]::new()
    foreach ($process in @($Processes)) {
        $key = '{0}|{1}|{2}' -f [int]$process.ProcessId,[long]$process.StartTicks,[IO.Path]::GetFullPath([string]$process.Path)
        if ($seen.Add($key)) { $unique.Add($process) }
    }
    return @($unique)
}

function ConvertTo-SanitizedWebDriverError {
    param([int]$StatusCode,[string]$BodyText)
    $code = 'unparsed'; $message = ''
    try {
        $parsed = $BodyText | ConvertFrom-Json -ErrorAction Stop
        $value = if ($null -ne $parsed -and $null -ne $parsed.PSObject.Properties['value']) { $parsed.value } else { $parsed }
        if ($null -ne $value -and $null -ne $value.PSObject.Properties['error']) { $code = [string]$value.error }
        if ($null -ne $value -and $null -ne $value.PSObject.Properties['message']) { $message = [string]$value.message }
    } catch { $code = 'unparsed' }
    if ($code -cnotmatch '^[a-z][a-z ]{0,39}$') { $code = 'unrecognized' }
    $message = (($message -split "`r?`n")[0])
    $message = $message -replace '(?i)[a-z]:\\[^\s''"<>|]*','<path>' -replace '\\\\[^\s''"<>|]+','<path>' -replace '(?i)file:///\S*','<path>' -replace '(?i)\b(users|runneradmin)\b','<redacted>'
    $message = ($message -replace '[^\x20-\x7E]',' ').Trim()
    if ($message.Length -gt 200) { $message = $message.Substring(0,200) }
    return [pscustomobject][ordered]@{ status = $StatusCode; error = $code; message = $message }
}

function Invoke-WebDriverScript {
    param([string]$SessionId,[string]$Script,[datetime]$Deadline)
    if ($SessionId -cnotmatch '^[A-Za-z0-9-]+$' -or $Script.Length -gt 8192) { throw 'WebDriver session or script is outside its bounded contract.' }
    $response = Invoke-BoundedLoopbackJson -Method POST -Path "/session/$SessionId/execute/sync" -Body ([ordered]@{ script = $Script; args = @() }) -Deadline $Deadline
    if ($null -eq $response.PSObject.Properties['value']) { throw 'WebDriver script response has no value.' }
    return $response.value
}

function Wait-WebDriverOracle {
    param([string]$SessionId,[string]$Script,[datetime]$Deadline,[scriptblock]$Predicate,[string]$Kind)
    do {
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $value = Invoke-WebDriverScript -SessionId $SessionId -Script $Script -Deadline $Deadline
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $matched = & $Predicate $value
        if ([datetime]::UtcNow -ge $Deadline) { break }
        if ($matched) { return $value }
        Start-Sleep -Milliseconds 200
    } while ([datetime]::UtcNow -lt $Deadline)
    throw "$Kind did not become true before the shared launch deadline."
}

function Get-BoundedWebDriverErrorCode {
    param([Exception]$Exception)
    $webException = $null
    $candidate = $Exception
    for ($depth = 0; $depth -lt 8 -and $null -ne $candidate; $depth++) {
        if ($candidate -is [Net.WebException]) { $webException = $candidate; break }
        $candidate = $candidate.InnerException
    }
    if ($null -eq $webException -or $null -eq $webException.Response) { return 'unavailable' }
    $response = $webException.Response
    $stream = $null
    $memory = $null
    try {
        if ($response.ContentLength -gt $script:LaunchPins.WebDriverErrorBytesMaximum) { return 'unavailable' }
        $stream = $response.GetResponseStream()
        if ($null -eq $stream) { return 'unavailable' }
        $memory = [IO.MemoryStream]::new()
        $buffer = [byte[]]::new(1024)
        while (($read = $stream.Read($buffer, 0, $buffer.Length)) -gt 0) {
            if ($memory.Length + $read -gt $script:LaunchPins.WebDriverErrorBytesMaximum) { return 'unavailable' }
            $memory.Write($buffer, 0, $read)
        }
        $text = [Text.UTF8Encoding]::new($false, $true).GetString($memory.ToArray())
        $body = $text | ConvertFrom-Json
        $errorProperty = $body.PSObject.Properties['value']?.Value?.PSObject.Properties['error']
        if ($null -eq $errorProperty -or $errorProperty.Value -isnot [string]) { return 'other' }
        $code = [string]$errorProperty.Value
        if (@('javascript error','unknown error','script timeout','stale element reference','no such window','invalid session id','unexpected alert open') -ccontains $code) { return $code }
        return 'other'
    } catch {
        return 'unavailable'
    } finally {
        if ($null -ne $memory) { $memory.Dispose() }
        if ($null -ne $stream) { $stream.Dispose() }
        $response.Dispose()
    }
}

function Invoke-OcrWebDriverScript {
    param(
        [string]$SessionId,
        [string]$Script,
        [datetime]$Deadline,
        [ValidateSet('menu-open','menu-ready','recognition-start','result-poll','dialog-close','dialog-closed','source-preservation')][string]$Stage
    )
    try {
        return Invoke-WebDriverScript -SessionId $SessionId -Script $Script -Deadline $Deadline
    } catch {
        $errorCode = Get-BoundedWebDriverErrorCode -Exception $_.Exception
        throw "Installed current-page OCR WebDriver stage failed: $Stage; w3cError=$errorCode."
    }
}

function Wait-OcrWebDriverOracle {
    param([string]$SessionId,[string]$Script,[datetime]$Deadline,[scriptblock]$Predicate,[string]$Kind,[string]$Stage)
    do {
        $value = Invoke-OcrWebDriverScript -SessionId $SessionId -Script $Script -Deadline $Deadline -Stage $Stage
        if (& $Predicate $value) { return $value }
        Start-Sleep -Milliseconds 200
    } while ([datetime]::UtcNow -lt $Deadline)
    throw "$Kind did not become true before the shared launch deadline."
}

function Get-LaunchOcrTextReceipt {
    param([Parameter(Mandatory = $true)][string]$Text,[Parameter(Mandatory = $true)][int]$ReportedUtf8Bytes)
    $bytes = [Text.Encoding]::UTF8.GetBytes($Text)
    if ([string]::IsNullOrWhiteSpace($Text) -or $bytes.Length -le 0 -or $bytes.Length -gt $script:LaunchPins.OcrUiTextBytesMaximum -or $ReportedUtf8Bytes -ne $bytes.Length) {
        throw 'Current-page OCR text was empty, oversized, or changed across the bounded WebDriver response.'
    }
    return [pscustomobject]@{
        utf8Bytes = $bytes.Length
        sha256 = [Convert]::ToHexString([Security.Cryptography.SHA256]::HashData($bytes))
    }
}

function Get-LaunchProcessSnapshot {
    $names = @('pdf-workstation','tauri-driver','msedgedriver','msedgewebview2','tesseract')
    return @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $names.Contains($_.ProcessName.ToLowerInvariant()) })
}

function Assert-FixedWebDriverPortsFree {
    $network = [Net.NetworkInformation.IPGlobalProperties]::GetIPGlobalProperties()
    $ports = @($script:LaunchPins.WebDriverPort,$script:LaunchPins.NativeDriverPort)
    if (@($network.GetActiveTcpListeners() | Where-Object { $ports.Contains($_.Port) }).Count -ne 0 -or
        @($network.GetActiveTcpConnections() | Where-Object { $ports.Contains($_.LocalEndPoint.Port) -or $ports.Contains($_.RemoteEndPoint.Port) }).Count -ne 0) {
        throw 'A fixed loopback WebDriver port is already in use.'
    }
}

function Get-OwnedLaunchProcesses {
    param(
        [Parameter(Mandatory = $true)][int]$RootProcessId,
        [Parameter(Mandatory = $true)][datetime]$StartedAfter,
        [scriptblock]$CimProvider,
        [scriptblock]$ProcessProvider
    )
    $all = @(if ($CimProvider) { & $CimProvider } else { Get-CimInstance Win32_Process -ErrorAction Stop })
    $rootRecords = @($all | Where-Object { [uint32]$_.ProcessId -eq [uint32]$RootProcessId })
    if ($rootRecords.Count -ne 1) { throw 'The owned tauri-driver process was absent from the process snapshot.' }
    $rootRecord = $rootRecords[0]
    $rootProcess = if ($ProcessProvider) { & $ProcessProvider $RootProcessId } else { Get-Process -Id $RootProcessId -ErrorAction Stop }
    if ($null -eq $rootProcess -or [uint32]$rootProcess.Id -ne [uint32]$RootProcessId) { throw 'The owned tauri-driver process identity changed during capture.' }
    try {
        $rootCimTicks = ([datetime]$rootRecord.CreationDate).ToUniversalTime().Ticks
        $rootLiveTicks = ([datetime]$rootProcess.StartTime).ToUniversalTime().Ticks
    } catch {
        throw 'The owned tauri-driver process timestamps were unavailable during capture.'
    }
    $tolerance = [int64]$script:LaunchPins.ProcessTimestampToleranceTicks
    $rootDifference = if ($rootLiveTicks -ge $rootCimTicks) { $rootLiveTicks - $rootCimTicks } else { $rootCimTicks - $rootLiveTicks }
    if ($tolerance -ne 10 -or $rootDifference -gt $tolerance) { throw 'The owned tauri-driver process identity changed during capture.' }
    if ($rootLiveTicks + $tolerance -lt $StartedAfter.ToUniversalTime().Ticks) { throw 'The owned tauri-driver process predates its launch boundary.' }
    $owned = [Collections.Generic.HashSet[uint32]]::new()
    $null = $owned.Add([uint32]$RootProcessId)
    $ownedStartTicks = @{}
    $ownedStartTicks[[uint32]$RootProcessId] = $rootCimTicks
    do {
        $added = $false
        foreach ($process in $all) {
            $processId = [uint32]$process.ProcessId
            $parentProcessId = [uint32]$process.ParentProcessId
            if ($owned.Contains($processId) -or -not $owned.Contains($parentProcessId)) { continue }
            try { $candidateTicks = ([datetime]$process.CreationDate).ToUniversalTime().Ticks } catch { throw 'A launch descendant timestamp was unavailable during capture.' }
            if ($candidateTicks + $tolerance -lt [int64]$ownedStartTicks[$parentProcessId] -or $candidateTicks + $tolerance -lt $rootCimTicks) { continue }
            if ($owned.Add($processId)) {
                $ownedStartTicks[$processId] = $candidateTicks
                $added = $true
            }
        }
    } while ($added)
    return @($all | Where-Object { $owned.Contains([uint32]$_.ProcessId) -and [uint32]$_.ProcessId -ne [uint32]$RootProcessId } | ForEach-Object {
        $process = if ($ProcessProvider) { & $ProcessProvider ([int]$_.ProcessId) } else { Get-Process -Id ([int]$_.ProcessId) -ErrorAction Stop }
        if ($null -eq $process -or [uint32]$process.Id -ne [uint32]$_.ProcessId) { throw 'A launch descendant identity changed during capture.' }
        try {
            $cimTicks = ([datetime]$_.CreationDate).ToUniversalTime().Ticks
            $liveTicks = ([datetime]$process.StartTime).ToUniversalTime().Ticks
        } catch {
            throw 'A launch descendant timestamp was unavailable during capture.'
        }
        $difference = if ($liveTicks -ge $cimTicks) { $liveTicks - $cimTicks } else { $cimTicks - $liveTicks }
        if ($difference -gt $tolerance -or $liveTicks + $tolerance -lt $rootLiveTicks) { throw 'A launch descendant identity changed during capture.' }
        [pscustomobject]@{ ProcessId = $process.Id; ParentProcessId = [int]$_.ParentProcessId; Path = [string]$process.Path; StartTicks = $liveTicks; CommandLine = [string]$_.CommandLine }
    })
}

function Wait-FixedWebDriverPortsFree {
    param(
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$ProbeProvider,
        [scriptblock]$SleepProvider,
        [scriptblock]$UtcNowProvider
    )
    $pollMilliseconds = [int]$script:LaunchPins.CleanupProcessPollMilliseconds
    if ($pollMilliseconds -le 0 -or $pollMilliseconds -gt 1000) { throw 'The fixed-port handoff poll bound is invalid.' }
    while ($true) {
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { throw 'Fixed loopback WebDriver ports did not become free before the handoff deadline.' }
        $portsFree = $false
        try {
            if ($ProbeProvider) { $null = & $ProbeProvider } else { Assert-FixedWebDriverPortsFree }
            $portsFree = $true
        } catch {
            if ($_.Exception.Message -cne 'A fixed loopback WebDriver port is already in use.') { throw }
        }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { throw 'Fixed loopback WebDriver ports did not become free before the handoff deadline.' }
        if ($portsFree) { return 'free' }
        $remainingMilliseconds = [int][Math]::Ceiling(($Deadline - $now).TotalMilliseconds)
        if ($remainingMilliseconds -le 0) { throw 'Fixed loopback WebDriver ports did not become free before the handoff deadline.' }
        $sleepMilliseconds = [Math]::Min($pollMilliseconds,$remainingMilliseconds)
        if ($SleepProvider) { $null = & $SleepProvider $sleepMilliseconds } else { Start-Sleep -Milliseconds $sleepMilliseconds }
    }
}

function Wait-LaunchProcessQuiescence {
    param([Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$SnapshotProvider)
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Installed application cleanup quiescence deadline expired before its first process probe.' }
    $emptySnapshots = 0
    $latest = @()
    while ([datetime]::UtcNow -lt $Deadline) {
        $latest = @(if ($SnapshotProvider) { & $SnapshotProvider } else { Get-LaunchProcessSnapshot })
        if ($latest.Count -eq 0) {
            $emptySnapshots++
            if ($emptySnapshots -ge 2) {
                if ([datetime]::UtcNow -lt $Deadline) { return [pscustomobject]@{ stable = $true; processes = @() } }
                break
            }
        } else {
            $emptySnapshots = 0
        }
        $remainingMilliseconds = [int][Math]::Floor(($Deadline - [datetime]::UtcNow).TotalMilliseconds)
        if ($remainingMilliseconds -le 0) { break }
        Start-Sleep -Milliseconds ([Math]::Min($script:LaunchPins.CleanupProcessPollMilliseconds,$remainingMilliseconds))
    }
    return [pscustomobject]@{ stable = $false; processes = @($latest) }
}

function Get-MinimalWindowsProcessEnvironment {
    $names = @('COMSPEC','SystemRoot','WINDIR','LOCALAPPDATA','APPDATA','USERPROFILE','TEMP','TMP','PATH','PATHEXT')
    $environment = [ordered]@{}
    foreach ($name in $names) {
        $value = [Environment]::GetEnvironmentVariable($name,[EnvironmentVariableTarget]::Process)
        if ([string]::IsNullOrWhiteSpace($value) -or $value.IndexOfAny([char[]]@([char]0,[char]10,[char]13)) -ge 0 -or $value.Length -gt 32767) {
            throw "The required minimal child-process environment variable $name was unavailable or invalid."
        }
        $environment[$name] = $value
    }
    return $environment
}

function Start-BoundedDiscardProcess {
    param([string]$Path,[string[]]$Arguments,[int]$MaximumCharacters = 65536,[Collections.IDictionary]$Environment)
    $environmentNames = [string[]]@(); $environmentValues = [string[]]@(); $replaceEnvironment = $null -ne $Environment
    if ($replaceEnvironment) {
        $allowed = @('COMSPEC','SystemRoot','WINDIR','LOCALAPPDATA','APPDATA','USERPROFILE','TEMP','TMP','PATH','PATHEXT')
        $actual = @($Environment.Keys | ForEach-Object { [string]$_ })
        if ($actual.Count -ne $allowed.Count -or (Compare-Object ($allowed | Sort-Object -CaseSensitive) ($actual | Sort-Object -CaseSensitive) -CaseSensitive)) {
            throw 'The child-process environment did not match the exact minimal allowlist.'
        }
        $totalCharacters = 0
        foreach ($name in $allowed) {
            $value = [string]$Environment[$name]
            if ([string]::IsNullOrWhiteSpace($value) -or $value.IndexOfAny([char[]]@([char]0,[char]10,[char]13)) -ge 0 -or $value.Length -gt 32767) {
                throw 'A minimal child-process environment value was unavailable or invalid.'
            }
            $totalCharacters += $name.Length + $value.Length
            $environmentNames += $name; $environmentValues += $value
        }
        if ($totalCharacters -gt 65536) { throw 'The minimal child-process environment exceeded its character cap.' }
    }
    if (-not ('BoundedDiscardProcess' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Diagnostics;
using System.Threading;
public sealed class BoundedDiscardProcess : IDisposable {
  public Process Process { get; }
  private long characters;
  private int exceeded;
  public bool Exceeded { get { return Volatile.Read(ref exceeded) != 0; } }
  private readonly int maximum;
  public BoundedDiscardProcess(string path, string[] arguments, int maximumCharacters, string[] environmentNames, string[] environmentValues, bool replaceEnvironment) {
    maximum = maximumCharacters;
    var start = new ProcessStartInfo { FileName = path, UseShellExecute = false, CreateNoWindow = true, RedirectStandardOutput = true, RedirectStandardError = true };
    if (replaceEnvironment) {
      start.Environment.Clear();
      for (var index = 0; index < environmentNames.Length; index++) start.Environment[environmentNames[index]] = environmentValues[index];
    }
    foreach (var argument in arguments) start.ArgumentList.Add(argument);
    Process = new Process { StartInfo = start };
    DataReceivedEventHandler count = (_, e) => { if (e.Data != null && Interlocked.Add(ref characters, e.Data.Length + 1) > maximum) Interlocked.Exchange(ref exceeded, 1); };
    Process.OutputDataReceived += count;
    Process.ErrorDataReceived += count;
  }
  public void Start() { if (!Process.Start()) throw new InvalidOperationException("Process did not start."); Process.BeginOutputReadLine(); Process.BeginErrorReadLine(); }
  public void Dispose() { Process.Dispose(); }
}
'@
    }
    return [BoundedDiscardProcess]::new($Path, $Arguments, $MaximumCharacters, $environmentNames, $environmentValues, $replaceEnvironment)
}

function Wait-BoundedOwnedProcessExit {
    param([Parameter(Mandatory = $true)]$Process,[Parameter(Mandatory = $true)][datetime]$Deadline)
    if ($Process.HasExited) { return 'already-exited' }
    if ([datetime]::UtcNow -ge $Deadline) { return 'deadline' }
    $killKind = 'tree-kill'
    try {
        $Process.Kill($true)
    } catch {
        $killKind = 'fallback-kill'
        try { $Process.Kill($false) } catch { return 'kill-failed' }
    }
    while (-not $Process.HasExited -and [datetime]::UtcNow -lt $Deadline) {
        $remaining = [int][Math]::Floor(($Deadline - [datetime]::UtcNow).TotalMilliseconds)
        if ($remaining -le 0) { break }
        try {
            if ($Process.WaitForExit([Math]::Min($remaining, 250))) { break }
        } catch { return 'wait-failed' }
    }
    if (-not $Process.HasExited) { return 'deadline' }
    if ($killKind -ceq 'fallback-kill') { return 'fallback-exited' }
    return 'tree-kill-exited'
}

function Get-LaunchProcessCategory {
    param([string]$Name)
    $leaf = [IO.Path]::GetFileNameWithoutExtension($Name).ToLowerInvariant()
    switch ($leaf) {
        'pdf-workstation' { return 'application' }
        'tauri-driver' { return 'tauri-driver' }
        'msedgedriver' { return 'edge-driver' }
        'msedgewebview2' { return 'webview' }
        'tesseract' { return 'ocr-engine' }
        default { return 'other' }
    }
}

function Get-CapturedCategoryOutcome {
    param([Collections.Generic.List[bool]]$States)
    if ($null -eq $States -or $States.Count -eq 0) { return 'absent' }
    if (@($States | Where-Object { -not $_ }).Count -eq 0) { return 'all-exited' }
    return 'incomplete'
}

function Stop-OwnedLaunchProcesses {
    param([Diagnostics.Process]$TauriDriver,[object[]]$Captured,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$ProcessProvider)
    $rootOutcome = 'not-invoked'
    if ($null -ne $TauriDriver) {
        $rootOutcome = Wait-BoundedOwnedProcessExit -Process $TauriDriver -Deadline $Deadline
    }
    $states = [ordered]@{}
    foreach ($category in @('application','tauri-driver','edge-driver','webview','ocr-engine','other')) {
        $states[$category] = [Collections.Generic.List[bool]]::new()
    }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($owned in @($Captured | Sort-Object ProcessId -Descending)) {
        $identity = "{0}`0{1}`0{2}" -f ([int]$owned.ProcessId),([string]$owned.Path),([int64]$owned.StartTicks)
        if (-not $seen.Add($identity)) { continue }
        $category = Get-LaunchProcessCategory -Name ([string]$owned.Path)
        $complete = $false
        try {
            $process = if ($ProcessProvider) { & $ProcessProvider ([int]$owned.ProcessId) } else { Get-Process -Id ([int]$owned.ProcessId) -ErrorAction Stop }
            $path = [string]$process.Path
            $started = $process.StartTime.ToUniversalTime().Ticks
            if ($path.Equals([string]$owned.Path, [StringComparison]::OrdinalIgnoreCase) -and $started -eq [int64]$owned.StartTicks) {
                $outcome = Wait-BoundedOwnedProcessExit -Process $process -Deadline $Deadline
                $complete = @('already-exited','tree-kill-exited','fallback-exited').Contains($outcome)
            }
        } catch {
            $complete = -not $ProcessProvider -and [string]$_.FullyQualifiedErrorId -like 'NoProcessFoundForGivenId*'
        }
        $states[$category].Add($complete)
    }
    return [pscustomobject]@{
        rootOutcome = $rootOutcome
        application = Get-CapturedCategoryOutcome -States $states.application
        tauriDriver = Get-CapturedCategoryOutcome -States $states.'tauri-driver'
        edgeDriver = Get-CapturedCategoryOutcome -States $states.'edge-driver'
        webview = Get-CapturedCategoryOutcome -States $states.webview
        ocrEngine = Get-CapturedCategoryOutcome -States $states.'ocr-engine'
        other = Get-CapturedCategoryOutcome -States $states.other
    }
}

function Get-LaunchResidualCategory {
    param([object[]]$Processes)
    $items = @($Processes)
    if ($items.Count -eq 0) { return 'none' }
    if ($items.Count -ne 1) { return 'multiple' }
    switch ([string]$items[0].ProcessName.ToLowerInvariant()) {
        'pdf-workstation' { return 'application' }
        'tauri-driver' { return 'tauri-driver' }
        'msedgedriver' { return 'edge-driver' }
        'msedgewebview2' { return 'webview' }
        'tesseract' { return 'ocr-engine' }
        default { return 'multiple' }
    }
}

function Get-LaunchResidualFacts {
    param([object[]]$Processes,[object[]]$Captured)
    $presence = [ordered]@{ application = $false; tauriDriver = $false; edgeDriver = $false; webview = $false; ocrEngine = $false; other = $false }
    $items = @($Processes)
    $matched = 0
    foreach ($process in $items) {
        $category = Get-LaunchProcessCategory -Name ([string]$process.ProcessName)
        switch ($category) {
            'application' { $presence.application = $true }
            'tauri-driver' { $presence.tauriDriver = $true }
            'edge-driver' { $presence.edgeDriver = $true }
            'webview' { $presence.webview = $true }
            'ocr-engine' { $presence.ocrEngine = $true }
            default { $presence.other = $true }
        }
        $exact = $false
        try {
            $path = [string]$process.Path
            $started = $process.StartTime.ToUniversalTime().Ticks
            $exact = @($Captured | Where-Object {
                [int]$_.ProcessId -eq [int]$process.Id -and
                ([string]$_.Path).Equals($path,[StringComparison]::OrdinalIgnoreCase) -and
                [int64]$_.StartTicks -eq [int64]$started
            }).Count -gt 0
        } catch { $exact = $false }
        if ($exact) { $matched++ }
    }
    $ownership = if ($items.Count -eq 0) { 'none' }
        elseif ($matched -eq 0) { 'uncaptured' }
        elseif ($matched -eq $items.Count) { 'captured' }
        else { 'mixed' }
    return [pscustomobject]@{
        ownership = $ownership
        application = [bool]$presence.application
        tauriDriver = [bool]$presence.tauriDriver
        edgeDriver = [bool]$presence.edgeDriver
        webview = [bool]$presence.webview
        ocrEngine = [bool]$presence.ocrEngine
        other = [bool]$presence.other
    }
}

function Invoke-SessionDeleteOutcome {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$RequestProvider)
    try {
        $delete = if ($RequestProvider) { & $RequestProvider $SessionId $Deadline } else { Invoke-BoundedLoopbackJson -Method DELETE -Path "/session/$SessionId" -Deadline $Deadline }
        if ($null -ne $delete.PSObject.Properties['value'] -and $null -eq $delete.value) { return 'verified' }
        return 'invalidresponse'
    } catch {
        return 'requestfailed'
    }
}

function Assert-LaunchCleanupState {
    param(
        $Result,
        [ValidateSet('verified','requestfailed','invalidresponse')][string]$SessionDeleteOutcome,
        [bool]$DriverExited,
        [bool]$RelevantProcessesClear,
        [ValidateSet('not-invoked','already-exited','tree-kill-exited','fallback-exited','kill-failed','wait-failed','deadline')][string]$DriverStopOutcome,
        [ValidateSet('none','application','tauri-driver','edge-driver','webview','ocr-engine','multiple')][string]$ResidualCategory,
        [ValidateSet('absent','all-exited','incomplete')][string]$CapturedApplication,
        [ValidateSet('absent','all-exited','incomplete')][string]$CapturedTauriDriver,
        [ValidateSet('absent','all-exited','incomplete')][string]$CapturedEdgeDriver,
        [ValidateSet('absent','all-exited','incomplete')][string]$CapturedWebView,
        [ValidateSet('absent','all-exited','incomplete')][string]$CapturedOcrEngine,
        [ValidateSet('absent','all-exited','incomplete')][string]$CapturedOther,
        [ValidateSet('none','captured','uncaptured','mixed')][string]$ResidualOwnership,
        [bool]$ResidualApplication,
        [bool]$ResidualTauriDriver,
        [bool]$ResidualEdgeDriver,
        [bool]$ResidualWebView,
        [bool]$ResidualOcrEngine,
        [bool]$ResidualOther
    )
    $resultComplete = $null -ne $Result
    $capturedProcessesComplete = @($CapturedApplication,$CapturedTauriDriver,$CapturedEdgeDriver,$CapturedWebView,$CapturedOcrEngine,$CapturedOther) -cnotcontains 'incomplete'
    if ($resultComplete -and $SessionDeleteOutcome -ceq 'verified' -and $DriverExited -and $RelevantProcessesClear -and $capturedProcessesComplete) { return }
    $driverExitedLabel = if ($DriverExited) { 'true' } else { 'false' }
    $relevantProcessesClearLabel = if ($RelevantProcessesClear) { 'true' } else { 'false' }
    $resultCompleteLabel = if ($resultComplete) { 'true' } else { 'false' }
    $residualApplicationLabel = if ($ResidualApplication) { 'true' } else { 'false' }
    $residualTauriDriverLabel = if ($ResidualTauriDriver) { 'true' } else { 'false' }
    $residualEdgeDriverLabel = if ($ResidualEdgeDriver) { 'true' } else { 'false' }
    $residualWebViewLabel = if ($ResidualWebView) { 'true' } else { 'false' }
    $residualOcrEngineLabel = if ($ResidualOcrEngine) { 'true' } else { 'false' }
    $residualOtherLabel = if ($ResidualOther) { 'true' } else { 'false' }
    throw "Installed application launch cleanup state is unverified: sessionDeleteOutcome=$SessionDeleteOutcome;driverExited=$driverExitedLabel;relevantProcessesClear=$relevantProcessesClearLabel;resultComplete=$resultCompleteLabel;driverStopOutcome=$DriverStopOutcome;residualCategory=$ResidualCategory;captured=application:$CapturedApplication,tauri-driver:$CapturedTauriDriver,edge-driver:$CapturedEdgeDriver,webview:$CapturedWebView,ocr-engine:$CapturedOcrEngine,other:$CapturedOther;residualOwnership=$ResidualOwnership;residualPresence=application:$residualApplicationLabel,tauri-driver:$residualTauriDriverLabel,edge-driver:$residualEdgeDriverLabel,webview:$residualWebViewLabel,ocr-engine:$residualOcrEngineLabel,other:$residualOtherLabel."
}

function Invoke-RealInstalledAppLaunch {
    param([string]$ApplicationPath,[string]$TauriDriverPath,[string]$EdgeDriverPath,[string]$OcrEnginePath,[string]$ProfileRoot,[string]$SettingsRoot,[string]$ExpectedEdgeDriverVersion)
    $deadline = [datetime]::UtcNow.AddMilliseconds($script:LaunchPins.TotalTimeoutMilliseconds)
    $driverCapture = $null
    $driver = $null
    $sessionId = $null
    $captured = @()
    $startedAfter = [datetime]::UtcNow
    $result = $null
    $sessionDeleted = $false
    $sessionDeleteOutcome = 'requestfailed'
    $ownedStopped = $false
    $remaining = -1
    $driverExited = $false
    $driverStopOutcome = 'not-invoked'
    $processesQuiescent = $false
    $residualCategory = 'multiple'
    $capturedOutcomes = [pscustomobject]@{ application = 'absent'; tauriDriver = 'absent'; edgeDriver = 'absent'; webview = 'absent'; ocrEngine = 'absent'; other = 'absent' }
    $residualFacts = [pscustomobject]@{ ownership = 'none'; application = $false; tauriDriver = $false; edgeDriver = $false; webview = $false; ocrEngine = $false; other = $false }
    try {
        Assert-FixedWebDriverPortsFree
        $driverCapture = Start-BoundedDiscardProcess -Path $TauriDriverPath -Arguments @(
            "--port=$($script:LaunchPins.WebDriverPort)",
            "--native-port=$($script:LaunchPins.NativeDriverPort)",
            "--native-driver=$EdgeDriverPath"
        ) -Environment (Get-MinimalWindowsProcessEnvironment)
        $driverCapture.Start()
        $driver = $driverCapture.Process
        do {
            try {
                $status = Invoke-BoundedLoopbackJson -Method GET -Path '/status' -Deadline $deadline
                if ($null -ne $status.value -and [bool]$status.value.ready) { break }
            } catch { }
            if ($driver.HasExited) { throw 'Pinned tauri-driver exited before readiness.' }
            Start-Sleep -Milliseconds 200
        } while ([datetime]::UtcNow -lt $deadline)
        if ($null -eq $status -or -not [bool]$status.value.ready) { throw 'Pinned tauri-driver did not become ready.' }
        if ($driver.HasExited) { throw 'Pinned tauri-driver exited after reporting readiness.' }
        $nativeDriverVersion = Wait-NativeDriverStatus -ExpectedVersion $ExpectedEdgeDriverVersion -Deadline $deadline -TauriDriver $driver
        if ($driver.HasExited) { throw 'Pinned tauri-driver exited while binding the native EdgeDriver status.' }

        $sessionBody = [ordered]@{ capabilities = [ordered]@{ alwaysMatch = [ordered]@{
            browserName = 'wry'
            'tauri:options' = [ordered]@{ application = $ApplicationPath; args = @(); webviewOptions = [ordered]@{ userDataFolder = $ProfileRoot } }
        } } }
        $session = Invoke-BoundedLoopbackJson -Method POST -Path '/session' -Body $sessionBody -Deadline $deadline
        if ($null -eq $session.value -or [string]$session.value.sessionId -cnotmatch '^[A-Za-z0-9-]+$') { throw 'WebDriver did not return a bounded session identifier.' }
        if ($driver.HasExited) { throw 'Pinned tauri-driver exited during session creation.' }
        $sessionId = [string]$session.value.sessionId
        $capabilities = $session.value.capabilities
        $capabilityKeys = @($capabilities.PSObject.Properties.Name | Sort-Object -CaseSensitive)
        if ($capabilityKeys.Count -eq 0 -or $capabilityKeys.Count -gt 32 -or @($capabilityKeys | Where-Object { $_ -cnotmatch '^[A-Za-z0-9:._-]{1,64}$' }).Count -ne 0) {
            throw 'WebDriver returned an unsafe capability-key set.'
        }
        $vendorDriverProperty = $capabilities.PSObject.Properties['msedge.msedgedriverVersion']
        if ($null -ne $vendorDriverProperty -and ([string]$vendorDriverProperty.Value -split '\s+')[0] -cne $nativeDriverVersion) {
            throw 'The optional session EdgeDriver version disagrees with the live native status.'
        }
        $returnedRuntimeVersion = [string]$capabilities.browserVersion
        $userDataProperty = $capabilities.PSObject.Properties['msedge.userDataDir']
        $returnedUserData = if ($null -ne $userDataProperty) { [string]$userDataProperty.Value } else { '' }

        $homeScript = "return {ready:document.readyState==='complete',title:document.title,home:[...document.querySelectorAll('button')].some(b=>b.textContent.trim()==='Explore a sample PDF')};"
        $homeOracle = Wait-WebDriverOracle -SessionId $sessionId -Script $homeScript -Deadline $deadline -Kind 'Home UI' -Predicate { param($v) [bool]$v.ready -and [string]$v.title -ceq 'PDF Workstation' -and [bool]$v.home }
        $ocrScript = "const b=[...document.querySelectorAll('button')].find(x=>x.textContent.trim()==='All tools');if(b)b.click();const o=[...document.querySelectorAll('button')].find(x=>x.textContent.trim()==='Scan & OCR');const s=o?.nextElementSibling;return {heading:[...document.querySelectorAll('h1')].some(x=>x.textContent.trim()==='All tools'),status:s?.tagName==='SPAN'&&o?.parentElement?.lastElementChild===s?s.textContent.trim():''};"
        $ocr = Wait-WebDriverOracle -SessionId $sessionId -Script $ocrScript -Deadline $deadline -Kind 'Native OCR capability UI' -Predicate { param($v) [bool]$v.heading -and [string]$v.status -ceq 'Available' }
        $homeClick = Invoke-WebDriverScript -SessionId $sessionId -Script "const a=[...document.querySelectorAll('button')].filter(x=>x.getAttribute('aria-label')==='Home'||x.textContent.trim()==='Home');if(a.length===1)a[0].click();return a.length===1;" -Deadline $deadline
        if ($homeClick -isnot [bool] -or -not $homeClick) { throw 'The exact Home control was unavailable.' }
        $null = Wait-WebDriverOracle -SessionId $sessionId -Script "return [...document.querySelectorAll('button')].some(x=>x.textContent.trim()==='Explore a sample PDF');" -Deadline $deadline -Kind 'Home sample control' -Predicate { param($v) $v -is [bool] -and $v }
        $sampleClick = "const b=[...document.querySelectorAll('button')].find(x=>x.textContent.trim()==='Explore a sample PDF');if(b)b.click();return !!b;"
        $clicked = Invoke-WebDriverScript -SessionId $sessionId -Script $sampleClick -Deadline $deadline
        if ($clicked -isnot [bool] -or -not $clicked) { throw 'The installed sample-open button was unavailable.' }
        $sampleScript = @'
const i=document.querySelector('img[alt="Page 1"]');return {tab:[...document.querySelectorAll('button')].some(x=>x.textContent.includes('welcome.pdf')),pages:[...document.querySelectorAll('span')].some(x=>x.textContent.trim()==='/ 6'),footer:[...document.querySelectorAll('footer span')].some(x=>x.textContent.trim()==='welcome.pdf · Source preserved'),image:!!i&&i.complete&&i.naturalWidth>0&&i.naturalHeight>0&&i.src.startsWith('blob:'),width:i?.naturalWidth||0,height:i?.naturalHeight||0};
'@
        $sample = Wait-WebDriverOracle -SessionId $sessionId -Script $sampleScript -Deadline $deadline -Kind 'Installed sample PDF render' -Predicate { param($v) [bool]$v.tab -and [bool]$v.pages -and [bool]$v.footer -and [bool]$v.image -and [int]$v.width -gt 0 -and [int]$v.height -gt 0 }
        $menuClick = Invoke-OcrWebDriverScript -SessionId $sessionId -Script "const m=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Menu'&&x.getAttribute('aria-expanded')==='false');if(m.length===1)m[0].click();return m.length===1;" -Deadline $deadline -Stage 'menu-open'
        if ($menuClick -isnot [bool] -or -not $menuClick) { throw 'The exact closed Menu control was unavailable before current-page OCR.' }
        $ocrControl = Wait-OcrWebDriverOracle -SessionId $sessionId -Script "const m=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Menu'&&x.getAttribute('aria-expanded')==='true'),p=m.length===1?m[0].closest('header')?.nextElementSibling:null,o=p?[...p.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Recognize current page…'):[];return {menu:m.length===1&&!!p,count:o.length,enabled:o.length===1&&!o[0].disabled,title:o.length===1?o[0].title:''};" -Deadline $deadline -Kind 'Current-page OCR menu control' -Stage 'menu-ready' -Predicate { param($v) [bool]$v.menu -and [int]$v.count -eq 1 -and [bool]$v.enabled -and [string]$v.title -ceq 'Recognize text on the current page in English' }
        $ocrClick = Invoke-OcrWebDriverScript -SessionId $sessionId -Script "const m=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Menu'&&x.getAttribute('aria-expanded')==='true'),p=m.length===1?m[0].closest('header')?.nextElementSibling:null,o=p?[...p.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Recognize current page…'&&!x.disabled):[];if(m.length===1&&p&&o.length===1)o[0].click();return m.length===1&&!!p&&o.length===1;" -Deadline $deadline -Stage 'recognition-start'
        if ($ocrClick -isnot [bool] -or -not $ocrClick) { throw 'The exact enabled current-page OCR control was unavailable.' }
        $ocrResultScript = @'
const dialogs=[...document.querySelectorAll('dialog[aria-labelledby="page-ocr-title"]')];if(dialogs.length!==1)return {state:'waiting',title:'',context:false,readOnly:false,aria:'',utf8Bytes:0,text:''};const d=dialogs[0],title=d.querySelector('#page-ocr-title')?.textContent.trim()||'',paragraphs=[...d.querySelectorAll('p')].map(x=>x.textContent.trim()),context=paragraphs.includes('welcome.pdf · physical page 1. OCR reads this one page in English. Recognition may contain errors.')&&paragraphs.includes('It does not change the PDF, add searchable text, index the document, or send content to a service.'),closeReady=[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Close'&&!x.disabled).length===1;if(d.querySelector('[role="alert"]'))return {state:'error',title,context,readOnly:false,aria:'',utf8Bytes:0,text:''};if(paragraphs.includes('No text was recognized on this page.'))return {state:'empty',title,context,readOnly:false,aria:'',utf8Bytes:0,text:''};const t=d.querySelector('textarea[aria-label="Recognized text on page 1"]');if(!t||!closeReady)return {state:'running',title,context,readOnly:false,aria:'',utf8Bytes:0,text:''};const text=t.value,utf8Bytes=new TextEncoder().encode(text).length;if(utf8Bytes>65536)return {state:'overflow',title,context,readOnly:t.readOnly,aria:t.getAttribute('aria-label')||'',utf8Bytes,text:''};return {state:'recognized',title,context,readOnly:t.readOnly,aria:t.getAttribute('aria-label')||'',utf8Bytes,text};
'@
        $ocrResult = $null
        do {
            $ocrResult = Invoke-OcrWebDriverScript -SessionId $sessionId -Script $ocrResultScript -Deadline $deadline -Stage 'result-poll'
            try { $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured) } catch { }
            if ([string]$ocrResult.state -in @('error','empty','overflow')) { throw 'Installed current-page OCR did not produce bounded recognized text.' }
            if ([string]$ocrResult.state -ceq 'recognized') { break }
            Start-Sleep -Milliseconds 200
        } while ([datetime]::UtcNow -lt $deadline)
        if ($null -eq $ocrResult -or [string]$ocrResult.state -cne 'recognized' -or [string]$ocrResult.title -cne 'Recognize text on page 1' -or
            $ocrResult.context -isnot [bool] -or -not $ocrResult.context -or $ocrResult.readOnly -isnot [bool] -or -not $ocrResult.readOnly -or
            [string]$ocrResult.aria -cne 'Recognized text on page 1') {
            throw 'Installed current-page OCR did not satisfy the exact signed UI receipt contract.'
        }
        $ocrTextReceipt = Get-LaunchOcrTextReceipt -Text ([string]$ocrResult.text) -ReportedUtf8Bytes ([int]$ocrResult.utf8Bytes)
        $ocrResult.text = ''
        $closeOcrScript = @'
const d=[...document.querySelectorAll('dialog[aria-labelledby="page-ocr-title"]')];const b=d.length===1?[...d[0].querySelectorAll('button')].filter(x=>x.textContent.trim()==='Close'&&!x.disabled):[];if(d.length===1&&b.length===1)setTimeout(()=>b[0].click(),0);return d.length===1&&b.length===1;
'@
        $closeOcr = Invoke-OcrWebDriverScript -SessionId $sessionId -Script $closeOcrScript -Deadline $deadline -Stage 'dialog-close'
        if ($closeOcr -isnot [bool] -or -not $closeOcr) { throw 'The completed current-page OCR dialog could not be closed exactly.' }
        $dialogClosedScript = @'
return document.querySelectorAll('dialog[aria-labelledby="page-ocr-title"]').length===0;
'@
        $null = Wait-OcrWebDriverOracle -SessionId $sessionId -Script $dialogClosedScript -Deadline $deadline -Kind 'Current-page OCR dialog close' -Stage 'dialog-closed' -Predicate { param($v) $v -is [bool] -and $v }
        $postOcrScript = @'
const tabs=[...document.querySelectorAll('button')].filter(x=>{const s=x.querySelector('span');return s&&s.textContent.trim()==='welcome.pdf'}),i=document.querySelector('img[alt="Page 1"]');return {tab:tabs.length===1,pages:[...document.querySelectorAll('span')].some(x=>x.textContent.trim()==='/ 6'),footer:[...document.querySelectorAll('footer span')].some(x=>x.textContent.trim()==='welcome.pdf · Source preserved'),image:!!i&&i.complete&&i.naturalWidth>0&&i.naturalHeight>0&&i.src.startsWith('blob:')};
'@
        $postOcr = Wait-OcrWebDriverOracle -SessionId $sessionId -Script $postOcrScript -Deadline $deadline -Kind 'Post-OCR sample preservation' -Stage 'source-preservation' -Predicate { param($v) [bool]$v.tab -and [bool]$v.pages -and [bool]$v.footer -and [bool]$v.image }
        $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter)
        $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
        Assert-OwnedLaunchExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -OcrEnginePath $OcrEnginePath -RootProcessId $driver.Id
        $profileBinding = if (-not [string]::IsNullOrWhiteSpace($returnedUserData)) {
            'session-capability-' + (Get-ExactProfileBinding -Candidate $returnedUserData -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot)
        } else {
            Get-OwnedProfileBinding -Owned $captured -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot
        }
        if ($driverCapture.Exceeded) { throw 'WebDriver diagnostic output exceeded its discarded character cap.' }
        $result = [pscustomobject]@{
            nativeDriverVersion = $nativeDriverVersion
            driverVersionBinding = 'native-status'
            returnedRuntimeVersion = $returnedRuntimeVersion
            profileBindingMethod = $profileBinding
            sessionCapabilityKeys = $capabilityKeys
            title = [string]$homeOracle.title
            homeButton = [bool]$homeOracle.home
            ocrCapabilityStatus = [string]$ocr.status
            sampleName = 'welcome.pdf'
            samplePages = 6
            renderedPageWidth = [int]$sample.width
            renderedPageHeight = [int]$sample.height
            renderedPageBlob = [bool]$sample.image
            ocrPage = 1
            ocrDialogTitle = [string]$ocrResult.title
            ocrTextUtf8Bytes = [int]$ocrTextReceipt.utf8Bytes
            ocrTextSha256 = [string]$ocrTextReceipt.sha256
            ocrSourceUiPreserved = [bool]$postOcr.footer
        }
    } finally {
        $cleanupDeadline = [datetime]::UtcNow.AddSeconds(10)
        if ($sessionId) {
            $sessionDeleteOutcome = Invoke-SessionDeleteOutcome -SessionId $sessionId -Deadline $cleanupDeadline
            $sessionDeleted = $sessionDeleteOutcome -ceq 'verified'
        }
        if ($driver) {
            try { $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured) } catch { }
            $processCleanupDeadline = [datetime]::UtcNow.AddMilliseconds($script:LaunchPins.CleanupProcessTimeoutMilliseconds)
            $capturedOutcomes = Stop-OwnedLaunchProcesses -TauriDriver $driver -Captured $captured -Deadline $processCleanupDeadline
            $driverStopOutcome = [string]$capturedOutcomes.rootOutcome
            $quiescence = Wait-LaunchProcessQuiescence -Deadline $processCleanupDeadline
            $processesQuiescent = [bool]$quiescence.stable
            $remainingProcesses = @($quiescence.processes)
            $remaining = $remainingProcesses.Count
            $residualCategory = Get-LaunchResidualCategory -Processes $remainingProcesses
            $residualFacts = Get-LaunchResidualFacts -Processes $remainingProcesses -Captured $captured
            $driverExited = [bool]$driver.HasExited
            $ownedStopped = $driverExited -and $processesQuiescent -and $residualCategory -ceq 'none'
        }
        if ($driverCapture) { $driverCapture.Dispose() }
    }
    $relevantProcessesClear = $processesQuiescent -and $residualCategory -ceq 'none'
    Assert-LaunchCleanupState -Result $result -SessionDeleteOutcome $sessionDeleteOutcome -DriverExited $driverExited -RelevantProcessesClear $relevantProcessesClear -DriverStopOutcome $driverStopOutcome -ResidualCategory $residualCategory -CapturedApplication $capturedOutcomes.application -CapturedTauriDriver $capturedOutcomes.tauriDriver -CapturedEdgeDriver $capturedOutcomes.edgeDriver -CapturedWebView $capturedOutcomes.webview -CapturedOcrEngine $capturedOutcomes.ocrEngine -CapturedOther $capturedOutcomes.other -ResidualOwnership $residualFacts.ownership -ResidualApplication $residualFacts.application -ResidualTauriDriver $residualFacts.tauriDriver -ResidualEdgeDriver $residualFacts.edgeDriver -ResidualWebView $residualFacts.webview -ResidualOcrEngine $residualFacts.ocrEngine -ResidualOther $residualFacts.other
    if ($null -eq $result -or -not $sessionDeleted -or -not $ownedStopped -or $remaining -ne 0) {
        throw 'Installed application launch cleanup did not delete the session and stop the owned process tree.'
    }
    $result | Add-Member -NotePropertyName sessionDeleted -NotePropertyValue $sessionDeleted
    $result | Add-Member -NotePropertyName ownedProcessTreeStopped -NotePropertyValue $ownedStopped
    $result | Add-Member -NotePropertyName relevantProcessesRemaining -NotePropertyValue $remaining
    return $result
}

function Invoke-InstalledAppLaunchSmoke {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,
        [Parameter(Mandatory = $true)]$ApplicationReceipt,
        [Parameter(Mandatory = $true)][string]$OcrEnginePath,
        [Parameter(Mandatory = $true)]$OcrEngineReceipt,
        [Parameter(Mandatory = $true)][string]$WelcomePath,
        [Parameter(Mandatory = $true)]$WelcomeReceipt,
        [Parameter(Mandatory = $true)][string]$WebDriverRoot,
        [Parameter(Mandatory = $true)][string]$ProfileRoot,
        [Parameter(Mandatory = $true)][string]$ApplicationSettingsRoot,
        [Parameter(Mandatory = $true)][string]$SettingsSentinelPath,
        [Parameter(Mandatory = $true)][uint64]$SettingsSentinelBytes,
        [Parameter(Mandatory = $true)][string]$SettingsSentinelSha256,
        [Parameter(Mandatory = $true)][string]$RunnerTemp,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher,
        [scriptblock]$ProcessProvider
    )
    $runner = [IO.Path]::GetFullPath($RunnerTemp).TrimEnd('\')
    $driverRoot = [IO.Path]::GetFullPath($WebDriverRoot).TrimEnd('\')
    $profilePath = [IO.Path]::GetFullPath($ProfileRoot).TrimEnd('\')
    $settingsRoot = [IO.Path]::GetFullPath($ApplicationSettingsRoot).TrimEnd('\')
    $settingsSentinel = [IO.Path]::GetFullPath($SettingsSentinelPath)
    foreach ($candidate in @($driverRoot,$profilePath)) {
        if (-not $candidate.StartsWith($runner + '\',[StringComparison]::OrdinalIgnoreCase)) { throw 'Launch-smoke paths must stay beneath RUNNER_TEMP.' }
        Assert-NoReparseAncestors -Path $candidate
    }
    if (-not (Test-Path -LiteralPath $driverRoot -PathType Container) -or (Test-Path -LiteralPath $profilePath)) { throw 'WebDriver inputs must exist and the WebView profile must be fresh.' }
    $expectedSettingsRoot = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\')
    $expectedSettingsSentinel = [IO.Path]::GetFullPath((Join-Path $settingsRoot 'upgrade-sentinel.json'))
    if (-not $settingsRoot.Equals($expectedSettingsRoot,[StringComparison]::OrdinalIgnoreCase) -or
        -not $settingsSentinel.Equals($expectedSettingsSentinel,[StringComparison]::OrdinalIgnoreCase)) {
        throw 'The launch settings root or sentinel is outside the exact application identity.'
    }
    Assert-NoReparseAncestors -Path $settingsRoot
    Assert-NoReparseAncestors -Path $settingsSentinel
    Assert-FileReceipt -Path $settingsSentinel -Bytes $SettingsSentinelBytes -Sha256 $SettingsSentinelSha256 -Kind 'Prelaunch synthetic settings sentinel'
    $settingsEntries = @(Get-ChildItem -LiteralPath $settingsRoot -Force)
    if ($settingsEntries.Count -ne 1 -or -not $settingsEntries[0].FullName.Equals($settingsSentinel,[StringComparison]::OrdinalIgnoreCase) -or
        $settingsEntries[0].PSIsContainer -or ($settingsEntries[0].Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw 'The controlled application settings root must contain only the receipt-bound sentinel before launch.'
    }
    $applicationProfile = [IO.Path]::GetFullPath((Join-Path $settingsRoot 'EBWebView')).TrimEnd('\')
    $requestedChildProfile = [IO.Path]::GetFullPath((Join-Path $profilePath 'EBWebView')).TrimEnd('\')
    if (Test-Path -LiteralPath $applicationProfile) { throw 'The exact Tauri application WebView profile must be absent before launch.' }
    if (Test-Path -LiteralPath $requestedChildProfile) { throw 'The exact requested WebView child profile must be absent before launch.' }
    $ambient = @(Get-LaunchProcessSnapshot)
    if ($ambient.Count -ne 0) { throw 'The hosted runner already has an application or WebDriver process.' }
    $receiptPath = Join-Path $driverRoot 'webdriver-receipt.json'
    Assert-NoReparseAncestors -Path $receiptPath
    $receipt = Get-Content -LiteralPath $receiptPath -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-WebDriverReceipt -Receipt $receipt
    $tauriDriver = Join-Path $driverRoot 'tauri-driver-install/bin/tauri-driver.exe'
    $edgeDriver = Join-Path $driverRoot 'edge-driver/msedgedriver.exe'
    Assert-FileReceipt -Path $ApplicationPath -Bytes ([uint64]$ApplicationReceipt.bytes) -Sha256 ([string]$ApplicationReceipt.sha256) -Kind 'Installed signed launch application'
    $null = Assert-TrustedWindowsSignature -Path $ApplicationPath -ExpectedPublisher $ExpectedPublisher
    Assert-FileReceipt -Path $OcrEnginePath -Bytes ([uint64]$OcrEngineReceipt.bytes) -Sha256 ([string]$OcrEngineReceipt.sha256) -Kind 'Installed signed launch OCR engine'
    $null = Assert-TrustedWindowsSignature -Path $OcrEnginePath -ExpectedPublisher $ExpectedPublisher
    Assert-FileReceipt -Path $WelcomePath -Bytes ([uint64]$WelcomeReceipt.bytes) -Sha256 ([string]$WelcomeReceipt.sha256) -Kind 'Installed launch sample PDF'
    Assert-FileReceipt -Path $tauriDriver -Bytes ([uint64]$receipt.tauriDriver.bytes) -Sha256 ([string]$receipt.tauriDriver.sha256) -Kind 'Pinned tauri-driver executable'
    Assert-FileReceipt -Path $edgeDriver -Bytes ([uint64]$receipt.edgeDriver.bytes) -Sha256 ([string]$receipt.edgeDriver.sha256) -Kind 'Exact EdgeDriver executable'
    $null = Assert-TrustedWindowsSignature -Path $edgeDriver -ExpectedPublisher $script:LaunchPins.EdgePublisher
    [IO.Directory]::CreateDirectory($profilePath) | Out-Null
    Assert-NoReparseAncestors -Path $profilePath
    if (@(Get-ChildItem -LiteralPath $profilePath -Force).Count -ne 0) { throw 'The requested WebDriver profile was not empty before launch.' }
    $expectedEdgeDriverVersion = [string]$receipt.edgeDriver.version
    $result = if ($ProcessProvider) { & $ProcessProvider $ApplicationPath $tauriDriver $edgeDriver $OcrEnginePath $profilePath $settingsRoot $applicationProfile $expectedEdgeDriverVersion } else { Invoke-RealInstalledAppLaunch -ApplicationPath $ApplicationPath -TauriDriverPath $tauriDriver -EdgeDriverPath $edgeDriver -OcrEnginePath $OcrEnginePath -ProfileRoot $profilePath -SettingsRoot $settingsRoot -ExpectedEdgeDriverVersion $expectedEdgeDriverVersion }
    Assert-LaunchExactProperties -Value $result -Expected @('nativeDriverVersion','driverVersionBinding','returnedRuntimeVersion','profileBindingMethod','sessionCapabilityKeys','title','homeButton','ocrCapabilityStatus','sampleName','samplePages','renderedPageWidth','renderedPageHeight','renderedPageBlob','ocrPage','ocrDialogTitle','ocrTextUtf8Bytes','ocrTextSha256','ocrSourceUiPreserved','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') -Kind 'Installed application launch result'
    $returnedRuntimeParts = ([string]$result.returnedRuntimeVersion).Split('.')
    $expectedRuntimeParts = ([string]$receipt.webView2RuntimeVersion).Split('.')
    $capabilityKeys = @($result.sessionCapabilityKeys)
    if ([string]$result.nativeDriverVersion -cne $expectedEdgeDriverVersion -or [string]$result.driverVersionBinding -cne 'native-status' -or
        $returnedRuntimeParts.Count -ne 4 -or ($returnedRuntimeParts[0..2] -join '.') -cne ($expectedRuntimeParts[0..2] -join '.') -or
        @('session-capability-requested-profile','session-capability-requested-ebwebview','session-capability-tauri-app-settings-ebwebview','owned-webview-requested-profile','owned-webview-requested-ebwebview','owned-webview-tauri-app-settings-ebwebview') -cnotcontains [string]$result.profileBindingMethod -or
        $capabilityKeys.Count -eq 0 -or $capabilityKeys.Count -gt 32 -or @($capabilityKeys | Where-Object { $_ -cnotmatch '^[A-Za-z0-9:._-]{1,64}$' }).Count -ne 0 -or
        [string]$result.title -cne 'PDF Workstation' -or $result.homeButton -isnot [bool] -or -not $result.homeButton -or
        [string]$result.ocrCapabilityStatus -cne 'Available' -or [string]$result.sampleName -cne 'welcome.pdf' -or
        [int]$result.samplePages -ne 6 -or [int]$result.renderedPageWidth -le 0 -or [int]$result.renderedPageHeight -le 0 -or
        $result.renderedPageBlob -isnot [bool] -or -not $result.renderedPageBlob -or
        [int]$result.ocrPage -ne 1 -or [string]$result.ocrDialogTitle -cne 'Recognize text on page 1' -or
        [int]$result.ocrTextUtf8Bytes -le 0 -or [int]$result.ocrTextUtf8Bytes -gt $script:LaunchPins.OcrUiTextBytesMaximum -or
        [string]$result.ocrTextSha256 -cnotmatch '^[A-F0-9]{64}$' -or $result.ocrSourceUiPreserved -isnot [bool] -or -not $result.ocrSourceUiPreserved -or
        $result.sessionDeleted -isnot [bool] -or -not $result.sessionDeleted -or
        $result.ownedProcessTreeStopped -isnot [bool] -or -not $result.ownedProcessTreeStopped -or [int]$result.relevantProcessesRemaining -ne 0) {
        throw 'Installed application launch result did not match its exact native IPC and rendered-sample oracles.'
    }
    if (@(Get-LaunchProcessSnapshot).Count -ne 0) { throw 'Installed application launch cleanup left a relevant process running.' }
    Assert-FileReceipt -Path $OcrEnginePath -Bytes ([uint64]$OcrEngineReceipt.bytes) -Sha256 ([string]$OcrEngineReceipt.sha256) -Kind 'Postlaunch installed signed OCR engine'
    $null = Assert-TrustedWindowsSignature -Path $OcrEnginePath -ExpectedPublisher $ExpectedPublisher
    Assert-FileReceipt -Path $WelcomePath -Bytes ([uint64]$WelcomeReceipt.bytes) -Sha256 ([string]$WelcomeReceipt.sha256) -Kind 'Postlaunch installed sample PDF'
    Assert-NoReparseAncestors -Path $profilePath
    Assert-FileReceipt -Path $settingsSentinel -Bytes $SettingsSentinelBytes -Sha256 $SettingsSentinelSha256 -Kind 'Postlaunch synthetic settings sentinel'
    $usesTauriProfile = ([string]$result.profileBindingMethod).EndsWith('tauri-app-settings-ebwebview',[StringComparison]::Ordinal)
    $usesRequestedChildProfile = ([string]$result.profileBindingMethod).EndsWith('requested-ebwebview',[StringComparison]::Ordinal)
    $requestedProfileEntries = @(Get-ChildItem -LiteralPath $profilePath -Force)
    $postlaunchSettingsEntries = @(Get-ChildItem -LiteralPath $settingsRoot -Force)
    if ($usesTauriProfile) {
        if (-not (Test-Path -LiteralPath $applicationProfile -PathType Container) -or $requestedProfileEntries.Count -ne 0) {
            throw 'The Tauri application profile was not created while the unused requested profile remained empty.'
        }
        Assert-NoReparseAncestors -Path $applicationProfile
        if (@(Get-ChildItem -LiteralPath $applicationProfile -Force).Count -eq 0 -or $postlaunchSettingsEntries.Count -ne 2) {
            throw 'The active Tauri application profile was empty or the controlled settings root gained an unexpected entry.'
        }
        $applicationProfileEntry = @($postlaunchSettingsEntries | Where-Object { $_.FullName.Equals($applicationProfile,[StringComparison]::OrdinalIgnoreCase) })
        if ($applicationProfileEntry.Count -ne 1 -or -not $applicationProfileEntry[0].PSIsContainer -or
            ($applicationProfileEntry[0].Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'The controlled settings root did not contain exactly the expected Tauri application profile.'
        }
    } elseif ($usesRequestedChildProfile) {
        if (-not (Test-Path -LiteralPath $requestedChildProfile -PathType Container) -or
            (Test-Path -LiteralPath $applicationProfile) -or $requestedProfileEntries.Count -ne 1 -or
            $postlaunchSettingsEntries.Count -ne 1 -or
            -not $postlaunchSettingsEntries[0].FullName.Equals($settingsSentinel,[StringComparison]::OrdinalIgnoreCase)) {
            throw 'The requested WebView child profile or controlled settings root changed outside its exact contract.'
        }
        Assert-NoReparseAncestors -Path $requestedChildProfile
        if (@(Get-ChildItem -LiteralPath $requestedChildProfile -Force).Count -eq 0 -or
            -not $requestedProfileEntries[0].FullName.Equals($requestedChildProfile,[StringComparison]::OrdinalIgnoreCase) -or
            -not $requestedProfileEntries[0].PSIsContainer -or
            ($requestedProfileEntries[0].Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'The exact requested WebView child profile was empty, inexact, or reparse-backed.'
        }
    } else {
        if (Test-Path -LiteralPath $applicationProfile) { throw 'The unused Tauri application profile was unexpectedly created.' }
        if ($requestedProfileEntries.Count -eq 0 -or $postlaunchSettingsEntries.Count -ne 1 -or
            -not $postlaunchSettingsEntries[0].FullName.Equals($settingsSentinel,[StringComparison]::OrdinalIgnoreCase)) {
            throw 'The requested profile was empty or the controlled settings root changed unexpectedly.'
        }
    }
    return [ordered]@{
        drivers = [ordered]@{
            tauriDriver = [ordered]@{ version = $script:LaunchPins.TauriDriverVersion; bytes = [uint64]$receipt.tauriDriver.bytes; sha256 = [string]$receipt.tauriDriver.sha256; sourceSha256 = $script:LaunchPins.TauriDriverPackageSha256 }
            webView2RuntimeVersion = [string]$receipt.webView2RuntimeVersion
            testedSessionRuntimeVersion = [string]$result.returnedRuntimeVersion
            edgeDriver = [ordered]@{ version = [string]$receipt.edgeDriver.version; bytes = [uint64]$receipt.edgeDriver.bytes; sha256 = [string]$receipt.edgeDriver.sha256; publisher = $script:LaunchPins.EdgePublisher; trustedTimestamp = $true; versionBinding = 'native-status' }
            sessionCapabilityKeys = $capabilityKeys
        }
        profile = [ordered]@{ state = 'controlled-runner-owned'; binding = [string]$result.profileBindingMethod; prelaunchSettingsEntries = 1; sentinelPreserved = $true }
        title = 'PDF Workstation'
        homeButton = 'Explore a sample PDF'
        ocrCapabilityStatus = 'Available'
        sample = [ordered]@{ name = 'welcome.pdf'; pages = 6; firstPageDecoded = $true; naturalWidth = [int]$result.renderedPageWidth; naturalHeight = [int]$result.renderedPageHeight; source = 'blob:' }
        currentPageOcr = [ordered]@{ sampleName = 'welcome.pdf'; physicalPage = 1; dialogTitle = 'Recognize text on page 1'; status = 'recognized'; language = 'eng'; textUtf8Bytes = [int]$result.ocrTextUtf8Bytes; textSha256 = [string]$result.ocrTextSha256; sourceUiPreserved = $true; sourceFileReceiptPreserved = $true; accuracyVerified = $false }
        cleanup = [ordered]@{ sessionDeleted = $true; ownedProcessTreeStopped = $true; relevantProcessesRemaining = 0 }
    }
}
