[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'installed-reading-tools.ps1')

$script:OrganizerPins = [ordered]@{
    LaunchTimeoutMilliseconds = 180000
    NativePickerTimeoutMilliseconds = 180000
    ProductTimeoutMilliseconds = 180000
    PostActionTimeoutMilliseconds = 30000
    PortReleaseTimeoutMilliseconds = 180000
    UiPollMilliseconds = 100
    PageCount = 6
    SourceWidth = 612
    SourceHeight = 792
    CropInset = 36
    CroppedWidth = 540
    CroppedHeight = 720
    PagesPerSplitFile = 2
    SplitFileMaximumBytes = 256MB
    SplitTotalMaximumBytes = 768MB
    PdfRenderSize = 512
    SplitFolderName = 'Split PDFs'
    SplitFileNames = @('pages-0001-0002.pdf','pages-0003-0004.pdf','pages-0005-0006.pdf')
}

function Wait-OrganizerDriverReady {
    param([Parameter(Mandatory = $true)]$Driver,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$StatusProvider,[scriptblock]$SleepProvider,[scriptblock]$UtcNowProvider)
    $status = $null
    while ($true) {
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        try {
            $status = if ($StatusProvider) { & $StatusProvider } else { Invoke-BoundedLoopbackJson -Method GET -Path '/status' -Deadline $Deadline }
            if ($null -ne $status -and $null -ne $status.value -and [bool]$status.value.ready) { return $status }
        } catch { $status = $null }
        if ([bool]$Driver.HasExited) { throw 'Pinned tauri-driver exited before organizer readiness.' }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
        if ($now -ge $Deadline) { break }
        $remaining = [int][Math]::Ceiling(($Deadline - $now).TotalMilliseconds)
        if ($remaining -le 0) { break }
        $delay = [Math]::Min(200,$remaining)
        if ($SleepProvider) { & $SleepProvider $delay } else { Start-Sleep -Milliseconds $delay }
    }
    throw 'Pinned tauri-driver did not become ready for organizer verification before its bounded deadline.'
}

