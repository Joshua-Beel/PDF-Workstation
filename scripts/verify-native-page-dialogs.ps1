[CmdletBinding()]
param(
    [string]$AppPath,
    [string]$WorkRoot,
    [string]$PdfiumPath,
    [string[]]$Scenario = @(),
    [int]$TimeoutSeconds = 30,
    [switch]$DumpTrees,
    [switch]$Strict
)
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$projectRoot = Split-Path -Parent $PSScriptRoot
if (-not $AppPath) {
    $targetRoot = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $projectRoot 'src-tauri\target' }
    $AppPath = Join-Path $targetRoot 'debug\pdf-workstation.exe'
}
if (-not $PdfiumPath) { $PdfiumPath = Join-Path $projectRoot 'src-tauri\resources\pdfium\bin\pdfium.dll' }
$tmpBase = $null
for ($ancestor = $projectRoot; $ancestor; $ancestor = Split-Path -Parent $ancestor) {
    $candidate = Join-Path $ancestor 'tmp'
    if ((Test-Path -LiteralPath $candidate -PathType Container) -and (Test-Path -LiteralPath (Join-Path $ancestor 'wt') -PathType Container)) { $tmpBase = [IO.Path]::GetFullPath($candidate); break }
}
if (-not $tmpBase) { throw 'Could not find the Smacrobat tmp folder (a tmp folder next to wt) above the repo.' }
if (-not $WorkRoot) { $WorkRoot = Join-Path $tmpBase ('sm2-dialogs\run-' + (Get-Date -Format 'yyyyMMdd-HHmmss')) }
$WorkRoot = [IO.Path]::GetFullPath($WorkRoot)
$systemDrive = [IO.Path]::GetPathRoot([Environment]::GetFolderPath('Windows'))
if ([IO.Path]::GetPathRoot($WorkRoot).Equals($systemDrive, [StringComparison]::OrdinalIgnoreCase)) { throw "WorkRoot must not be on the system drive ($systemDrive): $WorkRoot" }
$allowedTmp = $tmpBase.TrimEnd([char]92) + [char]92
if (-not ($WorkRoot.TrimEnd([char]92) + [char]92).StartsWith($allowedTmp, [StringComparison]::OrdinalIgnoreCase)) { throw "WorkRoot must be under the sibling tmp folder $allowedTmp (no UNC or other drives): $WorkRoot" }
if (Test-Path -LiteralPath $WorkRoot) { throw "WorkRoot must be a fresh folder: $WorkRoot" }
if (-not (Test-Path -LiteralPath $AppPath -PathType Leaf)) { throw "App executable not found: $AppPath" }
if (-not (Test-Path -LiteralPath $PdfiumPath -PathType Leaf)) { throw "PDFium library not found: $PdfiumPath" }

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
if (-not ('SmDialogNative' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Text;
public static class SmDialogNative {
    // The Windows common dialog hosts classic HWND controls (file name edit, Save/Cancel buttons). Their UI Automation
    // proxy exposes no Invoke/Value pattern to the managed client, so these controls are reached through the same
    // accessibility stack's MSAA (IAccessible) actions: accDoDefaultAction presses a button, accValue reads and sets an
    // edit. No synthetic keyboard or mouse input is involved.
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
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] static extern IntPtr LoadLibraryW(string path);
    [DllImport("kernel32.dll", CharSet = CharSet.Ansi, SetLastError = true)] static extern IntPtr GetProcAddress(IntPtr module, string name);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void VoidFn();
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr LoadDocumentFn(byte[] path, byte[] password);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate int PageCountFn(IntPtr document);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate IntPtr LoadPageFn(IntPtr document, int index);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate float PageSizeFn(IntPtr page);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void ClosePageFn(IntPtr page);
    [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void CloseDocumentFn(IntPtr document);
    static T Bind<T>(IntPtr module, string name) where T : class {
        IntPtr address = GetProcAddress(module, name);
        if (address == IntPtr.Zero) throw new InvalidOperationException("PDFium export missing: " + name);
        return Marshal.GetDelegateForFunctionPointer(address, typeof(T)) as T;
    }
    // Returns page sizes as "widthxheight" strings (PDF points, rounded), or throws when PDFium cannot open the file.
    public static string[] PdfPageSizes(string pdfiumPath, string pdfPath) {
        IntPtr module = LoadLibraryW(pdfiumPath);
        if (module == IntPtr.Zero) throw new InvalidOperationException("PDFium could not be loaded.");
        Bind<VoidFn>(module, "FPDF_InitLibrary")();
        try {
            byte[] path = Encoding.ASCII.GetBytes(pdfPath + "\0");
            IntPtr document = Bind<LoadDocumentFn>(module, "FPDF_LoadDocument")(path, null);
            if (document == IntPtr.Zero) throw new InvalidOperationException("PDFium refused to open " + pdfPath);
            try {
                int count = Bind<PageCountFn>(module, "FPDF_GetPageCount")(document);
                string[] sizes = new string[count];
                var loadPage = Bind<LoadPageFn>(module, "FPDF_LoadPage");
                var width = Bind<PageSizeFn>(module, "FPDF_GetPageWidthF");
                var height = Bind<PageSizeFn>(module, "FPDF_GetPageHeightF");
                var closePage = Bind<ClosePageFn>(module, "FPDF_ClosePage");
                for (int i = 0; i < count; i++) {
                    IntPtr page = loadPage(document, i);
                    if (page == IntPtr.Zero) throw new InvalidOperationException("PDFium could not load page " + (i + 1));
                    sizes[i] = Math.Round(width(page)).ToString() + "x" + Math.Round(height(page)).ToString();
                    closePage(page);
                }
                return sizes;
            } finally { Bind<CloseDocumentFn>(module, "FPDF_CloseDocument")(document); }
        } finally { Bind<VoidFn>(module, "FPDF_DestroyLibrary")(); }
    }
}
'@
}

$AE = [Windows.Automation.AutomationElement]
$script:Results = [Collections.Generic.List[object]]::new()
$script:AppPid = 0
$script:MainHwnd = [IntPtr]::Zero
$script:Process = $null
$script:DiagDir = Join-Path $WorkRoot 'diag'
$script:Deadline = $TimeoutSeconds
$script:OpenItem = 'Open' + [char]0x2026 + '*'

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
                if ($type -match '^(Button|Edit|ComboBox|Window|Text|CheckBox|RadioButton|List|ListItem|Document)$') { '{0}|{1}|enabled={2}' -f $type, $c.Name, $c.IsEnabled }
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
        $element = Find-Ui -Root $Root -Name $Name -Type $Type -Id $Id -Like:$Like -EnabledOnly:$EnabledOnly
        if ($Gone) { if ($null -eq $element) { return $true } } elseif ($null -ne $element) { return $element }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $limit)
    if ($Gone) { return $false }
    try { Save-Diag ("wait-timeout-$Type-$Name") (Get-WebRoot) } catch { }
    throw "UI element not found within ${Seconds}s: type=$Type name=$Name id=$Id"
}

