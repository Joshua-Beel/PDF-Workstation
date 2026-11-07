param(
    [string]$PinsPath = (Join-Path $PSScriptRoot 'qpdf\pins.json'),
    [string]$WorkRoot = '',
    [string]$ZipCache = '',
    [string]$ZipPath = '',
    [switch]$RecordFileHashes
)
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

function Fail([string]$Message) { throw "setup-qpdf: $Message" }
function Get-Sha256([string]$Path) { (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant() }
function Assert-NotReparse([string]$Path) {
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { Fail "$Path is a reparse point (junction, link or placeholder); refusing to use it." }
}
function Assert-Zip([string]$Path, $Pin) {
    Assert-NotReparse $Path
    $size = (Get-Item -LiteralPath $Path -Force).Length
    if ($size -ne [long]$Pin.size) { Fail "zip size $size does not match the pinned $($Pin.size); nothing was extracted." }
    $hash = Get-Sha256 $Path
    if ($hash -ne $Pin.sha256.ToLowerInvariant()) { Fail "zip SHA-256 $hash does not match the pinned $($Pin.sha256); nothing was extracted." }
}
function Remove-TreeNoFollow([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        if ($item.PSIsContainer) { [IO.Directory]::Delete($item.FullName, $false) } else { [IO.File]::Delete($item.FullName) }
        return
    }
    if ($item.PSIsContainer) { foreach ($child in @(Get-ChildItem -LiteralPath $item.FullName -Force)) { Remove-TreeNoFollow $child.FullName } }
    if ($item.Attributes -band [IO.FileAttributes]::ReadOnly) { $item.Attributes = [IO.FileAttributes]::Normal }
    if ($item.PSIsContainer) { [IO.Directory]::Delete($item.FullName, $false) } else { [IO.File]::Delete($item.FullName) }
}
# Extracts every entry under $Destination after checking that it stays inside it: no absolute or drive paths,
# no '..' segments, no colons (alternate data streams). The .NET extractor writes plain files only.
function Expand-Checked([string]$Zip, [string]$Destination) {
    $root = [IO.Path]::GetFullPath($Destination).TrimEnd('\') + '\'
    $archive = [IO.Compression.ZipFile]::OpenRead($Zip)
    try {
        foreach ($entry in $archive.Entries) {
            $name = $entry.FullName
            if ($name -match '^[\\/]' -or $name -match ':' -or (($name -split '[\\/]') -contains '..')) { Fail "zip entry '$name' would leave the extraction folder; nothing was used." }
            $target = [IO.Path]::GetFullPath((Join-Path $Destination $name))
            if (-not $target.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { Fail "zip entry '$name' resolves outside the extraction folder; nothing was used." }
            if ($name.EndsWith('/') -or $name.EndsWith('\')) { New-Item -ItemType Directory -Path $target -Force | Out-Null; continue }
            New-Item -ItemType Directory -Path (Split-Path -Parent $target) -Force | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $false)
        }
    } finally { $archive.Dispose() }
    foreach ($item in @(Get-ChildItem -LiteralPath $Destination -Recurse -Force)) {
        if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { Fail "extracted item $($item.FullName) is a reparse point; nothing was used." }
    }
}

$pins = Get-Content -LiteralPath $PinsPath -Raw | ConvertFrom-Json
if (-not $WorkRoot) {
    if ($env:RUNNER_TEMP) { $WorkRoot = Join-Path $env:RUNNER_TEMP 'qpdf' }
    else { Fail 'pass -WorkRoot (for example E:\Coding Projects\Smacrobat\tmp\qpdf-run); only CI has a default (RUNNER_TEMP).' }
}
$WorkRoot = [IO.Path]::GetFullPath($WorkRoot)
if (-not $env:CI -and $WorkRoot -match '^[Cc]:') { Fail "work root $WorkRoot is on C:; local runs keep everything under E:\Coding Projects\Smacrobat." }
New-Item -ItemType Directory -Path $WorkRoot -Force | Out-Null
Assert-NotReparse $WorkRoot
# Earlier runs' extracts are removed first: nothing extracted is ever reused.
foreach ($old in @(Get-ChildItem -LiteralPath $WorkRoot -Directory -Force)) { if ($old.Name -match '^[0-9a-f]{32}$') { Remove-TreeNoFollow $old.FullName } }

# The zip: an explicit local file (tests), else a cached copy re-verified every run, else a fresh download.
$run = Join-Path $WorkRoot ([guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $run | Out-Null
try {
    $zip = Join-Path $run 'qpdf.zip'
    if ($ZipPath) { Copy-Item -LiteralPath $ZipPath -Destination $zip }
    elseif ($ZipCache -and (Test-Path -LiteralPath (Join-Path $ZipCache "qpdf-$($pins.version).zip") -PathType Leaf)) { Copy-Item -LiteralPath (Join-Path $ZipCache "qpdf-$($pins.version).zip") -Destination $zip }
    else {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        Invoke-WebRequest -Uri $pins.zip.url -OutFile $zip -UseBasicParsing
    }
    Assert-Zip $zip $pins.zip
    if ($ZipCache -and -not $ZipPath) {
        New-Item -ItemType Directory -Path $ZipCache -Force | Out-Null
        Assert-NotReparse $ZipCache
        Copy-Item -LiteralPath $zip -Destination (Join-Path $ZipCache "qpdf-$($pins.version).zip") -Force
    }

    # Fresh extract every run; the extracted folder is never cached.
    $extract = Join-Path $run 'extract'
    New-Item -ItemType Directory -Path $extract | Out-Null
    Expand-Checked $zip $extract
    $binaries = @(Get-ChildItem -LiteralPath $extract -Recurse -File -Force | Where-Object { $_.Extension -in '.exe', '.dll' })
    $relative = { param($file) $file.FullName.Substring($extract.Length + 1).Replace('\', '/') }
    if ($RecordFileHashes -or $null -eq $pins.files) {
        $record = [ordered]@{}
        foreach ($file in ($binaries | Sort-Object FullName)) { $record[(& $relative $file)] = Get-Sha256 $file.FullName }
        Write-Output ($record | ConvertTo-Json)
        Remove-TreeNoFollow $run
        if ($null -eq $pins.files) { [Console]::Error.WriteLine('setup-qpdf: per-file hashes are not pinned yet. Review the hashes above, add them to the pins file as "files", and run again. qpdf was not made available.'); exit 3 }
        exit 0
    }
    $pinned = @{}
    foreach ($property in $pins.files.PSObject.Properties) { $pinned[$property.Name] = $property.Value.ToLowerInvariant() }
    foreach ($file in $binaries) {
        $name = & $relative $file
        if (-not $pinned.ContainsKey($name)) { Fail "unpinned executable or library $name in the zip; nothing was used." }
        if ((Get-Sha256 $file.FullName) -ne $pinned[$name]) { Fail "$name does not match its pinned SHA-256; nothing was used." }
        $pinned.Remove($name)
    }
    if ($pinned.Count) { Fail "pinned files missing from the zip: $($pinned.Keys -join ', '); nothing was used." }
    $exe = @($binaries | Where-Object { $_.Name -eq 'qpdf.exe' })
    if ($exe.Count -ne 1) { Fail 'the zip must hold exactly one qpdf.exe.' }
    # The caller sets QPDF_EXE from this line; the run folder is removed by the next run or by CI cleanup.
    Write-Output "QPDF_EXE=$($exe[0].FullName)"
} catch {
    Remove-TreeNoFollow $run
    throw
}
