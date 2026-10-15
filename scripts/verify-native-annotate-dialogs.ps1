[CmdletBinding()]
param(
    [string]$AppPath,
    [string]$WorkRoot,
    [string]$FixtureDir,
    [string]$PdfiumPath,
    [string[]]$Scenario = @(),
    [int]$TimeoutSeconds = 30,
    [int]$WaitMinutes = 30,
    [switch]$DumpTrees,
    [switch]$Strict
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest
$Scenario = @($Scenario | ForEach-Object { $_ -split ',' } | Where-Object { $_ })
if ($WaitMinutes -lt 1 -or $WaitMinutes -gt 30) { throw 'WaitMinutes must be between 1 and 30.' }

# Drives the native Windows dialogs of SM3's features in a debug build, through UI Automation (and the MSAA actions of
# the same accessibility stack for the classic dialog controls). No keyboard or mouse input is injected.
# Fixtures: src-tauri/tests/fixtures/*.pdf and resources/welcome.pdf come from the repo. annotated.pdf, strict-text.pdf
# and strict-image.pdf are written by two ignored tests, run once with SM3_DIALOG_FIXTURE_DIR set to an existing folder:
#   cargo test --bins -- --ignored write_dialog_annotation_fixture
#   cargo test --test dialog_fixtures -- --ignored
$projectRoot = [IO.Path]::GetFullPath((Split-Path -Parent $PSScriptRoot))
function Get-DirPrefix { param([string]$Path) return ([IO.Path]::GetFullPath($Path).TrimEnd([char]92) + [char]92) }
function Test-UnderBase {
    param([string]$Path,[string[]]$Bases)
    $full = [IO.Path]::GetFullPath($Path)
    if ($full.StartsWith('\\')) { return $false }
    foreach ($base in $Bases) {
        if (-not $base) { continue }
        if ((Get-DirPrefix $full).StartsWith((Get-DirPrefix $base), [StringComparison]::OrdinalIgnoreCase)) { return $true }
    }
    return $false
}
$siblingRoot = $null
for ($ancestor = $projectRoot; $ancestor; $ancestor = Split-Path -Parent $ancestor) {
    if ((Test-Path -LiteralPath (Join-Path $ancestor 'tmp') -PathType Container) -and (Test-Path -LiteralPath (Join-Path $ancestor 'wt') -PathType Container)) { $siblingRoot = $ancestor; break }
}
if (-not $siblingRoot) { throw 'Could not find the Smacrobat tmp folder (a tmp folder next to wt) above the repo.' }
$tmpBase = [IO.Path]::GetFullPath((Join-Path $siblingRoot 'tmp'))
$sharedTarget = [IO.Path]::GetFullPath((Join-Path $siblingRoot 'target'))
$targetBases = @($sharedTarget, $(if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { $null }), (Join-Path $projectRoot 'src-tauri\target'))
if (-not $AppPath) {
    $targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $projectRoot 'src-tauri\target' }
    $AppPath = Join-Path $targetRoot 'debug\pdf-workstation.exe'
}
if (-not $PdfiumPath) { $PdfiumPath = Join-Path $projectRoot 'src-tauri\resources\pdfium\bin\pdfium.dll' }
if (-not $WorkRoot) { $WorkRoot = Join-Path $tmpBase ('sm3-dialogs\run-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
if (-not $FixtureDir) { $FixtureDir = Join-Path $tmpBase 'sm3-dialogs\fixtures' }
$WorkRoot = [IO.Path]::GetFullPath($WorkRoot)
$FixtureDir = [IO.Path]::GetFullPath($FixtureDir)
$AppPath = [IO.Path]::GetFullPath($AppPath)
$PdfiumPath = [IO.Path]::GetFullPath($PdfiumPath)
$systemDrive = [IO.Path]::GetPathRoot([Environment]::GetFolderPath('Windows'))
if ([IO.Path]::GetPathRoot($WorkRoot).Equals($systemDrive, [StringComparison]::OrdinalIgnoreCase)) { throw "WorkRoot must not be on the system drive ($systemDrive): $WorkRoot" }
if (-not (Test-UnderBase $WorkRoot @($tmpBase)) -or (Get-DirPrefix $WorkRoot) -eq (Get-DirPrefix $tmpBase)) { throw "WorkRoot must be a folder under the sibling tmp folder $(Get-DirPrefix $tmpBase) (no UNC or other drives): $WorkRoot" }
if (Test-Path -LiteralPath $WorkRoot) { throw "WorkRoot must be a fresh folder: $WorkRoot" }
if (-not (Test-UnderBase $FixtureDir @($tmpBase, $projectRoot))) { throw "FixtureDir must be under the sibling tmp folder or the repo: $FixtureDir" }
if (-not (Test-UnderBase $AppPath (@($projectRoot) + $targetBases))) { throw "AppPath must be under the repo or the shared cargo target: $AppPath" }
if (-not (Test-UnderBase $PdfiumPath @($projectRoot))) { throw "PdfiumPath must be under the repo: $PdfiumPath" }
if (-not (Test-Path -LiteralPath $AppPath -PathType Leaf)) { throw "App executable not found: $AppPath" }
if (-not (Test-Path -LiteralPath $PdfiumPath -PathType Leaf)) { throw "PDFium library not found: $PdfiumPath" }
$recoveryDir = Join-Path ([Environment]::GetFolderPath('ApplicationData')) 'local.pdfworkstation.desktop\recovery'
function Get-RecoveryRecords {
    if (-not (Test-Path -LiteralPath $recoveryDir -PathType Container)) { return @() }
    return @(Get-ChildItem -LiteralPath $recoveryDir -File -Filter '*.smacrec' | ForEach-Object { $_.FullName })
}
# The debug app shares the real app-data recovery folder with the installed app. Any record present before this run
# is not ours: refuse to run rather than risk answering or removing someone else's recovered edits.
$preexisting = @(Get-RecoveryRecords)
if ($preexisting.Count -gt 0) { throw "Refusing to run: $($preexisting.Count) recovery record(s) already exist in $recoveryDir. Open the app yourself and resolve them first; this verifier never reads, answers or removes records it did not create." }
foreach ($name in @('annotated.pdf', 'strict-text.pdf', 'strict-image.pdf')) {
    if (-not (Test-Path -LiteralPath (Join-Path $FixtureDir $name) -PathType Leaf)) { throw "Fixture missing: $(Join-Path $FixtureDir $name). See the header of this script for the two cargo commands that write it." }
}
$formFixtureDir = Join-Path $projectRoot 'src-tauri\tests\fixtures'

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
if (-not ('Sm3DialogNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;
public static class Sm3DialogNative {
    // Classic dialog controls expose no Invoke/Value pattern to the managed UI Automation client, so they are reached
    // through MSAA (IAccessible): accDoDefaultAction presses a button, accValue reads and sets an edit.
    [DllImport("oleacc.dll")] static extern int AccessibleObjectFromWindow(IntPtr hwnd, uint id, ref Guid iid, [MarshalAs(UnmanagedType.IDispatch)] out object accessible);
    static object Accessible(IntPtr hwnd) {
        Guid iid = new Guid("618736E0-3C3D-11CF-810C-00AA00389B71"); object accessible;
        int hr = AccessibleObjectFromWindow(hwnd, 0xFFFFFFFC, ref iid, out accessible);
        if (hr != 0 || accessible == null) throw new InvalidOperationException("The dialog control exposes no accessible object (hr=" + hr + ").");
        return accessible;
    }
    public static string MsaaGetValue(IntPtr hwnd) { object a = Accessible(hwnd); return Convert.ToString(a.GetType().InvokeMember("accValue", System.Reflection.BindingFlags.GetProperty, null, a, new object[] { 0 })); }
    public static void MsaaSetValue(IntPtr hwnd, string value) { object a = Accessible(hwnd); a.GetType().InvokeMember("accValue", System.Reflection.BindingFlags.SetProperty, null, a, new object[] { 0, value }); }
    public static void MsaaDoDefaultAction(IntPtr hwnd) { object a = Accessible(hwnd); a.GetType().InvokeMember("accDoDefaultAction", System.Reflection.BindingFlags.InvokeMethod, null, a, new object[] { 0 }); }
    [DllImport("user32.dll")] public static extern IntPtr GetWindow(IntPtr window, uint command);
    [DllImport("user32.dll")] public static extern bool IsWindowEnabled(IntPtr window);
    [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr LoadLibraryW(string path);
    [DllImport("kernel32.dll", CharSet = CharSet.Ansi, SetLastError = true)] static extern IntPtr GetProcAddress(IntPtr module, string name);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void VoidFn();
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr LoadDocumentFn(byte[] path, byte[] password);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int IntOfPtrFn(IntPtr a);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr PtrOfPtrIntFn(IntPtr a, int b);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr PtrOfPtrFn(IntPtr a);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void ClosePtrFn(IntPtr a);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int GetTextFn(IntPtr textPage, int start, int count, byte[] buffer);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate uint GetStringValueFn(IntPtr annot, byte[] key, byte[] buffer, uint length);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr BitmapCreateFn(int width, int height, int alpha);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void FillRectFn(IntPtr bitmap, int left, int top, int width, int height, uint color);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void RenderFn(IntPtr bitmap, IntPtr page, int startX, int startY, int sizeX, int sizeY, int rotate, int flags);
    static T Bind<T>(IntPtr module, string name) where T : class {
        IntPtr address = GetProcAddress(module, name);
        if (address == IntPtr.Zero) throw new InvalidOperationException("PDFium export missing: " + name);
        return Marshal.GetDelegateForFunctionPointer(address, typeof(T)) as T;
    }
    // Opens the file with PDFium, runs the body on the document, and always closes everything.
    static T WithDocument<T>(string pdfiumPath, string pdfPath, Func<IntPtr, IntPtr, T> body) {
        IntPtr module = LoadLibraryW(pdfiumPath);
        if (module == IntPtr.Zero) throw new InvalidOperationException("PDFium could not be loaded.");
        Bind<VoidFn>(module, "FPDF_InitLibrary")();
        try {
            IntPtr document = Bind<LoadDocumentFn>(module, "FPDF_LoadDocument")(Encoding.ASCII.GetBytes(pdfPath + "\0"), null);
            if (document == IntPtr.Zero) throw new InvalidOperationException("PDFium refused to open " + pdfPath);
            try { return body(module, document); }
            finally { Bind<ClosePtrFn>(module, "FPDF_CloseDocument")(document); }
        } finally { Bind<VoidFn>(module, "FPDF_DestroyLibrary")(); }
    }
    static T WithPage<T>(string pdfiumPath, string pdfPath, int pageIndex, Func<IntPtr, IntPtr, T> body) {
        return WithDocument<T>(pdfiumPath, pdfPath, (module, document) => {
            IntPtr page = Bind<PtrOfPtrIntFn>(module, "FPDF_LoadPage")(document, pageIndex);
            if (page == IntPtr.Zero) throw new InvalidOperationException("PDFium could not load page " + (pageIndex + 1));
            try { return body(module, page); }
            finally { Bind<ClosePtrFn>(module, "FPDF_ClosePage")(page); }
        });
    }
    public static int PageCount(string pdfiumPath, string pdfPath) {
        return WithDocument<int>(pdfiumPath, pdfPath, (module, document) => Bind<IntOfPtrFn>(module, "FPDF_GetPageCount")(document));
    }
    public static string PageText(string pdfiumPath, string pdfPath, int pageIndex) {
        return WithPage<string>(pdfiumPath, pdfPath, pageIndex, (module, page) => {
            IntPtr text = Bind<PtrOfPtrFn>(module, "FPDFText_LoadPage")(page);
            if (text == IntPtr.Zero) return "";
            try {
                int count = Bind<IntOfPtrFn>(module, "FPDFText_CountChars")(text);
                if (count <= 0) return "";
                byte[] buffer = new byte[(count + 1) * 2];
                int written = Bind<GetTextFn>(module, "FPDFText_GetText")(text, 0, count, buffer);
                return written <= 1 ? "" : Encoding.Unicode.GetString(buffer, 0, (written - 1) * 2);
            } finally { Bind<ClosePtrFn>(module, "FPDFText_ClosePage")(text); }
        });
    }
    static string AnnotString(IntPtr module, IntPtr annot, string key) {
        var get = Bind<GetStringValueFn>(module, "FPDFAnnot_GetStringValue");
        byte[] k = Encoding.ASCII.GetBytes(key + "\0");
        uint needed = get(annot, k, null, 0);
        if (needed <= 2) return "";
        byte[] buffer = new byte[needed];
        get(annot, k, buffer, needed);
        return Encoding.Unicode.GetString(buffer, 0, (int)needed - 2);
    }
    // One line per annotation: subtype|Contents|T|V|AS|NM (PDFium subtype numbers: 1 Text, 9 Highlight, 20 Widget).
    public static string[] Annotations(string pdfiumPath, string pdfPath, int pageIndex) {
        return WithPage<string[]>(pdfiumPath, pdfPath, pageIndex, (module, page) => {
            int count = Bind<IntOfPtrFn>(module, "FPDFPage_GetAnnotCount")(page);
            var lines = new List<string>();
            for (int i = 0; i < count; i++) {
                IntPtr annot = Bind<PtrOfPtrIntFn>(module, "FPDFPage_GetAnnot")(page, i);
                if (annot == IntPtr.Zero) { lines.Add("unreadable"); continue; }
                try {
                    int subtype = Bind<IntOfPtrFn>(module, "FPDFAnnot_GetSubtype")(annot);
                    lines.Add(subtype + "|" + AnnotString(module, annot, "Contents") + "|" + AnnotString(module, annot, "T") + "|" + AnnotString(module, annot, "V") + "|" + AnnotString(module, annot, "AS") + "|" + AnnotString(module, annot, "NM"));
                } finally { Bind<ClosePtrFn>(module, "FPDFPage_CloseAnnot")(annot); }
            }
            return lines.ToArray();
        });
    }
    // Renders the page at 1 pixel per point and returns "R,G,B" of the requested pixel.
    public static string RenderPixel(string pdfiumPath, string pdfPath, int pageIndex, int x, int y) {
        return WithPage<string>(pdfiumPath, pdfPath, pageIndex, (module, page) => {
            var width = Bind<PageSizeFn>(module, "FPDF_GetPageWidthF")(page);
            var height = Bind<PageSizeFn>(module, "FPDF_GetPageHeightF")(page);
            int w = (int)Math.Round(width), h = (int)Math.Round(height);
            IntPtr bitmap = Bind<BitmapCreateFn>(module, "FPDFBitmap_Create")(w, h, 0);
            try {
                Bind<FillRectFn>(module, "FPDFBitmap_FillRect")(bitmap, 0, 0, w, h, 0xFFFFFFFF);
                Bind<RenderFn>(module, "FPDF_RenderPageBitmap")(bitmap, page, 0, 0, w, h, 0, 0);
                IntPtr buffer = Bind<PtrOfPtrFn>(module, "FPDFBitmap_GetBuffer")(bitmap);
                int stride = Bind<IntOfPtrFn>(module, "FPDFBitmap_GetStride")(bitmap);
                byte[] pixel = new byte[4];
                Marshal.Copy(IntPtr.Add(buffer, y * stride + x * 4), pixel, 0, 4);
                return pixel[2] + "," + pixel[1] + "," + pixel[0];
            } finally { Bind<ClosePtrFn>(module, "FPDFBitmap_Destroy")(bitmap); }
        });
    }
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate float PageSizeFn(IntPtr page);
}
'@
}

$AE = [Windows.Automation.AutomationElement]
$script:Results = [Collections.Generic.List[object]]::new()
$script:AppPid = 0
$script:MainHwnd = [IntPtr]::Zero
$script:Process = $null
$script:Deadline = $TimeoutSeconds
$script:OpenItem = 'Open' + [char]0x2026 + '*'
$script:DiagDir = $null
$script:RunDir = $null
$script:OpenedByRun = @{}
$script:SawForeign = $false

function Add-Result {
    param([string]$Command,[string]$Check,[ValidateSet('pass','fail','not-automatable','finding','info')][string]$Status,[string]$Detail = '')
    $script:Results.Add([pscustomobject]@{ command = $Command; check = $Check; status = $Status; detail = $Detail })
    $mark = switch ($Status) { 'pass' { 'PASS' } 'fail' { 'FAIL' } 'finding' { 'FIND' } 'info' { 'INFO' } default { 'N/A ' } }
    Write-Host ("[{0}] {1} :: {2}{3}" -f $mark, $Command, $Check, $(if ($Detail) { ' - ' + $Detail } else { '' }))
}

function Assert-Check {
    param([string]$Command,[string]$Check,[bool]$Condition,[string]$Detail = '')
    if ($Condition) { Add-Result $Command $Check 'pass' $Detail } else { Add-Result $Command $Check 'fail' $Detail }
    return $Condition
}

function Assert-AppPid { if ($script:AppPid -le 4) { throw 'No verifier app process id is set (PID 0 and 4 are never driven).' } }

function Get-OtherDevApps {
    # Only other dev builds (a copy under the sibling tmp folder or an exe in a cargo target) count; an installed app does not.
    $found = @()
    foreach ($process in @(Get-Process -Name 'pdf-workstation' -ErrorAction SilentlyContinue)) {
        if ($process.Id -eq $script:AppPid) { continue }
        $path = $null
        try { $path = $process.Path } catch { }
        if ($path -and (Test-UnderBase $path (@($tmpBase, $projectRoot) + $targetBases))) { $found += $process }
    }
    return $found
}

function Assert-Exclusive {
    # Another dev build means another stream's verifier started. Abort this run so the two never share a desktop.
    $others = @(Get-OtherDevApps)
    if ($others.Count -gt 0) { $script:SawForeign = $true; throw ('SM3_FOREIGN_APP: another pdf-workstation dev build appeared (pid ' + (($others | ForEach-Object Id) -join ',') + ').') }
}

function Get-UiTreeText {
    param($Root,[int]$MaxDepth = 8)
    $builder = [Text.StringBuilder]::new()
    $walk = {
        param($Element,[int]$Depth)
        if ($Depth -gt $MaxDepth) { return }
        $c = $Element.Current
        [void]$builder.AppendLine(('  ' * $Depth) + ('{0}|name={1}|id={2}|cls={3}|enabled={4}' -f $c.ControlType.ProgrammaticName.Replace('ControlType.', ''), $c.Name, $c.AutomationId, $c.ClassName, $c.IsEnabled))
        if ($c.Name -ceq 'Items View' -or $c.ClassName -ceq 'Breadcrumb Parent') { return }
        foreach ($child in $Element.FindAll([Windows.Automation.TreeScope]::Children, [Windows.Automation.Condition]::TrueCondition)) { & $walk $child ($Depth + 1) }
    }
    & $walk $Root 0
    return $builder.ToString()
}

function Save-Diag {
    param([string]$Label,$Root)
    try {
        [IO.Directory]::CreateDirectory($script:DiagDir) | Out-Null
        $safe = ($Label -replace '[^A-Za-z0-9._-]', '_')
        $text = Get-UiTreeText -Root $Root
        if ($Root.Current.ClassName -ceq 'WRY_WEBVIEW') {
            $flat = foreach ($e in $Root.FindAll([Windows.Automation.TreeScope]::Descendants, [Windows.Automation.Condition]::TrueCondition)) {
                $c = $e.Current; $type = $c.ControlType.ProgrammaticName.Replace('ControlType.', '')
                if ($type -match '^(Button|Edit|ComboBox|Window|Text|CheckBox|RadioButton|List|ListItem|Document|Group|Image|Custom|Pane)$') { '{0}|{1}|id={2}|enabled={3}' -f $type, $c.Name, $c.AutomationId, $c.IsEnabled }
            }
            $text += "`n--- flat web content ---`n" + ($flat -join "`n")
        }
        [IO.File]::WriteAllText((Join-Path $script:DiagDir ($safe + '.txt')), $text, [Text.UTF8Encoding]::new($false))
    } catch { Write-Host ('Diag dump failed: ' + $_.Exception.Message) }
}

function Get-ControlType { param([string]$Name) return [Windows.Automation.ControlType]::$Name }

function Find-Ui {
    param($Root,[string]$Name,[string]$Type,[string]$Id,[switch]$Like,[switch]$All,[switch]$EnabledOnly)
    $conditions = [Collections.Generic.List[Windows.Automation.Condition]]::new()
    if ($Type) { $conditions.Add([Windows.Automation.PropertyCondition]::new($AE::ControlTypeProperty, (Get-ControlType $Type))) }
    if ($Id) { $conditions.Add([Windows.Automation.PropertyCondition]::new($AE::AutomationIdProperty, $Id)) }
    if ($Name -and -not $Like) { $conditions.Add([Windows.Automation.PropertyCondition]::new($AE::NameProperty, $Name)) }
    $condition = if ($conditions.Count -eq 0) { [Windows.Automation.Condition]::TrueCondition } elseif ($conditions.Count -eq 1) { $conditions[0] } else { [Windows.Automation.AndCondition]::new($conditions.ToArray()) }
    $found = @($Root.FindAll([Windows.Automation.TreeScope]::Descendants, $condition))
    if ($Name -and $Like) { $found = @($found | Where-Object { $_.Current.Name -like $Name }) }
    if ($EnabledOnly) { $found = @($found | Where-Object { $_.Current.IsEnabled }) }
    if ($All) { return $found }
    if ($found.Count -eq 0) { return $null }
    return $found[0]
}

function Wait-Ui {
    param($Root,[string]$Name,[string]$Type,[string]$Id,[switch]$Like,[switch]$EnabledOnly,[int]$Seconds = $script:Deadline,[switch]$Gone)
    $limit = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        Assert-Exclusive
        $element = Find-Ui -Root $Root -Name $Name -Type $Type -Id $Id -Like:$Like -EnabledOnly:$EnabledOnly
        if ($Gone) { if ($null -eq $element) { return $true } } elseif ($null -ne $element) { return $element }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $limit)
    if ($Gone) { return $false }
    throw "UI element not found within ${Seconds}s: type=$Type name=$Name id=$Id"
}

function Invoke-Ui {
    param($Element)
    $pattern = $null
    if ($Element.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke(); return }
    if ($Element.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) { $pattern.Select(); return }
    if ($Element.TryGetCurrentPattern([Windows.Automation.TogglePattern]::Pattern, [ref]$pattern)) { $pattern.Toggle(); return }
    if ($Element.TryGetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern, [ref]$pattern)) {
        if ($pattern.Current.ExpandCollapseState -eq [Windows.Automation.ExpandCollapseState]::Collapsed) { $pattern.Expand() } else { $pattern.Collapse() }
        return
    }
    throw ('Element exposes no Invoke/SelectionItem/Toggle pattern: ' + $Element.Current.Name)
}

function Set-UiValue {
    param($Element,[string]$Value)
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) { throw ('Element exposes no Value pattern: ' + $Element.Current.Name) }
    $pattern.SetValue($Value)
}

function Get-UiValue {
    param($Element)
    $pattern = $null
    if ($Element.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern, [ref]$pattern)) { return [string]$pattern.Current.Value }
    return $null
}