function Invoke-Ui {
    param($Element)
    $pattern = $null
    if ($Element.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern, [ref]$pattern)) { $pattern.Invoke(); return }
    if ($Element.TryGetCurrentPattern([Windows.Automation.TogglePattern]::Pattern, [ref]$pattern)) { $pattern.Toggle(); return }
    if ($Element.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern, [ref]$pattern)) { $pattern.Select(); return }
    if ($Element.TryGetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern, [ref]$pattern)) {
        if ($pattern.Current.ExpandCollapseState -eq [Windows.Automation.ExpandCollapseState]::Collapsed) { $pattern.Expand() } else { $pattern.Collapse() }
        return
    }
    throw ('Element exposes no Invoke/Toggle/SelectionItem pattern: ' + $Element.Current.Name)
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
    # An owned common dialog is exposed by UI Automation as a child of its owner window (the Tauri main window).
    $condition = [Windows.Automation.PropertyCondition]::new($AE::ClassNameProperty, '#32770')
    $main = Get-MainWindow
    $found = [Collections.Generic.List[object]]::new()
    if ($main) { foreach ($element in $main.FindAll([Windows.Automation.TreeScope]::Children, $condition)) { $found.Add($element) } }
    $processCondition = [Windows.Automation.AndCondition]::new([Windows.Automation.PropertyCondition]::new($AE::ProcessIdProperty, $script:AppPid), $condition)
    foreach ($element in $AE::RootElement.FindAll([Windows.Automation.TreeScope]::Children, $processCondition)) { $found.Add($element) }
    return $found.ToArray()
}

function Test-MainEnabled { return [SmDialogNative]::IsWindowEnabled($script:MainHwnd) }

