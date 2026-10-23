[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

. (Join-Path $PSScriptRoot 'installed-reading-tools.ps1')
. (Join-Path $PSScriptRoot 'installed-print-dialog.ps1')

$script:ImagePagePins = [ordered]@{
    TotalTimeoutMilliseconds = 360000
    NativeDialogTimeoutMilliseconds = 180000
    OutputTimeoutMilliseconds = 60000
    OutputBytesMaximum = 256MB
    ExpectedPdfWidthPoints = 612.0
    ExpectedPdfHeightPoints = 792.0
    ExpectedExportDpi = 150
    ExpectedExportWidth = 1275
    ExpectedExportHeight = 1650
    ExpectedPixelsPerMeter = 5906
    RasterMeanChannelDifferenceMaximum = 0.25
    RasterMaximumChannelDifference = 8
    RasterDifferentChannelFractionMaximum = 0.02
}

function Assert-ImagePageExactProperties {
    param([Parameter(Mandatory = $true)]$Value,[Parameter(Mandatory = $true)][string[]]$Expected,[Parameter(Mandatory = $true)][string]$Kind)
    if ($null -eq $Value) { throw "$Kind is missing." }
    $actual = @($Value.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    $wanted = @($Expected | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $wanted.Count -or ($actual -join ',') -cne ($wanted -join ',')) { throw "$Kind has an unexpected shape." }
}

function Assert-ImagePageFileReceipt {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)][uint64]$Bytes,[Parameter(Mandatory = $true)][string]$Sha256,[Parameter(Mandatory = $true)][string]$Kind)
    Assert-NoReparseAncestors -Path $Path
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { throw "$Kind is missing." }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw "$Kind is reparse-backed." }
    if ([uint64]$item.Length -ne $Bytes -or [string]$Sha256 -cnotmatch '^[A-F0-9]{64}$' -or (Get-ExactSha256 -Path $Path) -cne $Sha256) { throw "$Kind does not match its exact receipt." }
}

