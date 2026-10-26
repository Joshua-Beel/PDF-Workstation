[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$TargetSourceRoot,[Parameter(Mandatory=$true)][string]$WorkRoot)
$ErrorActionPreference='Stop';Set-StrictMode -Version Latest
$assetId=[uint64]441936973;$assetName='pdfium-win-x64.tgz';$assetBytes=[uint64]3733154;$assetSha256='73CC0DE638AC2095E7445BF56A38200A5B7C7CA0E9F4BA144598F2457377AC08'
$metadataUrl='https://api.github.com/repos/bblanchon/pdfium-binaries/releases/tags/chromium%2F7881'
$downloadUrl='https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/7881/pdfium-win-x64.tgz'
$expectedEntries=@('LICENSE','PDFiumConfig.cmake','VERSION','args.gn','bin/','bin/pdfium.dll','include/','include/fpdf_edit.h','include/fpdf_searchex.h','include/fpdf_javascript.h','include/fpdf_transformpage.h','include/fpdf_thumbnail.h','include/fpdf_fwlevent.h','include/fpdfview.h','include/fpdfview.h.orig','include/fpdf_structtree.h','include/fpdf_save.h','include/fpdf_doc.h','include/fpdf_formfill.h','include/fpdf_dataavail.h','include/fpdf_ppo.h','include/fpdf_annot.h','include/fpdf_text.h','include/cpp/','include/cpp/fpdf_scopers.h','include/cpp/fpdf_deleters.h','include/fpdf_progressive.h','include/fpdf_flatten.h','include/fpdf_catalog.h','include/fpdf_sysfontinfo.h','include/fpdf_signature.h','include/fpdf_ext.h','include/fpdf_attachment.h','lib/','lib/pdfium.dll.lib','licenses/','licenses/pdfium.txt','licenses/abseil.txt','licenses/simdutf.txt','licenses/llvm-libc.txt','licenses/freetype.txt','licenses/zlib.txt','licenses/libjpeg_turbo.ijg','licenses/fast_float.txt','licenses/agg23.txt','licenses/libopenjpeg.txt','licenses/icu.txt','licenses/libtiff.txt','licenses/libpng.txt','licenses/lcms.txt','licenses/libjpeg_turbo.md')

