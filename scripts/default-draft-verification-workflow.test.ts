import { describe, expect, it } from 'vitest';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';

const workflow = readFileSync('.github/workflows/default-draft-verification.yml', 'utf8');
const verifier = readFileSync('scripts/verify-default-draft.ps1', 'utf8');
const pdfiumPreparation = readFileSync('scripts/prepare-default-draft-pdfium.ps1', 'utf8');
const currentVersion: string = JSON.parse(readFileSync('package.json', 'utf8')).version;
const wrongVersion = '0.0.0';
if (!/^\d+\.\d+\.\d+$/.test(currentVersion) || currentVersion === wrongVersion) throw new Error('package.json version is not a usable release version.');

function runPowerShell(source: string) {
  const root = `target/default-draft-test-${randomUUID()}`;
  mkdirSync(root, { recursive: true });
  const script = `${root}/run.ps1`;
  writeFileSync(script, source, 'utf8');
  try {
    return spawnSync('pwsh', ['-NoProfile', '-NonInteractive', '-File', script], { encoding: 'utf8', timeout: 30_000 });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

describe('default draft verification workflow', () => {
  it('is a pinned manual read-only proof with exact user-supplied receipts and no release or install capability', () => {
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('contents: read');
    expect(workflow.match(/contents:\s*write/g)).toHaveLength(1);
    expect(workflow.indexOf('contents: write')).toBeGreaterThan(workflow.indexOf('verify-default-draft:'));
    expect(workflow).not.toMatch(/actions:\s*write|id-token:\s*write/);
    for (const input of ['target_version', 'target_tag', 'target_source_revision', 'target_release_id', 'installer_asset_id', 'installer_name', 'installer_bytes', 'installer_sha256', 'signature_asset_id', 'signature_bytes', 'signature_sha256', 'manifest_asset_id', 'manifest_bytes', 'manifest_sha256', 'expected_publisher']) {
      expect(workflow).toContain(`${input}:`);
    }
    const uses = [...workflow.matchAll(/^\s*-?\s*uses:\s*([^\s#]+)\s*$/gm)].map(match => match[1]);
    expect(uses).toEqual([
      'actions/checkout@11d5960a326750d5838078e36cf38b85af677262',
      'actions/checkout@11d5960a326750d5838078e36cf38b85af677262',
      'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02',
    ]);
    for (const use of uses) expect(use).toMatch(/@[0-9a-f]{40}$/);
    expect(workflow.match(/persist-credentials: false/g)).toHaveLength(2);
    expect(workflow).toContain('ref: ${{ inputs.target_source_revision }}');
    expect(workflow).toContain('git -C target-source rev-parse "$env:TARGET_TAG^{commit}"');
    expect(workflow).toContain('if: ${{ always() }}');
    expect(workflow).not.toMatch(/gh release|cargo |npm run build|setup-dotnet|artifact-signing|Start-Process|\.\/(?:target-source\/)?src-tauri\/target/);
    expect(workflow).not.toMatch(/secrets\.|TAURI_SIGNING_PRIVATE_KEY|AZURE_(?:TENANT|CLIENT|SIGNING)/);
    const preparation = workflow.indexOf('Prepare exact pinned PDFium resources in target source');
    const proof = workflow.indexOf('Verify exact default draft without installation');
    expect(preparation).toBeGreaterThan(workflow.indexOf('Verify exact clean workflow and target sources'));
    expect(proof).toBeGreaterThan(preparation);
    expect(workflow).toContain("./scripts/prepare-default-draft-pdfium.ps1 -TargetSourceRoot (Resolve-Path target-source).Path");
    expect(pdfiumPreparation).toContain("$assetId=[uint64]441936973");
    expect(pdfiumPreparation).toContain("$assetBytes=[uint64]3733154");
    expect(pdfiumPreparation).toContain("$assetSha256='73CC0DE638AC2095E7445BF56A38200A5B7C7CA0E9F4BA144598F2457377AC08'");
    expect(pdfiumPreparation).not.toMatch(/setup-pdfium|cargo|Start-Process|Invoke-Expression/);
  });

  it('binds a bounded three-asset draft to exact source resources and separates signature text from crypto proof', () => {
    expect(verifier).toContain("$base.Count -ne 26");
    expect(verifier).toContain('Assert-SignedPdfiumEquivalent');
    expect(verifier).toContain('Assert-ReleaseSourceVersion -WorkflowRoot $ProjectRoot -SourceRoot $TargetSourceRoot -Version $TargetVersion');
    expect(verifier).toContain('Assert-PackagedApplicationIdentity -Path $application.FullName -Version $TargetVersion');
    expect(verifier).toContain('Convert-SevenZipInventory -Lines $listing');
    expect(verifier).toContain('Assert-ArchiveResourceInventory -ArchivePaths $archivePaths -BaseEntries $BaseEntries');
    expect(verifier).toContain("throw 'Extracted installer contains a reparse point.'");
    expect(verifier).toContain('Assert-TrustedWindowsSignature -Path $installerPath');
    expect(verifier).toContain("detachedUpdaterCryptographyVerified=$false");
    expect(verifier).toContain("ocrBundled=$false");
    expect(verifier).toContain("$actualNames.Count -ne 3");
    expect(verifier).toContain("$memory.Length+$read -gt 1MB");
    expect(verifier).toContain("$total -gt $Bytes");
    expect(verifier).toContain("sourceBuildExecutableByteProvenanceReconstructed=$false");
    expect(verifier).toContain("-Arguments @('i') -TimeoutMilliseconds 5000 -Capture");
    expect(verifier).not.toContain('ReadToEndAsync');
    expect(verifier).not.toMatch(/Write-(?:Host|Output|Verbose|Debug)|ConvertTo-SecureString|Start-Process|Remove-Item/);
  });

  it('uses the job-scoped draft visibility grant only for bounded GET requests', () => {
    const check = String.raw`
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path ./scripts/verify-default-draft.ps1),[ref]$tokens,[ref]$errors);if($errors.Count){throw 'parse'}
      foreach($name in @('New-GitHubGetRequest','Assert-GitHubResponseSuccess')){$node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true);if(-not$node){throw 'missing'};Invoke-Expression $node.Extent.Text}
      $constructors=@($ast.FindAll({param($n)$n-is[Management.Automation.Language.InvokeMemberExpressionAst]-and$n.Member.Value-ceq'new'-and$n.Expression.Extent.Text-match'HttpRequestMessage'},$true));if($constructors.Count-ne1-or$constructors[0].Arguments[0].Extent.Text-cne'[Net.Http.HttpMethod]::Get'){throw 'HTTP method surface changed'}
      $request=New-GitHubGetRequest 'https://api.github.com/repos/Joshua-Beel/Smacrobat/releases/1' 'placeholder' 'application/vnd.github+json';try{if($request.Method-ne[Net.Http.HttpMethod]::Get-or$null-ne$request.Content-or$request.RequestUri.AbsoluteUri-cne'https://api.github.com/repos/Joshua-Beel/Smacrobat/releases/1'-or$request.Headers.UserAgent.ToString()-cne'Smacrobat-DraftVerifier'){throw 'runtime request is not exact authenticated GitHub GET'}}finally{$request.Dispose()}
      foreach($code in @(401,403,404,429,500,503)){try{Assert-GitHubResponseSuccess ([pscustomobject]@{IsSuccessStatusCode=$false;StatusCode=[Net.HttpStatusCode]$code}) 'release-metadata';throw 'failure status accepted'}catch{if($_.Exception.Message-cne("GitHub request failed at release-metadata with HTTP status $code.")){throw 'failure diagnostic changed'}}}
      Assert-GitHubResponseSuccess ([pscustomobject]@{IsSuccessStatusCode=$true;StatusCode=[Net.HttpStatusCode]::OK}) 'release-asset'
    `;
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(verifier).not.toMatch(/HttpMethod\]::(?:Post|Put|Patch|Delete)|Invoke-RestMethod|Invoke-WebRequest|gh\s+(?:release|api)/i);
  });

  it('rejects changed PDFium release identity and unsafe archive entry types or expanded sizes', () => {
    const check = String.raw`
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path ./scripts/prepare-default-draft-pdfium.ps1),[ref]$tokens,[ref]$errors);if($errors.Count){throw 'parse'}
      foreach($name in @('Assert-PdfiumReleaseMetadata','ConvertFrom-PdfiumTarListing')){$node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true);if(-not$node){throw 'missing'};Invoke-Expression $node.Extent.Text}
      $assetId=[uint64]441936973;$assetName='pdfium-win-x64.tgz';$assetBytes=[uint64]3733154;$assetSha256='73CC0DE638AC2095E7445BF56A38200A5B7C7CA0E9F4BA144598F2457377AC08';$downloadUrl='https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/7881/pdfium-win-x64.tgz'
      $asset=[pscustomobject]@{id=$assetId;name=$assetName;size=$assetBytes;state='uploaded';digest=('sha256:'+$assetSha256.ToLowerInvariant());browser_download_url=$downloadUrl};$metadata=[pscustomobject]@{tag_name='chromium/7881';draft=$false;prerelease=$false;published_at='2026-06-08T00:00:00Z';assets=@($asset)}
      Assert-PdfiumReleaseMetadata $metadata
      function Reject([scriptblock]$action){$failed=$false;try{&$action}catch{$failed=$true};if(-not$failed){throw 'unsafe fixture accepted'}}
      $metadata.draft=$true;Reject {Assert-PdfiumReleaseMetadata $metadata};$metadata.draft=$false;$asset.digest='sha256:'+('0'*64);Reject {Assert-PdfiumReleaseMetadata $metadata};$asset.digest='sha256:'+$assetSha256.ToLowerInvariant()
      $expectedEntries=@('safe.txt');$null=ConvertFrom-PdfiumTarListing '-rw-r--r--  0 runner runner 12 Jun 08 12:05 safe.txt'
      Reject {ConvertFrom-PdfiumTarListing 'lrwxrwxrwx  0 runner runner 0 Jun 08 12:05 safe.txt -> ../escape'}
      Reject {ConvertFrom-PdfiumTarListing '-rw-r--r--  0 runner runner 34603008 Jun 08 12:05 safe.txt'}
    `;
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('executes the real metadata and extraction inventory guards against missing, extra, OCR, collision, and traversal-shaped cases', () => {
    const check = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/ocr/installer-package.ps1
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path ./scripts/verify-default-draft.ps1),[ref]$tokens,[ref]$errors)
      if($errors.Count){throw 'Verifier did not parse.'}
      foreach($name in @('Get-ExactReleaseAsset','Assert-DefaultInventory','Assert-BoundedArchiveListing','Invoke-BoundedSevenZip','Invoke-BoundedSevenZipEntry','Get-SevenZipVersionFact','Assert-ReleaseSourceVersion','Assert-PackagedApplicationIdentity')){$node=$ast.Find({param($n)$n -is [Management.Automation.Language.FunctionDefinitionAst]-and $n.Name-ceq$name},$true);if(-not$node){throw ('Missing '+$name)};Invoke-Expression $node.Extent.Text}
      $script:Repository='Joshua-Beel/Smacrobat';$TargetTag='v0.2.12';$sha='A'*64
      $asset=[pscustomobject]@{id=[uint64]7;name='PDF.Workstation_0.2.12_x64-setup.exe';size=[uint64]9;state='uploaded';digest=('sha256:'+$sha.ToLowerInvariant());browser_download_url='https://github.com/Joshua-Beel/Smacrobat/releases/download/v0.2.12/PDF.Workstation_0.2.12_x64-setup.exe'}
      $release=[pscustomobject]@{assets=@($asset)}
      $segment=Get-ExactReleaseAsset $release 7 $asset.name 9 $sha;if($segment-cne'v0.2.12'){throw 'tagged segment changed'}
      $asset.browser_download_url='https://github.com/Joshua-Beel/Smacrobat/releases/download/untagged-2194514d6aa620c2f2bb/PDF.Workstation_0.2.12_x64-setup.exe';$segment=Get-ExactReleaseAsset $release 7 $asset.name 9 $sha;if($segment-cne'untagged-2194514d6aa620c2f2bb'){throw 'draft segment changed'}
      function Reject([scriptblock]$Action,[string]$Kind){$failed=$false;try{&$Action}catch{$failed=$true};if(-not$failed){throw ($Kind+' accepted')}}
      Reject {Get-ExactReleaseAsset $release 7 $asset.name 10 $sha} 'wrong bytes'
      Reject {Get-ExactReleaseAsset $release 7 $asset.name 9 $sha 'untagged-00000000000000000000'} 'mismatched draft segment'
      foreach($url in @('https://github.com/Joshua-Beel/Smacrobat/releases/download/untagged-nothex/PDF.Workstation_0.2.12_x64-setup.exe','https://github.com/foreign/Smacrobat/releases/download/v0.2.12/PDF.Workstation_0.2.12_x64-setup.exe','https://example.com/Joshua-Beel/Smacrobat/releases/download/v0.2.12/PDF.Workstation_0.2.12_x64-setup.exe','http://github.com/Joshua-Beel/Smacrobat/releases/download/v0.2.12/PDF.Workstation_0.2.12_x64-setup.exe','https://github.com/Joshua-Beel/Smacrobat/releases/download/v0.2.12/PDF.Workstation_0.2.12_x64-setup.exe?x=1','https://github.com/Joshua-Beel/Smacrobat/releases/download/v0.2.12/../PDF.Workstation_0.2.12_x64-setup.exe')){$asset.browser_download_url=$url;Reject {Get-ExactReleaseAsset $release 7 $asset.name 9 $sha} 'invalid draft URL'}
      $root=Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'inventory';[IO.Directory]::CreateDirectory($root)|Out-Null
      function Put([string]$Relative){$path=Join-Path $root $Relative;[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path))|Out-Null;[IO.File]::WriteAllText($path,'x',[Text.UTF8Encoding]::new($false));return Get-Item $path}
      $streamSource=Put 'stream/input.bin';$streamZip=Join-Path $root 'stream.zip';Compress-Archive -LiteralPath $streamSource.FullName -DestinationPath $streamZip;$extractor=Get-InstallerExtractor;$streamHash=Get-ExactSha256 $streamSource.FullName;Invoke-BoundedSevenZipEntry $extractor.FullName $streamZip 'input.bin' (Join-Path $root 'stream-good.bin') 1 $streamHash
      Reject {Invoke-BoundedSevenZipEntry $extractor.FullName $streamZip 'input.bin' (Join-Path $root 'stream-long.bin') 0 $streamHash} 'stream overflow'
      Reject {Invoke-BoundedSevenZipEntry $extractor.FullName $streamZip 'input.bin' (Join-Path $root 'stream-hash.bin') 1 ('0'*64)} 'stream hash mismatch'
      $base=@([pscustomobject]@{Target='resources/welcome.pdf'})
      $paths=@('pdf-workstation.exe','resources/welcome.pdf','$PLUGINSDIR/modern-wizard.bmp','$PLUGINSDIR/nsDialogs.dll','$PLUGINSDIR/nsis_tauri_utils.dll','$PLUGINSDIR/NSISdl.dll','$PLUGINSDIR/StartMenu.dll','$PLUGINSDIR/System.dll')
      $files=@($paths|ForEach-Object{Put $_});$facts=Assert-DefaultInventory $files $root $base;if($facts.required-ne8-or$facts.actual-ne8){throw 'valid inventory failed'}
      $extra=@($files)+(Put 'extra.dll');Reject {Assert-DefaultInventory $extra $root $base} 'extra file'
      $ocr=@($files)+(Put 'resources/ocr/bad/model.bin');Reject {Assert-DefaultInventory $ocr $root $base} 'OCR file'
      $missing=@($files|Where-Object{$_.Name-ne'welcome.pdf'});Reject {Assert-DefaultInventory $missing $root $base} 'missing resource'
      $case=@($files)+(Put 'RESOURCES/WELCOME.PDF');Reject {Assert-DefaultInventory $case $root $base} 'case collision'
      $listing=@('----------','Path = app.exe','Size = 10','Path = data.bin','Size = 20');$bounded=Assert-BoundedArchiveListing $listing;if($bounded.entries-ne2-or$bounded.expandedBytes-ne30){throw 'bounded listing facts changed'}
      $blank=@('----------','Path = resources\welcome.pdf','Size = ');$bounded=Assert-BoundedArchiveListing $blank @{'resources/welcome.pdf'=[uint64]9};if($bounded.entries-ne1-or$bounded.expandedBytes-ne9){throw 'exact fallback size changed'}
      $bounded=Assert-BoundedArchiveListing @('----------','Path = resources\welcome.pdf','Size =   ') @{'resources/welcome.pdf'=[uint64]9};if($bounded.expandedBytes-ne9){throw 'whitespace fallback size changed'}
      $bounded=Assert-BoundedArchiveListing @('----------','Path = app.exe','Size =   10  ');if($bounded.expandedBytes-ne10){throw 'padded numeric size changed'}
      $bounded=Assert-BoundedArchiveListing @('----------','Path = uninstall.exe','Size = ');if($bounded.entries-ne1-or$bounded.expandedBytes-ne0-or$bounded.excludedUninstaller-cne'uninstall.exe'){throw 'final generated uninstaller exclusion changed'}
      $bounded=Assert-BoundedArchiveListing @('----------','Path = uninstall.exe','Size = 105704');if($bounded.excludedUninstaller-cne'uninstall.exe'){throw 'numeric generated uninstaller exclusion changed'}
      Reject {Assert-BoundedArchiveListing @('----------','Path = prefix/uninstall.exe','Size = ')} 'nested blank uninstaller'
      Reject {Assert-BoundedArchiveListing @('----------','Path = uninstall.exe','Size = ','Path = later.bin','Size = 1')} 'nonfinal blank uninstaller'
      Reject {Assert-BoundedArchiveListing @('----------','Path = uninstall.exe','Size = ','Path = prefix/uninstall.exe','Size = ')} 'duplicate blank uninstaller'
      Reject {Assert-BoundedArchiveListing $blank} 'unbound blank size'
      $message='';try{Assert-BoundedArchiveListing $blank}catch{$message=$_.Exception.Message};if($message-cne'Installer archive size is blank. EntryBucket=welcome-without-exact-fallback EntryOrdinal=1.'){throw 'welcome blank diagnostic changed'}
      Reject {Assert-BoundedArchiveListing $blank @{'resources/other.pdf'=[uint64]9}} 'wrong blank-size path'
      $message='';try{Assert-BoundedArchiveListing @('----------','Path = prefix/resources/pdfium/LICENSE','Size = ') @{'resources/welcome.pdf'=[uint64]9} @{'resources/pdfium/LICENSE'=$true}}catch{$message=$_.Exception.Message};if($message-cne'Installer archive size is blank. EntryBucket=base-other EntryOrdinal=1.'){throw 'known blank diagnostic changed'}
      $message='';try{Assert-BoundedArchiveListing @('----------','Path = [0]','Size = ')}catch{$message=$_.Exception.Message};if($message-cne'Installer archive size is blank. EntryBucket=nsis-container EntryOrdinal=1.'){throw 'container blank diagnostic changed'}
      $message='';try{Assert-BoundedArchiveListing @('----------','Path = prefix/pdf-workstation.exe','Size = ')}catch{$message=$_.Exception.Message};if($message-cne'Installer archive size is blank. EntryBucket=executable EntryOrdinal=1.'){throw 'package blank diagnostic changed'}
      Reject {Assert-BoundedArchiveListing @('----------','Path = resources/pdfium/LICENSE','Size =   ') @{'resources/welcome.pdf'=[uint64]9}} 'known non-welcome whitespace size'
      $message='';try{Assert-BoundedArchiveListing @('----------','Path = app.exe','Size = 1x')}catch{$message=$_.Exception.Message};if($message-cne'Installer archive size is nonnumeric.'){throw 'nonnumeric diagnostic changed'}
      if((Get-SevenZipVersionFact 'ignored' {throw 'probe failure'})-cne'unknown'-or(Get-SevenZipVersionFact 'ignored' {'7-Zip 26.03 (x64)'})-cne'26.03'){throw 'version fact fallback changed'}
      Reject {Assert-BoundedArchiveListing @('----------','Path = bomb.bin',('Size = '+(513MB)))} 'expanded-size bomb'
      Reject {Assert-BoundedArchiveListing @('----------','Path = missing.bin')} 'missing size'
      Reject {Assert-BoundedArchiveListing @('----------','Path = malformed.bin','Size = nope')} 'malformed size'
      Reject {Assert-BoundedArchiveListing @('----------','Size = 1')} 'orphan size'
      Reject {Assert-BoundedArchiveListing @('----------','Path = duplicate.bin','Size = 1','Size = 1')} 'duplicate size'
      $many=@('----------');for($index=0;$index-lt513;$index++){$many+=('Path = file-'+$index);$many+='Size = 1'};Reject {Assert-BoundedArchiveListing $many} 'entry-count bomb'
      Reject {Invoke-BoundedSevenZip -Executable (Get-Command pwsh).Source -Arguments @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 2') -TimeoutMilliseconds 100} 'process timeout'
      Reject {Invoke-BoundedSevenZip -Executable (Get-Command pwsh).Source -Arguments @('-NoProfile','-NonInteractive','-Command',"[Console]::Out.Write('x'*(3MB))") -TimeoutMilliseconds 10000} 'process output overflow'
      $fake=Put 'version-fixture.exe';$goodVersion={param($path)[pscustomobject]@{FileVersion='0.2.12';ProductVersion='0.2.12';ProductName='PDF Workstation'}};$oldVersion={param($path)[pscustomobject]@{FileVersion='0.2.11';ProductVersion='0.2.11';ProductName='PDF Workstation'}}
      Assert-PackagedApplicationIdentity $fake.FullName '0.2.12' $goodVersion;Reject {Assert-PackagedApplicationIdentity $fake.FullName '0.2.12' $oldVersion} 'old application'
      $source=Join-Path (Split-Path -Parent $MyInvocation.MyCommand.Path) 'source';foreach($relative in @('package.json','package-lock.json','src-tauri/tauri.conf.json','src-tauri/Cargo.toml','src-tauri/Cargo.lock')){$destination=Join-Path $source $relative;[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($destination))|Out-Null;[IO.File]::Copy((Join-Path (Get-Location) $relative),$destination)}
      Assert-ReleaseSourceVersion (Get-Location).Path $source '${currentVersion}';$package=Get-Content (Join-Path $source 'package.json') -Raw|ConvertFrom-Json;$package.version='${wrongVersion}';[IO.File]::WriteAllText((Join-Path $source 'package.json'),($package|ConvertTo-Json -Depth 20),[Text.UTF8Encoding]::new($false));Reject {Assert-ReleaseSourceVersion (Get-Location).Path $source '${currentVersion}'} 'source version mismatch'
      Write-Output PASS
    `;
    const result = runPowerShell(check);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 45_000);
});
