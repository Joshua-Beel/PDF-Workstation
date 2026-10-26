[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')
. (Join-Path $PSScriptRoot 'installed-app-launch.ps1')

$script:ReadingPins = [ordered]@{
    LaunchTimeoutMilliseconds = 180000
    NativePickerTimeoutMilliseconds = 180000
    ProductTimeoutMilliseconds = 180000
    PostOpenTimeoutMilliseconds = 30000
    PortReleaseTimeoutMilliseconds = 180000
    UiAutomationPollMilliseconds = 100
    ClipboardPollMilliseconds = 100
    RequestBytesMaximum = 1MB
    OwnedProcessMaximum = 128
    UnknownExecutableHashMaximum = 32
    SelectionText = 'A place for your PDFs.'
    SearchText = 'Sample document'
    Password = 'test password'
}

function New-ReadingPhaseDeadline {
    param([Parameter(Mandatory = $true)][ValidateSet('launch','native-picker','product')][string]$Phase,[scriptblock]$UtcNowProvider)
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    $timeout = switch ($Phase) {
        'launch' { [int]$script:ReadingPins.LaunchTimeoutMilliseconds }
        'native-picker' { [int]$script:ReadingPins.NativePickerTimeoutMilliseconds }
        'product' { [int]$script:ReadingPins.ProductTimeoutMilliseconds }
    }
    if ($timeout -lt 1 -or $timeout -gt 180000) { throw 'A reading-tools phase timeout was outside its exact cap.' }
    return $now.AddMilliseconds($timeout)
}

function Assert-ReadingPhaseTransition {
    param([Parameter(Mandatory = $true)][datetime]$Deadline,[Parameter(Mandatory = $true)][ValidateSet('launch','native-picker','product')][string]$Phase,[scriptblock]$UtcNowProvider)
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    if ($now -ge $Deadline) { throw "The reading-tools $Phase phase expired before its next phase." }
}

function Assert-ReadingFileReceipt {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)][uint64]$Bytes,[Parameter(Mandatory = $true)][string]$Sha256,[Parameter(Mandatory = $true)][string]$Kind)
    Assert-NoReparseAncestors -Path $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Kind is missing." }
    $item = Get-Item -LiteralPath $Path -Force
    if ([uint64]$item.Length -ne $Bytes -or (Get-ExactSha256 -Path $Path) -cne $Sha256) { throw "$Kind does not match its exact receipt." }
}

function Get-ReadingSha256 {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][byte[]]$Bytes)
    $sha = [Security.Cryptography.SHA256]::Create()
    try { return ([BitConverter]::ToString($sha.ComputeHash($Bytes))).Replace('-','') } finally { $sha.Dispose() }
}

function Get-ReadingTextReceipt {
    param([Parameter(Mandatory = $true)][AllowEmptyString()][string]$Text)
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes($Text)
    return [pscustomobject]@{ utf8Bytes = [int]$bytes.Length; sha256 = Get-ReadingSha256 -Bytes $bytes }
}

function Invoke-ReadingWebDriverActions {
    param(
        [Parameter(Mandatory = $true)][string]$SessionId,
        [Parameter(Mandatory = $true)][object[]]$Actions,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    if ($SessionId -cnotmatch '^[A-Za-z0-9-]+$' -or $Actions.Count -lt 1 -or $Actions.Count -gt 4) {
        throw 'WebDriver input actions are outside the exact reading-tools contract.'
    }
    $path = "/session/$SessionId/actions"
    $uri = [Uri]::new("http://127.0.0.1:$($script:LaunchPins.WebDriverPort)$path")
    if (-not $uri.IsLoopback -or $uri.Scheme -cne 'http') { throw 'Reading-tools WebDriver actions are not fixed loopback HTTP.' }
    $timeout = Get-LaunchRemainingMilliseconds -Deadline $Deadline -MaximumMilliseconds $script:LaunchPins.RequestTimeoutMilliseconds
    $request = [Net.HttpWebRequest]::CreateHttp($uri)
    $request.Method = 'POST'; $request.AllowAutoRedirect = $false; $request.Proxy = $null
    $request.Timeout = $timeout; $request.ReadWriteTimeout = $timeout
    $request.ContentType = 'application/json; charset=utf-8'
    $body = [ordered]@{ actions = $Actions }
    $bytes = [Text.UTF8Encoding]::new($false).GetBytes(($body | ConvertTo-Json -Depth 12 -Compress))
    if ($bytes.Length -gt $script:ReadingPins.RequestBytesMaximum) { throw 'Reading-tools WebDriver actions exceeded the request limit.' }
    $request.ContentLength = $bytes.Length
    $stream = $request.GetRequestStream()
    try { $stream.Write($bytes, 0, $bytes.Length) } finally { $stream.Dispose() }
    $response = $null
    try {
        $response = [Net.HttpWebResponse]$request.GetResponse()
        if ([int]$response.StatusCode -ne 200) { throw 'Reading-tools WebDriver actions returned an unexpected status.' }
        $responseStream = $response.GetResponseStream(); $memory = [IO.MemoryStream]::new()
        try {
            $buffer = [byte[]]::new(4096)
            while (($read = $responseStream.Read($buffer, 0, $buffer.Length)) -gt 0) {
                if ($memory.Length + $read -gt $script:ReadingPins.RequestBytesMaximum) { throw 'Reading-tools WebDriver actions exceeded the response limit.' }
                $memory.Write($buffer, 0, $read)
            }
            $text = [Text.UTF8Encoding]::new($false, $true).GetString($memory.ToArray())
        } finally { $memory.Dispose(); $responseStream.Dispose() }
    } finally { if ($response) { $response.Dispose() } }
    if ([string]::IsNullOrWhiteSpace($text)) { throw 'Reading-tools WebDriver actions returned no JSON.' }
    $json = $text | ConvertFrom-Json
    $nodes = 0; Assert-BoundedJsonShape -Value $json -Nodes ([ref]$nodes)
    if ($json.PSObject.Properties['value'] -eq $null) { throw 'Reading-tools WebDriver actions returned no value member.' }
}

function Send-ReadingKeyChord {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][ValidateSet('c','f')][string]$Key,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $control = [string][char]0xE009
    Invoke-ReadingWebDriverActions -SessionId $SessionId -Deadline $Deadline -Actions @(
        [ordered]@{ type = 'key'; id = 'reading-keyboard'; actions = @(
            [ordered]@{ type = 'keyDown'; value = $control },
            [ordered]@{ type = 'keyDown'; value = $Key },
            [ordered]@{ type = 'keyUp'; value = $Key },
            [ordered]@{ type = 'keyUp'; value = $control }
        ) }
    )
}

function Invoke-ReadingClipboardSta {
    param([ValidateSet('set-sentinel','clear','receipt')][string]$Operation)
    $runspace = [RunspaceFactory]::CreateRunspace()
    $runspace.ApartmentState = [Threading.ApartmentState]::STA
    $runspace.ThreadOptions = [Management.Automation.Runspaces.PSThreadOptions]::UseNewThread
    $runspace.Open()
    $powerShell = [PowerShell]::Create(); $powerShell.Runspace = $runspace
    try {
        if ($Operation -ceq 'set-sentinel') {
            $null = $powerShell.AddScript("Add-Type -AssemblyName System.Windows.Forms;[Windows.Forms.Clipboard]::SetText('reading-verifier-sentinel')")
            $null = $powerShell.Invoke()
            if ($powerShell.HadErrors) { throw 'The Windows clipboard sentinel could not be set.' }
            return
        }
        if ($Operation -ceq 'clear') {
            $null = $powerShell.AddScript('Add-Type -AssemblyName System.Windows.Forms;[Windows.Forms.Clipboard]::Clear()')
            $null = $powerShell.Invoke()
            if ($powerShell.HadErrors) { throw 'The Windows clipboard could not be cleared.' }
            return
        }
        $source = @'
Add-Type -AssemblyName System.Windows.Forms
$text=[Windows.Forms.Clipboard]::GetText([Windows.Forms.TextDataFormat]::UnicodeText)
$bytes=[Text.UTF8Encoding]::new($false).GetBytes($text)
$sha=[Security.Cryptography.SHA256]::Create()
try{$hash=([BitConverter]::ToString($sha.ComputeHash($bytes))).Replace('-','')}finally{$sha.Dispose()}
[pscustomobject]@{utf8Bytes=[int]$bytes.Length;sha256=$hash}
'@
        $null = $powerShell.AddScript($source)
        $values = @($powerShell.Invoke())
        if ($powerShell.HadErrors -or $values.Count -ne 1 -or [string]$values[0].sha256 -cnotmatch '^[A-F0-9]{64}$') {
            throw 'The Windows clipboard receipt was unavailable.'
        }
        return $values[0]
    } finally { $powerShell.Dispose(); $runspace.Dispose() }
}

function Wait-ReadingClipboardReceipt {
    param([Parameter(Mandatory = $true)]$Expected,[Parameter(Mandatory = $true)][datetime]$Deadline)
    do {
        try {
            $receipt = Invoke-ReadingClipboardSta -Operation receipt
            if ([int]$receipt.utf8Bytes -eq [int]$Expected.utf8Bytes -and [string]$receipt.sha256 -ceq [string]$Expected.sha256) { return $receipt }
        } catch { }
        Start-Sleep -Milliseconds $script:ReadingPins.ClipboardPollMilliseconds
    } while ([datetime]::UtcNow -lt $Deadline)
    throw 'The Windows clipboard did not receive the exact selected embedded text.'
}

function Clear-ReadingClipboardAndVerify {
    param(
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$OperationProvider,
        [scriptblock]$SleepProvider,
        [scriptblock]$UtcNowProvider
    )
    $emptyReceipt = Get-ReadingTextReceipt -Text ''
    while ($true) {
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        try {
            if ($OperationProvider) { $null = & $OperationProvider 'clear' } else { Invoke-ReadingClipboardSta -Operation clear }
            $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
            if ($now -ge $Deadline) { break }
            $clipboard = if ($OperationProvider) { & $OperationProvider 'receipt' } else { Invoke-ReadingClipboardSta -Operation receipt }
            $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
            if ($now -ge $Deadline) { break }
            if ([int]$clipboard.utf8Bytes -eq 0 -and [string]$clipboard.sha256 -ceq [string]$emptyReceipt.sha256) { return $true }
        } catch { }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        if ($SleepProvider) { & $SleepProvider $script:ReadingPins.ClipboardPollMilliseconds } else { Start-Sleep -Milliseconds $script:ReadingPins.ClipboardPollMilliseconds }
    }
    throw 'The Windows clipboard did not clear to its exact empty receipt before the cleanup deadline.'
}

function Initialize-ReadingNativePickerApi {
    if ($null -ne ('ReadingNativePickerApi' -as [type])) { return }
    $source = @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class ReadingNativePickerApi {
    public delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr parameter);
    [DllImport("user32.dll")] public static extern bool EnumWindows(EnumWindowsProc callback, IntPtr parameter);
    [DllImport("user32.dll")] public static extern bool EnumChildWindows(IntPtr parent, EnumWindowsProc callback, IntPtr parameter);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
    [DllImport("user32.dll", CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr hwnd, StringBuilder className, int maximum);
    [DllImport("user32.dll")] public static extern int GetDlgCtrlID(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr hwnd);
    [DllImport("user32.dll")] public static extern IntPtr GetAncestor(IntPtr hwnd, uint flags);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", CharSet=CharSet.Unicode, SetLastError=true)]
    public static extern IntPtr SendMessageTimeoutText(IntPtr hwnd, uint message, IntPtr wParam, string text, uint flags, uint timeout, out IntPtr result);
    [DllImport("user32.dll", EntryPoint="SendMessageTimeoutW", SetLastError=true)]
    public static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam, uint flags, uint timeout, out IntPtr result);
}
'@
    $null = Add-Type -TypeDefinition $source
}