function Get-MainWindow {
    Assert-AppPid
    $condition = [Windows.Automation.AndCondition]::new(
        [Windows.Automation.PropertyCondition]::new($AE::ProcessIdProperty, $script:AppPid),
        [Windows.Automation.PropertyCondition]::new($AE::ClassNameProperty, 'Tauri Window'))
    return $AE::RootElement.FindFirst([Windows.Automation.TreeScope]::Children, $condition)
}

function Get-WebRoot {
    $main = Get-MainWindow
    if (-not $main) { return $null }
    return $main.FindFirst([Windows.Automation.TreeScope]::Children, [Windows.Automation.PropertyCondition]::new($AE::ClassNameProperty, 'WRY_WEBVIEW'))
}

function Get-NativeDialogs {
    # Only windows of the verifier's own process are considered. An owned common dialog is a child of its owner in the UIA tree.
    $condition = [Windows.Automation.PropertyCondition]::new($AE::ClassNameProperty, '#32770')
    $main = Get-MainWindow
    $found = [Collections.Generic.List[object]]::new()
    if ($main) { foreach ($element in $main.FindAll([Windows.Automation.TreeScope]::Children, $condition)) { if ($element.Current.ProcessId -eq $script:AppPid) { $found.Add($element) } } }
    $processCondition = [Windows.Automation.AndCondition]::new([Windows.Automation.PropertyCondition]::new($AE::ProcessIdProperty, $script:AppPid), $condition)
    foreach ($element in $AE::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $processCondition)) { $found.Add($element) }
    return $found.ToArray()
}