function Initialize-OrganizerPdfiumProof {
    if ('OrganizerPdfiumProof' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

public sealed class OrganizerPdfiumProof {
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr LoadLibraryExW(string path, IntPtr file, uint flags);
  [DllImport("kernel32.dll", CharSet=CharSet.Ansi, SetLastError=true)] static extern IntPtr GetProcAddress(IntPtr module, string name);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void Init();
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr LoadMem(IntPtr data, ulong length, IntPtr password);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void CloseDocument(IntPtr document);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int PageCount(IntPtr document);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr LoadPage(IntPtr document, int index);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void ClosePage(IntPtr page);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate double PageMetric(IntPtr page);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr BitmapCreate(int width, int height, int alpha);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void BitmapDestroy(IntPtr bitmap);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void BitmapFill(IntPtr bitmap, int left, int top, int width, int height, uint color);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void Render(IntPtr bitmap, IntPtr page, int left, int top, int width, int height, int rotate, int flags);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr BitmapBuffer(IntPtr bitmap);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int BitmapInt(IntPtr bitmap);

  readonly IntPtr module;
  readonly LoadMem load;
  readonly CloseDocument closeDocument;
  readonly PageCount pageCount;
  readonly LoadPage loadPage;
  readonly ClosePage closePage;
  readonly PageMetric pageWidth;
  readonly PageMetric pageHeight;
  readonly BitmapCreate bitmapCreate;
  readonly BitmapDestroy bitmapDestroy;
  readonly BitmapFill bitmapFill;
  readonly Render render;
  readonly BitmapBuffer bitmapBuffer;
  readonly BitmapInt bitmapStride;

  T Get<T>(string name) where T : Delegate {
    var address=GetProcAddress(module,name); if(address==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium is missing an organizer proof export.");
    return Marshal.GetDelegateForFunctionPointer<T>(address);
  }

  public OrganizerPdfiumProof(string path) {
    module=LoadLibraryExW(Path.GetFullPath(path),IntPtr.Zero,0x00000100|0x00001000); if(module==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not be loaded for organizer proof.");
    var init=Get<Init>("FPDF_InitLibrary"); load=Get<LoadMem>("FPDF_LoadMemDocument64"); closeDocument=Get<CloseDocument>("FPDF_CloseDocument"); pageCount=Get<PageCount>("FPDF_GetPageCount"); loadPage=Get<LoadPage>("FPDF_LoadPage"); closePage=Get<ClosePage>("FPDF_ClosePage");
    pageWidth=Get<PageMetric>("FPDF_GetPageWidth");pageHeight=Get<PageMetric>("FPDF_GetPageHeight");bitmapCreate=Get<BitmapCreate>("FPDFBitmap_Create");bitmapDestroy=Get<BitmapDestroy>("FPDFBitmap_Destroy");bitmapFill=Get<BitmapFill>("FPDFBitmap_FillRect");render=Get<Render>("FPDF_RenderPageBitmap");bitmapBuffer=Get<BitmapBuffer>("FPDFBitmap_GetBuffer");bitmapStride=Get<BitmapInt>("FPDFBitmap_GetStride");init();
  }

  public sealed class PageProof { public double WidthPoints {get;set;} public double HeightPoints {get;set;} public int InkPixels {get;set;} public string FingerprintSha256 {get;set;} }
  public sealed class DocumentProof { public int Pages {get;set;} public PageProof[] PageProofs {get;set;} }

  public static string Fingerprint(byte[] raw,int stride,int renderSize) {
    if(raw==null||renderSize<1||stride<renderSize*4||raw.Length<stride*renderSize)throw new InvalidOperationException("Organizer proof raster is invalid.");
    var fullPageBgra=new byte[checked(renderSize*renderSize*4)];
    for(int y=0;y<renderSize;y++)Buffer.BlockCopy(raw,y*stride,fullPageBgra,y*renderSize*4,renderSize*4);
    return Convert.ToHexString(SHA256.HashData(fullPageBgra));
  }

  public DocumentProof Inspect(string path,int renderSize,long maximumBytes,int maximumPages) {
    var bytes=File.ReadAllBytes(path);if(bytes.Length<=0||bytes.LongLength>maximumBytes)throw new InvalidOperationException("Organizer PDF proof input is empty or oversized.");
    var pin=GCHandle.Alloc(bytes,GCHandleType.Pinned);IntPtr document=IntPtr.Zero;
    try {
      document=load(pin.AddrOfPinnedObject(),(ulong)bytes.LongLength,IntPtr.Zero);if(document==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not parse an organizer PDF proof input.");
      int pages=pageCount(document);if(pages<=0||pages>maximumPages)throw new InvalidOperationException("Organizer PDF proof page count is invalid.");
      var proofs=new List<PageProof>(pages);
      for(int pageIndex=0;pageIndex<pages;pageIndex++) {
        IntPtr page=IntPtr.Zero,bitmap=IntPtr.Zero;
        try {
          page=loadPage(document,pageIndex);if(page==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not load an organizer proof page.");
          double width=pageWidth(page),height=pageHeight(page);if(!(width>0&&height>0&&width<20000&&height<20000))throw new InvalidOperationException("Organizer proof page dimensions are invalid.");
          bitmap=bitmapCreate(renderSize,renderSize,1);if(bitmap==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not allocate an organizer proof bitmap.");
          bitmapFill(bitmap,0,0,renderSize,renderSize,0xFFFFFFFF);render(bitmap,page,0,0,renderSize,renderSize,0,0x801);
          int stride=bitmapStride(bitmap);if(stride<renderSize*4||stride>renderSize*8)throw new InvalidOperationException("Organizer proof bitmap stride is invalid.");
          var raw=new byte[stride*renderSize];Marshal.Copy(bitmapBuffer(bitmap),raw,0,raw.Length);int ink=0;
          for(int y=0;y<renderSize;y++)for(int x=0;x<renderSize;x++){int i=y*stride+x*4;byte g=(byte)((raw[i]*29+raw[i+1]*150+raw[i+2]*77)>>8);if(g<245)ink++;}
          if(ink<64)throw new InvalidOperationException("Organizer proof page has insufficient rendered content.");
          proofs.Add(new PageProof{WidthPoints=width,HeightPoints=height,InkPixels=ink,FingerprintSha256=Fingerprint(raw,stride,renderSize)});
        } finally {if(bitmap!=IntPtr.Zero)bitmapDestroy(bitmap);if(page!=IntPtr.Zero)closePage(page);}
      }
      return new DocumentProof{Pages=pages,PageProofs=proofs.ToArray()};
    } finally {if(document!=IntPtr.Zero)closeDocument(document);if(pin.IsAllocated)pin.Free();}
  }
}
'@
}

function Get-OrganizerPdfiumProof {
    param([Parameter(Mandatory = $true)][string]$PdfiumPath,[Parameter(Mandatory = $true)][string]$PdfPath,[Parameter(Mandatory = $true)][ValidateRange(1,64)][int]$MaximumPages)
    Initialize-OrganizerPdfiumProof
    $engine = [OrganizerPdfiumProof]::new($PdfiumPath)
    return $engine.Inspect($PdfPath,[int]$script:OrganizerPins.PdfRenderSize,[long]$script:OrganizerPins.SplitFileMaximumBytes,$MaximumPages)
}

function New-OrganizerPhaseDeadline {
    param([Parameter(Mandatory = $true)][ValidateSet('launch','native-picker','product')][string]$Phase,[scriptblock]$UtcNowProvider)
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    $timeout = switch ($Phase) {
        'launch' { [int]$script:OrganizerPins.LaunchTimeoutMilliseconds }
        'native-picker' { [int]$script:OrganizerPins.NativePickerTimeoutMilliseconds }
        'product' { [int]$script:OrganizerPins.ProductTimeoutMilliseconds }
    }
    if ($timeout -lt 1 -or $timeout -gt 180000) { throw 'An organizer phase timeout was outside its exact cap.' }
    return $now.AddMilliseconds($timeout)
}

function Assert-OrganizerPhaseTransition {
    param([Parameter(Mandatory = $true)][datetime]$Deadline,[Parameter(Mandatory = $true)][ValidateSet('launch','native-picker','product')][string]$Phase,[scriptblock]$UtcNowProvider)
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    if ($now -ge $Deadline) { throw "The organizer $Phase phase expired before its next phase." }
}

function Assert-OrganizerExactProperties {
    param([Parameter(Mandatory = $true)]$Value,[Parameter(Mandatory = $true)][string[]]$Names,[Parameter(Mandatory = $true)][string]$Kind)
    if ($null -eq $Value -or (($Value.PSObject.Properties.Name | Sort-Object -CaseSensitive) -join ',') -cne (($Names | Sort-Object -CaseSensitive) -join ',')) {
        throw "$Kind had an invalid schema."
    }
}

function Get-OrganizerViewerStateScript {
    param([Parameter(Mandatory = $true)][string]$ExpectedName)
    if ($ExpectedName -cnotmatch '^[A-Za-z0-9._-]{1,120}\.pdf$') { throw 'The organizer fixture filename was outside its exact contract.' }
    $expected = $ExpectedName | ConvertTo-Json -Compress
    return @"
const expected=$expected,tabs=[...document.querySelectorAll('div')].filter(x=>x.querySelector(':scope>button>span')&&x.querySelector(':scope>button[aria-label^="Close "]')),active=tabs.filter(x=>typeof x.className==='string'&&x.className.includes('selectedTab')),name=active.length===1?(active[0].querySelector(':scope>button>span')?.textContent||'').trim():'';return {schemaVersion:1,documentTabCount:tabs.length,activeTabCount:active.length,activeFilenameMatches:name===expected,pageInputCount:document.querySelectorAll('input[aria-label="Page number"]').length,pageCount:Number.parseInt(document.querySelector('input[aria-label="Page number"]')?.max||'0',10)||0,passwordDialogCount:document.querySelectorAll('dialog[aria-labelledby="password-title"]').length,alertCount:document.querySelectorAll('[role="alert"]').length};
"@
}

function Wait-OrganizerViewerState {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][string]$ExpectedName,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$StateProvider,[scriptblock]$SleepProvider,[scriptblock]$UtcNowProvider)
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    $limit = $now.AddMilliseconds([int]$script:OrganizerPins.PostActionTimeoutMilliseconds); if ($limit -gt $Deadline) { $limit = $Deadline }
    while ($now -lt $limit) {
        $state = if ($StateProvider) { & $StateProvider } else { Invoke-WebDriverScript -SessionId $SessionId -Deadline $limit -Script (Get-OrganizerViewerStateScript -ExpectedName $ExpectedName) }
        Assert-OrganizerExactProperties -Value $state -Names @('schemaVersion','documentTabCount','activeTabCount','activeFilenameMatches','pageInputCount','pageCount','passwordDialogCount','alertCount') -Kind 'The organizer viewer state'
        foreach ($name in @('schemaVersion','documentTabCount','activeTabCount','pageInputCount','pageCount','passwordDialogCount','alertCount')) {
            $value = $state.PSObject.Properties[$name].Value
            if (($value -isnot [int] -and $value -isnot [long]) -or [long]$value -lt 0 -or [long]$value -gt 4096) { throw 'The organizer viewer state exceeded its count contract.' }
        }
        if ($state.activeFilenameMatches -isnot [bool]) { throw 'The organizer viewer filename result was not Boolean.' }
        if ([int]$state.documentTabCount -gt 1 -or [int]$state.activeTabCount -gt 1 -or [int]$state.alertCount -gt 0 -or [int]$state.passwordDialogCount -gt 0 -or ([int]$state.pageCount -gt 0 -and [int]$state.pageCount -ne [int]$script:OrganizerPins.PageCount)) { throw 'The organizer fixture opened into an unexpected product state.' }
        if ([int]$state.documentTabCount -eq 1 -and [int]$state.activeTabCount -eq 1 -and [bool]$state.activeFilenameMatches -and [int]$state.pageInputCount -eq 1 -and [int]$state.pageCount -eq [int]$script:OrganizerPins.PageCount) { return $state }
        if ($SleepProvider) { & $SleepProvider $script:OrganizerPins.UiPollMilliseconds } else { Start-Sleep -Milliseconds $script:OrganizerPins.UiPollMilliseconds }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    }
    throw 'The exact organizer fixture did not reach its bounded viewer state.'
}

function Invoke-OrganizerMenuAction {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][ValidateSet('menu','organizer')][string]$Action,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $script = if ($Action -ceq 'menu') {
        "const b=[...document.querySelectorAll('button[aria-expanded]')].filter(x=>x.textContent.trim()==='Menu'&&!x.disabled);if(b.length===1)b[0].click();return {count:b.length,clicked:b.length===1};"
    } else {
        "const b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Organize pages'&&!x.disabled&&x.closest('[class*=menuPopover]'));if(b.length===1)b[0].click();return {count:b.length,clicked:b.length===1};"
    }
    $receipt = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script $script
    Assert-OrganizerExactProperties -Value $receipt -Names @('count','clicked') -Kind "The organizer $Action action receipt"
    if (($receipt.count -isnot [int] -and $receipt.count -isnot [long]) -or [int]$receipt.count -ne 1 -or $receipt.clicked -isnot [bool] -or -not [bool]$receipt.clicked) { throw "The exact organizer $Action control was unavailable." }
}

function Get-OrganizerWorkspaceStateScript {
    return @'
const roots=[...document.querySelectorAll('section[aria-label="Organize pages workspace"]')],root=roots.length===1?roots[0]:null,cards=root?[...root.querySelectorAll('button[aria-label^="Select page "]')]:[],first=root?.querySelector('button[aria-label="Select page 1"]'),ratio=(first?.querySelector('div[style*="aspect-ratio"]')?.style.aspectRatio||'').split('/').map(x=>Number(x.trim())),buttons=root?[...root.querySelectorAll('button')]:[],tabs=[...document.querySelectorAll('div')].filter(x=>x.querySelector(':scope>button>span')&&x.querySelector(':scope>button[aria-label^="Close "]')),active=tabs.filter(x=>typeof x.className==='string'&&x.className.includes('selectedTab')),tabText=active.length===1?(active[0].querySelector(':scope>button>span')?.textContent||''):'';return {schemaVersion:1,workspaceCount:roots.length,headingCount:root?[...root.querySelectorAll('h1')].filter(x=>x.textContent.trim()==='Organize pages').length:0,pageCount:cards.length,selectedCount:cards.filter(x=>x.getAttribute('aria-pressed')==='true').length,selectedPageOne:first?.getAttribute('aria-pressed')==='true',firstWidth:Number.isFinite(ratio[0])?ratio[0]:0,firstHeight:Number.isFinite(ratio[1])?ratio[1]:0,dirtyIndicator:tabText.endsWith(' *'),unsavedIndicator:root?[...root.querySelectorAll('span')].some(x=>x.textContent.includes('Unsaved changes')):false,cropReady:buttons.filter(x=>x.textContent.trim()==='Crop'&&!x.disabled).length===1,resetReady:buttons.filter(x=>x.getAttribute('aria-label')==='Reset crop on selected pages'&&!x.disabled).length===1,splitReady:buttons.filter(x=>x.textContent.trim()==='Split'&&!x.disabled).length===1,dialogCount:root?.querySelectorAll('dialog').length||0,alertCount:document.querySelectorAll('[role="alert"]').length};
'@
}

function Wait-OrganizerWorkspaceState {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][int]$ExpectedWidth,[Parameter(Mandatory = $true)][int]$ExpectedHeight,[Parameter(Mandatory = $true)][bool]$ExpectedDirty,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$StateProvider,[scriptblock]$SleepProvider,[scriptblock]$UtcNowProvider)
    $names = @('schemaVersion','workspaceCount','headingCount','pageCount','selectedCount','selectedPageOne','firstWidth','firstHeight','dirtyIndicator','unsavedIndicator','cropReady','resetReady','splitReady','dialogCount','alertCount')
    $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    $limit = $now.AddMilliseconds([int]$script:OrganizerPins.PostActionTimeoutMilliseconds); if ($limit -gt $Deadline) { $limit = $Deadline }
    while ($now -lt $limit) {
        $state = if ($StateProvider) { & $StateProvider } else { Invoke-WebDriverScript -SessionId $SessionId -Deadline $limit -Script (Get-OrganizerWorkspaceStateScript) }
        Assert-OrganizerExactProperties -Value $state -Names $names -Kind 'The organizer workspace state'
        foreach ($name in @('schemaVersion','workspaceCount','headingCount','pageCount','selectedCount','firstWidth','firstHeight','dialogCount','alertCount')) {
            $value = $state.PSObject.Properties[$name].Value
            if (($value -isnot [int] -and $value -isnot [long] -and $value -isnot [double]) -or [double]$value -lt 0 -or [double]$value -gt 4096) { throw 'The organizer workspace state exceeded its numeric contract.' }
        }
        foreach ($name in @('selectedPageOne','dirtyIndicator','unsavedIndicator','cropReady','resetReady','splitReady')) { if ($state.PSObject.Properties[$name].Value -isnot [bool]) { throw 'The organizer workspace state had a non-Boolean flag.' } }
        if ([int]$state.workspaceCount -gt 1 -or [int]$state.headingCount -gt 1 -or [int]$state.pageCount -gt [int]$script:OrganizerPins.PageCount -or [int]$state.alertCount -gt 0) { throw 'The organizer workspace reached an unexpected product state.' }
        if ([int]$state.workspaceCount -eq 1 -and [int]$state.headingCount -eq 1 -and [int]$state.pageCount -eq [int]$script:OrganizerPins.PageCount -and [int]$state.selectedCount -eq 1 -and [bool]$state.selectedPageOne -and [int]$state.firstWidth -eq $ExpectedWidth -and [int]$state.firstHeight -eq $ExpectedHeight -and [bool]$state.dirtyIndicator -eq $ExpectedDirty -and [bool]$state.unsavedIndicator -eq $ExpectedDirty -and [bool]$state.cropReady -and [bool]$state.resetReady -and [bool]$state.splitReady -and [int]$state.dialogCount -eq 0) { return $state }
        if ($SleepProvider) { & $SleepProvider $script:OrganizerPins.UiPollMilliseconds } else { Start-Sleep -Milliseconds $script:OrganizerPins.UiPollMilliseconds }
        $now = if ($UtcNowProvider) { [datetime](& $UtcNowProvider) } else { [datetime]::UtcNow }
    }
    throw 'The organizer workspace did not reach its exact bounded state.'
}

function Open-OrganizerWorkspace {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    Invoke-OrganizerMenuAction -SessionId $SessionId -Action menu -Deadline $Deadline
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'organizer menu command' -Script "return [...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Organize pages'&&!x.disabled&&x.closest('[class*=menuPopover]')).length;" -Predicate { param($value) ($value -is [int] -or $value -is [long]) -and [int]$value -eq 1 }
    Invoke-OrganizerMenuAction -SessionId $SessionId -Action organizer -Deadline $Deadline
    return Wait-OrganizerWorkspaceState -SessionId $SessionId -ExpectedWidth $script:OrganizerPins.SourceWidth -ExpectedHeight $script:OrganizerPins.SourceHeight -ExpectedDirty $false -Deadline $Deadline
}

function Invoke-OrganizerCropInteraction {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $opened = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const r=document.querySelector('section[aria-label=`"Organize pages workspace`"]'),b=r?[...r.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Crop'&&!x.disabled):[];if(b.length===1)b[0].click();return {count:b.length,clicked:b.length===1};"
    Assert-OrganizerExactProperties -Value $opened -Names @('count','clicked') -Kind 'The organizer crop-open receipt'
    if ([int]$opened.count -ne 1 -or $opened.clicked -isnot [bool] -or -not [bool]$opened.clicked) { throw 'The exact organizer Crop control was unavailable.' }
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'organizer crop dialog' -Script "const d=document.querySelector('dialog[aria-labelledby=`"crop-title`"]');return {dialogs:document.querySelectorAll('dialog[aria-labelledby=`"crop-title`"]' ).length,title:d?.querySelector('h2')?.textContent.trim()||'',inputs:d?.querySelectorAll('input[aria-label$=`" crop inset`"]' ).length||0,apply:d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Apply crop'&&!x.disabled).length:0};" -Predicate { param($value) [int]$value.dialogs -eq 1 -and [string]$value.title -ceq 'Crop page 1' -and [int]$value.inputs -eq 4 -and [int]$value.apply -eq 1 }
    $set = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const d=document.querySelector('dialog[aria-labelledby=`"crop-title`"]'),names=['Top','Right','Bottom','Left'],set=Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set,inputs=names.map(n=>d?.querySelector('input[aria-label=`"'+n+' crop inset`"]'));if(inputs.some(x=>!x||x.disabled))return {count:inputs.filter(Boolean).length,set:false};for(const i of inputs){set.call(i,'36');i.dispatchEvent(new Event('input',{bubbles:true}))}return {count:inputs.length,set:inputs.every(x=>x.value==='36')};"
    Assert-OrganizerExactProperties -Value $set -Names @('count','set') -Kind 'The organizer crop-input receipt'
    if ([int]$set.count -ne 4 -or $set.set -isnot [bool] -or -not [bool]$set.set) { throw 'The exact four organizer crop inputs did not accept their bounded values.' }
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'organizer crop preview' -Script "const d=document.querySelector('dialog[aria-labelledby=`"crop-title`"]');return {inputs:[...d?.querySelectorAll('input[aria-label$=`" crop inset`"]')||[]].filter(x=>x.value==='36').length,preview:[...d?.querySelectorAll('p')||[]].some(x=>x.textContent.trim()==='Result: 540.0 × 720.0 pt on every selected page.'),apply:d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Apply crop'&&!x.disabled).length:0};" -Predicate { param($value) [int]$value.inputs -eq 4 -and [bool]$value.preview -and [int]$value.apply -eq 1 }
    $applied = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const d=document.querySelector('dialog[aria-labelledby=`"crop-title`"]'),b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Apply crop'&&!x.disabled):[];if(b.length===1)b[0].click();return {count:b.length,clicked:b.length===1};"
    Assert-OrganizerExactProperties -Value $applied -Names @('count','clicked') -Kind 'The organizer crop-apply receipt'
    if ([int]$applied.count -ne 1 -or $applied.clicked -isnot [bool] -or -not [bool]$applied.clicked) { throw 'The exact organizer Apply crop control was unavailable.' }
    return Wait-OrganizerWorkspaceState -SessionId $SessionId -ExpectedWidth $script:OrganizerPins.CroppedWidth -ExpectedHeight $script:OrganizerPins.CroppedHeight -ExpectedDirty $true -Deadline $Deadline
}

