[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'installed-reading-tools.ps1')
. (Join-Path $PSScriptRoot 'installed-print-dialog.ps1')

$script:StructuralPins = [ordered]@{
    LaunchTimeoutMilliseconds = 180000
    PickerTimeoutMilliseconds = 180000
    SaveTimeoutMilliseconds = 180000
    ProductTimeoutMilliseconds = 180000
    PortReleaseTimeoutMilliseconds = 180000
    InputPages = 6
    CombinePages = 12
    InsertPages = 12
    ReplacePages = 10
    OutputBytesMaximum = 256MB
    PdfRenderSize = 160
    PdfiumProofTimeoutMilliseconds = 30000
    PdfiumProofRecordBytesMaximum = 64KB
}

function Assert-StructuralExactProperties {
    param([Parameter(Mandatory = $true)]$Value,[Parameter(Mandatory = $true)][string[]]$Expected,[Parameter(Mandatory = $true)][string]$Kind)
    $actual = @($Value.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    $wanted = @($Expected | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $wanted.Count -or (Compare-Object $wanted $actual -CaseSensitive)) { throw "$Kind has an unexpected or missing property." }
}

function Assert-StructuralPdfiumReceipt {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)]$Receipt,[Parameter(Mandatory = $true)][string]$Kind)
    Assert-StructuralExactProperties -Value $Receipt -Expected @('target','bytes','sha256','unsignedBytes','unsignedSha256') -Kind "$Kind receipt"
    if ([string]$Receipt.target -cne 'resources/pdfium/bin/pdfium.dll' -or [uint64]$Receipt.unsignedBytes -eq 0 -or [string]$Receipt.unsignedSha256 -cnotmatch '^[A-F0-9]{64}$') { throw "$Kind receipt is not the signed PDFium base-resource record." }
    if (-not [IO.Path]::GetFullPath($Path).EndsWith('\resources\pdfium\bin\pdfium.dll',[StringComparison]::OrdinalIgnoreCase)) { throw "$Kind is not the installed PDFium resource named by its receipt." }
    Assert-StructuralFileReceipt -Path $Path -Receipt ([pscustomobject][ordered]@{ bytes = [uint64]$Receipt.bytes; sha256 = [string]$Receipt.sha256 }) -Kind $Kind
}

function Assert-StructuralFileReceipt {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)]$Receipt,[Parameter(Mandatory = $true)][string]$Kind)
    Assert-StructuralExactProperties -Value $Receipt -Expected @('bytes','sha256') -Kind "$Kind receipt"
    Assert-NoReparseAncestors -Path $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Kind is missing." }
    $item = Get-Item -LiteralPath $Path -Force
    if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or [uint64]$item.Length -ne [uint64]$Receipt.bytes -or
        [string]$Receipt.sha256 -cnotmatch '^[A-F0-9]{64}$' -or (Get-ExactSha256 -Path $Path) -cne [string]$Receipt.sha256) {
        throw "$Kind does not match its exact receipt."
    }
}

function Wait-StructuralStableFile {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$SnapshotProvider,[scriptblock]$SleepProvider,[scriptblock]$UtcNowProvider)
    $lastBytes = [uint64]0; $lastSha256 = ''; $stable = 0
    while ($true) {
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        try {
            $snapshot = if ($SnapshotProvider) { & $SnapshotProvider $Path } else {
                Assert-NoReparseAncestors -Path $Path
                $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
                [pscustomobject]@{exists=(-not $item.PSIsContainer);reparse=(($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0);bytes=[uint64]$item.Length;sha256=Get-ExactSha256 -Path $Path}
            }
            if ($snapshot.exists -is [bool] -and [bool]$snapshot.exists -and $snapshot.reparse -is [bool] -and -not [bool]$snapshot.reparse -and
                [uint64]$snapshot.bytes -gt 0 -and [uint64]$snapshot.bytes -le [uint64]$script:StructuralPins.OutputBytesMaximum -and [string]$snapshot.sha256 -cmatch '^[A-F0-9]{64}$') {
                if ([uint64]$snapshot.bytes -eq $lastBytes -and [string]$snapshot.sha256 -ceq $lastSha256) { $stable++ } else { $stable = 1; $lastBytes = [uint64]$snapshot.bytes; $lastSha256 = [string]$snapshot.sha256 }
                if ($stable -ge 3) { return [pscustomobject][ordered]@{bytes=$lastBytes;sha256=$lastSha256} }
            } else { $stable = 0; $lastBytes = 0; $lastSha256 = '' }
        } catch { $stable = 0; $lastBytes = 0; $lastSha256 = '' }
        if ($SleepProvider) { & $SleepProvider 200 } else { Start-Sleep -Milliseconds 200 }
    }
    throw 'Structural PDF output did not become a positive bounded stable file before its deadline.'
}