function Initialize-ImagePageRasterProof {
    if ('ImagePagePdfiumRasterProof' -as [type]) { return }
    Add-Type -AssemblyName System.Drawing
    $null = [Drawing.Bitmap]; $null = [Security.Cryptography.SHA256]
    $proofAssemblies = @([AppDomain]::CurrentDomain.GetAssemblies() | Where-Object { -not [string]::IsNullOrWhiteSpace($_.Location) -and ($_.GetName().Name -like 'System.Drawing*' -or $_.GetName().Name -like 'System.Private.Windows.*' -or $_.GetName().Name -ceq 'System.Security.Cryptography') } | ForEach-Object Location | Sort-Object -Unique)
    Add-Type -ReferencedAssemblies $proofAssemblies -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.IO;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

public sealed class ImagePagePdfiumRasterProof {
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

  readonly IntPtr module; readonly Init init; readonly LoadMem load; readonly CloseDocument closeDocument; readonly PageCount pageCount;
  readonly LoadPage loadPage; readonly ClosePage closePage; readonly PageMetric pageWidth; readonly PageMetric pageHeight;
  readonly BitmapCreate bitmapCreate; readonly BitmapDestroy bitmapDestroy; readonly BitmapFill bitmapFill; readonly Render render;
  readonly BitmapBuffer bitmapBuffer; readonly BitmapInt bitmapStride;
  T Get<T>(string name) where T : Delegate { var address=GetProcAddress(module,name); if(address==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium is missing a required image-page export."); return Marshal.GetDelegateForFunctionPointer<T>(address); }
  public ImagePagePdfiumRasterProof(string path) {
    module=LoadLibraryExW(Path.GetFullPath(path),IntPtr.Zero,0x00000100|0x00001000); if(module==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not be loaded for image-page proof.");
    init=Get<Init>("FPDF_InitLibrary");load=Get<LoadMem>("FPDF_LoadMemDocument64");closeDocument=Get<CloseDocument>("FPDF_CloseDocument");pageCount=Get<PageCount>("FPDF_GetPageCount");
    loadPage=Get<LoadPage>("FPDF_LoadPage");closePage=Get<ClosePage>("FPDF_ClosePage");pageWidth=Get<PageMetric>("FPDF_GetPageWidth");pageHeight=Get<PageMetric>("FPDF_GetPageHeight");
    bitmapCreate=Get<BitmapCreate>("FPDFBitmap_Create");bitmapDestroy=Get<BitmapDestroy>("FPDFBitmap_Destroy");bitmapFill=Get<BitmapFill>("FPDFBitmap_FillRect");render=Get<Render>("FPDF_RenderPageBitmap");bitmapBuffer=Get<BitmapBuffer>("FPDFBitmap_GetBuffer");bitmapStride=Get<BitmapInt>("FPDFBitmap_GetStride");init();
  }
  public sealed class RasterProof { public byte[] Rgb{get;set;} public string Sha256{get;set;} public double RedFraction{get;set;} public double GreenFraction{get;set;} public double BlueFraction{get;set;} public double WhiteFraction{get;set;} }
  public sealed class Comparison { public double MeanChannelDifference{get;set;} public int MaximumChannelDifference{get;set;} public double DifferentChannelFraction{get;set;} }
  static double Fraction(byte[] rgb,int width,int x0,int y0,int x1,int y1,byte red,byte green,byte blue,int tolerance) {
    long match=0,total=0; for(int y=y0;y<y1;y++)for(int x=x0;x<x1;x++){int i=(y*width+x)*3;total++;if(Math.Abs(rgb[i]-red)<=tolerance&&Math.Abs(rgb[i+1]-green)<=tolerance&&Math.Abs(rgb[i+2]-blue)<=tolerance)match++;} return (double)match/total;
  }
  static RasterProof ExpectedRegions(byte[] rgb,int width,int height) {
    if(width!=1275||height!=1650||rgb==null||rgb.Length!=width*height*3)throw new InvalidOperationException("Image-page raster dimensions are not the exact 150 DPI portrait Letter contract.");
    double red=Fraction(rgb,width,220,250,380,1400,218,71,64,24),green=Fraction(rgb,width,540,250,700,1400,41,139,98,24),blue=Fraction(rgb,width,870,250,1050,1400,49,94,171,24);
    long whiteMatch=0,whiteTotal=0; int[,] boxes={{0,0,100,70},{1175,0,1275,70},{0,1580,100,1650},{1175,1580,1275,1650}};
    for(int b=0;b<4;b++)for(int y=boxes[b,1];y<boxes[b,3];y++)for(int x=boxes[b,0];x<boxes[b,2];x++){int i=(y*width+x)*3;whiteTotal++;if(rgb[i]>=250&&rgb[i+1]>=250&&rgb[i+2]>=250)whiteMatch++;}
    double white=(double)whiteMatch/whiteTotal; if(red<0.70||green<0.70||blue<0.70||white<0.98)throw new InvalidOperationException("Signed PDFium raster did not preserve the deterministic source-color regions and white page margins.");
    return new RasterProof{Rgb=rgb,Sha256=Convert.ToHexString(SHA256.HashData(rgb)),RedFraction=red,GreenFraction=green,BlueFraction=blue,WhiteFraction=white};
  }
  public static RasterProof ProveExpectedRegions(byte[] rgb,int width,int height) { return ExpectedRegions(rgb,width,height); }
  public RasterProof Inspect(string path,int width,int height,long maximumBytes) {
    var bytes=File.ReadAllBytes(path);if(bytes.Length<=0||bytes.LongLength>maximumBytes)throw new InvalidOperationException("Image-page PDF proof input is empty or oversized.");
    var hold=GCHandle.Alloc(bytes,GCHandleType.Pinned);IntPtr document=IntPtr.Zero,page=IntPtr.Zero,bitmap=IntPtr.Zero;
    try{
      document=load(hold.AddrOfPinnedObject(),(ulong)bytes.LongLength,IntPtr.Zero);if(document==IntPtr.Zero||pageCount(document)!=1)throw new InvalidOperationException("Signed PDFium did not parse exactly one created image page.");
      page=loadPage(document,0);if(page==IntPtr.Zero||Math.Abs(pageWidth(page)-612.0)>0.01||Math.Abs(pageHeight(page)-792.0)>0.01)throw new InvalidOperationException("Created image PDF is not one exact portrait Letter page.");
      bitmap=bitmapCreate(width,height,1);if(bitmap==IntPtr.Zero)throw new InvalidOperationException("Signed PDFium could not allocate the exact image-page raster.");bitmapFill(bitmap,0,0,width,height,0xFFFFFFFF);render(bitmap,page,0,0,width,height,0,0x801);
      int stride=bitmapStride(bitmap);if(stride<width*4||stride>width*8)throw new InvalidOperationException("Signed PDFium returned an invalid image-page stride.");var raw=new byte[stride*height];Marshal.Copy(bitmapBuffer(bitmap),raw,0,raw.Length);var rgb=new byte[width*height*3];
      for(int y=0;y<height;y++)for(int x=0;x<width;x++){int source=y*stride+x*4,target=(y*width+x)*3;int alpha=raw[source+3];rgb[target]=(byte)((raw[source+2]*alpha+255*(255-alpha)+127)/255);rgb[target+1]=(byte)((raw[source+1]*alpha+255*(255-alpha)+127)/255);rgb[target+2]=(byte)((raw[source]*alpha+255*(255-alpha)+127)/255);} return ExpectedRegions(rgb,width,height);
    }finally{if(bitmap!=IntPtr.Zero)bitmapDestroy(bitmap);if(page!=IntPtr.Zero)closePage(page);if(document!=IntPtr.Zero)closeDocument(document);if(hold.IsAllocated)hold.Free();}
  }
  public static Comparison ComparePng(byte[] expected,string path,int width,int height) {
    if(expected==null||expected.Length!=width*height*3)throw new ArgumentException("Expected image-page raster is invalid.");using(var image=new Bitmap(path)){if(image.Width!=width||image.Height!=height)throw new InvalidOperationException("Decoded PNG dimensions differ from the expected PDFium raster.");
      var rectangle=new Rectangle(0,0,width,height);var data=image.LockBits(rectangle,ImageLockMode.ReadOnly,PixelFormat.Format24bppRgb);try{int stride=Math.Abs(data.Stride);var raw=new byte[stride*height];Marshal.Copy(data.Scan0,raw,0,raw.Length);long total=0,different=0;int maximum=0;
        for(int y=0;y<height;y++){int sourceY=data.Stride>=0?y:height-1-y;for(int x=0;x<width;x++){int source=sourceY*stride+x*3,target=(y*width+x)*3;for(int channel=0;channel<3;channel++){int actual=raw[source+(2-channel)],difference=Math.Abs(actual-expected[target+channel]);total+=difference;if(difference!=0)different++;maximum=Math.Max(maximum,difference);}}}
        return new Comparison{MeanChannelDifference=(double)total/expected.Length,MaximumChannelDifference=maximum,DifferentChannelFraction=(double)different/expected.Length};
      }finally{image.UnlockBits(data);}}
  }
}
'@
}

function Get-ImagePagePdfiumRasterProof {
    param([Parameter(Mandatory = $true)][string]$PdfiumPath,[Parameter(Mandatory = $true)][string]$PdfPath)
    Initialize-ImagePageRasterProof
    $engine = [ImagePagePdfiumRasterProof]::new($PdfiumPath)
    return $engine.Inspect($PdfPath,[int]$script:ImagePagePins.ExpectedExportWidth,[int]$script:ImagePagePins.ExpectedExportHeight,[long]$script:ImagePagePins.OutputBytesMaximum)
}

function Get-ImagePagePhaseDeadline {
    param([Parameter(Mandatory = $true)][datetime]$TotalDeadline,[Parameter(Mandatory = $true)][int]$MaximumMilliseconds)
    if ($MaximumMilliseconds -lt 1 -or [datetime]::UtcNow -ge $TotalDeadline) { throw 'Installed image page-tools verification exceeded its shared total deadline.' }
    $phase = [datetime]::UtcNow.AddMilliseconds($MaximumMilliseconds)
    if ($phase -gt $TotalDeadline) { return $TotalDeadline }
    return $phase
}

function Get-ImagePageWebDriverFailureCategory {
    param([Parameter(Mandatory = $true)][string]$Message)
    if ($Message -like '*response headers exceeded their deadline*') { return 'response-headers-timeout' }
    if ($Message -like '*response body exceeded its deadline*' -or $Message -like '*response read exceeded its deadline*') { return 'response-body-timeout' }
    if ($Message -match '\bHTTP\b|unexpected HTTP status') { return 'http-failure' }
    if ($Message -like '*response has no value*' -or $Message -like '*empty JSON response*') { return 'invalid-response' }
    return 'request-failure'
}

function Invoke-ImagePageWebDriverScript {
    param(
        [Parameter(Mandatory = $true)][string]$SessionId,
        [Parameter(Mandatory = $true)][string]$Script,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [Parameter(Mandatory = $true)][ValidateSet('open-create-dialog','submit-create','open-menu','open-export-dialog','submit-export')][string]$Stage
    )
    try { return Invoke-WebDriverScript -SessionId $SessionId -Script $Script -Deadline $Deadline }
    catch { throw "Installed image page-tools WebDriver stage failed: $Stage/$((Get-ImagePageWebDriverFailureCategory -Message ([string]$_.Exception.Message)))." }
}

function Wait-ImagePageWebDriverOracle {
    param(
        [Parameter(Mandatory = $true)][string]$SessionId,
        [Parameter(Mandatory = $true)][string]$Script,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [Parameter(Mandatory = $true)][scriptblock]$Predicate,
        [Parameter(Mandatory = $true)][ValidateSet('home','create-defaults','created-workspace','export-defaults','export-completion')][string]$Stage
    )
    try { return Wait-WebDriverOracle -SessionId $SessionId -Script $Script -Deadline $Deadline -Kind "Installed image page-tools $Stage" -Predicate $Predicate }
    catch { throw "Installed image page-tools WebDriver stage failed: $Stage/$((Get-ImagePageWebDriverFailureCategory -Message ([string]$_.Exception.Message)))." }
}

function Initialize-ImagePagePngCrc {
    if ('ImagePagePngCrc32' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.IO;

public static class ImagePagePngCrc32 {
  static uint Update(uint crc, byte value) {
    crc ^= value;
    for (int bit=0;bit<8;bit++) crc=(crc&1)!=0?0xedb88320U^(crc>>1):crc>>1;
    return crc;
  }
  public static void ReadAndValidate(Stream stream, byte[] type, int length, byte[] capture) {
    if(stream==null||type==null||type.Length!=4||length<0||capture!=null&&capture.Length!=length)throw new ArgumentException("Invalid PNG chunk CRC input.");
    uint crc=0xffffffffU;for(int i=0;i<type.Length;i++)crc=Update(crc,type[i]);
    var buffer=new byte[Math.Min(8192,Math.Max(1,length))];int remaining=length,offset=0;
    while(remaining>0){int wanted=Math.Min(buffer.Length,remaining),read=stream.Read(buffer,0,wanted);if(read!=wanted)throw new InvalidDataException("Exported PNG chunk data was truncated.");for(int i=0;i<read;i++)crc=Update(crc,buffer[i]);if(capture!=null)Buffer.BlockCopy(buffer,0,capture,offset,read);offset+=read;remaining-=read;}
    crc=~crc;var stored=new byte[4];if(stream.Read(stored,0,4)!=4)throw new InvalidDataException("Exported PNG chunk checksum was truncated.");
    if(stored[0]!=(byte)(crc>>24)||stored[1]!=(byte)(crc>>16)||stored[2]!=(byte)(crc>>8)||stored[3]!=(byte)crc)throw new InvalidDataException("Exported PNG chunk CRC is invalid.");
  }
}
'@
}

function Wait-ImagePageStableFile {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $lastLength = -1L
    $stableObservations = 0
    while ([datetime]::UtcNow -lt $Deadline) {
        if (Test-Path -LiteralPath $Path -PathType Leaf) {
            $item = Get-Item -LiteralPath $Path -Force
            if ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'An image page-tools output is reparse-backed.' }
            $length = [long]$item.Length
            if ($length -gt $script:ImagePagePins.OutputBytesMaximum) { throw 'An image page-tools output exceeded its exact byte cap.' }
            if ($length -gt 0 -and $length -eq $lastLength) { $stableObservations++ } else { $stableObservations = 0; $lastLength = $length }
            if ($stableObservations -ge 2) { return $item }
        } else { $lastLength = -1L; $stableObservations = 0 }
        Start-Sleep -Milliseconds 200
    }
    throw 'An image page-tools output did not become stable before its bounded deadline.'
}

function Get-ImagePagePngProof {
    param([Parameter(Mandatory = $true)][string]$Path)
    Assert-NoReparseAncestors -Path $Path
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.Length -lt 33 -or $item.Length -gt $script:ImagePagePins.OutputBytesMaximum) { throw 'Exported PNG length is outside its exact bounds.' }
    Initialize-ImagePagePngCrc
    $stream = [IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    try {
        $header = [byte[]]::new(8)
        if ($stream.Read($header,0,$header.Length) -ne $header.Length) { throw 'Exported PNG header was truncated.' }
        $signature = [byte[]](137,80,78,71,13,10,26,10)
        for ($index = 0; $index -lt $signature.Length; $index++) { if ($header[$index] -ne $signature[$index]) { throw 'Exported PNG signature is invalid.' } }
        $width = 0; $height = 0; $headerSeen = $false; $imageDataSeen = $false; $physicalCount = 0; $physicalX = 0; $physicalY = 0; $physicalUnit = 0; $ended = $false
        while ($stream.Position -lt $stream.Length) {
            $chunkHeader = [byte[]]::new(8)
            if ($stream.Read($chunkHeader,0,8) -ne 8) { throw 'Exported PNG chunk header was truncated.' }
            $length = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($chunkHeader,0)); $name = [Text.Encoding]::ASCII.GetString($chunkHeader,4,4)
            if ($length -lt 0 -or $length -gt $script:ImagePagePins.OutputBytesMaximum -or $stream.Position + [int64]$length + 4 -gt $stream.Length) { throw 'Exported PNG chunk length is invalid.' }
            $capture = $null
            if ($name -ceq 'IHDR' -or $name -ceq 'pHYs') { $capture = [byte[]]::new($length) }
            [ImagePagePngCrc32]::ReadAndValidate($stream,[byte[]]$chunkHeader[4..7],$length,$capture)
            if (-not $headerSeen) {
                if ($name -cne 'IHDR' -or $length -ne 13) { throw 'Exported PNG is missing its exact initial IHDR chunk.' }
                $width = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($capture,0)); $height = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($capture,4))
                if ($width -lt 1 -or $height -lt 1 -or $capture[8] -ne 8 -or $capture[9] -ne 2 -or $capture[10] -ne 0 -or $capture[11] -ne 0 -or $capture[12] -ne 0) { throw 'Exported PNG is not an exact non-interlaced 8-bit RGB image.' }
                $headerSeen = $true
            } elseif ($name -ceq 'IHDR') { throw 'Exported PNG contains a duplicate IHDR chunk.'
            } elseif ($name -ceq 'IDAT') { $imageDataSeen = $true
            } elseif ($name -ceq 'pHYs') {
                if ($imageDataSeen) { throw 'Exported PNG pHYs chunk must precede the first IDAT chunk.' }
                $physicalCount++
                if ($length -ne 9) { throw 'Exported PNG pHYs chunk length is invalid.' }
                $physicalX = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($capture,0)); $physicalY = [Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($capture,4)); $physicalUnit = [int]$capture[8]
            }
            if ($name -ceq 'IEND') { if ($length -ne 0 -or $stream.Position -ne $stream.Length) { throw 'Exported PNG terminal chunk is invalid.' }; $ended = $true; break }
        }
        if (-not $ended -or $physicalCount -ne 1) { throw 'Exported PNG must contain exactly one pHYs chunk.' }
        if ($physicalX -ne $script:ImagePagePins.ExpectedPixelsPerMeter -or $physicalY -ne $script:ImagePagePins.ExpectedPixelsPerMeter -or $physicalUnit -ne 1) { throw 'Exported PNG pHYs density is not the exact 150 DPI metre value.' }
    } finally { $stream.Dispose() }
    Add-Type -AssemblyName System.Drawing
    $image = $null; $bitmap = $null
    try {
        $image = [Drawing.Image]::FromFile($Path,$true)
        if ($image.RawFormat.Guid -ne [Drawing.Imaging.ImageFormat]::Png.Guid -or $image.Width -ne $width -or $image.Height -ne $height) { throw 'Exported PNG decoder dimensions or format disagreed with its exact header.' }
        $bitmap = [Drawing.Bitmap]::new($image)
        $colors = [Collections.Generic.HashSet[int]]::new()
        foreach ($xRatio in @(0.35,0.45,0.55,0.65)) {
            foreach ($yRatio in @(0.4,0.5,0.6)) {
                $x = [Math]::Min($width - 1,[Math]::Max(0,[int][Math]::Floor($width * $xRatio)))
                $y = [Math]::Min($height - 1,[Math]::Max(0,[int][Math]::Floor($height * $yRatio)))
                $null = $colors.Add($bitmap.GetPixel($x,$y).ToArgb())
            }
        }
        if ($colors.Count -lt 2 -or ($colors.Count -eq 1 -and $colors.Contains([Drawing.Color]::White.ToArgb()))) { throw 'Exported PNG did not preserve the deterministic non-uniform source image on the page.' }
    } finally {
        if ($bitmap) { $bitmap.Dispose() }
        if ($image) { $image.Dispose() }
    }
    return [pscustomobject]@{ bytes=[uint64]$item.Length;sha256=Get-ExactSha256 -Path $Path;format='png';width=$width;height=$height;dpi=[int]$script:ImagePagePins.ExpectedExportDpi;pixelsPerMeter=$physicalX }
}