function Test-MainEnabled { return [Sm3DialogNative]::IsWindowEnabled($script:MainHwnd) }

function Wait-NativeDialog {
    param([int]$Seconds = $script:Deadline)
    $limit = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        Assert-Exclusive
        $dialogs = @(Get-NativeDialogs)
        if ($dialogs.Count -gt 0) { Start-Sleep -Milliseconds 400; return (Get-NativeDialogs)[0] }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $limit)
    throw "No native dialog appeared within ${Seconds}s."
}

function Wait-NativeDialogGone {
    $limit = [DateTime]::UtcNow.AddSeconds($script:Deadline)
    do { if (@(Get-NativeDialogs).Count -eq 0) { return $true }; Start-Sleep -Milliseconds 150 } while ([DateTime]::UtcNow -lt $limit)
    return $false
}

function Assert-NoNativeDialog {
    param([string]$Command,[string]$Check,[int]$Milliseconds = 1500)
    $limit = [DateTime]::UtcNow.AddMilliseconds($Milliseconds)
    do { if (@(Get-NativeDialogs).Count -gt 0) { Add-Result $Command $Check 'fail' 'A native dialog appeared.'; return $false }; Start-Sleep -Milliseconds 150 } while ([DateTime]::UtcNow -lt $limit)
    Add-Result $Command $Check 'pass'
    return $true
}

