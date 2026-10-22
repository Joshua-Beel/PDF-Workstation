param(
    [string]$SignaturePath,
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
    $id = [byte[]]$raw[2..9]
    [Array]::Reverse($id)
    return [BitConverter]::ToString($id).Replace('-', '')
}

$pin = Get-Content -LiteralPath (Join-Path $SourceRoot 'scripts/updater-signing-key.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$names = @($pin.PSObject.Properties.Name | Sort-Object)
if ($names.Count -ne 2 -or $names[0] -cne 'forbiddenEmbeddedPublicKey' -or $names[1] -cne 'releaseSigningPublicKey') { throw 'scripts/updater-signing-key.json must hold exactly releaseSigningPublicKey and forbiddenEmbeddedPublicKey.' }
$expected = Get-MinisignKeyId -Base64Text ([string]$pin.releaseSigningPublicKey) -Kind 'public key'

$configured = [string](Get-Content -LiteralPath (Join-Path $SourceRoot 'src-tauri/tauri.conf.json') -Raw -Encoding UTF8 | ConvertFrom-Json).plugins.updater.pubkey
$configuredId = Get-MinisignKeyId -Base64Text $configured -Kind 'public key'
if ($null -ne $pin.forbiddenEmbeddedPublicKey) {
    $forbiddenId = Get-MinisignKeyId -Base64Text ([string]$pin.forbiddenEmbeddedPublicKey) -Kind 'public key'
    if ($configuredId -ceq $forbiddenId) { throw "plugins.updater.pubkey in tauri.conf.json is still the retired key $forbiddenId; this release must embed the new updater public key." }
}

if ($SignaturePath) {
    $actual = Get-MinisignKeyId -Base64Text (Get-Content -LiteralPath $SignaturePath -Raw -Encoding UTF8) -Kind 'signature'
    if ($actual -cne $expected) { throw "The updater signature was made with key $actual, not $expected, the key that installed apps trust (scripts/updater-signing-key.json); they would refuse this update." }
}
Write-Output ('UPDATER_SIGNING_KEY_ID {0} EMBEDDED_KEY_ID {1}' -f $expected, $configuredId)
