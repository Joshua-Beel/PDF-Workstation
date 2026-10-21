import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

function runPowerShell(source: string) {
  return spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(source, 'utf16le').toString('base64')], {
    encoding: 'utf8',
    timeout: 30_000,
  });
}

describe('signed OCR installer infrastructure', () => {
  it('keeps artifact-only OCR explicit and gives the release workflow a signed OCR mode that keeps updater artifacts', () => {
    const installer = readFileSync('scripts/build-installer.ps1', 'utf8');
    const workflow = readFileSync('.github/workflows/release.yml', 'utf8');
    expect(installer).toContain('$ReleaseOcr = [bool]($AzureSigning -and $OcrSetupRoot -and -not $ArtifactOnly)');
    expect(installer).toContain('$SignedOcr = [bool]($ArtifactOnly -or $ReleaseOcr)');
    expect(installer).not.toContain('Azure-signed OCR requires the explicit artifact-only mode.');
    expect(installer).toContain('Release OCR bundle must contain exactly the installer updater signature and no manifest.');
    expect(installer).toContain('The release bundle folder must be empty before the release OCR installer is staged.');
    expect(installer).toContain('Staged release asset differs from the verified build output.');
    expect(installer).toContain("if (-not $UnsignedLocal -and -not $ArtifactOnly -and ($ReleaseOcr -or -not $OcrSetupRoot))");
    expect(installer).toMatch(/if \(\$ReleaseOcr\) \{\r?\n\s+\$releaseBundle = Join-Path \$projectRoot 'src-tauri\/target\/release\/bundle\/nsis'/);
    expect(installer).toContain("Join-Path $PSScriptRoot 'ocr/windows-signing.ps1'");
    expect(installer).toContain('New-SignedOcrSetup');
    expect(installer).toContain("cmd = 'artifact-signing-cli'");
    expect(installer).toContain('$env:PDF_WORKSTATION_EXPECTED_PUBLISHER = $ExpectedPublisher');
    expect(installer).toContain('Installer proof SourceRevision does not match the current git HEAD.');
    expect(installer).toContain('sourceRevision = $SourceRevision');
    expect(installer.match(/-ExpectedPublisher \$ExpectedPublisher/g)?.length).toBeGreaterThanOrEqual(5);
    expect(installer).toContain("$_.Name -CEQ 'latest.json' -or $_.Extension -CEQ '.sig'");
    expect(installer).toContain('($UnsignedLocal -or $ArtifactOnly) -and $unexpectedUpdaterArtifacts.Count -ne 0');
    expect(installer).toMatch(/if \(\$SignedOcr\) \{[\s\S]*Assert-TrustedWindowsSignature -Path \$installer[\s\S]*\} else \{[\s\S]*Assert-NotPublisherSigned -Path \$installer/);
    expect(workflow).toContain('-OcrSetupRoot $env:OCR_SETUP_ROOT `');
    expect(workflow).not.toContain('ArtifactOnly');
    expect(workflow).not.toContain('windows-signing.ps1');
  });

  it('rejects the artifact-only mode matrix before reading Azure or updater configuration', () => {
    const check = String.raw`
      $ErrorActionPreference = 'Stop'
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      foreach($name in @('AZURE_SIGNING_ENDPOINT','AZURE_SIGNING_ACCOUNT','AZURE_SIGNING_PROFILE','AZURE_TENANT_ID','AZURE_CLIENT_ID','AZURE_CLIENT_SECRET','TAURI_SIGNING_PRIVATE_KEY','TAURI_SIGNING_PRIVATE_KEY_PASSWORD')) { Remove-Item "Env:$name" -ErrorAction SilentlyContinue }
      $cases = @(
        @(@{UnsignedLocal=$true;OutputRoot='target/missing'}, 'UnsignedLocal requires SourceRevision'),
        @(@{ArtifactOnly=$true}, 'ArtifactOnly requires AzureSigning'),
        @(@{ArtifactOnly=$true;AzureSigning=$true}, 'ArtifactOnly requires AzureSigning'),
        @(@{ArtifactOnly=$true;OcrSetupRoot='missing'}, 'ArtifactOnly requires AzureSigning'),
        @(@{AzureSigning=$true;OcrSetupRoot='missing'}, 'Azure-signed release OCR requires SourceRevision'),
        @(@{AzureSigning=$true;OcrSetupRoot='missing';OutputRoot='target/missing';SourceRevision=('a'*40)}, 'valid explicit ExpectedPublisher'),
        @(@{AzureSigning=$true;OcrSetupRoot='missing';OutputRoot='target/missing';ExpectedPublisher='Test Publisher';SourceRevision='bad'}, 'exact lowercase 40-character git HEAD'),
        @(@{AzureSigning=$true;OcrSetupRoot='missing';OutputRoot='target/missing';ExpectedPublisher='Test Publisher';SourceRevision=('0'*40)}, 'does not match the current git HEAD'),
        @(@{AzureSigning=$true;ExpectedPublisher='Test Publisher'}, 'ExpectedPublisher is supported only for Azure-signed OCR.'),
        @(@{ExpectedPublisher='Test Publisher'}, 'ExpectedPublisher is supported only for Azure-signed OCR.'),
        @(@{SourceRevision=('a'*40)}, 'SourceRevision is supported only for explicit installer proof modes.'),
        @(@{ArtifactOnly=$true;AzureSigning=$true;OcrSetupRoot='missing';OutputRoot='target/missing';ExpectedPublisher=' bad'}, 'valid explicit ExpectedPublisher'),
        @(@{ArtifactOnly=$true;AzureSigning=$true;OcrSetupRoot='missing';OutputRoot='target/missing';ExpectedPublisher='Test Publisher';SourceRevision='bad'}, 'exact lowercase 40-character git HEAD'),
        @(@{ArtifactOnly=$true;AzureSigning=$true;OcrSetupRoot='missing';OutputRoot='target/missing';ExpectedPublisher='Test Publisher';SourceRevision=('0'*40)}, 'does not match the current git HEAD')
      )
      foreach($case in $cases) {
        $message=$null
        $arguments=$case[0]
        try { & ./scripts/build-installer.ps1 @arguments; throw 'Mode was unexpectedly accepted.' } catch { $message=$_.Exception.Message }
        if(-not $message.Contains($case[1])) { throw ('Unexpected mode rejection: ' + $message) }
        if($message -like '*signing setting*' -or $message -like '*signing credential*' -or $message -like '*updater.key*') { throw 'Mode guard read signing configuration.' }
      }
      Write-Output 'Rejected the artifact-only mode matrix before signing configuration.'
    `;
    const result = runPowerShell(check);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('before signing configuration');
  }, 35_000);

  it('binds artifact-only source revision to a clean tracked tree and index', () => {
    const check = String.raw`
      $ErrorActionPreference = 'Stop'
      $scriptPath = '${process.cwd().replaceAll("'", "''")}\\scripts\\build-installer.ps1'
      $tokens=$null;$errors=$null
      $ast=[Management.Automation.Language.Parser]::ParseFile($scriptPath,[ref]$tokens,[ref]$errors)
      $function=$ast.Find({param($node)$node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Assert-InstallerSourceRevision'},$true)
      if(-not $function){throw 'Source revision guard is missing.'}
      Invoke-Expression $function.Extent.Text
      $repository=Join-Path ([IO.Path]::GetTempPath()) ('smacrobat-source-revision-'+[Guid]::NewGuid().ToString('N'))
      [IO.Directory]::CreateDirectory($repository)|Out-Null
      git -C $repository init --quiet
      git -C $repository config user.name 'Smacrobat Test'
      git -C $repository config user.email 'smacrobat-test@example.invalid'
      [IO.File]::WriteAllText((Join-Path $repository 'tracked.txt'),'clean',[Text.UTF8Encoding]::new($false))
      git -C $repository add -- tracked.txt
      git -C $repository commit --quiet -m initial
      $head=(git -C $repository rev-parse HEAD).Trim()
      Assert-InstallerSourceRevision -ProjectRoot $repository -SourceRevision $head
      [IO.File]::WriteAllText((Join-Path $repository 'tracked.txt'),'dirty',[Text.UTF8Encoding]::new($false))
      $rejected=$false;try{Assert-InstallerSourceRevision -ProjectRoot $repository -SourceRevision $head}catch{$rejected=$_.Exception.Message.Contains('clean tracked working tree')}
      if(-not $rejected){throw 'Dirty tracked working tree was accepted.'}
      git -C $repository add -- tracked.txt
      $rejected=$false;try{Assert-InstallerSourceRevision -ProjectRoot $repository -SourceRevision $head}catch{$rejected=$_.Exception.Message.Contains('clean tracked index')}
      if(-not $rejected){throw 'Dirty tracked index was accepted.'}
      Write-Output 'PASS clean source revision guard'
    `;
    const result = runPowerShell(check);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 35_000);

  it('derives the signed identity before compile and skips only the exact trusted engine', () => {
    const check = String.raw`
      $ErrorActionPreference = 'Stop'
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/ocr/installer-package.ps1
      . ./scripts/ocr/windows-signing.ps1
      $evidence = Join-Path (Resolve-Path target) ('signed-ocr-helper-' + [Guid]::NewGuid().ToString('N'))
      [IO.Directory]::CreateDirectory($evidence) | Out-Null
      $source = Join-Path $evidence 'source'
      $output = Join-Path $evidence 'output'
      [IO.Directory]::CreateDirectory($output) | Out-Null
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
        $receipt = [pscustomobject]@{ Path=$item[1]; Bytes=[uint64](Get-Item $path).Length; Sha256=Get-SigningSha256 $path }
        $files += [pscustomobject]@{ Role=$item[3]; Receipt=$receipt; Source=$path; Suffix=$item[2] }
      }
      $manifestPath = Join-Path $source 'engine/ocr-engine-manifest.json'
      $manifest = [ordered]@{ artifacts = [ordered]@{ executable = [ordered]@{ path='engine/bin/tesseract.exe'; bytes=$files[0].Receipt.Bytes; sha256=$files[0].Receipt.Sha256 } } }
      [IO.File]::WriteAllText($manifestPath, ($manifest | ConvertTo-Json -Depth 8), [Text.UTF8Encoding]::new($false))
      $plan = [pscustomobject]@{ Root=$source; ManifestPath=$manifestPath; Files=$files; Engine=$files[0].Receipt; Identity=$files[0].Receipt.Sha256.ToLowerInvariant() }
      $originalHash = Get-SigningSha256 $files[0].Source
      $script:events = [Collections.Generic.List[string]]::new()
      $script:validSignatures = @{}
      $signatureProvider = {
        param($path)
        $full = [IO.Path]::GetFullPath($path)
        $script:events.Add('signature:' + [IO.Path]::GetFileName($full))
        if($script:validSignatures[$full]) { return [pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$true} }
        return [pscustomobject]@{Status='NotSigned';Publisher=$null;HasTimestamp=$false}
      }
      $signer = {
        param($path)
        $script:events.Add('sign:' + [IO.Path]::GetFileName($path))
        $bytes = [IO.File]::ReadAllBytes($path)
        $suffix = [Text.Encoding]::ASCII.GetBytes('|mock-signature|')
        $signed = [byte[]]::new($bytes.Length + $suffix.Length)
        [Array]::Copy($bytes, $signed, $bytes.Length); [Array]::Copy($suffix, 0, $signed, $bytes.Length, $suffix.Length)
        [IO.File]::WriteAllBytes($path, $signed)
        $script:validSignatures[[IO.Path]::GetFullPath($path)] = $true
        return 0
      }
      $verifier = {
        param($projectRoot,$root)
        $script:events.Add('verify-derived-plan')
        $value = Get-Content -LiteralPath (Join-Path $root 'engine/ocr-engine-manifest.json') -Raw -Encoding UTF8 | ConvertFrom-Json
        $enginePath = Join-Path $root $value.artifacts.executable.path
        if([uint64](Get-Item $enginePath).Length -ne [uint64]$value.artifacts.executable.bytes -or (Get-SigningSha256 $enginePath) -cne [string]$value.artifacts.executable.sha256) { throw 'Mock verifier rejected the signed engine receipt.' }
        $derivedFiles = @()
        foreach($file in $plan.Files) {
          $copy = Join-Path $root $file.Receipt.Path
          $receipt = if($file.Role -ceq 'engine') {
            [pscustomobject]@{Path=$file.Receipt.Path;Bytes=[uint64]$value.artifacts.executable.bytes;Sha256=[string]$value.artifacts.executable.sha256}
          } else { $file.Receipt }
          if([uint64](Get-Item $copy).Length -ne [uint64]$receipt.Bytes -or (Get-SigningSha256 $copy) -cne [string]$receipt.Sha256) { throw 'Mock verifier rejected a copied sidecar.' }
          $derivedFiles += [pscustomobject]@{Role=$file.Role;Receipt=$receipt;Source=$copy;Suffix=$file.Suffix}
        }
        return [pscustomobject]@{Root=$root;ManifestPath=(Join-Path $root 'engine/ocr-engine-manifest.json');Files=$derivedFiles;Engine=$derivedFiles[0].Receipt;Identity=$derivedFiles[0].Receipt.Sha256.ToLowerInvariant()}
      }
      $derived = New-SignedOcrSetup -ProjectRoot (Get-Location) -OriginalPlan $plan -OutputRoot $output -Signer $signer -SignatureProvider $signatureProvider -PlanVerifier $verifier -ExpectedPublisher 'Joshua Beel'
      $script:events.Add('compile-captured:' + $derived.Identity)
      $signIndex = $script:events.IndexOf('sign:tesseract.exe')
      $verifyIndex = $script:events.IndexOf('verify-derived-plan')
      $compileIndex = $script:events.IndexOf('compile-captured:' + $derived.Identity)
      if($signIndex -lt 0 -or $verifyIndex -le $signIndex -or $compileIndex -le $verifyIndex) { throw 'Signed OCR identity was not finalized before compile capture.' }
      if($derived.Identity -cne (Get-SigningSha256 $derived.Files[0].Source).ToLowerInvariant()) { throw 'Derived identity does not equal the signed engine hash.' }
      if((Get-SigningSha256 $files[0].Source) -cne $originalHash -or [Text.Encoding]::UTF8.GetString([IO.File]::ReadAllBytes($files[0].Source)) -cne 'owned-engine') { throw 'Original engine changed.' }
      $derivedLocks = Open-OcrReadLocks -Plan $derived
      try {
        $writeBlocked=$false
        try { $probe=[IO.File]::Open($derived.ManifestPath,[IO.FileMode]::Open,[IO.FileAccess]::Write,[IO.FileShare]::None); $probe.Dispose() } catch { $writeBlocked=$true }
        if(-not $writeBlocked) { throw 'Derived OCR manifest was not included in the read locks.' }
      } finally { foreach($lock in $derivedLocks){$lock.Dispose()} }

      $tauriCalls = [Collections.Generic.List[string]]::new()
      $tauriSigner = { param($path) $tauriCalls.Add([IO.Path]::GetFullPath($path)); return 0 }.GetNewClosure()
      $trustedPath = $derived.Files[0].Source
      $decision = Invoke-OcrAwareTauriSigner -Path $trustedPath -ExpectedOcrEnginePath $trustedPath -ExpectedOcrEngineBytes $derived.Engine.Bytes -ExpectedOcrEngineSha256 $derived.Engine.Sha256 -Signer $tauriSigner -SignatureProvider $signatureProvider -ExpectedPublisher 'Joshua Beel'
      if($decision -cne 'SkippedTrustedOcrEngine' -or $tauriCalls.Count -ne 0) { throw 'Exact trusted engine was not skipped.' }
      $app = Join-Path $evidence 'pdf-workstation.exe'; [IO.File]::WriteAllText($app,'app')
      $decision = Invoke-OcrAwareTauriSigner -Path $app -ExpectedOcrEnginePath $trustedPath -ExpectedOcrEngineBytes $derived.Engine.Bytes -ExpectedOcrEngineSha256 $derived.Engine.Sha256 -Signer $tauriSigner -SignatureProvider $signatureProvider -ExpectedPublisher 'Joshua Beel'
      if($decision -cne 'Signed' -or $tauriCalls.Count -ne 1 -or $tauriCalls[0] -cne [IO.Path]::GetFullPath($app)) { throw 'Non-engine candidate was not forwarded unchanged.' }

      $wrongRoot = Join-Path $evidence 'wrong'; [IO.Directory]::CreateDirectory($wrongRoot) | Out-Null
      $wrongEngine = Join-Path $wrongRoot 'tesseract.exe'; [IO.File]::Copy($trustedPath,$wrongEngine)
      $script:validSignatures[[IO.Path]::GetFullPath($wrongEngine)] = $true
      $rejected=$false; try { $null=Invoke-OcrAwareTauriSigner -Path $wrongEngine -ExpectedOcrEnginePath $trustedPath -ExpectedOcrEngineBytes $derived.Engine.Bytes -ExpectedOcrEngineSha256 $derived.Engine.Sha256 -Signer $tauriSigner -SignatureProvider $signatureProvider -ExpectedPublisher 'Joshua Beel' } catch { $rejected=$true }
      if(-not $rejected -or $tauriCalls.Count -ne 1) { throw 'Same-name engine outside the trusted canonical path was accepted.' }

      $trustedBytes = [IO.File]::ReadAllBytes($trustedPath)
      [IO.File]::WriteAllBytes($trustedPath, ($trustedBytes + [byte]0x7f))
      $rejected=$false; try { $null=Invoke-OcrAwareTauriSigner -Path $trustedPath -ExpectedOcrEnginePath $trustedPath -ExpectedOcrEngineBytes $derived.Engine.Bytes -ExpectedOcrEngineSha256 $derived.Engine.Sha256 -Signer $tauriSigner -SignatureProvider $signatureProvider -ExpectedPublisher 'Joshua Beel' } catch { $rejected=$true }
      if(-not $rejected) { throw 'Tampered trusted engine was accepted.' }
      [IO.File]::WriteAllBytes($trustedPath,$trustedBytes)
      $script:validSignatures[[IO.Path]::GetFullPath($trustedPath)] = $false
      $rejected=$false; try { $null=Invoke-OcrAwareTauriSigner -Path $trustedPath -ExpectedOcrEnginePath $trustedPath -ExpectedOcrEngineBytes $derived.Engine.Bytes -ExpectedOcrEngineSha256 $derived.Engine.Sha256 -Signer $tauriSigner -SignatureProvider $signatureProvider -ExpectedPublisher 'Joshua Beel' } catch { $rejected=$true }
      if(-not $rejected) { throw 'Invalid trusted-engine signature was accepted.' }
      $wrongPublisher = { param($path) [pscustomobject]@{Status='Valid';Publisher='Someone Else';HasTimestamp=$true} }
      $rejected=$false; try { $null=Invoke-OcrAwareTauriSigner -Path $trustedPath -ExpectedOcrEnginePath $trustedPath -ExpectedOcrEngineBytes $derived.Engine.Bytes -ExpectedOcrEngineSha256 $derived.Engine.Sha256 -Signer $tauriSigner -SignatureProvider $wrongPublisher -ExpectedPublisher 'Joshua Beel' } catch { $rejected=$true }
      if(-not $rejected) { throw 'Wrong trusted-engine publisher was accepted.' }
      $missingTimestamp = { param($path) [pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$false} }
      $rejected=$false; try { $null=Invoke-OcrAwareTauriSigner -Path $trustedPath -ExpectedOcrEnginePath $trustedPath -ExpectedOcrEngineBytes $derived.Engine.Bytes -ExpectedOcrEngineSha256 $derived.Engine.Sha256 -Signer $tauriSigner -SignatureProvider $missingTimestamp -ExpectedPublisher 'Joshua Beel' } catch { $rejected=$true }
      if(-not $rejected) { throw 'Untimestamped trusted-engine signature was accepted.' }
      $script:validSignatures[[IO.Path]::GetFullPath($trustedPath)] = $true

      $defaultEngine = Join-Path $evidence 'default-tesseract.exe'; [IO.File]::WriteAllText($defaultEngine,'default')
      $decision = Invoke-OcrAwareTauriSigner -Path $defaultEngine -Signer $tauriSigner -SignatureProvider $signatureProvider -ExpectedPublisher 'Joshua Beel'
      if($decision -cne 'Signed' -or $tauriCalls.Count -ne 2) { throw 'Default signing behavior did not forward a file without an OCR expectation.' }
      $failingSigner = { param($path) return 17 }
      $rejected=$false; try { $null=Invoke-OcrAwareTauriSigner -Path $app -Signer $failingSigner -SignatureProvider $signatureProvider -ExpectedPublisher 'Joshua Beel' } catch { $rejected=$true }
      if(-not $rejected) { throw 'Signer failure did not propagate.' }
      Write-Output ('PASS ' + $evidence)
    `;
    const result = runPowerShell(check);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 35_000);

  it('accepts only Authenticode field and certificate-table changes in a signed PE', () => {
    const check = String.raw`
      $ErrorActionPreference = 'Stop'
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/ocr/installer-package.ps1
      . ./scripts/ocr/windows-signing.ps1
      $evidence = Join-Path (Resolve-Path target) ('signed-pe-helper-' + [Guid]::NewGuid().ToString('N'))
      [IO.Directory]::CreateDirectory($evidence) | Out-Null
      function Set-U16([byte[]]$bytes,[int]$offset,[uint16]$value) { [BitConverter]::GetBytes($value).CopyTo($bytes,$offset) }
      function Set-U32([byte[]]$bytes,[int]$offset,[uint32]$value) { [BitConverter]::GetBytes($value).CopyTo($bytes,$offset) }
      $unsigned = [byte[]]::new(515)
      $unsigned[0]=0x4d; $unsigned[1]=0x5a; Set-U32 $unsigned 0x3c 0x80
      $unsigned[0x80]=0x50; $unsigned[0x81]=0x45
      Set-U16 $unsigned 0x94 240
      Set-U16 $unsigned 0x98 0x20b
      Set-U32 $unsigned 0x104 16
      for($i=0x188;$i -lt $unsigned.Length;$i++){ $unsigned[$i]=[byte](($i*13)%251) }
      $unsignedPath=Join-Path $evidence 'pdfium-unsigned.dll'; [IO.File]::WriteAllBytes($unsignedPath,$unsigned)
      $signed=[byte[]]::new(544); [Array]::Copy($unsigned,$signed,$unsigned.Length)
      Set-U32 $signed 0xd8 0x12345678
      Set-U32 $signed 0x128 520
      Set-U32 $signed 0x12c 24
      Set-U32 $signed 520 20
      Set-U16 $signed 524 0x0200
      Set-U16 $signed 526 0x0002
      for($i=528;$i -lt 540;$i++){ $signed[$i]=[byte](0xa0+$i-528) }
      $signedPath=Join-Path $evidence 'pdfium-signed.dll'; [IO.File]::WriteAllBytes($signedPath,$signed)
      $validSignature={ param($path) [pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$true} }
      Assert-SignedPdfiumEquivalent -UnsignedPath $unsignedPath -SignedPath $signedPath -SignatureProvider $validSignature -ExpectedPublisher 'Joshua Beel'
      $extractRoot=Join-Path $evidence 'extract/app'; $extractedPdfium=Join-Path $extractRoot 'resources/pdfium/bin/pdfium.dll'
      [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($extractedPdfium))|Out-Null; [IO.File]::Copy($signedPath,$extractedPdfium)
      $entry=[pscustomobject]@{Source=$signedPath;OriginalSource=$unsignedPath;Target='resources/pdfium/bin/pdfium.dll';Receipt=[pscustomobject]@{Bytes=[uint64](Get-Item $unsignedPath).Length;Sha256=Get-ExactSha256 $unsignedPath};IsSignablePe=$true}
      $receipts=@(Assert-ExtractedBaseResources -ExtractionRoot (Join-Path $evidence 'extract') -Entries @($entry) -PublisherSigned -SignatureProvider $validSignature -ExpectedPublisher 'Joshua Beel')
      if($receipts.Count -ne 1 -or $receipts[0].sha256 -cne (Get-ExactSha256 $signedPath) -or $receipts[0].unsignedSha256 -cne (Get-ExactSha256 $unsignedPath)) { throw 'Signed extraction receipt did not bind staged and original PDFium.' }
      $tamperedExtract=[byte[]]$signed.Clone();$tamperedExtract[400]=$tamperedExtract[400]-bxor 1;[IO.File]::WriteAllBytes($extractedPdfium,$tamperedExtract)
      $rejected=$false;try{$null=Assert-ExtractedBaseResources -ExtractionRoot (Join-Path $evidence 'extract') -Entries @($entry) -PublisherSigned -SignatureProvider $validSignature -ExpectedPublisher 'Joshua Beel'}catch{$rejected=$true};if(-not $rejected){throw 'Tampered extracted PDFium matched the staged receipt.'}
      [IO.File]::Copy($signedPath,$extractedPdfium,$true)
      $plainSource=Join-Path $evidence 'notice.txt';[IO.File]::WriteAllText($plainSource,'notice')
      $plainExtract=Join-Path $evidence 'plain/app/resources/notice.txt';[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($plainExtract))|Out-Null;[IO.File]::Copy($plainSource,$plainExtract)
      $plainEntry=[pscustomobject]@{Source=$plainSource;Target='resources/notice.txt';Receipt=[pscustomobject]@{Bytes=[uint64](Get-Item $plainSource).Length;Sha256=Get-ExactSha256 $plainSource}}
      $mustNotVerifySignature={param($path) throw 'Unsigned extraction called signature verification.'}
      $null=Assert-ExtractedBaseResources -ExtractionRoot (Join-Path $evidence 'plain') -Entries @($plainEntry) -SignatureProvider $mustNotVerifySignature -ExpectedPublisher 'Joshua Beel'

      $body=[byte[]]$signed.Clone(); $body[400]=$body[400]-bxor 1
      $bodyPath=Join-Path $evidence 'body.dll'; [IO.File]::WriteAllBytes($bodyPath,$body)
      $rejected=$false;try{Assert-SignedPdfiumEquivalent -UnsignedPath $unsignedPath -SignedPath $bodyPath -SignatureProvider $validSignature -ExpectedPublisher 'Joshua Beel'}catch{$rejected=$true};if(-not $rejected){throw 'Changed PE body was accepted.'}
      $gap=[byte[]]$signed.Clone(); $gap[516]=1
      $gapPath=Join-Path $evidence 'gap.dll'; [IO.File]::WriteAllBytes($gapPath,$gap)
      $rejected=$false;try{Assert-SignedPdfiumEquivalent -UnsignedPath $unsignedPath -SignedPath $gapPath -SignatureProvider $validSignature -ExpectedPublisher 'Joshua Beel'}catch{$rejected=$true};if(-not $rejected){throw 'Nonzero alignment gap was accepted.'}
      $trailing=[byte[]]::new(545);[Array]::Copy($signed,$trailing,$signed.Length)
      $trailingPath=Join-Path $evidence 'trailing.dll';[IO.File]::WriteAllBytes($trailingPath,$trailing)
      $rejected=$false;try{Assert-SignedPdfiumEquivalent -UnsignedPath $unsignedPath -SignedPath $trailingPath -SignatureProvider $validSignature -ExpectedPublisher 'Joshua Beel'}catch{$rejected=$true};if(-not $rejected){throw 'Trailing PE data was accepted.'}
      $badCertificate=[byte[]]$signed.Clone();Set-U16 $badCertificate 526 1
      $badCertificatePath=Join-Path $evidence 'bad-certificate.dll';[IO.File]::WriteAllBytes($badCertificatePath,$badCertificate)
      $rejected=$false;try{Assert-SignedPdfiumEquivalent -UnsignedPath $unsignedPath -SignedPath $badCertificatePath -SignatureProvider $validSignature -ExpectedPublisher 'Joshua Beel'}catch{$rejected=$true};if(-not $rejected){throw 'Malformed WIN_CERTIFICATE was accepted.'}
      $preexisting=[byte[]]$unsigned.Clone();Set-U32 $preexisting 0x128 504;Set-U32 $preexisting 0x12c 8
      $preexistingPath=Join-Path $evidence 'preexisting.dll';[IO.File]::WriteAllBytes($preexistingPath,$preexisting)
      $rejected=$false;try{Assert-SignedPdfiumEquivalent -UnsignedPath $preexistingPath -SignedPath $signedPath -SignatureProvider $validSignature -ExpectedPublisher 'Joshua Beel'}catch{$rejected=$true};if(-not $rejected){throw 'Pre-signed input was accepted.'}
      $badSignature={ param($path) [pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$false} }
      $rejected=$false;try{Assert-SignedPdfiumEquivalent -UnsignedPath $unsignedPath -SignedPath $signedPath -SignatureProvider $badSignature -ExpectedPublisher 'Joshua Beel'}catch{$rejected=$true};if(-not $rejected){throw 'Untimestamped signature was accepted.'}
      Write-Output ('PASS ' + $evidence)
    `;
    const result = runPowerShell(check);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 35_000);
});