function Find-DialogControl {
    param($Dialog,[string[]]$Ids,[string]$ClassName)
    foreach ($id in $Ids) {
        $match = @(Find-Ui -Root $Dialog -Id $id -All | Where-Object { $_.Current.ClassName -ceq $ClassName })
        if ($match.Count -gt 0) { return $match[0] }
    }
    return $null
}

function Get-DialogFileNameEdit { param($Dialog) return (Find-DialogControl $Dialog @('1001', '1148') 'Edit') }
function Get-DialogDefaultButton { param($Dialog) return (Find-DialogControl $Dialog @('1') 'Button') }
function Get-DialogCancelButton { param($Dialog) return (Find-DialogControl $Dialog @('2') 'Button') }

function Get-DialogFilterText {
    param($Dialog)
    $combo = Find-DialogControl $Dialog @('1136') 'ComboBox'
    if (-not $combo) { $combo = @(Find-Ui -Root $Dialog -Name '*(*.*' -Like -All | Where-Object { $_.Current.ClassName -ceq 'ComboBox' }) | Select-Object -First 1 }
    if (-not $combo) { return $null }
    return $combo.Current.Name
}

function Invoke-LegacyDefault { param($Element) [Sm3DialogNative]::MsaaDoDefaultAction([IntPtr]$Element.Current.NativeWindowHandle) }
function Set-LegacyValue { param($Element,[string]$Value) [Sm3DialogNative]::MsaaSetValue([IntPtr]$Element.Current.NativeWindowHandle, $Value) }
function Get-LegacyValue { param($Element) return [Sm3DialogNative]::MsaaGetValue([IntPtr]$Element.Current.NativeWindowHandle) }

function Test-NativeDialogShell {
    param([string]$Command,$Dialog,[string]$ExpectedTitle,[string]$ExpectedName,[string]$ExpectedFilter)
    $hwnd = [IntPtr]$Dialog.Current.NativeWindowHandle
    $owner = [Sm3DialogNative]::GetWindow($hwnd, 4)
    $pid2 = [uint32]0
    [void][Sm3DialogNative]::GetWindowThreadProcessId($hwnd, [ref]$pid2)
    $title = $Dialog.Current.Name
    if ($DumpTrees) { Save-Diag ("$Command-dialog") $Dialog }
    $ok = (Assert-Check $Command 'native dialog title' ($title -ceq $ExpectedTitle) "title='$title' expected='$ExpectedTitle'")
    $ok = (Assert-Check $Command 'dialog belongs to the verifier app process' ($pid2 -eq $script:AppPid) "dialog pid=$pid2 app pid=$($script:AppPid)") -and $ok
    $ok = (Assert-Check $Command 'dialog is owned by the app main window' ($owner -eq $script:MainHwnd) "owner=$owner main=$($script:MainHwnd)") -and $ok
    $ok = (Assert-Check $Command 'main window is disabled while the dialog is open (modal)' (-not (Test-MainEnabled))) -and $ok
    $edit = Get-DialogFileNameEdit $Dialog
    if ($null -eq $edit) {
        Add-Result $Command 'default filename' 'not-automatable' 'No file name edit exposed through UI Automation.'
    } else {
        $name = Get-LegacyValue $edit
        if ($null -ne $ExpectedName) { $ok = (Assert-Check $Command 'default filename' ($name -ceq $ExpectedName) "value='$name' expected='$ExpectedName'") -and $ok }
        else { Add-Result $Command 'default filename (observed)' 'info' "value='$name'" }
    }
    $filter = Get-DialogFilterText $Dialog
    if ($null -eq $filter) { Add-Result $Command 'file-type filter' 'not-automatable' 'No file-type combo exposed through UI Automation.' }
    elseif ($ExpectedFilter) { $ok = (Assert-Check $Command 'file-type filter' ($filter -like "*$ExpectedFilter*") "value='$filter' expected-contains='$ExpectedFilter'") -and $ok }
    return $ok
}

function Complete-NativeDialog {
    param($Dialog,[string]$Path)
    $edit = Get-DialogFileNameEdit $Dialog
    if ($null -eq $edit) { throw 'Cannot type into the dialog: no file name edit.' }
    Set-LegacyValue $edit $Path
    $button = Get-DialogDefaultButton $Dialog
    if (-not $button) { throw 'Dialog default button not found.' }
    Invoke-LegacyDefault $button
    if (-not (Wait-NativeDialogGone)) { throw 'The native dialog did not close after confirming.' }
    Start-Sleep -Milliseconds 300
}

function Stop-NativeDialog {
    param($Dialog)
    $button = Get-DialogCancelButton $Dialog
    if (-not $button) { throw 'Dialog Cancel button not found.' }
    Invoke-LegacyDefault $button
    if (-not (Wait-NativeDialogGone)) { throw 'The native dialog did not close after Cancel.' }
    Start-Sleep -Milliseconds 500
}

function Get-DirectorySnapshot {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    return (@(Get-ChildItem -LiteralPath $Path -Recurse -Force | Sort-Object FullName | ForEach-Object { '{0}|{1}' -f $_.FullName.Substring($Path.Length), $(if ($_.PSIsContainer) { 'dir' } else { $_.Length }) }) -join "`n")
}

function Get-DocumentTabs {
    $web = Get-WebRoot
    return @(Find-Ui -Root $web -Type 'Button' -Name '*.pdf*' -Like -All | Where-Object { $_.Current.Name -notmatch '^Close ' } | ForEach-Object { $_.Current.Name } | Sort-Object -Unique)
}

function Get-AppState {
    param([string]$OutDir)
    return [pscustomobject]@{
        tabs = (Get-DocumentTabs) -join ';'
        files = Get-DirectorySnapshot $OutDir
        mainEnabled = Test-MainEnabled
        nativeDialogs = @(Get-NativeDialogs).Count
    }
}

function Assert-StateUnchanged {
    param([string]$Command,$Before,$After)
    $same = ($Before.tabs -ceq $After.tabs) -and ($Before.files -ceq $After.files) -and $After.mainEnabled -and ($After.nativeDialogs -eq 0)
    Assert-Check $Command 'Cancel leaves the app unchanged (same tabs, no output written, main window enabled, no dialog left)' $same ("tabs before='$($Before.tabs)' after='$($After.tabs)'; files before=$([regex]::Matches($Before.files,"`n").Count) after=$([regex]::Matches($After.files,"`n").Count)") | Out-Null
}

function Click-Button {
    param([string]$Name,[switch]$Like,$Root,[switch]$Last)
    if (-not $Root) { $Root = Get-WebRoot }
    $limit = [DateTime]::UtcNow.AddSeconds($script:Deadline)
    do {
        Assert-Exclusive
        $buttons = @(Find-Ui -Root $Root -Type 'Button' -Name $Name -Like:$Like -EnabledOnly -All)
        if ($buttons.Count -gt 0) { Invoke-Ui $(if ($Last) { $buttons[-1] } else { $buttons[0] }); return }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $limit)
    Save-Diag ("missing-button-" + ($Name -replace '\*', '')) (Get-WebRoot)
    throw "Enabled button not found: $Name"
}