function Invoke-ImagePageNativeSave {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)][object[]]$NativeBaseline,
        [Parameter(Mandatory = $true)][string]$OutputPath,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $surface = Wait-NewProcessNativeWindowSurface -ProcessId $ProcessId -Baseline $NativeBaseline -AnchorNames @('Save') -AnchorControlTypes @('ControlType.Button') -Stage 'save-output-dialog' -Deadline $Deadline
    $roles = Get-ValidatedBindingNativeRoles -Binding $surface
    if ($null -eq $roles -or $null -eq $roles.PSObject.Properties['save']) { throw 'Image page-tools Save dialog did not expose exact native roles.' }
    $remaining = [int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline - [datetime]::UtcNow).TotalMilliseconds)))
    $deadlineTick = [Environment]::TickCount64 + $remaining
    Set-BoundNativeSaveFileNameExact -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Binding $surface -Value $OutputPath -DeadlineTickCount $deadlineTick
    Invoke-BoundNativeSaveButtonExact -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Binding $surface -DeadlineTickCount $deadlineTick
    Wait-BoundProcessUiSurfaceClosed -ProcessId $ProcessId -Binding $surface -Stage 'save-output-dialog' -Deadline $Deadline -ProcessStartUtcTicks $ProcessStartUtcTicks -ExpectedNativeSaveFileName $OutputPath -ActionTransport 'native-bm-click-delivered'
    return $true
}

