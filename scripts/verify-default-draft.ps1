[CmdletBinding()]
param(
    [Parameter(Mandatory=$true)][string]$ProjectRoot,
    [Parameter(Mandatory=$true)][string]$TargetSourceRoot,
    [Parameter(Mandatory=$true)][string]$AssetRoot,
    [Parameter(Mandatory=$true)][string]$WorkRoot,
    [Parameter(Mandatory=$true)][string]$OutputRoot,
    [Parameter(Mandatory=$true)][string]$WorkflowSourceRevision,
    [Parameter(Mandatory=$true)][string]$TargetVersion,
    [Parameter(Mandatory=$true)][string]$TargetTag,
    [Parameter(Mandatory=$true)][string]$TargetSourceRevision,
    [Parameter(Mandatory=$true)][string]$TargetReleaseId,
    [Parameter(Mandatory=$true)][string]$InstallerAssetId,
    [Parameter(Mandatory=$true)][string]$InstallerName,
    [Parameter(Mandatory=$true)][string]$InstallerBytes,
    [Parameter(Mandatory=$true)][string]$InstallerSha256,
    [Parameter(Mandatory=$true)][string]$SignatureAssetId,
    [Parameter(Mandatory=$true)][string]$SignatureBytes,
    [Parameter(Mandatory=$true)][string]$SignatureSha256,
    [Parameter(Mandatory=$true)][string]$ManifestAssetId,
    [Parameter(Mandatory=$true)][string]$ManifestBytes,
    [Parameter(Mandatory=$true)][string]$ManifestSha256,
    [Parameter(Mandatory=$true)][string]$ExpectedPublisher
)
$ErrorActionPreference='Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')

$script:Repository='Joshua-Beel/PDF-Workstation'
$script:MaximumAssetBytes=[uint64](256MB)

function ConvertTo-ExactUInt64 {
    param([string]$Value,[string]$Kind)
    [uint64]$parsed=0
    if(-not [uint64]::TryParse($Value,[Globalization.NumberStyles]::None,[Globalization.CultureInfo]::InvariantCulture,[ref]$parsed)-or $parsed -eq 0){throw "$Kind must be one positive base-10 integer."}
    return $parsed
}

