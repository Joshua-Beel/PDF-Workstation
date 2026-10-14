[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $OutputDir,
    [string] $SpecPath = (Join-Path $PSScriptRoot 'accuracy-synthetic.json')
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
Add-Type -AssemblyName System.Drawing

$spec = Get-Content -LiteralPath $SpecPath -Raw -Encoding UTF8 | ConvertFrom-Json
$out = [IO.Path]::GetFullPath($OutputDir)
New-Item -ItemType Directory -Force -Path $out | Out-Null
$utf8 = New-Object System.Text.UTF8Encoding($false)
$width = 1275
$entries = @()

foreach ($variant in $spec.variants) {
    $family = New-Object System.Drawing.FontFamily([string]$variant.font)
    $pixels = [single]($variant.pt * 150 / 72)
    $font = New-Object System.Drawing.Font($family, $pixels, [System.Drawing.FontStyle]::Regular, [System.Drawing.GraphicsUnit]::Pixel)
    $lineHeight = [int][Math]::Ceiling($pixels * 1.6)
    $margin = 90
    $height = $margin * 2 + $lineHeight * @($spec.lines).Count
    $bitmap = New-Object System.Drawing.Bitmap($width, $height, [System.Drawing.Imaging.PixelFormat]::Format24bppRgb)
    $bitmap.SetResolution(150, 150)
    $g = [System.Drawing.Graphics]::FromImage($bitmap)
    try {
        $g.Clear([System.Drawing.Color]::White)
        $g.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit
        if ($variant.PSObject.Properties['skewDegrees']) {
            $g.TranslateTransform($width / 2, $height / 2)
            $g.RotateTransform([single]$variant.skewDegrees)
            $g.TranslateTransform(-$width / 2, -$height / 2)
        }
        $y = $margin
        foreach ($line in $spec.lines) {
            $g.DrawString([string]$line, $font, [System.Drawing.Brushes]::Black, [single]$margin, [single]$y, [System.Drawing.StringFormat]::GenericTypographic)
            $y += $lineHeight
        }
    } finally { $g.Dispose() }
    if ($variant.PSObject.Properties['noiseDensity']) {
        $random = New-Object System.Random([int]$variant.noiseSeed)
        $count = [int]($width * $height * [double]$variant.noiseDensity)
        for ($i = 0; $i -lt $count; $i++) {
            $x = $random.Next(0, $width); $yy = $random.Next(0, $height)
            $v = $random.Next(0, 256)
            $bitmap.SetPixel($x, $yy, [System.Drawing.Color]::FromArgb($v, $v, $v))
        }
    }
    $png = Join-Path $out "$($variant.id).png"
    $bitmap.Save($png, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose(); $font.Dispose(); $family.Dispose()
    $truth = Join-Path $out "$($variant.id).txt"
    [IO.File]::WriteAllText($truth, ((@($spec.lines) -join "`n") + "`n"), $utf8)
    $entries += [ordered]@{ id = [string]$variant.id; image = "$($variant.id).png"; truth = "$($variant.id).txt"; note = "$($variant.font) $($variant.pt) pt, synthetic" }
}

$manifest = [ordered]@{ schemaVersion = 1; scope = [string]$spec.scope; entries = $entries }
[IO.File]::WriteAllText((Join-Path $out 'manifest.json'), (($manifest | ConvertTo-Json -Depth 5) + "`n"), $utf8)
Write-Output "Wrote $($entries.Count) synthetic fixtures to $out"