function Invoke-ImagePageCreateNativeFlow {
    param(
        [Parameter(Mandatory = $true)][string]$SessionId,
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)][string]$SourceImagePath,
        [Parameter(Mandatory = $true)][string]$CreatedPdfPath,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $baselineSurfaces = @(Get-ReadingProcessUiSurfaceSnapshot -ApplicationProcessId $ProcessId -ApplicationProcessStartUtcTicks $ProcessStartUtcTicks -Deadline $Deadline)
    $baselineSurfaceIdentities = Assert-ReadingSurfaceSnapshot -Snapshot $baselineSurfaces -ApplicationProcessId $ProcessId -Kind 'The pre-create top-level UI baseline'
    $baselineTargets = @(Get-ReadingPickerTargetSnapshot -Surfaces $baselineSurfaces -ApplicationProcessId $ProcessId -ApplicationProcessStartUtcTicks $ProcessStartUtcTicks -Deadline $Deadline)
    $null = Assert-ReadingPickerTargetSnapshot -Snapshot $baselineTargets -SurfaceIdentities $baselineSurfaceIdentities -ApplicationProcessId $ProcessId -Kind 'The pre-create picker target baseline'
    $nativeBaseline = @(Get-ProcessNativeWindowSnapshot -ProcessId $ProcessId -Deadline $Deadline -TopLevelOnly)
    $clicked = Invoke-ImagePageWebDriverScript -SessionId $SessionId -Deadline $Deadline -Stage 'submit-create' -Script "const d=document.querySelector('dialog[aria-labelledby=`"create-pdf-title`"]');const b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Create PDF'&&!x.disabled):[];if(b.length===1)setTimeout(()=>b[0].click(),0);return b.length===1;"
    if ($clicked -isnot [bool] -or -not $clicked) { throw 'The exact Create PDF submit control was unavailable.' }
    $picker = Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId $ProcessId -ApplicationProcessStartUtcTicks $ProcessStartUtcTicks -BaselineSurfaces $baselineSurfaces -BaselineTargets $baselineTargets -Deadline $Deadline
    Submit-ProcessBoundOpenDialog -Binding $picker -ApplicationProcessId $ProcessId -ApplicationProcessStartUtcTicks $ProcessStartUtcTicks -Path $SourceImagePath -Deadline $Deadline
    $saveVerified = Invoke-ImagePageNativeSave -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -NativeBaseline $nativeBaseline -OutputPath $CreatedPdfPath -Deadline $Deadline
    if ([ReadingNativePickerApi]::IsWindow([IntPtr]$picker.filenameHandle) -or [ReadingNativePickerApi]::IsWindow([IntPtr]$picker.openButtonHandle)) { throw 'The bound source-image picker targets remained after the Save transition.' }
    return [pscustomobject]@{ sourcePickerControlCategory=[string]$picker.filenameControlCategory;createSaveDialogVerified=[bool]$saveVerified }
}

function Invoke-ImagePageExportNativeFlow {
    param(
        [Parameter(Mandatory = $true)][string]$SessionId,
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)][string]$ExportedPngPath,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $nativeBaseline = @(Get-ProcessNativeWindowSnapshot -ProcessId $ProcessId -Deadline $Deadline -TopLevelOnly)
    $clicked = Invoke-ImagePageWebDriverScript -SessionId $SessionId -Deadline $Deadline -Stage 'submit-export' -Script "const d=document.querySelector('dialog[aria-labelledby=`"export-image-title`"]');const b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Export PNG'&&!x.disabled):[];if(b.length===1)setTimeout(()=>b[0].click(),0);return b.length===1;"
    if ($clicked -isnot [bool] -or -not $clicked) { throw 'The exact Export PNG submit control was unavailable.' }
    return Invoke-ImagePageNativeSave -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -NativeBaseline $nativeBaseline -OutputPath $ExportedPngPath -Deadline $Deadline
}