function Get-ReadingWindowProcessId {
    param([Parameter(Mandatory = $true)][IntPtr]$Handle)
    $ownerProcessId = [uint32]0
    $null = [ReadingNativePickerApi]::GetWindowThreadProcessId($Handle,[ref]$ownerProcessId)
    return [int]$ownerProcessId
}

function Get-ReadingWindowClassName {
    param([Parameter(Mandatory = $true)][IntPtr]$Handle)
    $builder = [Text.StringBuilder]::new(256)
    $length = [ReadingNativePickerApi]::GetClassNameW($Handle,$builder,$builder.Capacity)
    if ($length -lt 1 -or $length -ge $builder.Capacity) { return '' }
    return $builder.ToString()
}

function Get-ReadingHandleIdentity {
    param([Parameter(Mandatory = $true)][IntPtr]$Handle)
    $value = $Handle.ToInt64()
    if ($value -le 0) { throw 'A native picker HWND identity was invalid.' }
    return $value.ToString([Globalization.CultureInfo]::InvariantCulture)
}

function Assert-ReadingApplicationProcessIdentity {
    param(
        [Parameter(Mandatory = $true)][int]$ApplicationProcessId,
        [Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$ProcessIdentityProvider
    )
    if ($ApplicationProcessId -le 0 -or $ApplicationProcessStartUtcTicks -le 0 -or [datetime]::UtcNow -ge $Deadline) { throw 'The native picker application process identity or deadline was invalid.' }
    $facts = if ($ProcessIdentityProvider) { & $ProcessIdentityProvider $ApplicationProcessId $ApplicationProcessStartUtcTicks $Deadline } else {
        $process = Get-Process -Id $ApplicationProcessId -ErrorAction Stop
        try { [pscustomobject]@{ processId=[int]$process.Id;startUtcTicks=[long]$process.StartTime.ToUniversalTime().Ticks } } finally { $process.Dispose() }
    }
    $properties = @($facts.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if (($properties -join ',') -cne 'processId,startUtcTicks' -or [int]$facts.processId -ne $ApplicationProcessId -or [long]$facts.startUtcTicks -ne $ApplicationProcessStartUtcTicks -or [datetime]::UtcNow -ge $Deadline) {
        throw 'The native picker application process identity changed.'
    }
}

function Get-ReadingProcessUiSurfaceSnapshot {
    param([Parameter(Mandatory = $true)][int]$ApplicationProcessId,[Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$ElementProvider,[scriptblock]$ProcessIdentityProvider)
    Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    if ([datetime]::UtcNow -ge $Deadline) { throw 'The process-bound top-level UI snapshot deadline expired.' }
    if ($ElementProvider) {
        $elements = @(& $ElementProvider $ApplicationProcessId $ApplicationProcessStartUtcTicks $Deadline)
    } else {
        Initialize-ReadingNativePickerApi
        $handles = [Collections.Generic.List[IntPtr]]::new(); $state = [pscustomobject]@{expired=$false;exceeded=$false}
        $callback = [ReadingNativePickerApi+EnumWindowsProc]{
            param([IntPtr]$handle,[IntPtr]$parameter)
            if ([datetime]::UtcNow -ge $Deadline) { $state.expired = $true; return $false }
            if ((Get-ReadingWindowProcessId -Handle $handle) -eq $ApplicationProcessId -and [ReadingNativePickerApi]::IsWindowVisible($handle)) {
                $handles.Add($handle)
                if ($handles.Count -gt 8) { $state.exceeded = $true; return $false }
            }
            return $true
        }
        $null = [ReadingNativePickerApi]::EnumWindows($callback,[IntPtr]::Zero)
        if ($state.expired -or [datetime]::UtcNow -ge $Deadline) { throw 'The process-bound top-level HWND snapshot exceeded its deadline.' }
        if ($state.exceeded -or $handles.Count -gt 8) { throw 'The application exposed too many process-bound top-level HWNDs.' }
        $elements = @($handles)
    }
    Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    $identities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $snapshot = @()
    foreach ($handle in $elements) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'The process-bound top-level HWND snapshot exceeded its deadline.' }
        $ownerProcessId = Get-ReadingWindowProcessId -Handle $handle
        if ($ownerProcessId -ne $ApplicationProcessId) { throw 'A top-level HWND had a mismatched process identifier.' }
        $identity = Get-ReadingHandleIdentity -Handle $handle
        if (-not $identities.Add($identity)) { throw 'A process-bound top-level HWND identity was duplicated.' }
        $snapshot += [pscustomobject]@{ runtimeIdentity=$identity;controlType='Hwnd';isEnabled=[ReadingNativePickerApi]::IsWindowEnabled($handle);isOffscreen=(-not [ReadingNativePickerApi]::IsWindowVisible($handle));processId=$ownerProcessId;handle=$handle }
    }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'The process-bound top-level HWND snapshot exceeded its deadline.' }
    return $snapshot
}

function Assert-ReadingSurfaceSnapshot {
    param([Parameter(Mandatory = $true)][object[]]$Snapshot,[Parameter(Mandatory = $true)][int]$ApplicationProcessId,[Parameter(Mandatory = $true)][string]$Kind)
    if ($Snapshot.Count -lt 1 -or $Snapshot.Count -gt 8) { throw "$Kind was missing or outside its element-count cap." }
    $identities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Snapshot) {
        $properties = @($entry.PSObject.Properties.Name | Sort-Object -CaseSensitive)
        if (($properties -join ',') -cne 'controlType,handle,isEnabled,isOffscreen,processId,runtimeIdentity' -or [string]$entry.runtimeIdentity -cnotmatch '^\d{1,20}$' -or -not $identities.Add([string]$entry.runtimeIdentity) -or [string]$entry.controlType -cne 'Hwnd' -or [int]$entry.processId -ne $ApplicationProcessId -or [IntPtr]$entry.handle -eq [IntPtr]::Zero) {
            throw "$Kind was invalid or ambiguous."
        }
    }
    return ,$identities
}