function Invoke-OrganizerResetCropInteraction {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $receipt = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const r=document.querySelector('section[aria-label=`"Organize pages workspace`"]'),b=r?[...r.querySelectorAll('button[aria-label=`"Reset crop on selected pages`"]')].filter(x=>!x.disabled):[];if(b.length===1)b[0].click();return {count:b.length,clicked:b.length===1};"
    Assert-OrganizerExactProperties -Value $receipt -Names @('count','clicked') -Kind 'The organizer reset-crop receipt'
    if ([int]$receipt.count -ne 1 -or $receipt.clicked -isnot [bool] -or -not [bool]$receipt.clicked) { throw 'The exact organizer Reset crop control was unavailable.' }
    return Wait-OrganizerWorkspaceState -SessionId $SessionId -ExpectedWidth $script:OrganizerPins.SourceWidth -ExpectedHeight $script:OrganizerPins.SourceHeight -ExpectedDirty $false -Deadline $Deadline
}

function Open-OrganizerSplitDialog {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $receipt = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const r=document.querySelector('section[aria-label=`"Organize pages workspace`"]'),b=r?[...r.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Split'&&!x.disabled):[];if(b.length===1)b[0].click();return {count:b.length,clicked:b.length===1};"
    Assert-OrganizerExactProperties -Value $receipt -Names @('count','clicked') -Kind 'The organizer split-open receipt'
    if ([int]$receipt.count -ne 1 -or $receipt.clicked -isnot [bool] -or -not [bool]$receipt.clicked) { throw 'The exact organizer Split control was unavailable.' }
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'organizer split dialog' -Script "const d=document.querySelector('dialog[aria-labelledby=`"split-title`"]'),i=d?.querySelector('input[aria-label=`"Pages per split file`"]');return {dialogs:document.querySelectorAll('dialog[aria-labelledby=`"split-title`"]' ).length,title:d?.querySelector('h2')?.textContent.trim()||'',inputs:i?1:0};" -Predicate { param($value) [int]$value.dialogs -eq 1 -and [string]$value.title -ceq 'Split PDF' -and [int]$value.inputs -eq 1 }
    $set = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const d=document.querySelector('dialog[aria-labelledby=`"split-title`"]'),i=d?.querySelector('input[aria-label=`"Pages per split file`"]');if(!i||i.disabled)return {count:i?1:0,set:false};Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(i,'2');i.dispatchEvent(new Event('input',{bubbles:true}));return {count:1,set:i.value==='2'};"
    Assert-OrganizerExactProperties -Value $set -Names @('count','set') -Kind 'The organizer split-input receipt'
    if ([int]$set.count -ne 1 -or $set.set -isnot [bool] -or -not [bool]$set.set) { throw 'The exact organizer split input did not accept its bounded value.' }
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'organizer split preview' -Script "const d=document.querySelector('dialog[aria-labelledby=`"split-title`"]'),i=d?.querySelector('input[aria-label=`"Pages per split file`"]');return {value:i?.value||'',preview:[...d?.querySelectorAll('p')||[]].some(x=>x.textContent.trim()==='Creates 3 files from 6 current pages.'),submit:d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Split PDF'&&!x.disabled).length:0};" -Predicate { param($value) [string]$value.value -ceq '2' -and [bool]$value.preview -and [int]$value.submit -eq 1 }
}