function Invoke-BoundedTar {
 param([string[]]$Arguments)
 if(-not('PdfiumBoundedProcess'-as[type])){Add-Type -TypeDefinition @'
using System; using System.Diagnostics; using System.Text; using System.Threading; using System.Threading.Tasks;
public static class PdfiumBoundedProcess {
 sealed class Counter { public int Value; }
 static async Task<string> Read(System.IO.StreamReader reader,int maximum,Counter counter,Action overflow){var value=new StringBuilder();var buffer=new char[4096];while(true){int count=await reader.ReadAsync(buffer,0,buffer.Length);if(count==0)break;if(Interlocked.Add(ref counter.Value,count)>maximum){overflow();throw new InvalidOperationException("Output bound exceeded.");}value.Append(buffer,0,count);}return value.ToString();}
 public static string Run(string[] arguments){var start=new ProcessStartInfo{FileName="tar",UseShellExecute=false,CreateNoWindow=true,RedirectStandardOutput=true,RedirectStandardError=true};foreach(var value in arguments)start.ArgumentList.Add(value);using(var process=new Process{StartInfo=start}){if(!process.Start())throw new InvalidOperationException("Start failed.");Action kill=()=>{try{if(!process.HasExited)process.Kill(true);}catch{}};var counter=new Counter();var stdout=Read(process.StandardOutput,65536,counter,kill);var stderr=Read(process.StandardError,65536,counter,kill);var exited=process.WaitForExitAsync();var completion=Task.WhenAll(stdout,stderr,exited);try{if(!completion.Wait(30000)){kill();throw new TimeoutException();}completion.GetAwaiter().GetResult();}catch{kill();throw;}if(process.ExitCode!=0)throw new InvalidOperationException("tar failed.");return stdout.Result;}}
}
'@}
 try{return [PdfiumBoundedProcess]::Run($Arguments)}catch{throw 'Bounded tar failed, timed out, or exceeded its output bound.'}
}

function Assert-PdfiumReleaseMetadata {
 param($Metadata)
 if([string]$Metadata.tag_name-cne'chromium/7881'-or[bool]$Metadata.draft-or[bool]$Metadata.prerelease-or[string]::IsNullOrWhiteSpace([string]$Metadata.published_at)){throw 'Pinned PDFium release identity changed.'}
 $matches=@($Metadata.assets|Where-Object{[uint64]$_.id-eq$assetId})
 if($matches.Count-ne1-or[string]$matches[0].name-cne$assetName-or[uint64]$matches[0].size-ne$assetBytes-or[string]$matches[0].state-cne'uploaded'-or[string]$matches[0].digest-cne('sha256:'+$assetSha256.ToLowerInvariant())-or[string]$matches[0].browser_download_url-cne$downloadUrl){throw 'Pinned PDFium release metadata changed.'}
}

function ConvertFrom-PdfiumTarListing {
 param([string]$Listing)
 $facts=@();[uint64]$total=0
 foreach($line in @($Listing-split"`r?`n"|Where-Object{$_})){
  if($line-cnotmatch '^([d-])\S*\s+\d+\s+\S+\s+\S+\s+(\d+)\s+\S+\s+\S+\s+\S+\s+(.+)$'){throw 'Pinned PDFium archive listing is malformed.'}
  $kind=$Matches[1];[uint64]$bytes=[uint64]$Matches[2];$name=$Matches[3]
  if(($kind-ceq'd'-and-not$name.EndsWith('/'))-or($kind-ceq'-'-and$name.EndsWith('/'))-or$bytes-gt32MB){throw 'Pinned PDFium archive entry type or size is unsafe.'}
  $total+=$bytes;if($total-gt64MB){throw 'Pinned PDFium archive expanded size is unsafe.'};$facts+=,$name
 }
 if($facts.Count-ne$expectedEntries.Count-or(Compare-Object $expectedEntries $facts -CaseSensitive)){throw 'Pinned PDFium archive inventory changed.'};return $facts
}

function Invoke-PdfiumFetchWithRetry {
 param([scriptblock]$Action,[string]$Kind,[int[]]$DelaySeconds=@(0,5,15),[scriptblock]$SleepProvider)
 if($DelaySeconds.Count-lt1-or$DelaySeconds.Count-gt5){throw 'PDFium fetch retry count is out of bounds.'}
 for($attempt=1;$attempt-le$DelaySeconds.Count;$attempt++){
  $delay=$DelaySeconds[$attempt-1];if($delay-lt0-or$delay-gt60){throw 'PDFium fetch retry delay is out of bounds.'}
  if($delay-gt0){if($SleepProvider){&$SleepProvider $delay}else{Start-Sleep -Seconds $delay}}
  try{return (&$Action)}catch{
   $reason=[string]$_.Exception.Message;if($reason.Length-gt160){$reason=$reason.Substring(0,160)}
   [Console]::Out.WriteLine(('PDFIUM_FETCH_DIAG kind={0} attempt={1} failed={2}' -f $Kind,$attempt,($reason-replace'[
]',' ')))
   if($attempt-ge$DelaySeconds.Count){throw}
  }
 }
}

$target=[IO.Path]::GetFullPath($TargetSourceRoot).TrimEnd('\');$work=[IO.Path]::GetFullPath($WorkRoot).TrimEnd('\')
if(Test-Path -LiteralPath $work){throw 'PDFium preparation root must be fresh.'};[IO.Directory]::CreateDirectory($work)|Out-Null
$handler=[Net.Http.HttpClientHandler]::new();$handler.AllowAutoRedirect=$false;$client=[Net.Http.HttpClient]::new($handler);$client.Timeout=[TimeSpan]::FromSeconds(120);$downloadHandler=$null;$downloadClient=$null
try{
 $client.DefaultRequestHeaders.UserAgent.ParseAdd('Smacrobat-DraftVerifier')
 $metadata=Invoke-PdfiumFetchWithRetry -Kind 'metadata' -Action {
  $metadataResponse=$client.GetAsync($metadataUrl,[Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
  try{if(-not$metadataResponse.IsSuccessStatusCode-or$metadataResponse.RequestMessage.RequestUri.AbsoluteUri-cne$metadataUrl){throw ('Pinned PDFium metadata request failed with HTTP status '+[int]$metadataResponse.StatusCode+'.')};$metadataStream=$metadataResponse.Content.ReadAsStreamAsync().GetAwaiter().GetResult();$memory=[IO.MemoryStream]::new();try{$buffer=[byte[]]::new(16384);while(($read=$metadataStream.Read($buffer,0,$buffer.Length))-gt0){if($memory.Length+$read-gt1MB){throw 'Pinned PDFium metadata exceeded its bound.'};$memory.Write($buffer,0,$read)};$metadata=[Text.Encoding]::UTF8.GetString($memory.ToArray())|ConvertFrom-Json}finally{$memory.Dispose();$metadataStream.Dispose()}}finally{$metadataResponse.Dispose()}
  $metadata
 }
 Assert-PdfiumReleaseMetadata $metadata
 $archive=Join-Path $work $assetName;$downloadHandler=[Net.Http.HttpClientHandler]::new();$downloadHandler.AllowAutoRedirect=$true;$downloadHandler.MaxAutomaticRedirections=5;$downloadClient=[Net.Http.HttpClient]::new($downloadHandler);$downloadClient.Timeout=[TimeSpan]::FromSeconds(120);$downloadClient.DefaultRequestHeaders.UserAgent.ParseAdd('Smacrobat-DraftVerifier')
 $null=Invoke-PdfiumFetchWithRetry -Kind 'download' -Action {
  if(Test-Path -LiteralPath $archive){[IO.File]::Delete($archive)}
  $response=$downloadClient.GetAsync($downloadUrl,[Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
  try{if(-not$response.IsSuccessStatusCode){throw ('Pinned PDFium download failed with HTTP status '+[int]$response.StatusCode+'.')};$stream=$response.Content.ReadAsStreamAsync().GetAwaiter().GetResult();$file=[IO.File]::Open($archive,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None);try{[uint64]$total=0;$buffer=[byte[]]::new(65536);while(($read=$stream.Read($buffer,0,$buffer.Length))-gt0){$total+=[uint64]$read;if($total-gt$assetBytes){throw 'Pinned PDFium download exceeded its receipt.'};$file.Write($buffer,0,$read)};if($total-ne$assetBytes){throw 'Pinned PDFium download ended before its receipt.'}}finally{$file.Dispose();$stream.Dispose()}}finally{$response.Dispose()}
  $true
 }
}finally{if($null-ne$downloadClient){$downloadClient.Dispose()};if($null-ne$downloadHandler){$downloadHandler.Dispose()};$client.Dispose();$handler.Dispose()}
if((Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash-cne$assetSha256){throw 'Pinned PDFium archive hash differs from its receipt.'}
$null=ConvertFrom-PdfiumTarListing (Invoke-BoundedTar @('-tvzf',$archive))
$extract=Join-Path $work 'extract';[IO.Directory]::CreateDirectory($extract)|Out-Null;$required=@('LICENSE','VERSION','args.gn','bin/pdfium.dll')+@($expectedEntries|Where-Object{$_.StartsWith('licenses/')-and-not$_.EndsWith('/')});$null=Invoke-BoundedTar (@('-xzf',$archive,'-C',$extract,'--')+$required)
foreach($relative in $required){ $source=Join-Path $extract $relative;$resolved=[IO.Path]::GetFullPath($source);if(-not$resolved.StartsWith($extract+'\',[StringComparison]::OrdinalIgnoreCase)-or-not(Test-Path -LiteralPath $resolved -PathType Leaf)-or(Get-Item -LiteralPath $resolved).Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)){throw 'Pinned PDFium extracted resource is unsafe.'} }
foreach($item in Get-ChildItem -LiteralPath $extract -Recurse -Force){if($item.Attributes.HasFlag([IO.FileAttributes]::ReparsePoint)){throw 'Pinned PDFium extraction contains a reparse point.'}}
$destination=Join-Path $target 'src-tauri/resources/pdfium';if(Test-Path -LiteralPath $destination){throw 'Target PDFium resource root must be fresh.'};[IO.Directory]::CreateDirectory((Join-Path $destination 'bin'))|Out-Null;[IO.Directory]::CreateDirectory((Join-Path $destination 'licenses'))|Out-Null
[IO.File]::Copy((Join-Path $extract 'LICENSE'),(Join-Path $destination 'LICENSE'));[IO.File]::Copy((Join-Path $extract 'VERSION'),(Join-Path $destination 'VERSION'));[IO.File]::Copy((Join-Path $extract 'args.gn'),(Join-Path $destination 'args.gn'));[IO.File]::Copy((Join-Path $extract 'bin/pdfium.dll'),(Join-Path $destination 'bin/pdfium.dll'))
foreach($license in Get-ChildItem -LiteralPath (Join-Path $extract 'licenses') -File){[IO.File]::Copy($license.FullName,(Join-Path $destination ('licenses/'+$license.Name)))}
