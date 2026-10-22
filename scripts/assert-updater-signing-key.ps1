param(
    [Parameter(Mandatory = $true)][string]$SignaturePath,
    [Parameter(Mandatory = $true)][string]$SourceRoot
)
$ErrorActionPreference = 'Stop'

function Get-MinisignKeyId {
    param([Parameter(Mandatory = $true)][string]$Base64Text, [Parameter(Mandatory = $true)][ValidateSet('signature', 'public key')][string]$Kind)
    try { $text = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($Base64Text.Trim())) } catch { throw "Updater $Kind is not base64." }
    $lines = @($text -split "`r?`n")
    if ($lines.Count -lt 2 -or -not $lines[0].StartsWith('untrusted comment: ', [StringComparison]::Ordinal)) { throw "Updater $Kind is not a minisign $Kind." }
    try { $raw = [Convert]::FromBase64String($lines[1].Trim()) } catch { throw "Updater $Kind payload is not base64." }
    $expectedLength = if ($Kind -ceq 'signature') { 74 } else { 42 }
    $algorithm = if ($raw.Length -ge 2) { [string][char]$raw[0] + [char]$raw[1] } else { '' }
    $allowed = if ($Kind -ceq 'signature') { @('Ed', 'ED') } else { @('Ed') }
    if ($raw.Length -ne $expectedLength -or $allowed -cnotcontains $algorithm) { throw "Updater $Kind has an unsupported minisign layout." }
    return [BitConverter]::ToString($raw, 2, 8).Replace('-', '')
}

$pinPath = Join-Path $SourceRoot 'scripts/updater-signing-key.json'
$pin = Get-Content -LiteralPath $pinPath -Raw -Encoding UTF8 | ConvertFrom-Json
$names = @($pin.PSObject.Properties.Name)
if ($names.Count -ne 1 -or $names[0] -cne 'releaseSigningPublicKey') { throw 'scripts/updater-signing-key.json must hold exactly releaseSigningPublicKey.' }
$expected = Get-MinisignKeyId -Base64Text ([string]$pin.releaseSigningPublicKey) -Kind 'public key'
$actual = Get-MinisignKeyId -Base64Text (Get-Content -LiteralPath $SignaturePath -Raw -Encoding UTF8) -Kind 'signature'
if ($actual -cne $expected) { throw 'The updater signature was not made with the key that installed apps trust (scripts/updater-signing-key.json); they would refuse this update.' }
Write-Output ('UPDATER_SIGNING_KEY_ID {0}' -f $expected)