function Invoke-OrganizerProcessBoundSplit {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][int]$ApplicationProcessId,[Parameter(Mandatory = $true)][long]$ApplicationProcessStartUtcTicks,[Parameter(Mandatory = $true)][string]$TargetPath,[Parameter(Mandatory = $true)][datetime]$Deadline)
    if (Test-Path -LiteralPath $TargetPath) { throw 'The organizer split target must be fresh.' }
    $baseline = @(Get-ReadingProcessUiSurfaceSnapshot -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline)
    $baselineSurfaceIdentities = Assert-ReadingSurfaceSnapshot -Snapshot $baseline -ApplicationProcessId $ApplicationProcessId -Kind 'The organizer split pre-click top-level UI baseline'
    $baselineTargets = @(Get-ReadingPickerTargetSnapshot -Surfaces $baseline -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline)
    $null = Assert-ReadingPickerTargetSnapshot -Snapshot $baselineTargets -SurfaceIdentities $baselineSurfaceIdentities -ApplicationProcessId $ApplicationProcessId -Kind 'The organizer split pre-click picker target baseline'
    $clicked = Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script "const d=document.querySelector('dialog[aria-labelledby=`"split-title`"]'),i=d?.querySelector('input[aria-label=`"Pages per split file`"]'),b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Split PDF'&&!x.disabled):[];if(i?.value==='2'&&b.length===1)b[0].click();return {count:b.length,clicked:i?.value==='2'&&b.length===1};"
    Assert-OrganizerExactProperties -Value $clicked -Names @('count','clicked') -Kind 'The organizer split-submit receipt'
    if ([int]$clicked.count -ne 1 -or $clicked.clicked -isnot [bool] -or -not [bool]$clicked.clicked) { throw 'The exact organizer Split PDF control was unavailable.' }
    $binding = Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -BaselineSurfaces $baseline -BaselineTargets $baselineTargets -Deadline $Deadline
    Submit-ProcessBoundOpenDialog -Binding $binding -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Path ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($TargetPath))) -Deadline $Deadline
    Wait-ReadingProcessUiSurfaceClosed -Binding $binding -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline
    $null = Wait-WebDriverOracle -SessionId $SessionId -Deadline $Deadline -Kind 'organizer split completion' -Script "const d=document.querySelector('dialog[aria-labelledby=`"split-title`"]');return {dialogs:document.querySelectorAll('dialog[aria-labelledby=`"split-title`"]' ).length,title:d?.querySelector('h2')?.textContent.trim()||'',summary:[...d?.querySelectorAll('p')||[]].filter(x=>/^Created 3 files in .+\.$/.test(x.textContent.trim())).length,close:d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Close'&&!x.disabled).length:0,alerts:document.querySelectorAll('[role=`"alert`"]' ).length};" -Predicate { param($value) [int]$value.dialogs -eq 1 -and [string]$value.title -ceq 'Split complete' -and [int]$value.summary -eq 1 -and [int]$value.close -eq 1 -and [int]$value.alerts -eq 0 }
    return [string]$binding.filenameControlCategory
}

