. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')

function ConvertTo-SignedInputUInt64 {
    param([string]$Value,[string]$Kind)
    [uint64]$parsed = 0
    if ($Value -cnotmatch '^[1-9][0-9]{0,19}$' -or -not [uint64]::TryParse($Value,[Globalization.NumberStyles]::None,[Globalization.CultureInfo]::InvariantCulture,[ref]$parsed) -or $parsed -eq 0) {
        throw "$Kind must be one positive base-10 integer."
    }
    return $parsed
}

function Get-SignedCheckInputs {
    param(
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
    if ($TargetVersion -cnotmatch '^(0|[1-9][0-9]{0,3})\.(0|[1-9][0-9]{0,3})\.(0|[1-9][0-9]{0,3})$') { throw 'Target version must be plain MAJOR.MINOR.PATCH.' }
    if ($SourceRevision -cnotmatch '^[a-f0-9]{40}$') { throw 'Source revision must be a full lowercase 40-character commit id.' }
    if ($ArtifactName -cne "signed-ocr-installer-$SourceRevision") { throw 'Artifact name does not match the source revision.' }
    if ($ArtifactDigest -cnotmatch '^sha256:[a-f0-9]{64}$') { throw 'Artifact digest must be sha256: plus 64 lowercase hexadecimal characters.' }
    if ($InstallerName -cne "PDF Workstation_${TargetVersion}_x64-setup.exe") { throw 'Installer name does not match the target version.' }
    if ($InstallerSha256 -cnotmatch '^[A-F0-9]{64}$') { throw 'Installer SHA-256 must be 64 uppercase hexadecimal characters.' }
    if ($RecordSha256 -cnotmatch '^[A-F0-9]{64}$') { throw 'Record SHA-256 must be 64 uppercase hexadecimal characters.' }
    return [pscustomobject][ordered]@{
        TargetVersion = $TargetVersion
        SourceRevision = $SourceRevision
        ArtifactRunId = ConvertTo-SignedInputUInt64 $ArtifactRunId 'Artifact run id'
        ArtifactId = ConvertTo-SignedInputUInt64 $ArtifactId 'Artifact id'
        ArtifactName = $ArtifactName
        ArtifactBytes = ConvertTo-SignedInputUInt64 $ArtifactBytes 'Artifact bytes'
        ArtifactDigest = $ArtifactDigest
        InstallerName = $InstallerName
        InstallerBytes = ConvertTo-SignedInputUInt64 $InstallerBytes 'Installer bytes'
        InstallerSha256 = $InstallerSha256
        RecordBytes = ConvertTo-SignedInputUInt64 $RecordBytes 'Record bytes'
        RecordSha256 = $RecordSha256
    }
}

function Get-SignedSourceBlobs {
    param([Parameter(Mandatory = $true)][string]$ProjectRoot,[Parameter(Mandatory = $true)][string]$Revision,[Parameter(Mandatory = $true)][string[]]$Paths)
    & git -C $ProjectRoot cat-file -e "$Revision^{commit}"
    if ($LASTEXITCODE -ne 0) { throw 'The exact signed source commit is unavailable.' }
    $blobs = [ordered]@{}
    foreach ($path in $Paths) {
        $blob = (& git -C $ProjectRoot rev-parse "${Revision}:$path").Trim()
        if ($LASTEXITCODE -ne 0 -or $blob -cnotmatch '^[a-f0-9]{40,64}$') { throw "Could not resolve a signed source blob: $path" }
        $blobs[$path] = $blob
    }
    return $blobs
}

function Get-SignedSourceVersion {
    param([Parameter(Mandatory = $true)][string]$ProjectRoot,[Parameter(Mandatory = $true)][string]$Revision)
    $text = (& git -C $ProjectRoot show "${Revision}:src-tauri/tauri.conf.json") -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'Could not read tauri.conf.json at the signed source revision.' }
    $version = [string](($text | ConvertFrom-Json).version)
    if ($version -cnotmatch '^[0-9]+\.[0-9]+\.[0-9]+$') { throw 'The signed source revision has no plain tauri.conf.json version.' }
    return $version
}

function Get-SignedBaseResourceTargets {
    param([Parameter(Mandatory = $true)][string]$TargetSourceRoot,[Parameter(Mandatory = $true)]$Inputs)
    $head = (& git -C $TargetSourceRoot rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $head -cne $Inputs.SourceRevision) { throw 'The target source checkout is not the signed source revision.' }
    if ((Get-SignedSourceVersion -ProjectRoot $TargetSourceRoot -Revision $Inputs.SourceRevision) -cne $Inputs.TargetVersion) { throw 'The signed source revision version does not match the target version.' }
    $resourceMap = Get-BaseBundleResourceMap -ProjectRoot $TargetSourceRoot
    $targets = @($resourceMap.Entries | ForEach-Object { [string]$_.Target } | Sort-Object -CaseSensitive)
    if ($targets.Count -eq 0) { throw 'The signed source revision expands to no base resources.' }
    return $targets
}

function Add-SignedPinFields {
    param([Parameter(Mandatory = $true)]$Pins,[Parameter(Mandatory = $true)]$Inputs,[string[]]$BaseResourceTargets)
    $values = [ordered]@{
        TargetVersion = $Inputs.TargetVersion
        SignedArtifactId = $Inputs.ArtifactId
        SignedRunId = $Inputs.ArtifactRunId
        SignedSourceRevision = $Inputs.SourceRevision
        SignedInstallerName = $Inputs.InstallerName
        SignedInstallerBytes = $Inputs.InstallerBytes
        SignedInstallerSha256 = $Inputs.InstallerSha256
        SignedRecordName = 'artifact-verification.json'
        SignedRecordBytes = $Inputs.RecordBytes
        SignedRecordSha256 = $Inputs.RecordSha256
        LeakIds = "$($Inputs.ArtifactId)|$($Inputs.ArtifactRunId)"
    }
    if ($null -ne $BaseResourceTargets) { $values['BaseResourceTargets'] = @($BaseResourceTargets) }
    foreach ($key in $values.Keys) { $Pins[$key] = $values[$key] }
}