function Resolve-ProofPath {
    param([string]$Path,[string]$RunnerTemp,[switch]$Fresh)
    $root=[IO.Path]::GetFullPath($RunnerTemp).TrimEnd('\')
    $candidate=[IO.Path]::GetFullPath($Path).TrimEnd('\')
    if(-not $candidate.StartsWith($root+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Proof paths must remain beneath RUNNER_TEMP.'}
    Assert-NoReparseAncestors -Path $candidate
    if($Fresh -and (Test-Path -LiteralPath $candidate)){throw 'Proof path must be fresh.'}
    return $candidate
}

function Assert-ReceiptShape {
    param([uint64]$Bytes,[string]$Sha256,[string]$Kind)
    if($Bytes -eq 0 -or $Bytes -gt $script:MaximumAssetBytes -or $Sha256 -cnotmatch '^[A-F0-9]{64}$'){throw "$Kind receipt is malformed."}
}

function New-GitHubGetRequest {
    param([string]$Uri,[string]$Token,[string]$Accept)
    $request=[Net.Http.HttpRequestMessage]::new([Net.Http.HttpMethod]::Get,$Uri)
    $request.Headers.Authorization=[Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer',$Token)
    $request.Headers.Accept.ParseAdd($Accept);$request.Headers.UserAgent.ParseAdd('Smacrobat-DraftVerifier');$request.Headers.Add('X-GitHub-Api-Version','2022-11-28')
    return $request
}

function Assert-GitHubResponseSuccess {
    param($Response,[ValidateSet('release-metadata','release-asset')][string]$Stage)
    if(-not $Response.IsSuccessStatusCode){throw "GitHub request failed at $Stage with HTTP status $([int]$Response.StatusCode)."}
}

function Invoke-GitHubJson {
    param([string]$Uri,[string]$Token)
    $handler=[Net.Http.HttpClientHandler]::new();$handler.AllowAutoRedirect=$false
    $client=[Net.Http.HttpClient]::new($handler);$client.Timeout=[TimeSpan]::FromSeconds(30)
    $request=New-GitHubGetRequest $Uri $Token 'application/vnd.github+json'
    $response=$null
    try{
        $response=$client.SendAsync($request,[Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        Assert-GitHubResponseSuccess $response 'release-metadata'
        $length=$response.Content.Headers.ContentLength;if($null -ne $length-and ([uint64]$length -eq 0-or [uint64]$length -gt 1MB)){throw 'GitHub release metadata size is outside its bound.'}
        $stream=$response.Content.ReadAsStreamAsync().GetAwaiter().GetResult();$memory=[IO.MemoryStream]::new()
        try{$buffer=[byte[]]::new(16384);while(($read=$stream.Read($buffer,0,$buffer.Length))-gt 0){if($memory.Length+$read -gt 1MB){throw 'GitHub release metadata exceeded its byte bound.'};$memory.Write($buffer,0,$read)};if($memory.Length -eq 0){throw 'GitHub release metadata was empty.'};$text=[Text.Encoding]::UTF8.GetString($memory.ToArray())}finally{$memory.Dispose();$stream.Dispose()}
        try{return $text|ConvertFrom-Json}catch{throw 'GitHub release metadata was malformed.'}
    }finally{if($null-ne$response){$response.Dispose()};$request.Dispose();$client.Dispose();$handler.Dispose()}
}

function Get-ExactReleaseAsset {
    param($Release,[uint64]$Id,[string]$Name,[uint64]$Bytes,[string]$Sha256,[string]$ExpectedDownloadSegment)
    $matches=@($Release.assets|Where-Object{[uint64]$_.id -eq $Id})
    if($matches.Count -ne 1){throw 'Draft asset id is absent or ambiguous.'}
    $asset=$matches[0]
    $uri=[Uri][string]$asset.browser_download_url
    $prefix="/$script:Repository/releases/download/";$suffix='/'+[Uri]::EscapeDataString($Name);$path=$uri.AbsolutePath
    if(-not $path.StartsWith($prefix,[StringComparison]::Ordinal)-or-not $path.EndsWith($suffix,[StringComparison]::Ordinal)){throw 'Draft asset metadata does not match its exact receipt.'}
    $downloadSegment=$path.Substring($prefix.Length,$path.Length-$prefix.Length-$suffix.Length)
    if(($downloadSegment -cne $TargetTag-and $downloadSegment -cnotmatch '^untagged-[a-f0-9]{20}$')-or($ExpectedDownloadSegment-and $downloadSegment -cne $ExpectedDownloadSegment)){throw 'Draft asset metadata does not match its exact receipt.'}
    $expectedPath=$prefix+$downloadSegment+$suffix
    if([string]$asset.name -cne $Name -or [uint64]$asset.size -ne $Bytes -or [string]$asset.state -cne 'uploaded' -or
       [string]$asset.digest -cne ('sha256:'+$Sha256.ToLowerInvariant()) -or $uri.Scheme -cne 'https' -or $uri.DnsSafeHost -cne 'github.com' -or
       $uri.UserInfo -or $uri.Query -or $uri.Fragment -or $path -cne $expectedPath){throw 'Draft asset metadata does not match its exact receipt.'}
    return $downloadSegment
}

function Save-ExactReleaseAsset {
    param([uint64]$Id,[string]$Destination,[string]$Token,[uint64]$Bytes,[string]$Sha256)
    $handler=[Net.Http.HttpClientHandler]::new();$handler.AllowAutoRedirect=$true;$handler.MaxAutomaticRedirections=5
    $client=[Net.Http.HttpClient]::new($handler);$client.Timeout=[TimeSpan]::FromSeconds(120)
    $request=New-GitHubGetRequest "https://api.github.com/repos/$script:Repository/releases/assets/$Id" $Token 'application/octet-stream'
    $response=$null
    try{
      $response=$client.SendAsync($request,[Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
      Assert-GitHubResponseSuccess $response 'release-asset'
      $length=$response.Content.Headers.ContentLength;if($null-ne$length-and [uint64]$length-ne$Bytes){throw 'GitHub release asset content length differs from its receipt.'}
      $stream=$response.Content.ReadAsStreamAsync().GetAwaiter().GetResult();$file=[IO.File]::Open($Destination,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
      try{$buffer=[byte[]]::new(65536);[uint64]$total=0;while(($read=$stream.Read($buffer,0,$buffer.Length))-gt 0){$total += [uint64]$read;if($total -gt $Bytes){throw 'GitHub release asset exceeded its exact byte receipt.'};$file.Write($buffer,0,$read)};if($total-ne$Bytes){throw 'GitHub release asset ended before its exact byte receipt.'}}finally{$file.Dispose();$stream.Dispose()}
    }finally{if($null-ne$response){$response.Dispose()};$request.Dispose();$client.Dispose();$handler.Dispose()}
    $item=Get-Item -LiteralPath $Destination
    if([uint64]$item.Length -ne $Bytes -or (Get-ExactSha256 -Path $Destination) -cne $Sha256){throw 'Downloaded draft asset does not match its exact receipt.'}
}

function Invoke-BoundedSevenZip {
    param([string]$Executable,[string[]]$Arguments,[int]$TimeoutMilliseconds=120000,[switch]$Capture)
    if(-not ('DefaultDraftBoundedProcess' -as [type])){
      Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
public static class DefaultDraftBoundedProcess {
  sealed class Counter { public int Value; }
  static async Task<string> ReadBounded(System.IO.StreamReader reader, int maximum, Counter counter, Action overflow) {
    var value = new StringBuilder(); var buffer = new char[8192];
    while (true) { int count = await reader.ReadAsync(buffer, 0, buffer.Length); if (count == 0) break; if (Interlocked.Add(ref counter.Value, count) > maximum) { overflow(); throw new InvalidOperationException("Bounded process output exceeded its limit."); } value.Append(buffer, 0, count); }
    return value.ToString();
  }
  public static string Run(string executable, string[] arguments, int timeoutMilliseconds, int maximumCharacters) {
    var start = new ProcessStartInfo { FileName=executable, UseShellExecute=false, CreateNoWindow=true, WindowStyle=ProcessWindowStyle.Hidden, RedirectStandardOutput=true, RedirectStandardError=true };
    foreach (var argument in arguments) start.ArgumentList.Add(argument);
    using (var process = new Process { StartInfo=start }) {
      if (!process.Start()) throw new InvalidOperationException("Bounded process did not start.");
      Action kill = () => { try { if (!process.HasExited) process.Kill(true); } catch {} };
      var counter = new Counter(); var stdout = ReadBounded(process.StandardOutput, maximumCharacters, counter, kill); var stderr = ReadBounded(process.StandardError, maximumCharacters, counter, kill);
      try { if (!process.WaitForExit(timeoutMilliseconds)) { kill(); process.WaitForExit(); throw new TimeoutException("Bounded process timed out."); } Task.WaitAll(stdout, stderr); }
      catch { kill(); throw; }
      if (process.ExitCode != 0) throw new InvalidOperationException("Bounded process failed.");
      return stdout.Result;
    }
  }
}
'@
    }
    try{$stdout=[DefaultDraftBoundedProcess]::Run($Executable,$Arguments,$TimeoutMilliseconds,2MB)}catch{throw 'Bounded 7-Zip process failed, timed out, or exceeded its output bound.'}
    if($Capture){return @($stdout -split '\r?\n')}
}

function Assert-BoundedArchiveListing {
    param([string[]]$Lines,[Collections.IDictionary]$ExactFallbackSizes,[Collections.IDictionary]$KnownPaths)
    [uint64]$entries=0;$started=$false;$state=@{path=$null;size=$null;total=[uint64]0;excluded=$false;excludedUninstaller=$null}
    function Complete-Entry {
      if($null-eq$state.path){return}
      if($state.excluded){
        if($null-ne$state.excludedUninstaller){throw 'Installer archive has duplicate excluded uninstallers.'}
        $state.excludedUninstaller=[string]$state.path;$state.path=$null;$state.size=$null;$state.excluded=$false;return
      }
      if($null-eq$state.size){throw 'Installer archive entry has no exact numeric size.'}
      $scriptSize=[uint64]$state.size
      if([uint64]::MaxValue-[uint64]$state.total-lt$scriptSize){throw 'Installer archive expanded size overflowed.'};$state.total=[uint64]$state.total+$scriptSize
      if([uint64]$state.total -gt 512MB){throw 'Installer archive expanded size exceeds its bound.'}
      $state.path=$null;$state.size=$null;$state.excluded=$false
    }
    foreach($line in $Lines){
      if($line -ceq '----------'){$started=$true;continue}
      if(-not$started){continue}
      if($line -cmatch '^Path = (.+)$'){if($state.excluded){throw 'Excluded installer uninstaller is not the final archive entry.'};Complete-Entry;$state.path=$Matches[1];$entries++;if($entries -gt 512){throw 'Installer archive entry count exceeds its bound.'};continue}
      if($line.StartsWith('Size = ',[StringComparison]::Ordinal)){
        if($null-eq$state.path-or $null-ne$state.size){throw 'Installer archive has an orphan or duplicate size.'}
        [uint64]$size=0;$text=$line.Substring(7).Trim();$key=([string]$state.path).Replace('\','/')
        if($key.Equals('uninstall.exe',[StringComparison]::OrdinalIgnoreCase)){
          if($text.Length-gt0-and-not [uint64]::TryParse($text,[Globalization.NumberStyles]::None,[Globalization.CultureInfo]::InvariantCulture,[ref]$size)){throw 'Installer archive size is nonnumeric.'}
          if($size-gt256MB){throw 'Installer archive entry size exceeds its bound.'}
          $state.excluded=$true;$state.size=[uint64]0;continue
        }
        if($text.Length-eq0){
          if($null-ne$ExactFallbackSizes-and$ExactFallbackSizes.Contains($key)){$size=[uint64]$ExactFallbackSizes[$key]}
          else{
            $bucket='unknown'
            if(Test-ArchiveTargetMatch $key 'resources/welcome.pdf'){$bucket='welcome-without-exact-fallback'}
            elseif($null-ne$KnownPaths-and@($KnownPaths.Keys|Where-Object{Test-ArchiveTargetMatch $key ([string]$_)}).Count-gt0){$bucket='base-other'}
            elseif(Test-ArchiveTargetMatch $key 'pdf-workstation.exe'){$bucket='executable'}
            elseif(@('$PLUGINSDIR/System.dll','$PLUGINSDIR/modern-wizard.bmp','$PLUGINSDIR/nsDialogs.dll','$PLUGINSDIR/nsis_tauri_utils.dll','$PLUGINSDIR/StartMenu.dll','$PLUGINSDIR/NSISdl.dll')|Where-Object{Test-ArchiveTargetMatch $key $_}){$bucket='nsis-support'}
            elseif($key-ceq'[0]'){$bucket='nsis-container'}
            throw "Installer archive size is blank. EntryBucket=$bucket EntryOrdinal=$entries."
          }
        }
        elseif(-not [uint64]::TryParse($text,[Globalization.NumberStyles]::None,[Globalization.CultureInfo]::InvariantCulture,[ref]$size)){throw 'Installer archive size is nonnumeric.'}
        if($size-gt256MB){throw 'Installer archive entry size exceeds its bound.'}
        $state.size=$size
      }
    }
    Complete-Entry
    if($entries -eq 0){throw 'Installer archive listing is empty.'}
    return [pscustomobject]@{entries=$entries;expandedBytes=[uint64]$state.total;excludedUninstaller=$state.excludedUninstaller}
}

function Invoke-BoundedSevenZipEntry {
    param([string]$Executable,[string]$Installer,[string]$ArchivePath,[string]$Destination,[uint64]$ExpectedBytes,[string]$ExpectedSha256)
    if(-not('DefaultDraftBoundedBinaryProcess'-as[type])){Add-Type -TypeDefinition @'
using System; using System.Diagnostics; using System.IO; using System.Text; using System.Threading; using System.Threading.Tasks;
public static class DefaultDraftBoundedBinaryProcess {
 sealed class Counter { public int Value; }
 static async Task Copy(Stream input,string path,ulong maximum,Action overflow){using(var output=new FileStream(path,FileMode.CreateNew,FileAccess.Write,FileShare.None)){var buffer=new byte[65536];ulong total=0;while(true){int count=await input.ReadAsync(buffer,0,buffer.Length);if(count==0)break;total+=(ulong)count;if(total>maximum){overflow();throw new InvalidOperationException("Binary output exceeded its exact bound.");}await output.WriteAsync(buffer,0,count);}if(total!=maximum)throw new InvalidOperationException("Binary output ended before its exact bound.");}}
 static async Task ReadError(StreamReader reader,int maximum,Counter counter,Action overflow){var buffer=new char[4096];while(true){int count=await reader.ReadAsync(buffer,0,buffer.Length);if(count==0)break;if(Interlocked.Add(ref counter.Value,count)>maximum){overflow();throw new InvalidOperationException("Error output exceeded its bound.");}}}
 public static void Run(string executable,string installer,string archivePath,string destination,ulong expectedBytes){var start=new ProcessStartInfo{FileName=executable,UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true};foreach(var value in new[]{"x","-so",installer,archivePath})start.ArgumentList.Add(value);using(var process=new Process{StartInfo=start}){if(!process.Start())throw new InvalidOperationException("Process did not start.");Action kill=()=>{try{if(!process.HasExited)process.Kill(true);}catch{}};var copy=Copy(process.StandardOutput.BaseStream,destination,expectedBytes,kill);var error=ReadError(process.StandardError,65536,new Counter(),kill);var completion=Task.WhenAll(copy,error,process.WaitForExitAsync());try{if(!completion.Wait(30000)){kill();throw new TimeoutException();}completion.GetAwaiter().GetResult();}catch{kill();throw;}if(process.ExitCode!=0)throw new InvalidOperationException("Process failed.");}}
}

'@}
    try{[DefaultDraftBoundedBinaryProcess]::Run($Executable,$Installer,$ArchivePath,$Destination,$ExpectedBytes)}catch{throw 'Bounded installer entry extraction failed, timed out, or exceeded its exact byte bound.'}
    if((Get-ExactSha256 -Path $Destination)-cne$ExpectedSha256){throw 'Streamed installer entry differs from its exact source receipt.'}
}

function Get-SevenZipVersionFact {
    param([string]$Executable,[scriptblock]$Runner)
    try{$lines=if($Runner){@(& $Runner)}else{@(Invoke-BoundedSevenZip -Executable $Executable -Arguments @('i') -TimeoutMilliseconds 5000 -Capture)};foreach($line in $lines){if($line-cmatch'^7-Zip (\d+\.\d+)'){return $Matches[1]}}}catch{}
    return 'unknown'
}

function Assert-ReleaseSourceVersion {
    param([string]$WorkflowRoot,[string]$SourceRoot,[string]$Version)
    & (Join-Path $WorkflowRoot 'scripts/assert-release-version.ps1') -ProjectRoot $SourceRoot -Version $Version
}

function Assert-PackagedApplicationIdentity {
    param([string]$Path,[string]$Version,[scriptblock]$VersionProvider)
    $facts=if($VersionProvider){&$VersionProvider $Path}else{(Get-Item -LiteralPath $Path).VersionInfo}
    if([string]$facts.FileVersion -cne $Version-or [string]$facts.ProductVersion -cne $Version-or [string]$facts.ProductName -cne 'PDF Workstation'){
      throw 'Packaged application product identity or version differs from the exact release source.'
    }
}

function Get-DefaultOcrPlanShape {
    param([string[]]$Paths)
    $suffixes=@('bin/tesseract.exe','tessdata/eng.traineddata','licenses/Tesseract-Apache-2.0.txt','licenses/Leptonica-BSD-2-Clause.txt','licenses/eng-fast-Apache-2.0.txt')
    $ocr=@($Paths|Where-Object{$_.StartsWith('resources/ocr/',[StringComparison]::OrdinalIgnoreCase)-or $_.ToLowerInvariant().Contains('/resources/ocr/')})
    if($ocr.Count -ne 5){throw 'Default installer must contain exactly five OCR resources.'}
    $identities=@($ocr|ForEach-Object{if($_ -cnotmatch '(?:^|/)resources/ocr/([a-f0-9]{64})/(.+)$'-or $suffixes -cnotcontains $Matches[2]){throw 'Default installer OCR resource path is invalid.'};$Matches[1]}|Sort-Object -Unique)
    if($identities.Count -ne 1){throw 'Default installer OCR resources do not share one identity.'}
    return [pscustomobject]@{Identity=[string]$identities[0];Files=@($suffixes|ForEach-Object{[pscustomobject]@{Suffix=$_}})}
}

function Assert-DefaultOcrPackage {
    param([object[]]$Files,[string]$Identity,[string]$SourceRoot,[string]$ApplicationPath,[string]$ExpectedPublisher)
    if($Identity -cnotmatch '^[a-f0-9]{64}$'){throw 'Default installer OCR identity is invalid.'}
    $pins=Get-Content -LiteralPath (Join-Path $SourceRoot 'scripts/ocr/pins.json') -Raw -Encoding UTF8|ConvertFrom-Json
    $pinned=@(
        ,@('tessdata/eng.traineddata','model',$pins.model.data.file,'eng.traineddata',$pins.model.data)
        ,@('licenses/Tesseract-Apache-2.0.txt','license',$pins.tesseract.license.output,'Tesseract-Apache-2.0.txt',$pins.tesseract.license)
        ,@('licenses/Leptonica-BSD-2-Clause.txt','license',$pins.leptonica.license.output,'Leptonica-BSD-2-Clause.txt',$pins.leptonica.license)
        ,@('licenses/eng-fast-Apache-2.0.txt','license',$pins.model.license.output,'eng-fast-Apache-2.0.txt',$pins.model.license)
    )
    $receipts=@()
    foreach($pin in $pinned){
        if([string]$pin[2] -cne $pin[3]-or [uint64]$pin[4].bytes -eq 0-or [string]$pin[4].sha256 -cnotmatch '^[A-F0-9]{64}$'){throw 'Tagged OCR pins are not the expected model and license receipts.'}
        $target="resources/ocr/$Identity/$($pin[0])";$match=Get-UniqueExtractedFile $Files $target 'Packaged OCR resource'
        if([uint64]$match.Length -ne [uint64]$pin[4].bytes-or (Get-ExactSha256 $match.FullName)-cne [string]$pin[4].sha256){throw 'Packaged OCR resource differs from the tagged pins.'}
        $receipts+=[pscustomobject][ordered]@{target=$target;role=$pin[1];bytes=[uint64]$match.Length;sha256=[string]$pin[4].sha256}
    }
    $engineTarget="resources/ocr/$Identity/bin/tesseract.exe";$engine=Get-UniqueExtractedFile $Files $engineTarget 'Packaged OCR engine';$engineSha256=Get-ExactSha256 $engine.FullName
    if($engineSha256.ToLowerInvariant() -cne $Identity){throw 'Packaged OCR engine does not match its identity folder.'}
    $null=Assert-TrustedWindowsSignature -Path $engine.FullName -ExpectedPublisher $ExpectedPublisher
    if(-not (Test-ExecutableContainsAscii -Path $ApplicationPath -Marker $engineSha256)){throw 'Packaged application does not embed the packaged OCR engine identity.'}
    $receipts+=[pscustomobject][ordered]@{target=$engineTarget;role='engine';bytes=[uint64]$engine.Length;sha256=$engineSha256}
    return @($receipts|Sort-Object target)
}

function Expand-DefaultInstaller {
    param([string]$Installer,[string]$Destination,[object[]]$BaseEntries)
    if(Test-Path -LiteralPath $Destination){throw 'Extraction destination must be fresh.'}
    $extractor=Get-InstallerExtractor
    $listing=@(Invoke-BoundedSevenZip -Executable $extractor.FullName -Arguments @('l','-slt',$Installer) -Capture)
    $welcome=@($BaseEntries|Where-Object{[string]$_.Target-ceq'resources/welcome.pdf'});if($welcome.Count-ne1){throw 'Exact welcome resource receipt is absent or ambiguous.'}
    $fallback=@{'resources/welcome.pdf'=[uint64]$welcome[0].Receipt.Bytes};$known=@{};foreach($entry in $BaseEntries){$key=[string]$entry.Target;if($known.Contains($key)){throw 'Base resource inventory has a duplicate path.'};$known[$key]=$true}
    try{$listingFacts=Assert-BoundedArchiveListing -Lines $listing -ExactFallbackSizes $fallback -KnownPaths $known}catch{$primary=$_.Exception.Message;$version=Get-SevenZipVersionFact $extractor.FullName;throw ($primary+' SevenZipVersion='+$version+'.')}
    $script:OptionalUninstallerExcluded=$null-ne$listingFacts.excludedUninstaller
    $archivePaths=Convert-SevenZipInventory -Lines $listing
    Assert-ArchiveResourceInventory -ArchivePaths $archivePaths -BaseEntries $BaseEntries -OcrPlan (Get-DefaultOcrPlanShape $archivePaths)
    [IO.Directory]::CreateDirectory($Destination)|Out-Null
    Assert-NoReparseAncestors -Path $Destination
    $welcomePath=Join-Path $Destination 'resources/welcome.pdf';[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($welcomePath))|Out-Null
    Invoke-BoundedSevenZipEntry -Executable $extractor.FullName -Installer $Installer -ArchivePath 'resources\welcome.pdf' -Destination $welcomePath -ExpectedBytes ([uint64]$welcome[0].Receipt.Bytes) -ExpectedSha256 ([string]$welcome[0].Receipt.Sha256)
    $extractArguments=@('x',$Installer,"-o$Destination",'-y','-bd','-bb0','-xr!resources\welcome.pdf')
    if($null-ne$listingFacts.excludedUninstaller){$extractArguments+='-x!uninstall.exe'}
    Invoke-BoundedSevenZip -Executable $extractor.FullName -Arguments $extractArguments
    Assert-NoReparseAncestors -Path $Destination
    foreach($entry in @(Get-ChildItem -LiteralPath $Destination -Recurse -Force)){
        Assert-NoReparseAncestors -Path $entry.FullName
        if(($entry.Attributes -band [IO.FileAttributes]::ReparsePoint)-ne 0){throw 'Extracted installer contains a reparse point.'}
    }
    return @(Get-ChildItem -LiteralPath $Destination -Recurse -File -Force)
}

function Assert-DefaultInventory {
    param([object[]]$Files,[string]$Root,[object[]]$BaseEntries)
    $canonical=[IO.Path]::GetFullPath($Root).TrimEnd('\')
    $actual=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    foreach($file in $Files){
        $path=[IO.Path]::GetFullPath([string]$file.FullName)
        if(-not $path.StartsWith($canonical+'\',[StringComparison]::OrdinalIgnoreCase)){throw 'Extracted file escaped the proof root.'}
        Assert-NoReparseAncestors -Path $path
        $relative=$path.Substring($canonical.Length+1).Replace('\','/')
        if([string]::IsNullOrWhiteSpace($relative)-or $relative.Contains('..')-or -not $actual.Add($relative)){throw 'Extracted inventory has an unsafe, duplicate, or case-colliding path.'}
    }
    $required=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    $null=$required.Add('pdf-workstation.exe')
    foreach($entry in $BaseEntries){$null=$required.Add([string]$entry.Target)}
    $ocrShape=Get-DefaultOcrPlanShape @($actual)
    foreach($file in $ocrShape.Files){$null=$required.Add("resources/ocr/$($ocrShape.Identity)/$($file.Suffix)")}
    foreach($path in @('$PLUGINSDIR/modern-wizard.bmp','$PLUGINSDIR/nsDialogs.dll','$PLUGINSDIR/nsis_tauri_utils.dll','$PLUGINSDIR/NSISdl.dll','$PLUGINSDIR/StartMenu.dll','$PLUGINSDIR/System.dll')){$null=$required.Add($path)}
    foreach($path in $required){if(-not $actual.Contains($path)){throw 'Extracted installer is missing an exact payload or NSIS support file.'}}
    foreach($path in $actual){if(-not $required.Contains($path)-and -not $path.Equals('uninstall.exe',[StringComparison]::OrdinalIgnoreCase)){throw 'Extracted installer contains an unexpected file.'}}
    if($actual.Count -ne $required.Count -and $actual.Count -ne ($required.Count+1)){throw 'Extracted installer cardinality is unsupported.'}
    return [pscustomobject]@{required=[uint64]$required.Count;actual=[uint64]$actual.Count;optionalUninstaller=$actual.Contains('uninstall.exe');ocrIdentity=$ocrShape.Identity}
}

function Get-UniqueExtractedFile {
    param([object[]]$Files,[string]$Suffix,[string]$Kind)
    $shape=$Suffix.Replace('\','/')
    $matches=@($Files|Where-Object{$value=$_.FullName.Replace('\','/');$value.Equals($shape,[StringComparison]::OrdinalIgnoreCase)-or $value.EndsWith('/'+$shape,[StringComparison]::OrdinalIgnoreCase)})
    if($matches.Count -ne 1){throw "$Kind is absent or ambiguous."}
    return $matches[0]
}

if($TargetVersion -cnotmatch '^\d+\.\d+\.\d+$'-or $TargetTag -cne "v$TargetVersion"-or $WorkflowSourceRevision -cnotmatch '^[a-f0-9]{40}$'-or $TargetSourceRevision -cnotmatch '^[a-f0-9]{40}$'-or [string]::IsNullOrWhiteSpace($ExpectedPublisher)){throw 'Draft identity inputs are malformed.'}
Assert-ReleaseSourceVersion -WorkflowRoot $ProjectRoot -SourceRoot $TargetSourceRoot -Version $TargetVersion
$releaseId=ConvertTo-ExactUInt64 $TargetReleaseId 'Release id';$installerId=ConvertTo-ExactUInt64 $InstallerAssetId 'Installer asset id';$signatureId=ConvertTo-ExactUInt64 $SignatureAssetId 'Signature asset id';$manifestId=ConvertTo-ExactUInt64 $ManifestAssetId 'Manifest asset id'
$installerLength=ConvertTo-ExactUInt64 $InstallerBytes 'Installer bytes';$signatureLength=ConvertTo-ExactUInt64 $SignatureBytes 'Signature bytes';$manifestLength=ConvertTo-ExactUInt64 $ManifestBytes 'Manifest bytes'
Assert-ReceiptShape $installerLength $InstallerSha256 'Installer';Assert-ReceiptShape $signatureLength $SignatureSha256 'Signature';Assert-ReceiptShape $manifestLength $ManifestSha256 'Manifest'
if($InstallerName -cne "PDF.Workstation_${TargetVersion}_x64-setup.exe"){throw 'Installer name is not the exact release asset name.'}
$runnerTemp=$env:RUNNER_TEMP;if([string]::IsNullOrWhiteSpace($runnerTemp)){throw 'RUNNER_TEMP is unavailable.'}
$assetRoot=Resolve-ProofPath $AssetRoot $runnerTemp -Fresh;$workRoot=Resolve-ProofPath $WorkRoot $runnerTemp -Fresh;$outputRoot=Resolve-ProofPath $OutputRoot $runnerTemp -Fresh
[IO.Directory]::CreateDirectory($assetRoot)|Out-Null;[IO.Directory]::CreateDirectory($workRoot)|Out-Null;[IO.Directory]::CreateDirectory($outputRoot)|Out-Null
if([string]::IsNullOrWhiteSpace($env:GITHUB_TOKEN)){throw 'GitHub token is unavailable.'}
$release=Invoke-GitHubJson "https://api.github.com/repos/$script:Repository/releases/$releaseId" $env:GITHUB_TOKEN
$expectedNames=@($InstallerName,"$InstallerName.sig",'latest.json')|Sort-Object
$actualNames=@($release.assets|ForEach-Object{[string]$_.name}|Sort-Object)
if([uint64]$release.id -ne $releaseId-or [string]$release.tag_name -cne $TargetTag-or -not [bool]$release.draft-or [bool]$release.prerelease-or [string]$release.target_commitish -cne $TargetSourceRevision-or $actualNames.Count -ne 3-or (Compare-Object $expectedNames $actualNames -CaseSensitive)){throw 'Release is not the exact source-bound three-asset draft.'}
$downloadSegment=Get-ExactReleaseAsset $release $installerId $InstallerName $installerLength $InstallerSha256
$null=Get-ExactReleaseAsset $release $signatureId "$InstallerName.sig" $signatureLength $SignatureSha256 $downloadSegment
$null=Get-ExactReleaseAsset $release $manifestId 'latest.json' $manifestLength $ManifestSha256 $downloadSegment
$installerPath=Join-Path $assetRoot $InstallerName;$signaturePath=Join-Path $assetRoot "$InstallerName.sig";$manifestPath=Join-Path $assetRoot 'latest.json'
Save-ExactReleaseAsset $installerId $installerPath $env:GITHUB_TOKEN $installerLength $InstallerSha256
Save-ExactReleaseAsset $signatureId $signaturePath $env:GITHUB_TOKEN $signatureLength $SignatureSha256
Save-ExactReleaseAsset $manifestId $manifestPath $env:GITHUB_TOKEN $manifestLength $ManifestSha256
$signatureText=(Get-Content -LiteralPath $signaturePath -Raw -Encoding UTF8).Trim();if([string]::IsNullOrWhiteSpace($signatureText)-or [Text.Encoding]::UTF8.GetByteCount($signatureText)-gt 65536){throw 'Detached updater signature text is malformed.'}
try{$manifest=Get-Content -LiteralPath $manifestPath -Raw -Encoding UTF8|ConvertFrom-Json}catch{throw 'Updater manifest is malformed.'}
$platform=$manifest.platforms.'windows-x86_64';$notes=Get-Content -LiteralPath (Join-Path $TargetSourceRoot 'docs/release-notes.md') -Raw -Encoding UTF8
$expectedUrl="https://github.com/$script:Repository/releases/download/$TargetTag/$InstallerName"
if([string]$manifest.version -cne $TargetVersion-or [string]$platform.url -cne $expectedUrl-or [string]$platform.signature -cne $signatureText-or [string]$manifest.notes -cne $notes){throw 'Updater manifest is not exactly bound to version, URL, signature text, and tagged notes.'}
$null=Assert-TrustedWindowsSignature -Path $installerPath -ExpectedPublisher $ExpectedPublisher
$null=& (Join-Path $PSScriptRoot 'assert-updater-signing-key.ps1') -SignaturePath $signaturePath -SourceRoot $TargetSourceRoot
$configuredUpdaterKey=[string](Get-Content -LiteralPath (Join-Path $TargetSourceRoot 'src-tauri/tauri.conf.json') -Raw -Encoding UTF8|ConvertFrom-Json).plugins.updater.pubkey;if($configuredUpdaterKey -cnotmatch '^[A-Za-z0-9+/=]{100,400}$'){throw 'Tagged updater public key is malformed.'}
$base=Get-BaseBundleResourceMap -ProjectRoot $TargetSourceRoot
if([uint64]$base.Count -ne 28){throw 'Default tagged-source resource inventory is not exactly 28 files.'}
$extractionRoot=Join-Path $workRoot 'extracted-installer';$files=Expand-DefaultInstaller $installerPath $extractionRoot $base.Entries
$inventory=Assert-DefaultInventory $files $extractionRoot $base.Entries
$application=Get-UniqueExtractedFile $files 'pdf-workstation.exe' 'Packaged application';Assert-PackagedApplicationIdentity -Path $application.FullName -Version $TargetVersion;$null=Assert-TrustedWindowsSignature -Path $application.FullName -ExpectedPublisher $ExpectedPublisher
if(-not (Test-ExecutableContainsAscii -Path $application.FullName -Marker $configuredUpdaterKey)){throw 'Packaged application does not embed the tagged updater public key.'}
$ocrReceipts=@(Assert-DefaultOcrPackage -Files $files -Identity $inventory.ocrIdentity -SourceRoot $TargetSourceRoot -ApplicationPath $application.FullName -ExpectedPublisher $ExpectedPublisher)
$resourceReceipts=@()
foreach($entry in $base.Entries){
    $match=Get-UniqueExtractedFile $files ([string]$entry.Target) 'Base resource'
    if([string]$entry.Target -ceq 'resources/pdfium/bin/pdfium.dll'){
        Assert-SignedPdfiumEquivalent -UnsignedPath $entry.Source -SignedPath $match.FullName -ExpectedPublisher $ExpectedPublisher
        $resourceReceipts+=[ordered]@{target=$entry.Target;bytes=[uint64]$match.Length;sha256=Get-ExactSha256 $match.FullName;unsignedBytes=[uint64]$entry.Receipt.Bytes;unsignedSha256=[string]$entry.Receipt.Sha256}
    }else{
        if([uint64]$match.Length -ne [uint64]$entry.Receipt.Bytes-or (Get-ExactSha256 $match.FullName)-cne [string]$entry.Receipt.Sha256){throw 'Extracted base resource differs from tagged source.'}
        $resourceReceipts+=[ordered]@{target=$entry.Target;bytes=[uint64]$match.Length;sha256=[string]$entry.Receipt.Sha256}
    }
}
$record=[ordered]@{
 schemaVersion=2;scope='Read-only default draft download and extraction proof, including the packaged OCR engine, model and licenses; no installation, launch, OCR recognition, updater cryptographic verification, or publication claim.';mode='default-draft-extraction';workflowSourceRevision=$WorkflowSourceRevision;targetVersion=$TargetVersion;targetTag=$TargetTag;targetSourceRevision=$TargetSourceRevision;releaseId=$releaseId
 assets=@([ordered]@{role='installer';id=$installerId;name=$InstallerName;bytes=$installerLength;sha256=$InstallerSha256},[ordered]@{role='detachedUpdaterSignature';id=$signatureId;name="$InstallerName.sig";bytes=$signatureLength;sha256=$SignatureSha256},[ordered]@{role='updaterManifest';id=$manifestId;name='latest.json';bytes=$manifestLength;sha256=$ManifestSha256})
 packagedApplication=[ordered]@{bytes=[uint64]$application.Length;sha256=Get-ExactSha256 $application.FullName}
 baseResources=@($resourceReceipts|Sort-Object target);ocr=[ordered]@{identity=$inventory.ocrIdentity;resources=$ocrReceipts};signatures=[ordered]@{expectedPublisher=$ExpectedPublisher;installer='Valid';application='Valid';pdfium='Valid';ocrEngine='Valid';trustedTimestampsRequired=$true}
 verification=[ordered]@{draft=$true;prerelease=$false;exactAssetInventory=$true;exactExtractedInventory=$true;targetSourceSixVersionsMatched=$true;packagedApplicationIdentityAndVersionMatched=$true;sourceBuildExecutableByteProvenanceReconstructed=$false;requiredExtractedFiles=$inventory.required;actualExtractedFiles=$inventory.actual;optionalUninstaller=$inventory.optionalUninstaller;optionalUninstallerExcluded=[bool]$script:OptionalUninstallerExcluded;ocrBundled=$true;updaterSignatureKeyIsReleaseSigningKey=$true;packagedApplicationEmbedsTaggedUpdaterKey=$true;manifestSignatureTextBound=$true;detachedUpdaterCryptographyVerified=$false;installedBehaviorVerified=$false}
}
[IO.File]::WriteAllText((Join-Path $outputRoot 'default-draft-verification.json'),($record|ConvertTo-Json -Depth 20),[Text.UTF8Encoding]::new($false))