function Open-Menu-Item {
    param([string]$Pattern)
    Click-Button -Name 'Menu'
    Click-Button -Name $Pattern -Like
}

function Wait-Idle {
    # Idle once the top-bar Create button is enabled again and no in-app dialog reports "working" status.
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    do {
        Assert-Exclusive
        $web = Get-WebRoot
        if ($web) {
            $busyText = @(Find-Ui -Root $web -Type 'Text' -Name '*Choosing an output location*' -Like -All) + @(Find-Ui -Root $web -Type 'Text' -Name '*Waiting for the native*' -Like -All)
            $create = Find-Ui -Root $web -Type 'Button' -Name 'Create' -EnabledOnly
            $dismiss = Find-Ui -Root $web -Type 'Button' -Name 'Cancel' -EnabledOnly
            if (($create -or $dismiss) -and $busyText.Count -eq 0) { return $stopwatch.ElapsedMilliseconds }
        }
        Start-Sleep -Milliseconds 150
    } while ($stopwatch.Elapsed.TotalSeconds -lt $script:Deadline)
    throw 'The app did not become idle.'
}

function Wait-HtmlText {
    param([string]$Pattern,[int]$Seconds = $script:Deadline)
    $web = Get-WebRoot
    $limit = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
        Assert-Exclusive
        $text = @(Find-Ui -Root $web -Name $Pattern -Like -All)
        if ($text.Count -gt 0) { return $text[0].Current.Name }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $limit)
    Save-Diag ("missing-text-" + ($Pattern -replace '[^A-Za-z0-9]', '_')) $web
    throw "In-app text not shown: $Pattern"
}

function Close-HtmlDialogs {
    $web = Get-WebRoot
    for ($i = 0; $i -lt 3; $i++) {
        foreach ($name in @('Cancel','Close')) {
            $button = Find-Ui -Root $web -Type 'Button' -Name $name -EnabledOnly
            if ($button) { try { Invoke-Ui $button } catch { }; Start-Sleep -Milliseconds 300 }
        }
    }
}

function Activate-Tab {
    param([string]$Name)
    $tab = Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name $Name -EnabledOnly
    Invoke-Ui $tab
    Start-Sleep -Milliseconds 400
}

function Open-Pdf {
    param([string]$Path,[string]$TabName)
    $full = [IO.Path]::GetFullPath($Path)
    if (-not (Test-UnderBase $full @((Join-Path $script:RunDir 'in')))) { throw "Refusing to open a document outside this run's input folder: $full" }
    $script:OpenedByRun[$TabName] = $full
    Open-Menu-Item $script:OpenItem
    $dialog = Wait-NativeDialog
    Complete-NativeDialog $dialog $full
    # A recovery offer can only name a record this run created (the folder was empty at start). The offer shows just the
    # file name, so it is answered only when that name maps to a document this run itself opened from its own input folder.
    $limit = [DateTime]::UtcNow.AddSeconds($script:Deadline)
    do {
        Assert-Exclusive
        $web = Get-WebRoot
        if (Find-Ui -Root $web -Type 'Button' -Name $TabName -EnabledOnly) { break }
        $offer = Find-Ui -Root $web -Name 'A recovery record for*' -Like
        if ($offer) {
            $tracked = $script:OpenedByRun[$TabName]
            if ($tracked -and $tracked -ceq $full -and (Find-Ui -Root $web -Type 'Text' -Name $TabName)) { Add-Result 'recovery' 'recovery offer for a document this run opened answered with Open original' 'finding' $full; Click-Button -Name 'Open original' }
            else { throw ('Recovery offer for a document this run did not open from its own folder (expected ' + $full + ').') }
        }
        Start-Sleep -Milliseconds 200
    } while ([DateTime]::UtcNow -lt $limit)
    Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name $TabName -EnabledOnly | Out-Null
    Start-Sleep -Milliseconds 800
}

function Close-OwnDocuments {
    # Closes only the tabs this run opened (tracked by name from its own input folder), discarding their unsaved edits.
    Assert-AppPid
    foreach ($name in @($script:OpenedByRun.Keys)) {
        for ($i = 0; $i -lt 3; $i++) {
            $web = Get-WebRoot
            if (-not $web) { return }
            $close = Find-Ui -Root $web -Type 'Button' -Name ('Close ' + $name) -EnabledOnly
            if (-not $close) { break }
            Invoke-Ui $close
            $limit = [DateTime]::UtcNow.AddSeconds(3)
            do {
                $confirm = Find-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Discard and close' -EnabledOnly
                if ($confirm) { Invoke-Ui $confirm; break }
                Start-Sleep -Milliseconds 150
            } while ([DateTime]::UtcNow -lt $limit)
            Start-Sleep -Milliseconds 500
        }
    }
}

function Assert-PdfOpens {
    param([string]$Command,[string]$Path,[int]$ExpectedPages)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { Add-Result $Command 'output file exists' 'fail' $Path; return $false }
    Add-Result $Command 'output file exists' 'pass' ((Split-Path -Leaf $Path) + ", $((Get-Item -LiteralPath $Path).Length) bytes")
    try {
        $count = [Sm3DialogNative]::PageCount($PdfiumPath, $Path)
        return (Assert-Check $Command 'output opens with PDFium' ($count -eq $ExpectedPages) "pages=$count expected=$ExpectedPages")
    } catch { Add-Result $Command 'output opens with PDFium' 'fail' $_.Exception.Message; return $false }
}

function Get-Annots { param([string]$Path) return @([Sm3DialogNative]::Annotations($PdfiumPath, $Path, 0)) }

function Wait-FileStable {
    param([string]$Path)
    $limit = [DateTime]::UtcNow.AddSeconds($script:Deadline)
    while (-not (Test-Path -LiteralPath $Path) -and [DateTime]::UtcNow -lt $limit) { Start-Sleep -Milliseconds 200 }
    Start-Sleep -Milliseconds 500
}

function Invoke-Scenario {
    param([string]$Name,[scriptblock]$Body)
    if ($Scenario.Count -gt 0 -and $Scenario -notcontains $Name) { return }
    Write-Host "--- $Name"
    try { & $Body }
    catch {
        if ($_.Exception.Message -like 'SM3_FOREIGN_APP*') { throw }
        Add-Result $Name 'scenario completed without an automation error' 'fail' $_.Exception.Message
        try { Save-Diag ("failure-" + $Name) (Get-WebRoot) } catch { }
        try { foreach ($dialog in Get-NativeDialogs) { Stop-NativeDialog $dialog } } catch { }
        try { Close-HtmlDialogs } catch { }
    }
}

function Set-ComboChoice {
    param($Combo,[string]$Option)
    $pattern = $null
    if ($Combo.TryGetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern, [ref]$pattern)) {
        $pattern.Expand(); Start-Sleep -Milliseconds 400
        $item = Find-Ui -Root $Combo -Name $Option -Type 'ListItem'
        if (-not $item) { $item = Find-Ui -Root (Get-WebRoot) -Name $Option -Type 'ListItem' }
        if (-not $item) { Save-Diag 'combo-expanded' (Get-WebRoot); try { $pattern.Collapse() } catch { }; throw "Combo option not found after expanding: $Option" }
        Invoke-Ui $item; try { $pattern.Collapse() } catch { }
        return 'expand-collapse and selection-item'
    }
    if ($Combo.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern, [ref]$pattern) -and -not $pattern.Current.IsReadOnly) { $pattern.SetValue($Option); return 'value' }
    throw 'Combo exposes neither a writable Value nor an ExpandCollapse pattern.'
}

function Save-Copy-Cancel-And-Confirm {
    param([string]$Command,[scriptblock]$Trigger,[string]$Title,[string]$Name,[string]$Filter,[string]$OutPath,[string]$OutDir,[scriptblock]$AfterCancel,[scriptblock]$Retrigger)
    $before = Get-AppState $OutDir
    & $Trigger
    $dialog = Wait-NativeDialog
    Test-NativeDialogShell $Command $dialog $Title $Name $Filter | Out-Null
    Stop-NativeDialog $dialog
    Wait-Idle | Out-Null
    Assert-StateUnchanged $Command $before (Get-AppState $OutDir)
    if ($AfterCancel) { & $AfterCancel }
    if ($Retrigger) { & $Retrigger } else { & $Trigger }
    $dialog = Wait-NativeDialog
    Complete-NativeDialog $dialog $OutPath
    Wait-FileStable $OutPath
}