function Invoke-FreshInstalledImagePageToolsSession {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,
        [Parameter(Mandatory = $true)][string]$TauriDriverPath,
        [Parameter(Mandatory = $true)][string]$EdgeDriverPath,
        [Parameter(Mandatory = $true)][string]$ProfileRoot,
        [Parameter(Mandatory = $true)][string]$SettingsRoot,
        [Parameter(Mandatory = $true)][string]$ExpectedEdgeDriverVersion,
        [Parameter(Mandatory = $true)][scriptblock]$SessionCallback
    )
    $driverCapture = $null; $driver = $null; $sessionId = $null; $captured = @(); $result = $null
    $sessionDeleteOutcome = 'requestfailed'; $driverExited = $false; $driverStopOutcome = 'not-invoked'; $processesQuiescent = $false; $residualCategory = 'multiple'
    $capturedOutcomes = [pscustomobject]@{ application='absent';tauriDriver='absent';edgeDriver='absent';webview='absent';ocrEngine='absent';other='absent' }
    $residualFacts = [pscustomobject]@{ ownership='none';application=$false;tauriDriver=$false;edgeDriver=$false;webview=$false;ocrEngine=$false;other=$false }
    try {
        $null = Wait-FixedWebDriverPortsFree -Deadline ([datetime]::UtcNow.AddMilliseconds($script:PrintPins.PortHandoffTimeoutMilliseconds))
        $deadline = [datetime]::UtcNow.AddMilliseconds($script:ImagePagePins.TotalTimeoutMilliseconds)
        $startedAfter = [datetime]::UtcNow
        $driverCapture = Start-BoundedDiscardProcess -Path $TauriDriverPath -Arguments @("--port=$($script:LaunchPins.WebDriverPort)","--native-port=$($script:LaunchPins.NativeDriverPort)","--native-driver=$EdgeDriverPath") -Environment (Get-MinimalWindowsProcessEnvironment)
        $driverCapture.Start(); $driver = $driverCapture.Process
        $status = $null
        do {
            try { $status = Invoke-BoundedLoopbackJson -Method GET -Path '/status' -Deadline $deadline } catch { }
            if ($null -ne $status -and [bool]$status.value.ready) { break }
            if ($driver.HasExited) { throw 'Pinned tauri-driver exited before image page-tools readiness.' }
            Start-Sleep -Milliseconds 200
        } while ([datetime]::UtcNow -lt $deadline)
        if ($null -eq $status -or -not [bool]$status.value.ready) { throw 'Pinned tauri-driver did not become ready for image page-tools verification.' }
        $nativeVersion = Wait-NativeDriverStatus -ExpectedVersion $ExpectedEdgeDriverVersion -Deadline $deadline -TauriDriver $driver
        $sessionBody = [ordered]@{ capabilities=[ordered]@{ alwaysMatch=[ordered]@{ browserName='wry';'tauri:options'=[ordered]@{application=$ApplicationPath;args=@();webviewOptions=[ordered]@{userDataFolder=$ProfileRoot}} } } }
        $session = Invoke-BoundedLoopbackJson -Method POST -Path '/session' -Body $sessionBody -Deadline $deadline
        if ([string]$session.value.sessionId -cnotmatch '^[A-Za-z0-9-]+$') { throw 'WebDriver did not return a bounded image page-tools session identifier.' }
        $sessionId = [string]$session.value.sessionId
        $capabilities = $session.value.capabilities; $runtimeVersion = [string]$capabilities.browserVersion
        $vendor = $capabilities.PSObject.Properties['msedge.msedgedriverVersion']
        if ($null -ne $vendor -and ([string]$vendor.Value -split '\s+')[0] -cne $nativeVersion) { throw 'Image page-tools session EdgeDriver version disagrees with native status.' }
        $userData = $capabilities.PSObject.Properties['msedge.userDataDir']
        $ownedApp = Wait-OwnedPrintApplication -Driver $driver -StartedAfter $startedAfter -ApplicationPath $ApplicationPath -Deadline $deadline
        $appProcessId = [int]$ownedApp.ProcessId; $appProcessStartUtcTicks = [long]$ownedApp.ProcessStartUtcTicks; $captured += @($ownedApp.Owned)
        $result = & $SessionCallback ([pscustomobject]@{
            SessionId=$sessionId;Deadline=$deadline;ApplicationProcessId=$appProcessId;ApplicationProcessStartUtcTicks=$appProcessStartUtcTicks
        })
        if ($null -eq $result) { throw 'Installed fresh-session callback returned no result.' }
        $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
        Assert-PrintOwnedExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -RootProcessId $driver.Id
        $profileBinding = if ($null -ne $userData -and -not [string]::IsNullOrWhiteSpace([string]$userData.Value)) { 'session-capability-' + (Get-ExactProfileBinding -Candidate ([string]$userData.Value) -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot) } else { Get-OwnedProfileBinding -Owned $captured -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot }
        if ($driverCapture.Exceeded) { throw 'WebDriver diagnostic output exceeded its discarded character cap.' }
        $sessionResult = [ordered]@{nativeDriverVersion=$nativeVersion;returnedRuntimeVersion=$runtimeVersion;profileBinding=$profileBinding}
        foreach ($property in $result.PSObject.Properties) { $sessionResult.Add($property.Name,$property.Value) }
        $result = [pscustomobject]$sessionResult
    } finally {
        $cleanupDeadline = [datetime]::UtcNow.AddSeconds(10)
        if ($sessionId) { $sessionDeleteOutcome = Invoke-SessionDeleteOutcome -SessionId $sessionId -Deadline $cleanupDeadline }
        if ($driver) {
            try { $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured) } catch { }
            $processCleanupDeadline = [datetime]::UtcNow.AddMilliseconds($script:LaunchPins.CleanupProcessTimeoutMilliseconds)
            $capturedOutcomes = Stop-OwnedLaunchProcesses -TauriDriver $driver -Captured $captured -Deadline $processCleanupDeadline
            $driverStopOutcome = [string]$capturedOutcomes.rootOutcome; $driverExited = [bool]$driver.HasExited
            $quiescence = Wait-LaunchProcessQuiescence -Deadline $processCleanupDeadline; $processesQuiescent = [bool]$quiescence.stable
            $remaining = @($quiescence.processes); $residualCategory = Get-LaunchResidualCategory -Processes $remaining
            $residualFacts = Get-LaunchResidualFacts -Processes $remaining -Captured $captured
        }
        if ($driverCapture) { $driverCapture.Dispose() }
    }
    $clear = $processesQuiescent -and $residualCategory -ceq 'none'
    $assertedDeleteOutcome = $sessionDeleteOutcome
    if ($sessionDeleteOutcome -cne 'verified' -and $null -ne $result -and $null -ne $result.PSObject.Properties['abruptTermination'] -and $result.abruptTermination -is [bool] -and $result.abruptTermination) { $assertedDeleteOutcome = 'verified' }
    Assert-LaunchCleanupState -Result $result -SessionDeleteOutcome $assertedDeleteOutcome -DriverExited $driverExited -RelevantProcessesClear $clear -DriverStopOutcome $driverStopOutcome -ResidualCategory $residualCategory -CapturedApplication $capturedOutcomes.application -CapturedTauriDriver $capturedOutcomes.tauriDriver -CapturedEdgeDriver $capturedOutcomes.edgeDriver -CapturedWebView $capturedOutcomes.webview -CapturedOcrEngine $capturedOutcomes.ocrEngine -CapturedOther $capturedOutcomes.other -ResidualOwnership $residualFacts.ownership -ResidualApplication $residualFacts.application -ResidualTauriDriver $residualFacts.tauriDriver -ResidualEdgeDriver $residualFacts.edgeDriver -ResidualWebView $residualFacts.webview -ResidualOcrEngine $residualFacts.ocrEngine -ResidualOther $residualFacts.other
    $result | Add-Member sessionDeleted ($sessionDeleteOutcome -ceq 'verified')
    $result | Add-Member ownedProcessTreeStopped $driverExited
    $result | Add-Member relevantProcessesRemaining 0
    return $result
}