function Get-OrganizerSplitInventory {
    param([Parameter(Mandatory = $true)][string]$SplitParentRoot,[Parameter(Mandatory = $true)][string]$TargetPath)
    $parent = [IO.Path]::GetFullPath($SplitParentRoot).TrimEnd('\'); $target = [IO.Path]::GetFullPath($TargetPath).TrimEnd('\')
    if (-not $target.StartsWith($parent + '\',[StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetFileName($target) -cne [string]$script:OrganizerPins.SplitFolderName) { throw 'The organizer split target escaped its exact parent and leaf contract.' }
    Assert-NoReparseAncestors -Path $target
    if (-not (Test-Path -LiteralPath $target -PathType Container)) { throw 'The organizer split folder was not created.' }
    $parentEntries = @(Get-ChildItem -LiteralPath $parent -Force)
    if ($parentEntries.Count -ne 1 -or -not $parentEntries[0].PSIsContainer -or $parentEntries[0].Name -cne [string]$script:OrganizerPins.SplitFolderName -or ($parentEntries[0].Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'The organizer split parent inventory was not exact.' }
    $entries = @(Get-ChildItem -LiteralPath $target -Force | Sort-Object Name)
    if ($entries.Count -ne $script:OrganizerPins.SplitFileNames.Count -or @($entries | Where-Object { $_.PSIsContainer -or ($_.Attributes -band [IO.FileAttributes]::ReparsePoint) }).Count -ne 0) { throw 'The organizer split file inventory was not exact.' }
    $total = [uint64]0
    for ($index = 0; $index -lt $entries.Count; $index++) {
        if ($entries[$index].Name -cne [string]$script:OrganizerPins.SplitFileNames[$index] -or [uint64]$entries[$index].Length -eq 0 -or [uint64]$entries[$index].Length -gt [uint64]$script:OrganizerPins.SplitFileMaximumBytes) { throw 'An organizer split output file was missing, empty, oversized, or misnamed.' }
        $total += [uint64]$entries[$index].Length
    }
    if ($total -eq 0 -or $total -gt [uint64]$script:OrganizerPins.SplitTotalMaximumBytes) { throw 'The organizer split output inventory exceeded its total byte contract.' }
    return [pscustomobject][ordered]@{ fileCount=[int]$entries.Count; totalBytes=[uint64]$total }
}

function Assert-OrganizerSplitPdfProof {
    param([Parameter(Mandatory = $true)][string]$PdfiumPath,[Parameter(Mandatory = $true)][string]$SourcePath,[Parameter(Mandatory = $true)][string[]]$SplitPaths,[scriptblock]$ProofProvider)
    if ($SplitPaths.Count -ne 3) { throw 'The organizer PDF proof requires exactly three ordered split files.' }
    $source = if ($ProofProvider) { & $ProofProvider $SourcePath 6 } else { Get-OrganizerPdfiumProof -PdfiumPath $PdfiumPath -PdfPath $SourcePath -MaximumPages 6 }
    if ($null -eq $source -or [int]$source.Pages -ne 6 -or @($source.PageProofs).Count -ne 6) { throw 'Signed PDFium did not independently parse the exact six-page organizer source.' }
    $sourceHashes = [Collections.Generic.List[string]]::new(); $splitHashes = [Collections.Generic.List[string]]::new(); $pageCounts = [Collections.Generic.List[int]]::new()
    foreach ($page in @($source.PageProofs)) {
        if ([double]$page.WidthPoints -ne [double]$script:OrganizerPins.SourceWidth -or [double]$page.HeightPoints -ne [double]$script:OrganizerPins.SourceHeight -or [int]$page.InkPixels -lt 64 -or [string]$page.FingerprintSha256 -cnotmatch '^[A-F0-9]{64}$') { throw 'An organizer source page proof was invalid.' }
        $sourceHashes.Add([string]$page.FingerprintSha256)
    }
    for ($fileIndex = 0; $fileIndex -lt $SplitPaths.Count; $fileIndex++) {
        $proof = if ($ProofProvider) { & $ProofProvider $SplitPaths[$fileIndex] 2 } else { Get-OrganizerPdfiumProof -PdfiumPath $PdfiumPath -PdfPath $SplitPaths[$fileIndex] -MaximumPages 2 }
        if ($null -eq $proof -or [int]$proof.Pages -ne 2 -or @($proof.PageProofs).Count -ne 2) { throw 'Signed PDFium did not independently parse exactly two pages from an organizer split PDF.' }
        $pageCounts.Add([int]$proof.Pages)
        for ($pageIndex = 0; $pageIndex -lt 2; $pageIndex++) {
            $page = @($proof.PageProofs)[$pageIndex]; $sourcePage = @($source.PageProofs)[$fileIndex * 2 + $pageIndex]
            if ([double]$page.WidthPoints -ne [double]$sourcePage.WidthPoints -or [double]$page.HeightPoints -ne [double]$sourcePage.HeightPoints -or [int]$page.InkPixels -lt 64 -or [string]$page.FingerprintSha256 -cnotmatch '^[A-F0-9]{64}$') { throw 'An organizer split page proof was invalid.' }
            $splitHashes.Add([string]$page.FingerprintSha256)
        }
    }
    if (($sourceHashes -join ',') -cne ($splitHashes -join ',')) { throw 'Organizer split page fingerprints did not preserve the exact source page order.' }
    return [pscustomobject][ordered]@{
        splitOutputPageCounts = [int[]]$pageCounts.ToArray()
        sourcePageFingerprintSha256 = [string[]]$sourceHashes.ToArray()
        splitPageFingerprintSha256 = [string[]]$splitHashes.ToArray()
        splitPageFingerprintOrderVerified = $true
    }
}

function Assert-OrganizerResult {
    param([Parameter(Mandatory = $true)]$Result)
    $names = @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','processBoundOpenPickerVerified','openFilenameControlCategory','organizerWorkspaceVerified','cropInteractionVerified','croppedPageWidth','croppedPageHeight','resetCropInteractionVerified','restoredPageWidth','restoredPageHeight','processBoundSplitPickerVerified','splitFilenameControlCategory','splitOutputFileCount','splitOutputBytes','splitOutputPageCounts','sourcePageFingerprintSha256','splitPageFingerprintSha256','splitPageFingerprintOrderVerified','splitFolderCreatedVerified','sourceFixturePreserved','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining')
    if ($null -eq $Result -or ($Result.PSObject.Properties.Name -join ',') -cne ($names -join ',')) { throw 'The installed organizer result had an invalid schema.' }
    if ([string]$Result.nativeDriverVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or [string]$Result.returnedRuntimeVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or [string]$Result.profileBinding -cnotmatch '^(session-capability|owned-webview)-(requested-profile|requested-ebwebview)$' -or [string]$Result.openFilenameControlCategory -notin @('edit-1001','edit-1148') -or [string]$Result.splitFilenameControlCategory -cne 'edit-1152') { throw 'The installed organizer result had an invalid environment or native-control receipt.' }
    foreach ($name in @('processBoundOpenPickerVerified','organizerWorkspaceVerified','cropInteractionVerified','resetCropInteractionVerified','processBoundSplitPickerVerified','splitPageFingerprintOrderVerified','splitFolderCreatedVerified','sourceFixturePreserved','sessionDeleted','ownedProcessTreeStopped')) { if ($Result.PSObject.Properties[$name].Value -isnot [bool] -or -not [bool]$Result.PSObject.Properties[$name].Value) { throw 'The installed organizer result contained an unverified Boolean claim.' } }
    $pageCounts = @($Result.splitOutputPageCounts); $sourceHashes = @($Result.sourcePageFingerprintSha256); $splitHashes = @($Result.splitPageFingerprintSha256)
    if (($pageCounts -join ',') -cne '2,2,2' -or $sourceHashes.Count -ne 6 -or $splitHashes.Count -ne 6 -or @($sourceHashes | Where-Object { [string]$_ -cnotmatch '^[A-F0-9]{64}$' }).Count -ne 0 -or @($splitHashes | Where-Object { [string]$_ -cnotmatch '^[A-F0-9]{64}$' }).Count -ne 0 -or ($sourceHashes -join ',') -cne ($splitHashes -join ',')) { throw 'The installed organizer result did not preserve the exact independently parsed split page order.' }
    if ([int]$Result.croppedPageWidth -ne [int]$script:OrganizerPins.CroppedWidth -or [int]$Result.croppedPageHeight -ne [int]$script:OrganizerPins.CroppedHeight -or [int]$Result.restoredPageWidth -ne [int]$script:OrganizerPins.SourceWidth -or [int]$Result.restoredPageHeight -ne [int]$script:OrganizerPins.SourceHeight -or [int]$Result.splitOutputFileCount -ne 3 -or [uint64]$Result.splitOutputBytes -eq 0 -or [uint64]$Result.splitOutputBytes -gt [uint64]$script:OrganizerPins.SplitTotalMaximumBytes -or [int]$Result.relevantProcessesRemaining -ne 0) { throw 'The installed organizer result did not match its exact crop, split, or cleanup contract.' }
}

function Invoke-RealInstalledOrganizerTools {
    param([string]$ApplicationPath,[string]$PdfiumPath,[string]$TauriDriverPath,[string]$EdgeDriverPath,[string]$ProfileRoot,[string]$SettingsRoot,[string]$FixturePath,[string]$SplitParentRoot,[string]$ExpectedEdgeDriverVersion,[string]$ExpectedRuntimeVersion)
    $null = Wait-FixedWebDriverPortsFree -Deadline ([datetime]::UtcNow.AddMilliseconds($script:OrganizerPins.PortReleaseTimeoutMilliseconds))
    $launchDeadline = New-OrganizerPhaseDeadline -Phase launch
    $driverCapture = $null; $driver = $null; $sessionId = $null; $captured = @(); $result = $null
    $startedAfter = [datetime]::UtcNow; $sessionDeleteOutcome = 'requestfailed'; $driverExited = $false; $driverStopOutcome = 'not-invoked'; $processesQuiescent = $false; $remaining = -1; $residualCategory = 'multiple'
    $capturedOutcomes = [pscustomobject]@{ application='absent';tauriDriver='absent';edgeDriver='absent';webview='absent';ocrEngine='absent';other='absent' }
    $residualFacts = [pscustomobject]@{ ownership='none';application=$false;tauriDriver=$false;edgeDriver=$false;webview=$false;ocrEngine=$false;other=$false }
    try {
        $driverCapture = Start-BoundedDiscardProcess -Path $TauriDriverPath -Arguments @("--port=$($script:LaunchPins.WebDriverPort)","--native-port=$($script:LaunchPins.NativeDriverPort)","--native-driver=$EdgeDriverPath") -Environment (Get-MinimalWindowsProcessEnvironment)
        $driverCapture.Start(); $driver = $driverCapture.Process
        $status = $null
        $status = Wait-OrganizerDriverReady -Driver $driver -Deadline $launchDeadline
        $nativeDriverVersion = Wait-NativeDriverStatus -ExpectedVersion $ExpectedEdgeDriverVersion -Deadline $launchDeadline -TauriDriver $driver
        $sessionBody = [ordered]@{ capabilities=[ordered]@{ alwaysMatch=[ordered]@{ browserName='wry';'tauri:options'=[ordered]@{ application=$ApplicationPath;args=@();webviewOptions=[ordered]@{userDataFolder=$ProfileRoot} } } } }
        $session = Invoke-BoundedLoopbackJson -Method POST -Path '/session' -Body $sessionBody -Deadline $launchDeadline
        if ($null -eq $session.value -or [string]$session.value.sessionId -cnotmatch '^[A-Za-z0-9-]+$') { throw 'Organizer WebDriver session identifier was invalid.' }
        $sessionId = [string]$session.value.sessionId; $capabilities = $session.value.capabilities
        $capabilityKeys = @($capabilities.PSObject.Properties.Name | Sort-Object -CaseSensitive)
        if ($capabilityKeys.Count -eq 0 -or $capabilityKeys.Count -gt 32 -or @($capabilityKeys | Where-Object { $_ -cnotmatch '^[A-Za-z0-9:._-]{1,64}$' }).Count -ne 0) { throw 'Organizer returned an unsafe session capability-key set.' }
        $vendorDriver = $capabilities.PSObject.Properties['msedge.msedgedriverVersion']
        if ($null -ne $vendorDriver -and ([string]$vendorDriver.Value -split '\s+')[0] -cne $ExpectedEdgeDriverVersion) { throw 'Organizer session EdgeDriver capability disagrees with its trusted receipt.' }
        $returnedRuntimeVersion = [string]$capabilities.browserVersion; $runtimeParts = $returnedRuntimeVersion.Split('.'); $expectedRuntimeParts = $ExpectedRuntimeVersion.Split('.')
        if ($returnedRuntimeVersion -cnotmatch '^\d+\.\d+\.\d+\.\d+$' -or $runtimeParts.Count -ne 4 -or ($runtimeParts[0..2] -join '.') -cne ($expectedRuntimeParts[0..2] -join '.')) { throw 'Organizer WebView2 runtime capability disagrees with its trusted receipt.' }
        $returnedUserData = if ($null -ne $capabilities.PSObject.Properties['msedge.userDataDir']) { [string]$capabilities.'msedge.userDataDir' } else { '' }
        $null = Wait-WebDriverOracle -SessionId $sessionId -Deadline $launchDeadline -Kind 'organizer home' -Script (Get-ReadingHomeScript) -Predicate { param($value) [bool]$value.ready -and [string]$value.title -ceq 'PDF Workstation' -and [int]$value.sample -eq 1 }
        do {
            $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
            $apps = @($captured | Where-Object { [string]$_.Path -and [IO.Path]::GetFullPath([string]$_.Path).Equals([IO.Path]::GetFullPath($ApplicationPath),[StringComparison]::OrdinalIgnoreCase) })
            if ($apps.Count -eq 1) { break }; if ($apps.Count -gt 1) { throw 'More than one owned installed application process was found for organizer verification.' }; Start-Sleep -Milliseconds 100
        } while ([datetime]::UtcNow -lt $launchDeadline)
        if ($apps.Count -ne 1) { throw 'The owned installed application process was unavailable for organizer UI Automation.' }
        $applicationProcessId = [int]$apps[0].ProcessId; $applicationProcessStartUtcTicks = [long]$apps[0].StartTicks
        if ($applicationProcessStartUtcTicks -le 0) { throw 'The organizer application process start identity was invalid.' }
        Assert-ReadingOwnedExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -RootProcessId $driver.Id
        $profileBinding = if ($returnedUserData) { 'session-capability-' + (Get-ExactProfileBinding -Candidate $returnedUserData -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot) } else { Get-OwnedProfileBinding -Owned $captured -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot }
        Assert-ReadingProfileScope -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -Binding $profileBinding

        Assert-OrganizerPhaseTransition -Deadline $launchDeadline -Phase launch
        $openDeadline = New-OrganizerPhaseDeadline -Phase native-picker
        $openCategory = Open-ReadingUserFile -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationProcessStartUtcTicks -Path $FixturePath -Deadline $openDeadline
        Assert-OrganizerPhaseTransition -Deadline $openDeadline -Phase native-picker
        $productDeadline = New-OrganizerPhaseDeadline -Phase product
        $null = Wait-OrganizerViewerState -SessionId $sessionId -ExpectedName ([IO.Path]::GetFileName($FixturePath)) -Deadline $productDeadline
        $baseline = Open-OrganizerWorkspace -SessionId $sessionId -Deadline $productDeadline
        $cropped = Invoke-OrganizerCropInteraction -SessionId $sessionId -Deadline $productDeadline
        $restored = Invoke-OrganizerResetCropInteraction -SessionId $sessionId -Deadline $productDeadline
        Open-OrganizerSplitDialog -SessionId $sessionId -Deadline $productDeadline
        Assert-OrganizerPhaseTransition -Deadline $productDeadline -Phase product
        $splitDeadline = New-OrganizerPhaseDeadline -Phase native-picker
        $splitTarget = Join-Path $SplitParentRoot ([string]$script:OrganizerPins.SplitFolderName)
        $splitCategory = Invoke-OrganizerProcessBoundSplit -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationProcessStartUtcTicks -TargetPath $splitTarget -Deadline $splitDeadline
        Assert-OrganizerPhaseTransition -Deadline $splitDeadline -Phase native-picker
        $finalDeadline = New-OrganizerPhaseDeadline -Phase product
        $inventory = Get-OrganizerSplitInventory -SplitParentRoot $SplitParentRoot -TargetPath $splitTarget
        $splitPaths = [string[]]@($script:OrganizerPins.SplitFileNames | ForEach-Object { Join-Path $splitTarget ([string]$_) })
        $pdfProof = Assert-OrganizerSplitPdfProof -PdfiumPath $PdfiumPath -SourcePath $FixturePath -SplitPaths $splitPaths

        $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
        Assert-ReadingOwnedExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -RootProcessId $driver.Id
        Assert-ReadingProfileScope -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -Binding $profileBinding
        if ($driverCapture.Exceeded) { throw 'Organizer WebDriver diagnostic output exceeded its discarded cap.' }
        Assert-OrganizerPhaseTransition -Deadline $finalDeadline -Phase product
        $result = [pscustomobject][ordered]@{
            nativeDriverVersion = [string]$nativeDriverVersion
            returnedRuntimeVersion = [string]$returnedRuntimeVersion
            profileBinding = [string]$profileBinding
            processBoundOpenPickerVerified = $true
            openFilenameControlCategory = [string]$openCategory
            organizerWorkspaceVerified = ([int]$baseline.workspaceCount -eq 1)
            cropInteractionVerified = $true
            croppedPageWidth = [int]$cropped.firstWidth
            croppedPageHeight = [int]$cropped.firstHeight
            resetCropInteractionVerified = $true
            restoredPageWidth = [int]$restored.firstWidth
            restoredPageHeight = [int]$restored.firstHeight
            processBoundSplitPickerVerified = $true
            splitFilenameControlCategory = [string]$splitCategory
            splitOutputFileCount = [int]$inventory.fileCount
            splitOutputBytes = [uint64]$inventory.totalBytes
            splitOutputPageCounts = [int[]]$pdfProof.splitOutputPageCounts
            sourcePageFingerprintSha256 = [string[]]$pdfProof.sourcePageFingerprintSha256
            splitPageFingerprintSha256 = [string[]]$pdfProof.splitPageFingerprintSha256
            splitPageFingerprintOrderVerified = [bool]$pdfProof.splitPageFingerprintOrderVerified
            splitFolderCreatedVerified = $true
            sourceFixturePreserved = $true
        }
    } finally {
        $cleanupDeadline = [datetime]::UtcNow.AddSeconds(10)
        if ($sessionId) { $sessionDeleteOutcome = Invoke-SessionDeleteOutcome -SessionId $sessionId -Deadline $cleanupDeadline }
        if ($driver) {
            try { $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured) } catch { }
            $processCleanupDeadline = [datetime]::UtcNow.AddMilliseconds($script:LaunchPins.CleanupProcessTimeoutMilliseconds)
            $capturedOutcomes = Stop-OwnedLaunchProcesses -TauriDriver $driver -Captured $captured -Deadline $processCleanupDeadline
            $driverStopOutcome = [string]$capturedOutcomes.rootOutcome
            $quiescence = Wait-LaunchProcessQuiescence -Deadline $processCleanupDeadline; $processesQuiescent = [bool]$quiescence.stable
            $remainingProcesses = @($quiescence.processes); $remaining = $remainingProcesses.Count; $residualCategory = Get-LaunchResidualCategory -Processes $remainingProcesses; $residualFacts = Get-LaunchResidualFacts -Processes $remainingProcesses -Captured $captured; $driverExited = [bool]$driver.HasExited
        }
        if ($driverCapture) { $driverCapture.Dispose() }
    }
    Assert-LaunchCleanupState -Result $result -SessionDeleteOutcome $sessionDeleteOutcome -DriverExited $driverExited -RelevantProcessesClear ($processesQuiescent -and $residualCategory -ceq 'none') -DriverStopOutcome $driverStopOutcome -ResidualCategory $residualCategory -CapturedApplication $capturedOutcomes.application -CapturedTauriDriver $capturedOutcomes.tauriDriver -CapturedEdgeDriver $capturedOutcomes.edgeDriver -CapturedWebView $capturedOutcomes.webview -CapturedOcrEngine $capturedOutcomes.ocrEngine -CapturedOther $capturedOutcomes.other -ResidualOwnership $residualFacts.ownership -ResidualApplication $residualFacts.application -ResidualTauriDriver $residualFacts.tauriDriver -ResidualEdgeDriver $residualFacts.edgeDriver -ResidualWebView $residualFacts.webview -ResidualOcrEngine $residualFacts.ocrEngine -ResidualOther $residualFacts.other
    if ($null -eq $result -or $sessionDeleteOutcome -cne 'verified' -or -not $driverExited -or -not $processesQuiescent -or $remaining -ne 0) { throw 'Installed organizer cleanup did not reach its exact zero-process state.' }
    $result | Add-Member -NotePropertyName sessionDeleted -NotePropertyValue $true
    $result | Add-Member -NotePropertyName ownedProcessTreeStopped -NotePropertyValue $true
    $result | Add-Member -NotePropertyName relevantProcessesRemaining -NotePropertyValue 0
    Assert-OrganizerResult -Result $result
    return $result
}

function Invoke-InstalledOrganizerTools {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,
        [Parameter(Mandatory = $true)]$ApplicationReceipt,
        [Parameter(Mandatory = $true)][string]$PdfiumPath,
        [Parameter(Mandatory = $true)]$PdfiumReceipt,
        [Parameter(Mandatory = $true)][string]$WebDriverRoot,
        [Parameter(Mandatory = $true)][string]$ProfileRoot,
        [Parameter(Mandatory = $true)][string]$SettingsRoot,
        [Parameter(Mandatory = $true)][string]$FixturePath,
        [Parameter(Mandatory = $true)]$FixtureReceipt,
        [Parameter(Mandatory = $true)][string]$SplitParentRoot,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher
    )
    $receipt = Get-Content -LiteralPath (Join-Path $WebDriverRoot 'webdriver-receipt.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-WebDriverReceipt -Receipt $receipt
    $tauriDriver = Join-Path $WebDriverRoot 'tauri-driver-install/bin/tauri-driver.exe'; $edgeDriver = Join-Path $WebDriverRoot 'edge-driver/msedgedriver.exe'
    Assert-ReadingFileReceipt -Path $ApplicationPath -Bytes ([uint64]$ApplicationReceipt.bytes) -Sha256 ([string]$ApplicationReceipt.sha256) -Kind 'Installed signed organizer application'
    $null = Assert-TrustedWindowsSignature -Path $ApplicationPath -ExpectedPublisher $ExpectedPublisher
    Assert-ReadingFileReceipt -Path $PdfiumPath -Bytes ([uint64]$PdfiumReceipt.bytes) -Sha256 ([string]$PdfiumReceipt.sha256) -Kind 'Installed signed organizer PDFium'
    $null = Assert-TrustedWindowsSignature -Path $PdfiumPath -ExpectedPublisher $ExpectedPublisher
    Assert-ReadingFileReceipt -Path $tauriDriver -Bytes ([uint64]$receipt.tauriDriver.bytes) -Sha256 ([string]$receipt.tauriDriver.sha256) -Kind 'Pinned organizer tauri-driver'
    Assert-ReadingFileReceipt -Path $edgeDriver -Bytes ([uint64]$receipt.edgeDriver.bytes) -Sha256 ([string]$receipt.edgeDriver.sha256) -Kind 'Pinned organizer EdgeDriver'
    $null = Assert-TrustedWindowsSignature -Path $edgeDriver -ExpectedPublisher $script:LaunchPins.EdgePublisher
    if ([IO.Path]::GetFileName($FixturePath) -cne 'organizer-source.pdf') { throw 'The organizer fixture must use its exact allowlisted filename.' }
    Assert-ReadingFileReceipt -Path $FixturePath -Bytes ([uint64]$FixtureReceipt.bytes) -Sha256 ([string]$FixtureReceipt.sha256) -Kind 'Organizer source fixture'
    if (Test-Path -LiteralPath $ProfileRoot) { throw 'Organizer WebView profile must be fresh.' }
    if (-not (Test-Path -LiteralPath $SettingsRoot -PathType Container) -or -not (Test-Path -LiteralPath $SplitParentRoot -PathType Container)) { throw 'The controlled organizer settings or split parent root is missing.' }
    Assert-NoReparseAncestors -Path $SettingsRoot; Assert-NoReparseAncestors -Path $SplitParentRoot
    if (@(Get-ChildItem -LiteralPath $SplitParentRoot -Force).Count -ne 0) { throw 'The organizer split parent root must be fresh and empty.' }
    [IO.Directory]::CreateDirectory($ProfileRoot) | Out-Null
    $settingsEntries = @(Get-ChildItem -LiteralPath $SettingsRoot -Force)
    if ($settingsEntries.Count -ne 1 -or $settingsEntries[0].PSIsContainer -or $settingsEntries[0].Name -cne 'upgrade-sentinel.json' -or ($settingsEntries[0].Attributes -band [IO.FileAttributes]::ReparsePoint) -or @(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -ne 0) { throw 'Organizer profile roots were not fresh, exclusive, and sentinel-only before launch.' }
    $settingsSentinel = $settingsEntries[0].FullName; $settingsSentinelBytes = [uint64]$settingsEntries[0].Length; $settingsSentinelSha256 = Get-ExactSha256 -Path $settingsSentinel
    $fixtureBytes = [uint64]$FixtureReceipt.bytes; $fixtureSha256 = [string]$FixtureReceipt.sha256
    if (@(Get-LaunchProcessSnapshot).Count -ne 0) { throw 'A relevant application or WebDriver process existed before organizer verification.' }
    $uiResult = $null
    try {
        $uiResult = Invoke-RealInstalledOrganizerTools -ApplicationPath $ApplicationPath -PdfiumPath $PdfiumPath -TauriDriverPath $tauriDriver -EdgeDriverPath $edgeDriver -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -FixturePath $FixturePath -SplitParentRoot $SplitParentRoot -ExpectedEdgeDriverVersion ([string]$receipt.edgeDriver.version) -ExpectedRuntimeVersion ([string]$receipt.webView2RuntimeVersion)
    } finally {
        Assert-ReadingFileReceipt -Path $settingsSentinel -Bytes $settingsSentinelBytes -Sha256 $settingsSentinelSha256 -Kind 'Post-flow organizer settings sentinel'
        Assert-ReadingFileReceipt -Path $FixturePath -Bytes $fixtureBytes -Sha256 $fixtureSha256 -Kind 'Post-flow organizer source fixture'
    }
    Assert-OrganizerResult -Result $uiResult
    return $uiResult
}