function Wait-NativeDialog {
    param([int]$Seconds = $script:Deadline)
    $limit = [DateTime]::UtcNow.AddSeconds($Seconds)
    do {
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
    if (-not $combo) { return $null }
    return $combo.Current.Name
}

function Invoke-LegacyDefault { param($Element) [SmDialogNative]::MsaaDoDefaultAction([IntPtr]$Element.Current.NativeWindowHandle) }
function Set-LegacyValue { param($Element,[string]$Value) [SmDialogNative]::MsaaSetValue([IntPtr]$Element.Current.NativeWindowHandle, $Value) }
function Get-LegacyValue { param($Element) return [SmDialogNative]::MsaaGetValue([IntPtr]$Element.Current.NativeWindowHandle) }

function Test-NativeDialogShell {
    param([string]$Command,$Dialog,[string]$ExpectedTitle,[string]$ExpectedName,[string]$ExpectedFilter,[switch]$NoFilter)
    $hwnd = [IntPtr]$Dialog.Current.NativeWindowHandle
    $owner = [SmDialogNative]::GetWindow($hwnd, 4)
    $title = $Dialog.Current.Name
    $ok = $true
    if ($DumpTrees) { Save-Diag ("$Command-dialog") $Dialog }
    $ok = (Assert-Check $Command 'native dialog title' ($title -ceq $ExpectedTitle) "title='$title' expected='$ExpectedTitle'") -and $ok
    $ok = (Assert-Check $Command 'dialog is owned by the app main window' ($owner -eq $script:MainHwnd) "owner=$owner main=$($script:MainHwnd)") -and $ok
    $ok = (Assert-Check $Command 'main window is disabled while the dialog is open (modal)' (-not (Test-MainEnabled))) -and $ok
    $edit = Get-DialogFileNameEdit $Dialog
    if ($null -eq $edit) {
        Add-Result $Command 'default filename' 'not-automatable' 'No file name edit exposed through UI Automation.'
    } else {
        $name = Get-LegacyValue $edit
        if ($null -ne $ExpectedName) { $ok = (Assert-Check $Command 'default filename' ($name -ceq $ExpectedName) "value='$name' expected='$ExpectedName'") -and $ok }
        else { Add-Result $Command 'default filename (observed)' 'pass' "value='$name'" }
    }
    $filter = Get-DialogFilterText $Dialog
    if ($NoFilter) {
        Add-Result $Command 'file-type filter (observed, none requested by the app)' 'pass' "value='$filter'"
    } elseif ($null -eq $filter) {
        Add-Result $Command 'file-type filter' 'not-automatable' 'No file-type combo exposed through UI Automation.'
    } elseif ($ExpectedFilter) {
        $ok = (Assert-Check $Command 'file-type filter' ($filter -like "*$ExpectedFilter*") "value='$filter' expected-contains='$ExpectedFilter'") -and $ok
    }
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

function Wait-AppIdle {
    # The page is idle when the top-bar Create button is enabled (nothing is busy) or an in-app dialog has its
    # Cancel/Close button enabled (the dialog is no longer working). Observed: about 2.5 s after a native dialog closes.
    $stopwatch = [Diagnostics.Stopwatch]::StartNew()
    do {
        $web = Get-WebRoot
        if ($web) {
            $create = Find-Ui -Root $web -Type 'Button' -Name 'Create' -EnabledOnly
            $dismiss = Find-Ui -Root $web -Type 'Button' -Name 'Cancel' -EnabledOnly
            if (-not $dismiss) { $dismiss = Find-Ui -Root $web -Type 'Button' -Name 'Close' -EnabledOnly }
            if ($create -or $dismiss) { return $stopwatch.ElapsedMilliseconds }
        }
        Start-Sleep -Milliseconds 100
    } while ($stopwatch.Elapsed.TotalSeconds -lt $script:Deadline)
    throw 'The app did not become idle after the native dialog closed.'
}

function Stop-NativeDialog {
    param($Dialog)
    $button = Get-DialogCancelButton $Dialog
    if (-not $button) { throw 'Dialog Cancel button not found.' }
    Invoke-LegacyDefault $button
    if (-not (Wait-NativeDialogGone)) { throw 'The native dialog did not close after Cancel.' }
    $null = Wait-AppIdle
}

function Get-DirectorySnapshot {
    param([string]$Path)
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    return (@(Get-ChildItem -LiteralPath $Path -Recurse -Force | Sort-Object FullName | ForEach-Object { '{0}|{1}' -f $_.FullName.Substring($Path.Length), $(if ($_.PSIsContainer) { 'dir' } else { $_.Length }) }) -join "`n")
}

function Get-DocumentTabs {
    $main = Get-WebRoot
    return @(Find-Ui -Root $main -Type 'Button' -Name '*.pdf*' -Like -All | Where-Object { $_.Current.Name -notmatch '^Close ' } | ForEach-Object { $_.Current.Name } | Sort-Object -Unique)
}

function Get-AppState {
    param([string]$OutDir)
    $main = Get-WebRoot
    return [pscustomobject]@{
        tabs = (Get-DocumentTabs) -join ';'
        files = Get-DirectorySnapshot $OutDir
        mainEnabled = Test-MainEnabled
        nativeDialogs = @(Get-NativeDialogs).Count
    }
}

function Assert-StateUnchanged {
    param([string]$Command,$Before,$After,[switch]$NoTabs)
    $same = ($NoTabs -or ($Before.tabs -ceq $After.tabs)) -and ($Before.files -ceq $After.files) -and $After.mainEnabled -and ($After.nativeDialogs -eq 0)
    Assert-Check $Command 'Cancel leaves the app unchanged (same tabs, no output written, main window enabled, no dialog left)' $same ("tabs before='$($Before.tabs)' after='$($After.tabs)'; files before=$([regex]::Matches($Before.files,"`n").Count) after=$([regex]::Matches($After.files,"`n").Count)") | Out-Null
}

function Click-Button {
    param([string]$Name,[switch]$Like,$Root,[switch]$Last)
    if (-not $Root) { $Root = Get-WebRoot }
    $limit = [DateTime]::UtcNow.AddSeconds($script:Deadline)
    do {
        $buttons = @(Find-Ui -Root $Root -Type 'Button' -Name $Name -Like:$Like -EnabledOnly -All)
        if ($buttons.Count -gt 0) { Invoke-Ui $(if ($Last) { $buttons[-1] } else { $buttons[0] }); return }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $limit)
    Save-Diag ("missing-button-" + $Name) (Get-WebRoot)
    throw "Enabled button not found: $Name"
}

function Open-Menu-Item {
    param([string]$Pattern)
    Click-Button -Name 'Menu'
    Click-Button -Name $Pattern -Like
}

function Wait-HtmlDialog {
    param([string]$Heading)
    $main = Get-WebRoot
    $limit = [DateTime]::UtcNow.AddSeconds($script:Deadline)
    do {
        $text = @(Find-Ui -Root $main -Name $Heading -All | Where-Object { try { $_.Current.ControlType.ProgrammaticName -match 'Text|Window|Pane|Group|Dialog' } catch { $false } })
        if ($text.Count -gt 0) { return $main }
        Start-Sleep -Milliseconds 150
    } while ([DateTime]::UtcNow -lt $limit)
    Save-Diag ("missing-dialog-" + $Heading) $main
    throw "In-app dialog not shown: $Heading"
}

function Close-HtmlDialogs {
    param([string[]]$ButtonNames = @('Cancel','Close'))
    $main = Get-WebRoot
    for ($i = 0; $i -lt 3; $i++) {
        foreach ($name in $ButtonNames) {
            $button = Find-Ui -Root $main -Type 'Button' -Name $name -EnabledOnly
            if ($button) { try { Invoke-Ui $button } catch { }; Start-Sleep -Milliseconds 300 }
        }
    }
}

function Activate-Tab {
    param([string]$Name)
    $main = Get-WebRoot
    $tab = Wait-Ui -Root $main -Type 'Button' -Name $Name -EnabledOnly
    Invoke-Ui $tab
    Start-Sleep -Milliseconds 400
}

function New-TestPdf {
    param([string]$Path,[int]$Pages,[int]$Width,[int]$Height,[string]$Label)
    $objects = [Collections.Generic.List[string]]::new()
    $kids = (3..(2 + $Pages) | ForEach-Object { "$(($_ * 2 - 3)) 0 R" }) -join ' '
    $objects.Add('<< /Type /Catalog /Pages 2 0 R >>')
    $objects.Add("<< /Type /Pages /Kids [$kids] /Count $Pages >>")
    for ($i = 0; $i -lt $Pages; $i++) {
        $pageId = 3 + 2 * $i; $contentId = $pageId + 1
        $text = "BT /F1 24 Tf 20 $($Height - 40) Td ($Label page $($i + 1)) Tj ET"
        $objects.Add("<< /Type /Page /Parent 2 0 R /MediaBox [0 0 $Width $Height] /Resources << /Font << /F1 << /Type /Font /Subtype /Type1 /BaseFont /Helvetica >> >> >> /Contents $contentId 0 R >>")
        $objects.Add("<< /Length $($text.Length) >>`nstream`n$text`nendstream")
    }
    $builder = [Text.StringBuilder]::new('%PDF-1.4' + "`n")
    $offsets = @()
    for ($n = 0; $n -lt $objects.Count; $n++) { $offsets += $builder.Length; [void]$builder.Append("$($n + 1) 0 obj`n$($objects[$n])`nendobj`n") }
    $xref = $builder.Length
    [void]$builder.Append("xref`n0 $($objects.Count + 1)`n0000000000 65535 f `n")
    foreach ($offset in $offsets) { [void]$builder.Append(('{0:D10} 00000 n ' -f $offset) + "`n") }
    [void]$builder.Append("trailer`n<< /Size $($objects.Count + 1) /Root 1 0 R >>`nstartxref`n$xref`n%%EOF`n")
    [IO.File]::WriteAllBytes($Path, [Text.Encoding]::ASCII.GetBytes($builder.ToString()))
}

function Get-PdfSizes { param([string]$Path) return @([SmDialogNative]::PdfPageSizes($PdfiumPath, $Path)) }

function Assert-PdfOutput {
    param([string]$Command,[string]$Path,[string[]]$ExpectedSizes)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) { Add-Result $Command 'output file exists' 'fail' $Path; return }
    Add-Result $Command 'output file exists' 'pass' ((Split-Path -Leaf $Path) + ", $((Get-Item -LiteralPath $Path).Length) bytes")
    try {
        $sizes = @(Get-PdfSizes $Path)
        Add-Result $Command 'output opens with PDFium' 'pass' "$($sizes.Count) page(s)"
        Assert-Check $Command 'output page count and sizes' (($sizes -join ',') -ceq ($ExpectedSizes -join ',')) "actual=$($sizes -join ',') expected=$($ExpectedSizes -join ',')" | Out-Null
    } catch { Add-Result $Command 'output opens with PDFium' 'fail' $_.Exception.Message }
}

function Invoke-Scenario {
    param([string]$Name,[scriptblock]$Body)
    if ($Scenario.Count -gt 0 -and $Scenario -notcontains $Name) { return }
    Write-Host "--- $Name"
    try { & $Body }
    catch {
        Add-Result $Name 'scenario completed without an automation error' 'fail' $_.Exception.Message
        try { Save-Diag ("failure-" + $Name) (Get-WebRoot) } catch { }
        try { foreach ($dialog in Get-NativeDialogs) { Stop-NativeDialog $dialog } } catch { }
        try { Close-HtmlDialogs } catch { }
    }
}

function Select-OrganizerPages {
    param([int[]]$PageNumbers)
    $main = Get-WebRoot
    foreach ($number in $PageNumbers) { Click-Button -Name "Select page $number" }
}

function Open-Organizer {
    Open-Menu-Item 'Organize pages'
    Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Select all pages' | Out-Null
}

function Close-Organizer {
    $main = Get-WebRoot
    $close = Find-Ui -Root $main -Type 'Button' -Name '*Close tool*' -Like -EnabledOnly
    if ($close) { Invoke-Ui $close; Start-Sleep -Milliseconds 300 }
}

function Read-DialogText {
    param([string]$Pattern)
    $main = Get-WebRoot
    $match = @(Find-Ui -Root $main -Type 'Text' -Name $Pattern -Like -All)
    if ($match.Count -eq 0) { return $null }
    return $match[0].Current.Name
}

# ---------------------------------------------------------------------------------------------------------------
$null = New-Item -ItemType Directory -Path $WorkRoot
$inDir = Join-Path $WorkRoot 'in'; $outDir = Join-Path $WorkRoot 'out'; $appDir = Join-Path $WorkRoot 'app'
foreach ($dir in @($inDir, $outDir, $appDir)) { $null = New-Item -ItemType Directory -Path $dir }
$identifierDataDir = $null
try {
    $configPath = Join-Path (Split-Path -Parent $AppPath) 'resources'
    Copy-Item -LiteralPath $AppPath -Destination (Join-Path $appDir 'pdf-workstation.exe')
    if (Test-Path -LiteralPath $configPath) { Copy-Item -LiteralPath $configPath -Destination (Join-Path $appDir 'resources') -Recurse }

    $pdfA = Join-Path $inDir 'a.pdf'; $pdfB = Join-Path $inDir 'b.pdf'
    New-TestPdf $pdfA 3 612 792 'A'
    New-TestPdf $pdfB 2 300 400 'B'
    $png = Join-Path $inDir 'pic.png'
    $bitmap = [Drawing.Bitmap]::new(80, 40)
    $graphics = [Drawing.Graphics]::FromImage($bitmap); $graphics.Clear([Drawing.Color]::SteelBlue); $graphics.Dispose()
    $bitmap.Save($png, [Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Save((Join-Path $inDir 'pic2.png'), [Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    $pngB = Join-Path $inDir 'pic2.png'

    $startInfo = [Diagnostics.ProcessStartInfo]::new((Join-Path $appDir 'pdf-workstation.exe'))
    $startInfo.WorkingDirectory = $appDir
    $startInfo.UseShellExecute = $false
    $startInfo.EnvironmentVariables['WEBVIEW2_USER_DATA_FOLDER'] = (Join-Path $WorkRoot 'webview')
    $process = [Diagnostics.Process]::Start($startInfo)
    $script:Process = $process
    $script:AppPid = $process.Id
    Write-Host "App started (pid $($script:AppPid)) from $appDir; work root $WorkRoot"

    $limit = [DateTime]::UtcNow.AddSeconds($TimeoutSeconds)
    $main = $null
    do { $main = Get-MainWindow; if ($main) { break }; Start-Sleep -Milliseconds 250 } while ([DateTime]::UtcNow -lt $limit)
    if (-not $main) { throw 'Main window did not appear.' }
    $script:MainHwnd = [IntPtr]$main.Current.NativeWindowHandle
    Wait-Ui -Root $main -Type 'Button' -Name 'Menu' -Seconds ($TimeoutSeconds + 30) | Out-Null
    Add-Result 'launch' 'app window reachable through UI Automation' 'pass' "title='$($main.Current.Name)'"
    Assert-Check 'launch' 'no native dialog or disabled main window at start' ((Test-MainEnabled) -and @(Get-NativeDialogs).Count -eq 0) | Out-Null

    # ---- Open (needed to get documents; also covers the shared open dialog) --------------------------------
    Invoke-Scenario 'open' {
        $before = Get-AppState $outDir
        Open-Menu-Item $script:OpenItem
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'open' $dialog 'Open a PDF' $null 'PDF documents' | Out-Null
        Stop-NativeDialog $dialog
        Assert-StateUnchanged 'open' $before (Get-AppState $outDir)
        foreach ($pdf in @($pdfA, $pdfB)) {
            Open-Menu-Item $script:OpenItem
            $dialog = Wait-NativeDialog
            Complete-NativeDialog $dialog $pdf
            # The app keeps recovery records under its own app-data folder (not redirectable); a record left by an
            # earlier run can offer recovered edits first. Always take the original file.
            $limit = [DateTime]::UtcNow.AddSeconds($script:Deadline)
            do {
                $original = Find-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Open original' -EnabledOnly
                if ($original) { Invoke-Ui $original; Add-Result 'open' 'recovery prompt from an earlier record answered with Open original' 'pass' (Split-Path -Leaf $pdf) }
                $tab = Find-Ui -Root (Get-WebRoot) -Type 'Button' -Name (Split-Path -Leaf $pdf) -EnabledOnly
                if ($tab) { break }
                Start-Sleep -Milliseconds 200
            } while ([DateTime]::UtcNow -lt $limit)
            Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name (Split-Path -Leaf $pdf) -EnabledOnly | Out-Null
        }
        Assert-Check 'open' 'both fixture PDFs opened as tabs after typing their E: paths' ((Get-DocumentTabs) -join ';' -match 'a\.pdf' -and (Get-DocumentTabs) -join ';' -match 'b\.pdf') ((Get-DocumentTabs) -join ';') | Out-Null
    }

    # ---- Combine --------------------------------------------------------------------------------------------
    Invoke-Scenario 'combine' {
        Activate-Tab 'b.pdf'
        $toolsRow = Find-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Combine files*' -Like -EnabledOnly
        if (-not $toolsRow) { Click-Button -Name 'All tools'; $toolsRow = Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Combine files*' -Like -EnabledOnly }
        $outPath = Join-Path $outDir 'combined-out.pdf'
        # cancel path
        $before = Get-AppState $outDir
        Invoke-Ui $toolsRow; Wait-HtmlDialog 'Combine PDFs' | Out-Null
        $preview = Read-DialogText '*then*pages.'
        Assert-Check 'combine' 'in-app dialog lists b.pdf then a.pdf with 5 pages' ([bool]($preview -match 'b\.pdf.*then.*a\.pdf.*5 pages')) "$preview" | Out-Null
        Click-Button -Name 'Combine PDFs' -Last
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'combine' $dialog 'Save combined PDF as a new file' 'combined.pdf' 'PDF documents' | Out-Null
        Stop-NativeDialog $dialog
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Combine PDFs' -Gone -Seconds 10 | Out-Null
        Assert-StateUnchanged 'combine' $before (Get-AppState $outDir)
        # confirm path
        $toolsRow = Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Combine files*' -Like -EnabledOnly
        Invoke-Ui $toolsRow; Wait-HtmlDialog 'Combine PDFs' | Out-Null
        Click-Button -Name 'Combine PDFs' -Last
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $outPath
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'combined-out.pdf' -EnabledOnly | Out-Null
        Assert-PdfOutput 'combine' $outPath @('300x400','300x400','612x792','612x792','612x792')
        Assert-Check 'combine' 'main window enabled again after the dialog closed' (Test-MainEnabled) | Out-Null
    }

    # ---- Insert ---------------------------------------------------------------------------------------------
    Invoke-Scenario 'insert' {
        Activate-Tab 'b.pdf'
        Open-Organizer
        $outPath = Join-Path $outDir 'insert-out.pdf'
        $before = Get-AppState $outDir
        Click-Button -Name 'Insert pages'; Wait-HtmlDialog 'Insert Pages into a New Copy' | Out-Null
        Click-Button -Name 'Insert pages' -Last
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'insert' $dialog 'Save PDF with inserted pages as a new file' 'inserted-pages.pdf' 'PDF documents' | Out-Null
        Stop-NativeDialog $dialog
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Insert pages' -Seconds 10 | Out-Null
        Assert-StateUnchanged 'insert' $before (Get-AppState $outDir)
        Close-HtmlDialogs
        Click-Button -Name 'Insert pages'; Wait-HtmlDialog 'Insert Pages into a New Copy' | Out-Null
        Click-Button -Name 'Insert pages' -Last
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $outPath
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'insert-out.pdf' -EnabledOnly | Out-Null
        Assert-PdfOutput 'insert' $outPath @('612x792','612x792','612x792','300x400','300x400')
    }

    # ---- Replace --------------------------------------------------------------------------------------------
    Invoke-Scenario 'replace' {
        Activate-Tab 'b.pdf'
        Open-Organizer
        Select-OrganizerPages @(1)
        $outPath = Join-Path $outDir 'replace-out.pdf'
        $before = Get-AppState $outDir
        Click-Button -Name 'Replace pages'; Wait-HtmlDialog 'Replace Pages in a New Copy' | Out-Null
        Click-Button -Name 'Replace pages' -Last
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'replace' $dialog 'Save PDF with replaced pages as a new file' 'replaced-pages.pdf' 'PDF documents' | Out-Null
        Stop-NativeDialog $dialog
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Replace pages' -Seconds 10 | Out-Null
        Assert-StateUnchanged 'replace' $before (Get-AppState $outDir)
        Close-HtmlDialogs
        Click-Button -Name 'Replace pages'; Wait-HtmlDialog 'Replace Pages in a New Copy' | Out-Null
        Click-Button -Name 'Replace pages' -Last
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $outPath
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'replace-out.pdf' -EnabledOnly | Out-Null
        Assert-PdfOutput 'replace' $outPath @('612x792','612x792','612x792','300x400')
    }

    # ---- Split ----------------------------------------------------------------------------------------------
    Invoke-Scenario 'split' {
        Activate-Tab 'b.pdf'
        Open-Organizer
        $parent = Join-Path $outDir 'split-parent'
        New-Item -ItemType Directory -Path $parent | Out-Null
        $folder = Join-Path $parent 'Split PDFs'
        $before = Get-AppState $outDir
        Click-Button -Name 'Split'; Wait-HtmlDialog 'Split PDF' | Out-Null
        Click-Button -Name 'Split PDF' -Last
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'split' $dialog 'Choose a folder for split PDFs' $null $null -NoFilter | Out-Null
        $select = Get-DialogDefaultButton $dialog
        Assert-Check 'split' 'the chooser is a folder picker (default button selects a folder)' ($select -and $select.Current.Name -like '*Folder*') "button='$(if ($select) { $select.Current.Name })'" | Out-Null
        Stop-NativeDialog $dialog
        Assert-StateUnchanged 'split' $before (Get-AppState $outDir)
        Close-HtmlDialogs
        Click-Button -Name 'Split'; Wait-HtmlDialog 'Split PDF' | Out-Null
        Click-Button -Name 'Split PDF' -Last
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $parent
        Wait-HtmlDialog 'Split complete' | Out-Null
        $files = @(if (Test-Path -LiteralPath $folder) { @(Get-ChildItem -LiteralPath $folder -File | Sort-Object Name) })
        Assert-Check 'split' 'split folder created with one PDF per page' ($files.Count -eq 2) (($files | ForEach-Object Name) -join ', ') | Out-Null
        foreach ($file in $files) { Assert-PdfOutput 'split' $file.FullName @('300x400') }
        Close-HtmlDialogs
    }

    # ---- Save a copy after crop (crop itself has no native dialog) -------------------------------------------
    Invoke-Scenario 'crop-save' {
        Activate-Tab 'a.pdf'
        Open-Organizer
        Select-OrganizerPages @(1)
        Click-Button -Name 'Crop'; Wait-HtmlDialog 'Crop page 1' | Out-Null
        Set-UiValue (Wait-Ui -Root (Get-WebRoot) -Type 'Edit' -Name 'Left crop inset') '36'
        Set-UiValue (Wait-Ui -Root (Get-WebRoot) -Type 'Edit' -Name 'Right crop inset') '36'
        Add-Result 'crop' 'crop dialog is in-app only (no native dialog involved in applying a crop)' 'info' 'Apply crop is a pure IPC call; see main.rs crop_pages'
        $beforeDialogs = @(Get-NativeDialogs).Count
        Click-Button -Name 'Apply crop'
        Start-Sleep -Milliseconds 800
        Assert-Check 'crop' 'applying a crop opens no native dialog and leaves the main window enabled' ((@(Get-NativeDialogs).Count -eq $beforeDialogs) -and (Test-MainEnabled)) | Out-Null
        $outPath = Join-Path $outDir 'cropped-out.pdf'
        $before = Get-AppState $outDir
        Click-Button -Name 'Save a copy' -Last
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'save-copy' $dialog 'Save a copy as a new file' 'organized-copy.pdf' 'PDF documents' | Out-Null
        Stop-NativeDialog $dialog
        Assert-StateUnchanged 'save-copy' $before (Get-AppState $outDir)
        Click-Button -Name 'Save a copy' -Last
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $outPath
        $limit = [DateTime]::UtcNow.AddSeconds($script:Deadline)
        while (-not (Test-Path -LiteralPath $outPath) -and [DateTime]::UtcNow -lt $limit) { Start-Sleep -Milliseconds 200 }
        Assert-PdfOutput 'crop' $outPath @('540x792','612x792','612x792')
    }

    # ---- Export page image ------------------------------------------------------------------------------------
    Invoke-Scenario 'export-image' {
        Close-Organizer
        Activate-Tab 'b.pdf'
        $outPath = Join-Path $outDir 'page-out.png'
        $before = Get-AppState $outDir
        Open-Menu-Item 'Export page as image*'; Wait-HtmlDialog 'Export page as image' | Out-Null
        Click-Button -Name 'Export PNG'
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'export-image' $dialog 'Export page as PNG' 'b-page-1.png' 'PNG images' | Out-Null
        Stop-NativeDialog $dialog
        Assert-StateUnchanged 'export-image' $before (Get-AppState $outDir) -NoTabs
        Close-HtmlDialogs
        Assert-StateUnchanged 'export-image(after closing the dialog)' $before (Get-AppState $outDir)
        Open-Menu-Item 'Export page as image*'; Wait-HtmlDialog 'Export page as image' | Out-Null
        Click-Button -Name 'Export PNG'
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $outPath
        Wait-HtmlDialog 'Image export complete' | Out-Null
        if (Test-Path -LiteralPath $outPath) {
            $image = [Drawing.Image]::FromFile($outPath)
            try { Assert-Check 'export-image' 'PNG decodes with the expected size (300x400 pt at 150 DPI)' ($image.RawFormat.Guid -eq [Drawing.Imaging.ImageFormat]::Png.Guid -and $image.Width -eq 625 -and $image.Height -eq 834) "format=$($image.RawFormat) size=$($image.Width)x$($image.Height)" | Out-Null } finally { $image.Dispose() }
        } else { Add-Result 'export-image' 'output file exists' 'fail' $outPath }
        Close-HtmlDialogs
    }

    # ---- Create PDF from one image ------------------------------------------------------------------------------
    Invoke-Scenario 'create-image' {
        Activate-Tab 'b.pdf'
        $outPath = Join-Path $outDir 'create-out.pdf'
        $before = Get-AppState $outDir
        Click-Button -Name 'Create'; Wait-HtmlDialog 'Create a PDF from an image' | Out-Null
        Click-Button -Name 'Create PDF' -Last
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'create-image(pick)' $dialog 'Choose one PNG or JPEG image' $null 'PNG and JPEG images' | Out-Null
        Stop-NativeDialog $dialog
        Wait-Ui -Root (Get-WebRoot) -Type 'Text' -Name 'Creation canceled*' -Like -Seconds 10 | Out-Null
        Assert-StateUnchanged 'create-image(pick)' $before (Get-AppState $outDir) -NoTabs
        Click-Button -Name 'Create PDF' -Last
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $png
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'create-image(save)' $dialog 'Save image as a new PDF' 'pic.pdf' 'PDF documents' | Out-Null
        Stop-NativeDialog $dialog
        Wait-Ui -Root (Get-WebRoot) -Type 'Text' -Name 'Creation canceled*' -Like -Seconds 10 | Out-Null
        Assert-StateUnchanged 'create-image(save)' $before (Get-AppState $outDir) -NoTabs
        Close-HtmlDialogs
        Assert-StateUnchanged 'create-image(after closing the dialog)' $before (Get-AppState $outDir)
        Click-Button -Name 'Create'; Wait-HtmlDialog 'Create a PDF from an image' | Out-Null
        Click-Button -Name 'Create PDF' -Last
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $png
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $outPath
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'create-out.pdf' -EnabledOnly | Out-Null
        Assert-PdfOutput 'create-image' $outPath @('792x612')
    }

    # ---- Create PDF from several images -------------------------------------------------------------------------
    Invoke-Scenario 'create-images' {
        Activate-Tab 'b.pdf'
        $outPath = Join-Path $outDir 'images-out.pdf'
        $before = Get-AppState $outDir
        Open-Menu-Item 'Create PDF from images*'; Wait-HtmlDialog 'Create a PDF from images' | Out-Null
        Click-Button -Name 'Choose images'
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'create-images(pick)' $dialog 'Choose PNG or JPEG images' $null 'PNG and JPEG images' | Out-Null
        Stop-NativeDialog $dialog
        Assert-StateUnchanged 'create-images(pick)' $before (Get-AppState $outDir) -NoTabs
        Click-Button -Name 'Choose images'
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog ('"{0}" "{1}"' -f $png, $pngB)
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'Choose different images' -EnabledOnly | Out-Null
        Click-Button -Name 'Create PDF' -Last
        $dialog = Wait-NativeDialog
        Test-NativeDialogShell 'create-images(save)' $dialog 'Save images as a new PDF' 'images.pdf' 'PDF documents' | Out-Null
        Stop-NativeDialog $dialog
        Assert-StateUnchanged 'create-images(save)' $before (Get-AppState $outDir) -NoTabs
        Click-Button -Name 'Create PDF' -Last
        $dialog = Wait-NativeDialog
        Complete-NativeDialog $dialog $outPath
        Wait-Ui -Root (Get-WebRoot) -Type 'Button' -Name 'images-out.pdf' -EnabledOnly | Out-Null
        Assert-PdfOutput 'create-images' $outPath @('792x612','792x612')
    }
}
catch {
    Add-Result 'harness' 'harness completed' 'fail' $_.Exception.Message
}
finally {
    try {
        if ($script:Process -and -not $script:Process.HasExited) {
            [void]$script:Process.CloseMainWindow()
            [void]$script:Process.WaitForExit(5000)
        }
    } catch { }
    try { if ($script:Process -and -not $script:Process.HasExited) { $script:Process.Kill($true); $script:Process.WaitForExit(5000) | Out-Null } } catch { }
    $leftover = if ($script:AppPid -gt 0) { @(Get-Process -ErrorAction SilentlyContinue | Where-Object { $_.Id -eq $script:AppPid }) } else { @() }
    Add-Result 'cleanup' 'app process closed' $(if ($leftover.Count -eq 0) { 'pass' } else { 'fail' }) "pid=$($script:AppPid)"
    try {
        [IO.File]::WriteAllText((Join-Path $WorkRoot 'results.json'), ($script:Results | ConvertTo-Json -Depth 4), [Text.UTF8Encoding]::new($false))
    } catch { }
}

$failed = @($script:Results | Where-Object { $_.status -eq 'fail' })
$notAutomatable = @($script:Results | Where-Object { $_.status -eq 'not-automatable' })
Write-Host ''
Write-Host ("Summary: {0} pass, {1} fail, {2} not automatable. Work root: {3}" -f (@($script:Results | Where-Object { $_.status -eq 'pass' }).Count), $failed.Count, $notAutomatable.Count, $WorkRoot)
if ($failed.Count -gt 0) { exit 1 }
if ($Strict -and $notAutomatable.Count -gt 0) { Write-Host 'Strict: not-automatable checks present.'; exit 2 }
