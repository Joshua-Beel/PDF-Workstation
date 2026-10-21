import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

function runPowerShell(source: string) {
  return spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(source, 'utf16le').toString('base64')], {
    encoding: 'utf8',
    timeout: 30_000,
  });
}

describe('opt-in OCR installer packaging', () => {
  it('keeps default and Azure release behavior explicitly separate from opt-in proof modes', () => {
    const installer = readFileSync('scripts/build-installer.ps1', 'utf8');
    expect(installer).toContain('Remove-Item Env:PDF_WORKSTATION_OCR_SETUP_ROOT');
    expect(installer.indexOf('Remove-Item Env:PDF_WORKSTATION_OCR_SETUP_ROOT')).toBeLessThan(installer.indexOf('if ($OcrSetupRoot)'));
    expect(installer).toContain("if ($AzureSigning -and $UnsignedLocal)");
    expect(installer).toContain("if ($OcrSetupRoot -and -not ($AzureSigning -or $UnsignedLocal))");
    expect(installer).toContain("if (-not $UnsignedLocal -and -not $ArtifactOnly -and -not $env:TAURI_SIGNING_PRIVATE_KEY)");
    expect(installer).toContain('Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY -ErrorAction SilentlyContinue');
    expect(installer).toContain('Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -ErrorAction SilentlyContinue');
    expect(installer.indexOf('Remove-Item Env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD')).toBeLessThan(installer.indexOf('npm.cmd run tauri'));
    expect(installer).toContain("if (-not $UnsignedLocal -and -not $ArtifactOnly -and ($ReleaseOcr -or -not $OcrSetupRoot))");
    expect(installer).toContain('New-InstallerOverrideConfig -SignCommand $signCommand -UnsignedLocal:$UnsignedLocal -ArtifactOnly:$ArtifactOnly');
    expect(installer).toContain('$ReleaseOcr = [bool]($AzureSigning -and $OcrSetupRoot -and -not $ArtifactOnly)');
    expect(installer).not.toContain('Azure-signed OCR requires the explicit artifact-only mode.');
    expect(installer.indexOf('$env:PDF_WORKSTATION_OCR_SETUP_ROOT = $ocrPlan.Root')).toBeLessThan(installer.indexOf('npm.cmd run tauri'));
    expect(installer).toContain('New-SignedOcrSetup');
    expect(installer).toContain('New-SignableBaseResourcePlan');
    expect(installer).not.toMatch(/AZURE_(TENANT|CLIENT|SIGNING)_[A-Z_]+\s*=\s*['"][^'"]+['"]/);
  });

  it('rejects resource tampering, path ambiguity, archive duplicates, and unsafe extraction receipts', () => {
    const check = String.raw`
      $ErrorActionPreference = 'Stop'
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/ocr/installer-package.ps1
      . ./scripts/ocr/windows-signing.ps1
      $evidenceCandidate = Join-Path (Resolve-Path target) ('ocr-installer-focused-' + [Guid]::NewGuid().ToString('N'))
      $evidence = Resolve-FreshInstallerOutput -ProjectRoot (Get-Location) -OutputRoot ($evidenceCandidate + '\')
      if($evidence.EndsWith('\')) { throw 'Fresh installer output retained a trailing separator.' }
      $receiptProbe = Join-Path $evidence 'cargo-target/release/bundle/nsis/setup.exe'
      if($receiptProbe.Substring($evidence.Length + 1).Replace('\','/') -cne 'cargo-target/release/bundle/nsis/setup.exe') { throw 'Fresh installer output corrupted a relative receipt path.' }
      $source = Join-Path $evidence 'source'
      $items = @(
        @('engine','engine/bin/tesseract.exe','bin/tesseract.exe','engine'),
        @('model','engine/tessdata/eng.traineddata','tessdata/eng.traineddata','model'),
        @('tess','engine/licenses/Tesseract-Apache-2.0.txt','licenses/Tesseract-Apache-2.0.txt','license'),
        @('lep','engine/licenses/Leptonica-BSD-2-Clause.txt','licenses/Leptonica-BSD-2-Clause.txt','license'),
        @('eng','engine/licenses/eng-fast-Apache-2.0.txt','licenses/eng-fast-Apache-2.0.txt','license')
      )
      $files = @()
      foreach($item in $items) {
        $path = Join-Path $source $item[1]
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path)) | Out-Null
        [IO.File]::WriteAllBytes($path, [Text.Encoding]::UTF8.GetBytes('owned-' + $item[0]))
        $receipt = [pscustomobject]@{ Path=$item[1]; Bytes=[uint64](Get-Item $path).Length; Sha256=Get-ExactSha256 $path }
        $files += [pscustomobject]@{ Role=$item[3]; Receipt=$receipt; Source=$path; Suffix=$item[2] }
      }
      $identity = 'a' * 64
      $plan = [pscustomobject]@{ Identity=$identity; Files=$files; Engine=$files[0].Receipt; Model=$files[1].Receipt; Licenses=@($files[2].Receipt,$files[3].Receipt,$files[4].Receipt); ManifestSha256=('b'*64) }

      $entries = @(Get-OcrBundleEntries -Plan $plan)
      if($entries.Count -ne 5 -or $entries[0].Source -cne $plan.Files[0].Source -or $entries[0].Target -cne ('resources/ocr/'+$identity+'/bin/tesseract.exe')) { throw 'Bundle entries did not use the locked plan files.' }
      $base = Get-BaseBundleResourceMap -ProjectRoot (Get-Location)
      $baseAgain = Get-BaseBundleResourceMap -ProjectRoot (Get-Location)
      Assert-BaseResourceMapStable -Before $base -After $baseAgain
      $baseAgain.Entries[0].Target += '.changed'
      $rejected = $false; try { Assert-BaseResourceMapStable -Before $base -After $baseAgain } catch { $rejected=$true }
      if(-not $rejected) { throw 'Changed base resource map was accepted.' }
      $signableInput = Get-BaseBundleResourceMap -ProjectRoot (Get-Location)
      $pdfiumInput = @($signableInput.Entries | Where-Object Target -CEQ 'resources/pdfium/bin/pdfium.dll')
      if($pdfiumInput.Count -ne 1) { throw 'Pinned PDFium input is not unique.' }
      $pdfiumLayout=Get-PeSigningLayout -Bytes ([IO.File]::ReadAllBytes($pdfiumInput[0].Source))
      if($pdfiumLayout.CertificateOffset -ne 0 -or $pdfiumLayout.CertificateBytes -ne 0) { throw 'Pinned PDFium input already has a PE certificate table.' }
      $pdfiumHash = Get-ExactSha256 $pdfiumInput[0].Source
      $signableOutput = Join-Path $evidence 'signable-output'; [IO.Directory]::CreateDirectory($signableOutput) | Out-Null
      $signable = New-SignableBaseResourcePlan -Base $signableInput -OutputRoot $signableOutput
      $signablePdfium = @($signable.Entries | Where-Object IsSignablePe)
      if($signable.Count -ne $signableInput.Count -or $signable.Targets.Count -ne $signableInput.Targets.Count -or $signablePdfium.Count -ne 1) { throw 'Signable base-resource map changed target cardinality.' }
      if($signable.Map.Contains([IO.Path]::GetFullPath($pdfiumInput[0].Source)) -or -not $signable.Map.Contains([IO.Path]::GetFullPath($signablePdfium[0].Source))) { throw 'Signable map retained the original PDFium source.' }
      if($signablePdfium[0].Target -cne $pdfiumInput[0].Target -or (Get-ExactSha256 $signablePdfium[0].Source) -cne $pdfiumHash -or (Get-ExactSha256 $pdfiumInput[0].Source) -cne $pdfiumHash) { throw 'PDFium staging changed source bytes or target.' }
      $artifactOverride = New-InstallerOverrideConfig -SignCommand @{cmd='mock';args=@('%1')} -ArtifactOnly -ResourceMap $signable.Map
      if($artifactOverride.bundle.createUpdaterArtifacts -ne $false) { throw 'Artifact-only override did not disable updater artifacts.' }
      $baseCount = $base.Count
      $map = Add-OcrBundleResources -Base $base -Entries $entries -Identity $identity
      if($map.Count -ne $baseCount + 5 -or @($map.Values | Where-Object {$_ -like 'resources/ocr/*'}).Count -ne 5) { throw 'Resource map is incomplete.' }
      if(@($map.Keys | Where-Object {$_.IndexOfAny([char[]]'*?[') -ge 0}).Count -ne 0) { throw 'Resource map retained a wildcard.' }

      $cargo = Join-Path $evidence 'cargo-one'
      $staged = @(Copy-OcrPackageFiles -Plan $plan -CargoTarget $cargo)
      if($staged.Count -ne 5) { throw 'Exact stage failed.' }
      [IO.File]::WriteAllText((Join-Path $cargo ('release/resources/ocr/'+$identity+'/extra.txt')), 'extra')
      $rejected = $false; try { $null=Assert-OcrPackageFiles -Plan $plan -ResourceRoot (Join-Path $cargo ('release/resources/ocr/'+$identity)) } catch { $rejected=$true }
      if(-not $rejected) { throw 'Extra resource was accepted.' }
      $cargo2 = Join-Path $evidence 'cargo-two'; $null=Copy-OcrPackageFiles -Plan $plan -CargoTarget $cargo2
      [IO.File]::WriteAllText((Join-Path $cargo2 ('release/resources/ocr/'+$identity+'/bin/tesseract.exe')), 'tampered')
      $rejected = $false; try { $null=Assert-OcrPackageFiles -Plan $plan -ResourceRoot (Join-Path $cargo2 ('release/resources/ocr/'+$identity)) } catch { $rejected=$true }
      if(-not $rejected) { throw 'Tampered resource was accepted.' }
      $bad = @(Get-OcrBundleEntries -Plan $plan); $bad[0].Target = 'resources/ocr/' + ('c'*64) + '/bin/tesseract.exe'
      $base2 = Get-BaseBundleResourceMap -ProjectRoot (Get-Location)
      $rejected = $false; try { $null=Add-OcrBundleResources -Base $base2 -Entries $bad -Identity $identity } catch { $rejected=$true }
      if(-not $rejected) { throw 'Wrong OCR identity target was accepted.' }

      $archive = @('Path = ignored-header.exe','Type = Nsis','----------')
      $archive += @($base.Entries | ForEach-Object {'Path = app/' + $_.Target})
      $archive += @($plan.Files | ForEach-Object {'Path = app/resources/ocr/' + $identity + '/' + $_.Suffix})
      $paths = @(Convert-SevenZipInventory -Lines $archive)
      Assert-ArchiveResourceInventory -ArchivePaths $paths -BaseEntries $base.Entries -OcrPlan $plan
      $duplicate = $archive + ('Path = APP/RESOURCES/OCR/' + $identity + '/BIN/TESSERACT.EXE')
      $rejected = $false; try { $null=Convert-SevenZipInventory -Lines $duplicate } catch { $rejected=$true }
      if(-not $rejected) { throw 'Case-colliding archive path was accepted.' }
      $traversal = $archive + 'Path = app/../escape'
      $rejected = $false; try { $null=Convert-SevenZipInventory -Lines $traversal } catch { $rejected=$true }
      if(-not $rejected) { throw 'Archive traversal was accepted.' }
      $ads = $archive + 'Path = app/file.txt:stream'
      $rejected = $false; try { $null=Convert-SevenZipInventory -Lines $ads } catch { $rejected=$true }
      if(-not $rejected) { throw 'Archive ADS was accepted.' }
      $missingBase = @($paths | Select-Object -Skip 1)
      $rejected = $false; try { Assert-ArchiveResourceInventory -ArchivePaths $missingBase -BaseEntries $base.Entries -OcrPlan $plan } catch { $rejected=$true }
      if(-not $rejected) { throw 'Missing base resource was accepted.' }
      $defaultExactRoot = @('resources/ocr/'+$identity+'/bin/tesseract.exe')
      $rejected = $false; try { Assert-ArchiveResourceInventory -ArchivePaths $defaultExactRoot -BaseEntries @() -OcrPlan $null } catch { $rejected=$true }
      if(-not $rejected) { throw 'Exact-root OCR path evaded default omission.' }

      $extract = Join-Path $evidence 'fake-extract/app'
      foreach($file in $plan.Files) {
        $dest=Join-Path $extract ('resources/ocr/'+$identity+'/'+$file.Suffix)
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($dest))|Out-Null
        [IO.File]::Copy($file.Source,$dest,$false)
      }
      $safeReceipts = @(Assert-ExtractedOcrPackage -ExtractionRoot (Join-Path $evidence 'fake-extract') -Plan $plan)
      if($safeReceipts.Count -ne 5 -or @($safeReceipts | ForEach-Object {$_.PSObject.Properties.Name} | Where-Object {$_ -eq 'extractedPath'}).Count) { throw 'Extraction receipt exposed a private path.' }

      Write-Output ('PASS ' + $evidence)
    `;
    const result = runPowerShell(check);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 35_000);
});