# ---------------------------------------------------------------------------------------------------------------
function Wait-ForExclusive {
    $limit = [DateTime]::UtcNow.AddMinutes($WaitMinutes)
    while ($true) {
        $others = @(Get-OtherDevApps)
        if ($others.Count -eq 0) { return }
        if ([DateTime]::UtcNow -ge $limit) { throw "Another pdf-workstation dev build (pid $(($others | ForEach-Object Id) -join ',')) was still running after $WaitMinutes minutes." }
        Write-Host ("{0:HH:mm:ss} waiting for another pdf-workstation dev build to exit (pid {1})..." -f (Get-Date), (($others | ForEach-Object Id) -join ','))
        Start-Sleep -Seconds 30
    }
}

function Stop-OwnApp {
    if ($script:AppPid -le 4 -and -not $script:Process) { return }
    try {
        if ($script:Process -and -not $script:Process.HasExited) {
            [void]$script:Process.CloseMainWindow()
            [void]$script:Process.WaitForExit(5000)
        }
    } catch { }
    try { if ($script:Process -and -not $script:Process.HasExited) { $script:Process.Kill($true); $script:Process.WaitForExit(5000) | Out-Null } } catch { }
    $leftover = @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Id -eq $script:AppPid -and $script:AppPid -gt 4 })
    Add-Result 'cleanup' 'app process closed' $(if ($leftover.Count -eq 0) { 'pass' } else { 'fail' }) "pid=$($script:AppPid)"
}