function Get-ReadingPickerTargetSnapshot {
    param(
        [Parameter(Mandatory = $true)][int]$ApplicationProcessId,
        [Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,
        [Parameter(Mandatory = $true)][object[]]$Surfaces,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$TargetProvider,
        [scriptblock]$ProcessIdentityProvider
    )
    Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    $surfaceIdentities = Assert-ReadingSurfaceSnapshot -Snapshot $Surfaces -ApplicationProcessId $ApplicationProcessId -Kind 'The picker target surface snapshot'
    if ($TargetProvider) { $providedTargets = @(& $TargetProvider $Surfaces $ApplicationProcessId $ApplicationProcessStartUtcTicks $Deadline); Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider; return $providedTargets }
    Initialize-ReadingNativePickerApi
    $targets = [Collections.Generic.List[object]]::new(); $targetIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($surface in $Surfaces) {
        $state = [pscustomobject]@{childCount=0;expired=$false;exceeded=$false}
        $callback = [ReadingNativePickerApi+EnumWindowsProc]{
            param([IntPtr]$handle,[IntPtr]$parameter)
            if ([datetime]::UtcNow -ge $Deadline) { $state.expired = $true; return $false }
            $state.childCount++
            if ($state.childCount -gt 256) { $state.exceeded = $true; return $false }
            $ownerProcessId = Get-ReadingWindowProcessId -Handle $handle
            if ($ownerProcessId -ne $ApplicationProcessId) { return $true }
            $controlId = [ReadingNativePickerApi]::GetDlgCtrlID($handle)
            $className = Get-ReadingWindowClassName -Handle $handle
            $targetKind = if ($className -ceq 'Edit' -and $controlId -in @(1001,1148,1152)) { 'filename' } elseif ($className -ceq 'Button' -and $controlId -eq 1) { 'open' } else { '' }
            if ($targetKind) {
                $identity = Get-ReadingHandleIdentity -Handle $handle
                if (-not $targetIdentities.Add($identity)) { $state.exceeded = $true; return $false }
                $nativeControlCategory = if ($targetKind -ceq 'filename') { "edit-$controlId" } else { 'button-idok' }
                $targets.Add([pscustomobject]@{runtimeIdentity=$identity;surfaceRuntimeIdentity=[string]$surface.runtimeIdentity;targetKind=$targetKind;nativeControlCategory=$nativeControlCategory;isEnabled=[ReadingNativePickerApi]::IsWindowEnabled($handle);isOffscreen=(-not [ReadingNativePickerApi]::IsWindowVisible($handle));processId=$ownerProcessId;handle=$handle})
                if ($targets.Count -gt 32) { $state.exceeded = $true; return $false }
            }
            return $true
        }
        $null = [ReadingNativePickerApi]::EnumChildWindows([IntPtr]$surface.handle,$callback,[IntPtr]::Zero)
        if ($state.expired -or [datetime]::UtcNow -ge $Deadline) { throw 'The native picker HWND snapshot deadline expired.' }
        if ($state.exceeded) { throw 'The native picker HWND snapshot exceeded its bounded child or target count.' }
    }
    Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    if ([datetime]::UtcNow -ge $Deadline) { throw 'The native picker HWND snapshot deadline expired.' }
    return @($targets)
}

function Assert-ReadingPickerTargetSnapshot {
    param([Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Snapshot,[Parameter(Mandatory = $true)]$SurfaceIdentities,[Parameter(Mandatory = $true)][int]$ApplicationProcessId,[Parameter(Mandatory = $true)][string]$Kind)
    if ($Snapshot.Count -gt 32) { throw "$Kind exceeded its element-count cap." }
    $identities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Snapshot) {
        $properties = @($entry.PSObject.Properties.Name | Sort-Object -CaseSensitive)
        $categoryValid = ([string]$entry.targetKind -ceq 'filename' -and [string]$entry.nativeControlCategory -in @('edit-1001','edit-1148','edit-1152')) -or ([string]$entry.targetKind -ceq 'open' -and [string]$entry.nativeControlCategory -ceq 'button-idok')
        if (($properties -join ',') -cne 'handle,isEnabled,isOffscreen,nativeControlCategory,processId,runtimeIdentity,surfaceRuntimeIdentity,targetKind' -or [string]$entry.runtimeIdentity -cnotmatch '^\d{1,20}$' -or -not $identities.Add([string]$entry.runtimeIdentity) -or -not $SurfaceIdentities.Contains([string]$entry.surfaceRuntimeIdentity) -or -not $categoryValid -or [int]$entry.processId -ne $ApplicationProcessId -or [IntPtr]$entry.handle -eq [IntPtr]::Zero) {
            throw "$Kind was invalid, cross-surface, or ambiguous."
        }
    }
    return ,$identities
}

function Wait-ReadingProcessBoundPickerTargets {
    param(
        [Parameter(Mandatory = $true)][int]$ApplicationProcessId,
        [Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,
        [Parameter(Mandatory = $true)][object[]]$BaselineSurfaces,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$BaselineTargets,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$SurfaceSnapshotProvider,
        [scriptblock]$TargetSnapshotProvider,
        [scriptblock]$ProcessIdentityProvider,
        [scriptblock]$SleepProvider,
        [scriptblock]$UtcNowProvider
    )
    $baselineSurfaceIdentities = Assert-ReadingSurfaceSnapshot -Snapshot $BaselineSurfaces -ApplicationProcessId $ApplicationProcessId -Kind 'The pre-click top-level UI baseline'
    $baselineTargetIdentities = Assert-ReadingPickerTargetSnapshot -Snapshot $BaselineTargets -SurfaceIdentities $baselineSurfaceIdentities -ApplicationProcessId $ApplicationProcessId -Kind 'The pre-click picker target baseline'
    $diagnostic = [ordered]@{schemaVersion=2;baselineSurfaceCount=[int]$BaselineSurfaces.Count;currentSurfaceCount=0;newSurfaceCount=0;baselineTargetCount=[int]$BaselineTargets.Count;currentTargetCount=0;newFilenameCount=0;newOpenCount=0;baselineMissingCount=0}
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    while ($now -lt $Deadline) {
        $currentSurfaces = @(if ($SurfaceSnapshotProvider) { Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider; & $SurfaceSnapshotProvider $ApplicationProcessId $ApplicationProcessStartUtcTicks $Deadline } else { Get-ReadingProcessUiSurfaceSnapshot -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider })
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        $currentSurfaceIdentities = Assert-ReadingSurfaceSnapshot -Snapshot $currentSurfaces -ApplicationProcessId $ApplicationProcessId -Kind 'The post-click top-level UI snapshot'
        $missingSurfaces = @($BaselineSurfaces | Where-Object { -not $currentSurfaceIdentities.Contains([string]$_.runtimeIdentity) })
        $newSurfaces = @($currentSurfaces | Where-Object { -not $baselineSurfaceIdentities.Contains([string]$_.runtimeIdentity) })
        $currentTargets = @(if ($TargetSnapshotProvider) { Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider; & $TargetSnapshotProvider $currentSurfaces $ApplicationProcessId $ApplicationProcessStartUtcTicks $Deadline } else { Get-ReadingPickerTargetSnapshot -Surfaces $currentSurfaces -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider })
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        $null = Assert-ReadingPickerTargetSnapshot -Snapshot $currentTargets -SurfaceIdentities $currentSurfaceIdentities -ApplicationProcessId $ApplicationProcessId -Kind 'The post-click picker target snapshot'
        $newTargets = @($currentTargets | Where-Object { -not $baselineTargetIdentities.Contains([string]$_.runtimeIdentity) })
        $newFilename = @($newTargets | Where-Object { [string]$_.targetKind -ceq 'filename' })
        $newOpen = @($newTargets | Where-Object { [string]$_.targetKind -ceq 'open' })
        $diagnostic.currentSurfaceCount = [int]$currentSurfaces.Count; $diagnostic.newSurfaceCount = [int]$newSurfaces.Count
        $diagnostic.currentTargetCount = [int]$currentTargets.Count; $diagnostic.newFilenameCount = [int]$newFilename.Count
        $diagnostic.newOpenCount = [int]$newOpen.Count; $diagnostic.baselineMissingCount = [int]$missingSurfaces.Count
        if ($missingSurfaces.Count -ne 0) { throw 'The pre-click top-level UI baseline was replaced while opening the native picker.' }
        if ($newSurfaces.Count -gt 1) { throw 'More than one new process-bound native picker surface appeared.' }
        if ($newFilename.Count -gt 1 -or $newOpen.Count -gt 1) { throw 'The new native picker targets were ambiguous.' }
        if ($newFilename.Count -eq 1 -and $newOpen.Count -eq 1) {
            if ([string]$newFilename[0].surfaceRuntimeIdentity -cne [string]$newOpen[0].surfaceRuntimeIdentity) { throw 'The new native picker targets appeared on different owned surfaces.' }
            $surface = @($currentSurfaces | Where-Object { [string]$_.runtimeIdentity -ceq [string]$newFilename[0].surfaceRuntimeIdentity })
            if ($surface.Count -ne 1 -or -not [bool]$surface[0].isEnabled -or [bool]$surface[0].isOffscreen -or -not [bool]$newFilename[0].isEnabled -or [bool]$newFilename[0].isOffscreen -or -not [bool]$newOpen[0].isEnabled -or [bool]$newOpen[0].isOffscreen) { throw 'The bound native picker surface or targets were not uniquely enabled and visible.' }
            if ($newSurfaces.Count -eq 1 -and [string]$newSurfaces[0].runtimeIdentity -cne [string]$surface[0].runtimeIdentity) { throw 'A sibling native surface appeared outside the bound picker target surface.' }
            return [pscustomobject]@{surfaceRuntimeIdentity=[string]$surface[0].runtimeIdentity;surfaceControlType=[string]$surface[0].controlType;surfaceHandle=[IntPtr]$surface[0].handle;baselineIdentities=[string[]]@($baselineSurfaceIdentities);baselineTargetIdentities=[string[]]@($baselineTargetIdentities);dedicatedNewSurface=($newSurfaces.Count -eq 1);filenameRuntimeIdentity=[string]$newFilename[0].runtimeIdentity;filenameHandle=[IntPtr]$newFilename[0].handle;filenameControlCategory=[string]$newFilename[0].nativeControlCategory;openButtonRuntimeIdentity=[string]$newOpen[0].runtimeIdentity;openButtonHandle=[IntPtr]$newOpen[0].handle}
        }
        if ($SleepProvider) { & $SleepProvider $script:ReadingPins.UiAutomationPollMilliseconds } else { Start-Sleep -Milliseconds $script:ReadingPins.UiAutomationPollMilliseconds }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    }
    throw "No single new process-bound native picker target pair appeared; diagnostic=$($diagnostic | ConvertTo-Json -Compress)"
}

function Assert-ReadingSurfaceBinding {
    param([Parameter(Mandatory = $true)]$Binding,[Parameter(Mandatory = $true)][int]$ApplicationProcessId,[Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,[Parameter(Mandatory = $true)][datetime]$Deadline,[switch]$AllowUnavailableTargets,[scriptblock]$ProcessIdentityProvider)
    Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    $properties = @($Binding.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if (($properties -join ',') -cne 'baselineIdentities,baselineTargetIdentities,dedicatedNewSurface,filenameControlCategory,filenameHandle,filenameRuntimeIdentity,openButtonHandle,openButtonRuntimeIdentity,surfaceControlType,surfaceHandle,surfaceRuntimeIdentity' -or [string]$Binding.surfaceControlType -cne 'Hwnd' -or [string]$Binding.filenameControlCategory -notin @('edit-1001','edit-1148','edit-1152') -or @($Binding.baselineIdentities).Count -lt 1 -or @($Binding.baselineIdentities).Count -gt 8 -or @($Binding.baselineTargetIdentities).Count -gt 32 -or [string]$Binding.filenameRuntimeIdentity -cnotmatch '^\d{1,20}$' -or [string]$Binding.openButtonRuntimeIdentity -cnotmatch '^\d{1,20}$' -or [IntPtr]$Binding.surfaceHandle -eq [IntPtr]::Zero -or [IntPtr]$Binding.filenameHandle -eq [IntPtr]::Zero -or [IntPtr]$Binding.openButtonHandle -eq [IntPtr]::Zero) {
        throw 'The native picker surface binding was invalid.'
    }
    if (-not $AllowUnavailableTargets) {
        Initialize-ReadingNativePickerApi
        if ([datetime]::UtcNow -ge $Deadline) { throw 'The native picker HWND binding deadline expired.' }
        foreach ($handle in @([IntPtr]$Binding.surfaceHandle,[IntPtr]$Binding.filenameHandle,[IntPtr]$Binding.openButtonHandle)) {
            if (-not [ReadingNativePickerApi]::IsWindow($handle) -or (Get-ReadingWindowProcessId -Handle $handle) -ne $ApplicationProcessId) { throw 'A bound native picker HWND was unavailable or had a mismatched process identifier.' }
        }
        if ((Get-ReadingHandleIdentity -Handle ([IntPtr]$Binding.surfaceHandle)) -cne [string]$Binding.surfaceRuntimeIdentity -or (Get-ReadingHandleIdentity -Handle ([IntPtr]$Binding.filenameHandle)) -cne [string]$Binding.filenameRuntimeIdentity -or (Get-ReadingHandleIdentity -Handle ([IntPtr]$Binding.openButtonHandle)) -cne [string]$Binding.openButtonRuntimeIdentity) { throw 'A bound native picker HWND identity changed before its action.' }
        if ([ReadingNativePickerApi]::GetAncestor([IntPtr]$Binding.filenameHandle,2) -ne [IntPtr]$Binding.surfaceHandle -or [ReadingNativePickerApi]::GetAncestor([IntPtr]$Binding.openButtonHandle,2) -ne [IntPtr]$Binding.surfaceHandle) { throw 'A bound native picker target escaped the exact owned HWND before its action.' }
        $expectedFilenameControlId = switch -CaseSensitive ([string]$Binding.filenameControlCategory) { 'edit-1001' { 1001 } 'edit-1148' { 1148 } 'edit-1152' { 1152 } default { throw 'The native picker filename control category is unknown.' } }
        if ((Get-ReadingWindowClassName -Handle ([IntPtr]$Binding.filenameHandle)) -cne 'Edit' -or [ReadingNativePickerApi]::GetDlgCtrlID([IntPtr]$Binding.filenameHandle) -ne $expectedFilenameControlId -or (Get-ReadingWindowClassName -Handle ([IntPtr]$Binding.openButtonHandle)) -cne 'Button' -or [ReadingNativePickerApi]::GetDlgCtrlID([IntPtr]$Binding.openButtonHandle) -ne 1) { throw 'A bound native picker HWND no longer matched the exact filename or Open control contract.' }
        Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
        if ([datetime]::UtcNow -ge $Deadline) { throw 'The native picker HWND binding deadline expired.' }
    }
}

function Wait-ReadingProcessUiSurfaceClosed {
    param([Parameter(Mandatory = $true)]$Binding,[Parameter(Mandatory = $true)][int]$ApplicationProcessId,[Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$SurfaceSnapshotProvider,[scriptblock]$TargetSnapshotProvider,[scriptblock]$ProcessIdentityProvider,[scriptblock]$SleepProvider,[scriptblock]$UtcNowProvider)
    Assert-ReadingSurfaceBinding -Binding $Binding -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -AllowUnavailableTargets -ProcessIdentityProvider $ProcessIdentityProvider
    $baselineIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($identity in @($Binding.baselineIdentities)) { if (-not $baselineIdentities.Add([string]$identity)) { throw 'The native picker surface baseline was ambiguous.' } }
    $baselineTargetIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($identity in @($Binding.baselineTargetIdentities)) { if (-not $baselineTargetIdentities.Add([string]$identity)) { throw 'The native picker target baseline was ambiguous.' } }
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    while ($now -lt $Deadline) {
        $current = @(if ($SurfaceSnapshotProvider) { Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider; & $SurfaceSnapshotProvider $ApplicationProcessId $ApplicationProcessStartUtcTicks $Deadline } else { Get-ReadingProcessUiSurfaceSnapshot -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider })
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        $currentIdentities = Assert-ReadingSurfaceSnapshot -Snapshot $current -ApplicationProcessId $ApplicationProcessId -Kind 'The post-submit top-level UI snapshot'
        if (@($Binding.baselineIdentities | Where-Object { -not $currentIdentities.Contains([string]$_) }).Count -ne 0) { throw 'The pre-click top-level UI baseline was replaced while closing the native picker.' }
        $remainingNew = @($current | Where-Object { -not $baselineIdentities.Contains([string]$_.runtimeIdentity) })
        $currentTargets = @(if ($TargetSnapshotProvider) { Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider; & $TargetSnapshotProvider $current $ApplicationProcessId $ApplicationProcessStartUtcTicks $Deadline } else { Get-ReadingPickerTargetSnapshot -Surfaces $current -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider })
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        $null = Assert-ReadingPickerTargetSnapshot -Snapshot $currentTargets -SurfaceIdentities $currentIdentities -ApplicationProcessId $ApplicationProcessId -Kind 'The post-submit picker target snapshot'
        $remainingTargets = @($currentTargets | Where-Object { -not $baselineTargetIdentities.Contains([string]$_.runtimeIdentity) })
        $unexpectedTargets = @($remainingTargets | Where-Object { [string]$_.runtimeIdentity -cne [string]$Binding.filenameRuntimeIdentity -and [string]$_.runtimeIdentity -cne [string]$Binding.openButtonRuntimeIdentity })
        if ($unexpectedTargets.Count -ne 0) { throw 'A bound native picker target was replaced before closing.' }
        $surfacePresent = $currentIdentities.Contains([string]$Binding.surfaceRuntimeIdentity)
        $filenameEntries = @($remainingTargets | Where-Object { [string]$_.runtimeIdentity -ceq [string]$Binding.filenameRuntimeIdentity })
        $openEntries = @($remainingTargets | Where-Object { [string]$_.runtimeIdentity -ceq [string]$Binding.openButtonRuntimeIdentity })
        if (($filenameEntries.Count -eq 1 -and ([string]$filenameEntries[0].targetKind -cne 'filename' -or [string]$filenameEntries[0].surfaceRuntimeIdentity -cne [string]$Binding.surfaceRuntimeIdentity)) -or ($openEntries.Count -eq 1 -and ([string]$openEntries[0].targetKind -cne 'open' -or [string]$openEntries[0].surfaceRuntimeIdentity -cne [string]$Binding.surfaceRuntimeIdentity))) {
            throw 'A bound native picker target moved or changed kind before closing.'
        }
        $filenamePresent = $filenameEntries.Count -eq 1
        $openPresent = $openEntries.Count -eq 1
        if ([bool]$Binding.dedicatedNewSurface) {
            if (-not $surfacePresent) {
                if ($remainingNew.Count -ne 0 -or $remainingTargets.Count -ne 0) { throw 'The exact dedicated native picker surface was replaced before closing.' }
                return
            }
            if ($remainingNew.Count -ne 1 -or [string]$remainingNew[0].runtimeIdentity -cne [string]$Binding.surfaceRuntimeIdentity) { throw 'The dedicated native picker surface topology changed before closing.' }
        } else {
            if (-not $surfacePresent -or $remainingNew.Count -ne 0) { throw 'The stable native picker surface topology changed before closing.' }
            if (-not $filenamePresent -and -not $openPresent) { return }
        }
        if ($SleepProvider) { & $SleepProvider $script:ReadingPins.UiAutomationPollMilliseconds } else { Start-Sleep -Milliseconds $script:ReadingPins.UiAutomationPollMilliseconds }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    }
    throw 'The exact process-bound native picker surface did not close.'
}

function Set-ReadingNativePickerValueBeforeDeadline {
    param([Parameter(Mandatory = $true)][IntPtr]$Handle,[Parameter(Mandatory = $true)][string]$Value,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$MessageProvider,[scriptblock]$UtcNowProvider)
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    if ($now -ge $Deadline) { throw 'The process-bound filename mutation deadline expired.' }
    $remaining = [int][Math]::Floor(($Deadline - $now).TotalMilliseconds)
    if ($remaining -lt 1) { throw 'The process-bound filename mutation deadline expired.' }
    $timeout = [uint32][Math]::Min(5000,$remaining)
    if ($MessageProvider) {
        $receipts = @(& $MessageProvider $Handle $Value $timeout)
        if ($receipts.Count -ne 1) { throw 'The bounded WM_SETTEXT provider returned an invalid receipt.' }
        $receipt = $receipts[0]; $properties = @($receipt.PSObject.Properties.Name | Sort-Object -CaseSensitive)
        if (($properties -join ',') -cne 'delivered,result' -or $receipt.delivered -isnot [bool] -or $receipt.result -isnot [IntPtr]) { throw 'The bounded WM_SETTEXT provider returned an invalid receipt.' }
        $sent = [bool]$receipt.delivered; $messageResult = [IntPtr]$receipt.result
    } else {
        Initialize-ReadingNativePickerApi
        $messageResult = [IntPtr]::Zero
        $sent = [ReadingNativePickerApi]::SendMessageTimeoutText($Handle,0x000C,[IntPtr]::Zero,$Value,3,$timeout,[ref]$messageResult) -ne [IntPtr]::Zero
    }
    if (-not $sent) { throw 'The process-bound filename HWND did not accept bounded WM_SETTEXT.' }
    if ($messageResult -eq [IntPtr]::Zero) { throw 'The process-bound filename HWND reported that WM_SETTEXT did not set the value.' }
}

function Invoke-ReadingNativePickerButtonBeforeDeadline {
    param([Parameter(Mandatory = $true)][IntPtr]$Handle,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$MessageProvider,[scriptblock]$UtcNowProvider)
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    if ($now -ge $Deadline) { throw 'The process-bound native Open mutation deadline expired.' }
    $remaining = [int][Math]::Floor(($Deadline - $now).TotalMilliseconds)
    if ($remaining -lt 1) { throw 'The process-bound native Open mutation deadline expired.' }
    $timeout = [uint32][Math]::Min(5000,$remaining)
    if ($MessageProvider) {
        $receipts = @(& $MessageProvider $Handle $timeout)
        if ($receipts.Count -ne 1) { throw 'The bounded BM_CLICK provider returned an invalid receipt.' }
        $receipt = $receipts[0]; $properties = @($receipt.PSObject.Properties.Name | Sort-Object -CaseSensitive)
        if (($properties -join ',') -cne 'delivered,result' -or $receipt.delivered -isnot [bool] -or $receipt.result -isnot [IntPtr]) { throw 'The bounded BM_CLICK provider returned an invalid receipt.' }
        $sent = [bool]$receipt.delivered; $messageResult = [IntPtr]$receipt.result
    } else {
        Initialize-ReadingNativePickerApi
        $messageResult = [IntPtr]::Zero
        $sent = [ReadingNativePickerApi]::SendMessageTimeout($Handle,0x00F5,[IntPtr]::Zero,[IntPtr]::Zero,3,$timeout,[ref]$messageResult) -ne [IntPtr]::Zero
    }
    if (-not $sent) { throw 'The process-bound native Open HWND did not accept bounded BM_CLICK.' }
}

function Submit-ProcessBoundOpenDialog {
    param([Parameter(Mandatory = $true)]$Binding,[Parameter(Mandatory = $true)][int]$ApplicationProcessId,[Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,[Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$ProcessIdentityProvider)
    Assert-ReadingSurfaceBinding -Binding $Binding -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    if (-not [ReadingNativePickerApi]::IsWindowEnabled([IntPtr]$Binding.filenameHandle) -or -not [ReadingNativePickerApi]::IsWindowVisible([IntPtr]$Binding.filenameHandle)) { throw 'The exact bound filename HWND was no longer actionable.' }
    if (-not [ReadingNativePickerApi]::IsWindowEnabled([IntPtr]$Binding.openButtonHandle) -or -not [ReadingNativePickerApi]::IsWindowVisible([IntPtr]$Binding.openButtonHandle)) { throw 'The exact bound native Open HWND was no longer actionable.' }
    Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    Set-ReadingNativePickerValueBeforeDeadline -Handle ([IntPtr]$Binding.filenameHandle) -Value $Path -Deadline $Deadline
    Assert-ReadingSurfaceBinding -Binding $Binding -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    Invoke-ReadingNativePickerButtonBeforeDeadline -Handle ([IntPtr]$Binding.openButtonHandle) -Deadline $Deadline
}

function Get-ReadingHomeScript {
    return @'
return {ready:document.readyState==='complete',title:document.title,sample:[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Explore a sample PDF'&&!x.disabled).length};
'@
}

function Get-ReadingOpenFileScript {
    return @'
const b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Open a file'&&!x.disabled&&x.parentElement?.querySelector('button[aria-label="Toggle theme"]'));if(b.length===1)b[0].click();return b.length===1;
'@
}

function Open-ReadingUserFile {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][int]$ApplicationProcessId,[Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,[Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$ProcessIdentityProvider)
    $baseline = @(Get-ReadingProcessUiSurfaceSnapshot -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider)
    $baselineSurfaceIdentities = Assert-ReadingSurfaceSnapshot -Snapshot $baseline -ApplicationProcessId $ApplicationProcessId -Kind 'The pre-click top-level UI baseline'
    $baselineTargets = @(Get-ReadingPickerTargetSnapshot -Surfaces $baseline -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider)
    $null = Assert-ReadingPickerTargetSnapshot -Snapshot $baselineTargets -SurfaceIdentities $baselineSurfaceIdentities -ApplicationProcessId $ApplicationProcessId -Kind 'The pre-click picker target baseline'
    $clicked = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script (Get-ReadingOpenFileScript)
    if ($clicked -isnot [bool] -or -not $clicked) { throw 'The exact installed Open a file control was unavailable.' }
    $surface = Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -BaselineSurfaces $baseline -BaselineTargets $baselineTargets -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    Submit-ProcessBoundOpenDialog -Binding $surface -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Path $Path -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    Wait-ReadingProcessUiSurfaceClosed -Binding $surface -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider
    return [string]$surface.filenameControlCategory
}

function Get-ReadingPostOpenScript {
    param([Parameter(Mandatory = $true)][ValidateSet('reading-source.pdf')][string]$ExpectedName)
    $expected = $ExpectedName | ConvertTo-Json -Compress
    return @"
const expected=$expected,tabs=[...document.querySelectorAll('div')].filter(x=>x.querySelector(':scope>button>span')&&x.querySelector(':scope>button[aria-label^="Close "]')),active=tabs.filter(x=>typeof x.className==='string'&&x.className.includes('selectedTab')),name=active.length===1?(active[0].querySelector(':scope>button>span')?.textContent||'').trim():'',input=document.querySelector('input[aria-label="Page number"]'),pages=[...document.querySelectorAll('[aria-label^="Page "]')].filter(x=>/^Page \d+$/.test(x.getAttribute('aria-label')||'')),images=[...document.querySelectorAll('img[alt^="Page "]')].filter(x=>/^Page \d+$/.test(x.getAttribute('alt')||'')),layers=[...document.querySelectorAll('[data-testid="text-layer"]')],pageOne=document.querySelector('[aria-label="Page 1"]');return {schemaVersion:1,documentTabCount:tabs.length,activeTabCount:active.length,activeFilenameMatches:name===expected,pageInputCount:document.querySelectorAll('input[aria-label="Page number"]').length,pageCount:Number.parseInt(input?.max||'0',10)||0,renderedPageCount:pages.length,imageCount:images.length,loadedImageCount:images.filter(x=>x.complete&&x.naturalWidth>0&&x.naturalHeight>0).length,textLayerCount:layers.length,pageOneGlyphCount:pageOne?.querySelector('[data-testid="text-layer"]')?.querySelectorAll('[data-geometry-index]').length||0,pageTextStatusCount:pageOne?.querySelectorAll('[role="status"]').length||0,passwordDialogCount:document.querySelectorAll('dialog[aria-labelledby="password-title"]').length,alertCount:document.querySelectorAll('[role="alert"]').length};
"@
}

function Assert-ReadingPostOpenState {
    param([Parameter(Mandatory = $true)]$State)
    $properties = @($State.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if (($properties -join ',') -cne 'activeFilenameMatches,activeTabCount,alertCount,documentTabCount,imageCount,loadedImageCount,pageCount,pageInputCount,pageOneGlyphCount,pageTextStatusCount,passwordDialogCount,renderedPageCount,schemaVersion,textLayerCount' -or [int]$State.schemaVersion -ne 1 -or $State.activeFilenameMatches -isnot [bool]) { throw 'The post-open document state had an invalid schema.' }
    foreach ($name in @('activeTabCount','alertCount','documentTabCount','imageCount','loadedImageCount','pageCount','pageInputCount','pageTextStatusCount','passwordDialogCount','renderedPageCount','textLayerCount')) {
        $value = $State.PSObject.Properties[$name].Value
        if (($value -isnot [int] -and $value -isnot [long]) -or [long]$value -lt 0 -or [long]$value -gt 4096) { throw 'The post-open document state exceeded its count contract.' }
    }
    if (($State.pageOneGlyphCount -isnot [int] -and $State.pageOneGlyphCount -isnot [long]) -or [long]$State.pageOneGlyphCount -lt 0 -or [long]$State.pageOneGlyphCount -gt 100000 -or [long]$State.loadedImageCount -gt [long]$State.imageCount) { throw 'The post-open document state exceeded its render contract.' }
}

function Wait-ReadingPostOpenState {
    param(
        [Parameter(Mandatory = $true)][string]$SessionId,
        [Parameter(Mandatory = $true)][ValidateSet('reading-source.pdf')][string]$ExpectedName,
        [Parameter(Mandatory = $true)][ValidateRange(1,4096)][int]$ExpectedPages,
        [Parameter(Mandatory = $true)][ValidateSet('edit-1001','edit-1148')][string]$NativeControlCategory,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$StateProvider,
        [scriptblock]$SleepProvider,
        [scriptblock]$UtcNowProvider
    )
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    $postOpenTimeout = [int]$script:ReadingPins.PostOpenTimeoutMilliseconds
    if ($postOpenTimeout -lt 1 -or $postOpenTimeout -gt 30000) { throw 'The post-open document-state timeout exceeded its exact cap.' }
    $postOpenDeadline = $now.AddMilliseconds($postOpenTimeout)
    if ($postOpenDeadline -gt $Deadline) { $postOpenDeadline = $Deadline }
    $diagnostic = [ordered]@{schemaVersion=1;nativeFilenameControlCategory=$NativeControlCategory;documentTabCount=0;activeTabCount=0;activeFilenameMatches=$false;pageInputCount=0;pageCount=0;renderedPageCount=0;imageCount=0;loadedImageCount=0;textLayerCount=0;pageOneGlyphCount=0;pageTextStatusCount=0;passwordDialogCount=0;alertCount=0}
    while ($now -lt $postOpenDeadline) {
        $state = if ($StateProvider) { & $StateProvider } else { Invoke-WebDriverScript -SessionId $SessionId -Deadline $postOpenDeadline -Script (Get-ReadingPostOpenScript -ExpectedName $ExpectedName) }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $postOpenDeadline) { break }
        Assert-ReadingPostOpenState -State $state
        foreach ($name in @('documentTabCount','activeTabCount','activeFilenameMatches','pageInputCount','pageCount','renderedPageCount','imageCount','loadedImageCount','textLayerCount','pageOneGlyphCount','pageTextStatusCount','passwordDialogCount','alertCount')) { $diagnostic[$name] = $state.PSObject.Properties[$name].Value }
        $wrongDocument = [int]$state.documentTabCount -gt 1 -or ([int]$state.documentTabCount -eq 1 -and ([int]$state.activeTabCount -ne 1 -or -not [bool]$state.activeFilenameMatches)) -or ([int]$state.pageCount -gt 0 -and [int]$state.pageCount -ne $ExpectedPages) -or [int]$state.alertCount -gt 0 -or [int]$state.passwordDialogCount -gt 0
        if ($wrongDocument) { throw "The native picker did not open the exact allowlisted plain fixture; diagnostic=$($diagnostic | ConvertTo-Json -Compress)" }
        if ([int]$state.documentTabCount -eq 1 -and [int]$state.activeTabCount -eq 1 -and [bool]$state.activeFilenameMatches -and [int]$state.pageInputCount -eq 1 -and [int]$state.pageCount -eq $ExpectedPages) { return [pscustomobject]$diagnostic }
        if ($SleepProvider) { & $SleepProvider $script:ReadingPins.UiAutomationPollMilliseconds } else { Start-Sleep -Milliseconds $script:ReadingPins.UiAutomationPollMilliseconds }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    }
    throw "The exact allowlisted plain fixture did not appear after native picker close; diagnostic=$($diagnostic | ConvertTo-Json -Compress)"
}

function Get-ReadingOwnedExecutableDiagnostic {
    param([object[]]$Owned,[string]$ApplicationPath,[string]$EdgeDriverPath,[int]$TrustedConsoleHostCount = 0)
    if ($Owned.Count -gt $script:ReadingPins.OwnedProcessMaximum) { throw 'Reading-tools owned executable inventory exceeded its process-count cap.' }
    $application = [IO.Path]::GetFullPath($ApplicationPath); $edgeDriver = [IO.Path]::GetFullPath($EdgeDriverPath)
    $applications = 0; $edgeDrivers = 0; $webViews = 0; $consoleHosts = 0; $unknown = 0; $missingPaths = 0
    $unknownHashes = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($process in $Owned) {
        $pathProperty = $process.PSObject.Properties['Path']
        if ($null -eq $pathProperty -or [string]::IsNullOrWhiteSpace([string]$pathProperty.Value)) { $missingPaths++; $unknown++; continue }
        try { $path = [IO.Path]::GetFullPath([string]$pathProperty.Value); $name = [IO.Path]::GetFileName($path) } catch { $missingPaths++; $unknown++; continue }
        if ([string]::IsNullOrWhiteSpace($name)) { $missingPaths++; $unknown++; continue }
        if ($path.Equals($application,[StringComparison]::OrdinalIgnoreCase)) { $applications++; continue }
        if ($path.Equals($edgeDriver,[StringComparison]::OrdinalIgnoreCase)) { $edgeDrivers++; continue }
        if ($name.Equals('msedgewebview2.exe',[StringComparison]::OrdinalIgnoreCase)) { $webViews++; continue }
        if ($TrustedConsoleHostCount -gt 0 -and $name.Equals('conhost.exe',[StringComparison]::OrdinalIgnoreCase)) { $consoleHosts++; continue }
        $unknown++
        $leafBytes = [Text.UTF8Encoding]::new($false).GetBytes($name.ToLowerInvariant())
        $null = $unknownHashes.Add((Get-ReadingSha256 -Bytes $leafBytes))
    }
    $sortedHashes = @($unknownHashes | Sort-Object)
    $reportedHashes = @($sortedHashes | Select-Object -First $script:ReadingPins.UnknownExecutableHashMaximum)
    return [pscustomobject][ordered]@{
        schemaVersion = 1
        capturedCount = [int]$Owned.Count
        applicationCount = $applications
        edgeDriverCount = $edgeDrivers
        webViewCount = $webViews
        consoleHostCount = $consoleHosts
        unknownCount = $unknown
        missingPathCount = $missingPaths
        unknownLeafHashCount = [int]$reportedHashes.Count
        unknownLeafHashes = $reportedHashes
        unknownLeafHashesTruncated = $sortedHashes.Count -gt $script:ReadingPins.UnknownExecutableHashMaximum
        parentCategoryCountsAvailable = $false
        parentCategoryCounts = @()
    }
}

function Assert-ReadingOwnedExecutables {
    param(
        [object[]]$Owned,
        [string]$ApplicationPath,
        [string]$EdgeDriverPath,
        [Parameter(Mandatory = $true)][int]$RootProcessId,
        [string]$SystemDirectory,
        [scriptblock]$ConsoleHostSignatureProvider,
        [scriptblock]$ConsoleHostVersionInfoProvider
    )
    $trustedConsoleHostCount = Assert-TrustedConsoleHostTopology -Owned $Owned -RootProcessId $RootProcessId -SystemDirectory $SystemDirectory -SignatureProvider $ConsoleHostSignatureProvider -VersionInfoProvider $ConsoleHostVersionInfoProvider
    $diagnostic = Get-ReadingOwnedExecutableDiagnostic -Owned $Owned -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -TrustedConsoleHostCount $trustedConsoleHostCount
    if ($diagnostic.unknownCount -ne 0 -or $diagnostic.missingPathCount -ne 0) {
        $summary = $diagnostic | ConvertTo-Json -Depth 4 -Compress
        throw "Reading-tools captured an unexpected descendant executable; diagnostic=$summary"
    }
    if ($diagnostic.applicationCount -ne 1 -or $diagnostic.edgeDriverCount -ne 1 -or $diagnostic.webViewCount -lt 1 -or $diagnostic.consoleHostCount -ne $trustedConsoleHostCount) { throw 'Reading-tools did not capture the exact app, EdgeDriver, WebView, and trusted console-host descendant topology.' }
}

function Assert-ReadingProfileScope {
    param([string]$ProfileRoot,[string]$SettingsRoot,[string]$Binding)
    $profile = [IO.Path]::GetFullPath($ProfileRoot).TrimEnd('\'); $settings = [IO.Path]::GetFullPath($SettingsRoot).TrimEnd('\')
    if ($profile.Equals($settings,[StringComparison]::OrdinalIgnoreCase) -or $profile.StartsWith($settings + '\',[StringComparison]::OrdinalIgnoreCase) -or $settings.StartsWith($profile + '\',[StringComparison]::OrdinalIgnoreCase)) { throw 'Reading-tools profile and settings roots are not exclusive.' }
    Assert-NoReparseAncestors -Path $profile; Assert-NoReparseAncestors -Path $settings
    $settingsEntries = @(Get-ChildItem -LiteralPath $settings -Force)
    if ($settingsEntries.Count -ne 1 -or $settingsEntries[0].PSIsContainer -or $settingsEntries[0].Name -cne 'upgrade-sentinel.json' -or ($settingsEntries[0].Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Reading-tools found stale, linked, or unrelated application-settings siblings.' }
    if ($Binding -cnotmatch '^(session-capability|owned-webview)-(requested-profile|requested-ebwebview)$') { throw 'Reading-tools did not bind the fresh requested WebView profile.' }
    $entries = @(Get-ChildItem -LiteralPath $profile -Force)
    $active = if ($Binding.EndsWith('requested-ebwebview',[StringComparison]::Ordinal)) {
        if ($entries.Count -ne 1 -or -not $entries[0].PSIsContainer -or $entries[0].Name -cne 'EBWebView') { throw 'The requested child WebView profile has stale or unrelated siblings.' }
        $entries[0].FullName
    } else {
        if ($entries.Count -eq 0) { throw 'The requested WebView profile remained empty.' }
        $profile
    }
    $pending = [Collections.Generic.Stack[string]]::new(); $pending.Push($active); $count = 0
    while ($pending.Count -gt 0) {
        foreach ($entry in @(Get-ChildItem -LiteralPath $pending.Pop() -Force)) {
            $count++; if ($count -gt 20000) { throw 'The requested WebView profile exceeded its inventory cap.' }
            if (($entry.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) { throw 'The requested WebView profile contains a reparse point.' }
            if ($entry.PSIsContainer) { $pending.Push($entry.FullName) }
        }
    }
    if ($count -eq 0) { throw 'The bound requested WebView profile remained empty.' }
}

function Wait-ReadingDocument {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][int]$Pages,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $script = "const i=document.querySelector('img[alt=`"Page 1`"]'),n=document.querySelector('input[aria-label=`"Page number`"]');return {dialog:document.querySelectorAll('dialog[aria-labelledby=`"password-title`"]').length,images:!!i&&i.complete&&i.naturalWidth>0&&i.naturalHeight>0,pages:[...document.querySelectorAll('span')].some(x=>x.textContent.trim()==='/ $Pages'),page:n?.value||''};"
    return Wait-WebDriverOracle -SessionId $SessionId -Script $script -Deadline $Deadline -Kind 'user PDF render' -Predicate { param($v) [int]$v.dialog -eq 0 -and [bool]$v.images -and [bool]$v.pages -and [string]$v.page -ceq '1' }
}

function Get-ReadingEnableTextSelectionScript {
    return @'
const b=[...document.querySelectorAll('button[aria-label="Select text on page"]')].filter(x=>!x.disabled);if(b.length===1)b[0].click();return {count:b.length,clicked:b.length===1};
'@
}

function Enable-ReadingTextSelection {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$StateProvider,[scriptblock]$UtcNowProvider)
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    if ($now -ge $Deadline) { throw 'The text-selection mode deadline expired before its exact action.' }
    $state = if ($StateProvider) { & $StateProvider } else { Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script (Get-ReadingEnableTextSelectionScript) }
    $properties = @($state.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if (($properties -join ',') -cne 'clicked,count' -or ($state.count -isnot [int] -and $state.count -isnot [long]) -or [int]$state.count -ne 1 -or $state.clicked -isnot [bool] -or -not [bool]$state.clicked) { throw 'The exact installed text-selection mode control was unavailable.' }
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    if ($now -ge $Deadline) { throw 'The text-selection mode action completed after its deadline.' }
}

function Close-ActiveReadingDocument {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $closed = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const t=[...document.querySelectorAll('div')].filter(x=>x.querySelector(':scope>button>span')&&x.querySelector(':scope>button[aria-label^=`"Close `"]'));const a=t.filter(x=>x.className.includes('selectedTab'));const b=a.length===1?a[0].querySelector(':scope>button[aria-label^=`"Close `"]'):null;if(b)b.click();return !!b;"
    if ($closed -isnot [bool] -or -not $closed) { throw 'The active document close control was unavailable.' }
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'document close' -Script "return document.querySelectorAll('input[aria-label=`"Page number`"]').length===0;" -Predicate { param($v) $v -is [bool] -and $v }
}

function Set-ReadingPasswordInput {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][string]$Password,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $escaped = $Password | ConvertTo-Json -Compress
    $script = "const i=document.querySelector('dialog[aria-labelledby=`"password-title`"] input[type=`"password`"]');if(!i)return false;const s=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;s.call(i,$escaped);i.dispatchEvent(new Event('input',{bubbles:true}));return i.value.length===$($Password.Length);"
    $set = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script $script
    if ($set -isnot [bool] -or -not $set) { throw 'The password input did not accept the bounded test value.' }
}

function Submit-ReadingPassword {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $clicked = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const d=document.querySelector('dialog[aria-labelledby=`"password-title`"]'),b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Open PDF'&&!x.disabled):[];if(b.length===1)b[0].click();return b.length===1;"
    if ($clicked -isnot [bool] -or -not $clicked) { throw 'The password submit control was unavailable.' }
}

function Cancel-ReadingPassword {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $clicked = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const d=document.querySelector('dialog[aria-labelledby=`"password-title`"]'),b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Cancel'&&!x.disabled):[];if(b.length===1)b[0].click();return b.length===1;"
    if ($clicked -isnot [bool] -or -not $clicked) { throw 'The password cancel control was unavailable.' }
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'password cancellation' -Script "return {dialog:document.querySelectorAll('dialog[aria-labelledby=`"password-title`"]').length,documents:document.querySelectorAll('input[aria-label=`"Page number`"]').length};" -Predicate { param($v) [int]$v.dialog -eq 0 -and [int]$v.documents -eq 0 }
}

function Invoke-RealInstalledReadingTools {
    param([string]$ApplicationPath,[string]$TauriDriverPath,[string]$EdgeDriverPath,[string]$ProfileRoot,[string]$SettingsRoot,[string]$PlainFixture,[string]$ProtectedFixture,[string]$ExpectedEdgeDriverVersion,[string]$ExpectedRuntimeVersion)
    $null = Wait-FixedWebDriverPortsFree -Deadline ([datetime]::UtcNow.AddMilliseconds($script:ReadingPins.PortReleaseTimeoutMilliseconds))
    $launchDeadline = New-ReadingPhaseDeadline -Phase launch
    $driverCapture = $null; $driver = $null; $sessionId = $null; $captured = @(); $result = $null
    $startedAfter = [datetime]::UtcNow; $sessionDeleteOutcome = 'requestfailed'; $driverExited = $false
    $driverStopOutcome = 'not-invoked'; $processesQuiescent = $false; $remaining = -1; $residualCategory = 'multiple'; $clipboardCleared = $false
    $capturedOutcomes = [pscustomobject]@{ application='absent';tauriDriver='absent';edgeDriver='absent';webview='absent';ocrEngine='absent';other='absent' }
    $residualFacts = [pscustomobject]@{ ownership='none';application=$false;tauriDriver=$false;edgeDriver=$false;webview=$false;ocrEngine=$false;other=$false }
    try {
        $driverCapture = Start-BoundedDiscardProcess -Path $TauriDriverPath -Arguments @("--port=$($script:LaunchPins.WebDriverPort)","--native-port=$($script:LaunchPins.NativeDriverPort)","--native-driver=$EdgeDriverPath") -Environment (Get-MinimalWindowsProcessEnvironment)
        $driverCapture.Start(); $driver = $driverCapture.Process
        $status = $null
        do {
            try { $status = Invoke-BoundedLoopbackJson -Method GET -Path '/status' -Deadline $launchDeadline; if ($status.value.ready) { break } } catch { }
            if ($driver.HasExited) { throw 'Pinned tauri-driver exited before reading-tools readiness.' }
            Start-Sleep -Milliseconds 200
        } while ([datetime]::UtcNow -lt $launchDeadline)
        if ($null -eq $status -or -not [bool]$status.value.ready) { throw 'Pinned tauri-driver did not become ready for reading tools.' }
        $nativeDriverVersion = Wait-NativeDriverStatus -ExpectedVersion $ExpectedEdgeDriverVersion -Deadline $launchDeadline -TauriDriver $driver
        $sessionBody = [ordered]@{ capabilities=[ordered]@{ alwaysMatch=[ordered]@{ browserName='wry';'tauri:options'=[ordered]@{ application=$ApplicationPath;args=@();webviewOptions=[ordered]@{userDataFolder=$ProfileRoot} } } } }
        $session = Invoke-BoundedLoopbackJson -Method POST -Path '/session' -Body $sessionBody -Deadline $launchDeadline
        if ($null -eq $session.value -or [string]$session.value.sessionId -cnotmatch '^[A-Za-z0-9-]+$') { throw 'Reading-tools WebDriver session identifier was invalid.' }
        $sessionId = [string]$session.value.sessionId
        $capabilities = $session.value.capabilities
        $capabilityKeys = @($capabilities.PSObject.Properties.Name | Sort-Object -CaseSensitive)
        if ($capabilityKeys.Count -eq 0 -or $capabilityKeys.Count -gt 32 -or @($capabilityKeys | Where-Object { $_ -cnotmatch '^[A-Za-z0-9:._-]{1,64}$' }).Count -ne 0) { throw 'Reading-tools returned an unsafe session capability-key set.' }
        $vendorDriver = $capabilities.PSObject.Properties['msedge.msedgedriverVersion']
        if ($null -ne $vendorDriver -and ([string]$vendorDriver.Value -split '\s+')[0] -cne $ExpectedEdgeDriverVersion) { throw 'Reading-tools session EdgeDriver capability disagrees with its trusted receipt.' }
        $returnedRuntimeVersion = [string]$capabilities.browserVersion
        $runtimeParts = $returnedRuntimeVersion.Split('.'); $expectedRuntimeParts = $ExpectedRuntimeVersion.Split('.')
        if ($returnedRuntimeVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or $runtimeParts.Count -ne 4 -or ($runtimeParts[0..2] -join '.') -cne ($expectedRuntimeParts[0..2] -join '.')) { throw 'Reading-tools WebView2 runtime capability disagrees with its trusted receipt.' }
        $returnedUserData = if ($null -ne $capabilities.PSObject.Properties['msedge.userDataDir']) { [string]$capabilities.'msedge.userDataDir' } else { '' }
        $homeOracle = Wait-WebDriverOracle -SessionId $sessionId -Deadline $launchDeadline -Kind 'reading-tools home' -Script (Get-ReadingHomeScript) -Predicate { param($v) [bool]$v.ready -and [string]$v.title -ceq 'PDF Workstation' -and [int]$v.sample -eq 1 }
        do {
            $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
            $apps = @($captured | Where-Object { [string]$_.Path -and [IO.Path]::GetFullPath([string]$_.Path).Equals([IO.Path]::GetFullPath($ApplicationPath),[StringComparison]::OrdinalIgnoreCase) })
            if ($apps.Count -eq 1) { break }
            if ($apps.Count -gt 1) { throw 'More than one owned installed application process was found.' }
            Start-Sleep -Milliseconds 100
        } while ([datetime]::UtcNow -lt $launchDeadline)
        if ($apps.Count -ne 1) { throw 'The owned installed application process was unavailable for process-bound UI Automation.' }
        $applicationProcessId = [int]$apps[0].ProcessId; $applicationProcessStartUtcTicks = [long]$apps[0].StartTicks
        Assert-ReadingOwnedExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -RootProcessId $driver.Id
        $profileBinding = if ($returnedUserData) { 'session-capability-' + (Get-ExactProfileBinding -Candidate $returnedUserData -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot) } else { Get-OwnedProfileBinding -Owned $captured -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot }
        Assert-ReadingProfileScope -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -Binding $profileBinding

        Assert-ReadingPhaseTransition -Deadline $launchDeadline -Phase launch
        $plainPickerDeadline = New-ReadingPhaseDeadline -Phase native-picker
        $plainPickerControlCategory = Open-ReadingUserFile -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationProcessStartUtcTicks -Path $PlainFixture -Deadline $plainPickerDeadline
        Assert-ReadingPhaseTransition -Deadline $plainPickerDeadline -Phase native-picker
        $plainProductDeadline = New-ReadingPhaseDeadline -Phase product
        $postOpen = Wait-ReadingPostOpenState -SessionId $sessionId -ExpectedName 'reading-source.pdf' -ExpectedPages 6 -NativeControlCategory $plainPickerControlCategory -Deadline $plainProductDeadline
        $null = Wait-ReadingDocument -SessionId $sessionId -Pages 6 -Deadline $plainProductDeadline
        Enable-ReadingTextSelection -SessionId $sessionId -Deadline $plainProductDeadline
        try {
            $layer = Wait-WebDriverOracle -SessionId $sessionId -Deadline $plainProductDeadline -Kind 'embedded text layer' -Script "const p=document.querySelector('[aria-label=`"Page 1`"]'),l=p?.querySelector('[data-testid=`"text-layer`"]'),i=p?.querySelector('img[alt=`"Page 1`"]');return {ready:!!l&&!!i&&i.complete,glyphs:l?.querySelectorAll('[data-geometry-index]').length||0};" -Predicate { param($v) [bool]$v.ready -and [int]$v.glyphs -gt 20 }
        } catch { throw "Embedded text layer verification failed after the sanitized post-open receipt; diagnostic=$($postOpen | ConvertTo-Json -Compress)" }
        $selectionScript = @'
const target='A place for your PDFs.',p=document.querySelector('[aria-label="Page 1"]'),l=p?.querySelector('[data-testid="text-layer"]'),i=p?.querySelector('img[alt="Page 1"]');if(!l||!i)return {ready:false,exact:false,utf16:0,rects:0,inside:false};const s=[...l.querySelectorAll('[data-geometry-index]')],joined=s.map(x=>x.textContent||'').join(''),start=joined.indexOf(target);if(start<0)return {ready:true,exact:false,utf16:0,rects:0,inside:false};let offset=0,first=null,last=null,firstOffset=0,lastOffset=0;for(const x of s){const text=x.textContent||'',next=offset+text.length;if(first===null&&start>=offset&&start<next){first=x;firstOffset=start-offset}if(start+target.length>offset&&start+target.length<=next){last=x;lastOffset=start+target.length-offset;break}offset=next}if(!first||!last)return {ready:true,exact:false,utf16:0,rects:0,inside:false};const r=document.createRange();r.setStart(first.firstChild,firstOffset);r.setEnd(last.firstChild,lastOffset);const sel=getSelection();sel.removeAllRanges();sel.addRange(r);const rects=[...r.getClientRects()],page=i.getBoundingClientRect(),inside=rects.length>0&&rects.every(x=>x.width>0&&x.height>0&&x.left>=page.left-2&&x.top>=page.top-2&&x.right<=page.right+2&&x.bottom<=page.bottom+2);return {ready:true,exact:sel.toString()===target,utf16:sel.toString().length,rects:rects.length,inside};
'@
        $selection = Invoke-WebDriverScript -SessionId $sessionId -Script $selectionScript -Deadline $plainProductDeadline
        if (-not [bool]$selection.ready -or -not [bool]$selection.exact -or [int]$selection.utf16 -ne $script:ReadingPins.SelectionText.Length -or [int]$selection.rects -lt 1 -or -not [bool]$selection.inside) { throw 'Installed embedded-text selection or its on-page geometry was not exact.' }
        $expectedClipboard = Get-ReadingTextReceipt -Text $script:ReadingPins.SelectionText
        Invoke-ReadingClipboardSta -Operation set-sentinel
        $sentinelReceipt = Invoke-ReadingClipboardSta -Operation receipt
        $expectedSentinel = Get-ReadingTextReceipt -Text 'reading-verifier-sentinel'
        if ([int]$sentinelReceipt.utf8Bytes -ne [int]$expectedSentinel.utf8Bytes -or [string]$sentinelReceipt.sha256 -cne [string]$expectedSentinel.sha256) { throw 'The Windows clipboard sentinel was not established before the real copy action.' }
        Send-ReadingKeyChord -SessionId $sessionId -Key c -Deadline $plainProductDeadline
        $clipboard = Wait-ReadingClipboardReceipt -Expected $expectedClipboard -Deadline $plainProductDeadline

        Send-ReadingKeyChord -SessionId $sessionId -Key f -Deadline $plainProductDeadline
        $null = Wait-WebDriverOracle -SessionId $sessionId -Deadline $plainProductDeadline -Kind 'Find panel' -Script "return document.querySelectorAll('aside[aria-label=`"Find in document`"] input[aria-label=`"Find in document`"]').length===1;" -Predicate { param($v) $v -is [bool] -and $v }
        $searchScript = @'
const i=document.querySelector('aside[aria-label="Find in document"] input[aria-label="Find in document"]');if(!i)return false;const s=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set;s.call(i,'Sample document');i.dispatchEvent(new Event('input',{bubbles:true}));i.closest('form').requestSubmit();return true;
'@
        if (-not (Invoke-WebDriverScript -SessionId $sessionId -Script $searchScript -Deadline $plainProductDeadline)) { throw 'The installed Find form was unavailable.' }
        $search = Wait-WebDriverOracle -SessionId $sessionId -Deadline $plainProductDeadline -Kind 'Find results' -Script "const a=document.querySelector('aside[aria-label=`"Find in document`"]');return {status:[...a.querySelectorAll('[role=`"status`"]')].map(x=>x.textContent.trim()).includes('6 matching pages.'),results:[...a.querySelectorAll('button')].filter(x=>/^Go to page [1-6]$/.test(x.textContent.trim())).length,next:[...a.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Next matching page'&&!x.disabled).length};" -Predicate { param($v) [bool]$v.status -and [int]$v.results -eq 6 -and [int]$v.next -eq 1 }
        for ($step = 0; $step -lt 2; $step++) {
            $next = Invoke-WebDriverScript -SessionId $sessionId -Deadline $plainProductDeadline -Script "const a=document.querySelector('aside[aria-label=`"Find in document`"]'),b=a?[...a.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Next matching page'&&!x.disabled):[];if(b.length===1)b[0].click();return b.length===1;"
            if ($next -isnot [bool] -or -not $next) { throw 'Find next-page navigation control was unavailable.' }
        }
        $geometryScript = @'
const a=document.querySelector('aside[aria-label="Find in document"]'),live=[...a.querySelectorAll('[aria-live="polite"]')].map(x=>x.textContent.trim()),p=document.querySelector('[aria-label="Page 2"]'),i=p?.querySelector('img[alt="Page 2"]'),l=p?.querySelector('[data-testid="text-layer"]'),h=p?[...p.querySelectorAll('[data-testid="search-highlight"]')]:[];if(!i||!l)return {ready:false,nav:false,count:0,inside:false,paired:false};const page=i.getBoundingClientRect(),glyphs=[...l.querySelectorAll('[data-geometry-index]')],target='Sample document',joined=glyphs.map(x=>x.textContent||'').join(''),start=joined.indexOf(target),indexes=[];let offset=0;for(let n=0;n<glyphs.length;n++){const text=glyphs[n].textContent||'',next=offset+text.length;if(start>=0&&next>start&&offset<start+target.length&&glyphs[n].hasAttribute('data-angle'))indexes.push(n);offset=next}const inside=h.length>0&&h.every(x=>{const r=x.getBoundingClientRect();return r.width>0&&r.height>0&&r.left>=page.left-2&&r.top>=page.top-2&&r.right<=page.right+2&&r.bottom<=page.bottom+2}),paired=h.length===indexes.length&&h.every((x,n)=>{const r=x.getBoundingClientRect(),g=glyphs[indexes[n]].getBoundingClientRect();return Math.abs(r.left-g.left)<=2&&Math.abs(r.top-g.top)<=2&&Math.abs(r.width-g.width)<=2&&Math.abs(r.height-g.height)<=2});return {ready:i.complete&&i.naturalWidth>0,nav:live.includes('Matching page 2 of 6'),count:h.length,inside,paired};
'@
        $geometry = Wait-WebDriverOracle -SessionId $sessionId -Script $geometryScript -Deadline $plainProductDeadline -Kind 'search highlight geometry' -Predicate { param($v) [bool]$v.ready -and [bool]$v.nav -and [int]$v.count -gt 5 -and [bool]$v.inside -and [bool]$v.paired }
        Close-ActiveReadingDocument -SessionId $sessionId -Deadline $plainProductDeadline

        Assert-ReadingPhaseTransition -Deadline $plainProductDeadline -Phase product
        $passwordPickerDeadline = New-ReadingPhaseDeadline -Phase native-picker
        $null = Open-ReadingUserFile -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationProcessStartUtcTicks -Path $ProtectedFixture -Deadline $passwordPickerDeadline
        Assert-ReadingPhaseTransition -Deadline $passwordPickerDeadline -Phase native-picker
        $passwordProductDeadline = New-ReadingPhaseDeadline -Phase product
        $prompt = Wait-WebDriverOracle -SessionId $sessionId -Deadline $passwordProductDeadline -Kind 'password prompt' -Script "const d=document.querySelector('dialog[aria-labelledby=`"password-title`"]'),i=d?.querySelector('input[type=`"password`"]');return {one:!!d&&!!i&&i.value==='',message:[...d?.querySelectorAll('p')||[]].some(x=>x.textContent.trim()==='Your password is used only to open this document.')};" -Predicate { param($v) [bool]$v.one -and [bool]$v.message }
        Set-ReadingPasswordInput -SessionId $sessionId -Password 'wrong password' -Deadline $passwordProductDeadline; Submit-ReadingPassword -SessionId $sessionId -Deadline $passwordProductDeadline
        $wrong = Wait-WebDriverOracle -SessionId $sessionId -Deadline $passwordProductDeadline -Kind 'wrong password retry' -Script "const d=document.querySelector('dialog[aria-labelledby=`"password-title`"]'),i=d?.querySelector('input[type=`"password`"]');return {prompt:!!d,cleared:i?.value==='',alert:[...d?.querySelectorAll('[role=`"alert`"]')||[]].some(x=>x.textContent.trim()==='Incorrect password. Try again.')};" -Predicate { param($v) [bool]$v.prompt -and [bool]$v.cleared -and [bool]$v.alert }
        Set-ReadingPasswordInput -SessionId $sessionId -Password $script:ReadingPins.Password -Deadline $passwordProductDeadline; Submit-ReadingPassword -SessionId $sessionId -Deadline $passwordProductDeadline
        $null = Wait-ReadingDocument -SessionId $sessionId -Pages 1 -Deadline $passwordProductDeadline
        Close-ActiveReadingDocument -SessionId $sessionId -Deadline $passwordProductDeadline
        Assert-ReadingPhaseTransition -Deadline $passwordProductDeadline -Phase product
        $reopenPickerDeadline = New-ReadingPhaseDeadline -Phase native-picker
        $null = Open-ReadingUserFile -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationProcessStartUtcTicks -Path $ProtectedFixture -Deadline $reopenPickerDeadline
        Assert-ReadingPhaseTransition -Deadline $reopenPickerDeadline -Phase native-picker
        $reopenProductDeadline = New-ReadingPhaseDeadline -Phase product
        $reopened = Wait-WebDriverOracle -SessionId $sessionId -Deadline $reopenProductDeadline -Kind 'transient password reopen' -Script "const d=document.querySelector('dialog[aria-labelledby=`"password-title`"]'),i=d?.querySelector('input[type=`"password`"]');return {prompt:!!d,empty:i?.value===''};" -Predicate { param($v) [bool]$v.prompt -and [bool]$v.empty }
        Cancel-ReadingPassword -SessionId $sessionId -Deadline $reopenProductDeadline
        Start-Sleep -Milliseconds 250
        Assert-ReadingPhaseTransition -Deadline $reopenProductDeadline -Phase product
        $postCancelPickerDeadline = New-ReadingPhaseDeadline -Phase native-picker
        $null = Open-ReadingUserFile -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationProcessStartUtcTicks -Path $ProtectedFixture -Deadline $postCancelPickerDeadline
        Assert-ReadingPhaseTransition -Deadline $postCancelPickerDeadline -Phase native-picker
        $postCancelProductDeadline = New-ReadingPhaseDeadline -Phase product
        $afterCancel = Wait-WebDriverOracle -SessionId $sessionId -Deadline $postCancelProductDeadline -Kind 'post-cancel password reopen' -Script "const d=document.querySelector('dialog[aria-labelledby=`"password-title`"]'),i=d?.querySelector('input[type=`"password`"]');return {prompt:!!d,empty:i?.value===''};" -Predicate { param($v) [bool]$v.prompt -and [bool]$v.empty }
        Cancel-ReadingPassword -SessionId $sessionId -Deadline $postCancelProductDeadline

        $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
        Assert-ReadingOwnedExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -RootProcessId $driver.Id
        Assert-ReadingProfileScope -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -Binding $profileBinding
        if ($driverCapture.Exceeded) { throw 'Reading-tools WebDriver diagnostic output exceeded its discarded cap.' }
        $result = [pscustomobject]@{
            nativeDriverVersion = $nativeDriverVersion
            returnedRuntimeVersion = $returnedRuntimeVersion
            profileBinding = $profileBinding
            processBoundPickerVerified = $true
            nativeFilenameControlCategory = [string]$plainPickerControlCategory
            postOpenDocumentTabCount = [int]$postOpen.documentTabCount
            postOpenActiveTabCount = [int]$postOpen.activeTabCount
            postOpenActiveFilenameMatched = [bool]$postOpen.activeFilenameMatches
            postOpenPageCount = [int]$postOpen.pageCount
            postOpenRenderedPageCount = [int]$postOpen.renderedPageCount
            postOpenImageCount = [int]$postOpen.imageCount
            postOpenLoadedImageCount = [int]$postOpen.loadedImageCount
            postOpenTextLayerCount = [int]$postOpen.textLayerCount
            postOpenPageOneGlyphCount = [int]$postOpen.pageOneGlyphCount
            postOpenPageTextStatusCount = [int]$postOpen.pageTextStatusCount
            textSelectionModeActivated = $true
            programmaticDomSelectionVerified = $true
            selectionGeometryInsidePage = [bool]$selection.inside
            selectionClientRectCount = [int]$selection.rects
            selectedTextUtf8Bytes = [int]$expectedClipboard.utf8Bytes
            selectedTextSha256 = [string]$expectedClipboard.sha256
            windowsClipboardVerified = $true
            clipboardUtf8Bytes = [int]$clipboard.utf8Bytes
            clipboardSha256 = [string]$clipboard.sha256
            searchResultCount = [int]$search.results
            searchNavigationVerified = [bool]$geometry.nav
            searchHighlightCount = [int]$geometry.count
            searchHighlightInsidePage = [bool]$geometry.inside
            searchHighlightMatchesGlyphGeometry = [bool]$geometry.paired
            passwordPromptVerified = [bool]$prompt.one
            wrongPasswordRetryVerified = [bool]$wrong.alert
            passwordInputClearedAfterWrong = [bool]$wrong.cleared
            correctPasswordOpenVerified = $true
            passwordReentryRequiredAfterCloseAndReopen = [bool]$reopened.prompt
            passwordCancelVerified = $true
            postCancelPromptVerified = [bool]$afterCancel.prompt
        }
    } finally {
        try {
            $clipboardCleared = Clear-ReadingClipboardAndVerify -Deadline ([datetime]::UtcNow.AddSeconds(10))
        } catch { $clipboardCleared = $false }
        $cleanupDeadline = [datetime]::UtcNow.AddSeconds(10)
        if ($sessionId) { $sessionDeleteOutcome = Invoke-SessionDeleteOutcome -SessionId $sessionId -Deadline $cleanupDeadline }
        if ($driver) {
            try { $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured) } catch { }
            $processCleanupDeadline = [datetime]::UtcNow.AddMilliseconds($script:LaunchPins.CleanupProcessTimeoutMilliseconds)
            $capturedOutcomes = Stop-OwnedLaunchProcesses -TauriDriver $driver -Captured $captured -Deadline $processCleanupDeadline
            $driverStopOutcome = [string]$capturedOutcomes.rootOutcome
            $quiescence = Wait-LaunchProcessQuiescence -Deadline $processCleanupDeadline
            $processesQuiescent = [bool]$quiescence.stable
            $remainingProcesses = @($quiescence.processes); $remaining = $remainingProcesses.Count
            $residualCategory = Get-LaunchResidualCategory -Processes $remainingProcesses
            $residualFacts = Get-LaunchResidualFacts -Processes $remainingProcesses -Captured $captured
            $driverExited = [bool]$driver.HasExited
        }
        if ($driverCapture) { $driverCapture.Dispose() }
    }
    if (-not $clipboardCleared) { throw 'Installed reading-tools verification did not clear and verify the Windows clipboard.' }
    Assert-LaunchCleanupState -Result $result -SessionDeleteOutcome $sessionDeleteOutcome -DriverExited $driverExited -RelevantProcessesClear ($processesQuiescent -and $residualCategory -ceq 'none') -DriverStopOutcome $driverStopOutcome -ResidualCategory $residualCategory -CapturedApplication $capturedOutcomes.application -CapturedTauriDriver $capturedOutcomes.tauriDriver -CapturedEdgeDriver $capturedOutcomes.edgeDriver -CapturedWebView $capturedOutcomes.webview -CapturedOcrEngine $capturedOutcomes.ocrEngine -CapturedOther $capturedOutcomes.other -ResidualOwnership $residualFacts.ownership -ResidualApplication $residualFacts.application -ResidualTauriDriver $residualFacts.tauriDriver -ResidualEdgeDriver $residualFacts.edgeDriver -ResidualWebView $residualFacts.webview -ResidualOcrEngine $residualFacts.ocrEngine -ResidualOther $residualFacts.other
    if ($null -eq $result -or $sessionDeleteOutcome -cne 'verified' -or -not $driverExited -or -not $processesQuiescent -or $remaining -ne 0) { throw 'Installed reading-tools cleanup did not reach its exact zero-process state.' }
    $result | Add-Member -NotePropertyName sessionDeleted -NotePropertyValue $true
    $result | Add-Member -NotePropertyName ownedProcessTreeStopped -NotePropertyValue $true
    $result | Add-Member -NotePropertyName relevantProcessesRemaining -NotePropertyValue 0
    return $result
}

function Invoke-InstalledReadingTools {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,
        [Parameter(Mandatory = $true)]$ApplicationReceipt,
        [Parameter(Mandatory = $true)][string]$WebDriverRoot,
        [Parameter(Mandatory = $true)][string]$ProfileRoot,
        [Parameter(Mandatory = $true)][string]$SettingsRoot,
        [Parameter(Mandatory = $true)][string]$PlainFixture,
        [Parameter(Mandatory = $true)][string]$ProtectedFixture,
        [Parameter(Mandatory = $true)]$PlainReceipt,
        [Parameter(Mandatory = $true)]$ProtectedReceipt,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher
    )
    $receipt = Get-Content -LiteralPath (Join-Path $WebDriverRoot 'webdriver-receipt.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-WebDriverReceipt -Receipt $receipt
    $tauriDriver = Join-Path $WebDriverRoot 'tauri-driver-install/bin/tauri-driver.exe'
    $edgeDriver = Join-Path $WebDriverRoot 'edge-driver/msedgedriver.exe'
    Assert-ReadingFileReceipt -Path $ApplicationPath -Bytes ([uint64]$ApplicationReceipt.bytes) -Sha256 ([string]$ApplicationReceipt.sha256) -Kind 'Installed signed reading-tools application'
    $null = Assert-TrustedWindowsSignature -Path $ApplicationPath -ExpectedPublisher $ExpectedPublisher
    Assert-ReadingFileReceipt -Path $tauriDriver -Bytes ([uint64]$receipt.tauriDriver.bytes) -Sha256 ([string]$receipt.tauriDriver.sha256) -Kind 'Pinned reading-tools tauri-driver'
    Assert-ReadingFileReceipt -Path $edgeDriver -Bytes ([uint64]$receipt.edgeDriver.bytes) -Sha256 ([string]$receipt.edgeDriver.sha256) -Kind 'Pinned reading-tools EdgeDriver'
    $null = Assert-TrustedWindowsSignature -Path $edgeDriver -ExpectedPublisher $script:LaunchPins.EdgePublisher
    foreach ($fixture in @(@($PlainFixture,$PlainReceipt,'Plain reading fixture'),@($ProtectedFixture,$ProtectedReceipt,'Protected reading fixture'))) {
        Assert-ReadingFileReceipt -Path ([string]$fixture[0]) -Bytes ([uint64]$fixture[1].bytes) -Sha256 ([string]$fixture[1].sha256) -Kind ([string]$fixture[2])
    }
    if (Test-Path -LiteralPath $ProfileRoot) { throw 'Reading-tools WebView profile must be fresh.' }
    if (-not (Test-Path -LiteralPath $SettingsRoot -PathType Container)) { throw 'The controlled application settings root is missing after the signed upgrade proof.' }
    Assert-NoReparseAncestors -Path $SettingsRoot
    [IO.Directory]::CreateDirectory($ProfileRoot) | Out-Null
    $preSettingsEntries = @(Get-ChildItem -LiteralPath $SettingsRoot -Force)
    if ($preSettingsEntries.Count -ne 1 -or $preSettingsEntries[0].PSIsContainer -or $preSettingsEntries[0].Name -cne 'upgrade-sentinel.json' -or ($preSettingsEntries[0].Attributes -band [IO.FileAttributes]::ReparsePoint) -or @(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -ne 0) { throw 'Reading-tools profile roots were not fresh, exclusive, and sentinel-only before launch.' }
    $settingsSentinel = $preSettingsEntries[0].FullName
    $settingsSentinelBytes = [uint64]$preSettingsEntries[0].Length
    $settingsSentinelSha256 = Get-ExactSha256 -Path $settingsSentinel
    if (@(Get-LaunchProcessSnapshot).Count -ne 0) { throw 'A relevant application or WebDriver process existed before reading-tools verification.' }
    $uiResult = $null
    try {
        $uiResult = Invoke-RealInstalledReadingTools -ApplicationPath $ApplicationPath -TauriDriverPath $tauriDriver -EdgeDriverPath $edgeDriver -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -PlainFixture $PlainFixture -ProtectedFixture $ProtectedFixture -ExpectedEdgeDriverVersion ([string]$receipt.edgeDriver.version) -ExpectedRuntimeVersion ([string]$receipt.webView2RuntimeVersion)
    } finally {
        Assert-ReadingFileReceipt -Path $settingsSentinel -Bytes $settingsSentinelBytes -Sha256 $settingsSentinelSha256 -Kind 'Post-flow upgrade settings sentinel'
        foreach ($fixture in @(@($PlainFixture,$PlainReceipt,'Post-flow plain reading fixture'),@($ProtectedFixture,$ProtectedReceipt,'Post-flow protected reading fixture'))) {
            Assert-ReadingFileReceipt -Path ([string]$fixture[0]) -Bytes ([uint64]$fixture[1].bytes) -Sha256 ([string]$fixture[1].sha256) -Kind ([string]$fixture[2])
        }
        $fixtureFiles = @(Get-ChildItem -LiteralPath ([IO.Path]::GetDirectoryName($PlainFixture)) -File -Force)
        if ($fixtureFiles.Count -ne 2 -or @($fixtureFiles | Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0 }).Count -ne 0) { throw 'The reading fixture inventory changed or contains a reparse point.' }
    }
    return $uiResult
}