function Invoke-RealInstalledImagePageTools {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,
        [Parameter(Mandatory = $true)][string]$PdfiumPath,
        [Parameter(Mandatory = $true)][string]$TauriDriverPath,
        [Parameter(Mandatory = $true)][string]$EdgeDriverPath,
        [Parameter(Mandatory = $true)][string]$ProfileRoot,
        [Parameter(Mandatory = $true)][string]$SettingsRoot,
        [Parameter(Mandatory = $true)][string]$SourceImagePath,
        [Parameter(Mandatory = $true)][string]$CreatedPdfPath,
        [Parameter(Mandatory = $true)][string]$ExportedPngPath,
        [Parameter(Mandatory = $true)][string]$ExpectedEdgeDriverVersion
    )
    $sessionCallback = {
        param($context)
        $sessionId = [string]$context.SessionId
        $deadline = [datetime]$context.Deadline
        $appProcessId = [int]$context.ApplicationProcessId
        $appProcessStartUtcTicks = [long]$context.ApplicationProcessStartUtcTicks
        $null = Wait-ImagePageWebDriverOracle -SessionId $sessionId -Script "return {ready:document.readyState==='complete',title:document.title,create:[...document.querySelectorAll('button')].filter(x=>x.title==='Create a PDF from an image').length};" -Deadline $deadline -Stage 'home' -Predicate { param($v) [bool]$v.ready -and [string]$v.title -ceq 'PDF Workstation' -and [int]$v.create -eq 1 }
        $createOpened = Invoke-ImagePageWebDriverScript -SessionId $sessionId -Deadline $deadline -Stage 'open-create-dialog' -Script "const b=[...document.querySelectorAll('button')].filter(x=>x.title==='Create a PDF from an image');if(b.length===1)b[0].click();return b.length===1;"
        if ($createOpened -isnot [bool] -or -not $createOpened) { throw 'The exact installed Create control was unavailable.' }
        $createDefaults = Wait-ImagePageWebDriverOracle -SessionId $sessionId -Script "const d=document.querySelector('dialog[aria-labelledby=`"create-pdf-title`"]');const s=d?.querySelector('select[aria-label=`"Page size`"]');const o=d?.querySelector('select[aria-label=`"Orientation`"]');const m=d?.querySelector('input[aria-label=`"Margin in points`"]');return {dialog:!!d,pageSize:s?.value||'',orientation:o?.value||'',margin:m?.value||''};" -Deadline $deadline -Stage 'create-defaults' -Predicate { param($v) [bool]$v.dialog -and [string]$v.pageSize -ceq 'letter' -and [string]$v.orientation -ceq 'auto' -and [string]$v.margin -ceq '36' }
        $nativeDeadline = Get-ImagePagePhaseDeadline -TotalDeadline $deadline -MaximumMilliseconds $script:ImagePagePins.NativeDialogTimeoutMilliseconds
        $createNative = Invoke-ImagePageCreateNativeFlow -SessionId $sessionId -ProcessId $appProcessId -ProcessStartUtcTicks $appProcessStartUtcTicks -SourceImagePath $SourceImagePath -CreatedPdfPath $CreatedPdfPath -Deadline $nativeDeadline
        $createdItem = Wait-ImagePageStableFile -Path $CreatedPdfPath -Deadline (Get-ImagePagePhaseDeadline -TotalDeadline $deadline -MaximumMilliseconds $script:ImagePagePins.OutputTimeoutMilliseconds)
        $createdProof = Get-PdfiumPrintProof -PdfiumPath $PdfiumPath -PdfPath $CreatedPdfPath
        if ([int]$createdProof.Pages -ne 1 -or [Math]::Abs([double]$createdProof.WidthPoints - $script:ImagePagePins.ExpectedPdfWidthPoints) -gt 0.01 -or [Math]::Abs([double]$createdProof.HeightPoints - $script:ImagePagePins.ExpectedPdfHeightPoints) -gt 0.01) { throw 'Created image PDF did not parse as one exact Letter page.' }
        $createdRaster = Get-ImagePagePdfiumRasterProof -PdfiumPath $PdfiumPath -PdfPath $CreatedPdfPath
        $workspace = Wait-ImagePageWebDriverOracle -SessionId $sessionId -Script "const i=document.querySelector('img[alt=`"Page 1`"]');return {createDialog:document.querySelectorAll('dialog[aria-labelledby=`"create-pdf-title`"]').length,pages:[...document.querySelectorAll('span')].filter(x=>x.textContent.trim()==='/ 1').length,image:!!i&&i.complete&&i.naturalWidth>0&&i.naturalHeight>0&&i.src.startsWith('blob:')};" -Deadline $deadline -Stage 'created-workspace' -Predicate { param($v) [int]$v.createDialog -eq 0 -and [int]$v.pages -eq 1 -and [bool]$v.image }
        $menuOpened = Invoke-ImagePageWebDriverScript -SessionId $sessionId -Deadline $deadline -Stage 'open-menu' -Script "const b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Menu');if(b.length===1)b[0].click();return b.length===1;"
        if ($menuOpened -isnot [bool] -or -not $menuOpened) { throw 'The installed Menu control was unavailable for image export.' }
        $exportOpened = Invoke-ImagePageWebDriverScript -SessionId $sessionId -Deadline $deadline -Stage 'open-export-dialog' -Script "const b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Export page as image…');if(b.length===1)b[0].click();return b.length===1;"
        if ($exportOpened -isnot [bool] -or -not $exportOpened) { throw 'The exact Export page as image menu control was unavailable.' }
        $exportDefaults = Wait-ImagePageWebDriverOracle -SessionId $sessionId -Script "const d=document.querySelector('dialog[aria-labelledby=`"export-image-title`"]');const f=d?.querySelector('select[aria-label=`"Image format`"]');const r=d?.querySelector('select[aria-label=`"Image resolution`"]');return {dialog:!!d,format:f?.value||'',dpi:r?.value||''};" -Deadline $deadline -Stage 'export-defaults' -Predicate { param($v) [bool]$v.dialog -and [string]$v.format -ceq 'png' -and [string]$v.dpi -ceq '150' }
        $nativeDeadline = Get-ImagePagePhaseDeadline -TotalDeadline $deadline -MaximumMilliseconds $script:ImagePagePins.NativeDialogTimeoutMilliseconds
        $exportSaveVerified = Invoke-ImagePageExportNativeFlow -SessionId $sessionId -ProcessId $appProcessId -ProcessStartUtcTicks $appProcessStartUtcTicks -ExportedPngPath $ExportedPngPath -Deadline $nativeDeadline
        $null = Wait-ImagePageStableFile -Path $ExportedPngPath -Deadline (Get-ImagePagePhaseDeadline -TotalDeadline $deadline -MaximumMilliseconds $script:ImagePagePins.OutputTimeoutMilliseconds)
        $pngProof = Get-ImagePagePngProof -Path $ExportedPngPath
        if ([int]$pngProof.width -ne $script:ImagePagePins.ExpectedExportWidth -or [int]$pngProof.height -ne $script:ImagePagePins.ExpectedExportHeight) { throw 'Exported PNG dimensions did not match the exact Letter-page 150 DPI request.' }
        $rasterComparison = [ImagePagePdfiumRasterProof]::ComparePng($createdRaster.Rgb,$ExportedPngPath,[int]$pngProof.width,[int]$pngProof.height)
        if ([double]$rasterComparison.MeanChannelDifference -gt $script:ImagePagePins.RasterMeanChannelDifferenceMaximum -or [int]$rasterComparison.MaximumChannelDifference -gt $script:ImagePagePins.RasterMaximumChannelDifference -or [double]$rasterComparison.DifferentChannelFraction -gt $script:ImagePagePins.RasterDifferentChannelFractionMaximum) { throw 'Exported PNG pixels differ materially from the independent signed-PDFium raster of the created PDF.' }
        $exportComplete = Wait-ImagePageWebDriverOracle -SessionId $sessionId -Script "const d=document.querySelector('dialog[aria-labelledby=`"export-image-title`"]');return {title:d?.querySelector('h2')?.textContent||'',summary:[...(d?.querySelectorAll('p')||[])].some(x=>x.textContent.includes('pixel PNG at 150 DPI.')),close:[...(d?.querySelectorAll('button')||[])].filter(x=>x.textContent.trim()==='Close').length};" -Deadline $deadline -Stage 'export-completion' -Predicate { param($v) [string]$v.title -ceq 'Image export complete' -and [bool]$v.summary -and [int]$v.close -eq 1 }
        return [pscustomobject]@{
            sourcePickerControlCategory=[string]$createNative.sourcePickerControlCategory;createSaveDialogVerified=[bool]$createNative.createSaveDialogVerified
            createdPdf=[pscustomobject]@{bytes=[uint64]$createdItem.Length;sha256=Get-ExactSha256 -Path $CreatedPdfPath;pages=[int]$createdProof.Pages;widthPoints=[Math]::Round([double]$createdProof.WidthPoints,3);heightPoints=[Math]::Round([double]$createdProof.HeightPoints,3);workspaceOpened=[bool]$workspace.image}
            exportSaveDialogVerified=[bool]$exportSaveVerified
            exportedImage=[pscustomobject]@{bytes=[uint64]$pngProof.bytes;sha256=[string]$pngProof.sha256;format='png';width=[int]$pngProof.width;height=[int]$pngProof.height;dpi=[int]$pngProof.dpi;uiCompleted=([string]$exportComplete.title -ceq 'Image export complete')}
        }
    }
    return Invoke-FreshInstalledImagePageToolsSession -ApplicationPath $ApplicationPath -TauriDriverPath $TauriDriverPath -EdgeDriverPath $EdgeDriverPath -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -ExpectedEdgeDriverVersion $ExpectedEdgeDriverVersion -SessionCallback $sessionCallback
}

function Invoke-InstalledImagePageTools {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,[Parameter(Mandatory = $true)]$ApplicationReceipt,
        [Parameter(Mandatory = $true)][string]$PdfiumPath,[Parameter(Mandatory = $true)]$PdfiumReceipt,
        [Parameter(Mandatory = $true)][string]$WebDriverRoot,[Parameter(Mandatory = $true)][string]$ProfileRoot,
        [Parameter(Mandatory = $true)][string]$SettingsRoot,[Parameter(Mandatory = $true)][string]$SourceImagePath,
        [Parameter(Mandatory = $true)]$SourceImageReceipt,[Parameter(Mandatory = $true)][string]$CreatedPdfPath,
        [Parameter(Mandatory = $true)][string]$ExportedPngPath,[Parameter(Mandatory = $true)][string]$ExpectedPublisher,
        [scriptblock]$ProcessProvider
    )
    Assert-ImagePageFileReceipt -Path $ApplicationPath -Bytes ([uint64]$ApplicationReceipt.bytes) -Sha256 ([string]$ApplicationReceipt.sha256) -Kind 'Installed signed image page-tools application'
    $null = Assert-TrustedWindowsSignature -Path $ApplicationPath -ExpectedPublisher $ExpectedPublisher
    Assert-ImagePageFileReceipt -Path $PdfiumPath -Bytes ([uint64]$PdfiumReceipt.bytes) -Sha256 ([string]$PdfiumReceipt.sha256) -Kind 'Installed signed image page-tools PDFium'
    $null = Assert-TrustedWindowsSignature -Path $PdfiumPath -ExpectedPublisher $ExpectedPublisher
    Assert-ImagePageFileReceipt -Path $SourceImagePath -Bytes ([uint64]$SourceImageReceipt.bytes) -Sha256 ([string]$SourceImageReceipt.sha256) -Kind 'Image page-tools source PNG'
    foreach ($path in @($SourceImagePath,$CreatedPdfPath,$ExportedPngPath)) { if (-not [IO.Path]::IsPathFullyQualified($path)) { throw 'Image page-tools fixture and output paths must be fully qualified.' } }
    if ([IO.Path]::GetExtension($SourceImagePath) -cne '.png' -or [IO.Path]::GetExtension($CreatedPdfPath) -cne '.pdf' -or [IO.Path]::GetExtension($ExportedPngPath) -cne '.png') { throw 'Image page-tools paths do not have the exact PNG, PDF, and PNG extensions.' }
    $distinctPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    foreach ($path in @($SourceImagePath,$CreatedPdfPath,$ExportedPngPath)) { if (-not $distinctPaths.Add([IO.Path]::GetFullPath($path))) { throw 'Image page-tools fixture and output paths must be distinct.' } }
    $receipt = Get-Content -LiteralPath (Join-Path $WebDriverRoot 'webdriver-receipt.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-WebDriverReceipt -Receipt $receipt
    $tauriDriver = Join-Path $WebDriverRoot 'tauri-driver-install/bin/tauri-driver.exe'; $edgeDriver = Join-Path $WebDriverRoot 'edge-driver/msedgedriver.exe'
    Assert-ImagePageFileReceipt -Path $tauriDriver -Bytes ([uint64]$receipt.tauriDriver.bytes) -Sha256 ([string]$receipt.tauriDriver.sha256) -Kind 'Pinned image page-tools tauri-driver'
    Assert-ImagePageFileReceipt -Path $edgeDriver -Bytes ([uint64]$receipt.edgeDriver.bytes) -Sha256 ([string]$receipt.edgeDriver.sha256) -Kind 'Exact image page-tools EdgeDriver'
    $null = Assert-TrustedWindowsSignature -Path $edgeDriver -ExpectedPublisher $script:LaunchPins.EdgePublisher
    $expectedSettings = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\')
    $actualSettings = [IO.Path]::GetFullPath($SettingsRoot).TrimEnd('\')
    if (-not $actualSettings.Equals($expectedSettings,[StringComparison]::OrdinalIgnoreCase)) { throw 'Image page-tools settings root does not match the exact application identity.' }
    Assert-NoReparseAncestors -Path $actualSettings
    if (-not (Test-Path -LiteralPath $actualSettings -PathType Container) -or @(Get-ChildItem -LiteralPath $actualSettings -Force).Count -ne 0) { throw 'Image page-tools settings root must be an empty controlled directory before launch.' }
    if (Test-Path -LiteralPath $ProfileRoot) { throw 'Image page-tools WebView profile must be fresh.' }
    [IO.Directory]::CreateDirectory($ProfileRoot) | Out-Null; Assert-NoReparseAncestors -Path $ProfileRoot
    if (@(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -ne 0) { throw 'Image page-tools WebView profile was not empty.' }
    foreach ($output in @($CreatedPdfPath,$ExportedPngPath)) {
        $parent = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($output))
        if (-not (Test-Path -LiteralPath $parent -PathType Container)) { throw 'An image page-tools output parent directory is missing.' }
        Assert-NoReparseAncestors -Path $parent
        if (Test-Path -LiteralPath $output) { throw 'Image page-tools output paths must be fresh.' }
    }
    if (@(Get-LaunchProcessSnapshot).Count -ne 0) { throw 'A relevant application or WebDriver process existed before image page-tools verification.' }
    $result = if ($ProcessProvider) { & $ProcessProvider $ApplicationPath $PdfiumPath $tauriDriver $edgeDriver $ProfileRoot $SettingsRoot $SourceImagePath $CreatedPdfPath $ExportedPngPath ([string]$receipt.edgeDriver.version) } else { Invoke-RealInstalledImagePageTools -ApplicationPath $ApplicationPath -PdfiumPath $PdfiumPath -TauriDriverPath $tauriDriver -EdgeDriverPath $edgeDriver -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -SourceImagePath $SourceImagePath -CreatedPdfPath $CreatedPdfPath -ExportedPngPath $ExportedPngPath -ExpectedEdgeDriverVersion ([string]$receipt.edgeDriver.version) }
    Assert-ImagePageExactProperties -Value $result -Expected @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','sourcePickerControlCategory','createSaveDialogVerified','createdPdf','exportSaveDialogVerified','exportedImage','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') -Kind 'Installed image page-tools result'
    Assert-ImagePageExactProperties -Value $result.createdPdf -Expected @('bytes','sha256','pages','widthPoints','heightPoints','workspaceOpened') -Kind 'Created image PDF result'
    Assert-ImagePageExactProperties -Value $result.exportedImage -Expected @('bytes','sha256','format','width','height','dpi','uiCompleted') -Kind 'Exported page image result'
    $runtime = ([string]$result.returnedRuntimeVersion).Split('.'); $expectedRuntime = ([string]$receipt.webView2RuntimeVersion).Split('.')
    if ([string]$result.nativeDriverVersion -cne [string]$receipt.edgeDriver.version -or $runtime.Count -ne 4 -or ($runtime[0..2] -join '.') -cne ($expectedRuntime[0..2] -join '.') -or
        [string]$result.profileBinding -cnotmatch '^(session-capability-|owned-webview-)(requested-profile|requested-ebwebview|tauri-app-settings-ebwebview)$' -or
        [string]$result.sourcePickerControlCategory -cnotmatch '^edit-(1001|1148)$' -or -not [bool]$result.createSaveDialogVerified -or -not [bool]$result.exportSaveDialogVerified -or
        -not [bool]$result.createdPdf.workspaceOpened -or [int]$result.createdPdf.pages -ne 1 -or [double]$result.createdPdf.widthPoints -ne $script:ImagePagePins.ExpectedPdfWidthPoints -or [double]$result.createdPdf.heightPoints -ne $script:ImagePagePins.ExpectedPdfHeightPoints -or
        [string]$result.exportedImage.format -cne 'png' -or [int]$result.exportedImage.width -ne $script:ImagePagePins.ExpectedExportWidth -or [int]$result.exportedImage.height -ne $script:ImagePagePins.ExpectedExportHeight -or [int]$result.exportedImage.dpi -ne $script:ImagePagePins.ExpectedExportDpi -or -not [bool]$result.exportedImage.uiCompleted -or
        -not [bool]$result.sessionDeleted -or -not [bool]$result.ownedProcessTreeStopped -or [int]$result.relevantProcessesRemaining -ne 0) { throw 'Installed image page-tools result did not satisfy its exact native picker, output, UI, runtime, and cleanup oracles.' }
    Assert-ImagePageFileReceipt -Path $CreatedPdfPath -Bytes ([uint64]$result.createdPdf.bytes) -Sha256 ([string]$result.createdPdf.sha256) -Kind 'Created image PDF output'
    Assert-ImagePageFileReceipt -Path $ExportedPngPath -Bytes ([uint64]$result.exportedImage.bytes) -Sha256 ([string]$result.exportedImage.sha256) -Kind 'Exported page PNG output'
    Assert-ImagePageFileReceipt -Path $SourceImagePath -Bytes ([uint64]$SourceImageReceipt.bytes) -Sha256 ([string]$SourceImageReceipt.sha256) -Kind 'Post-flow source PNG'
    $applicationProfile = Join-Path $actualSettings 'EBWebView'; $requestedChild = Join-Path $ProfileRoot 'EBWebView'
    $usesApplicationProfile = ([string]$result.profileBinding).EndsWith('tauri-app-settings-ebwebview',[StringComparison]::Ordinal)
    $usesRequestedChild = ([string]$result.profileBinding).EndsWith('requested-ebwebview',[StringComparison]::Ordinal)
    if ($usesApplicationProfile) {
        if (-not (Test-Path -LiteralPath $applicationProfile -PathType Container) -or @(Get-ChildItem -LiteralPath $applicationProfile -Force).Count -eq 0 -or @(Get-ChildItem -LiteralPath $actualSettings -Force).Count -ne 1 -or @(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -ne 0) { throw 'The image page-tools run did not exclusively populate its controlled application profile.' }
        Assert-NoReparseAncestors -Path $applicationProfile
    } elseif ($usesRequestedChild) {
        if (-not (Test-Path -LiteralPath $requestedChild -PathType Container) -or @(Get-ChildItem -LiteralPath $requestedChild -Force).Count -eq 0 -or @(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -ne 1 -or @(Get-ChildItem -LiteralPath $actualSettings -Force).Count -ne 0) { throw 'The image page-tools run did not exclusively populate its controlled requested child profile.' }
        Assert-NoReparseAncestors -Path $requestedChild
    } else {
        if (@(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -eq 0 -or @(Get-ChildItem -LiteralPath $actualSettings -Force).Count -ne 0) { throw 'The image page-tools run did not exclusively populate its exact requested profile.' }
    }
    return $result
}