function Invoke-Run {
    param([int]$Attempt)
    # Every attempt gets its own fresh folder under the confined, freshly created WorkRoot; nothing is ever deleted here.
    $script:RunDir = Join-Path $WorkRoot ('attempt-' + $Attempt)
    if (-not (Test-UnderBase $script:RunDir @($WorkRoot)) -or (Test-Path -LiteralPath $script:RunDir)) { throw "Run folder is not a fresh folder under WorkRoot: $($script:RunDir)" }
    $null = New-Item -ItemType Directory -Path $script:RunDir
    $script:DiagDir = Join-Path $script:RunDir 'diag'
    foreach ($name in @('in', 'out', 'app', 'diag')) { $null = New-Item -ItemType Directory -Path (Join-Path $script:RunDir $name) }
    $inDir = Join-Path $script:RunDir 'in'; $outDir = Join-Path $script:RunDir 'out'; $appDir = Join-Path $script:RunDir 'app'
    $script:Results.Clear(); $script:AppPid = 0; $script:Process = $null; $script:OpenedByRun = @{}; $script:SawForeign = $false
    $foreign = $false
    $recoverySnapshot = @(Get-RecoveryRecords)
    try {
        Copy-Item -LiteralPath $AppPath -Destination (Join-Path $appDir 'pdf-workstation.exe')
        $resources = Join-Path (Split-Path -Parent $AppPath) 'resources'
        if (Test-Path -LiteralPath $resources) { Copy-Item -LiteralPath $resources -Destination (Join-Path $appDir 'resources') -Recurse }
        $exeHash = (Get-FileHash -LiteralPath (Join-Path $appDir 'pdf-workstation.exe') -Algorithm SHA256).Hash
        Write-Host "App binary SHA256 $exeHash (built $((Get-Item -LiteralPath $AppPath).LastWriteTime))"

        foreach ($name in @('annotated.pdf', 'strict-text.pdf', 'strict-image.pdf')) { Copy-Item -LiteralPath (Join-Path $FixtureDir $name) -Destination $inDir }
        foreach ($name in @('reportlab-mixed-fields.pdf', 'reportlab-radio-fields.pdf', 'reportlab-choice-fields.pdf')) { Copy-Item -LiteralPath (Join-Path $formFixtureDir $name) -Destination $inDir }
        Copy-Item -LiteralPath (Join-Path $projectRoot 'src-tauri\resources\welcome.pdf') -Destination $inDir
        $imageA = Join-Path $inDir 'replacement-2x1.png'
        $bitmap = [Drawing.Bitmap]::new(2, 1, [Drawing.Imaging.PixelFormat]::Format24bppRgb)
        $bitmap.SetPixel(0, 0, [Drawing.Color]::FromArgb(0, 0, 255)); $bitmap.SetPixel(1, 0, [Drawing.Color]::FromArgb(255, 255, 0))
        $bitmap.Save($imageA, [Drawing.Imaging.ImageFormat]::Png); $bitmap.Dispose()

        $startInfo = [Diagnostics.ProcessStartInfo]::new((Join-Path $appDir 'pdf-workstation.exe'))
        $startInfo.WorkingDirectory = $appDir
        $startInfo.UseShellExecute = $false
        $startInfo.EnvironmentVariables['WEBVIEW2_USER_DATA_FOLDER'] = (Join-Path $script:RunDir 'webview')
        $script:Process = [Diagnostics.Process]::Start($startInfo)
        $script:AppPid = $script:Process.Id
        Write-Host "App started (pid $($script:AppPid)) from $appDir; run folder $($script:RunDir)"

        $limit = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
        $main = $null
        do { Assert-Exclusive; $main = Get-MainWindow; if ($main) { break }; Start-Sleep -Milliseconds 250 } while ([DateTime]::UtcNow -lt $limit)
        if (-not $main) { throw 'Main window did not appear.' }
        $script:MainHwnd = [IntPtr]$main.Current.NativeWindowHandle
        Wait-Ui -Root $main -Type 'Button' -Name 'Menu' -Seconds ($TimeoutSeconds + 30) | Out-Null
        Assert-Check 'launch' 'app window reachable through UI Automation and owned by the verifier process' ($main.Current.ProcessId -eq $script:AppPid -and $main.Current.Name -ceq 'PDF Workstation') "title='$($main.Current.Name)' exe-sha256=$exeHash" | Out-Null
        Assert-Check 'launch' 'no native dialog or disabled main window at start' ((Test-MainEnabled) -and @(Get-NativeDialogs).Count -eq 0) | Out-Null

        $annotated = Join-Path $inDir 'annotated.pdf'

        # ---- Open dialog (shared by every scenario) --------------------------------------------------------------
        Invoke-Scenario 'open' {
            $before = Get-AppState $outDir
            Open-Menu-Item $script:OpenItem
            $dialog = Wait-NativeDialog
            Test-NativeDialogShell 'open' $dialog 'Open' $null 'PDF documents' | Out-Null
            Stop-NativeDialog $dialog
            Assert-StateUnchanged 'open' $before (Get-AppState $outDir)
        }

        # ---- Save a copy of an annotated document (note + area highlight + text highlight) -----------------------
        Invoke-Scenario 'comments-save-copy' {
            Open-Pdf $annotated 'annotated.pdf'
            Click-Button -Name 'Comments'
            Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Comment*Page 1*Dialog verifier note*' -Like | Out-Null
            $items = @(Find-Ui -Root (Get-WebRoot) -Type 'Button' -Name '*Page 1*Dialog verifier*' -Like -All)
            Assert-Check 'comments' 'Comments panel lists the note, the area highlight and the text highlight from the fixture' ($items.Count -eq 3) (($items | ForEach-Object { $_.Current.Name }) -join ' ; ') | Out-Null
            # Edit the note through the in-app editor so the copy carries a change made in this session.
            Click-Button -Name 'Comment*Page 1*Dialog verifier note*' -Like
            $editor = Wait-Ui -Root (Get-WebRoot) -Type 'Edit' -Name 'Comment text'
            Set-UiValue $editor 'Edited by the dialog verifier'
            Click-Button -Name 'Save comment'
            Wait-Ui -Root (Get-WebRoot) -Type 'Edit' -Name 'Comment text' -Gone -Seconds 10 | Out-Null
            Start-Sleep -Milliseconds 800
            $outPath = Join-Path $outDir 'comments-copy.pdf'
            $trigger = { Click-Button -Name 'Save a copy' }
            Save-Copy-Cancel-And-Confirm -Command 'comments-save-copy' -Trigger $trigger -Title 'Save As' -Name 'organized-copy.pdf' -Filter 'PDF documents' -OutPath $outPath -OutDir $outDir
            if (Assert-PdfOpens 'comments-save-copy' $outPath 6) {
                $annots = Get-Annots $outPath
                $text = @($annots | Where-Object { $_ -like '1|*' }); $highlights = @($annots | Where-Object { $_ -like '9|*' })
                Assert-Check 'comments-save-copy' 'copy holds exactly 1 sticky note (Text) and 2 highlights on page 1' ($text.Count -eq 1 -and $highlights.Count -eq 2 -and $annots.Count -eq 3) ($annots -join ' ; ') | Out-Null
                Assert-Check 'comments-save-copy' 'sticky note carries the text edited in this session' ($text.Count -eq 1 -and $text[0] -like '1|Edited by the dialog verifier|*') ($text -join ' ; ') | Out-Null
                Assert-Check 'comments-save-copy' 'area and text highlight bodies survive the copy' (@($highlights | Where-Object { $_ -like '*Dialog verifier area*' }).Count -eq 1 -and @($highlights | Where-Object { $_ -like '*Dialog verifier text*' }).Count -eq 1) ($highlights -join ' ; ') | Out-Null
            }
            Wait-Idle | Out-Null
        }

        # ---- Creating a note / area highlight / text highlight needs pointer or selection input --------------------
        Invoke-Scenario 'comments-create' {
            Add-Result 'comments-create' 'add sticky note' 'not-automatable' 'Placement is pointer-only (CommentLayer onPointerDown/Up); the Comments button only enters placement mode and the page layer exposes no UI Automation pattern that dispatches pointer events. The fixture carries a note written by the same annotation writer instead.'
            Add-Result 'comments-create' 'add area highlight' 'not-automatable' 'Same reason: drag on the page layer (CommentLayer pointer handlers).'
        }

        # ---- Text highlight: selection through the UI Automation TextPattern, then the real toolbar button ----------
        Invoke-Scenario 'text-highlight' {
            Open-Pdf (Join-Path $inDir 'welcome.pdf') 'welcome.pdf'
            $web = Get-WebRoot
            Start-Sleep -Seconds 4
            if (-not (Find-Ui -Root $web -Type 'Text' -Name 'A place for your PDFs*' -Like)) {
                Add-Result 'text-highlight' 'add text highlight' 'not-automatable' 'The selectable text layer is aria-hidden (TextLayer.tsx:107), so its text is absent from the UI Automation tree and no TextPattern range can select it. Selection needs pointer or keyboard input. The fixture carries a text highlight written by the same annotation writer instead.'
                return
            }
            $doc = Find-Ui -Root $web -Type 'Document'
            $pattern = $null
            if (-not $doc -or -not $doc.TryGetCurrentPattern([Windows.Automation.TextPattern]::Pattern, [ref]$pattern)) { Save-Diag 'text-highlight-doc' $web; throw 'The page document exposes no TextPattern.' }
            $range = $pattern.DocumentRange.FindText('A place for your PDFs', $false, $false)
            if (-not $range) { throw 'TextPattern could not find the page text.' }
            $range.Select()
            Start-Sleep -Milliseconds 500
            $button = Wait-Ui -Root $web -Type 'Button' -Name 'Highlight selected text' -EnabledOnly -Seconds 5
            Invoke-Ui $button
            $editor = Wait-Ui -Root (Get-WebRoot) -Type 'Edit' -Name 'Highlight description'
            Set-UiValue $editor 'Text highlight made through the dialog verifier'
            Click-Button -Name 'Save text highlight'
            Wait-Ui -Root (Get-WebRoot) -Type 'Edit' -Name 'Highlight description' -Gone -Seconds 10 | Out-Null
            Start-Sleep -Milliseconds 800
            $outPath = Join-Path $outDir 'text-highlight-copy.pdf'
            $trigger = { Click-Button -Name 'Save a copy' }
            Save-Copy-Cancel-And-Confirm -Command 'text-highlight-save-copy' -Trigger $trigger -Title 'Save As' -Name 'organized-copy.pdf' -Filter 'PDF documents' -OutPath $outPath -OutDir $outDir
            if (Assert-PdfOpens 'text-highlight-save-copy' $outPath 6) {
                $annots = Get-Annots $outPath
                Assert-Check 'text-highlight-save-copy' 'copy holds one text highlight with the typed description' ($annots.Count -eq 1 -and $annots[0] -like '9|Text highlight made through the dialog verifier|*') ($annots -join ' ; ') | Out-Null
            }
            Wait-Idle | Out-Null
        }

        # ---- Fill forms ------------------------------------------------------------------------------------------
        $openFillForms = {
            Click-Button -Name 'Fill existing fields*' -Like
            Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Save filled copy' | Out-Null
        }
        $filled = {
            param([string]$Label,[string]$Pdf,[string]$Tab,[scriptblock]$Fill,[string]$OutName,[scriptblock]$Verify)
            Open-Pdf (Join-Path $inDir $Pdf) $Tab
            & $openFillForms
            & $Fill
            $outPath = Join-Path $outDir $OutName
            $trigger = { Click-Button -Name 'Save filled copy' }
            $retrigger = { Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Save filled copy' -EnabledOnly | Out-Null; Click-Button -Name 'Save filled copy' }
            Save-Copy-Cancel-And-Confirm -Command $Label -Trigger $trigger -Title 'Save filled form as a new PDF' -Name 'filled-form.pdf' -Filter 'PDF documents' -OutPath $outPath -OutDir $outDir -Retrigger $retrigger
            if (Assert-PdfOpens $Label $outPath 1) { & $Verify $outPath }
            Wait-Idle | Out-Null
        }

        Invoke-Scenario 'forms-text-checkbox' {
            & $filled 'forms-text-checkbox' 'reportlab-mixed-fields.pdf' 'reportlab-mixed-fields.pdf' {
                $web = Get-WebRoot
                Set-UiValue (Wait-Ui -Root $web -Type 'Edit' -Name 'Name') 'Dialog verifier'
                $box = Wait-Ui -Root $web -Type 'CheckBox' -Name 'Consent, page 1'
                Invoke-Ui $box
                Start-Sleep -Milliseconds 300
            } 'forms-text-checkbox-out.pdf' {
                param($Path)
                $widgets = @(Get-Annots $Path | Where-Object { $_ -like '20|*' })
                $name = @($widgets | Where-Object { $_ -like '20||Name|*' }); $consent = @($widgets | Where-Object { $_ -like '20||Consent|*' })
                Assert-Check 'forms-text-checkbox' 'text field value written' ($name.Count -eq 1 -and $name[0] -like '*|Dialog verifier|*') ($widgets -join ' ; ') | Out-Null
                Assert-Check 'forms-text-checkbox' 'checkbox is on (V and AS not Off)' ($consent.Count -eq 1 -and $consent[0].Split('|')[3] -notin @('', 'Off') -and $consent[0].Split('|')[4] -notin @('', 'Off')) ($consent -join ' ; ') | Out-Null
            }
        }

        Invoke-Scenario 'forms-radio' {
            & $filled 'forms-radio' 'reportlab-radio-fields.pdf' 'reportlab-radio-fields.pdf' {
                $web = Get-WebRoot
                $radio = Wait-Ui -Root $web -Type 'RadioButton' -Name 'Preference: Choice A, page 1'
                Invoke-Ui $radio
                Start-Sleep -Milliseconds 300
            } 'forms-radio-out.pdf' {
                param($Path)
                $widgets = @(Get-Annots $Path | Where-Object { $_ -like '20|*' })
                $states = @($widgets | ForEach-Object { $_.Split('|')[4] })
                Assert-Check 'forms-radio' 'group has 3 widgets and only the first (Choice A) is on' ($widgets.Count -eq 3 -and $states[0] -ne 'Off' -and $states[0] -ne '' -and $states[1] -eq 'Off' -and $states[2] -eq 'Off') ($states -join ',') | Out-Null
            }
        }

        Invoke-Scenario 'forms-dropdown' {
            & $filled 'forms-dropdown' 'reportlab-choice-fields.pdf' 'reportlab-choice-fields.pdf' {
                $web = Get-WebRoot
                $combo = Wait-Ui -Root $web -Type 'ComboBox' -Name 'Destination, page 1'
                $how = Set-ComboChoice $combo 'North Hub'
                Add-Result 'forms-dropdown' 'dropdown option chosen through UI Automation' 'info' "via $how pattern"
                Start-Sleep -Milliseconds 300
            } 'forms-dropdown-out.pdf' {
                param($Path)
                $widgets = @(Get-Annots $Path | Where-Object { $_ -like '20|*' })
                $dest = @($widgets | Where-Object { $_ -like '20||Destination|*' })
                Assert-Check 'forms-dropdown' 'Destination dropdown now holds the North Hub export value' ($dest.Count -eq 1 -and $dest[0].Split('|')[3] -eq 'north-001') ($widgets -join ' ; ') | Out-Null
            }
        }

        # ---- Strict text replacement -----------------------------------------------------------------------------
        Invoke-Scenario 'text-replacement' {
            Open-Pdf (Join-Path $inDir 'strict-text.pdf') 'strict-text.pdf'
            $outPath = Join-Path $outDir 'text-replaced-out.pdf'
            $open = { Open-Menu-Item 'Replace strict text*'; Wait-Ui -Root (Get-WebRoot) -Type 'Edit' -Name 'Replacement text' | Out-Null }
            & $open
            Set-UiValue (Wait-Ui -Root (Get-WebRoot) -Type 'Edit' -Name 'Replacement text') '67890'
            $trigger = { Click-Button -Name 'Save replacement copy' }
            $retrigger = { Wait-HtmlText '*Save canceled*' | Out-Null; Click-Button -Name 'Save replacement copy' }
            $before = Get-AppState $outDir
            & $trigger
            $dialog = Wait-NativeDialog
            Test-NativeDialogShell 'text-replacement' $dialog 'Save PDF with replaced text as a new file' 'text-replaced.pdf' 'PDF documents' | Out-Null
            Stop-NativeDialog $dialog
            $notice = Wait-HtmlText '*Save canceled*'
            Assert-Check 'text-replacement' 'Cancel reports "Save canceled" in the dialog' ($notice -like 'Save canceled*') $notice | Out-Null
            Assert-StateUnchanged 'text-replacement' $before (Get-AppState $outDir)
            & $trigger
            $dialog = Wait-NativeDialog
            Complete-NativeDialog $dialog $outPath
            Wait-FileStable $outPath
            if (Assert-PdfOpens 'text-replacement' $outPath 1) {
                $text = [Sm3DialogNative]::PageText($PdfiumPath, $outPath, 0).Trim()
                Assert-Check 'text-replacement' 'copy shows the replaced text and not the original' ($text -ceq '67890') "text='$text'" | Out-Null
            }
            Wait-Idle | Out-Null
        }

        # ---- Strict image replacement (picker only after the live document check) -----------------------------------
        Invoke-Scenario 'image-replacement' {
            Open-Pdf (Join-Path $inDir 'welcome.pdf') 'welcome.pdf'
            Open-Menu-Item 'Replace full-page image*'
            $refusal = Wait-Ui -Root (Get-WebRoot) -Type 'Text' -Name '*image*' -Like -Seconds 10
            Assert-NoNativeDialog 'image-replacement' 'a document without a supported image opens no picker (refused before any native dialog)' | Out-Null
            Assert-Check 'image-replacement' 'the unsupported document is refused with an image-edit message' ($refusal.Current.Name -like '*image*') $refusal.Current.Name | Out-Null
            Click-Button -Name 'Dismiss message'
            Open-Pdf (Join-Path $inDir 'strict-image.pdf') 'strict-image.pdf'
            Open-Menu-Item 'Replace full-page image*'
            Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Choose image and save copy' | Out-Null
            $outPath = Join-Path $outDir 'image-replaced-out.pdf'
            $trigger = { Click-Button -Name 'Choose image and save copy' }
            $before = Get-AppState $outDir
            & $trigger
            $dialog = Wait-NativeDialog
            Test-NativeDialogShell 'image-replacement(pick)' $dialog 'Choose an exact-size replacement PNG or JPEG' $null 'PNG and JPEG images' | Out-Null
            Stop-NativeDialog $dialog
            $notice = Wait-HtmlText '*Replacement or output selection canceled*'
            Assert-Check 'image-replacement(pick)' 'Cancel at the picker reports the canceled notice' ($notice -like 'Replacement or output selection canceled*') $notice | Out-Null
            Assert-StateUnchanged 'image-replacement(pick)' $before (Get-AppState $outDir)
            & $trigger
            $dialog = Wait-NativeDialog
            Complete-NativeDialog $dialog (Join-Path $inDir 'replacement-2x1.png')
            $dialog = Wait-NativeDialog
            Test-NativeDialogShell 'image-replacement(save)' $dialog 'Save PDF with replaced image as a new file' 'image-replaced.pdf' 'PDF documents' | Out-Null
            Stop-NativeDialog $dialog
            Wait-HtmlText '*Replacement or output selection canceled*' | Out-Null
            Assert-StateUnchanged 'image-replacement(save)' $before (Get-AppState $outDir)
            & $trigger
            $dialog = Wait-NativeDialog
            Complete-NativeDialog $dialog (Join-Path $inDir 'replacement-2x1.png')
            $dialog = Wait-NativeDialog
            Complete-NativeDialog $dialog $outPath
            Wait-FileStable $outPath
            if (Assert-PdfOpens 'image-replacement' $outPath 1) {
                $left = [Sm3DialogNative]::RenderPixel($PdfiumPath, $outPath, 0, 20, 50)
                $right = [Sm3DialogNative]::RenderPixel($PdfiumPath, $outPath, 0, 180, 50)
                Assert-Check 'image-replacement' 'rendered copy shows the replacement pixels (left blue, right yellow)' ($left -eq '0,0,255' -and $right -eq '255,255,0') "left=$left right=$right" | Out-Null
            }
            Wait-Idle | Out-Null
        }
    }
    catch {
        if ($_.Exception.Message -like 'SM3_FOREIGN_APP*') { $foreign = $true; Write-Host ('ABORT: ' + $_.Exception.Message) }
        else { Add-Result 'harness' 'harness completed' 'fail' $_.Exception.Message; try { Save-Diag 'harness-failure' (Get-WebRoot) } catch { } }
    }
    finally {
        try { if ($script:AppPid -gt 4 -and (Get-Process -Id $script:AppPid -ErrorAction SilentlyContinue) -and -not $foreign) { Close-OwnDocuments } } catch { }
        Stop-OwnApp
        try { [IO.File]::WriteAllText((Join-Path $script:RunDir 'results.json'), ($script:Results | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false)) } catch { }
        # Remove only the recovery record files that appeared during this run (diff against the snapshot taken before launch).
        try {
            $created = @(Get-RecoveryRecords | Where-Object { $recoverySnapshot -notcontains $_ })
            if ($script:SawForeign -or @(Get-OtherDevApps).Count -gt 0) {
                if ($created.Count -gt 0) { Write-Host "Left $($created.Count) new recovery record(s) in place because another dev build was running." }
            } else {
                foreach ($record in $created) {
                    if ((Split-Path -Parent $record) -ceq $recoveryDir -and $record.EndsWith('.smacrec', [StringComparison]::OrdinalIgnoreCase)) { Remove-Item -LiteralPath $record -Force }
                }
                if ($created.Count -gt 0) { Add-Result 'cleanup' 'recovery records created by this run removed' 'info' "$($created.Count) file(s)" }
            }
        } catch { Write-Host ('Recovery cleanup failed: ' + $_.Exception.Message) }
    }
    return $foreign
}

$null = New-Item -ItemType Directory -Force -Path $WorkRoot
$completed = $false
$attemptLimit = [DateTime]::UtcNow.AddMinutes($WaitMinutes)
for ($attempt = 1; [DateTime]::UtcNow -lt $attemptLimit -and -not $completed; $attempt++) {
    Wait-ForExclusive
    Write-Host "=== run attempt $attempt"
    $foreign = Invoke-Run -Attempt $attempt
    if (-not $foreign) { $completed = $true }
}
if (-not $completed) { Write-Host 'Gave up: another pdf-workstation process kept appearing.'; exit 2 }

$failed = @($script:Results | Where-Object { $_.status -eq 'fail' })
$notAutomatable = @($script:Results | Where-Object { $_.status -eq 'not-automatable' })
Write-Host ''
Write-Host ("Summary: {0} pass, {1} fail, {2} not automatable. Work root: {3}" -f (@($script:Results | Where-Object { $_.status -eq 'pass' }).Count), $failed.Count, $notAutomatable.Count, $WorkRoot)
if ($failed.Count -gt 0) { exit 1 }
if ($Strict -and $notAutomatable.Count -gt 0) { Write-Host 'Strict: not-automatable checks present.'; exit 2 }