function Initialize-StructuralPdfiumProof {
    if ('SignedPdfiumStructuralProof' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;
public sealed class SignedPdfiumStructuralProof {
  [DllImport("kernel32.dll",CharSet=CharSet.Unicode,SetLastError=true)] static extern IntPtr LoadLibraryExW(string path,IntPtr file,uint flags);
  [DllImport("kernel32.dll",CharSet=CharSet.Ansi,SetLastError=true)] static extern IntPtr GetProcAddress(IntPtr module,string name);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void Init();
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr LoadMem(IntPtr data,ulong length,IntPtr password);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void CloseDocument(IntPtr document);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int PageCount(IntPtr document);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr LoadPage(IntPtr document,int index);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void ClosePage(IntPtr page);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate double PageMetric(IntPtr page);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr BitmapCreate(int width,int height,int alpha);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void BitmapDestroy(IntPtr bitmap);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void BitmapFill(IntPtr bitmap,int left,int top,int width,int height,uint color);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void Render(IntPtr bitmap,IntPtr page,int left,int top,int width,int height,int rotate,int flags);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr BitmapBuffer(IntPtr bitmap);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int BitmapInt(IntPtr bitmap);
  readonly IntPtr module; readonly Init init; readonly LoadMem load; readonly CloseDocument closeDocument; readonly PageCount pageCount; readonly LoadPage loadPage; readonly ClosePage closePage; readonly PageMetric pageWidth,pageHeight; readonly BitmapCreate bitmapCreate; readonly BitmapDestroy bitmapDestroy; readonly BitmapFill bitmapFill; readonly Render render; readonly BitmapBuffer bitmapBuffer; readonly BitmapInt bitmapStride;
  T Get<T>(string name) where T:Delegate { var address=GetProcAddress(module,name); if(address==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium is missing a structural proof export."); return Marshal.GetDelegateForFunctionPointer<T>(address); }
  public SignedPdfiumStructuralProof(string path){module=LoadLibraryExW(Path.GetFullPath(path),IntPtr.Zero,0x00000100|0x00001000);if(module==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not be loaded from its exact installed path.");init=Get<Init>("FPDF_InitLibrary");load=Get<LoadMem>("FPDF_LoadMemDocument64");closeDocument=Get<CloseDocument>("FPDF_CloseDocument");pageCount=Get<PageCount>("FPDF_GetPageCount");loadPage=Get<LoadPage>("FPDF_LoadPage");closePage=Get<ClosePage>("FPDF_ClosePage");pageWidth=Get<PageMetric>("FPDF_GetPageWidth");pageHeight=Get<PageMetric>("FPDF_GetPageHeight");bitmapCreate=Get<BitmapCreate>("FPDFBitmap_Create");bitmapDestroy=Get<BitmapDestroy>("FPDFBitmap_Destroy");bitmapFill=Get<BitmapFill>("FPDFBitmap_FillRect");render=Get<Render>("FPDF_RenderPageBitmap");bitmapBuffer=Get<BitmapBuffer>("FPDFBitmap_GetBuffer");bitmapStride=Get<BitmapInt>("FPDFBitmap_GetStride");init();}
  public sealed class Proof { public int Pages{get;set;} public double[] WidthPoints{get;set;} public double[] HeightPoints{get;set;} public string[] PageFingerprintSha256{get;set;} }
  public Proof Inspect(string path,int renderSize,long maximumBytes,int expectedPages){if(expectedPages<=0||expectedPages>64)throw new ArgumentOutOfRangeException(nameof(expectedPages));var bytes=File.ReadAllBytes(path);if(bytes.Length<=0||bytes.LongLength>maximumBytes)throw new InvalidOperationException("Structural PDF is empty or oversized.");var pinned=GCHandle.Alloc(bytes,GCHandleType.Pinned);IntPtr document=IntPtr.Zero;try{document=load(pinned.AddrOfPinnedObject(),(ulong)bytes.LongLength,IntPtr.Zero);if(document==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not parse a structural PDF.");int pages=pageCount(document);if(pages!=expectedPages)throw new InvalidOperationException("Structural PDF page count does not match the exact expected count.");var widths=new double[pages];var heights=new double[pages];var hashes=new string[pages];for(int index=0;index<pages;index++){IntPtr page=IntPtr.Zero,bitmap=IntPtr.Zero;try{page=loadPage(document,index);if(page==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not load a structural PDF page.");double width=pageWidth(page),height=pageHeight(page);if(!(width>0&&height>0&&width<=20000&&height<=20000))throw new InvalidOperationException("Structural PDF page dimensions are invalid.");widths[index]=width;heights[index]=height;bitmap=bitmapCreate(renderSize,renderSize,1);if(bitmap==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not allocate a structural proof bitmap.");bitmapFill(bitmap,0,0,renderSize,renderSize,0xFFFFFFFF);render(bitmap,page,0,0,renderSize,renderSize,0,0x801);int stride=bitmapStride(bitmap);if(stride<renderSize*4||stride>renderSize*8)throw new InvalidOperationException("Structural proof bitmap stride is invalid.");var raw=new byte[stride*renderSize];Marshal.Copy(bitmapBuffer(bitmap),raw,0,raw.Length);int ink=0;for(int y=0;y<renderSize;y++)for(int x=0;x<renderSize;x++){int offset=y*stride+x*4;if(raw[offset]<245||raw[offset+1]<245||raw[offset+2]<245)ink++;}if(ink<64)throw new InvalidOperationException("Structural PDF page has insufficient rendered content.");using(var sha=SHA256.Create()){sha.TransformBlock(BitConverter.GetBytes(width),0,8,null,0);sha.TransformBlock(BitConverter.GetBytes(height),0,8,null,0);sha.TransformFinalBlock(raw,0,raw.Length);hashes[index]=BitConverter.ToString(sha.Hash).Replace("-","");}}finally{if(bitmap!=IntPtr.Zero)bitmapDestroy(bitmap);if(page!=IntPtr.Zero)closePage(page);}}return new Proof{Pages=pages,WidthPoints=widths,HeightPoints=heights,PageFingerprintSha256=hashes};}finally{if(document!=IntPtr.Zero)closeDocument(document);if(pinned.IsAllocated)pinned.Free();}}
}
'@
}

function Get-StructuralPdfiumProofInProcess {
    param([Parameter(Mandatory = $true)][string]$PdfiumPath,[Parameter(Mandatory = $true)][string]$PdfPath,[Parameter(Mandatory = $true)][ValidateRange(1,64)][int]$ExpectedPages)
    Initialize-StructuralPdfiumProof
    $engine = [SignedPdfiumStructuralProof]::new($PdfiumPath)
    return $engine.Inspect($PdfPath,[int]$script:StructuralPins.PdfRenderSize,[long]$script:StructuralPins.OutputBytesMaximum,$ExpectedPages)
}

function Get-StructuralPdfiumProof {
    param(
        [Parameter(Mandatory = $true)][string]$PdfiumPath,
        [Parameter(Mandatory = $true)][string]$PdfPath,
        [Parameter(Mandatory = $true)][ValidateRange(1,64)][int]$ExpectedPages,
        [Parameter(Mandatory = $true)][string]$ProofPath,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [string]$HelperPath = (Join-Path $PSScriptRoot 'installed-structural-pdfium-proof.ps1')
    )
    $proofDeadline = [datetime]::UtcNow.AddMilliseconds($script:StructuralPins.PdfiumProofTimeoutMilliseconds)
    if ($proofDeadline -gt $Deadline) { $proofDeadline = $Deadline }
    if ([datetime]::UtcNow -ge $proofDeadline) { throw 'Structural PDFium proof deadline expired before its child process launch.' }
    foreach ($path in @($PdfiumPath,$PdfPath,$HelperPath)) { Assert-NoReparseAncestors -Path $path; if (-not (Test-Path -LiteralPath $path -PathType Leaf)) { throw 'A structural PDFium proof input is missing.' } }
    Assert-NoReparseAncestors -Path $ProofPath
    if (Test-Path -LiteralPath $ProofPath) { throw 'Structural PDFium proof record path must be fresh.' }
    $proofParent = Split-Path -Parent $ProofPath
    if (-not (Test-Path -LiteralPath $proofParent -PathType Container)) { throw 'Structural PDFium proof record parent is missing.' }
    $powerShellPath = (Get-Process -Id $PID -ErrorAction Stop).Path
    $capture = $null; $process = $null
    try {
        $capture = Start-BoundedDiscardProcess -Path $powerShellPath -Arguments @('-NoProfile','-NonInteractive','-File',$HelperPath,'-PdfiumPath',$PdfiumPath,'-PdfPath',$PdfPath,'-ExpectedPages',[string]$ExpectedPages,'-OutputPath',$ProofPath) -MaximumCharacters 4096 -Environment (Get-MinimalWindowsProcessEnvironment)
        $capture.Start(); $process = $capture.Process
        while (-not $process.HasExited -and [datetime]::UtcNow -lt $proofDeadline) {
            $remaining = [int][Math]::Floor(($proofDeadline - [datetime]::UtcNow).TotalMilliseconds)
            if ($remaining -le 0) { break }
            $null = $process.WaitForExit([Math]::Min(250,$remaining))
        }
        if (-not $process.HasExited) {
            $killOutcome = Wait-BoundedOwnedProcessExit -Process $process -Deadline ([datetime]::UtcNow.AddSeconds(5))
            throw "Structural PDFium proof exceeded its killable deadline; processOutcome=$killOutcome."
        }
        $process.WaitForExit()
        if ($capture.Exceeded -or $process.ExitCode -ne 0) { throw 'Structural PDFium proof child failed or exceeded its diagnostic cap.' }
    } finally {
        if ($capture) { $capture.Dispose() }
    }
    if ([datetime]::UtcNow -ge $proofDeadline) { throw 'Structural PDFium proof completed after its deadline.' }
    $item = Get-Item -LiteralPath $ProofPath -Force -ErrorAction Stop
    if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $item.Length -le 0 -or $item.Length -gt $script:StructuralPins.PdfiumProofRecordBytesMaximum) { throw 'Structural PDFium proof record was empty, linked, or oversized.' }
    $json = [IO.File]::ReadAllText($ProofPath,[Text.UTF8Encoding]::new($false,$true))
    $proof = $json | ConvertFrom-Json
    Assert-StructuralExactProperties -Value $proof -Expected @('pages','widthPoints','heightPoints','pageFingerprintSha256') -Kind 'Structural PDFium proof record'
    if ([int]$proof.pages -ne $ExpectedPages -or @($proof.widthPoints).Count -ne $ExpectedPages -or @($proof.heightPoints).Count -ne $ExpectedPages -or @($proof.pageFingerprintSha256).Count -ne $ExpectedPages -or
        @($proof.widthPoints | Where-Object { $_ -isnot [int] -and $_ -isnot [long] -and $_ -isnot [double] -and $_ -isnot [decimal] }).Count -ne 0 -or
        @($proof.heightPoints | Where-Object { $_ -isnot [int] -and $_ -isnot [long] -and $_ -isnot [double] -and $_ -isnot [decimal] }).Count -ne 0 -or
        @($proof.pageFingerprintSha256 | Where-Object { [string]$_ -cnotmatch '^[A-F0-9]{64}$' }).Count -ne 0) { throw 'Structural PDFium proof record did not match its exact expected page contract.' }
    return [pscustomobject][ordered]@{Pages=[int]$proof.pages;WidthPoints=[double[]]$proof.widthPoints;HeightPoints=[double[]]$proof.heightPoints;PageFingerprintSha256=[string[]]$proof.pageFingerprintSha256}
}

function Assert-StructuralFingerprintSequence {
    param([Parameter(Mandatory = $true)][string[]]$Expected,[Parameter(Mandatory = $true)][string[]]$Actual,[Parameter(Mandatory = $true)][string]$Kind)
    if ($Expected.Count -ne $Actual.Count -or @($Actual | Where-Object { [string]$_ -cnotmatch '^[A-F0-9]{64}$' }).Count -ne 0) { throw "$Kind page fingerprint count or format was invalid." }
    for ($index=0;$index -lt $Expected.Count;$index++) { if ([string]$Expected[$index] -cne [string]$Actual[$index]) { throw "$Kind page fingerprint order did not match its exact source sequence." } }
}

function Wait-StructuralDriverReady {
    param([Parameter(Mandatory = $true)]$Driver,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$StatusProvider,[scriptblock]$SleepProvider,[scriptblock]$UtcNowProvider)
    $status = $null
    while ($true) {
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        try {
            $status = if ($StatusProvider) { & $StatusProvider $Deadline } else { Invoke-BoundedLoopbackJson -Method GET -Path '/status' -Deadline $Deadline }
            if ($null -ne $status -and $null -ne $status.value -and $status.value.ready -is [bool] -and [bool]$status.value.ready) { return $status }
        } catch { }
        if ($Driver.HasExited) { throw 'Pinned tauri-driver exited before structural page-tools readiness.' }
        if ($SleepProvider) { & $SleepProvider 200 } else { Start-Sleep -Milliseconds 200 }
    }
    throw 'Pinned tauri-driver did not become ready for structural page tools.'
}

function New-StructuralDeadline {
    param([Parameter(Mandatory = $true)][ValidateSet('launch','picker','save','product')][string]$Phase)
    $milliseconds = switch ($Phase) {
        'launch' { [int]$script:StructuralPins.LaunchTimeoutMilliseconds }
        'picker' { [int]$script:StructuralPins.PickerTimeoutMilliseconds }
        'save' { [int]$script:StructuralPins.SaveTimeoutMilliseconds }
        'product' { [int]$script:StructuralPins.ProductTimeoutMilliseconds }
    }
    if ($milliseconds -lt 1 -or $milliseconds -gt 180000) { throw 'A structural page-tools phase timeout exceeded its exact cap.' }
    return [datetime]::UtcNow.AddMilliseconds($milliseconds)
}

function Wait-StructuralDocumentState {
    param(
        [Parameter(Mandatory = $true)][string]$SessionId,
        [Parameter(Mandatory = $true)][string]$ExpectedName,
        [Parameter(Mandatory = $true)][ValidateRange(1,4096)][int]$ExpectedPages,
        [Parameter(Mandatory = $true)][ValidateRange(1,16)][int]$ExpectedTabs,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $nameJson = $ExpectedName | ConvertTo-Json -Compress
    $script = @"
const expected=$nameJson,tabs=[...document.querySelectorAll('div')].filter(x=>x.querySelector(':scope>button>span')&&x.querySelector(':scope>button[aria-label^="Close "]')),active=tabs.filter(x=>typeof x.className==='string'&&x.className.includes('selectedTab')),name=active.length===1?(active[0].querySelector(':scope>button>span')?.textContent||'').trim():'',input=document.querySelector('input[aria-label="Page number"]'),images=[...document.querySelectorAll('img[alt^="Page "]')].filter(x=>/^Page \d+$/.test(x.getAttribute('alt')||''));return {tabs:tabs.length,active:active.length,name:name,pages:Number.parseInt(input?.max||'0',10)||0,loaded:images.filter(x=>x.complete&&x.naturalWidth>0&&x.naturalHeight>0).length,alerts:document.querySelectorAll('[role="alert"]').length,dialogs:document.querySelectorAll('dialog[open]').length};
"@
    return Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind "structural document $ExpectedName" -Script $script -Predicate {
        param($value)
        [int]$value.tabs -eq $ExpectedTabs -and [int]$value.active -eq 1 -and [string]$value.name -ceq $ExpectedName -and
            [int]$value.pages -eq $ExpectedPages -and [int]$value.loaded -gt 0 -and [int]$value.alerts -eq 0 -and [int]$value.dialogs -eq 0
    }
}

function Select-StructuralTab {
    param([string]$SessionId,[string]$Name,[int]$Pages,[int]$Tabs,[datetime]$Deadline)
    $nameJson = $Name | ConvertTo-Json -Compress
    $clicked = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const n=$nameJson,b=[...document.querySelectorAll('div>button>span')].filter(x=>x.textContent.trim()===n).map(x=>x.closest('button')).filter(Boolean);if(b.length===1)b[0].click();return b.length===1;"
    if ($clicked -isnot [bool] -or -not $clicked) { throw 'The exact structural source tab was unavailable or ambiguous.' }
    return Wait-StructuralDocumentState -SessionId $SessionId -ExpectedName $Name -ExpectedPages $Pages -ExpectedTabs $Tabs -Deadline $Deadline
}

function Wait-StructuralToolDialog {
    param([string]$SessionId,[ValidateSet('combine','insert','replace')][string]$Tool,[datetime]$Deadline)
    $contract = switch ($Tool) {
        'combine' { [pscustomobject]@{ launch='Combine files'; title='combine-title'; confirm='Combine PDFs' } }
        'insert' { [pscustomobject]@{ launch='Insert pages'; title='insert-pages-title'; confirm='Insert pages' } }
        'replace' { [pscustomobject]@{ launch='Replace pages'; title='replace-pages-title'; confirm='Replace pages' } }
    }
    if ($Tool -ne 'combine') {
        $organizer = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Organize pages'&&!x.disabled);if(b.length===1)b[0].click();return b.length===1;"
        if ($organizer -isnot [bool] -or -not $organizer) { throw 'The exact Organize pages control was unavailable.' }
        $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'organizer workspace' -Script "return document.querySelectorAll('section[aria-label=`"Organize pages workspace`"] h1').length===1;" -Predicate { param($v) $v -is [bool] -and $v }
    }
    $launchJson = $contract.launch | ConvertTo-Json -Compress
    $clicked = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const n=$launchJson,b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()===n&&!x.disabled);if(b.length===1)b[0].click();return b.length===1;"
    if ($clicked -isnot [bool] -or -not $clicked) { throw "The exact $($contract.launch) control was unavailable or ambiguous." }
    $titleJson = $contract.title | ConvertTo-Json -Compress
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind "$Tool dialog" -Script "const id=$titleJson,d=document.querySelector('dialog[aria-labelledby=\"'+id+'\"]');return !!d&&d.open;" -Predicate { param($v) $v -is [bool] -and $v }
    return $contract
}

function Set-StructuralDialogInput {
    param([string]$SessionId,[string]$Label,[string]$Value,[datetime]$Deadline)
    $labelJson = $Label | ConvertTo-Json -Compress; $valueJson = $Value | ConvertTo-Json -Compress
    $set = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const a=$labelJson,v=$valueJson,i=document.querySelector('input[aria-label=\"'+a+'\"]');if(!i||i.disabled)return false;Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(i,v);i.dispatchEvent(new Event('input',{bubbles:true}));return true;"
    if ($set -isnot [bool] -or -not $set) { throw 'An exact structural dialog input was unavailable.' }
}

function Invoke-StructuralNativeSave {
    param(
        [string]$SessionId,[int]$ApplicationProcessId,[long]$ApplicationStartTicks,[string]$Tool,[string]$ConfirmLabel,
        [string]$OutputPath,[string]$ExpectedName,[int]$ExpectedPages,[int]$ExpectedTabs,[string]$PdfiumPath,[string]$PdfiumProofPath,[string[]]$ExpectedPageFingerprintSha256,[datetime]$Deadline
    )
    if (Test-Path -LiteralPath $OutputPath) { throw 'A structural output path must be fresh.' }
    Assert-NoReparseAncestors -Path $OutputPath
    $baseline = @(Get-ProcessNativeWindowSnapshot -ProcessId $ApplicationProcessId -Deadline $Deadline -TopLevelOnly)
    if ($baseline.Count -lt 1) { throw 'The structural native Save baseline was empty.' }
    $confirmJson = $ConfirmLabel | ConvertTo-Json -Compress
    $clicked = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const n=$confirmJson,b=[...document.querySelectorAll('dialog[open] button')].filter(x=>x.textContent.trim()===n&&!x.disabled);if(b.length===1)b[0].click();return b.length===1;"
    if ($clicked -isnot [bool] -or -not $clicked) { throw "The exact $Tool submit control was unavailable or ambiguous." }
    $binding = Wait-NewProcessNativeWindowSurface -ProcessId $ApplicationProcessId -Baseline $baseline -AnchorNames @('Save') -AnchorControlTypes @('ControlType.Button') -Stage 'save-output-dialog' -Deadline $Deadline
    $roles = Get-ValidatedBindingNativeRoles -Binding $binding
    if ($null -eq $roles -or $null -eq $roles.PSObject.Properties['save'] -or $null -eq $roles.PSObject.Properties['filenameEdit']) { throw "The $Tool Save dialog did not expose the exact native HWND role contract." }
    $remaining = [int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline - [datetime]::UtcNow).TotalMilliseconds)))
    $actionDeadline = [Environment]::TickCount64 + $remaining
    Set-BoundNativeSaveFileNameExact -ProcessId $ApplicationProcessId -ProcessStartUtcTicks $ApplicationStartTicks -Binding $binding -Value $OutputPath -DeadlineTickCount $actionDeadline -AwaitSuggestion -SuggestedName ([IO.Path]::GetFileName($OutputPath)) -AcceptSuggestion
    Invoke-BoundNativeSaveButtonExact -ProcessId $ApplicationProcessId -ProcessStartUtcTicks $ApplicationStartTicks -Binding $binding -DeadlineTickCount $actionDeadline -ExpectedSuggestion ([IO.Path]::GetFileName($OutputPath))
    Wait-BoundProcessUiSurfaceClosed -ProcessId $ApplicationProcessId -Binding $binding -Stage 'save-output-dialog' -Deadline $Deadline -ProcessStartUtcTicks $ApplicationStartTicks -ExpectedNativeSaveFileName $OutputPath -ActionTransport 'native-bm-click-delivered'
    $state = Wait-StructuralDocumentState -SessionId $SessionId -ExpectedName $ExpectedName -ExpectedPages $ExpectedPages -ExpectedTabs $ExpectedTabs -Deadline $Deadline
    $receipt = Wait-StructuralStableFile -Path $OutputPath -Deadline $Deadline
    $proof = Get-StructuralPdfiumProof -PdfiumPath $PdfiumPath -PdfPath $OutputPath -ExpectedPages $ExpectedPages -ProofPath $PdfiumProofPath -Deadline $Deadline
    if ([int]$proof.Pages -ne $ExpectedPages) { throw "The $Tool output PDFium page count did not match its exact expected count." }
    Assert-StructuralFingerprintSequence -Expected $ExpectedPageFingerprintSha256 -Actual ([string[]]$proof.PageFingerprintSha256) -Kind $Tool
    Assert-StructuralFileReceipt -Path $OutputPath -Receipt $receipt -Kind "$Tool stable output"
    return [pscustomobject][ordered]@{ saveDialogVerified=$true;bytes=[uint64]$receipt.bytes;sha256=[string]$receipt.sha256;pages=[int]$proof.Pages;pageFingerprintSha256=[string[]]$proof.PageFingerprintSha256;pageFingerprintOrderVerified=$true;workspaceOpened=([string]$state.name -ceq $ExpectedName) }
}

function Invoke-RealInstalledStructuralPageTools {
    param(
        [string]$ApplicationPath,[string]$TauriDriverPath,[string]$EdgeDriverPath,[string]$ProfileRoot,[string]$SettingsRoot,
        [string]$FirstFixturePath,[string]$SecondFixturePath,[string]$CombineOutputPath,[string]$InsertOutputPath,[string]$ReplaceOutputPath,[string]$PdfiumPath,[string]$PdfiumProofRoot,
        [string[]]$FirstPageFingerprintSha256,[string[]]$SecondPageFingerprintSha256,[string[]]$CombinePageFingerprintSha256,[string[]]$InsertPageFingerprintSha256,[string[]]$ReplacePageFingerprintSha256,
        [string]$ExpectedEdgeDriverVersion,[string]$ExpectedRuntimeVersion
    )
    $null = Wait-FixedWebDriverPortsFree -Deadline ([datetime]::UtcNow.AddMilliseconds($script:StructuralPins.PortReleaseTimeoutMilliseconds))
    $launchDeadline = New-StructuralDeadline -Phase launch
    $capture = $null; $driver = $null; $sessionId = $null; $captured = @(); $result = $null
    $startedAfter = [datetime]::UtcNow; $sessionDeleteOutcome = 'requestfailed'; $driverExited = $false; $processesQuiescent = $false; $remaining = -1
    $driverStopOutcome = 'not-invoked'; $residualCategory = 'multiple'
    $capturedOutcomes = [pscustomobject]@{ application='absent';tauriDriver='absent';edgeDriver='absent';webview='absent';ocrEngine='absent';other='absent' }
    $residualFacts = [pscustomobject]@{ ownership='none';application=$false;tauriDriver=$false;edgeDriver=$false;webview=$false;ocrEngine=$false;other=$false }
    try {
        $capture = Start-BoundedDiscardProcess -Path $TauriDriverPath -Arguments @("--port=$($script:LaunchPins.WebDriverPort)","--native-port=$($script:LaunchPins.NativeDriverPort)","--native-driver=$EdgeDriverPath") -Environment (Get-MinimalWindowsProcessEnvironment)
        $capture.Start(); $driver = $capture.Process
        $status = $null
        $status = Wait-StructuralDriverReady -Driver $driver -Deadline $launchDeadline
        $nativeDriverVersion = Wait-NativeDriverStatus -ExpectedVersion $ExpectedEdgeDriverVersion -Deadline $launchDeadline -TauriDriver $driver
        $body = [ordered]@{ capabilities=[ordered]@{ alwaysMatch=[ordered]@{ browserName='wry';'tauri:options'=[ordered]@{ application=$ApplicationPath;args=@();webviewOptions=[ordered]@{userDataFolder=$ProfileRoot} } } } }
        $session = Invoke-BoundedLoopbackJson -Method POST -Path '/session' -Body $body -Deadline $launchDeadline
        if ([string]$session.value.sessionId -cnotmatch '^[A-Za-z0-9-]+$') { throw 'Structural page-tools WebDriver session identifier was invalid.' }
        $sessionId = [string]$session.value.sessionId
        $returnedRuntimeVersion = [string]$session.value.capabilities.browserVersion
        $runtimeParts = $returnedRuntimeVersion.Split('.'); $expectedParts = $ExpectedRuntimeVersion.Split('.')
        if ($returnedRuntimeVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or $runtimeParts.Count -ne 4 -or ($runtimeParts[0..2] -join '.') -cne ($expectedParts[0..2] -join '.')) { throw 'Structural page-tools WebView2 runtime capability disagrees with its trusted receipt.' }
        $returnedUserData = if ($null -ne $session.value.capabilities.PSObject.Properties['msedge.userDataDir']) { [string]$session.value.capabilities.'msedge.userDataDir' } else { '' }
        $null = Wait-WebDriverOracle -SessionId $sessionId -Deadline $launchDeadline -Kind 'structural home' -Script (Get-ReadingHomeScript) -Predicate { param($v) [bool]$v.ready -and [string]$v.title -ceq 'PDF Workstation' -and [int]$v.sample -eq 1 }
        do {
            $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
            $apps = @($captured | Where-Object { [string]$_.Path -and [IO.Path]::GetFullPath([string]$_.Path).Equals([IO.Path]::GetFullPath($ApplicationPath),[StringComparison]::OrdinalIgnoreCase) })
            if ($apps.Count -eq 1) { break }; if ($apps.Count -gt 1) { throw 'More than one owned installed application process was found.' }
            Start-Sleep -Milliseconds 100
        } while ([datetime]::UtcNow -lt $launchDeadline)
        if ($apps.Count -ne 1) { throw 'The owned installed application process was unavailable for structural page tools.' }
        $applicationProcessId = [int]$apps[0].ProcessId; $applicationStartTicks = [long]$apps[0].StartTicks
        Assert-ReadingOwnedExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -RootProcessId $driver.Id
        $profileBinding = if ($returnedUserData) { 'session-capability-' + (Get-ExactProfileBinding -Candidate $returnedUserData -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot) } else { Get-OwnedProfileBinding -Owned $captured -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot }
        Assert-ReadingProfileScope -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -Binding $profileBinding

        $pickerDeadline = New-StructuralDeadline -Phase picker
        $firstPicker = Open-ReadingUserFile -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationStartTicks -Path $FirstFixturePath -Deadline $pickerDeadline
        $null = Wait-StructuralDocumentState -SessionId $sessionId -ExpectedName ([IO.Path]::GetFileName($FirstFixturePath)) -ExpectedPages $script:StructuralPins.InputPages -ExpectedTabs 1 -Deadline (New-StructuralDeadline -Phase product)
        $pickerDeadline = New-StructuralDeadline -Phase picker
        $secondPicker = Open-ReadingUserFile -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationStartTicks -Path $SecondFixturePath -Deadline $pickerDeadline
        $null = Wait-StructuralDocumentState -SessionId $sessionId -ExpectedName ([IO.Path]::GetFileName($SecondFixturePath)) -ExpectedPages $script:StructuralPins.InputPages -ExpectedTabs 2 -Deadline (New-StructuralDeadline -Phase product)

        $firstName = [IO.Path]::GetFileName($FirstFixturePath); $secondName = [IO.Path]::GetFileName($SecondFixturePath)
        $null = Select-StructuralTab -SessionId $sessionId -Name $firstName -Pages 6 -Tabs 2 -Deadline (New-StructuralDeadline -Phase product)
        $combineDialog = Wait-StructuralToolDialog -SessionId $sessionId -Tool combine -Deadline (New-StructuralDeadline -Phase product)
        $combine = Invoke-StructuralNativeSave -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationStartTicks $applicationStartTicks -Tool 'Combine Files' -ConfirmLabel $combineDialog.confirm -OutputPath $CombineOutputPath -ExpectedName ([IO.Path]::GetFileName($CombineOutputPath)) -ExpectedPages 12 -ExpectedTabs 3 -PdfiumPath $PdfiumPath -PdfiumProofPath (Join-Path $PdfiumProofRoot 'combine.json') -ExpectedPageFingerprintSha256 $CombinePageFingerprintSha256 -Deadline (New-StructuralDeadline -Phase save)

        $null = Select-StructuralTab -SessionId $sessionId -Name $firstName -Pages 6 -Tabs 3 -Deadline (New-StructuralDeadline -Phase product)
        $insertDialog = Wait-StructuralToolDialog -SessionId $sessionId -Tool insert -Deadline (New-StructuralDeadline -Phase product)
        Set-StructuralDialogInput -SessionId $sessionId -Label 'Insertion boundary' -Value '2' -Deadline (New-StructuralDeadline -Phase product)
        $insert = Invoke-StructuralNativeSave -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationStartTicks $applicationStartTicks -Tool 'Insert Pages' -ConfirmLabel $insertDialog.confirm -OutputPath $InsertOutputPath -ExpectedName ([IO.Path]::GetFileName($InsertOutputPath)) -ExpectedPages 12 -ExpectedTabs 4 -PdfiumPath $PdfiumPath -PdfiumProofPath (Join-Path $PdfiumProofRoot 'insert.json') -ExpectedPageFingerprintSha256 $InsertPageFingerprintSha256 -Deadline (New-StructuralDeadline -Phase save)

        $null = Select-StructuralTab -SessionId $sessionId -Name $firstName -Pages 6 -Tabs 4 -Deadline (New-StructuralDeadline -Phase product)
        $replaceDialog = Wait-StructuralToolDialog -SessionId $sessionId -Tool replace -Deadline (New-StructuralDeadline -Phase product)
        Set-StructuralDialogInput -SessionId $sessionId -Label 'First target page' -Value '2' -Deadline (New-StructuralDeadline -Phase product)
        Set-StructuralDialogInput -SessionId $sessionId -Label 'Target pages to replace' -Value '2' -Deadline (New-StructuralDeadline -Phase product)
        $replace = Invoke-StructuralNativeSave -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationStartTicks $applicationStartTicks -Tool 'Replace Pages' -ConfirmLabel $replaceDialog.confirm -OutputPath $ReplaceOutputPath -ExpectedName ([IO.Path]::GetFileName($ReplaceOutputPath)) -ExpectedPages 10 -ExpectedTabs 5 -PdfiumPath $PdfiumPath -PdfiumProofPath (Join-Path $PdfiumProofRoot 'replace.json') -ExpectedPageFingerprintSha256 $ReplacePageFingerprintSha256 -Deadline (New-StructuralDeadline -Phase save)

        $null = Select-StructuralTab -SessionId $sessionId -Name $firstName -Pages 6 -Tabs 5 -Deadline (New-StructuralDeadline -Phase product)
        $null = Select-StructuralTab -SessionId $sessionId -Name $secondName -Pages 6 -Tabs 5 -Deadline (New-StructuralDeadline -Phase product)
        $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
        Assert-ReadingOwnedExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -RootProcessId $driver.Id
        Assert-ReadingProfileScope -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -Binding $profileBinding
        if ($capture.Exceeded) { throw 'Structural page-tools WebDriver diagnostic output exceeded its discarded cap.' }
        $result = [pscustomobject][ordered]@{
            nativeDriverVersion=$nativeDriverVersion;returnedRuntimeVersion=$returnedRuntimeVersion;profileBinding=$profileBinding
            processBoundOpenPickerVerified=$true;openFilenameControlCategories=@([string]$firstPicker,[string]$secondPicker)
            firstSourcePageFingerprintSha256=[string[]]$FirstPageFingerprintSha256;secondSourcePageFingerprintSha256=[string[]]$SecondPageFingerprintSha256
            combine=$combine;insert=$insert;replace=$replace;sourceTabsPreserved=$true
        }
    } finally {
        $cleanupDeadline = [datetime]::UtcNow.AddSeconds(10)
        if ($sessionId) { $sessionDeleteOutcome = Invoke-SessionDeleteOutcome -SessionId $sessionId -Deadline $cleanupDeadline }
        if ($driver) {
            try { $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured) } catch { }
            $processDeadline = [datetime]::UtcNow.AddMilliseconds($script:LaunchPins.CleanupProcessTimeoutMilliseconds)
            $capturedOutcomes = Stop-OwnedLaunchProcesses -TauriDriver $driver -Captured $captured -Deadline $processDeadline
            $driverStopOutcome = [string]$capturedOutcomes.rootOutcome
            $quiescence = Wait-LaunchProcessQuiescence -Deadline $processDeadline; $processesQuiescent = [bool]$quiescence.stable
            $remainingProcesses = @($quiescence.processes); $remaining = $remainingProcesses.Count; $residualCategory = Get-LaunchResidualCategory -Processes $remainingProcesses
            $residualFacts = Get-LaunchResidualFacts -Processes $remainingProcesses -Captured $captured; $driverExited = [bool]$driver.HasExited
        }
        if ($capture) { $capture.Dispose() }
    }
    Assert-LaunchCleanupState -Result $result -SessionDeleteOutcome $sessionDeleteOutcome -DriverExited $driverExited -RelevantProcessesClear ($processesQuiescent -and $residualCategory -ceq 'none') -DriverStopOutcome $driverStopOutcome -ResidualCategory $residualCategory -CapturedApplication $capturedOutcomes.application -CapturedTauriDriver $capturedOutcomes.tauriDriver -CapturedEdgeDriver $capturedOutcomes.edgeDriver -CapturedWebView $capturedOutcomes.webview -CapturedOcrEngine $capturedOutcomes.ocrEngine -CapturedOther $capturedOutcomes.other -ResidualOwnership $residualFacts.ownership -ResidualApplication $residualFacts.application -ResidualTauriDriver $residualFacts.tauriDriver -ResidualEdgeDriver $residualFacts.edgeDriver -ResidualWebView $residualFacts.webview -ResidualOcrEngine $residualFacts.ocrEngine -ResidualOther $residualFacts.other
    if ($null -eq $result -or $sessionDeleteOutcome -cne 'verified' -or -not $driverExited -or -not $processesQuiescent -or $remaining -ne 0) { throw 'Installed structural page-tools cleanup did not reach its exact zero-process state.' }
    $result | Add-Member -NotePropertyName sessionDeleted -NotePropertyValue $true
    $result | Add-Member -NotePropertyName ownedProcessTreeStopped -NotePropertyValue $true
    $result | Add-Member -NotePropertyName relevantProcessesRemaining -NotePropertyValue 0
    return $result
}

function Assert-StructuralResult {
    param([Parameter(Mandatory = $true)]$Result)
    Assert-StructuralExactProperties -Value $Result -Expected @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','processBoundOpenPickerVerified','openFilenameControlCategories','firstSourcePageFingerprintSha256','secondSourcePageFingerprintSha256','combine','insert','replace','sourceTabsPreserved','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') -Kind 'Installed structural page-tools result'
    if ([string]$Result.nativeDriverVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or [string]$Result.returnedRuntimeVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or
        [string]$Result.profileBinding -cnotmatch '^(session-capability|owned-webview)-(requested-profile|requested-ebwebview)$' -or
        -not [bool]$Result.processBoundOpenPickerVerified -or @($Result.openFilenameControlCategories).Count -ne 2 -or
        @($Result.openFilenameControlCategories | Where-Object { [string]$_ -notin @('edit-1001','edit-1148') }).Count -ne 0 -or
        -not [bool]$Result.sourceTabsPreserved -or -not [bool]$Result.sessionDeleted -or -not [bool]$Result.ownedProcessTreeStopped -or [int]$Result.relevantProcessesRemaining -ne 0) {
        throw 'Installed structural page-tools result did not prove its exact runtime, picker, source, and cleanup contract.'
    }
    $first = [string[]]@($Result.firstSourcePageFingerprintSha256); $second = [string[]]@($Result.secondSourcePageFingerprintSha256); $allSourceFingerprints = [string[]]@($first + $second)
    if ($first.Count -ne 6 -or $second.Count -ne 6 -or @($allSourceFingerprints | Where-Object { [string]$_ -cnotmatch '^[A-F0-9]{64}$' }).Count -ne 0 -or @($allSourceFingerprints | Sort-Object -Unique).Count -ne 12) { throw 'Structural source page fingerprints were not twelve distinct exact hashes.' }
    $expected = [ordered]@{combine=[string[]]@($first + $second);insert=[string[]]@($first[0..1] + $second + $first[2..5]);replace=[string[]]@(@($first[0]) + $second + $first[3..5])}
    foreach ($entry in @(@('combine',$script:StructuralPins.CombinePages),@('insert',$script:StructuralPins.InsertPages),@('replace',$script:StructuralPins.ReplacePages))) {
        $value = $Result.PSObject.Properties[[string]$entry[0]].Value
        Assert-StructuralExactProperties -Value $value -Expected @('saveDialogVerified','bytes','sha256','pages','pageFingerprintSha256','pageFingerprintOrderVerified','workspaceOpened') -Kind ([string]$entry[0] + ' output')
        if (-not [bool]$value.saveDialogVerified -or [uint64]$value.bytes -eq 0 -or [uint64]$value.bytes -gt [uint64]$script:StructuralPins.OutputBytesMaximum -or [string]$value.sha256 -cnotmatch '^[A-F0-9]{64}$' -or [int]$value.pages -ne [int]$entry[1] -or -not [bool]$value.pageFingerprintOrderVerified -or -not [bool]$value.workspaceOpened) { throw 'A structural page-tool output receipt was incomplete.' }
        Assert-StructuralFingerprintSequence -Expected ([string[]]$expected[[string]$entry[0]]) -Actual ([string[]]$value.pageFingerprintSha256) -Kind ([string]$entry[0])
    }
}

function Invoke-InstalledStructuralPageTools {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,[Parameter(Mandatory = $true)]$ApplicationReceipt,
        [Parameter(Mandatory = $true)][string]$PdfiumPath,[Parameter(Mandatory = $true)]$PdfiumReceipt,
        [Parameter(Mandatory = $true)][string]$WebDriverRoot,[Parameter(Mandatory = $true)][string]$ProfileRoot,[Parameter(Mandatory = $true)][string]$SettingsRoot,
        [Parameter(Mandatory = $true)][string]$FirstFixturePath,[Parameter(Mandatory = $true)]$FirstFixtureReceipt,[Parameter(Mandatory = $true)][string]$SecondFixturePath,[Parameter(Mandatory = $true)]$SecondFixtureReceipt,
        [Parameter(Mandatory = $true)][string]$CombineOutputPath,[Parameter(Mandatory = $true)][string]$InsertOutputPath,[Parameter(Mandatory = $true)][string]$ReplaceOutputPath,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher,[scriptblock]$ProcessProvider
    )
    $receipt = Get-Content -LiteralPath (Join-Path $WebDriverRoot 'webdriver-receipt.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-WebDriverReceipt -Receipt $receipt
    $tauri = Join-Path $WebDriverRoot 'tauri-driver-install/bin/tauri-driver.exe'; $edge = Join-Path $WebDriverRoot 'edge-driver/msedgedriver.exe'
    Assert-StructuralFileReceipt -Path $ApplicationPath -Receipt $ApplicationReceipt -Kind 'Installed signed structural page-tools application'
    $null = Assert-TrustedWindowsSignature -Path $ApplicationPath -ExpectedPublisher $ExpectedPublisher
    Assert-StructuralPdfiumReceipt -Path $PdfiumPath -Receipt $PdfiumReceipt -Kind 'Installed signed structural page-tools PDFium'
    $null = Assert-TrustedWindowsSignature -Path $PdfiumPath -ExpectedPublisher $ExpectedPublisher
    Assert-StructuralFileReceipt -Path $tauri -Receipt ([pscustomobject][ordered]@{ bytes = [uint64]$receipt.tauriDriver.bytes; sha256 = [string]$receipt.tauriDriver.sha256 }) -Kind 'Pinned structural page-tools tauri-driver'
    Assert-StructuralFileReceipt -Path $edge -Receipt ([pscustomobject][ordered]@{ bytes = [uint64]$receipt.edgeDriver.bytes; sha256 = [string]$receipt.edgeDriver.sha256 }) -Kind 'Pinned structural page-tools EdgeDriver'
    $null = Assert-TrustedWindowsSignature -Path $edge -ExpectedPublisher $script:LaunchPins.EdgePublisher
    Assert-StructuralFileReceipt -Path $FirstFixturePath -Receipt $FirstFixtureReceipt -Kind 'First structural fixture'
    Assert-StructuralFileReceipt -Path $SecondFixturePath -Receipt $SecondFixtureReceipt -Kind 'Second structural fixture'
    if ([IO.Path]::GetFullPath($FirstFixturePath).Equals([IO.Path]::GetFullPath($SecondFixturePath),[StringComparison]::OrdinalIgnoreCase) -or [string]$FirstFixtureReceipt.sha256 -ceq [string]$SecondFixtureReceipt.sha256) { throw 'Structural fixtures must have different exact paths and receipts.' }
    $proofRoot = [IO.Path]::GetFullPath($ProfileRoot + '-pdfium-proofs')
    Assert-NoReparseAncestors -Path $proofRoot
    if (Test-Path -LiteralPath $proofRoot) { throw 'Structural PDFium proof root must be fresh.' }
    [IO.Directory]::CreateDirectory($proofRoot) | Out-Null
    $firstProof = Get-StructuralPdfiumProof -PdfiumPath $PdfiumPath -PdfPath $FirstFixturePath -ExpectedPages $script:StructuralPins.InputPages -ProofPath (Join-Path $proofRoot 'source-first.json') -Deadline ([datetime]::UtcNow.AddMilliseconds($script:StructuralPins.PdfiumProofTimeoutMilliseconds))
    $secondProof = Get-StructuralPdfiumProof -PdfiumPath $PdfiumPath -PdfPath $SecondFixturePath -ExpectedPages $script:StructuralPins.InputPages -ProofPath (Join-Path $proofRoot 'source-second.json') -Deadline ([datetime]::UtcNow.AddMilliseconds($script:StructuralPins.PdfiumProofTimeoutMilliseconds))
    if ([int]$firstProof.Pages -ne $script:StructuralPins.InputPages -or [int]$secondProof.Pages -ne $script:StructuralPins.InputPages) { throw 'Structural source fixtures did not contain exactly six PDFium-measured pages.' }
    foreach ($proof in @($firstProof,$secondProof)) {
        if (@($proof.WidthPoints | Where-Object { [Math]::Abs([double]$_ - 612.0) -gt 0.01 }).Count -ne 0 -or @($proof.HeightPoints | Where-Object { [Math]::Abs([double]$_ - 792.0) -gt 0.01 }).Count -ne 0) { throw 'Structural source fixture page dimensions were not exact Letter portrait pages.' }
    }
    $firstFingerprints = [string[]]$firstProof.PageFingerprintSha256; $secondFingerprints = [string[]]$secondProof.PageFingerprintSha256
    $allSourceFingerprints = [string[]]@($firstFingerprints + $secondFingerprints)
    if (@($allSourceFingerprints | Sort-Object -Unique).Count -ne 12) { throw 'Structural source fixtures did not contain twelve visually distinct pages.' }
    $combineFingerprints = [string[]]@($firstFingerprints + $secondFingerprints)
    $insertFingerprints = [string[]]@($firstFingerprints[0..1] + $secondFingerprints + $firstFingerprints[2..5])
    $replaceFingerprints = [string[]]@(@($firstFingerprints[0]) + $secondFingerprints + $firstFingerprints[3..5])
    foreach ($path in @($CombineOutputPath,$InsertOutputPath,$ReplaceOutputPath)) { if (Test-Path -LiteralPath $path) { throw 'Structural output paths must be fresh.' }; Assert-NoReparseAncestors -Path $path }
    if ([IO.Path]::GetFileName($CombineOutputPath) -cne 'combined.pdf' -or [IO.Path]::GetFileName($InsertOutputPath) -cne 'inserted-pages.pdf' -or [IO.Path]::GetFileName($ReplaceOutputPath) -cne 'replaced-pages.pdf') { throw 'Structural outputs must use the application''s suggested names.' }
    $distinctOutputs = @(@($CombineOutputPath,$InsertOutputPath,$ReplaceOutputPath) | ForEach-Object { [IO.Path]::GetFullPath($_).ToLowerInvariant() } | Sort-Object -Unique)
    if ($distinctOutputs.Count -ne 3) { throw 'Structural output paths must be distinct.' }
    if (Test-Path -LiteralPath $ProfileRoot) { throw 'Structural page-tools WebView profile must be fresh.' }
    if (-not (Test-Path -LiteralPath $SettingsRoot -PathType Container)) { throw 'The controlled application settings root is missing.' }
    Assert-NoReparseAncestors -Path $SettingsRoot; [IO.Directory]::CreateDirectory($ProfileRoot) | Out-Null
    $settingsEntries = @(Get-ChildItem -LiteralPath $SettingsRoot -Force); $profileEntries = @(Get-ChildItem -LiteralPath $ProfileRoot -Force)
    if ($settingsEntries.Count -ne 1 -or $settingsEntries[0].PSIsContainer -or $settingsEntries[0].Name -cne 'upgrade-sentinel.json' -or ($settingsEntries[0].Attributes -band [IO.FileAttributes]::ReparsePoint) -or $profileEntries.Count -ne 0) { throw 'Structural page-tools profile roots were not fresh, exclusive, and sentinel-only before launch.' }
    $result = if ($ProcessProvider) {
        & $ProcessProvider $ApplicationPath $tauri $edge $ProfileRoot $SettingsRoot $FirstFixturePath $SecondFixturePath $CombineOutputPath $InsertOutputPath $ReplaceOutputPath $PdfiumPath $proofRoot $firstFingerprints $secondFingerprints $combineFingerprints $insertFingerprints $replaceFingerprints ([string]$receipt.edgeDriver.version) ([string]$receipt.webView2RuntimeVersion)
    } else {
        Invoke-RealInstalledStructuralPageTools -ApplicationPath $ApplicationPath -TauriDriverPath $tauri -EdgeDriverPath $edge -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -FirstFixturePath $FirstFixturePath -SecondFixturePath $SecondFixturePath -CombineOutputPath $CombineOutputPath -InsertOutputPath $InsertOutputPath -ReplaceOutputPath $ReplaceOutputPath -PdfiumPath $PdfiumPath -PdfiumProofRoot $proofRoot -FirstPageFingerprintSha256 $firstFingerprints -SecondPageFingerprintSha256 $secondFingerprints -CombinePageFingerprintSha256 $combineFingerprints -InsertPageFingerprintSha256 $insertFingerprints -ReplacePageFingerprintSha256 $replaceFingerprints -ExpectedEdgeDriverVersion ([string]$receipt.edgeDriver.version) -ExpectedRuntimeVersion ([string]$receipt.webView2RuntimeVersion)
    }
    Assert-StructuralResult -Result $result
    $proofFiles = @(Get-ChildItem -LiteralPath $proofRoot -File -Force)
    if ($proofFiles.Count -ne 5 -or @($proofFiles | Where-Object { ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $_.Length -le 0 -or $_.Length -gt $script:StructuralPins.PdfiumProofRecordBytesMaximum }).Count -ne 0) { throw 'Structural PDFium proof root did not contain exactly five bounded regular proof records.' }
    Assert-StructuralFileReceipt -Path $FirstFixturePath -Receipt $FirstFixtureReceipt -Kind 'Final first structural fixture'
    Assert-StructuralFileReceipt -Path $SecondFixturePath -Receipt $SecondFixtureReceipt -Kind 'Final second structural fixture'
    Assert-StructuralPdfiumReceipt -Path $PdfiumPath -Receipt $PdfiumReceipt -Kind 'Final installed signed structural page-tools PDFium'
    return $result
}
