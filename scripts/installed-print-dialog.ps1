$script:PrintPins = [ordered]@{
    PrinterName = 'Microsoft Print to PDF'
    TotalTimeoutMilliseconds = 240000
    PortHandoffTimeoutMilliseconds = 300000
    NativeDialogTimeoutMilliseconds = 120000
    OutputTimeoutMilliseconds = 45000
    UiElementMaximum = 2048
    NativeTopLevelDiagnosticMaximum = 32
    NativeChildDiagnosticMaximum = 64
    NativePrinterListControlId = 1
    OutputBytesMaximum = 64MB
    PdfRenderSize = 384
    PdfFingerprintSize = 64
    CorrelationMinimum = 0.82
    MeanAbsoluteDifferenceMaximum = 38.0
}
$script:InstalledPrintDialogScriptPath = $PSCommandPath

function Assert-PrintExactProperties {
    param($Value,[string[]]$Expected,[string]$Kind)
    $actual = @($Value.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    $wanted = @($Expected | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $wanted.Count -or (Compare-Object $wanted $actual -CaseSensitive)) {
        throw "$Kind has an unexpected or missing property."
    }
}

function Get-PrintCapabilityFacts {
    param([scriptblock]$PrinterProvider)
    if ($PrinterProvider) { $printers = @(& $PrinterProvider) } else { $printers = @(Get-CimInstance -ClassName Win32_Printer -ErrorAction Stop) }
    if ($printers.Count -gt 256) { throw 'Printer preflight returned an unsupported number of printers.' }
    $names = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $pdf = 0
    foreach ($printer in $printers) {
        $name = [string]$printer.Name
        if ([string]::IsNullOrWhiteSpace($name) -or $name.Length -gt 260 -or -not $names.Add($name)) {
            throw 'Printer preflight returned an invalid or duplicated printer identity.'
        }
        if ($name.Equals($script:PrintPins.PrinterName,[StringComparison]::OrdinalIgnoreCase)) { $pdf++ }
    }
    if ($pdf -gt 1) { throw 'Microsoft Print to PDF is duplicated.' }
    return [pscustomobject]@{
        printerCount = $printers.Count
        anyPrinterAvailable = $printers.Count -gt 0
        microsoftPrintToPdfAvailable = $pdf -eq 1
        featureInstallationAttempted = $false
    }
}

function Assert-PrintCapabilityFacts {
    param($Facts)
    Assert-PrintExactProperties -Value $Facts -Expected @('printerCount','anyPrinterAvailable','microsoftPrintToPdfAvailable','featureInstallationAttempted') -Kind 'Printer capability facts'
    if ([int]$Facts.printerCount -lt 0 -or [int]$Facts.printerCount -gt 256 -or
        $Facts.anyPrinterAvailable -isnot [bool] -or $Facts.microsoftPrintToPdfAvailable -isnot [bool] -or
        $Facts.featureInstallationAttempted -isnot [bool] -or $Facts.featureInstallationAttempted -or
        ([bool]$Facts.anyPrinterAvailable -ne ([int]$Facts.printerCount -gt 0)) -or
        ([bool]$Facts.microsoftPrintToPdfAvailable -and -not [bool]$Facts.anyPrinterAvailable)) {
        throw 'Printer capability facts are inconsistent.'
    }
}

function Get-PrintPhaseDeadline {
    param([Parameter(Mandatory = $true)][datetime]$TotalDeadline,[Parameter(Mandatory = $true)][int]$MaximumMilliseconds)
    if ($MaximumMilliseconds -le 0 -or [datetime]::UtcNow -ge $TotalDeadline) { throw 'Installed print verification exceeded its shared total deadline.' }
    $phase = [datetime]::UtcNow.AddMilliseconds($MaximumMilliseconds)
    if ($phase -gt $TotalDeadline) { return $TotalDeadline }
    return $phase
}

function Initialize-PrintUiAutomation {
    if (-not ('Windows.Automation.AutomationElement' -as [type])) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
    }
}

function Initialize-PrintNativeWindowInterop {
    if ('Smacrobat.PrintVerification.NativeWindows' -as [type]) { return }
    Add-Type -AssemblyName Accessibility
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.Globalization;
using System.Reflection;
using System.Runtime.InteropServices;
using System.Text;

namespace Smacrobat.PrintVerification {
    public sealed class NativeWindowRecord {
        public long HandleValue { get; set; }
        public long ParentHandleValue { get; set; }
        public int ProcessId { get; set; }
        public string ClassName { get; set; }
        public int ControlId { get; set; }
        public bool IsVisible { get; set; }
        public bool IsEnabled { get; set; }
        public int ButtonStyle { get; set; }
        public bool IsLabelPrint { get; set; }
        public bool IsLabelSave { get; set; }
        public bool IsLabelCancel { get; set; }
        public bool IsLabelFileName { get; set; }
        public bool IsLabelCurrentPage { get; set; }
        public bool IsLabelPrinterName { get; set; }
        public bool IsLabelYes { get; set; }
        public bool IsLabelNo { get; set; }
        public bool IsLabelOk { get; set; }
        public bool IsLabelContinue { get; set; }
    }

    public static class NativeWindows {
        private delegate bool EnumWindowProc(IntPtr hwnd, IntPtr state);
        [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowProc callback, IntPtr state);
        [DllImport("user32.dll")] private static extern bool EnumChildWindows(IntPtr parent, EnumWindowProc callback, IntPtr state);
        [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
        [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr hwnd);
        [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll")] private static extern bool IsWindowEnabled(IntPtr hwnd);
        [DllImport("user32.dll")] private static extern IntPtr GetParent(IntPtr hwnd);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(IntPtr hwnd, StringBuilder value, int maximum);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr hwnd, StringBuilder value, int maximum);
        [DllImport("user32.dll")] private static extern int GetDlgCtrlID(IntPtr hwnd);
        [DllImport("user32.dll", SetLastError = true)] private static extern int GetWindowLong(IntPtr hwnd, int index);
        [DllImport("kernel32.dll")] private static extern void SetLastError(uint errorCode);
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, IntPtr wParam, string lParam, uint flags, uint timeout, out IntPtr result);
        [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, IntPtr wParam, StringBuilder lParam, uint flags, uint timeout, out IntPtr result);
        [DllImport("user32.dll", SetLastError = true)] private static extern IntPtr SendMessageTimeout(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam, uint flags, uint timeout, out IntPtr result);
        [DllImport("oleacc.dll")] private static extern int AccessibleObjectFromWindow(IntPtr hwnd, uint objectId, ref Guid interfaceId, [MarshalAs(UnmanagedType.Interface)] out object accessible);
        [DllImport("oleacc.dll")] private static extern int AccessibleChildren([MarshalAs(UnmanagedType.Interface)] object container, int childStart, int childCount, [Out, MarshalAs(UnmanagedType.LPArray, SizeParamIndex = 2)] object[] children, out int obtained);
        [DllImport("oleacc.dll")] private static extern int WindowFromAccessibleObject([MarshalAs(UnmanagedType.Interface)] object accessible, out IntPtr hwnd);
        private const uint CB_GETCOUNT = 0x0146;
        private const uint CB_GETCURSEL = 0x0147;
        private const uint CB_SETCURSEL = 0x014E;
        private const uint CB_FINDSTRINGEXACT = 0x0158;
        private const uint BM_CLICK = 0x00F5;
        private const uint BM_GETCHECK = 0x00F0;
        private const uint WM_COMMAND = 0x0111;
        private const uint WM_SETTEXT = 0x000C;
        private const uint WM_CHAR = 0x0102;
        private const uint EM_SETSEL = 0x00B1;
        private const uint WM_GETTEXT = 0x000D;
        private const uint WM_GETTEXTLENGTH = 0x000E;
        private const int CBN_SELCHANGE = 1;
        private const int CB_ERR = -1;
        private const uint SMTO_ABORTIFHUNG = 0x0002;
        private const uint OBJID_CLIENT = 0xFFFFFFFC;
        private const int CHILDID_SELF = 0;
        private const int ROLE_SYSTEM_LISTITEM = 0x22;
        private const int STATE_SYSTEM_UNAVAILABLE = 0x00000001;
        private const int STATE_SYSTEM_SELECTED = 0x00000002;
        private const int STATE_SYSTEM_SELECTABLE = 0x00200000;
        private const int SELFLAG_TAKESELECTION = 0x2;
        private const int ACCESSIBLE_CHILD_MAXIMUM = 64;
        private static readonly Type AccessibleInterface = Type.GetType("Accessibility.IAccessible, Accessibility", true);
        private static readonly PropertyInfo AccessibleChildCount = AccessibleInterface.GetProperty("accChildCount");
        private static readonly PropertyInfo AccessibleName = AccessibleInterface.GetProperty("accName");
        private static readonly PropertyInfo AccessibleRole = AccessibleInterface.GetProperty("accRole");
        private static readonly PropertyInfo AccessibleState = AccessibleInterface.GetProperty("accState");
        private static readonly PropertyInfo AccessibleSelection = AccessibleInterface.GetProperty("accSelection");
        private static readonly MethodInfo AccessibleSelect = AccessibleInterface.GetMethod("accSelect");

        private sealed class AccessibleCandidate {
            public object Accessible;
            public object ChildReference;
            public bool UsesChildId;
            public int ChildId;
            public int State;
        }

        private static int GetAccessibleChildCount(object accessible) {
            return Convert.ToInt32(AccessibleChildCount.GetValue(accessible, null), CultureInfo.InvariantCulture);
        }

        private static object GetAccessibleIndexedProperty(PropertyInfo property, object accessible, object childReference) {
            return property.GetValue(accessible, new object[] { childReference });
        }

        private static bool IsOwned(IntPtr hwnd, int processId) {
            uint owner;
            GetWindowThreadProcessId(hwnd, out owner);
            return owner == (uint)processId;
        }

        private static int GetButtonStyle(IntPtr hwnd, string className) {
            if (!className.Equals("Button", StringComparison.OrdinalIgnoreCase)) return -1;
            SetLastError(0);
            int style = GetWindowLong(hwnd, -16);
            if (style == 0 && Marshal.GetLastWin32Error() != 0) return -1;
            return style & 0x0f;
        }

        private static string NormalizeAccessKeyMarkers(string value) {
            if (String.IsNullOrEmpty(value) || value.IndexOf('&') < 0) return value ?? String.Empty;
            var normalized = new StringBuilder(value.Length);
            for (int index = 0; index < value.Length; index++) {
                if (value[index] != '&') { normalized.Append(value[index]); continue; }
                if (index + 1 >= value.Length) { normalized.Append('&'); continue; }
                if (value[index + 1] == '&') { normalized.Append('&'); index++; }
            }
            return normalized.ToString();
        }

        public static bool AccessKeyLabelEquals(string observed, string expected) {
            return !String.IsNullOrEmpty(expected) && NormalizeAccessKeyMarkers(observed).Equals(expected, StringComparison.OrdinalIgnoreCase);
        }

        private static NativeWindowRecord Describe(IntPtr hwnd, IntPtr parent) {
            uint ownerProcessId;
            GetWindowThreadProcessId(hwnd, out ownerProcessId);
            var className = new StringBuilder(128);
            int length = GetClassName(hwnd, className, className.Capacity);
            string classValue = length > 0 ? className.ToString() : String.Empty;
            var label = new StringBuilder(260);
            int labelLength = GetWindowText(hwnd, label, label.Capacity);
            string labelValue = labelLength > 0 ? label.ToString() : String.Empty;
            return new NativeWindowRecord {
                HandleValue = hwnd.ToInt64(), ParentHandleValue = parent.ToInt64(), ProcessId = checked((int)ownerProcessId),
                ClassName = classValue, ControlId = GetDlgCtrlID(hwnd), IsVisible = IsWindowVisible(hwnd), IsEnabled = IsWindowEnabled(hwnd),
                ButtonStyle = GetButtonStyle(hwnd, classValue),
                IsLabelPrint = AccessKeyLabelEquals(labelValue, "Print"),
                IsLabelSave = AccessKeyLabelEquals(labelValue, "Save"),
                IsLabelCancel = AccessKeyLabelEquals(labelValue, "Cancel"),
                IsLabelFileName = AccessKeyLabelEquals(labelValue, "File name") || AccessKeyLabelEquals(labelValue, "File name:"),
                IsLabelCurrentPage = AccessKeyLabelEquals(labelValue, "Current page"),
                IsLabelPrinterName = AccessKeyLabelEquals(labelValue, "Microsoft Print to PDF"),
                IsLabelYes = AccessKeyLabelEquals(labelValue, "Yes"),
                IsLabelNo = AccessKeyLabelEquals(labelValue, "No"),
                IsLabelOk = AccessKeyLabelEquals(labelValue, "OK"),
                IsLabelContinue = AccessKeyLabelEquals(labelValue, "Continue")
            };
        }

        public static NativeWindowRecord[] Enumerate(int processId, long rootHandleValue, bool topLevelOnly) {
            if (processId <= 0) throw new ArgumentOutOfRangeException(nameof(processId));
            var result = new List<NativeWindowRecord>();
            if (rootHandleValue == 0) {
                EnumWindows((hwnd, state) => {
                    if (IsOwned(hwnd, processId)) result.Add(Describe(hwnd, IntPtr.Zero));
                    return true;
                }, IntPtr.Zero);
                if (topLevelOnly) return result.ToArray();
                var roots = result.ToArray();
                foreach (var root in roots) {
                    EnumChildWindows(new IntPtr(root.HandleValue), (hwnd, state) => {
                        if (IsOwned(hwnd, processId)) result.Add(Describe(hwnd, GetParent(hwnd)));
                        return true;
                    }, IntPtr.Zero);
                }
                return result.ToArray();
            }
            var rootHandle = new IntPtr(rootHandleValue);
            if (!IsWindow(rootHandle) || !IsOwned(rootHandle, processId)) throw new InvalidOperationException("Native print surface was not owned by the application process.");
            result.Add(Describe(rootHandle, IntPtr.Zero));
            if (!topLevelOnly) {
                EnumChildWindows(rootHandle, (hwnd, state) => {
                    if (IsOwned(hwnd, processId)) result.Add(Describe(hwnd, GetParent(hwnd)));
                    return true;
                }, IntPtr.Zero);
            }
            return result.ToArray();
        }

        private static uint Remaining(long deadline) {
            long remaining = deadline - Environment.TickCount64;
            if (remaining <= 0) throw new TimeoutException("Native combo message deadline expired.");
            return (uint)Math.Min(remaining, UInt32.MaxValue);
        }

        private static IntPtr Call(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam, long deadline) {
            IntPtr result;
            if (SendMessageTimeout(hwnd, message, wParam, lParam, SMTO_ABORTIFHUNG, Remaining(deadline), out result) == IntPtr.Zero) throw new TimeoutException("Native combo message timed out.");
            return result;
        }

        private static IntPtr CallText(IntPtr hwnd, uint message, IntPtr wParam, string lParam, long deadline) {
            IntPtr result;
            if (SendMessageTimeout(hwnd, message, wParam, lParam, SMTO_ABORTIFHUNG, Remaining(deadline), out result) == IntPtr.Zero) throw new TimeoutException("Native combo text message timed out.");
            return result;
        }

        private static IntPtr CallTextOutput(IntPtr hwnd, uint message, IntPtr wParam, StringBuilder lParam, long deadline) {
            IntPtr result;
            if (SendMessageTimeout(hwnd, message, wParam, lParam, SMTO_ABORTIFHUNG, Remaining(deadline), out result) == IntPtr.Zero) throw new TimeoutException("Native text read timed out.");
            return result;
        }

        private static string ReadWindowTextBounded(IntPtr hwnd, long deadline) {
            const int maximumCharacters = 32767;
            int beforeLength = Call(hwnd, WM_GETTEXTLENGTH, IntPtr.Zero, IntPtr.Zero, deadline).ToInt32();
            if (beforeLength < 0 || beforeLength >= maximumCharacters) throw new InvalidOperationException("Native control text length was invalid.");
            var value = new StringBuilder(maximumCharacters + 1);
            int copied = CallTextOutput(hwnd, WM_GETTEXT, new IntPtr(value.Capacity), value, deadline).ToInt32();
            if (copied < 0 || copied >= maximumCharacters || value.Length != copied) throw new InvalidOperationException("Native control text read was invalid.");
            int afterLength = Call(hwnd, WM_GETTEXTLENGTH, IntPtr.Zero, IntPtr.Zero, deadline).ToInt32();
            if (afterLength != beforeLength || afterLength != copied || value.Length != afterLength) throw new InvalidOperationException("Native control text changed during its bounded read.");
            return value.ToString();
        }

        private static void ValidateProcessStart(int processId, long processStartUtcTicks) {
            using (var process = Process.GetProcessById(processId)) {
                if (process.StartTime.ToUniversalTime().Ticks != processStartUtcTicks) throw new InvalidOperationException("Native save process identity changed.");
            }
        }

        private static bool IsOtherSaveContainerClass(string value) {
            return !String.IsNullOrEmpty(value) && !value.Equals("#32770", StringComparison.OrdinalIgnoreCase) && !value.Equals("Button", StringComparison.OrdinalIgnoreCase) &&
                !value.Equals("ComboBox", StringComparison.OrdinalIgnoreCase) && !value.Equals("ComboBoxEx32", StringComparison.OrdinalIgnoreCase) && !value.Equals("Edit", StringComparison.OrdinalIgnoreCase) &&
                !value.Equals("SysListView32", StringComparison.OrdinalIgnoreCase) && !value.Equals("DirectUIHWND", StringComparison.OrdinalIgnoreCase) && !value.Equals("Static", StringComparison.OrdinalIgnoreCase) &&
                !value.Equals("SysTabControl32", StringComparison.OrdinalIgnoreCase);
        }

        private static string ClassOf(IntPtr hwnd) {
            var value = new StringBuilder(128);
            if (GetClassName(hwnd, value, value.Capacity) <= 0) throw new InvalidOperationException("Native save control class was unavailable.");
            return value.ToString();
        }

        private static void ValidateSaveRoot(int processId, IntPtr root) {
            if (!IsWindow(root) || !IsOwned(root, processId) || !IsWindowVisible(root) || !IsWindowEnabled(root) || !ClassOf(root).Equals("#32770", StringComparison.OrdinalIgnoreCase)) throw new InvalidOperationException("Native save dialog root identity changed.");
        }

        private static void ValidateSaveButton(int processId, IntPtr root, IntPtr button, int controlId, int style, string label, long deadline) {
            ValidateSaveRoot(processId, root);
            if (!IsWindow(button) || !IsOwned(button, processId) || GetParent(button) != root || !IsWindowVisible(button) || !IsWindowEnabled(button) ||
                !ClassOf(button).Equals("Button", StringComparison.OrdinalIgnoreCase) || GetDlgCtrlID(button) != controlId || GetButtonStyle(button, "Button") != style ||
                !AccessKeyLabelEquals(ReadWindowTextBounded(button, deadline), label)) throw new InvalidOperationException("Native save dialog button identity changed.");
        }

        private static void ValidateSaveFileNameEdit(int processId, IntPtr root, IntPtr edit) {
            ValidateSaveRoot(processId, root);
            if (!IsWindow(edit) || !IsOwned(edit, processId) || !IsWindowVisible(edit) || !IsWindowEnabled(edit) || !ClassOf(edit).Equals("Edit", StringComparison.OrdinalIgnoreCase) || GetDlgCtrlID(edit) != 1001) throw new InvalidOperationException("Native save filename edit identity changed.");
            var visited = new HashSet<IntPtr>();
            IntPtr combo = GetParent(edit); IntPtr inner = GetParent(combo); IntPtr direct = GetParent(inner); IntPtr outer = GetParent(direct); IntPtr observedRoot = GetParent(outer);
            foreach (IntPtr hwnd in new[] { edit, combo, inner, direct, outer, observedRoot }) if (hwnd == IntPtr.Zero || !visited.Add(hwnd) || !IsWindow(hwnd) || !IsOwned(hwnd, processId)) throw new InvalidOperationException("Native save filename ancestry was invalid.");
            if (observedRoot != root || !ClassOf(combo).Equals("ComboBox", StringComparison.OrdinalIgnoreCase) || GetDlgCtrlID(combo) != 0 || !IsOtherSaveContainerClass(ClassOf(inner)) || GetDlgCtrlID(inner) != 0 ||
                !ClassOf(direct).Equals("DirectUIHWND", StringComparison.OrdinalIgnoreCase) || GetDlgCtrlID(direct) != 0 || !IsOtherSaveContainerClass(ClassOf(outer)) || GetDlgCtrlID(outer) != 0) throw new InvalidOperationException("Native save filename topology changed.");
        }

        public static bool SetSaveFileNameExact(int processId, long processStartUtcTicks, long rootHandleValue, long editHandleValue, string value, long deadline) {
            if (processId <= 0 || processStartUtcTicks <= 0 || rootHandleValue == 0 || editHandleValue == 0 || String.IsNullOrEmpty(value) || value.Length > 32767 || deadline <= Environment.TickCount64 || deadline - Environment.TickCount64 > 120000) throw new ArgumentOutOfRangeException();
            ValidateProcessStart(processId, processStartUtcTicks);
            var root = new IntPtr(rootHandleValue); var edit = new IntPtr(editHandleValue);
            ValidateSaveFileNameEdit(processId, root, edit);
            if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native save filename deadline expired.");
            if (CallText(edit, WM_SETTEXT, IntPtr.Zero, value, deadline) == IntPtr.Zero) throw new InvalidOperationException("Native save filename update was rejected.");
            if (!ReadWindowTextBounded(edit, deadline).Equals(value, StringComparison.Ordinal)) throw new InvalidOperationException("Native save filename was not exactly revalidated.");
            ValidateProcessStart(processId, processStartUtcTicks);
            ValidateSaveFileNameEdit(processId, root, edit);
            if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native save filename deadline expired.");
            return true;
        }

        public static bool TypeSaveFileNameExact(int processId, long processStartUtcTicks, long rootHandleValue, long editHandleValue, string value, long deadline) {
            if (processId <= 0 || processStartUtcTicks <= 0 || rootHandleValue == 0 || editHandleValue == 0 || String.IsNullOrEmpty(value) || value.Length > 1024 || deadline <= Environment.TickCount64 || deadline - Environment.TickCount64 > 120000) throw new ArgumentOutOfRangeException();
            foreach (char character in value) if (character < 0x20 || character == 0x7F) throw new ArgumentOutOfRangeException(nameof(value));
            ValidateProcessStart(processId, processStartUtcTicks);
            var root = new IntPtr(rootHandleValue); var edit = new IntPtr(editHandleValue);
            ValidateSaveFileNameEdit(processId, root, edit);
            Call(edit, EM_SETSEL, IntPtr.Zero, new IntPtr(-1), deadline);
            Call(edit, WM_CHAR, new IntPtr(0x08), new IntPtr(1), deadline);
            if (ReadWindowTextBounded(edit, deadline).Length != 0) throw new InvalidOperationException("Native save filename could not be cleared by keyboard input.");
            foreach (char character in value) {
                if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native save filename deadline expired.");
                Call(edit, WM_CHAR, new IntPtr(character), new IntPtr(1), deadline);
            }
            if (!ReadWindowTextBounded(edit, deadline).Equals(value, StringComparison.Ordinal)) throw new InvalidOperationException("Native save filename was not exactly revalidated after keyboard input.");
            ValidateProcessStart(processId, processStartUtcTicks);
            ValidateSaveFileNameEdit(processId, root, edit);
            if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native save filename deadline expired.");
            return true;
        }

        public static string ReadSaveFolderBounded(int processId, long processStartUtcTicks, long rootHandleValue, long deadline) {
            if (processId <= 0 || processStartUtcTicks <= 0 || rootHandleValue == 0 || deadline <= Environment.TickCount64 || deadline - Environment.TickCount64 > 120000) throw new ArgumentOutOfRangeException();
            ValidateProcessStart(processId, processStartUtcTicks);
            var root = new IntPtr(rootHandleValue);
            ValidateSaveRoot(processId, root);
            var crumbs = new List<IntPtr>();
            EnumChildWindows(root, (hwnd, state) => { if (IsOwned(hwnd, processId) && ClassOf(hwnd).Equals("ToolbarWindow32", StringComparison.Ordinal) && GetDlgCtrlID(hwnd) == 1001 && ClassOf(GetParent(hwnd)).Equals("Breadcrumb Parent", StringComparison.Ordinal)) crumbs.Add(hwnd); return crumbs.Count < 4; }, IntPtr.Zero);
            if (crumbs.Count != 1) return "";
            string text = ReadWindowTextBounded(crumbs[0], deadline);
            ValidateProcessStart(processId, processStartUtcTicks);
            ValidateSaveRoot(processId, root);
            const string prefix = "Address: ";
            return text.StartsWith(prefix, StringComparison.Ordinal) ? text.Substring(prefix.Length) : "";
        }

        public static string ReadSaveFileNameBounded(int processId, long processStartUtcTicks, long rootHandleValue, long editHandleValue, long deadline) {
            if (processId <= 0 || processStartUtcTicks <= 0 || rootHandleValue == 0 || editHandleValue == 0 || deadline <= Environment.TickCount64 || deadline - Environment.TickCount64 > 120000) throw new ArgumentOutOfRangeException();
            ValidateProcessStart(processId, processStartUtcTicks);
            var root = new IntPtr(rootHandleValue); var edit = new IntPtr(editHandleValue);
            ValidateSaveFileNameEdit(processId, root, edit);
            string text = ReadWindowTextBounded(edit, deadline);
            ValidateProcessStart(processId, processStartUtcTicks);
            ValidateSaveFileNameEdit(processId, root, edit);
            if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native save filename read deadline expired.");
            return text;
        }

        public static bool SaveFileNameEqualsExact(int processId, long processStartUtcTicks, long rootHandleValue, long editHandleValue, string value, long deadline) {
            if (processId <= 0 || processStartUtcTicks <= 0 || rootHandleValue == 0 || editHandleValue == 0 || String.IsNullOrEmpty(value) || value.Length > 32767 || deadline <= Environment.TickCount64 || deadline - Environment.TickCount64 > 120000) throw new ArgumentOutOfRangeException();
            ValidateProcessStart(processId, processStartUtcTicks);
            var root = new IntPtr(rootHandleValue); var edit = new IntPtr(editHandleValue);
            ValidateSaveFileNameEdit(processId, root, edit);
            bool matches = ReadWindowTextBounded(edit, deadline).Equals(value, StringComparison.Ordinal);
            ValidateProcessStart(processId, processStartUtcTicks);
            ValidateSaveFileNameEdit(processId, root, edit);
            if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native save filename diagnostic deadline expired.");
            return matches;
        }

        public static bool ClickSaveButtonExact(int processId, long processStartUtcTicks, long rootHandleValue, long buttonHandleValue, long deadline) {
            if (processId <= 0 || processStartUtcTicks <= 0 || rootHandleValue == 0 || buttonHandleValue == 0 || deadline <= Environment.TickCount64 || deadline - Environment.TickCount64 > 120000) throw new ArgumentOutOfRangeException();
            ValidateProcessStart(processId, processStartUtcTicks);
            var root = new IntPtr(rootHandleValue); var button = new IntPtr(buttonHandleValue);
            ValidateSaveButton(processId, root, button, 1, 1, "Save", deadline);
            if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native save button deadline expired.");
            ValidateProcessStart(processId, processStartUtcTicks);
            ValidateSaveRoot(processId, root);
            if (!IsWindow(button) || !IsOwned(button, processId) || GetParent(button) != root || !IsWindowVisible(button) || !IsWindowEnabled(button) ||
                !ClassOf(button).Equals("Button", StringComparison.OrdinalIgnoreCase) || GetDlgCtrlID(button) != 1 || GetButtonStyle(button, "Button") != 1) throw new InvalidOperationException("Native save dialog button topology changed before its action.");
            if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native save button deadline expired.");
            Call(button, BM_CLICK, IntPtr.Zero, IntPtr.Zero, deadline);
            return true;
        }

        public static bool ClickButton(int processId, long handleValue, bool requireChecked, int timeoutMilliseconds) {
            if (timeoutMilliseconds <= 0 || timeoutMilliseconds > 120000) throw new ArgumentOutOfRangeException(nameof(timeoutMilliseconds));
            var hwnd = new IntPtr(handleValue);
            if (!IsWindow(hwnd) || !IsOwned(hwnd, processId) || !IsWindowVisible(hwnd) || !IsWindowEnabled(hwnd)) return false;
            var className = new StringBuilder(64);
            if (GetClassName(hwnd, className, className.Capacity) <= 0 || !String.Equals(className.ToString(), "Button", StringComparison.Ordinal)) return false;
            long deadline = Environment.TickCount64 + timeoutMilliseconds;
            Call(hwnd, BM_CLICK, IntPtr.Zero, IntPtr.Zero, deadline);
            if (requireChecked && (!IsWindow(hwnd) || !IsOwned(hwnd, processId) || Call(hwnd, BM_GETCHECK, IntPtr.Zero, IntPtr.Zero, deadline).ToInt32() != 1)) return false;
            return true;
        }

        private static void EnsureAccessibleDeadline(long deadline) {
            if (Environment.TickCount64 >= deadline) throw new TimeoutException("Native accessible printer deadline expired.");
        }

        private static bool ExactClass(IntPtr hwnd, string expected) {
            var className = new StringBuilder(64);
            return GetClassName(hwnd, className, className.Capacity) > 0 && String.Equals(className.ToString(), expected, StringComparison.Ordinal);
        }

        private static void ValidateAccessibleTopology(int processId, long processStartUtcTicks, IntPtr dialogRoot, IntPtr listHwnd, int expectedControlId) {
            using (var process = Process.GetProcessById(processId)) {
                if (process.StartTime.ToUniversalTime().Ticks != processStartUtcTicks) throw new InvalidOperationException("Native accessible printer process identity changed.");
            }
            if (!IsWindow(dialogRoot) || !IsOwned(dialogRoot, processId) || !IsWindowVisible(dialogRoot) || !IsWindowEnabled(dialogRoot) || !ExactClass(dialogRoot, "#32770")) throw new InvalidOperationException("Native accessible printer dialog identity changed.");
            if (!IsWindow(listHwnd) || !IsOwned(listHwnd, processId) || !IsWindowVisible(listHwnd) || !IsWindowEnabled(listHwnd) || !ExactClass(listHwnd, "SysListView32") || GetDlgCtrlID(listHwnd) != expectedControlId) throw new InvalidOperationException("Native accessible printer list identity changed.");
            var visited = new HashSet<IntPtr>();
            IntPtr current = listHwnd;
            for (int depth = 0; depth < ACCESSIBLE_CHILD_MAXIMUM; depth++) {
                if (!visited.Add(current) || !IsWindow(current) || !IsOwned(current, processId)) throw new InvalidOperationException("Native accessible printer ancestry was invalid.");
                if (current == dialogRoot) return;
                current = GetParent(current);
                if (current == IntPtr.Zero) break;
            }
            throw new InvalidOperationException("Native accessible printer list did not reach its exact dialog root.");
        }

        private static bool SameComIdentity(object left, object right) {
            if (left == null || right == null || !Marshal.IsComObject(left) || !Marshal.IsComObject(right)) return false;
            IntPtr leftIdentity = IntPtr.Zero;
            IntPtr rightIdentity = IntPtr.Zero;
            try {
                leftIdentity = Marshal.GetIUnknownForObject(left);
                rightIdentity = Marshal.GetIUnknownForObject(right);
                return leftIdentity == rightIdentity;
            } finally {
                if (leftIdentity != IntPtr.Zero) Marshal.Release(leftIdentity);
                if (rightIdentity != IntPtr.Zero) Marshal.Release(rightIdentity);
            }
        }

        private static bool SameAccessibleReference(AccessibleCandidate candidate, object value) {
            if (candidate.UsesChildId) return value is int && (int)value == candidate.ChildId;
            return value != null && AccessibleInterface.IsInstanceOfType(value) && SameComIdentity(candidate.Accessible, value);
        }

        public static bool AccessibleEnumerationStable(int initialChildCount, int recheckedChildCount, int initialMatchCount, int recheckedMatchCount, bool sameCandidateIdentity) {
            return initialChildCount >= 0 && initialChildCount <= ACCESSIBLE_CHILD_MAXIMUM && initialChildCount == recheckedChildCount && initialMatchCount == 1 && recheckedMatchCount == 1 && sameCandidateIdentity;
        }

        private static bool SelectionValueMatches(AccessibleCandidate candidate, object value) {
            return SameAccessibleReference(candidate, value);
        }

        private static bool ExactSingleSelection(object root, AccessibleCandidate candidate, long deadline) {
            object selection = AccessibleSelection.GetValue(root, null);
            EnsureAccessibleDeadline(deadline);
            if (SelectionValueMatches(candidate, selection)) return true;
            var values = selection as object[];
            if (values != null) return values.Length == 1 && SelectionValueMatches(candidate, values[0]);
            var variants = selection as System.Runtime.InteropServices.ComTypes.IEnumVARIANT;
            if (variants == null) return false;
            var enumerated = new object[2];
            IntPtr fetchedPointer = Marshal.AllocCoTaskMem(sizeof(int));
            try {
                Marshal.WriteInt32(fetchedPointer, 0);
                int result = variants.Next(2, enumerated, fetchedPointer);
                EnsureAccessibleDeadline(deadline);
                int fetched = Marshal.ReadInt32(fetchedPointer);
                return result == 1 && fetched == 1 && SelectionValueMatches(candidate, enumerated[0]);
            } finally {
                Marshal.FreeCoTaskMem(fetchedPointer);
            }
        }

        private static AccessibleCandidate[] ReadAccessiblePrinterCandidates(object root, IntPtr boundList, string exactName, long deadline, out int stableChildCount) {
            EnsureAccessibleDeadline(deadline);
            int childCount = GetAccessibleChildCount(root);
            EnsureAccessibleDeadline(deadline);
            if (childCount < 0 || childCount > ACCESSIBLE_CHILD_MAXIMUM) throw new InvalidOperationException("Native accessible printer child count was unsupported.");
            if (childCount == 0) {
                if (GetAccessibleChildCount(root) != 0) throw new InvalidOperationException("Native accessible printer child count changed during enumeration.");
                EnsureAccessibleDeadline(deadline);
                stableChildCount = 0;
                return new AccessibleCandidate[0];
            }
            var children = new object[childCount];
            int obtained;
            int childrenResult = AccessibleChildren(root, 0, childCount, children, out obtained);
            EnsureAccessibleDeadline(deadline);
            if (childrenResult < 0 || obtained != childCount) throw new InvalidOperationException("Native accessible printer children were incomplete.");
            int recheckedCount = GetAccessibleChildCount(root);
            EnsureAccessibleDeadline(deadline);
            if (recheckedCount != childCount) throw new InvalidOperationException("Native accessible printer child count changed during enumeration.");
            var matches = new List<AccessibleCandidate>();
            foreach (object child in children) {
                EnsureAccessibleDeadline(deadline);
                object accessible;
                object childReference;
                bool usesChildId;
                int childId;
                if (child is int) {
                    childId = (int)child;
                    if (childId <= CHILDID_SELF) throw new InvalidOperationException("Native accessible printer child identifier was invalid.");
                    accessible = root;
                    childReference = childId;
                    usesChildId = true;
                } else {
                    accessible = child;
                    if (accessible == null || !AccessibleInterface.IsInstanceOfType(accessible)) throw new InvalidOperationException("Native accessible printer child type was unsupported.");
                    childReference = CHILDID_SELF;
                    usesChildId = false;
                    childId = 0;
                }
                string name = Convert.ToString(GetAccessibleIndexedProperty(AccessibleName, accessible, childReference), CultureInfo.InvariantCulture) ?? String.Empty;
                EnsureAccessibleDeadline(deadline);
                if (name.Length > 260) throw new InvalidOperationException("Native accessible printer child name was oversized.");
                int role = Convert.ToInt32(GetAccessibleIndexedProperty(AccessibleRole, accessible, childReference), CultureInfo.InvariantCulture);
                EnsureAccessibleDeadline(deadline);
                int state = Convert.ToInt32(GetAccessibleIndexedProperty(AccessibleState, accessible, childReference), CultureInfo.InvariantCulture);
                EnsureAccessibleDeadline(deadline);
                if (!name.Equals(exactName, StringComparison.OrdinalIgnoreCase)) continue;
                if (role != ROLE_SYSTEM_LISTITEM || (state & STATE_SYSTEM_SELECTABLE) == 0 || (state & STATE_SYSTEM_UNAVAILABLE) != 0) throw new InvalidOperationException("Exact native accessible printer child was not an available selectable list item.");
                matches.Add(new AccessibleCandidate { Accessible = accessible, ChildReference = childReference, UsesChildId = usesChildId, ChildId = childId, State = state });
            }
            stableChildCount = childCount;
            return matches.ToArray();
        }

        public static bool SelectAccessibleChildExact(int processId, long processStartUtcTicks, long dialogRootHandleValue, long handleValue, int expectedControlId, string exactName, long deadlineTickCount) {
            if (processId <= 0 || processStartUtcTicks <= 0 || dialogRootHandleValue == 0 || handleValue == 0 || expectedControlId <= 0 || String.IsNullOrEmpty(exactName) || exactName.Length > 260) throw new ArgumentOutOfRangeException();
            long remaining = deadlineTickCount - Environment.TickCount64;
            if (remaining <= 0 || remaining > 120000) throw new ArgumentOutOfRangeException(nameof(deadlineTickCount));
            var dialogRoot = new IntPtr(dialogRootHandleValue);
            var listHwnd = new IntPtr(handleValue);
            long deadline = deadlineTickCount;
            ValidateAccessibleTopology(processId, processStartUtcTicks, dialogRoot, listHwnd, expectedControlId);
            EnsureAccessibleDeadline(deadline);
            var accessibleId = AccessibleInterface.GUID;
            object root;
            if (AccessibleObjectFromWindow(listHwnd, OBJID_CLIENT, ref accessibleId, out root) < 0 || root == null) throw new InvalidOperationException("Native accessible printer root was unavailable.");
            IntPtr rootWindow;
            if (WindowFromAccessibleObject(root, out rootWindow) < 0 || rootWindow != listHwnd) throw new InvalidOperationException("Native accessible printer root identity changed.");
            int initialChildCount;
            AccessibleCandidate[] matches = ReadAccessiblePrinterCandidates(root, listHwnd, exactName, deadline, out initialChildCount);
            if (matches.Length == 0) return false;
            if (matches.Length != 1) throw new InvalidOperationException("Native accessible printer name was ambiguous.");
            ValidateAccessibleTopology(processId, processStartUtcTicks, dialogRoot, listHwnd, expectedControlId);
            EnsureAccessibleDeadline(deadline);
            int preSelectChildCount;
            AccessibleCandidate[] preSelectMatches = ReadAccessiblePrinterCandidates(root, listHwnd, exactName, deadline, out preSelectChildCount);
            bool preSelectIdentityMatches = preSelectMatches.Length == 1 && SameAccessibleReference(matches[0], preSelectMatches[0].UsesChildId ? (object)preSelectMatches[0].ChildId : preSelectMatches[0].Accessible);
            if (!AccessibleEnumerationStable(initialChildCount, preSelectChildCount, matches.Length, preSelectMatches.Length, preSelectIdentityMatches)) throw new InvalidOperationException("Native accessible printer identity changed immediately before selection.");
            ValidateAccessibleTopology(processId, processStartUtcTicks, dialogRoot, listHwnd, expectedControlId);
            EnsureAccessibleDeadline(deadline);
            AccessibleCandidate selected = preSelectMatches[0];
            AccessibleSelect.Invoke(selected.Accessible, new object[] { SELFLAG_TAKESELECTION, selected.ChildReference });
            EnsureAccessibleDeadline(deadline);
            int finalChildCount;
            AccessibleCandidate[] rechecked = ReadAccessiblePrinterCandidates(root, listHwnd, exactName, deadline, out finalChildCount);
            bool finalIdentityMatches = rechecked.Length == 1 && SameAccessibleReference(selected, rechecked[0].UsesChildId ? (object)rechecked[0].ChildId : rechecked[0].Accessible);
            if (!AccessibleEnumerationStable(preSelectChildCount, finalChildCount, preSelectMatches.Length, rechecked.Length, finalIdentityMatches)) throw new InvalidOperationException("Native accessible printer identity changed after selection.");
            if ((rechecked[0].State & STATE_SYSTEM_SELECTED) == 0 || !ExactSingleSelection(root, rechecked[0], deadline)) throw new InvalidOperationException("Native accessible printer selection was not exactly revalidated.");
            ValidateAccessibleTopology(processId, processStartUtcTicks, dialogRoot, listHwnd, expectedControlId);
            EnsureAccessibleDeadline(deadline);
            return true;
        }

        public static bool IsUniqueExactIndex(int count, int first, int next) {
            return count >= 0 && count <= 256 && first >= 0 && first < count && next == first;
        }

        public static bool AllEqualToIndex(int expected, params int[] observed) {
            if (expected < 0 || observed == null || observed.Length == 0) return false;
            foreach (int value in observed) if (value != expected) return false;
            return true;
        }

        private static int ExactComboIndex(int processId, long handleValue, string value, long deadline) {
            if (String.IsNullOrEmpty(value) || value.Length > 260) return CB_ERR;
            var hwnd = new IntPtr(handleValue);
            if (!IsWindow(hwnd) || !IsOwned(hwnd, processId)) return CB_ERR;
            var className = new StringBuilder(64);
            if (GetClassName(hwnd, className, className.Capacity) <= 0 || !String.Equals(className.ToString(), "ComboBox", StringComparison.Ordinal)) return CB_ERR;
            int count = Call(hwnd, CB_GETCOUNT, IntPtr.Zero, IntPtr.Zero, deadline).ToInt32();
            if (count < 0 || count > 256) return CB_ERR;
            int match = CallText(hwnd, CB_FINDSTRINGEXACT, new IntPtr(-1), value, deadline).ToInt32();
            int next = CallText(hwnd, CB_FINDSTRINGEXACT, new IntPtr(match), value, deadline).ToInt32();
            if (!IsUniqueExactIndex(count, match, next)) return CB_ERR;
            return match;
        }

        public static bool ComboContainsExact(int processId, long handleValue, string value, int timeoutMilliseconds) {
            if (timeoutMilliseconds <= 0 || timeoutMilliseconds > 120000) throw new ArgumentOutOfRangeException(nameof(timeoutMilliseconds));
            return ExactComboIndex(processId, handleValue, value, Environment.TickCount64 + timeoutMilliseconds) != CB_ERR;
        }

        public static bool SelectComboExact(int processId, long handleValue, string value, int timeoutMilliseconds) {
            if (timeoutMilliseconds <= 0 || timeoutMilliseconds > 120000) throw new ArgumentOutOfRangeException(nameof(timeoutMilliseconds));
            long deadline = Environment.TickCount64 + timeoutMilliseconds;
            int index = ExactComboIndex(processId, handleValue, value, deadline);
            if (index == CB_ERR) return false;
            var hwnd = new IntPtr(handleValue);
            int recheckedIndex = ExactComboIndex(processId, handleValue, value, deadline);
            if (!AllEqualToIndex(index, recheckedIndex)) return false;
            if (Call(hwnd, CB_SETCURSEL, new IntPtr(index), IntPtr.Zero, deadline).ToInt32() == CB_ERR) return false;
            if (!AllEqualToIndex(index, ExactComboIndex(processId, handleValue, value, deadline), Call(hwnd, CB_GETCURSEL, IntPtr.Zero, IntPtr.Zero, deadline).ToInt32())) return false;
            var parent = GetParent(hwnd);
            int controlId = GetDlgCtrlID(hwnd);
            if (parent == IntPtr.Zero || !IsOwned(parent, processId) || controlId <= 0) return false;
            long command = ((long)CBN_SELCHANGE << 16) | ((uint)controlId & 0xffffU);
            Call(parent, WM_COMMAND, new IntPtr(command), hwnd, deadline);
            if (!AllEqualToIndex(index, ExactComboIndex(processId, handleValue, value, deadline), Call(hwnd, CB_GETCURSEL, IntPtr.Zero, IntPtr.Zero, deadline).ToInt32())) return false;
            return true;
        }
    }
}
'@
}

function New-PrintTargetUiCondition {
    param([Parameter(Mandatory = $true)][int]$ProcessId)
    Initialize-PrintUiAutomation
    $process = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty,$ProcessId)
    $buttonType = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Button)
    $buttonNames = [Windows.Automation.OrCondition]::new([Windows.Automation.Condition[]]@(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,'Cancel'),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,'Print'),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,'Save')
    ))
    $radioType = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::RadioButton)
    $currentPageNames = [Windows.Automation.OrCondition]::new([Windows.Automation.Condition[]]@(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,'Current Page'),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,'Current page')
    ))
    $printerTypes = [Windows.Automation.OrCondition]::new([Windows.Automation.Condition[]]@(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::ListItem),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Button),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::RadioButton),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::ComboBox)
    ))
    $printerName = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$script:PrintPins.PrinterName)
    $comboType = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::ComboBox)
    $editType = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Edit)
    $filenameIdentity = [Windows.Automation.OrCondition]::new([Windows.Automation.Condition[]]@(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty,'1001'),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::AutomationIdProperty,'FileNameControlHost'),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,'File name:'),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,'File name')
    ))
    $targets = [Windows.Automation.OrCondition]::new([Windows.Automation.Condition[]]@(
        [Windows.Automation.AndCondition]::new([Windows.Automation.Condition[]]@($buttonType,$buttonNames)),
        [Windows.Automation.AndCondition]::new([Windows.Automation.Condition[]]@($radioType,$currentPageNames)),
        [Windows.Automation.AndCondition]::new([Windows.Automation.Condition[]]@($printerTypes,$printerName)),
        $comboType,
        [Windows.Automation.AndCondition]::new([Windows.Automation.Condition[]]@($editType,$filenameIdentity))
    ))
    return [Windows.Automation.AndCondition]::new([Windows.Automation.Condition[]]@($process,$targets))
}

function New-PrintSurfaceUiCondition {
    param([Parameter(Mandatory = $true)][int]$ProcessId)
    Initialize-PrintUiAutomation
    $process = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty,$ProcessId)
    $surfaceTypes = [Windows.Automation.OrCondition]::new([Windows.Automation.Condition[]]@(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Window),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,[Windows.Automation.ControlType]::Pane)
    ))
    return [Windows.Automation.AndCondition]::new([Windows.Automation.Condition[]]@($process,$surfaceTypes))
}

function Get-ProcessUiElements {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [switch]$WindowsOnly,
        [switch]$SurfaceContainersOnly,
        $RootElement,
        [datetime]$Deadline = [datetime]::MaxValue,
        [scriptblock]$DesktopProvider,
        [scriptblock]$FindAllProvider
    )
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI enumeration deadline expired.' }
    Initialize-PrintUiAutomation
    $processCondition = [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ProcessIdProperty,$ProcessId)
    $descendantCondition = if ($SurfaceContainersOnly) { New-PrintSurfaceUiCondition -ProcessId $ProcessId } else { New-PrintTargetUiCondition -ProcessId $ProcessId }
    if ($null -ne $RootElement) {
        Assert-ProcessUiElement -Element $RootElement -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI enumeration deadline expired.' }
        $collection = @(if ($FindAllProvider) { & $FindAllProvider $RootElement ([Windows.Automation.TreeScope]::Descendants) $descendantCondition } else { $RootElement.FindAll([Windows.Automation.TreeScope]::Descendants,$descendantCondition) | ForEach-Object { $_ } })
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI enumeration exceeded its deadline.' }
        if ($collection.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native print UI exceeded its bounded element count.' }
        foreach ($element in $collection) {
            Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI enumeration exceeded its deadline.' }
        }
        return $collection
    }
    $desktop = if ($DesktopProvider) { & $DesktopProvider } else { [Windows.Automation.AutomationElement]::RootElement }
    $topLevel = @(if ($FindAllProvider) { & $FindAllProvider $desktop ([Windows.Automation.TreeScope]::Children) $processCondition } else { $desktop.FindAll([Windows.Automation.TreeScope]::Children,$processCondition) | ForEach-Object { $_ } })
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI top-level enumeration exceeded its deadline.' }
    if ($topLevel.Count -lt 1 -or $topLevel.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native print UI top-level enumeration was missing or oversized.' }
    foreach ($element in $topLevel) {
        Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI top-level enumeration exceeded its deadline.' }
        if ((Get-UiControlTypeName -Element $element) -notin @('ControlType.Window','ControlType.Pane')) { throw 'Native print UI top-level root had an unsupported control type.' }
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI top-level enumeration exceeded its deadline.' }
    }
    if ($WindowsOnly) { return $topLevel }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $elements = [Collections.Generic.List[object]]::new()
    foreach ($root in $topLevel) {
        $rootIdentity = Get-ProcessUiRuntimeIdentity -Element $root -ProcessId $ProcessId -Deadline $Deadline
        if ($seen.Add($rootIdentity)) { $elements.Add($root) }
        $descendants = @(if ($FindAllProvider) { & $FindAllProvider $root ([Windows.Automation.TreeScope]::Descendants) $descendantCondition } else { $root.FindAll([Windows.Automation.TreeScope]::Descendants,$descendantCondition) | ForEach-Object { $_ } })
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI rooted descendant enumeration exceeded its deadline.' }
        if ($elements.Count + $descendants.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native print UI exceeded its bounded element count.' }
        foreach ($element in $descendants) {
            Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI rooted descendant enumeration exceeded its deadline.' }
            $identity = Get-ProcessUiRuntimeIdentity -Element $element -ProcessId $ProcessId -Deadline $Deadline
            if ($seen.Add($identity)) { $elements.Add($element) }
        }
    }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI enumeration exceeded its deadline.' }
    return @($elements)
}

function Assert-ProcessUiElement {
    param([Parameter(Mandatory = $true)]$Element,[Parameter(Mandatory = $true)][int]$ProcessId)
    if ([int]$Element.Current.ProcessId -ne $ProcessId) { throw 'Native print UI element escaped the owned application process.' }
}

function Get-UiControlTypeName {
    param([Parameter(Mandatory = $true)]$Element)
    try { return [string]$Element.Current.ControlType.ProgrammaticName } catch { return '' }
}

function Get-SanitizedProcessUiStructureReceipt {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][scriptblock]$ElementProvider
    )
    $topLevel = @(& $ElementProvider $ProcessId $true)
    $elements = @(& $ElementProvider $ProcessId $false)
    if ($topLevel.Count -gt $script:PrintPins.UiElementMaximum -or $elements.Count -gt $script:PrintPins.UiElementMaximum) {
        throw 'Native print UI diagnostics exceeded the bounded element count.'
    }
    foreach ($element in $topLevel) { Assert-ProcessUiElement -Element $element -ProcessId $ProcessId }
    $counts = [ordered]@{
        window = 0; pane = 0; button = 0; radioButton = 0; comboBox = 0
        edit = 0; list = 0; listItem = 0; other = 0
    }
    foreach ($element in $elements) {
        Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
        switch (Get-UiControlTypeName -Element $element) {
            'ControlType.Window' { $counts.window++; break }
            'ControlType.Pane' { $counts.pane++; break }
            'ControlType.Button' { $counts.button++; break }
            'ControlType.RadioButton' { $counts.radioButton++; break }
            'ControlType.ComboBox' { $counts.comboBox++; break }
            'ControlType.Edit' { $counts.edit++; break }
            'ControlType.List' { $counts.list++; break }
            'ControlType.ListItem' { $counts.listItem++; break }
            default { $counts.other++ }
        }
    }
    return [pscustomobject][ordered]@{
        inventoryStatus = 'available'
        topLevelWindowCount = [int]$topLevel.Count
        processElementCount = [int]$elements.Count
        windowCount = [int]$counts.window
        paneCount = [int]$counts.pane
        buttonCount = [int]$counts.button
        radioButtonCount = [int]$counts.radioButton
        comboBoxCount = [int]$counts.comboBox
        editCount = [int]$counts.edit
        listCount = [int]$counts.list
        listItemCount = [int]$counts.listItem
        otherCount = [int]$counts.other
    }
}

function Get-SanitizedProcessUiStructureJson {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [scriptblock]$ElementProvider,
        [switch]$DeadlineExpired
    )
    $unavailable = '{"inventoryStatus":"unavailable","topLevelWindowCount":-1,"processElementCount":-1,"windowCount":-1,"paneCount":-1,"buttonCount":-1,"radioButtonCount":-1,"comboBoxCount":-1,"editCount":-1,"listCount":-1,"listItemCount":-1,"otherCount":-1}'
    if ($DeadlineExpired) { return $unavailable }
    try {
        if (-not $ElementProvider) { throw 'A bounded UI element provider is required before the deadline.' }
        return (Get-SanitizedProcessUiStructureReceipt -ProcessId $ProcessId -ElementProvider $ElementProvider | ConvertTo-Json -Compress)
    } catch {
        return $unavailable
    }
}

function Get-SanitizedObservedUiStructureJson {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Elements,
        [Parameter(Mandatory = $true)][ValidateSet('top-level','process-descendants')][string]$Scope
    )
    try {
        if ($Elements.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Observed native print UI exceeded the bounded element count.' }
        $counts = [ordered]@{
            window = 0; pane = 0; button = 0; radioButton = 0; comboBox = 0
            edit = 0; list = 0; listItem = 0; other = 0
        }
        foreach ($element in $Elements) {
            Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
            switch (Get-UiControlTypeName -Element $element) {
                'ControlType.Window' { $counts.window++; break }
                'ControlType.Pane' { $counts.pane++; break }
                'ControlType.Button' { $counts.button++; break }
                'ControlType.RadioButton' { $counts.radioButton++; break }
                'ControlType.ComboBox' { $counts.comboBox++; break }
                'ControlType.Edit' { $counts.edit++; break }
                'ControlType.List' { $counts.list++; break }
                'ControlType.ListItem' { $counts.listItem++; break }
                default { $counts.other++ }
            }
        }
        $receipt = [ordered]@{
            inventoryStatus = if ($Scope -ceq 'top-level') { 'top-level-observed' } else { 'process-descendants-observed' }
            topLevelWindowCount = if ($Scope -ceq 'top-level') { [int]$Elements.Count } else { -1 }
            processElementCount = if ($Scope -ceq 'process-descendants') { [int]$Elements.Count } else { -1 }
            windowCount = [int]$counts.window
            paneCount = [int]$counts.pane
            buttonCount = [int]$counts.button
            radioButtonCount = [int]$counts.radioButton
            comboBoxCount = [int]$counts.comboBox
            editCount = [int]$counts.edit
            listCount = [int]$counts.list
            listItemCount = [int]$counts.listItem
            otherCount = [int]$counts.other
        }
        return ([pscustomobject]$receipt | ConvertTo-Json -Compress)
    } catch {
        return Get-SanitizedProcessUiStructureJson -ProcessId $ProcessId -DeadlineExpired
    }
}

function Find-ProcessUiElement {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][string[]]$Names,
        [string[]]$ControlTypes = @(),
        [switch]$WindowsOnly,
        [switch]$AllowNone,
        [ref]$ObservedElements,
        $RootElement
    )
    $foundElements = @()
    $elements = @(Get-ProcessUiElements -ProcessId $ProcessId -WindowsOnly:$WindowsOnly -RootElement $RootElement)
    if ($null -ne $ObservedElements) { $ObservedElements.Value = $elements }
    foreach ($element in $elements) {
        try {
            Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
            $name = [string]$element.Current.Name
            $type = Get-UiControlTypeName -Element $element
            if (@($Names | Where-Object { $name.Equals($_,[StringComparison]::OrdinalIgnoreCase) }).Count -eq 1 -and
                ($ControlTypes.Count -eq 0 -or $ControlTypes.Contains($type))) {
                $foundElements += $element
            }
        } catch { }
    }
    if ($foundElements.Count -eq 0 -and $AllowNone) { return $null }
    if ($foundElements.Count -ne 1) { throw 'Native print UI target was missing or ambiguous.' }
    return $foundElements[0]
}

function Find-ProcessUiElementByAutomationId {
    param([Parameter(Mandatory = $true)][int]$ProcessId,[Parameter(Mandatory = $true)][string[]]$AutomationIds,[Parameter(Mandatory = $true)][string[]]$ControlTypes,$RootElement)
    $foundElements = @()
    foreach ($element in @(Get-ProcessUiElements -ProcessId $ProcessId -RootElement $RootElement)) {
        try {
            Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
            $automationId = [string]$element.Current.AutomationId
            $type = Get-UiControlTypeName -Element $element
            if ($AutomationIds.Contains($automationId) -and $ControlTypes.Contains($type)) { $foundElements += $element }
        } catch { }
    }
    if ($foundElements.Count -ne 1) { throw 'Native print UI automation identifier was missing or ambiguous.' }
    return $foundElements[0]
}

function Wait-ProcessUiElement {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][string[]]$Names,
        [string[]]$ControlTypes = @(),
        [switch]$WindowsOnly,
        $RootElement,
        [Parameter(Mandatory = $true)][ValidateSet('first-print-dialog','second-print-dialog','current-page-control','save-output-dialog')][string]$Stage,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $structure = Get-SanitizedProcessUiStructureJson -ProcessId $ProcessId -DeadlineExpired
    while ([datetime]::UtcNow -lt $Deadline) {
        $observed = $null
        $observedScope = if ($WindowsOnly) { 'top-level' } else { 'process-descendants' }
        try {
            $match = Find-ProcessUiElement -ProcessId $ProcessId -Names $Names -ControlTypes $ControlTypes -WindowsOnly:$WindowsOnly -AllowNone -ObservedElements ([ref]$observed) -RootElement $RootElement
            if ($null -ne $match) { return $match }
        } catch {
            if ([datetime]::UtcNow -ge $Deadline) { break }
        } finally {
            if ($observed -is [array]) {
                $structure = Get-SanitizedObservedUiStructureJson -ProcessId $ProcessId -Elements $observed -Scope $observedScope
            }
        }
        if ([datetime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 150 }
    }
    throw "Native print UI stage '$Stage' timed out; uiStructure=$structure."
}

function Get-ProcessTopLevelUiSnapshot {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$ElementProvider
    )
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI snapshot deadline expired.' }
    $elements = @(if ($ElementProvider) { & $ElementProvider $ProcessId $true } else { Get-ProcessUiElements -ProcessId $ProcessId -WindowsOnly -Deadline $Deadline })
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI snapshot exceeded its deadline.' }
    if ($elements.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native top-level UI snapshot exceeded the bounded element count.' }
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $snapshot = @()
    foreach ($element in $elements) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI snapshot exceeded its deadline.' }
        Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI snapshot exceeded its deadline.' }
        try { $runtime = @($element.GetRuntimeId()) } catch { throw 'Native top-level UI runtime identity was unavailable.' }
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI snapshot exceeded its deadline.' }
        if ($runtime.Count -lt 1 -or $runtime.Count -gt 64 -or @($runtime | Where-Object { $_ -isnot [int] }).Count -ne 0) {
            throw 'Native top-level UI runtime identity was invalid.'
        }
        $identity = ($runtime | ForEach-Object { ([int]$_).ToString([Globalization.CultureInfo]::InvariantCulture) }) -join ':'
        if (-not $seen.Add($identity)) { throw 'Native top-level UI runtime identity was duplicated.' }
        $controlType = Get-UiControlTypeName -Element $element
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI snapshot exceeded its deadline.' }
        $snapshot += [pscustomobject]@{ runtimeIdentity=$identity;controlType=$controlType;element=$element }
    }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI snapshot exceeded its deadline.' }
    return $snapshot
}

function Get-ProcessUiRuntimeIdentity {
    param(
        [Parameter(Mandatory = $true)]$Element,
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI identity deadline expired.' }
    Assert-ProcessUiElement -Element $Element -ProcessId $ProcessId
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI identity deadline expired.' }
    try { $runtime = @($Element.GetRuntimeId()) } catch { throw 'Native process UI runtime identity was unavailable.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI identity deadline expired.' }
    if ($runtime.Count -lt 1 -or $runtime.Count -gt 64 -or @($runtime | Where-Object { $_ -isnot [int] }).Count -ne 0) {
        throw 'Native process UI runtime identity was invalid.'
    }
    return (($runtime | ForEach-Object { ([int]$_).ToString([Globalization.CultureInfo]::InvariantCulture) }) -join ':')
}

function Test-PrintTargetUiElement {
    param([Parameter(Mandatory = $true)]$Element,[Parameter(Mandatory = $true)][datetime]$Deadline)
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window target classification deadline expired.' }
    $type = Get-UiControlTypeName -Element $Element
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window target classification deadline expired.' }
    $name = [string]$Element.Current.Name
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window target classification deadline expired.' }
    $automationId = [string]$Element.Current.AutomationId
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window target classification deadline expired.' }
    return (
        ($type -ceq 'ControlType.Button' -and $name -in @('Cancel','Print','Save')) -or
        ($type -ceq 'ControlType.RadioButton' -and $name -in @('Current Page','Current page')) -or
        ($type -in @('ControlType.ListItem','ControlType.Button','ControlType.RadioButton','ControlType.ComboBox') -and $name.Equals($script:PrintPins.PrinterName,[StringComparison]::OrdinalIgnoreCase)) -or
        $type -in @('ControlType.ComboBox','ControlType.List') -or
        ($type -ceq 'ControlType.Edit' -and ($automationId -in @('1001','FileNameControlHost') -or $name -in @('File name:','File name')))
    )
}

function Get-SanitizedNativeClassBucket {
    param([Parameter(Mandatory = $true)]$Record)
    $property = $Record.PSObject.Properties['ClassName']
    $value = if ($null -ne $property) { [string]$property.Value } else { '' }
    if ($value.Equals('#32770',[StringComparison]::OrdinalIgnoreCase)) { return 'dialog32770' }
    if ($value.Equals('Button',[StringComparison]::OrdinalIgnoreCase)) { return 'button' }
    if ($value.Equals('ComboBox',[StringComparison]::OrdinalIgnoreCase)) { return 'comboBox' }
    if ($value.Equals('ComboBoxEx32',[StringComparison]::OrdinalIgnoreCase)) { return 'comboBoxEx32' }
    if ($value.Equals('Edit',[StringComparison]::OrdinalIgnoreCase)) { return 'edit' }
    if ($value.Equals('SysListView32',[StringComparison]::OrdinalIgnoreCase)) { return 'sysListView32' }
    if ($value.Equals('DirectUIHWND',[StringComparison]::OrdinalIgnoreCase)) { return 'directUiHwnd' }
    if ($value.Equals('Static',[StringComparison]::OrdinalIgnoreCase)) { return 'static' }
    if ($value.Equals('SysTabControl32',[StringComparison]::OrdinalIgnoreCase)) { return 'sysTabControl32' }
    return 'other'
}

function Get-SanitizedNativeControlIdBucket {
    param([Parameter(Mandatory = $true)]$Record)
    $property = $Record.PSObject.Properties['ControlId']
    $value = if ($null -ne $property) { [int]$property.Value } else { 0 }
    if ($value -eq 1) { return 'idOk' }
    if ($value -eq 2) { return 'idCancel' }
    if ($value -ge 0x0400 -and $value -le 0x040f) { return 'pushButtonRange' }
    if ($value -ge 0x0410 -and $value -le 0x041f) { return 'checkBoxRange' }
    if ($value -ge 0x0420 -and $value -le 0x042f) { return 'radioButtonRange' }
    if ($value -ge 0x0430 -and $value -le 0x043f) { return 'groupRange' }
    if ($value -ge 0x0440 -and $value -le 0x045f) { return 'staticRange' }
    if ($value -ge 0x0460 -and $value -le 0x046f) { return 'listRange' }
    if ($value -ge 0x0470 -and $value -le 0x047f) { return 'comboRange' }
    if ($value -ge 0x0480 -and $value -le 0x048f) { return 'editRange' }
    if ($value -ge 0x0490 -and $value -le 0x0497) { return 'scrollRange' }
    if ($value -gt 0) { return 'otherPositive' }
    return 'none'
}

function Get-SanitizedNativeUiControlTypeBucket {
    param([string]$ControlType)
    switch -CaseSensitive ($ControlType) {
        'ControlType.Window' { return 'window' }
        'ControlType.Pane' { return 'pane' }
        'ControlType.Button' { return 'button' }
        'ControlType.RadioButton' { return 'radioButton' }
        'ControlType.ComboBox' { return 'comboBox' }
        'ControlType.List' { return 'list' }
        'ControlType.Edit' { return 'edit' }
        '' { return 'unavailable' }
        default { return 'other' }
    }
}

function Get-SanitizedNativeButtonStyleBucket {
    param([Parameter(Mandatory = $true)]$Record)
    if ((Get-SanitizedNativeClassBucket -Record $Record) -cne 'button') { return 'notButton' }
    $property = $Record.PSObject.Properties['ButtonStyle']
    if ($null -eq $property) { return 'unavailable' }
    switch ([int]$property.Value) {
        0 { return 'pushButton' }
        1 { return 'defaultPushButton' }
        2 { return 'checkBox' }
        3 { return 'autoCheckBox' }
        4 { return 'radioButton' }
        5 { return 'threeState' }
        6 { return 'autoThreeState' }
        7 { return 'groupBox' }
        8 { return 'userButton' }
        9 { return 'autoRadioButton' }
        10 { return 'pushBox' }
        11 { return 'ownerDraw' }
        12 { return 'splitButton' }
        13 { return 'defaultSplitButton' }
        14 { return 'commandLink' }
        15 { return 'defaultCommandLink' }
        default { return 'unavailable' }
    }
}

function Get-SanitizedNativeWindowTopologyJson {
    param(
        [AllowEmptyCollection()][object[]]$TopLevelRecords = @(),
        [AllowEmptyCollection()][object[]]$CandidateTopLevelRecords = @(),
        [AllowEmptyCollection()][object[]]$SurfaceRecords = @(),
        $RoleCounts,
        [ValidateSet('not-started','native-enumeration-started','native-enumeration-complete','uia-conversion-started','uia-conversion-complete')][string]$NativeEnumerationStage = 'not-started',
        [switch]$Unavailable
    )
    if ($Unavailable) {
        $unavailableClasses = [ordered]@{ dialog32770=-1;button=-1;comboBox=-1;comboBoxEx32=-1;edit=-1;sysListView32=-1;directUiHwnd=-1;static=-1;sysTabControl32=-1;other=-1 }
        $unavailableControlIds = [ordered]@{ idOk=-1;idCancel=-1;pushButtonRange=-1;checkBoxRange=-1;radioButtonRange=-1;groupRange=-1;staticRange=-1;listRange=-1;comboRange=-1;editRange=-1;scrollRange=-1;otherPositive=-1;none=-1 }
        $unavailableRoles = [ordered]@{ cancelButton=-1;printButton=-1;saveButton=-1;currentPageRadio=-1;namedPrinter=-1;printerCombo=-1;printerList=-1;filenameEdit=-1 }
        return ([pscustomobject][ordered]@{
            inventoryStatus='unavailable';nativeEnumerationStage='unavailable';topLevelOwnedCount=-1;topLevelOwnedVisibleCount=-1;topLevelOwnedEnabledCount=-1;topLevelCountCapped=$false
            candidateSurfaceCount=-1;candidateSurfaceCountCapped=$false;visibleDialogCandidateCount=-1;visibleEnabledDialogCandidateCount=-1;candidateTopLevels=@();childCount=-1;childCountCapped=$false;childDiagnosticsCapped=$false;childDiagnostics=@()
            classHistogram=[pscustomobject]$unavailableClasses;controlIdHistogram=[pscustomobject]$unavailableControlIds
            requiredRoleMatchesAvailable=$false;requiredRoleMatches=[pscustomobject]$unavailableRoles
        } | ConvertTo-Json -Compress -Depth 6)
    }
    $maximum = [int]$script:PrintPins.UiElementMaximum
    $candidateMaximum = [int]$script:PrintPins.NativeTopLevelDiagnosticMaximum
    $topLevelCount = [Math]::Min($TopLevelRecords.Count,$maximum)
    $surfaceRecordCount = $SurfaceRecords.Count
    $boundedSurfaceRecords = @($SurfaceRecords | Select-Object -First $maximum)
    $childRecords = @($boundedSurfaceRecords | Where-Object { [long]$_.ParentHandleValue -ne 0 })
    $childDiagnosticMaximum = [int]$script:PrintPins.NativeChildDiagnosticMaximum
    $boundedChildRecords = @($childRecords | Select-Object -First $childDiagnosticMaximum)
    $rootRecords = @($boundedSurfaceRecords | Where-Object { [long]$_.ParentHandleValue -eq 0 })
    $rootHandleValue = if ($rootRecords.Count -eq 1) { [long]$rootRecords[0].HandleValue } else { 0L }
    $childIndexesByHandle = @{}
    for ($childIndex = 0; $childIndex -lt $boundedChildRecords.Count; $childIndex++) {
        $childIndexesByHandle[[long]$boundedChildRecords[$childIndex].HandleValue] = $childIndex
    }
    $childDiagnostics = @()
    for ($childIndex = 0; $childIndex -lt $boundedChildRecords.Count; $childIndex++) {
        $record = $boundedChildRecords[$childIndex]
        $parentHandleValue = [long]$record.ParentHandleValue
        $parentIndex = if ($rootHandleValue -ne 0 -and $parentHandleValue -eq $rootHandleValue) { -1 } elseif ($childIndexesByHandle.ContainsKey($parentHandleValue)) { [int]$childIndexesByHandle[$parentHandleValue] } else { -2 }
        $controlIdProperty = $record.PSObject.Properties['ControlId']; $controlIdValue = if ($null -ne $controlIdProperty) { [int]$controlIdProperty.Value } else { -1 }
        if ($controlIdValue -lt 0 -or $controlIdValue -gt 65535) { $controlIdValue = -1 }
        $visibleProperty = $record.PSObject.Properties['IsVisible']; $enabledProperty = $record.PSObject.Properties['IsEnabled']
        $printProperty = $record.PSObject.Properties['IsLabelPrint']; $saveProperty = $record.PSObject.Properties['IsLabelSave']; $cancelProperty = $record.PSObject.Properties['IsLabelCancel']
        $fileNameProperty = $record.PSObject.Properties['IsLabelFileName']; $currentPageProperty = $record.PSObject.Properties['IsLabelCurrentPage']; $printerProperty = $record.PSObject.Properties['IsLabelPrinterName']
        $childDiagnostics += [pscustomobject][ordered]@{
            index=$childIndex;parentIndex=$parentIndex;classBucket=(Get-SanitizedNativeClassBucket -Record $record);controlId=$controlIdValue
            visible=($null -ne $visibleProperty -and [bool]$visibleProperty.Value);enabled=($null -ne $enabledProperty -and [bool]$enabledProperty.Value)
            buttonStyleBucket=(Get-SanitizedNativeButtonStyleBucket -Record $record)
            labelMatches=[pscustomobject][ordered]@{
                print=($null -ne $printProperty -and [bool]$printProperty.Value);save=($null -ne $saveProperty -and [bool]$saveProperty.Value);cancel=($null -ne $cancelProperty -and [bool]$cancelProperty.Value);fileName=($null -ne $fileNameProperty -and [bool]$fileNameProperty.Value)
                currentPage=($null -ne $currentPageProperty -and [bool]$currentPageProperty.Value);printerName=($null -ne $printerProperty -and [bool]$printerProperty.Value)
            }
        }
    }
    $visibleTopLevelCount = 0; $enabledTopLevelCount = 0
    foreach ($record in @($TopLevelRecords | Select-Object -First $maximum)) {
        $visibleProperty = $record.PSObject.Properties['IsVisible']
        if ($null -ne $visibleProperty -and [bool]$visibleProperty.Value) { $visibleTopLevelCount++ }
        $enabledProperty = $record.PSObject.Properties['IsEnabled']
        if ($null -ne $enabledProperty -and [bool]$enabledProperty.Value) { $enabledTopLevelCount++ }
    }
    $candidateTopLevels = @(); $visibleDialogs = 0; $visibleEnabledDialogs = 0
    foreach ($record in @($CandidateTopLevelRecords | Select-Object -First $candidateMaximum)) {
        Assert-PrintExactProperties -Value $record -Expected @('ClassName','ControlId','IsVisible','IsEnabled','UiControlType','UiNameMatchesAvailable','UiNameMatches') -Kind 'Native top-level diagnostic candidate'
        Assert-PrintExactProperties -Value $record.UiNameMatches -Expected @('cancel','print','save','currentPage','printerName','fileName') -Kind 'Native top-level diagnostic name matches'
        $classBucket = Get-SanitizedNativeClassBucket -Record $record
        $controlIdBucket = Get-SanitizedNativeControlIdBucket -Record $record
        $visible = [bool]$record.IsVisible; $enabled = [bool]$record.IsEnabled
        if ($visible -and $classBucket -ceq 'dialog32770') { $visibleDialogs++ }
        if ($visible -and $enabled -and $classBucket -ceq 'dialog32770') { $visibleEnabledDialogs++ }
        $candidateTopLevels += [pscustomobject][ordered]@{
            visible=$visible;enabled=$enabled;classBucket=$classBucket;controlIdBucket=$controlIdBucket
            uiaControlTypeBucket=(Get-SanitizedNativeUiControlTypeBucket -ControlType ([string]$record.UiControlType))
            uiaNameMatchesAvailable=[bool]$record.UiNameMatchesAvailable
            uiaNameMatches=[pscustomobject][ordered]@{
                cancel=[bool]$record.UiNameMatches.cancel;print=[bool]$record.UiNameMatches.print;save=[bool]$record.UiNameMatches.save
                currentPage=[bool]$record.UiNameMatches.currentPage;printerName=[bool]$record.UiNameMatches.printerName;fileName=[bool]$record.UiNameMatches.fileName
            }
        }
    }
    $classes = [ordered]@{ dialog32770=0;button=0;comboBox=0;comboBoxEx32=0;edit=0;sysListView32=0;directUiHwnd=0;static=0;sysTabControl32=0;other=0 }
    $controlIds = [ordered]@{ idOk=0;idCancel=0;pushButtonRange=0;checkBoxRange=0;radioButtonRange=0;groupRange=0;staticRange=0;listRange=0;comboRange=0;editRange=0;scrollRange=0;otherPositive=0;none=0 }
    foreach ($record in $boundedSurfaceRecords) {
        $classes[(Get-SanitizedNativeClassBucket -Record $record)]++
        $controlIds[(Get-SanitizedNativeControlIdBucket -Record $record)]++
    }
    $roles = [ordered]@{ cancelButton=-1;printButton=-1;saveButton=-1;currentPageRadio=-1;namedPrinter=-1;printerCombo=-1;printerList=-1;filenameEdit=-1 }
    $rolesAvailable = $null -ne $RoleCounts
    if ($rolesAvailable) {
        Assert-PrintExactProperties -Value $RoleCounts -Expected @('cancelButton','printButton','saveButton','currentPageRadio','namedPrinter','printerCombo','printerList','filenameEdit') -Kind 'Native window role counts'
        foreach ($property in @('cancelButton','printButton','saveButton','currentPageRadio','namedPrinter','printerCombo','printerList','filenameEdit')) {
            $value = [int]$RoleCounts.$property
            if ($value -lt 0 -or $value -gt $maximum) { throw 'Native window role count was invalid.' }
            $roles[$property] = $value
        }
    }
    $receipt = [ordered]@{
        inventoryStatus='native-window-observed'
        nativeEnumerationStage=$NativeEnumerationStage
        topLevelOwnedCount=[int]$topLevelCount
        topLevelOwnedVisibleCount=[int]$visibleTopLevelCount
        topLevelOwnedEnabledCount=[int]$enabledTopLevelCount
        topLevelCountCapped=[bool]($TopLevelRecords.Count -gt $maximum)
        candidateSurfaceCount=[int][Math]::Min($CandidateTopLevelRecords.Count,$candidateMaximum)
        candidateSurfaceCountCapped=[bool]($CandidateTopLevelRecords.Count -gt $candidateMaximum)
        visibleDialogCandidateCount=[int]$visibleDialogs
        visibleEnabledDialogCandidateCount=[int]$visibleEnabledDialogs
        candidateTopLevels=$candidateTopLevels
        childCount=[int][Math]::Min($childRecords.Count,$maximum)
        childCountCapped=[bool]($surfaceRecordCount -gt $maximum)
        childDiagnosticsCapped=[bool]($childRecords.Count -gt $childDiagnosticMaximum)
        childDiagnostics=$childDiagnostics
        classHistogram=[pscustomobject]$classes
        controlIdHistogram=[pscustomobject]$controlIds
        requiredRoleMatchesAvailable=[bool]$rolesAvailable
        requiredRoleMatches=[pscustomobject]$roles
    }
    return ([pscustomobject]$receipt | ConvertTo-Json -Compress -Depth 6)
}

function Get-ProcessNativeWindowRecords {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [long]$RootHandleValue = 0,
        [switch]$TopLevelOnly,
        [scriptblock]$WindowProvider,
        [ref]$EnumerationStage
    )
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window snapshot deadline expired.' }
    if ($null -ne $EnumerationStage) { $EnumerationStage.Value = 'native-enumeration-started' }
    $records = @(if ($WindowProvider) { & $WindowProvider $ProcessId $RootHandleValue ([bool]$TopLevelOnly) } else {
        Initialize-PrintNativeWindowInterop
        [Smacrobat.PrintVerification.NativeWindows]::Enumerate($ProcessId,$RootHandleValue,[bool]$TopLevelOnly)
    })
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window enumeration exceeded its deadline.' }
    if ($records.Count -lt 1 -or $records.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native window enumeration was missing or oversized.' }
    if ($null -ne $EnumerationStage) { $EnumerationStage.Value = 'native-enumeration-complete' }
    return @($records)
}

function Convert-ProcessNativeWindowRecordsToSnapshot {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [Parameter(Mandatory = $true)][object[]]$Records,
        [switch]$TopLevelOnly,
        [ref]$EnumerationStage
    )
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window conversion deadline expired.' }
    if ($null -ne $EnumerationStage) { $EnumerationStage.Value = 'uia-conversion-started' }
    Initialize-PrintUiAutomation
    $byHandle = @{}
    foreach ($record in $Records) {
        $handleValue = [long]$record.HandleValue
        $parentHandleValue = [long]$record.ParentHandleValue
        if ($handleValue -eq 0 -or $byHandle.ContainsKey($handleValue)) { throw 'Native window enumeration contained an invalid or duplicated handle.' }
        $elementProperty = $record.PSObject.Properties['Element']
        $element = if ($null -ne $elementProperty) { $elementProperty.Value } else { [Windows.Automation.AutomationElement]::FromHandle([IntPtr]::new($handleValue)) }
        if ($null -eq $element) { throw 'Native window did not expose an AutomationElement.' }
        Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window element conversion exceeded its deadline.' }
        $runtimeIdentity = Get-ProcessUiRuntimeIdentity -Element $element -ProcessId $ProcessId -Deadline $Deadline
        $identity = 'hwnd:' + $handleValue.ToString([Globalization.CultureInfo]::InvariantCulture) + '|' + $runtimeIdentity
        $byHandle.Add($handleValue,[pscustomobject]@{ handleValue=$handleValue;parentHandleValue=$parentHandleValue;runtimeIdentity=$identity;element=$element })
    }
    $snapshot = @()
    foreach ($record in $byHandle.Values) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window snapshot exceeded its deadline.' }
        $parentIdentity = $null
        if ([long]$record.parentHandleValue -ne 0) {
            if (-not $byHandle.ContainsKey([long]$record.parentHandleValue)) { throw 'Native window ancestry referenced an unobserved parent.' }
            $parentIdentity = [string]$byHandle[[long]$record.parentHandleValue].runtimeIdentity
        }
        $controlType = Get-UiControlTypeName -Element $record.element
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window snapshot exceeded its deadline.' }
        $snapshot += [pscustomobject]@{
            runtimeIdentity=[string]$record.runtimeIdentity
            parentRuntimeIdentity=$parentIdentity
            controlType=$controlType
            isTarget=(Test-PrintTargetUiElement -Element $record.element -Deadline $Deadline)
            isSurface=$controlType -in @('ControlType.Window','ControlType.Pane')
            element=$record.element
        }
    }
    if ($TopLevelOnly -and @($snapshot | Where-Object { $null -ne $_.parentRuntimeIdentity }).Count -ne 0) { throw 'Native top-level window snapshot contained descendants.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native window snapshot exceeded its deadline.' }
    if ($null -ne $EnumerationStage) { $EnumerationStage.Value = 'uia-conversion-complete' }
    return $snapshot
}

function Get-ProcessNativeWindowSnapshot {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [long]$RootHandleValue = 0,
        [switch]$TopLevelOnly,
        [scriptblock]$WindowProvider,
        [ref]$ObservedRecords,
        [ref]$EnumerationStage
    )
    $records = @(if ($null -ne $EnumerationStage) {
        Get-ProcessNativeWindowRecords -ProcessId $ProcessId -Deadline $Deadline -RootHandleValue $RootHandleValue -TopLevelOnly:$TopLevelOnly -WindowProvider $WindowProvider -EnumerationStage $EnumerationStage
    } else {
        Get-ProcessNativeWindowRecords -ProcessId $ProcessId -Deadline $Deadline -RootHandleValue $RootHandleValue -TopLevelOnly:$TopLevelOnly -WindowProvider $WindowProvider
    })
    if ($null -ne $ObservedRecords) { $ObservedRecords.Value = $records }
    return @(if ($null -ne $EnumerationStage) {
        Convert-ProcessNativeWindowRecordsToSnapshot -ProcessId $ProcessId -Deadline $Deadline -Records $records -TopLevelOnly:$TopLevelOnly -EnumerationStage $EnumerationStage
    } else {
        Convert-ProcessNativeWindowRecordsToSnapshot -ProcessId $ProcessId -Deadline $Deadline -Records $records -TopLevelOnly:$TopLevelOnly
    })
}

function Get-NativeSurfaceHandleValue {
    param([Parameter(Mandatory = $true)][string]$Identity)
    if ($Identity -cnotmatch '^hwnd:(-?[0-9]+)\|') { throw 'Native surface identity was invalid.' }
    $value = 0L
    if (-not [long]::TryParse($Matches[1],[Globalization.NumberStyles]::Integer,[Globalization.CultureInfo]::InvariantCulture,[ref]$value) -or $value -eq 0) { throw 'Native surface handle was invalid.' }
    return $value
}

function Get-NativeTopLevelCandidateDiagnosticRecords {
    param(
        [Parameter(Mandatory = $true)][object[]]$TopLevelRecords,
        [AllowEmptyCollection()][object[]]$TopLevelSnapshot = @(),
        [Parameter(Mandatory = $true)][Collections.Generic.HashSet[long]]$BaselineHandleValues,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $entriesByHandle = @{}
    foreach ($entry in $TopLevelSnapshot) {
        $handleValue = Get-NativeSurfaceHandleValue -Identity ([string]$entry.runtimeIdentity)
        if ($entriesByHandle.ContainsKey($handleValue)) { throw 'Native top-level diagnostic snapshot duplicated a handle.' }
        $entriesByHandle.Add($handleValue,$entry)
    }
    $result = @()
    foreach ($record in @($TopLevelRecords | Sort-Object { [long]$_.HandleValue })) {
        $handleValue = [long]$record.HandleValue
        if ($BaselineHandleValues.Contains($handleValue)) { continue }
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level diagnostic classification exceeded its deadline.' }
        $classProperty = $record.PSObject.Properties['ClassName']; $controlIdProperty = $record.PSObject.Properties['ControlId']
        $visibleProperty = $record.PSObject.Properties['IsVisible']; $enabledProperty = $record.PSObject.Properties['IsEnabled']
        $controlType = ''; $nameMatchesAvailable = $false
        $nameMatches = [ordered]@{ cancel=$false;print=$false;save=$false;currentPage=$false;printerName=$false;fileName=$false }
        if ($entriesByHandle.ContainsKey($handleValue)) {
            $entry = $entriesByHandle[$handleValue]
            $controlType = [string]$entry.controlType
            try {
                $name = [string]$entry.element.Current.Name
                if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level diagnostic name read exceeded its deadline.' }
                $nameMatches.cancel = $name.Equals('Cancel',[StringComparison]::OrdinalIgnoreCase)
                $nameMatches.print = $name.Equals('Print',[StringComparison]::OrdinalIgnoreCase)
                $nameMatches.save = $name.Equals('Save',[StringComparison]::OrdinalIgnoreCase)
                $nameMatches.currentPage = $name -in @('Current Page','Current page')
                $nameMatches.printerName = $name.Equals($script:PrintPins.PrinterName,[StringComparison]::OrdinalIgnoreCase)
                $nameMatches.fileName = $name -in @('File name:','File name')
                $nameMatchesAvailable = $true
            } catch {
                if ([datetime]::UtcNow -ge $Deadline) { throw }
            }
        }
        $result += [pscustomobject][ordered]@{
            ClassName=if ($null -ne $classProperty) { [string]$classProperty.Value } else { '' }
            ControlId=if ($null -ne $controlIdProperty) { [int]$controlIdProperty.Value } else { 0 }
            IsVisible=($null -ne $visibleProperty -and [bool]$visibleProperty.Value)
            IsEnabled=($null -ne $enabledProperty -and [bool]$enabledProperty.Value)
            UiControlType=$controlType
            UiNameMatchesAvailable=[bool]$nameMatchesAvailable
            UiNameMatches=[pscustomobject]$nameMatches
        }
    }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level diagnostic classification exceeded its deadline.' }
    return $result
}

function Get-ExactNativePrintDialogRoles {
    param(
        [Parameter(Mandatory = $true)][object[]]$SurfaceRecords,
        [Parameter(Mandatory = $true)][object[]]$Snapshot,
        [Parameter(Mandatory = $true)][long]$RootHandleValue,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $recordsByHandle = @{}
    foreach ($record in $SurfaceRecords) {
        $handleValue = [long]$record.HandleValue
        if ($recordsByHandle.ContainsKey($handleValue)) { throw 'Native print role records duplicated a handle.' }
        $recordsByHandle.Add($handleValue,$record)
    }
    $entriesByHandle = @{}
    foreach ($entry in $Snapshot) {
        $handleValue = Get-NativeSurfaceHandleValue -Identity ([string]$entry.runtimeIdentity)
        if ($entriesByHandle.ContainsKey($handleValue)) { throw 'Native print role snapshot duplicated a handle.' }
        $entriesByHandle.Add($handleValue,$entry)
    }
    if (-not $recordsByHandle.ContainsKey($RootHandleValue) -or -not $entriesByHandle.ContainsKey($RootHandleValue)) { throw 'Native print role root was missing.' }
    $reachesBoundRoot = {
        param([long]$StartHandleValue)
        if ($StartHandleValue -eq $RootHandleValue) { return $false }
        $visitedHandles = [Collections.Generic.HashSet[long]]::new()
        $currentHandleValue = $StartHandleValue
        for ($depth = 0; $depth -lt $SurfaceRecords.Count; $depth++) {
            if (-not $visitedHandles.Add($currentHandleValue)) { throw 'Native print role ancestry contained a cycle.' }
            if (-not $recordsByHandle.ContainsKey($currentHandleValue)) { throw 'Native print role ancestry referenced an unobserved handle.' }
            $parentProperty = $recordsByHandle[$currentHandleValue].PSObject.Properties['ParentHandleValue']
            if ($null -eq $parentProperty) { throw 'Native print role ancestry was missing its parent.' }
            $parentHandleValue = [long]$parentProperty.Value
            if ($parentHandleValue -eq $RootHandleValue) { return $true }
            if ($parentHandleValue -eq 0) { return $false }
            $currentHandleValue = $parentHandleValue
        }
        throw 'Native print role ancestry exceeded its bounded depth.'
    }
    $matchesCommon = {
        param($Record,[string]$ClassName,[int]$ControlId,[int]$ButtonStyle,[long]$ParentHandle,[string]$LabelProperty)
        $classProperty = $Record.PSObject.Properties['ClassName']; $controlIdProperty = $Record.PSObject.Properties['ControlId']
        $styleProperty = $Record.PSObject.Properties['ButtonStyle']; $parentProperty = $Record.PSObject.Properties['ParentHandleValue']
        $visibleProperty = $Record.PSObject.Properties['IsVisible']; $enabledProperty = $Record.PSObject.Properties['IsEnabled']
        if ($null -eq $classProperty -or -not ([string]$classProperty.Value).Equals($ClassName,[StringComparison]::OrdinalIgnoreCase) -or
            $null -eq $controlIdProperty -or [int]$controlIdProperty.Value -ne $ControlId -or $null -eq $parentProperty -or [long]$parentProperty.Value -ne $ParentHandle -or
            $null -eq $visibleProperty -or -not [bool]$visibleProperty.Value -or $null -eq $enabledProperty -or -not [bool]$enabledProperty.Value) { return $false }
        if ($ButtonStyle -ge 0 -and ($null -eq $styleProperty -or [int]$styleProperty.Value -ne $ButtonStyle)) { return $false }
        if (-not [string]::IsNullOrEmpty($LabelProperty)) {
            $label = $Record.PSObject.Properties[$LabelProperty]
            if ($null -eq $label -or -not [bool]$label.Value) { return $false }
        }
        return $true
    }
    $cancelMatches = @($SurfaceRecords | Where-Object { & $matchesCommon $_ 'Button' 2 0 $RootHandleValue 'IsLabelCancel' })
    $printMatches = @($SurfaceRecords | Where-Object { & $matchesCommon $_ 'Button' 1 1 $RootHandleValue 'IsLabelPrint' })
    $nestedDialogs = @($SurfaceRecords | Where-Object {
        (& $matchesCommon $_ '#32770' 0 -1 ([long]$_.ParentHandleValue) '') -and (& $reachesBoundRoot ([long]$_.HandleValue))
    })
    $currentMatches = @()
    foreach ($dialog in $nestedDialogs) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print role classification deadline expired.' }
        $dialogHandleValue = [long]$dialog.HandleValue
        $radioGroup = @($SurfaceRecords | Where-Object {
            $classProperty = $_.PSObject.Properties['ClassName']; $controlIdProperty = $_.PSObject.Properties['ControlId']; $styleProperty = $_.PSObject.Properties['ButtonStyle']
            $parentProperty = $_.PSObject.Properties['ParentHandleValue']; $visibleProperty = $_.PSObject.Properties['IsVisible']
            $null -ne $classProperty -and ([string]$classProperty.Value).Equals('Button',[StringComparison]::OrdinalIgnoreCase) -and
                $null -ne $controlIdProperty -and [int]$controlIdProperty.Value -in @(1056,1057,1058,1059) -and
                $null -ne $styleProperty -and [int]$styleProperty.Value -eq 4 -and $null -ne $parentProperty -and [long]$parentProperty.Value -eq $dialogHandleValue -and
                $null -ne $visibleProperty -and [bool]$visibleProperty.Value
        })
        $radioIds = @($radioGroup | ForEach-Object { [int]$_.ControlId } | Sort-Object -Unique)
        if ($radioGroup.Count -ne 4 -or $radioIds.Count -ne 4 -or (Compare-Object $radioIds @(1056,1057,1058,1059))) { continue }
        $currentMatches += @($radioGroup | Where-Object {
            $enabledProperty = $_.PSObject.Properties['IsEnabled']; $labelProperty = $_.PSObject.Properties['IsLabelCurrentPage']
            $null -ne $enabledProperty -and [bool]$enabledProperty.Value -and $null -ne $labelProperty -and [bool]$labelProperty.Value
        })
    }
    $printerListMatches = @()
    foreach ($record in $SurfaceRecords) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print role classification deadline expired.' }
        if (-not (& $matchesCommon $record 'SysListView32' 1 -1 ([long]$record.ParentHandleValue) '')) { continue }
        $parentHandleValue = [long]$record.ParentHandleValue
        if (-not $recordsByHandle.ContainsKey($parentHandleValue)) { continue }
        $parent = $recordsByHandle[$parentHandleValue]
        $parentControlId = $parent.PSObject.Properties['ControlId']; $parentVisible = $parent.PSObject.Properties['IsVisible']; $parentEnabled = $parent.PSObject.Properties['IsEnabled']
        if ((Get-SanitizedNativeClassBucket -Record $parent) -cne 'other' -or $null -eq $parentControlId -or [int]$parentControlId.Value -ne 0 -or
            $null -eq $parentVisible -or -not [bool]$parentVisible.Value -or $null -eq $parentEnabled -or -not [bool]$parentEnabled.Value) { continue }
        $grandHandleValue = [long]$parent.ParentHandleValue
        if (-not $recordsByHandle.ContainsKey($grandHandleValue)) { continue }
        $grand = $recordsByHandle[$grandHandleValue]
        if (-not (& $matchesCommon $grand '#32770' 0 -1 $RootHandleValue '')) { continue }
        $printerListMatches += $record
    }
    if ($cancelMatches.Count -gt 1 -or $printMatches.Count -gt 1 -or $currentMatches.Count -gt 1 -or $printerListMatches.Count -gt 1) { throw 'Native print role mapping was ambiguous.' }
    if ($cancelMatches.Count -ne 1 -or $printMatches.Count -ne 1 -or $currentMatches.Count -ne 1 -or $printerListMatches.Count -ne 1) { return $null }
    $roleIdentities = [ordered]@{}
    foreach ($role in @(
        [pscustomobject]@{Name='cancel';Record=$cancelMatches[0]},[pscustomobject]@{Name='print';Record=$printMatches[0]},
        [pscustomobject]@{Name='currentPage';Record=$currentMatches[0]},[pscustomobject]@{Name='printerList';Record=$printerListMatches[0]}
    )) {
        $handleValue = [long]$role.Record.HandleValue
        if (-not $entriesByHandle.ContainsKey($handleValue)) { throw 'Native print role did not map to an owned snapshot entry.' }
        $roleIdentities[$role.Name] = [string]$entriesByHandle[$handleValue].runtimeIdentity
    }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print role classification deadline expired.' }
    return [pscustomobject]$roleIdentities
}

function Get-ExactNativeSaveDialogRoles {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][object[]]$SurfaceRecords,
        [Parameter(Mandatory = $true)][long]$RootHandleValue,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    if ($SurfaceRecords.Count -lt 1 -or $SurfaceRecords.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native save role inventory was missing or oversized.' }
    $recordsByHandle = @{}
    foreach ($record in $SurfaceRecords) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native save role classification deadline expired.' }
        $handleProperty = $record.PSObject.Properties['HandleValue']; $parentProperty = $record.PSObject.Properties['ParentHandleValue']; $processProperty = $record.PSObject.Properties['ProcessId']
        if ($null -eq $handleProperty -or [long]$handleProperty.Value -eq 0 -or $null -eq $parentProperty -or $null -eq $processProperty -or [int]$processProperty.Value -ne $ProcessId) { throw 'Native save role inventory contained a foreign or malformed window.' }
        $handleValue = [long]$handleProperty.Value
        if ($recordsByHandle.ContainsKey($handleValue)) { throw 'Native save role inventory duplicated a window.' }
        $recordsByHandle.Add($handleValue,$record)
    }
    if (-not $recordsByHandle.ContainsKey($RootHandleValue)) { return $null }
    $root = $recordsByHandle[$RootHandleValue]
    $rootParent = $root.PSObject.Properties['ParentHandleValue']; $rootVisible = $root.PSObject.Properties['IsVisible']; $rootEnabled = $root.PSObject.Properties['IsEnabled']
    if ($null -eq $rootParent -or [long]$rootParent.Value -ne 0 -or (Get-SanitizedNativeClassBucket -Record $root) -cne 'dialog32770' -or $null -eq $rootVisible -or -not [bool]$rootVisible.Value -or $null -eq $rootEnabled -or -not [bool]$rootEnabled.Value) { return $null }
    $matchesButton = {
        param($Record,[int]$ControlId,[string]$StyleBucket,[string]$LabelProperty)
        $controlIdProperty = $Record.PSObject.Properties['ControlId']; $style = Get-SanitizedNativeButtonStyleBucket -Record $Record
        $parentProperty = $Record.PSObject.Properties['ParentHandleValue']; $visible = $Record.PSObject.Properties['IsVisible']; $enabled = $Record.PSObject.Properties['IsEnabled']; $label = $Record.PSObject.Properties[$LabelProperty]
        return (Get-SanitizedNativeClassBucket -Record $Record) -ceq 'button' -and $null -ne $controlIdProperty -and [int]$controlIdProperty.Value -eq $ControlId -and $style -ceq $StyleBucket -and
            $null -ne $parentProperty -and [long]$parentProperty.Value -eq $RootHandleValue -and $null -ne $visible -and [bool]$visible.Value -and $null -ne $enabled -and [bool]$enabled.Value -and $null -ne $label -and [bool]$label.Value
    }
    $saveMatches = @($SurfaceRecords | Where-Object { & $matchesButton $_ 1 'defaultPushButton' 'IsLabelSave' })
    $cancelMatches = @($SurfaceRecords | Where-Object { & $matchesButton $_ 2 'pushButton' 'IsLabelCancel' })
    $editCandidates = @($SurfaceRecords | Where-Object {
        $controlId = $_.PSObject.Properties['ControlId']; $visible = $_.PSObject.Properties['IsVisible']; $enabled = $_.PSObject.Properties['IsEnabled']
        (Get-SanitizedNativeClassBucket -Record $_) -ceq 'edit' -and $null -ne $controlId -and [int]$controlId.Value -eq 1001 -and $null -ne $visible -and [bool]$visible.Value -and $null -ne $enabled -and [bool]$enabled.Value
    })
    if ($saveMatches.Count -gt 1 -or $cancelMatches.Count -gt 1 -or $editCandidates.Count -gt 1) { throw 'Native save roles were ambiguous.' }
    if ($saveMatches.Count -ne 1 -or $cancelMatches.Count -ne 1 -or $editCandidates.Count -ne 1) { return $null }
    $edit = $editCandidates[0]; $visited = [Collections.Generic.HashSet[long]]::new(); $chain = @(); $cursor = [long]$edit.HandleValue
    for ($depth = 0; $depth -lt 6; $depth++) {
        if (-not $visited.Add($cursor) -or -not $recordsByHandle.ContainsKey($cursor)) { throw 'Native save filename ancestry was cyclic or incomplete.' }
        $chain += $recordsByHandle[$cursor]
        if ($cursor -eq $RootHandleValue) { break }
        $cursor = [long]$recordsByHandle[$cursor].ParentHandleValue
        if ($cursor -eq 0) { break }
    }
    if ($chain.Count -ne 6 -or [long]$chain[5].HandleValue -ne $RootHandleValue) { return $null }
    $expectedBuckets = @('edit','comboBox','other','directUiHwnd','other','dialog32770')
    for ($index = 0; $index -lt $expectedBuckets.Count; $index++) {
        if ((Get-SanitizedNativeClassBucket -Record $chain[$index]) -cne $expectedBuckets[$index]) { return $null }
        if ($index -gt 0 -and $index -lt 5) {
            $controlId = $chain[$index].PSObject.Properties['ControlId']
            if ($null -eq $controlId -or [int]$controlId.Value -ne 0) { return $null }
        }
    }
    return [pscustomobject][ordered]@{
        cancel=('hwnd:' + ([long]$cancelMatches[0].HandleValue).ToString([Globalization.CultureInfo]::InvariantCulture) + '|native')
        save=('hwnd:' + ([long]$saveMatches[0].HandleValue).ToString([Globalization.CultureInfo]::InvariantCulture) + '|native')
        filenameEdit=('hwnd:' + ([long]$edit.HandleValue).ToString([Globalization.CultureInfo]::InvariantCulture) + '|native')
    }
}

function Wait-NewProcessNativeWindowSurface {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][object[]]$Baseline,
        [Parameter(Mandatory = $true)][string[]]$AnchorNames,
        [Parameter(Mandatory = $true)][string[]]$AnchorControlTypes,
        [Parameter(Mandatory = $true)][ValidateSet('first-print-dialog','second-print-dialog','save-output-dialog')][string]$Stage,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$WindowProvider
    )
    $baselineIdentities = Get-ProcessUiSnapshotIdentitySet -Snapshot $Baseline -Kind 'Native window baseline'
    $baselineHandleValues = [Collections.Generic.HashSet[long]]::new()
    foreach ($identity in @($baselineIdentities)) { $null = $baselineHandleValues.Add((Get-NativeSurfaceHandleValue -Identity ([string]$identity))) }
    $structure = Get-SanitizedNativeWindowTopologyJson -Unavailable
    while ([datetime]::UtcNow -lt $Deadline) {
        $topLevelRecords = @(); $surfaceRecords = @(); $candidateTopLevelRecords = @(); $topLevelEnumerationStage = 'not-started'; $rootedEnumerationStage = 'not-started'
        try {
            $topLevelRecords = @(Get-ProcessNativeWindowRecords -ProcessId $ProcessId -Deadline $Deadline -TopLevelOnly -WindowProvider $WindowProvider -EnumerationStage ([ref]$topLevelEnumerationStage))
            $candidateRecords = @($topLevelRecords | Where-Object { -not $baselineHandleValues.Contains([long]$_.HandleValue) })
            $candidateTopLevelRecords = @(Get-NativeTopLevelCandidateDiagnosticRecords -TopLevelRecords $topLevelRecords -BaselineHandleValues $baselineHandleValues -Deadline $Deadline)
            if ([datetime]::UtcNow -lt $Deadline) { $structure = Get-SanitizedNativeWindowTopologyJson -TopLevelRecords $topLevelRecords -CandidateTopLevelRecords $candidateTopLevelRecords -SurfaceRecords @() -NativeEnumerationStage $rootedEnumerationStage }
            $visibleDialogRecords = @($candidateRecords | Where-Object {
                $classProperty = $_.PSObject.Properties['ClassName']; $visibleProperty = $_.PSObject.Properties['IsVisible']
                $null -ne $classProperty -and ([string]$classProperty.Value).Equals('#32770',[StringComparison]::OrdinalIgnoreCase) -and
                    $null -ne $visibleProperty -and [bool]$visibleProperty.Value
            })
            $enabledProperty = if ($visibleDialogRecords.Count -eq 1) { $visibleDialogRecords[0].PSObject.Properties['IsEnabled'] } else { $null }
            if ($visibleDialogRecords.Count -ne 1 -or $null -eq $enabledProperty -or -not [bool]$enabledProperty.Value) { throw 'Native window delta did not expose exactly one visible enabled dialog-class surface.' }
            $rootHandleValue = [long]$visibleDialogRecords[0].HandleValue
            $surfaceRecords = @(Get-ProcessNativeWindowRecords -ProcessId $ProcessId -RootHandleValue $rootHandleValue -Deadline $Deadline -WindowProvider $WindowProvider -EnumerationStage ([ref]$rootedEnumerationStage))
            $structure = Get-SanitizedNativeWindowTopologyJson -TopLevelRecords $topLevelRecords -CandidateTopLevelRecords $candidateTopLevelRecords -SurfaceRecords $surfaceRecords -NativeEnumerationStage $rootedEnumerationStage
            if ($Stage -ceq 'save-output-dialog') {
                $nativeSaveRoles = Get-ExactNativeSaveDialogRoles -ProcessId $ProcessId -SurfaceRecords $surfaceRecords -RootHandleValue $rootHandleValue -Deadline $Deadline
                if ($null -eq $nativeSaveRoles) { throw 'Native save dialog roles were incomplete.' }
                $rootIdentity = 'hwnd:' + $rootHandleValue.ToString([Globalization.CultureInfo]::InvariantCulture) + '|native'
                return [pscustomobject]@{
                    surfaceRootIdentity=$rootIdentity
                    surfaceElement=$null
                    baselineIdentities=[string[]]@($baselineIdentities)
                    trackedIdentities=[string[]]@($rootIdentity,[string]$nativeSaveRoles.cancel,[string]$nativeSaveRoles.save,[string]$nativeSaveRoles.filenameEdit)
                    anchorElement=$null
                    nativeRoles=$nativeSaveRoles
                }
            }
            $topLevel = @(Convert-ProcessNativeWindowRecordsToSnapshot -ProcessId $ProcessId -Deadline $Deadline -Records $topLevelRecords -TopLevelOnly -EnumerationStage ([ref]$topLevelEnumerationStage))
            $candidateTopLevelRecords = @(Get-NativeTopLevelCandidateDiagnosticRecords -TopLevelRecords $topLevelRecords -TopLevelSnapshot $topLevel -BaselineHandleValues $baselineHandleValues -Deadline $Deadline)
            $selectedRoots = @($topLevel | Where-Object { (Get-NativeSurfaceHandleValue -Identity ([string]$_.runtimeIdentity)) -eq $rootHandleValue })
            if ($selectedRoots.Count -ne 1 -or -not [bool]$selectedRoots[0].isSurface) { throw 'Native dialog-class surface did not map to one owned UI Automation surface.' }
            $selectedRoot = $selectedRoots[0]
            $current = @(Convert-ProcessNativeWindowRecordsToSnapshot -ProcessId $ProcessId -Deadline $Deadline -Records $surfaceRecords -EnumerationStage ([ref]$rootedEnumerationStage))
            $currentRoots = @($current | Where-Object { $null -eq $_.parentRuntimeIdentity -and [string]$_.runtimeIdentity -ceq [string]$selectedRoot.runtimeIdentity -and [bool]$_.isSurface })
            if ($currentRoots.Count -ne 1) { throw 'Native window root identity changed during exact descendant enumeration.' }
            $structure = Get-SanitizedNativeWindowTopologyJson -TopLevelRecords $topLevelRecords -CandidateTopLevelRecords $candidateTopLevelRecords -SurfaceRecords $surfaceRecords -NativeEnumerationStage $rootedEnumerationStage
            if ([datetime]::UtcNow -ge $Deadline) { break }
            $targets = @($current | Where-Object { [bool]$_.isTarget })
            $anchors = @(); $cancelButtons = 0; $printButtons = 0; $saveButtons = 0; $currentPageRadios = 0; $printerControls = 0; $comboBoxes = 0; $printerLists = 0; $filenameEdits = 0
            foreach ($entry in $targets) {
                Assert-ProcessUiElement -Element $entry.element -ProcessId $ProcessId
                if ([datetime]::UtcNow -ge $Deadline) { break }
                $name = [string]$entry.element.Current.Name
                $automationId = [string]$entry.element.Current.AutomationId
                if ([datetime]::UtcNow -ge $Deadline) { break }
                if ([string]$entry.controlType -ceq 'ControlType.Button' -and $name.Equals('Cancel',[StringComparison]::OrdinalIgnoreCase)) { $cancelButtons++ }
                if ([string]$entry.controlType -ceq 'ControlType.Button' -and $name.Equals('Print',[StringComparison]::OrdinalIgnoreCase)) { $printButtons++ }
                if ([string]$entry.controlType -ceq 'ControlType.Button' -and $name.Equals('Save',[StringComparison]::OrdinalIgnoreCase)) { $saveButtons++ }
                if ([string]$entry.controlType -ceq 'ControlType.RadioButton' -and $name -in @('Current Page','Current page')) { $currentPageRadios++ }
                if ($name.Equals($script:PrintPins.PrinterName,[StringComparison]::OrdinalIgnoreCase)) { $printerControls++ }
                if ([string]$entry.controlType -ceq 'ControlType.ComboBox') { $comboBoxes++ }
                if ([string]$entry.controlType -ceq 'ControlType.List') { $printerLists++ }
                if ([string]$entry.controlType -ceq 'ControlType.Edit' -and ($automationId -in @('1001','FileNameControlHost') -or $name -in @('File name:','File name'))) { $filenameEdits++ }
                if ($AnchorControlTypes.Contains([string]$entry.controlType) -and @($AnchorNames | Where-Object { $name.Equals($_,[StringComparison]::OrdinalIgnoreCase) }).Count -eq 1) { $anchors += $entry }
            }
            if ([datetime]::UtcNow -lt $Deadline) {
                $roleCounts = [pscustomobject][ordered]@{
                    cancelButton=$cancelButtons;printButton=$printButtons;saveButton=$saveButtons;currentPageRadio=$currentPageRadios
                    namedPrinter=$printerControls;printerCombo=$comboBoxes;printerList=$printerLists;filenameEdit=$filenameEdits
                }
                $structure = Get-SanitizedNativeWindowTopologyJson -TopLevelRecords $topLevelRecords -CandidateTopLevelRecords $candidateTopLevelRecords -SurfaceRecords $surfaceRecords -RoleCounts $roleCounts -NativeEnumerationStage $rootedEnumerationStage
            }
            $nativeRoles = Get-ExactNativePrintDialogRoles -SurfaceRecords $surfaceRecords -Snapshot $current -RootHandleValue $rootHandleValue -Deadline $Deadline
            $requiredTargetSet = $null -ne $nativeRoles -or ($cancelButtons -eq 1 -and $printButtons -eq 1 -and $currentPageRadios -eq 1 -and
                $printerControls -le 1 -and $comboBoxes -le 8 -and $printerLists -le 1 -and ($printerControls -eq 1 -or $comboBoxes -ge 1 -or $printerLists -eq 1))
            if (($null -ne $nativeRoles -or $anchors.Count -eq 1) -and $requiredTargetSet) {
                if ([datetime]::UtcNow -ge $Deadline) { break }
                $anchorElement = if ($null -ne $nativeRoles) {
                    $nativeAnchors = @($current | Where-Object { [string]$_.runtimeIdentity -ceq [string]$nativeRoles.cancel })
                    if ($nativeAnchors.Count -ne 1) { throw 'Native cancel role did not map to one exact bound element.' }
                    $nativeAnchors[0].element
                } else { $anchors[0].element }
                return [pscustomobject]@{
                    surfaceRootIdentity=[string]$selectedRoot.runtimeIdentity
                    surfaceElement=$currentRoots[0].element
                    baselineIdentities=[string[]]@($baselineIdentities)
                    trackedIdentities=[string[]]@($current | ForEach-Object { [string]$_.runtimeIdentity })
                    anchorElement=$anchorElement
                    nativeRoles=$nativeRoles
                }
            }
        } catch {
            if ([datetime]::UtcNow -ge $Deadline) {
                $structure = Get-SanitizedNativeWindowTopologyJson -TopLevelRecords $topLevelRecords -CandidateTopLevelRecords $candidateTopLevelRecords -SurfaceRecords $surfaceRecords -NativeEnumerationStage $rootedEnumerationStage
                break
            }
            if ($topLevelRecords.Count -gt 0) {
                if ($candidateTopLevelRecords.Count -eq 0) {
                    $candidateTopLevelRecords = @(Get-NativeTopLevelCandidateDiagnosticRecords -TopLevelRecords $topLevelRecords -BaselineHandleValues $baselineHandleValues -Deadline $Deadline)
                }
                $structure = Get-SanitizedNativeWindowTopologyJson -TopLevelRecords $topLevelRecords -CandidateTopLevelRecords $candidateTopLevelRecords -SurfaceRecords $surfaceRecords -NativeEnumerationStage $rootedEnumerationStage
            }
        }
        if ([datetime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 150 }
    }
    throw "Native print UI stage '$Stage' did not expose one complete process-owned HWND surface; uiStructure=$structure."
}

function Get-ProcessUiTreeSnapshot {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        $RootElement,
        [scriptblock]$ElementProvider,
        [scriptblock]$ParentProvider
    )
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot deadline expired.' }
    $topLevel = @(if ($ElementProvider) { & $ElementProvider $ProcessId $true } else { Get-ProcessUiElements -ProcessId $ProcessId -WindowsOnly -Deadline $Deadline })
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
    $targetElements = @(if ($ElementProvider) { & $ElementProvider $ProcessId $false 'targets' $RootElement } elseif ($null -ne $RootElement) { Get-ProcessUiElements -ProcessId $ProcessId -RootElement $RootElement -Deadline $Deadline } else { Get-ProcessUiElements -ProcessId $ProcessId -Deadline $Deadline })
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
    $surfaceElements = @(if ($ElementProvider) { & $ElementProvider $ProcessId $false 'surfaces' $RootElement } elseif ($null -ne $RootElement) { Get-ProcessUiElements -ProcessId $ProcessId -SurfaceContainersOnly -RootElement $RootElement -Deadline $Deadline } else { Get-ProcessUiElements -ProcessId $ProcessId -SurfaceContainersOnly -Deadline $Deadline })
    if ($null -ne $RootElement) { $surfaceElements = @($surfaceElements) + @($RootElement) }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
    if ($topLevel.Count -lt 1 -or $topLevel.Count -gt $script:PrintPins.UiElementMaximum -or $targetElements.Count -gt $script:PrintPins.UiElementMaximum -or $surfaceElements.Count -gt $script:PrintPins.UiElementMaximum) {
        throw 'Native process UI snapshot was missing or exceeded its bounded element count.'
    }
    $topLevelIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $topLevelByIdentity = @{}
    foreach ($element in $topLevel) {
        $identity = Get-ProcessUiRuntimeIdentity -Element $element -ProcessId $ProcessId -Deadline $Deadline
        if (-not $topLevelIdentities.Add($identity)) { throw 'Native process UI top-level identity was duplicated.' }
        $topLevelByIdentity.Add($identity,$element)
    }
    $targetIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $surfaceIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($identity in $topLevelIdentities) { $null = $surfaceIdentities.Add($identity) }
    $seedElements = @{}
    foreach ($element in $targetElements) {
        $identity = Get-ProcessUiRuntimeIdentity -Element $element -ProcessId $ProcessId -Deadline $Deadline
        if ($seedElements.ContainsKey($identity)) { throw 'Native process UI targeted runtime identity was duplicated.' }
        $seedElements.Add($identity,$element)
        if (-not $topLevelIdentities.Contains($identity)) { $null = $targetIdentities.Add($identity) }
    }
    foreach ($element in $surfaceElements) {
        $identity = Get-ProcessUiRuntimeIdentity -Element $element -ProcessId $ProcessId -Deadline $Deadline
        $null = $surfaceIdentities.Add($identity)
        if (-not $seedElements.ContainsKey($identity)) { $seedElements.Add($identity,$element) }
    }
    foreach ($identity in $topLevelIdentities) {
        if (-not $seedElements.ContainsKey($identity)) { $seedElements.Add($identity,$topLevelByIdentity[$identity]) }
    }
    if ($seedElements.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native process UI snapshot exceeded its bounded element count.' }
    $byIdentity = @{}
    $pending = [Collections.Generic.Queue[object]]::new()
    foreach ($element in $seedElements.Values) { $pending.Enqueue($element) }
    while ($pending.Count -gt 0) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
        $element = $pending.Dequeue()
        $identity = Get-ProcessUiRuntimeIdentity -Element $element -ProcessId $ProcessId -Deadline $Deadline
        if ($byIdentity.ContainsKey($identity)) { continue }
        $controlType = Get-UiControlTypeName -Element $element
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
        $entry = [pscustomobject]@{ runtimeIdentity=$identity;parentRuntimeIdentity=$null;controlType=$controlType;isTarget=$targetIdentities.Contains($identity);isSurface=$surfaceIdentities.Contains($identity);element=$element }
        $byIdentity.Add($identity,$entry)
        if ($byIdentity.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native process UI snapshot exceeded its bounded element count.' }
        if ($topLevelIdentities.Contains($identity)) { continue }
        $parent = if ($ParentProvider) { & $ParentProvider $element } else { [Windows.Automation.TreeWalker]::RawViewWalker.GetParent($element) }
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
        if ($null -eq $parent) { throw 'Native process UI ancestry ended outside an owned top-level surface.' }
        $parentProcessId = [int]$parent.Current.ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
        if ($parentProcessId -ne $ProcessId) { throw 'Native process UI ancestry escaped the owned application process.' }
        $entry.parentRuntimeIdentity = Get-ProcessUiRuntimeIdentity -Element $parent -ProcessId $ProcessId -Deadline $Deadline
        if (-not $byIdentity.ContainsKey([string]$entry.parentRuntimeIdentity)) { $pending.Enqueue($parent) }
    }
    $snapshot = @($byIdentity.Values)
    foreach ($entry in $snapshot) {
        $cursor = $entry
        $visited = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        while ($null -ne $cursor) {
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
            if (-not $visited.Add([string]$cursor.runtimeIdentity)) { throw 'Native process UI ancestry contained a cycle.' }
            if ($null -eq $cursor.parentRuntimeIdentity) {
                if (-not $topLevelIdentities.Contains([string]$cursor.runtimeIdentity)) { throw 'Native process UI ancestry did not reach an owned top-level surface.' }
                break
            }
            if (-not $byIdentity.ContainsKey([string]$cursor.parentRuntimeIdentity)) { throw 'Native process UI ancestry referenced an unobserved owned parent.' }
            $cursor = $byIdentity[[string]$cursor.parentRuntimeIdentity]
        }
    }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI snapshot exceeded its deadline.' }
    return $snapshot
}

function Test-ProcessUiSnapshotDescendantOf {
    param(
        [Parameter(Mandatory = $true)]$Entry,
        [Parameter(Mandatory = $true)][string]$AncestorIdentity,
        [Parameter(Mandatory = $true)]$EntriesByIdentity,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $cursor = $Entry
    $visited = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    while ($null -ne $cursor) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI ancestry deadline expired.' }
        if (-not $visited.Add([string]$cursor.runtimeIdentity)) { throw 'Native process UI ancestry contained a cycle.' }
        if ([string]$cursor.runtimeIdentity -ceq $AncestorIdentity) { return $true }
        if ($null -eq $cursor.parentRuntimeIdentity) { return $false }
        if (-not $EntriesByIdentity.ContainsKey([string]$cursor.parentRuntimeIdentity)) { throw 'Native process UI ancestry referenced an unobserved parent.' }
        $cursor = $EntriesByIdentity[[string]$cursor.parentRuntimeIdentity]
    }
    return $false
}

function Get-ProcessUiSnapshotCommonAncestorIdentity {
    param(
        [Parameter(Mandatory = $true)][object[]]$Entries,
        [Parameter(Mandatory = $true)]$EntriesByIdentity,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    if ($Entries.Count -lt 1) { throw 'Native process UI descendant delta was empty.' }
    $cursor = $Entries[0]
    $visited = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    while ($null -ne $cursor) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI common-ancestor deadline expired.' }
        if (-not $visited.Add([string]$cursor.runtimeIdentity)) { throw 'Native process UI ancestry contained a cycle.' }
        $candidateIdentity = [string]$cursor.runtimeIdentity
        $containsAll = $true
        foreach ($entry in $Entries) {
            if (-not (Test-ProcessUiSnapshotDescendantOf -Entry $entry -AncestorIdentity $candidateIdentity -EntriesByIdentity $EntriesByIdentity -Deadline $Deadline)) { $containsAll = $false; break }
        }
        if ($containsAll) { return $candidateIdentity }
        if ($null -eq $cursor.parentRuntimeIdentity) { break }
        if (-not $EntriesByIdentity.ContainsKey([string]$cursor.parentRuntimeIdentity)) { throw 'Native process UI ancestry referenced an unobserved parent.' }
        $cursor = $EntriesByIdentity[[string]$cursor.parentRuntimeIdentity]
    }
    throw 'Native process UI descendant delta did not share one owned ancestor surface.'
}

function Get-ProcessUiSnapshotIdentitySet {
    param([Parameter(Mandatory = $true)][object[]]$Snapshot,[Parameter(Mandatory = $true)][string]$Kind)
    if ($Snapshot.Count -lt 1 -or $Snapshot.Count -gt $script:PrintPins.UiElementMaximum) { throw "$Kind was missing or oversized." }
    $identities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Snapshot) {
        Assert-PrintExactProperties -Value $entry -Expected @('runtimeIdentity','parentRuntimeIdentity','controlType','isTarget','isSurface','element') -Kind "$Kind entry"
        if ([string]::IsNullOrWhiteSpace([string]$entry.runtimeIdentity) -or -not $identities.Add([string]$entry.runtimeIdentity)) { throw "$Kind was invalid or ambiguous." }
    }
    return $identities
}

function Wait-NewProcessUiSurface {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][object[]]$Baseline,
        [Parameter(Mandatory = $true)][string[]]$AnchorNames,
        [Parameter(Mandatory = $true)][string[]]$AnchorControlTypes,
        [Parameter(Mandatory = $true)][ValidateSet('first-print-dialog','second-print-dialog','save-output-dialog')][string]$Stage,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$ElementProvider,
        [scriptblock]$ParentProvider
    )
    $baselineIdentities = Get-ProcessUiSnapshotIdentitySet -Snapshot $Baseline -Kind 'Native process UI baseline'
    $baselineTargetIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Baseline) {
        Assert-ProcessUiElement -Element $entry.element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI baseline deadline expired.' }
        if ([bool]$entry.isTarget) { $null = $baselineTargetIdentities.Add([string]$entry.runtimeIdentity) }
    }
    $baselineStructure = Get-SanitizedObservedUiStructureJson -ProcessId $ProcessId -Elements @($Baseline | ForEach-Object { $_.element }) -Scope 'process-descendants'
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI baseline deadline expired.' }
    $postStructure = Get-SanitizedProcessUiStructureJson -ProcessId $ProcessId -DeadlineExpired
    $newStructure = Get-SanitizedProcessUiStructureJson -ProcessId $ProcessId -DeadlineExpired
    while ([datetime]::UtcNow -lt $Deadline) {
        try {
            $current = @(Get-ProcessUiTreeSnapshot -ProcessId $ProcessId -Deadline $Deadline -ElementProvider $ElementProvider -ParentProvider $ParentProvider)
        } catch {
            if ([datetime]::UtcNow -ge $Deadline) { break }
            Start-Sleep -Milliseconds 150
            continue
        }
        $postStructure = Get-SanitizedObservedUiStructureJson -ProcessId $ProcessId -Elements @($current | ForEach-Object { $_.element }) -Scope 'process-descendants'
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $newEntries = @($current | Where-Object { [bool]$_.isTarget -and -not $baselineTargetIdentities.Contains([string]$_.runtimeIdentity) })
        $newSurfaceEntries = @()
        $candidateSurfaceRootIdentity = $null
        $byIdentity = @{}
        foreach ($entry in $current) { $byIdentity.Add([string]$entry.runtimeIdentity,$entry) }
        if ($newEntries.Count -eq 0) {
            $newSurfaceEntries = @($current | Where-Object { [bool]$_.isSurface -and -not $baselineIdentities.Contains([string]$_.runtimeIdentity) })
            if ($newSurfaceEntries.Count -gt 0) {
                try { $commonSurfaceIdentity = Get-ProcessUiSnapshotCommonAncestorIdentity -Entries $newSurfaceEntries -EntriesByIdentity $byIdentity -Deadline $Deadline } catch { $commonSurfaceIdentity = $null }
                $cursorIdentity = $commonSurfaceIdentity
                while ($null -ne $cursorIdentity -and $byIdentity.ContainsKey($cursorIdentity) -and -not $baselineIdentities.Contains($cursorIdentity)) {
                    if ([bool]$byIdentity[$cursorIdentity].isSurface) { $candidateSurfaceRootIdentity = $cursorIdentity }
                    $parentIdentity = [string]$byIdentity[$cursorIdentity].parentRuntimeIdentity
                    if ([string]::IsNullOrWhiteSpace($parentIdentity) -or $baselineIdentities.Contains($parentIdentity)) { break }
                    if (-not $byIdentity.ContainsKey($parentIdentity)) { $candidateSurfaceRootIdentity = $null; break }
                    $cursorIdentity = $parentIdentity
                    if ([datetime]::UtcNow -ge $Deadline) { $candidateSurfaceRootIdentity = $null; break }
                }
                if ($null -ne $candidateSurfaceRootIdentity -and $byIdentity.ContainsKey($candidateSurfaceRootIdentity)) {
                    try {
                        $candidateSurfaceElement = $byIdentity[$candidateSurfaceRootIdentity].element
                        Assert-ProcessUiElement -Element $candidateSurfaceElement -ProcessId $ProcessId
                        if ([datetime]::UtcNow -ge $Deadline) { break }
                        $rooted = @(Get-ProcessUiTreeSnapshot -ProcessId $ProcessId -Deadline $Deadline -RootElement $candidateSurfaceElement -ElementProvider $ElementProvider -ParentProvider $ParentProvider)
                        $newEntries = @($rooted | Where-Object { [bool]$_.isTarget -and -not $baselineTargetIdentities.Contains([string]$_.runtimeIdentity) })
                    } catch {
                        if ([datetime]::UtcNow -ge $Deadline) { break }
                        $newEntries = @()
                    }
                }
            }
        }
        $newStructure = Get-SanitizedObservedUiStructureJson -ProcessId $ProcessId -Elements @($newEntries | ForEach-Object { $_.element }) -Scope 'process-descendants'
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $anchors = @()
        $cancelButtons = 0; $printButtons = 0; $saveButtons = 0; $currentPageRadios = 0; $printerControls = 0; $comboBoxes = 0; $filenameEdits = 0
        foreach ($entry in $newEntries) {
            Assert-ProcessUiElement -Element $entry.element -ProcessId $ProcessId
            if ([datetime]::UtcNow -ge $Deadline) { break }
            $name = [string]$entry.element.Current.Name
            if ([datetime]::UtcNow -ge $Deadline) { break }
            if ([string]$entry.controlType -ceq 'ControlType.Button' -and $name.Equals('Cancel',[StringComparison]::OrdinalIgnoreCase)) { $cancelButtons++ }
            if ([string]$entry.controlType -ceq 'ControlType.Button' -and $name.Equals('Print',[StringComparison]::OrdinalIgnoreCase)) { $printButtons++ }
            if ([string]$entry.controlType -ceq 'ControlType.Button' -and $name.Equals('Save',[StringComparison]::OrdinalIgnoreCase)) { $saveButtons++ }
            if ([string]$entry.controlType -ceq 'ControlType.RadioButton' -and ($name.Equals('Current Page',[StringComparison]::Ordinal) -or $name.Equals('Current page',[StringComparison]::Ordinal))) { $currentPageRadios++ }
            if ($name.Equals($script:PrintPins.PrinterName,[StringComparison]::OrdinalIgnoreCase)) { $printerControls++ }
            if ([string]$entry.controlType -ceq 'ControlType.ComboBox') { $comboBoxes++ }
            if ([string]$entry.controlType -ceq 'ControlType.Edit') { $filenameEdits++ }
            if ($AnchorControlTypes.Contains([string]$entry.controlType) -and @($AnchorNames | Where-Object { $name.Equals($_,[StringComparison]::OrdinalIgnoreCase) }).Count -eq 1) { $anchors += $entry }
        }
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $requiredTargetSet = if ($Stage -ceq 'save-output-dialog') {
            $saveButtons -eq 1 -and $filenameEdits -eq 1
        } else {
            $cancelButtons -eq 1 -and $printButtons -eq 1 -and $currentPageRadios -eq 1 -and
                $printerControls -le 1 -and $comboBoxes -le 8 -and ($printerControls -eq 1 -or $comboBoxes -ge 1)
        }
        if ($anchors.Count -eq 1 -and $requiredTargetSet -and $newEntries.Count -gt 0) {
            $surfaceRootIdentity = $candidateSurfaceRootIdentity
            if ($null -eq $surfaceRootIdentity) {
                try { $commonAncestorIdentity = Get-ProcessUiSnapshotCommonAncestorIdentity -Entries $newEntries -EntriesByIdentity $byIdentity -Deadline $Deadline } catch { $commonAncestorIdentity = $null }
                $cursorIdentity = $commonAncestorIdentity
                while ($null -ne $cursorIdentity -and $byIdentity.ContainsKey($cursorIdentity) -and -not $baselineIdentities.Contains($cursorIdentity)) {
                    if ([bool]$byIdentity[$cursorIdentity].isSurface) { $surfaceRootIdentity = $cursorIdentity }
                    $parentIdentity = [string]$byIdentity[$cursorIdentity].parentRuntimeIdentity
                    if ([string]::IsNullOrWhiteSpace($parentIdentity) -or $baselineIdentities.Contains($parentIdentity)) { break }
                    if (-not $byIdentity.ContainsKey($parentIdentity)) { $surfaceRootIdentity = $null; break }
                    $cursorIdentity = $parentIdentity
                    if ([datetime]::UtcNow -ge $Deadline) { $surfaceRootIdentity = $null; break }
                }
            }
            if ($null -ne $surfaceRootIdentity -and -not $baselineIdentities.Contains($surfaceRootIdentity) -and $byIdentity.ContainsKey($surfaceRootIdentity)) {
                if ([datetime]::UtcNow -ge $Deadline) { break }
                $tracked = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
                foreach ($entry in @($newSurfaceEntries) + @($newEntries)) { $null = $tracked.Add([string]$entry.runtimeIdentity) }
                return [pscustomobject]@{
                    surfaceRootIdentity=$surfaceRootIdentity
                    surfaceElement=$byIdentity[$surfaceRootIdentity].element
                    baselineIdentities=[string[]]@($baselineIdentities)
                    trackedIdentities=[string[]]@($tracked)
                    anchorElement=$anchors[0].element
                    nativeRoles=$null
                }
            }
        }
        if ([datetime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 150 }
    }
    throw "Native print UI stage '$Stage' did not expose one process-owned descendant surface; baselineUiStructure=$baselineStructure; postUiStructure=$postStructure; newUiStructure=$newStructure."
}

function Get-ValidatedBindingNativeRoles {
    param([Parameter(Mandatory = $true)]$Binding)
    if ($null -eq $Binding.nativeRoles) { return $null }
    $roleProperties = @($Binding.nativeRoles.PSObject.Properties.Name)
    $expectedRoles = if ($roleProperties.Count -eq 3 -and $roleProperties -contains 'cancel' -and $roleProperties -contains 'save' -and $roleProperties -contains 'filenameEdit') {
        @('cancel','save','filenameEdit')
    } else { @('cancel','print','currentPage','printerList') }
    Assert-PrintExactProperties -Value $Binding.nativeRoles -Expected $expectedRoles -Kind 'Native print role binding'
    $tracked = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($identity in @($Binding.trackedIdentities)) { $null = $tracked.Add([string]$identity) }
    $roles = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($property in $expectedRoles) {
        $identity = [string]$Binding.nativeRoles.$property
        if ([string]::IsNullOrWhiteSpace($identity) -or $identity -cnotmatch '^hwnd:' -or -not $tracked.Contains($identity) -or -not $roles.Add($identity)) { throw 'Native print role binding identity was invalid.' }
    }
    return $Binding.nativeRoles
}

function Get-BoundProcessUiEntries {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$ElementProvider,
        [scriptblock]$ParentProvider,
        [scriptblock]$NativeWindowProvider
    )
    Assert-PrintExactProperties -Value $Binding -Expected @('surfaceRootIdentity','surfaceElement','baselineIdentities','trackedIdentities','anchorElement','nativeRoles') -Kind 'Native process UI surface binding'
    if (@($Binding.baselineIdentities).Count -lt 1 -or @($Binding.baselineIdentities).Count -gt $script:PrintPins.UiElementMaximum -or
        @($Binding.trackedIdentities).Count -lt 1 -or @($Binding.trackedIdentities).Count -gt $script:PrintPins.UiElementMaximum) {
        throw 'Native process UI surface binding identity counts were invalid.'
    }
    $baselineIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($identity in @($Binding.baselineIdentities)) { if ([string]::IsNullOrWhiteSpace([string]$identity) -or -not $baselineIdentities.Add([string]$identity)) { throw 'Native process UI surface baseline was invalid.' } }
    $null = Get-ValidatedBindingNativeRoles -Binding $Binding
    $isNativeWindowBinding = [string]$Binding.surfaceRootIdentity -cmatch '^hwnd:'
    $snapshot = @(if ($isNativeWindowBinding) {
        $rootHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$Binding.surfaceRootIdentity)
        Get-ProcessNativeWindowSnapshot -ProcessId $ProcessId -RootHandleValue $rootHandleValue -Deadline $Deadline -WindowProvider $NativeWindowProvider
    } else {
        Get-ProcessUiTreeSnapshot -ProcessId $ProcessId -Deadline $Deadline -RootElement $Binding.surfaceElement -ElementProvider $ElementProvider -ParentProvider $ParentProvider
    })
    $byIdentity = @{}
    foreach ($entry in $snapshot) { $byIdentity.Add([string]$entry.runtimeIdentity,$entry) }
    if (-not $byIdentity.ContainsKey([string]$Binding.surfaceRootIdentity)) { throw 'Native process UI surface root disappeared before its action.' }
    if (-not [object]::ReferenceEquals($byIdentity[[string]$Binding.surfaceRootIdentity].element,$Binding.surfaceElement)) {
        $observedRootIdentity = Get-ProcessUiRuntimeIdentity -Element $Binding.surfaceElement -ProcessId $ProcessId -Deadline $Deadline
        if ($isNativeWindowBinding) {
            $rootHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$Binding.surfaceRootIdentity)
            $observedRootIdentity = 'hwnd:' + $rootHandleValue.ToString([Globalization.CultureInfo]::InvariantCulture) + '|' + $observedRootIdentity
        }
        if ($observedRootIdentity -cne [string]$Binding.surfaceRootIdentity) { throw 'Native process UI surface root identity changed before its action.' }
    }
    $boundEntries = @()
    foreach ($entry in $snapshot) {
        if ($baselineIdentities.Contains([string]$entry.runtimeIdentity)) { continue }
        if (Test-ProcessUiSnapshotDescendantOf -Entry $entry -AncestorIdentity ([string]$Binding.surfaceRootIdentity) -EntriesByIdentity $byIdentity -Deadline $Deadline) { $boundEntries += $entry }
    }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI surface action deadline expired.' }
    return $boundEntries
}

function Get-BoundNativeRoleElement {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][ValidateSet('cancel','print','currentPage','printerList')][string]$Role,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$NativeWindowProvider
    )
    $roles = Get-ValidatedBindingNativeRoles -Binding $Binding
    if ($null -eq $roles) { return $null }
    $rootHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$Binding.surfaceRootIdentity)
    $observedRecords = @()
    $snapshot = @(Get-ProcessNativeWindowSnapshot -ProcessId $ProcessId -RootHandleValue $rootHandleValue -Deadline $Deadline -WindowProvider $NativeWindowProvider -ObservedRecords ([ref]$observedRecords))
    $rootMatches = @($snapshot | Where-Object { $null -eq $_.parentRuntimeIdentity -and [string]$_.runtimeIdentity -ceq [string]$Binding.surfaceRootIdentity })
    if ($rootMatches.Count -ne 1) { throw 'Native print role root identity changed before its action.' }
    $currentRoles = Get-ExactNativePrintDialogRoles -SurfaceRecords $observedRecords -Snapshot $snapshot -RootHandleValue $rootHandleValue -Deadline $Deadline
    if ($null -eq $currentRoles) { throw 'Native print roles were incomplete before their action.' }
    foreach ($property in @('cancel','print','currentPage','printerList')) {
        if ([string]$currentRoles.$property -cne [string]$roles.$property) { throw 'Native print role identity changed before its action.' }
    }
    $identity = [string]$roles.$Role
    $roleMatches = @($snapshot | Where-Object { [string]$_.runtimeIdentity -ceq $identity })
    if ($roleMatches.Count -ne 1) { throw 'Native print role disappeared or became ambiguous before its action.' }
    Assert-ProcessUiElement -Element $roleMatches[0].element -ProcessId $ProcessId
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print role action deadline expired.' }
    return $roleMatches[0].element
}

function Get-BoundNativeSaveRoleRecord {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][ValidateSet('save','filenameEdit')][string]$Role,
        [Parameter(Mandatory = $true)][long]$DeadlineTickCount,
        [scriptblock]$NativeWindowProvider
    )
    $remainingMilliseconds = $DeadlineTickCount - [Environment]::TickCount64
    if ($remainingMilliseconds -le 0 -or $remainingMilliseconds -gt 120000) { throw 'Native save role deadline was invalid.' }
    $roles = Get-ValidatedBindingNativeRoles -Binding $Binding
    if ($null -eq $roles -or $null -eq $roles.PSObject.Properties['save'] -or $null -eq $roles.PSObject.Properties['filenameEdit']) { throw 'Native save role binding was invalid.' }
    $rootHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$Binding.surfaceRootIdentity)
    $deadline = [datetime]::UtcNow.AddMilliseconds($remainingMilliseconds)
    $records = @(Get-ProcessNativeWindowRecords -ProcessId $ProcessId -RootHandleValue $rootHandleValue -Deadline $deadline -WindowProvider $NativeWindowProvider)
    if ([Environment]::TickCount64 -ge $DeadlineTickCount) { throw 'Native save role revalidation exceeded its deadline.' }
    $currentRoles = Get-ExactNativeSaveDialogRoles -ProcessId $ProcessId -SurfaceRecords $records -RootHandleValue $rootHandleValue -Deadline $deadline
    if ($null -eq $currentRoles) { throw 'Native save roles were incomplete before their action.' }
    foreach ($property in @('cancel','save','filenameEdit')) {
        if ([string]$currentRoles.$property -cne [string]$roles.$property) { throw 'Native save role identity changed before its action.' }
    }
    $roleHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$roles.$Role)
    $roleRecords = @($records | Where-Object { [long]$_.HandleValue -eq $roleHandleValue })
    if ($roleRecords.Count -ne 1 -or [Environment]::TickCount64 -ge $DeadlineTickCount) { throw 'Native save role disappeared or became ambiguous before its action.' }
    return $roleRecords[0]
}

function Set-BoundNativeSaveFileNameExact {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][string]$Value,
        [Parameter(Mandatory = $true)][long]$DeadlineTickCount,
        [scriptblock]$NativeWindowProvider,
        [scriptblock]$SetProvider,
        [scriptblock]$ReadProvider,
        [ValidateRange(1,2000)][int]$SettleMilliseconds = 250,
        [switch]$AwaitSuggestion,
        [string]$SuggestedName = '',
        [ValidateRange(1,60000)][int]$SuggestionTimeoutMilliseconds = 20000,
        [ValidateSet('settext','keyboard')][string]$Transport = 'settext',
        [switch]$AcceptSuggestion,
        [scriptblock]$FolderProvider
    )
    $diagStart = [Environment]::TickCount64
    if ($AcceptSuggestion -and (($SuggestedName.Length -eq 0) -or (-not [IO.Path]::GetFileName($Value).Equals($SuggestedName,[StringComparison]::Ordinal)) -or (($null -ne $SetProvider) -and ($null -eq $ReadProvider)))) { throw 'Accepting the Save dialog suggestion requires the expected output to be named exactly as the suggestion.' }
    $isSuggestion = { param([string]$Name) $SuggestedName.Length -gt 0 -and $Name.Length -gt 0 -and ($Name.Equals($SuggestedName,[StringComparison]::OrdinalIgnoreCase) -or $Name.Equals([IO.Path]::GetFileNameWithoutExtension($SuggestedName),[StringComparison]::OrdinalIgnoreCase)) }
    $describe = { param([string]$Name) if ($Name.Length -eq 0) { 'empty' } elseif ($Name -ceq $Value) { 'typed' } elseif (& $isSuggestion $Name) { 'suggested' } elseif ($Name.IndexOfAny([char[]]'\/:') -lt 0 -and $Name.Length -le 64) { 'other:' + $Name } else { 'other-path' } }
    $diag = { param([string]$Phase,[string]$Name) [Console]::Out.WriteLine(('NATIVE_SAVE_DIAG t={0} phase={1} name={2}' -f ([Environment]::TickCount64 - $diagStart),$Phase,(& $describe $Name))) }
    if ($SuggestedName.Length -gt 0 -and ($SuggestedName.IndexOfAny([char[]]'\/:') -ge 0 -or $SuggestedName.Length -gt 255)) { throw 'The expected Save dialog suggestion must be a bare file name.' }
    $record = Get-BoundNativeSaveRoleRecord -ProcessId $ProcessId -Binding $Binding -Role 'filenameEdit' -DeadlineTickCount $DeadlineTickCount -NativeWindowProvider $NativeWindowProvider
    $rootHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$Binding.surfaceRootIdentity)
    $editHandleValue = [long]$record.HandleValue
    if ([Environment]::TickCount64 -ge $DeadlineTickCount) { throw 'Native save filename deadline expired.' }
    if ($null -eq $SetProvider -or $null -eq $ReadProvider) { Initialize-PrintNativeWindowInterop }
    $settles = ($null -eq $SetProvider) -or ($null -ne $ReadProvider)
    $readName = {
        if ($ReadProvider) { [string](& $ReadProvider $ProcessId $ProcessStartUtcTicks $rootHandleValue $editHandleValue $DeadlineTickCount) }
        else { [string][Smacrobat.PrintVerification.NativeWindows]::ReadSaveFileNameBounded($ProcessId,$ProcessStartUtcTicks,$rootHandleValue,$editHandleValue,$DeadlineTickCount) }
    }
    $pause = { if ([Environment]::TickCount64 + $SettleMilliseconds -ge $DeadlineTickCount) { throw 'Native save filename deadline expired.' }; Start-Sleep -Milliseconds $SettleMilliseconds }
    $awaiting = $settles -and ($AwaitSuggestion -or $SuggestedName.Length -gt 0)
    if ($awaiting) {
        $suggestionEnd = [Math]::Min([Environment]::TickCount64 + $SuggestionTimeoutMilliseconds, $DeadlineTickCount)
        $seen = $false
        while ([Environment]::TickCount64 -lt $suggestionEnd) {
            $current = & $readName
            if ((& $isSuggestion $current) -or ($SuggestedName.Length -eq 0 -and $current.Length -gt 0)) { $seen = $true; & $diag 'suggestion-seen' $current; break }
            & $pause
        }
        if (-not $seen) { & $diag 'suggestion-missing' (& $readName); throw 'The Save dialog never showed the application''s suggested filename, so typing could race its initialization.' }
    }
    if ($settles) {
        $settleEnd = [Math]::Min([Environment]::TickCount64 + 3000, $DeadlineTickCount)
        $previous = $null
        while ([Environment]::TickCount64 -lt $settleEnd) {
            $current = & $readName
            if ($null -ne $previous -and $current -ceq $previous) { break }
            $previous = $current
            & $pause
        }
        if ($awaiting) { & $diag 'settled' $current }
    }
    if ($AcceptSuggestion) {
        $final = & $readName
        if (-not (& $isSuggestion $final)) { & $diag 'suggestion-not-kept' $final; throw 'The Save dialog suggestion changed before it could be accepted.' }
        $folder = if ($FolderProvider) { [string](& $FolderProvider $ProcessId $ProcessStartUtcTicks $rootHandleValue $DeadlineTickCount) } else { [string][Smacrobat.PrintVerification.NativeWindows]::ReadSaveFolderBounded($ProcessId,$ProcessStartUtcTicks,$rootHandleValue,$DeadlineTickCount) }
        $expectedFolder = [IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($Value)).TrimEnd('\')
        $folderState = if ($folder.Length -eq 0) { 'unavailable' } elseif ($folder.TrimEnd('\').Equals($expectedFolder,[StringComparison]::OrdinalIgnoreCase)) { 'match' } else { 'mismatch' }
        [Console]::Out.WriteLine(('NATIVE_SAVE_DIAG t={0} phase=dialog-folder state={1}' -f ([Environment]::TickCount64 - $diagStart),$folderState))
        if ($folderState -ceq 'mismatch') { throw 'The Save dialog is not in the expected output folder, so its suggested name would land elsewhere.' }
        & $diag 'accepted-suggestion' $final
        return
    }
    for ($attempt = 1; $attempt -le 4; $attempt++) {
        $set = if ($SetProvider) { & $SetProvider $ProcessId $ProcessStartUtcTicks $rootHandleValue $editHandleValue $Value $DeadlineTickCount } elseif ($Transport -ceq 'keyboard') {
            [Smacrobat.PrintVerification.NativeWindows]::TypeSaveFileNameExact($ProcessId,$ProcessStartUtcTicks,$rootHandleValue,$editHandleValue,$Value,$DeadlineTickCount)
        } else {
            [Smacrobat.PrintVerification.NativeWindows]::SetSaveFileNameExact($ProcessId,$ProcessStartUtcTicks,$rootHandleValue,$editHandleValue,$Value,$DeadlineTickCount)
        }
        if ($set -isnot [bool] -or -not $set -or [Environment]::TickCount64 -ge $DeadlineTickCount) { throw 'Native save filename action was not exactly accepted before its deadline.' }
        if (-not $settles) { return }
        $stable = $true
        foreach ($check in 1..2) { & $pause; $observed = & $readName; if ($observed -cne $Value) { $stable = $false; if ($awaiting) { & $diag ('replaced-after-attempt-' + $attempt) $observed }; break } }
        if ($stable) { if ($awaiting) { & $diag 'typed-stable' $Value }; return }
    }
    throw 'Native save filename did not stay set; the dialog replaced it after it was typed.'
}

function Invoke-BoundNativeSaveButtonExact {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][long]$DeadlineTickCount,
        [scriptblock]$NativeWindowProvider,
        [scriptblock]$ClickProvider,
        [string]$ExpectedFileName = '',
        [scriptblock]$ReadProvider,
        [string]$ExpectedSuggestion = ''
    )
    if ($ExpectedFileName.Length -gt 0 -and $ExpectedSuggestion.Length -gt 0) { throw 'Pass either a typed filename or an accepted suggestion, not both.' }
    $record = Get-BoundNativeSaveRoleRecord -ProcessId $ProcessId -Binding $Binding -Role 'save' -DeadlineTickCount $DeadlineTickCount -NativeWindowProvider $NativeWindowProvider
    $rootHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$Binding.surfaceRootIdentity)
    $buttonHandleValue = [long]$record.HandleValue
    if ([Environment]::TickCount64 -ge $DeadlineTickCount) { throw 'Native save button deadline expired.' }
    $checking = $ExpectedFileName.Length -gt 0 -or $ExpectedSuggestion.Length -gt 0
    if ($null -eq $ClickProvider -or ($checking -and $null -eq $ReadProvider)) { Initialize-PrintNativeWindowInterop }
    if ($checking) {
        $editRecord = Get-BoundNativeSaveRoleRecord -ProcessId $ProcessId -Binding $Binding -Role 'filenameEdit' -DeadlineTickCount $DeadlineTickCount -NativeWindowProvider $NativeWindowProvider
        $current = if ($ReadProvider) { [string](& $ReadProvider $ProcessId $ProcessStartUtcTicks $rootHandleValue ([long]$editRecord.HandleValue) $DeadlineTickCount) } else { [string][Smacrobat.PrintVerification.NativeWindows]::ReadSaveFileNameBounded($ProcessId,$ProcessStartUtcTicks,$rootHandleValue,[long]$editRecord.HandleValue,$DeadlineTickCount) }
        $nameMatches = if ($ExpectedSuggestion.Length -gt 0) { $current.Length -gt 0 -and ($current.Equals($ExpectedSuggestion,[StringComparison]::OrdinalIgnoreCase) -or $current.Equals([IO.Path]::GetFileNameWithoutExtension($ExpectedSuggestion),[StringComparison]::OrdinalIgnoreCase)) } else { $current -ceq $ExpectedFileName }
        $kind = if ($ExpectedSuggestion.Length -gt 0) { 'suggested' } else { 'typed' }
        [Console]::Out.WriteLine(('NATIVE_SAVE_DIAG phase=pre-click name={0}' -f $(if ($nameMatches) { $kind } elseif ($current.Length -eq 0) { 'empty' } else { 'replaced' })))
        if (-not $nameMatches) { throw 'Native save filename was replaced before the Save button was pressed.' }
    }
    $clicked = if ($ClickProvider) { & $ClickProvider $ProcessId $ProcessStartUtcTicks $rootHandleValue $buttonHandleValue $DeadlineTickCount } else {
        [Smacrobat.PrintVerification.NativeWindows]::ClickSaveButtonExact($ProcessId,$ProcessStartUtcTicks,$rootHandleValue,$buttonHandleValue,$DeadlineTickCount)
    }
    if ($clicked -isnot [bool] -or -not $clicked -or [Environment]::TickCount64 -ge $DeadlineTickCount) { throw 'Native save button action was not exactly accepted before its deadline.' }
}

function Invoke-BoundNativeButtonRole {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][ValidateSet('cancel','print','currentPage')][string]$Role,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$NativeWindowProvider,
        [scriptblock]$ClickProvider
    )
    $element = Get-BoundNativeRoleElement -ProcessId $ProcessId -Binding $Binding -Role $Role -Deadline $Deadline -NativeWindowProvider $NativeWindowProvider
    if ($null -eq $element) { return $false }
    $roles = Get-ValidatedBindingNativeRoles -Binding $Binding
    $handleValue = Get-NativeSurfaceHandleValue -Identity ([string]$roles.$Role)
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print role action deadline expired.' }
    $remainingMilliseconds = [int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline - [datetime]::UtcNow).TotalMilliseconds)))
    if ($null -eq $ClickProvider) { Initialize-PrintNativeWindowInterop }
    $clicked = if ($ClickProvider) { & $ClickProvider $ProcessId $handleValue ($Role -ceq 'currentPage') $remainingMilliseconds } else { [Smacrobat.PrintVerification.NativeWindows]::ClickButton($ProcessId,$handleValue,($Role -ceq 'currentPage'),$remainingMilliseconds) }
    if ($clicked -isnot [bool] -or -not $clicked) { throw 'Native print role action was not accepted.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print role action exceeded its deadline.' }
    return $true
}

function Find-BoundProcessUiElement {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [string[]]$Names = @(),
        [string[]]$AutomationIds = @(),
        [string[]]$ControlTypes = @(),
        [switch]$AllowNone,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$ElementProvider,
        [scriptblock]$ParentProvider,
        [scriptblock]$NativeWindowProvider
    )
    $targetElements = @()
    foreach ($entry in @(Get-BoundProcessUiEntries -ProcessId $ProcessId -Binding $Binding -Deadline $Deadline -ElementProvider $ElementProvider -ParentProvider $ParentProvider -NativeWindowProvider $NativeWindowProvider)) {
        if ($ControlTypes.Count -gt 0 -and -not $ControlTypes.Contains([string]$entry.controlType)) { continue }
        Assert-ProcessUiElement -Element $entry.element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI target deadline expired.' }
        $matched = $false
        if ($Names.Count -gt 0) {
            $name = [string]$entry.element.Current.Name
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI target deadline expired.' }
            $matched = @($Names | Where-Object { $name.Equals($_,[StringComparison]::OrdinalIgnoreCase) }).Count -eq 1
        }
        if (-not $matched -and $AutomationIds.Count -gt 0) {
            $automationId = [string]$entry.element.Current.AutomationId
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native process UI target deadline expired.' }
            $matched = $AutomationIds.Contains($automationId)
        }
        if ($matched) { $targetElements += $entry.element }
    }
    if ($targetElements.Count -eq 0 -and $AllowNone) { return $null }
    if ($targetElements.Count -ne 1) { throw 'Native process UI bound target was missing or ambiguous.' }
    return $targetElements[0]
}

function Wait-BoundProcessUiElement {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][string[]]$Names,
        [Parameter(Mandatory = $true)][string[]]$ControlTypes,
        [Parameter(Mandatory = $true)][ValidateSet('current-page-control')][string]$Stage,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$NativeWindowProvider
    )
    while ([datetime]::UtcNow -lt $Deadline) {
        try {
            $target = Find-BoundProcessUiElement -ProcessId $ProcessId -Binding $Binding -Names $Names -ControlTypes $ControlTypes -AllowNone -Deadline $Deadline -NativeWindowProvider $NativeWindowProvider
            if ($null -ne $target) {
                if ([datetime]::UtcNow -ge $Deadline) { break }
                return $target
            }
        } catch {
            if ([datetime]::UtcNow -ge $Deadline) { break }
        }
        if ([datetime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 150 }
    }
    throw "Native print UI stage '$Stage' did not expose its exact bound control before the deadline."
}

function Get-BoundProcessUiElementsByControlType {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][string]$ControlType,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$ElementProvider,
        [scriptblock]$ParentProvider,
        [scriptblock]$NativeWindowProvider
    )
    return @(Get-BoundProcessUiEntries -ProcessId $ProcessId -Binding $Binding -Deadline $Deadline -ElementProvider $ElementProvider -ParentProvider $ParentProvider -NativeWindowProvider $NativeWindowProvider | Where-Object { [string]$_.controlType -ceq $ControlType } | ForEach-Object { $_.element })
}

function Get-SanitizedUnexpectedNativeClassBucket {
    param([Parameter(Mandatory = $true)]$Record)
    $property = $Record.PSObject.Properties['ClassName']
    $value = if ($null -ne $property) { [string]$property.Value } else { '' }
    if ($value.Equals('Chrome_WidgetWin_0',[StringComparison]::OrdinalIgnoreCase)) { return 'chromeWidgetWin0' }
    if ($value.Equals('Chrome_WidgetWin_1',[StringComparison]::OrdinalIgnoreCase)) { return 'chromeWidgetWin1' }
    if ($value.Equals('Chrome_SystemMessageWindow',[StringComparison]::OrdinalIgnoreCase)) { return 'chromeSystemMessageWindow' }
    if ($value.Equals('Chrome_RenderWidgetHostHWND',[StringComparison]::OrdinalIgnoreCase)) { return 'chromeRenderWidgetHostHwnd' }
    if ($value.Equals('Chrome_MessageWindow',[StringComparison]::OrdinalIgnoreCase)) { return 'chromeMessageWindow' }
    if ($value.Equals('Intermediate D3D Window',[StringComparison]::OrdinalIgnoreCase)) { return 'intermediateD3DWindow' }
    return Get-SanitizedNativeClassBucket -Record $Record
}

function Get-SanitizedUnexpectedNativeSurfaceDescriptors {
    param(
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$UnexpectedEntries,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$TopLevelRecords,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    if ($UnexpectedEntries.Count -gt $script:PrintPins.UiElementMaximum -or $TopLevelRecords.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Unexpected native surface diagnostic exceeded its bounded inventory.' }
    $recordsByHandle = @{}
    foreach ($record in $TopLevelRecords) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Unexpected native surface diagnostic deadline expired.' }
        $handleValue = [long]$record.HandleValue
        if ($handleValue -eq 0 -or $recordsByHandle.ContainsKey($handleValue)) { throw 'Unexpected native surface diagnostic records were invalid.' }
        $recordsByHandle.Add($handleValue,$record)
    }
    $groups = @{}
    foreach ($entry in $UnexpectedEntries) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Unexpected native surface diagnostic deadline expired.' }
        $handleValue = Get-NativeSurfaceHandleValue -Identity ([string]$entry.runtimeIdentity)
        if (-not $recordsByHandle.ContainsKey($handleValue)) { throw 'Unexpected native surface diagnostic record was unavailable.' }
        $record = $recordsByHandle[$handleValue]
        $visibleProperty = $record.PSObject.Properties['IsVisible']; $enabledProperty = $record.PSObject.Properties['IsEnabled']
        $classBucket = Get-SanitizedUnexpectedNativeClassBucket -Record $record
        $controlIdBucket = Get-SanitizedNativeControlIdBucket -Record $record
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Unexpected native surface diagnostic deadline expired.' }
        $controlTypeBucket = Get-SanitizedNativeUiControlTypeBucket -ControlType ([string]$entry.controlType)
        $visible = $null -ne $visibleProperty -and [bool]$visibleProperty.Value
        $enabled = $null -ne $enabledProperty -and [bool]$enabledProperty.Value
        $isSurface = [bool]$entry.isSurface
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Unexpected native surface diagnostic deadline expired.' }
        $labels = [ordered]@{}
        foreach ($label in @(
            [pscustomobject]@{key='print';property='IsLabelPrint'},[pscustomobject]@{key='save';property='IsLabelSave'},[pscustomobject]@{key='cancel';property='IsLabelCancel'},
            [pscustomobject]@{key='currentPage';property='IsLabelCurrentPage'},[pscustomobject]@{key='printer';property='IsLabelPrinterName'},[pscustomobject]@{key='fileName';property='IsLabelFileName'},
            [pscustomobject]@{key='yes';property='IsLabelYes'},[pscustomobject]@{key='no';property='IsLabelNo'},[pscustomobject]@{key='ok';property='IsLabelOk'},[pscustomobject]@{key='continue';property='IsLabelContinue'}
        )) {
            $property = $record.PSObject.Properties[[string]$label.property]
            $labels[[string]$label.key] = $null -ne $property -and [bool]$property.Value
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Unexpected native surface diagnostic deadline expired.' }
        }
        $labelBits = @($labels.Values | ForEach-Object { [int][bool]$_ }) -join ''
        $key = @($classBucket,$controlIdBucket,$controlTypeBucket,[int]$visible,[int]$enabled,[int]$isSurface,$labelBits) -join '|'
        if (-not $groups.ContainsKey($key)) {
            $groups.Add($key,[pscustomobject][ordered]@{count=0;classBucket=$classBucket;controlIdBucket=$controlIdBucket;controlTypeBucket=$controlTypeBucket;visible=$visible;enabled=$enabled;isSurface=$isSurface;fixedLabelMatches=[pscustomobject]$labels})
        }
        $groups[$key].count = [int]$groups[$key].count + 1
    }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Unexpected native surface diagnostic deadline expired.' }
    return @($groups.GetEnumerator() | Sort-Object Key | ForEach-Object { $_.Value })
}

function Get-SanitizedNativeCloseStateJson {
    param(
        [Parameter(Mandatory = $true)][ValidateSet('first-print-dialog','second-print-dialog','save-output-dialog')][string]$Stage,
        [Parameter(Mandatory = $true)][ValidateSet('unavailable','native-close-observed')][string]$InventoryStatus,
        [Parameter(Mandatory = $true)][ValidateSet('unavailable','not-applicable','native-bm-click-delivered','uia-invoke-returned')][string]$ActionTransport,
        [Parameter(Mandatory = $true)][int]$BaselineExpectedCount,
        [Parameter(Mandatory = $true)][int]$BaselineExactSurfaceCount,
        [Parameter(Mandatory = $true)][ValidateSet('unknown','present','absent')][string]$BoundSurfaceStatus,
        [Parameter(Mandatory = $true)][ValidateSet('not-applicable','unavailable','absent','incomplete','complete')][string]$BoundSaveRoleStatus,
        [Parameter(Mandatory = $true)][ValidateSet('not-applicable','unavailable','exact','changed')][string]$FilenameStatus,
        [Parameter(Mandatory = $true)][int]$UnexpectedSurfaceCount,
        [Parameter(Mandatory = $true)][ValidateSet('unavailable','available')][string]$UnexpectedSurfaceDescriptorsStatus,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$UnexpectedSurfaceDescriptors,
        [Parameter(Mandatory = $true)][int]$SuccessorDialogCount,
        [Parameter(Mandatory = $true)][ValidateSet('not-observed','available','unavailable')][string]$SuccessorRoleStatus,
        [Parameter(Mandatory = $true)][int]$SuccessorYesCount,
        [Parameter(Mandatory = $true)][int]$SuccessorNoCount,
        [Parameter(Mandatory = $true)][int]$SuccessorOkCount,
        [Parameter(Mandatory = $true)][int]$SuccessorContinueCount,
        [Parameter(Mandatory = $true)][int]$SuccessorCancelCount,
        [Parameter(Mandatory = $true)][int]$SuccessorSaveCount
    )
    foreach ($count in @($BaselineExpectedCount,$BaselineExactSurfaceCount,$UnexpectedSurfaceCount,$SuccessorDialogCount,$SuccessorYesCount,$SuccessorNoCount,$SuccessorOkCount,$SuccessorContinueCount,$SuccessorCancelCount,$SuccessorSaveCount)) {
        if ($count -lt -1 -or $count -gt $script:PrintPins.UiElementMaximum) { throw 'Native close diagnostic count was invalid.' }
    }
    if ($UnexpectedSurfaceDescriptorsStatus -ceq 'unavailable') {
        if ($UnexpectedSurfaceDescriptors.Count -ne 0) { throw 'Unavailable unexpected surface descriptors were not empty.' }
    } else {
        if ($UnexpectedSurfaceCount -lt 0 -or $UnexpectedSurfaceDescriptors.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Available unexpected surface descriptors were invalid.' }
        $descriptorTotal = 0
        foreach ($descriptor in $UnexpectedSurfaceDescriptors) {
            Assert-PrintExactProperties -Value $descriptor -Expected @('count','classBucket','controlIdBucket','controlTypeBucket','visible','enabled','isSurface','fixedLabelMatches') -Kind 'Unexpected native surface descriptor'
            Assert-PrintExactProperties -Value $descriptor.fixedLabelMatches -Expected @('print','save','cancel','currentPage','printer','fileName','yes','no','ok','continue') -Kind 'Unexpected native surface fixed label descriptor'
            if ([int]$descriptor.count -lt 1 -or [int]$descriptor.count -gt $script:PrintPins.UiElementMaximum) { throw 'Unexpected native surface descriptor count was invalid.' }
            if ([string]$descriptor.classBucket -notin @('chromeWidgetWin0','chromeWidgetWin1','chromeSystemMessageWindow','chromeRenderWidgetHostHwnd','chromeMessageWindow','intermediateD3DWindow','dialog32770','button','comboBox','comboBoxEx32','edit','sysListView32','directUiHwnd','static','sysTabControl32','other')) { throw 'Unexpected native surface class bucket was invalid.' }
            if ([string]$descriptor.controlIdBucket -notin @('idOk','idCancel','pushButtonRange','checkBoxRange','radioButtonRange','groupRange','staticRange','listRange','comboRange','editRange','scrollRange','otherPositive','none')) { throw 'Unexpected native surface control ID bucket was invalid.' }
            if ([string]$descriptor.controlTypeBucket -notin @('window','pane','button','radioButton','comboBox','list','edit','unavailable','other')) { throw 'Unexpected native surface control type bucket was invalid.' }
            $descriptorTotal += [int]$descriptor.count
        }
        if ($descriptorTotal -ne $UnexpectedSurfaceCount) { throw 'Unexpected native surface descriptor total did not match its inventory.' }
    }
    return ([ordered]@{
        inventoryStatus=$InventoryStatus;stage=$Stage;actionTransport=$ActionTransport
        baselineExpectedCount=$BaselineExpectedCount;baselineExactSurfaceCount=$BaselineExactSurfaceCount;boundSurfaceStatus=$BoundSurfaceStatus
        boundSaveRoleStatus=$BoundSaveRoleStatus;filenameStatus=$FilenameStatus;unexpectedSurfaceCount=$UnexpectedSurfaceCount;unexpectedSurfaceDescriptorsStatus=$UnexpectedSurfaceDescriptorsStatus;unexpectedSurfaceDescriptors=$UnexpectedSurfaceDescriptors;successorDialogCount=$SuccessorDialogCount
        successorRoleStatus=$SuccessorRoleStatus;successorRoleCounts=[ordered]@{yes=$SuccessorYesCount;no=$SuccessorNoCount;ok=$SuccessorOkCount;continue=$SuccessorContinueCount;cancel=$SuccessorCancelCount;save=$SuccessorSaveCount}
    } | ConvertTo-Json -Compress -Depth 6)
}

function Test-ExactSecondPrintResidualProfile {
    param(
        [Parameter(Mandatory = $true)][int]$UnexpectedSurfaceCount,
        [Parameter(Mandatory = $true)][ValidateSet('unavailable','available')][string]$DescriptorStatus,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Descriptors
    )
    if ($DescriptorStatus -cne 'available' -or $UnexpectedSurfaceCount -ne 2 -or $Descriptors.Count -ne 2) { return $false }
    $paneCount = 0; $windowCount = 0
    foreach ($descriptor in $Descriptors) {
        Assert-PrintExactProperties -Value $descriptor -Expected @('count','classBucket','controlIdBucket','controlTypeBucket','visible','enabled','isSurface','fixedLabelMatches') -Kind 'Transitional second-print residual descriptor'
        Assert-PrintExactProperties -Value $descriptor.fixedLabelMatches -Expected @('print','save','cancel','currentPage','printer','fileName','yes','no','ok','continue') -Kind 'Transitional second-print residual labels'
        if ([int]$descriptor.count -ne 1 -or [string]$descriptor.classBucket -cne 'other' -or [string]$descriptor.controlIdBucket -cne 'none' -or [bool]$descriptor.visible -or -not [bool]$descriptor.isSurface) { return $false }
        foreach ($labelValue in @($descriptor.fixedLabelMatches.PSObject.Properties.Value)) { if ([bool]$labelValue) { return $false } }
        if ([string]$descriptor.controlTypeBucket -ceq 'pane' -and -not [bool]$descriptor.enabled) { $paneCount++ }
        elseif ([string]$descriptor.controlTypeBucket -ceq 'window' -and [bool]$descriptor.enabled) { $windowCount++ }
        else { return $false }
    }
    return $paneCount -eq 1 -and $windowCount -eq 1
}

function Get-SecondPrintResidualIdentityDescriptorKey {
    param(
        [Parameter(Mandatory = $true)][object[]]$UnexpectedEntries,
        [Parameter(Mandatory = $true)][object[]]$TopLevelRecords,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    if ($UnexpectedEntries.Count -ne 2) { throw 'Transitional second-print residual identity count was invalid.' }
    $pairs = @()
    foreach ($entry in $UnexpectedEntries) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Transitional second-print residual identity binding deadline expired.' }
        $descriptor = @(Get-SanitizedUnexpectedNativeSurfaceDescriptors -UnexpectedEntries @($entry) -TopLevelRecords $TopLevelRecords -Deadline $Deadline)
        if ($descriptor.Count -ne 1 -or [int]$descriptor[0].count -ne 1) { throw 'Transitional second-print residual identity descriptor was invalid.' }
        $runtimeIdentity = [string]$entry.runtimeIdentity
        if ([string]::IsNullOrWhiteSpace($runtimeIdentity)) { throw 'Transitional second-print residual runtime identity was invalid.' }
        $descriptorJson = $descriptor[0] | ConvertTo-Json -Compress -Depth 5
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Transitional second-print residual identity binding deadline expired.' }
        $pairs += $runtimeIdentity + '=' + $descriptorJson
    }
    return @($pairs | Sort-Object) -join '|'
}

function Wait-BoundProcessUiSurfaceClosed {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][ValidateSet('first-print-dialog','second-print-dialog','save-output-dialog')][string]$Stage,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [string[]]$AllowedSurfaceIdentities = @(),
        [scriptblock]$ElementProvider,
        [scriptblock]$ParentProvider,
        [scriptblock]$NativeWindowProvider,
        [long]$ProcessStartUtcTicks = 0,
        [string]$ExpectedNativeSaveFileName = '',
        [scriptblock]$NativeSaveFileNameMatchProvider,
        [ValidateSet('unavailable','not-applicable','native-bm-click-delivered','uia-invoke-returned')][string]$ActionTransport = 'unavailable'
    )
    Assert-PrintExactProperties -Value $Binding -Expected @('surfaceRootIdentity','surfaceElement','baselineIdentities','trackedIdentities','anchorElement','nativeRoles') -Kind 'Native process UI surface binding'
    if (@($Binding.trackedIdentities).Count -lt 1 -or @($Binding.trackedIdentities).Count -gt $script:PrintPins.UiElementMaximum) { throw 'Native process UI tracked identity count was invalid.' }
    $tracked = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($identity in @($Binding.trackedIdentities)) { if ([string]::IsNullOrWhiteSpace([string]$identity) -or -not $tracked.Add([string]$identity)) { throw 'Native process UI tracked identity set was invalid.' } }
    if ($tracked.Count -lt 1) { throw 'Native process UI tracked identity set was empty.' }
    $allowedSurfaces = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($identity in @($AllowedSurfaceIdentities)) { if ([string]::IsNullOrWhiteSpace([string]$identity) -or -not $allowedSurfaces.Add([string]$identity)) { throw 'Native process UI allowed successor surface set was invalid.' } }
    if ($allowedSurfaces.Count -gt 1) { throw 'Native process UI allowed more than one successor surface.' }
    $baselineIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($identity in @($Binding.baselineIdentities)) { if ([string]::IsNullOrWhiteSpace([string]$identity) -or -not $baselineIdentities.Add([string]$identity)) { throw 'Native process UI close baseline identity set was invalid.' } }
    $structure = Get-SanitizedProcessUiStructureJson -ProcessId $ProcessId -DeadlineExpired
    $closeState = Get-SanitizedNativeCloseStateJson -Stage $Stage -InventoryStatus 'unavailable' -ActionTransport $ActionTransport -BaselineExpectedCount $baselineIdentities.Count -BaselineExactSurfaceCount -1 -BoundSurfaceStatus 'unknown' -BoundSaveRoleStatus 'unavailable' -FilenameStatus 'unavailable' -UnexpectedSurfaceCount -1 -UnexpectedSurfaceDescriptorsStatus 'unavailable' -UnexpectedSurfaceDescriptors @() -SuccessorDialogCount -1 -SuccessorRoleStatus 'unavailable' -SuccessorYesCount -1 -SuccessorNoCount -1 -SuccessorOkCount -1 -SuccessorContinueCount -1 -SuccessorCancelCount -1 -SuccessorSaveCount -1
    $deferUnexpectedSurfaceCheck = $Stage -ceq 'save-output-dialog'
    if ([string]$Binding.surfaceRootIdentity -cmatch '^hwnd:') {
        $boundIdentity = [string]$Binding.surfaceRootIdentity
        $boundHandleValue = Get-NativeSurfaceHandleValue -Identity $boundIdentity
        $boundIsExplicitNative = $boundIdentity -cmatch '\|native$'
        if ($Stage -ceq 'save-output-dialog' -and $boundIsExplicitNative -and (($ProcessStartUtcTicks -le 0) -or [string]::IsNullOrWhiteSpace($ExpectedNativeSaveFileName))) { throw 'Native Save close diagnostic identity or expected filename was missing.' }
        $nativeAllowedIdentities = @($allowedSurfaces | Where-Object { [string]$_ -cmatch '\|native$' })
        $uiaAllowedIdentities = @($allowedSurfaces | Where-Object { [string]$_ -cnotmatch '\|native$' })
        $baselineHandleValues = [Collections.Generic.HashSet[long]]::new()
        foreach ($identity in $baselineIdentities) { $null = $baselineHandleValues.Add((Get-NativeSurfaceHandleValue -Identity ([string]$identity))) }
        $nextDetailedDiagnostic = [datetime]::MinValue
        $residualProfileIdentityDescriptorKey = ''; $residualProfileObservations = 0; $residualProfileInvalid = $false; $residualObservationBegan = $false
        while ([datetime]::UtcNow -lt $Deadline) {
            try {
                $topLevelRecords = @()
                $current = @(Get-ProcessNativeWindowSnapshot -ProcessId $ProcessId -Deadline $Deadline -TopLevelOnly -WindowProvider $NativeWindowProvider -ObservedRecords ([ref]$topLevelRecords))
                $structure = Get-SanitizedObservedUiStructureJson -ProcessId $ProcessId -Elements @($current | ForEach-Object { $_.element }) -Scope 'top-level'
                if ([datetime]::UtcNow -ge $Deadline) { break }
                $byIdentity = @{}
                foreach ($entry in $current) { $byIdentity.Add([string]$entry.runtimeIdentity,$entry) }
                $baselinePresent = $true; $baselineExactSurfaceCount = 0
                foreach ($identity in $baselineIdentities) {
                    if ($byIdentity.ContainsKey($identity) -and [bool]$byIdentity[$identity].isSurface) { $baselineExactSurfaceCount++ } else { $baselinePresent = $false }
                }
                $boundSurfaceRemains = if ($boundIsExplicitNative) {
                    @($current | Where-Object { (Get-NativeSurfaceHandleValue -Identity ([string]$_.runtimeIdentity)) -eq $boundHandleValue }).Count -gt 0
                } else { $byIdentity.ContainsKey($boundIdentity) }
                $allowedPresent = $true
                foreach ($identity in $uiaAllowedIdentities) {
                    if (-not $byIdentity.ContainsKey($identity) -or -not [bool]$byIdentity[$identity].isSurface) { $allowedPresent = $false; break }
                }
                $validatedNativeAllowedHandles = [Collections.Generic.HashSet[long]]::new()
                if ($allowedPresent) {
                    foreach ($identity in $nativeAllowedIdentities) {
                        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native allowed Save surface validation deadline expired.' }
                        $handleValue = Get-NativeSurfaceHandleValue -Identity ([string]$identity)
                        $topLevelMatches = @($topLevelRecords | Where-Object { [long]$_.HandleValue -eq $handleValue })
                        if ($topLevelMatches.Count -ne 1) { $allowedPresent = $false; break }
                        $surfaceRecords = @(Get-ProcessNativeWindowRecords -ProcessId $ProcessId -RootHandleValue $handleValue -Deadline $Deadline -WindowProvider $NativeWindowProvider)
                        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native allowed Save surface validation deadline expired.' }
                        $saveRoles = Get-ExactNativeSaveDialogRoles -ProcessId $ProcessId -SurfaceRecords $surfaceRecords -RootHandleValue $handleValue -Deadline $Deadline
                        if ($null -eq $saveRoles) { $allowedPresent = $false; break }
                        $null = $validatedNativeAllowedHandles.Add($handleValue)
                    }
                }
                $unexpectedSurface = $false; $unexpectedSurfaceCount = 0; $unexpectedEntries = @()
                foreach ($entry in @($current | Where-Object { [bool]$_.isSurface })) {
                    $identity = [string]$entry.runtimeIdentity
                    $handleValue = Get-NativeSurfaceHandleValue -Identity $identity
                    if ($baselineIdentities.Contains($identity) -or $uiaAllowedIdentities -ccontains $identity -or $validatedNativeAllowedHandles.Contains($handleValue)) { continue }
                    $unexpectedSurface = $true; $unexpectedSurfaceCount++; $unexpectedEntries += $entry
                }
                $unexpectedSurfaceDescriptorsStatus = 'available'; $unexpectedSurfaceDescriptors = @()
                try { $unexpectedSurfaceDescriptors = @(Get-SanitizedUnexpectedNativeSurfaceDescriptors -UnexpectedEntries $unexpectedEntries -TopLevelRecords $topLevelRecords -Deadline $Deadline) }
                catch { if ([datetime]::UtcNow -ge $Deadline) { throw }; $unexpectedSurfaceDescriptorsStatus = 'unavailable'; $unexpectedSurfaceDescriptors = @() }
                $successorDialogs = @($topLevelRecords | Where-Object {
                    [long]$_.HandleValue -ne $boundHandleValue -and -not $baselineHandleValues.Contains([long]$_.HandleValue) -and
                        $null -ne $_.PSObject.Properties['ClassName'] -and ([string]$_.ClassName).Equals('#32770',[StringComparison]::OrdinalIgnoreCase) -and
                        $null -ne $_.PSObject.Properties['IsVisible'] -and [bool]$_.IsVisible
                })
                $residualEvaluationReady = $Stage -ceq 'second-print-dialog' -and -not $boundSurfaceRemains -and $baselinePresent -and $baselineExactSurfaceCount -eq $baselineIdentities.Count
                $exactResidualProfile = $residualEvaluationReady -and $allowedSurfaces.Count -eq 0 -and $allowedPresent -and $successorDialogs.Count -eq 0 -and
                    (Test-ExactSecondPrintResidualProfile -UnexpectedSurfaceCount $unexpectedSurfaceCount -DescriptorStatus $unexpectedSurfaceDescriptorsStatus -Descriptors $unexpectedSurfaceDescriptors)
                if ($exactResidualProfile) {
                    $identityDescriptorKey = Get-SecondPrintResidualIdentityDescriptorKey -UnexpectedEntries $unexpectedEntries -TopLevelRecords $topLevelRecords -Deadline $Deadline
                    if ([datetime]::UtcNow -ge $Deadline) { throw 'Transitional second-print residual validation deadline expired.' }
                    if (-not $residualObservationBegan) { $residualObservationBegan = $true; $residualProfileIdentityDescriptorKey = $identityDescriptorKey }
                    elseif ($identityDescriptorKey -cne $residualProfileIdentityDescriptorKey) { $residualProfileInvalid = $true }
                    if (-not $residualProfileInvalid) { $residualProfileObservations++ }
                } elseif ($residualObservationBegan -or $residualEvaluationReady) { $residualProfileInvalid = $true }
                if ([datetime]::UtcNow -ge $nextDetailedDiagnostic) {
                    $boundSaveRoleStatus = if ($Stage -ceq 'save-output-dialog' -and $boundIsExplicitNative) { if ($boundSurfaceRemains) { 'unavailable' } else { 'absent' } } else { 'not-applicable' }
                    $filenameStatus = if ($Stage -ceq 'save-output-dialog' -and $boundIsExplicitNative) { if ($boundSurfaceRemains) { 'unavailable' } else { 'not-applicable' } } else { 'not-applicable' }
                    if ($Stage -ceq 'save-output-dialog' -and $boundIsExplicitNative -and $boundSurfaceRemains -and [datetime]::UtcNow -lt $Deadline) {
                        try {
                            $boundRecords = @(Get-ProcessNativeWindowRecords -ProcessId $ProcessId -RootHandleValue $boundHandleValue -Deadline $Deadline -WindowProvider $NativeWindowProvider)
                            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native Save close diagnostic deadline expired.' }
                            $boundRoles = Get-ExactNativeSaveDialogRoles -ProcessId $ProcessId -SurfaceRecords $boundRecords -RootHandleValue $boundHandleValue -Deadline $Deadline
                            if ($null -eq $boundRoles) { $boundSaveRoleStatus = 'incomplete'; $filenameStatus = 'unavailable' } else {
                                $boundSaveRoleStatus = 'complete'
                                $editHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$boundRoles.filenameEdit)
                                $remainingMilliseconds = [int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline - [datetime]::UtcNow).TotalMilliseconds)))
                                $diagnosticDeadlineTick = [Environment]::TickCount64 + $remainingMilliseconds
                                $filenameMatches = if ($NativeSaveFileNameMatchProvider) { & $NativeSaveFileNameMatchProvider $ProcessId $ProcessStartUtcTicks $boundHandleValue $editHandleValue $ExpectedNativeSaveFileName $diagnosticDeadlineTick } else {
                                    Initialize-PrintNativeWindowInterop
                                    [Smacrobat.PrintVerification.NativeWindows]::SaveFileNameEqualsExact($ProcessId,$ProcessStartUtcTicks,$boundHandleValue,$editHandleValue,$ExpectedNativeSaveFileName,$diagnosticDeadlineTick)
                                }
                                if ($filenameMatches -isnot [bool]) { throw 'Native Save filename diagnostic returned an invalid result.' }
                                $filenameStatus = if ($filenameMatches) { 'exact' } else { 'changed' }
                            }
                        } catch { if ([datetime]::UtcNow -ge $Deadline) { throw }; $boundSaveRoleStatus = 'unavailable'; $filenameStatus = 'unavailable' }
                    }
                    $successorRoleStatus = if ($successorDialogs.Count -eq 0) { 'not-observed' } else { 'available' }
                    $successorCounts = [ordered]@{yes=0;no=0;ok=0;continue=0;cancel=0;save=0}
                    if ($successorDialogs.Count -gt 4) { $successorRoleStatus = 'unavailable'; foreach ($key in @($successorCounts.Keys)) { $successorCounts[$key] = -1 } }
                    elseif ($successorDialogs.Count -gt 0) {
                        try {
                            foreach ($dialogRecord in $successorDialogs) {
                                if ([datetime]::UtcNow -ge $Deadline) { throw 'Native successor dialog diagnostic deadline expired.' }
                                $successorRecords = @(Get-ProcessNativeWindowRecords -ProcessId $ProcessId -RootHandleValue ([long]$dialogRecord.HandleValue) -Deadline $Deadline -WindowProvider $NativeWindowProvider)
                                foreach ($record in $successorRecords) {
                                    foreach ($role in @([pscustomobject]@{key='yes';property='IsLabelYes'},[pscustomobject]@{key='no';property='IsLabelNo'},[pscustomobject]@{key='ok';property='IsLabelOk'},[pscustomobject]@{key='continue';property='IsLabelContinue'},[pscustomobject]@{key='cancel';property='IsLabelCancel'},[pscustomobject]@{key='save';property='IsLabelSave'})) {
                                        $property = $record.PSObject.Properties[[string]$role.property]
                                        if ($null -ne $property -and [bool]$property.Value) { $successorCounts[[string]$role.key]++ }
                                    }
                                }
                            }
                        } catch { if ([datetime]::UtcNow -ge $Deadline) { throw }; $successorRoleStatus = 'unavailable'; foreach ($key in @($successorCounts.Keys)) { $successorCounts[$key] = -1 } }
                    }
                    $boundSurfaceStatus = if ($boundSurfaceRemains) { 'present' } else { 'absent' }
                    $closeState = Get-SanitizedNativeCloseStateJson -Stage $Stage -InventoryStatus 'native-close-observed' -ActionTransport $ActionTransport -BaselineExpectedCount $baselineIdentities.Count -BaselineExactSurfaceCount $baselineExactSurfaceCount -BoundSurfaceStatus $boundSurfaceStatus -BoundSaveRoleStatus $boundSaveRoleStatus -FilenameStatus $filenameStatus -UnexpectedSurfaceCount $unexpectedSurfaceCount -UnexpectedSurfaceDescriptorsStatus $unexpectedSurfaceDescriptorsStatus -UnexpectedSurfaceDescriptors $unexpectedSurfaceDescriptors -SuccessorDialogCount $successorDialogs.Count -SuccessorRoleStatus $successorRoleStatus -SuccessorYesCount $successorCounts.yes -SuccessorNoCount $successorCounts.no -SuccessorOkCount $successorCounts.ok -SuccessorContinueCount $successorCounts.continue -SuccessorCancelCount $successorCounts.cancel -SuccessorSaveCount $successorCounts.save
                    $nextDetailedDiagnostic = [datetime]::UtcNow.AddSeconds(1)
                }
                if (-not $boundSurfaceRemains -and $baselinePresent -and $allowedPresent -and ($deferUnexpectedSurfaceCheck -or -not $unexpectedSurface)) {
                    if ([datetime]::UtcNow -ge $Deadline) { break }
                    return
                }
                if ($exactResidualProfile -and -not $residualProfileInvalid -and $residualProfileObservations -ge 3) {
                    if ([datetime]::UtcNow -ge $Deadline) { break }
                    return
                }
            } catch {
                if ([datetime]::UtcNow -ge $Deadline) { break }
                if ($Stage -ceq 'second-print-dialog' -and $residualObservationBegan) { $residualProfileInvalid = $true }
            }
            if ([datetime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 150 }
        }
        $nativeCloseFailure = if ($deferUnexpectedSurfaceCheck) { 'retained its bound HWND surface or changed the exact baseline during Save close' } else { 'retained its bound HWND surface or an unbound replacement surface after close' }
        throw "Native print UI stage '$Stage' $nativeCloseFailure; uiStructure=$structure; closeState=$closeState."
    }
    while ([datetime]::UtcNow -lt $Deadline) {
        try { $current = @(Get-ProcessUiTreeSnapshot -ProcessId $ProcessId -Deadline $Deadline -ElementProvider $ElementProvider -ParentProvider $ParentProvider) }
        catch {
            $snapshotFailure = [string]$_.Exception.Message
            $deadlineFailure = $snapshotFailure -ceq 'Native process UI snapshot deadline expired.' -or $snapshotFailure -ceq 'Native process UI snapshot exceeded its deadline.' -or $snapshotFailure -ceq 'Native process UI identity deadline expired.' -or $snapshotFailure -ceq 'Native process UI ancestry deadline expired.'
            if ($deadlineFailure -and [datetime]::UtcNow -ge $Deadline) { break }
            throw
        }
        $structure = Get-SanitizedObservedUiStructureJson -ProcessId $ProcessId -Elements @($current | ForEach-Object { $_.element }) -Scope 'process-descendants'
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $byIdentity = @{}
        foreach ($entry in $current) { $byIdentity.Add([string]$entry.runtimeIdentity,$entry) }
        $trackedRemain = @($current | Where-Object { $tracked.Contains([string]$_.runtimeIdentity) }).Count
        $boundSurfaceRemains = $byIdentity.ContainsKey([string]$Binding.surfaceRootIdentity)
        $baselinePresent = $true
        foreach ($identity in $baselineIdentities) { if (-not $byIdentity.ContainsKey($identity)) { $baselinePresent = $false; break } }
        $allowedPresent = $true
        foreach ($identity in $allowedSurfaces) { if (-not $byIdentity.ContainsKey($identity) -or -not [bool]$byIdentity[$identity].isSurface) { $allowedPresent = $false; break } }
        $unexpectedSurface = $false
        foreach ($entry in @($current | Where-Object { [bool]$_.isSurface -and -not $baselineIdentities.Contains([string]$_.runtimeIdentity) })) {
            $isAllowed = $false
            foreach ($identity in $allowedSurfaces) {
                if (Test-ProcessUiSnapshotDescendantOf -Entry $entry -AncestorIdentity $identity -EntriesByIdentity $byIdentity -Deadline $Deadline) { $isAllowed = $true; break }
            }
            if (-not $isAllowed) { $unexpectedSurface = $true; break }
        }
        if ($trackedRemain -eq 0 -and -not $boundSurfaceRemains -and $baselinePresent -and $allowedPresent -and ($deferUnexpectedSurfaceCheck -or -not $unexpectedSurface)) {
            if ([datetime]::UtcNow -ge $Deadline) { break }
            return
        }
        if ([datetime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 150 }
    }
    $uiaCloseFailure = if ($deferUnexpectedSurfaceCheck) { 'retained its bound surface or targeted identities, or changed the exact baseline during Save close' } else { 'retained its bound surface, targeted identities, or an unbound replacement surface after close' }
    throw "Native print UI stage '$Stage' $uiaCloseFailure; uiStructure=$structure."
}

function Wait-ProcessTopLevelUiBaselineRestored {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][object[]]$Baseline,
        [Parameter(Mandatory = $true)][ValidateSet('first-native-cleanup','final-native-cleanup')][string]$Stage,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$ElementProvider
    )
    if ($Baseline.Count -lt 1 -or $Baseline.Count -gt $script:PrintPins.UiElementMaximum) { throw 'Final native UI baseline was missing or oversized.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Final native UI baseline deadline expired.' }
    $expected = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Baseline) {
        Assert-PrintExactProperties -Value $entry -Expected @('runtimeIdentity','controlType','element') -Kind 'Final native UI baseline entry'
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Final native UI baseline deadline expired.' }
        if (-not $expected.Add([string]$entry.runtimeIdentity)) { throw 'Final native UI baseline was ambiguous.' }
        Assert-ProcessUiElement -Element $entry.element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Final native UI baseline deadline expired.' }
    }
    $structure = Get-SanitizedProcessUiStructureJson -ProcessId $ProcessId -DeadlineExpired
    while ([datetime]::UtcNow -lt $Deadline) {
        $current = @(Get-ProcessTopLevelUiSnapshot -ProcessId $ProcessId -Deadline $Deadline -ElementProvider $ElementProvider)
        $observedElements = @($current | ForEach-Object { $_.element })
        $structure = Get-SanitizedObservedUiStructureJson -ProcessId $ProcessId -Elements $observedElements -Scope 'top-level'
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $actual = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
        foreach ($entry in $current) { $null = $actual.Add([string]$entry.runtimeIdentity) }
        if ($expected.SetEquals($actual)) {
            if ([datetime]::UtcNow -ge $Deadline) { break }
            return
        }
        if ([datetime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 150 }
    }
    throw "Native print UI stage '$Stage' did not restore its exact top-level baseline; uiStructure=$structure."
}

function Assert-ProcessTopLevelUiBaselineMatch {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][object[]]$Expected,
        [Parameter(Mandatory = $true)][object[]]$Actual,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI baseline comparison deadline expired.' }
    if ($Expected.Count -lt 1 -or $Actual.Count -lt 1 -or $Expected.Count -gt $script:PrintPins.UiElementMaximum -or $Actual.Count -gt $script:PrintPins.UiElementMaximum) {
        throw 'Native top-level UI baseline comparison was missing or oversized.'
    }
    $expectedIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    $actualIdentities = [Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal)
    foreach ($entry in $Expected) {
        Assert-PrintExactProperties -Value $entry -Expected @('runtimeIdentity','controlType','element') -Kind 'Native top-level UI baseline comparison entry'
        Assert-ProcessUiElement -Element $entry.element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI baseline comparison deadline expired.' }
        if (-not $expectedIdentities.Add([string]$entry.runtimeIdentity)) { throw 'Native top-level UI baseline comparison was ambiguous.' }
    }
    foreach ($entry in $Actual) {
        Assert-PrintExactProperties -Value $entry -Expected @('runtimeIdentity','controlType','element') -Kind 'Native top-level UI baseline comparison entry'
        Assert-ProcessUiElement -Element $entry.element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI baseline comparison deadline expired.' }
        if (-not $actualIdentities.Add([string]$entry.runtimeIdentity)) { throw 'Native top-level UI baseline comparison was ambiguous.' }
    }
    if (-not $expectedIdentities.SetEquals($actualIdentities)) { throw 'Native top-level UI baseline changed between print attempts.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native top-level UI baseline comparison deadline expired.' }
}

function Assert-NativePrintDeadline {
    param([Parameter(Mandatory = $true)][datetime]$Deadline)
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native print UI action deadline expired.' }
}

function Invoke-ProcessUiElement {
    param([Parameter(Mandatory = $true)]$Element,[Parameter(Mandatory = $true)][int]$ProcessId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    Assert-NativePrintDeadline -Deadline $Deadline
    Assert-ProcessUiElement -Element $Element -ProcessId $ProcessId
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)) {
        throw 'Native print UI control does not expose InvokePattern.'
    }
    Assert-NativePrintDeadline -Deadline $Deadline
    ([Windows.Automation.InvokePattern]$pattern).Invoke()
}

function Select-ProcessUiElement {
    param([Parameter(Mandatory = $true)]$Element,[Parameter(Mandatory = $true)][int]$ProcessId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    Assert-NativePrintDeadline -Deadline $Deadline
    Assert-ProcessUiElement -Element $Element -ProcessId $ProcessId
    $selection = $null
    if ($Element.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern,[ref]$selection)) {
        Assert-NativePrintDeadline -Deadline $Deadline
        ([Windows.Automation.SelectionItemPattern]$selection).Select()
        return
    }
    $invoke = $null
    if ($Element.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$invoke)) {
        Assert-NativePrintDeadline -Deadline $Deadline
        ([Windows.Automation.InvokePattern]$invoke).Invoke()
        return
    }
    throw 'Native print UI selection does not expose a supported UI Automation pattern.'
}

function Set-ProcessUiElementValue {
    param([Parameter(Mandatory = $true)]$Element,[Parameter(Mandatory = $true)][int]$ProcessId,[Parameter(Mandatory = $true)][string]$Value,[Parameter(Mandatory = $true)][datetime]$Deadline)
    Assert-NativePrintDeadline -Deadline $Deadline
    Assert-ProcessUiElement -Element $Element -ProcessId $ProcessId
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern,[ref]$pattern)) {
        throw 'Native print UI edit does not expose ValuePattern.'
    }
    $valuePattern = [Windows.Automation.ValuePattern]$pattern
    if ($valuePattern.Current.IsReadOnly) { throw 'Native print UI edit is read-only.' }
    Assert-NativePrintDeadline -Deadline $Deadline
    $valuePattern.SetValue($Value)
}

function Wait-ProcessUiWindowClosed {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][string[]]$Names,
        [Parameter(Mandatory = $true)][ValidateSet('first-print-dialog','second-print-dialog','save-output-dialog','final-native-cleanup')][string]$Stage,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $structure = Get-SanitizedProcessUiStructureJson -ProcessId $ProcessId -DeadlineExpired
    while ([datetime]::UtcNow -lt $Deadline) {
        $foundWindows = @()
        $observed = @(Get-ProcessUiElements -ProcessId $ProcessId -WindowsOnly)
        $structure = Get-SanitizedObservedUiStructureJson -ProcessId $ProcessId -Elements $observed -Scope 'top-level'
        foreach ($element in $observed) {
            try {
                $name = [string]$element.Current.Name
                if (@($Names | Where-Object { $name.Equals($_,[StringComparison]::OrdinalIgnoreCase) }).Count -eq 1) { $foundWindows += $element }
            } catch { }
        }
        if ($foundWindows.Count -eq 0) { return }
        if ([datetime]::UtcNow -lt $Deadline) { Start-Sleep -Milliseconds 150 }
    }
    throw "Native print UI stage '$Stage' remained open after its bounded close action; uiStructure=$structure."
}

function Open-NativePrintDialogFromWebView {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $script = @'
const dialogs=[...document.querySelectorAll('dialog[aria-labelledby="print-title"]')];
if(dialogs.length!==1)return false;
const buttons=[...dialogs[0].querySelectorAll('button')].filter(x=>x.textContent.trim()==='Choose printer…'&&!x.disabled);
if(buttons.length===1)buttons[0].click();
return buttons.length===1;
'@
    $clicked = Invoke-WebDriverScript -SessionId $SessionId -Script $script -Deadline $Deadline
    if ($clicked -isnot [bool] -or -not $clicked) { throw 'The exact enabled print control was unavailable.' }
}

function Wait-WebPrintStatus {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][string]$Prefix,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $encoded = $Prefix | ConvertTo-Json -Compress
    $script = @'
const prefix=__PREFIX__;
const d=[...document.querySelectorAll('dialog[aria-labelledby="print-title"]')];
const s=d.length===1?d[0].querySelector('[role="status"]'):null;
return {count:d.length,text:s?.textContent.trim()||'',ready:d.length===1&&!!s&&s.textContent.trim().startsWith(prefix)};
'@.Replace('__PREFIX__',$encoded)
    return Wait-WebDriverOracle -SessionId $SessionId -Script $script -Deadline $Deadline -Kind 'Installed print result transport' -Predicate { param($v) [int]$v.count -eq 1 -and [bool]$v.ready -and ([string]$v.text).Length -le 240 }
}

function Close-WebPrintDialog {
    param([Parameter(Mandatory = $true)][string]$SessionId,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $script = @'
const dialogs=[...document.querySelectorAll('dialog[aria-labelledby="print-title"]')];
if(dialogs.length!==1)return false;
const buttons=[...dialogs[0].querySelectorAll('button')].filter(x=>x.textContent.trim()==='Close'&&!x.disabled);
if(buttons.length===1)buttons[0].click();
return buttons.length===1;
'@
    $closed = Invoke-WebDriverScript -SessionId $SessionId -Script $script -Deadline $Deadline
    if ($closed -isnot [bool] -or -not $closed) { throw 'The completed print dialog could not be closed exactly.' }
    $closedOracle = @'
return document.querySelectorAll('dialog[aria-labelledby="print-title"]').length===0;
'@
    $null = Wait-WebDriverOracle -SessionId $SessionId -Script $closedOracle -Deadline $Deadline -Kind 'Installed print dialog close' -Predicate { param($v) $v -is [bool] -and $v }
}

function Cancel-NativePrintDialog {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Baseline,
        [AllowEmptyCollection()][object[]]$NativeBaseline = @(),
        [Parameter(Mandatory = $true)][ValidateSet('first-print-dialog','second-print-dialog')][string]$Stage,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $surface = if ($NativeBaseline.Count -gt 0) {
        Wait-NewProcessNativeWindowSurface -ProcessId $ProcessId -Baseline $NativeBaseline -AnchorNames @('Cancel') -AnchorControlTypes @('ControlType.Button') -Stage $Stage -Deadline $Deadline
    } else {
        Wait-NewProcessUiSurface -ProcessId $ProcessId -Baseline $Baseline -AnchorNames @('Cancel') -AnchorControlTypes @('ControlType.Button') -Stage $Stage -Deadline $Deadline
    }
    Assert-NativePrintDeadline -Deadline $Deadline
    if ($null -ne $surface.nativeRoles) {
        $null = Invoke-BoundNativeButtonRole -ProcessId $ProcessId -Binding $surface -Role 'cancel' -Deadline $Deadline
    } else {
        $cancel = Find-BoundProcessUiElement -ProcessId $ProcessId -Binding $surface -Names @('Cancel') -ControlTypes @('ControlType.Button') -Deadline $Deadline
        Invoke-ProcessUiElement -Element $cancel -ProcessId $ProcessId -Deadline $Deadline
    }
    Wait-BoundProcessUiSurfaceClosed -ProcessId $ProcessId -Binding $surface -Stage $Stage -Deadline $Deadline
    return $surface
}

function Find-BoundNativePrinterElement {
    param([Parameter(Mandatory = $true)][int]$ProcessId,[Parameter(Mandatory = $true)]$Binding,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$NativeWindowProvider,[scriptblock]$TargetProvider)
    $boundNativeList = Get-BoundNativeRoleElement -ProcessId $ProcessId -Binding $Binding -Role 'printerList' -Deadline $Deadline -NativeWindowProvider $NativeWindowProvider
    $lists = if ($null -ne $boundNativeList) { @([pscustomobject]@{element=$boundNativeList}) } else {
        $entries = @(Get-BoundProcessUiEntries -ProcessId $ProcessId -Binding $Binding -Deadline $Deadline -NativeWindowProvider $NativeWindowProvider)
        @($entries | Where-Object { [string]$_.controlType -ceq 'ControlType.List' })
    }
    if ($lists.Count -gt 1) { throw 'Native print dialog exposed an ambiguous printer list.' }
    $printerMatches = @()
    foreach ($list in $lists) {
        Assert-ProcessUiElement -Element $list.element -ProcessId $ProcessId
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list query deadline expired.' }
        foreach ($element in @(if ($TargetProvider) { & $TargetProvider $ProcessId $list.element } else { Get-ProcessUiElements -ProcessId $ProcessId -RootElement $list.element -Deadline $Deadline })) {
            Assert-ProcessUiElement -Element $element -ProcessId $ProcessId
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list query deadline expired.' }
            $name = [string]$element.Current.Name
            $type = Get-UiControlTypeName -Element $element
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list query deadline expired.' }
            if ($name.Equals($script:PrintPins.PrinterName,[StringComparison]::OrdinalIgnoreCase) -and $type -in @('ControlType.ListItem','ControlType.Button','ControlType.RadioButton')) { $printerMatches += $element }
        }
    }
    if ($printerMatches.Count -eq 0) { return $null }
    if ($printerMatches.Count -ne 1) { throw 'Microsoft Print to PDF was ambiguous inside the exact native printer list.' }
    return $printerMatches[0]
}

function Get-BoundedNativePrinterListDescendants {
    param(
        [Parameter(Mandatory = $true)]$RootElement,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$TraversalProvider
    )
    $maximum = 64
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic deadline expired.' }
    if ($TraversalProvider) {
        $provided = @(& $TraversalProvider $RootElement $maximum $Deadline)
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic traversal exceeded its deadline.' }
        return @($provided | Select-Object -First ($maximum + 1))
    }
    Initialize-PrintUiAutomation
    $walker = [Windows.Automation.TreeWalker]::RawViewWalker
    $pending = [Collections.Generic.Queue[object]]::new()
    $result = [Collections.Generic.List[object]]::new()
    $pending.Enqueue($RootElement)
    while ($pending.Count -gt 0 -and $result.Count -le $maximum) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic deadline expired.' }
        $parentElement = $pending.Dequeue()
        $childElement = $walker.GetFirstChild($parentElement)
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic traversal exceeded its deadline.' }
        while ($null -ne $childElement) {
            $result.Add($childElement)
            if ($result.Count -gt $maximum) { break }
            $pending.Enqueue($childElement)
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic deadline expired.' }
            $childElement = $walker.GetNextSibling($childElement)
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic traversal exceeded its deadline.' }
        }
    }
    return @($result)
}

function Get-BoundNativePrinterListDiagnosticJson {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$NativeWindowProvider,
        [scriptblock]$TraversalProvider
    )
    $unavailable = '{"inventoryStatus":"unavailable","descendantCount":-1,"countCapped":false,"facts":[]}'
    if ([datetime]::UtcNow -ge $Deadline) { return $unavailable }
    try {
        $listElement = Get-BoundNativeRoleElement -ProcessId $ProcessId -Binding $Binding -Role 'printerList' -Deadline $Deadline -NativeWindowProvider $NativeWindowProvider
        if ($null -eq $listElement) { return $unavailable }
        $observed = @(Get-BoundedNativePrinterListDescendants -RootElement $listElement -Deadline $Deadline -TraversalProvider $TraversalProvider)
        $maximum = 64
        $facts = @()
        foreach ($element in @($observed | Select-Object -First $maximum)) {
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic deadline expired.' }
            $ownerMatches = [int]$element.Current.ProcessId -eq $ProcessId
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic fact read exceeded its deadline.' }
            $controlTypeBucket = Get-SanitizedNativeUiControlTypeBucket -ControlType (Get-UiControlTypeName -Element $element)
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic fact read exceeded its deadline.' }
            $name = [string]$element.Current.Name
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic fact read exceeded its deadline.' }
            $selectionPattern = $null
            $selectionAvailable = [bool]$element.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern,[ref]$selectionPattern)
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic pattern read exceeded its deadline.' }
            $invokePattern = $null
            $invokeAvailable = [bool]$element.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$invokePattern)
            if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic pattern read exceeded its deadline.' }
            $facts += [pscustomobject][ordered]@{
                controlTypeBucket=$controlTypeBucket
                processIdMatches=[bool]$ownerMatches
                exactPrinterName=[bool]$name.Equals($script:PrintPins.PrinterName,[StringComparison]::OrdinalIgnoreCase)
                selectionPatternAvailable=[bool]$selectionAvailable
                invokePatternAvailable=[bool]$invokeAvailable
            }
        }
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer-list diagnostic deadline expired.' }
        return ([pscustomobject][ordered]@{
            inventoryStatus='available'
            descendantCount=[int][Math]::Min($observed.Count,$maximum)
            countCapped=[bool]($observed.Count -gt $maximum)
            facts=$facts
        } | ConvertTo-Json -Compress -Depth 4)
    } catch {
        return $unavailable
    }
}

function Find-BoundPdfPrinterElement {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$UiFinder,
        [scriptblock]$NativeFinder
    )
    Assert-NativePrintDeadline -Deadline $Deadline
    $printer = if ($UiFinder) {
        & $UiFinder $ProcessId $Binding $script:PrintPins.PrinterName $Deadline
    } else {
        Find-BoundProcessUiElement -ProcessId $ProcessId -Binding $Binding -Names @($script:PrintPins.PrinterName) -AllowNone -Deadline $Deadline
    }
    if ($null -eq $printer -and [string]$Binding.surfaceRootIdentity -cmatch '^hwnd:') {
        Assert-NativePrintDeadline -Deadline $Deadline
        $printer = if ($NativeFinder) { & $NativeFinder $ProcessId $Binding $Deadline } else { Find-BoundNativePrinterElement -ProcessId $ProcessId -Binding $Binding -Deadline $Deadline }
    }
    return $printer
}

function Invoke-IsolatedNativeAccessiblePrinterSelection {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)][long]$DialogRootHandleValue,
        [Parameter(Mandatory = $true)][long]$ListHandleValue,
        [Parameter(Mandatory = $true)][int]$ExpectedControlId,
        [Parameter(Mandatory = $true)][string]$ExactName,
        [Parameter(Mandatory = $true)][long]$DeadlineTickCount
    )
    $initialRemainingMilliseconds = $DeadlineTickCount - [Environment]::TickCount64
    if ($initialRemainingMilliseconds -le 0 -or $initialRemainingMilliseconds -gt 120000 -or [string]::IsNullOrWhiteSpace($script:InstalledPrintDialogScriptPath)) { throw 'Native accessible printer worker inputs were invalid.' }
    $workerSource = @'
$ErrorActionPreference = 'Stop'
. $env:SMACROBAT_PRINT_ACCESSIBLE_SCRIPT
Initialize-PrintNativeWindowInterop
$selected = [Smacrobat.PrintVerification.NativeWindows]::SelectAccessibleChildExact(
    [int]$env:SMACROBAT_PRINT_ACCESSIBLE_PROCESS,
    [long]$env:SMACROBAT_PRINT_ACCESSIBLE_START_TICKS,
    [long]$env:SMACROBAT_PRINT_ACCESSIBLE_DIALOG_ROOT,
    [long]$env:SMACROBAT_PRINT_ACCESSIBLE_HANDLE,
    [int]$env:SMACROBAT_PRINT_ACCESSIBLE_CONTROL_ID,
    [string]$env:SMACROBAT_PRINT_ACCESSIBLE_NAME,
    [long]$env:SMACROBAT_PRINT_ACCESSIBLE_DEADLINE_TICK)
if ($selected) { [Console]::Out.Write('selected') } else { [Console]::Out.Write('missing') }
'@
    $encodedWorker = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($workerSource))
    $startInfo = [Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = [Environment]::ProcessPath
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.ArgumentList.Add('-NoProfile'); $startInfo.ArgumentList.Add('-NonInteractive'); $startInfo.ArgumentList.Add('-EncodedCommand'); $startInfo.ArgumentList.Add($encodedWorker)
    $startInfo.Environment['SMACROBAT_PRINT_ACCESSIBLE_SCRIPT'] = $script:InstalledPrintDialogScriptPath
    $startInfo.Environment['SMACROBAT_PRINT_ACCESSIBLE_PROCESS'] = $ProcessId.ToString([Globalization.CultureInfo]::InvariantCulture)
    $startInfo.Environment['SMACROBAT_PRINT_ACCESSIBLE_START_TICKS'] = $ProcessStartUtcTicks.ToString([Globalization.CultureInfo]::InvariantCulture)
    $startInfo.Environment['SMACROBAT_PRINT_ACCESSIBLE_DIALOG_ROOT'] = $DialogRootHandleValue.ToString([Globalization.CultureInfo]::InvariantCulture)
    $startInfo.Environment['SMACROBAT_PRINT_ACCESSIBLE_HANDLE'] = $ListHandleValue.ToString([Globalization.CultureInfo]::InvariantCulture)
    $startInfo.Environment['SMACROBAT_PRINT_ACCESSIBLE_CONTROL_ID'] = $ExpectedControlId.ToString([Globalization.CultureInfo]::InvariantCulture)
    $startInfo.Environment['SMACROBAT_PRINT_ACCESSIBLE_NAME'] = $ExactName
    $startInfo.Environment['SMACROBAT_PRINT_ACCESSIBLE_DEADLINE_TICK'] = $DeadlineTickCount.ToString([Globalization.CultureInfo]::InvariantCulture)
    $workerProcess = [Diagnostics.Process]::new(); $workerProcess.StartInfo = $startInfo; $workerStarted = $false
    try {
        if (-not $workerProcess.Start()) { throw 'Native accessible printer worker did not start.' }
        $workerStarted = $true
        $waitMilliseconds = [long]$DeadlineTickCount - [Environment]::TickCount64
        if ($waitMilliseconds -le 0 -or -not $workerProcess.WaitForExit([int][Math]::Min([int]::MaxValue,$waitMilliseconds))) {
            try { $workerProcess.Kill($true) } catch { }
            if (-not $workerProcess.WaitForExit(5000)) { throw 'Native accessible printer worker could not be stopped after its deadline.' }
            throw 'Native accessible printer worker exceeded its deadline and was stopped.'
        }
        $output = $workerProcess.StandardOutput.ReadToEnd()
        $null = $workerProcess.StandardError.ReadToEnd()
        if ($workerProcess.ExitCode -ne 0 -or $output -notin @('selected','missing')) { throw 'Native accessible printer worker failed closed.' }
        return $output -ceq 'selected'
    } finally {
        if ($workerStarted -and -not $workerProcess.HasExited) {
            try { $workerProcess.Kill($true) } catch { }
            $null = $workerProcess.WaitForExit(5000)
        }
        $workerProcess.Dispose()
    }
}

function Select-BoundNativeAccessiblePrinterExact {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][string]$Value,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$NativeWindowProvider,
        [scriptblock]$AccessibleSelectProvider
    )
    $listElement = Get-BoundNativeRoleElement -ProcessId $ProcessId -Binding $Binding -Role 'printerList' -Deadline $Deadline -NativeWindowProvider $NativeWindowProvider
    if ($null -eq $listElement) { return $false }
    $roles = Get-ValidatedBindingNativeRoles -Binding $Binding
    $dialogRootHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$Binding.surfaceRootIdentity)
    $listHandleValue = Get-NativeSurfaceHandleValue -Identity ([string]$roles.printerList)
    if ($ProcessStartUtcTicks -le 0) { throw 'Native accessible printer process start identity was invalid.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native accessible printer action deadline expired.' }
    $remainingMilliseconds = [int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline - [datetime]::UtcNow).TotalMilliseconds)))
    $deadlineTickCount = [Environment]::TickCount64 + $remainingMilliseconds
    $selected = if ($AccessibleSelectProvider) {
        & $AccessibleSelectProvider $ProcessId $ProcessStartUtcTicks $dialogRootHandleValue $listHandleValue ([int]$script:PrintPins.NativePrinterListControlId) $Value $deadlineTickCount
    } else {
        Invoke-IsolatedNativeAccessiblePrinterSelection -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -DialogRootHandleValue $dialogRootHandleValue -ListHandleValue $listHandleValue -ExpectedControlId ([int]$script:PrintPins.NativePrinterListControlId) -ExactName $Value -DeadlineTickCount $deadlineTickCount
    }
    if ($selected -isnot [bool]) { throw 'Native accessible printer selection returned an invalid result.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native accessible printer action exceeded its deadline.' }
    return $selected
}

function Select-BoundNativePrinterExact {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)]$Binding,
        [Parameter(Mandatory = $true)][string]$Value,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [scriptblock]$AccessibleSelector,
        [scriptblock]$ComboSelector
    )
    $accessibleSelected = if ($AccessibleSelector) { & $AccessibleSelector $ProcessId $ProcessStartUtcTicks $Binding $Value $Deadline } else { Select-BoundNativeAccessiblePrinterExact -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Binding $Binding -Value $Value -Deadline $Deadline }
    if ($accessibleSelected -isnot [bool]) { throw 'Native accessible printer selector returned an invalid result.' }
    if ($accessibleSelected) { return $true }
    $comboSelected = if ($ComboSelector) { & $ComboSelector $ProcessId $Binding $Value $Deadline } else { Select-BoundNativeComboItemExact -ProcessId $ProcessId -Binding $Binding -Value $Value -Deadline $Deadline }
    if ($comboSelected -isnot [bool]) { throw 'Native printer combo selector returned an invalid result.' }
    return $comboSelected
}

function Select-BoundNativeComboItemExact {
    param([Parameter(Mandatory = $true)][int]$ProcessId,[Parameter(Mandatory = $true)]$Binding,[Parameter(Mandatory = $true)][string]$Value,[Parameter(Mandatory = $true)][datetime]$Deadline,[scriptblock]$ComboContainsProvider,[scriptblock]$ComboSelectProvider,[scriptblock]$NativeWindowProvider)
    if (($null -eq $ComboContainsProvider) -xor ($null -eq $ComboSelectProvider)) { throw 'Native printer combo test providers were incomplete.' }
    if ($null -eq $ComboContainsProvider) { Initialize-PrintNativeWindowInterop }
    $comboMatches = @()
    foreach ($entry in @(Get-BoundProcessUiEntries -ProcessId $ProcessId -Binding $Binding -Deadline $Deadline -NativeWindowProvider $NativeWindowProvider | Where-Object { [string]$_.controlType -ceq 'ControlType.ComboBox' })) {
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer combo query deadline expired.' }
        $handleValue = Get-NativeSurfaceHandleValue -Identity ([string]$entry.runtimeIdentity)
        $remainingMilliseconds = [int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline - [datetime]::UtcNow).TotalMilliseconds)))
        $contains = if ($ComboContainsProvider) { & $ComboContainsProvider $ProcessId $handleValue $Value $remainingMilliseconds } else { [Smacrobat.PrintVerification.NativeWindows]::ComboContainsExact($ProcessId,$handleValue,$Value,$remainingMilliseconds) }
        if ($contains -isnot [bool]) { throw 'Native printer combo exact lookup returned an invalid result.' }
        if ($contains) { $comboMatches += $handleValue }
        if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer combo query deadline expired.' }
    }
    if ($comboMatches.Count -eq 0) { return $false }
    if ($comboMatches.Count -ne 1) { throw 'Microsoft Print to PDF was ambiguous inside native printer combo boxes.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer combo action deadline expired.' }
    $remainingMilliseconds = [int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline - [datetime]::UtcNow).TotalMilliseconds)))
    $selected = if ($ComboSelectProvider) { & $ComboSelectProvider $ProcessId ([long]$comboMatches[0]) $Value $remainingMilliseconds } else { [Smacrobat.PrintVerification.NativeWindows]::SelectComboExact($ProcessId,[long]$comboMatches[0],$Value,$remainingMilliseconds) }
    if ($selected -isnot [bool] -or -not $selected) { throw 'Microsoft Print to PDF could not be selected in its exact native combo box.' }
    if ([datetime]::UtcNow -ge $Deadline) { throw 'Native printer combo action exceeded its deadline.' }
    return $true
}

function Select-PdfPrinterAndCurrentPage {
    param([Parameter(Mandatory = $true)][int]$ProcessId,[Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,[Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Baseline,[AllowEmptyCollection()][object[]]$NativeBaseline = @(),[Parameter(Mandatory = $true)][datetime]$Deadline)
    $surface = if ($NativeBaseline.Count -gt 0) {
        Wait-NewProcessNativeWindowSurface -ProcessId $ProcessId -Baseline $NativeBaseline -AnchorNames @('Cancel') -AnchorControlTypes @('ControlType.Button') -Stage 'second-print-dialog' -Deadline $Deadline
    } else {
        Wait-NewProcessUiSurface -ProcessId $ProcessId -Baseline $Baseline -AnchorNames @('Cancel') -AnchorControlTypes @('ControlType.Button') -Stage 'second-print-dialog' -Deadline $Deadline
    }
    $printer = $null
    $printerSelectedByNativeTransport = $false
    $printer = Find-BoundPdfPrinterElement -ProcessId $ProcessId -Binding $surface -Deadline $Deadline
    if ($null -eq $printer) {
        Assert-NativePrintDeadline -Deadline $Deadline
        if ([string]$surface.surfaceRootIdentity -cmatch '^hwnd:') {
            $printerSelectedByNativeTransport = Select-BoundNativePrinterExact -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Binding $surface -Value $script:PrintPins.PrinterName -Deadline $Deadline
        }
        if ($null -eq $printer -and -not $printerSelectedByNativeTransport) {
            $combos = @(Get-BoundProcessUiElementsByControlType -ProcessId $ProcessId -Binding $surface -ControlType 'ControlType.ComboBox' -Deadline $Deadline)
            if ($combos.Count -gt 8) { throw 'Native print dialog exposed too many combo boxes.' }
            foreach ($combo in $combos) {
                Assert-NativePrintDeadline -Deadline $Deadline
                Assert-ProcessUiElement -Element $combo -ProcessId $ProcessId
                $expand = $null
                if ($combo.TryGetCurrentPattern([Windows.Automation.ExpandCollapsePattern]::Pattern,[ref]$expand)) {
                    Assert-NativePrintDeadline -Deadline $Deadline
                    ([Windows.Automation.ExpandCollapsePattern]$expand).Expand()
                    Start-Sleep -Milliseconds 100
                    Assert-NativePrintDeadline -Deadline $Deadline
                    $printer = Find-BoundProcessUiElement -ProcessId $ProcessId -Binding $surface -Names @($script:PrintPins.PrinterName) -AllowNone -Deadline $Deadline
                    if ($null -ne $printer) { break }
                }
            }
        }
    }
    if ($null -eq $printer -and -not $printerSelectedByNativeTransport) {
        $printerListUiStructure = if ([string]$surface.surfaceRootIdentity -cmatch '^hwnd:') {
            Get-BoundNativePrinterListDiagnosticJson -ProcessId $ProcessId -Binding $surface -Deadline $Deadline
        } else { '{"inventoryStatus":"unavailable","descendantCount":-1,"countCapped":false,"facts":[]}' }
        throw "Microsoft Print to PDF was not exposed by the process-bound native dialog; printerListUiStructure=$printerListUiStructure."
    }
    if ($null -ne $printer) { Select-ProcessUiElement -Element $printer -ProcessId $ProcessId -Deadline $Deadline }
    if ($null -ne $surface.nativeRoles) {
        $null = Invoke-BoundNativeButtonRole -ProcessId $ProcessId -Binding $surface -Role 'currentPage' -Deadline $Deadline
    } else {
        $current = Wait-BoundProcessUiElement -ProcessId $ProcessId -Binding $surface -Names @('Current Page','Current page') -ControlTypes @('ControlType.RadioButton') -Stage 'current-page-control' -Deadline $Deadline
        Select-ProcessUiElement -Element $current -ProcessId $ProcessId -Deadline $Deadline
    }
    return $surface
}

function Submit-NativePrintToPdf {
    param(
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][long]$ProcessStartUtcTicks,
        [Parameter(Mandatory = $true)][AllowEmptyCollection()][object[]]$Baseline,
        [AllowEmptyCollection()][object[]]$NativeBaseline = @(),
        [Parameter(Mandatory = $true)][string]$OutputPath,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    $surface = Select-PdfPrinterAndCurrentPage -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Baseline $Baseline -NativeBaseline $NativeBaseline -Deadline $Deadline
    Assert-NativePrintDeadline -Deadline $Deadline
    if ($null -ne $surface.nativeRoles) {
        $null = Invoke-BoundNativeButtonRole -ProcessId $ProcessId -Binding $surface -Role 'print' -Deadline $Deadline
    } else {
        $print = Find-BoundProcessUiElement -ProcessId $ProcessId -Binding $surface -Names @('Print') -ControlTypes @('ControlType.Button') -Deadline $Deadline
        Invoke-ProcessUiElement -Element $print -ProcessId $ProcessId -Deadline $Deadline
    }
    $saveSurface = if ($NativeBaseline.Count -gt 0) {
        Wait-NewProcessNativeWindowSurface -ProcessId $ProcessId -Baseline $NativeBaseline -AnchorNames @('Save') -AnchorControlTypes @('ControlType.Button') -Stage 'save-output-dialog' -Deadline $Deadline
    } else {
        Wait-NewProcessUiSurface -ProcessId $ProcessId -Baseline $Baseline -AnchorNames @('Save') -AnchorControlTypes @('ControlType.Button') -Stage 'save-output-dialog' -Deadline $Deadline
    }
    Assert-NativePrintDeadline -Deadline $Deadline
    $saveRoles = Get-ValidatedBindingNativeRoles -Binding $saveSurface
    $saveActionTransport = 'unavailable'
    if ($null -ne $saveRoles -and $null -ne $saveRoles.PSObject.Properties['save']) {
        $remainingMilliseconds = [int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline - [datetime]::UtcNow).TotalMilliseconds)))
        $saveActionDeadlineTick = [Environment]::TickCount64 + $remainingMilliseconds
        Set-BoundNativeSaveFileNameExact -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Binding $saveSurface -Value $OutputPath -DeadlineTickCount $saveActionDeadlineTick
        Invoke-BoundNativeSaveButtonExact -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Binding $saveSurface -DeadlineTickCount $saveActionDeadlineTick
        $saveActionTransport = 'native-bm-click-delivered'
    } else {
        try {
            $filename = Find-BoundProcessUiElement -ProcessId $ProcessId -Binding $saveSurface -AutomationIds @('1001','FileNameControlHost') -ControlTypes @('ControlType.Edit') -Deadline $Deadline
        } catch {
            Assert-NativePrintDeadline -Deadline $Deadline
            $filename = Find-BoundProcessUiElement -ProcessId $ProcessId -Binding $saveSurface -Names @('File name:','File name') -ControlTypes @('ControlType.Edit') -Deadline $Deadline
        }
        Set-ProcessUiElementValue -Element $filename -ProcessId $ProcessId -Value $OutputPath -Deadline $Deadline
        Assert-NativePrintDeadline -Deadline $Deadline
        $save = Find-BoundProcessUiElement -ProcessId $ProcessId -Binding $saveSurface -Names @('Save') -ControlTypes @('ControlType.Button') -Deadline $Deadline
        Invoke-ProcessUiElement -Element $save -ProcessId $ProcessId -Deadline $Deadline
        $saveActionTransport = 'uia-invoke-returned'
    }
    Wait-BoundProcessUiSurfaceClosed -ProcessId $ProcessId -Binding $saveSurface -Stage 'save-output-dialog' -Deadline $Deadline -ProcessStartUtcTicks $ProcessStartUtcTicks -ExpectedNativeSaveFileName $OutputPath -ActionTransport $saveActionTransport
    Wait-BoundProcessUiSurfaceClosed -ProcessId $ProcessId -Binding $surface -Stage 'second-print-dialog' -Deadline $Deadline
}

function Wait-StablePrintFile {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)][datetime]$Deadline)
    $last = -1L
    $stable = 0
    while ($true) {
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $exists = Test-Path -LiteralPath $Path -PathType Leaf
        if ([datetime]::UtcNow -ge $Deadline) { break }
        if ($exists) {
            $item = Get-Item -LiteralPath $Path
            if ([datetime]::UtcNow -ge $Deadline) { break }
            $attributes = $item.Attributes
            if ([datetime]::UtcNow -ge $Deadline) { break }
            if ($attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'Printed PDF output is reparse-backed.' }
            $length = [long]$item.Length
            if ([datetime]::UtcNow -ge $Deadline) { break }
            if ($length -gt $script:PrintPins.OutputBytesMaximum) { throw 'Printed PDF output is oversized.' }
            if ($length -eq 0) {
                $last = -1L
                $stable = 0
            } elseif ($length -lt 0) {
                throw 'Printed PDF output length is invalid.'
            } elseif ($length -eq $last) {
                $stable++
            } else {
                $stable = 0
                $last = $length
            }
            if ($stable -ge 2) {
                if ([datetime]::UtcNow -ge $Deadline) { break }
                return $item
            }
        } else {
            $last = -1L
            $stable = 0
        }
        $remainingMilliseconds = ($Deadline - [datetime]::UtcNow).TotalMilliseconds
        if ($remainingMilliseconds -le 0) { break }
        Start-Sleep -Milliseconds ([int][Math]::Min(250.0,[Math]::Ceiling($remainingMilliseconds)))
    }
    throw 'Printed PDF output did not become stable before its bounded deadline.'
}

function Initialize-PdfiumPrintProof {
    if ('SignedPdfiumPrintProof' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Security.Cryptography;

public sealed class SignedPdfiumPrintProof {
  [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] static extern IntPtr LoadLibraryExW(string path, IntPtr file, uint flags);
  [DllImport("kernel32.dll", CharSet=CharSet.Ansi, SetLastError=true)] static extern IntPtr GetProcAddress(IntPtr module, string name);
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void Init();
  [UnmanagedFunctionPointer(CallingConvention.Cdecl)] delegate void Destroy();
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
  readonly Init init;
  readonly Destroy destroy;
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
    var address = GetProcAddress(module, name);
    if (address == IntPtr.Zero) throw new InvalidOperationException("Signed PDFium is missing a required export.");
    return Marshal.GetDelegateForFunctionPointer<T>(address);
  }

  public SignedPdfiumPrintProof(string path) {
    module = LoadLibraryExW(Path.GetFullPath(path), IntPtr.Zero, 0x00000100 | 0x00001000);
    if (module == IntPtr.Zero) throw new InvalidOperationException("Signed PDFium could not be loaded from its exact installed path.");
    init=Get<Init>("FPDF_InitLibrary"); destroy=Get<Destroy>("FPDF_DestroyLibrary"); load=Get<LoadMem>("FPDF_LoadMemDocument64"); closeDocument=Get<CloseDocument>("FPDF_CloseDocument");
    pageCount=Get<PageCount>("FPDF_GetPageCount"); loadPage=Get<LoadPage>("FPDF_LoadPage"); closePage=Get<ClosePage>("FPDF_ClosePage"); pageWidth=Get<PageMetric>("FPDF_GetPageWidth"); pageHeight=Get<PageMetric>("FPDF_GetPageHeight");
    bitmapCreate=Get<BitmapCreate>("FPDFBitmap_Create"); bitmapDestroy=Get<BitmapDestroy>("FPDFBitmap_Destroy"); bitmapFill=Get<BitmapFill>("FPDFBitmap_FillRect"); render=Get<Render>("FPDF_RenderPageBitmap"); bitmapBuffer=Get<BitmapBuffer>("FPDFBitmap_GetBuffer"); bitmapStride=Get<BitmapInt>("FPDFBitmap_GetStride");
    init();
  }

  public sealed class Proof {
    public int Pages { get; set; }
    public double WidthPoints { get; set; }
    public double HeightPoints { get; set; }
    public int InkPixels { get; set; }
    public byte[] Fingerprint { get; set; }
    public string FingerprintSha256 { get; set; }
  }

  public Proof Inspect(string path, int renderSize, int fingerprintSize, long maximumBytes) {
    var bytes=File.ReadAllBytes(path);
    if(bytes.Length<=0 || bytes.LongLength>maximumBytes) throw new InvalidOperationException("PDF proof input is empty or oversized.");
    var handle=GCHandle.Alloc(bytes,GCHandleType.Pinned); IntPtr document=IntPtr.Zero, page=IntPtr.Zero, bitmap=IntPtr.Zero;
    try {
      document=load(handle.AddrOfPinnedObject(),(ulong)bytes.LongLength,IntPtr.Zero); if(document==IntPtr.Zero) throw new InvalidOperationException("Signed PDFium could not parse the PDF proof input.");
      int pages=pageCount(document); if(pages<=0 || pages>65536) throw new InvalidOperationException("PDF proof page count is invalid.");
      page=loadPage(document,0); if(page==IntPtr.Zero) throw new InvalidOperationException("Signed PDFium could not load the first proof page.");
      double width=pageWidth(page), height=pageHeight(page); if(!(width>0&&height>0&&width<20000&&height<20000)) throw new InvalidOperationException("PDF proof page dimensions are invalid.");
      bitmap=bitmapCreate(renderSize,renderSize,1); if(bitmap==IntPtr.Zero) throw new InvalidOperationException("Signed PDFium could not allocate the bounded proof bitmap.");
      bitmapFill(bitmap,0,0,renderSize,renderSize,0xFFFFFFFF); render(bitmap,page,0,0,renderSize,renderSize,0,0x801);
      int stride=bitmapStride(bitmap); if(stride<renderSize*4 || stride>renderSize*8) throw new InvalidOperationException("PDF proof bitmap stride is invalid.");
      var raw=new byte[stride*renderSize]; Marshal.Copy(bitmapBuffer(bitmap),raw,0,raw.Length);
      var gray=new byte[renderSize*renderSize]; int minX=renderSize,minY=renderSize,maxX=-1,maxY=-1,ink=0;
      for(int y=0;y<renderSize;y++) for(int x=0;x<renderSize;x++) { int i=y*stride+x*4; byte g=(byte)((raw[i]*29+raw[i+1]*150+raw[i+2]*77)>>8); gray[y*renderSize+x]=g; if(g<245){ink++;minX=Math.Min(minX,x);minY=Math.Min(minY,y);maxX=Math.Max(maxX,x);maxY=Math.Max(maxY,y);} }
      if(ink<64 || maxX<=minX || maxY<=minY) throw new InvalidOperationException("PDF proof first page has insufficient rendered content.");
      var fingerprint=new byte[fingerprintSize*fingerprintSize]; double boxW=maxX-minX+1, boxH=maxY-minY+1;
      for(int y=0;y<fingerprintSize;y++) for(int x=0;x<fingerprintSize;x++) { int sx=Math.Min(maxX,minX+(int)Math.Floor((x+0.5)*boxW/fingerprintSize)); int sy=Math.Min(maxY,minY+(int)Math.Floor((y+0.5)*boxH/fingerprintSize)); fingerprint[y*fingerprintSize+x]=gray[sy*renderSize+sx]; }
      string hash=Convert.ToHexString(SHA256.HashData(fingerprint));
      return new Proof{Pages=pages,WidthPoints=width,HeightPoints=height,InkPixels=ink,Fingerprint=fingerprint,FingerprintSha256=hash};
    } finally { if(bitmap!=IntPtr.Zero)bitmapDestroy(bitmap); if(page!=IntPtr.Zero)closePage(page); if(document!=IntPtr.Zero)closeDocument(document); if(handle.IsAllocated)handle.Free(); }
  }

  public static double Correlation(byte[] a, byte[] b) {
    if(a==null||b==null||a.Length!=b.Length||a.Length==0) throw new ArgumentException("PDF fingerprints are incompatible.");
    double ma=a.Average(x=>(double)x),mb=b.Average(x=>(double)x),num=0,da=0,db=0;
    for(int i=0;i<a.Length;i++){double x=a[i]-ma,y=b[i]-mb;num+=x*y;da+=x*x;db+=y*y;}
    if(da<=0||db<=0)return 0; return num/Math.Sqrt(da*db);
  }
  public static double MeanAbsoluteDifference(byte[] a, byte[] b) {
    if(a==null||b==null||a.Length!=b.Length||a.Length==0) throw new ArgumentException("PDF fingerprints are incompatible.");
    double sum=0;for(int i=0;i<a.Length;i++)sum+=Math.Abs(a[i]-b[i]);return sum/a.Length;
  }
}
'@
}

function Get-PdfiumPrintProof {
    param([Parameter(Mandatory = $true)][string]$PdfiumPath,[Parameter(Mandatory = $true)][string]$PdfPath)
    Initialize-PdfiumPrintProof
    $engine = [SignedPdfiumPrintProof]::new($PdfiumPath)
    return $engine.Inspect($PdfPath,$script:PrintPins.PdfRenderSize,$script:PrintPins.PdfFingerprintSize,$script:PrintPins.OutputBytesMaximum)
}

function Compare-PdfiumPrintProof {
    param([Parameter(Mandatory = $true)]$Source,[Parameter(Mandatory = $true)]$Output)
    $correlation = [SignedPdfiumPrintProof]::Correlation($Source.Fingerprint,$Output.Fingerprint)
    $difference = [SignedPdfiumPrintProof]::MeanAbsoluteDifference($Source.Fingerprint,$Output.Fingerprint)
    if ($Output.Pages -ne 1 -or $correlation -lt $script:PrintPins.CorrelationMinimum -or $difference -gt $script:PrintPins.MeanAbsoluteDifferenceMaximum) {
        throw 'Printed PDF did not parse as one page or correlate with the installed sample first page.'
    }
    return [pscustomobject]@{ correlation = [Math]::Round($correlation,6); meanAbsoluteDifference = [Math]::Round($difference,3) }
}

function Assert-PrintCleanupResult {
    param([Parameter(Mandatory = $true)]$Result)
    if ($Result.sessionDeleted -isnot [bool] -or -not $Result.sessionDeleted -or
        $Result.ownedProcessTreeStopped -isnot [bool] -or -not $Result.ownedProcessTreeStopped -or
        [int]$Result.relevantProcessesRemaining -ne 0) {
        throw 'Installed print cleanup did not delete the session, stop the owned process tree, and clear relevant processes.'
    }
}

function Wait-OwnedPrintApplication {
    param([Parameter(Mandatory = $true)]$Driver,[Parameter(Mandatory = $true)][datetime]$StartedAfter,[Parameter(Mandatory = $true)][string]$ApplicationPath,[Parameter(Mandatory = $true)][datetime]$Deadline)
    do {
        $owned = @(Get-OwnedLaunchProcesses -RootProcessId $Driver.Id -StartedAfter $StartedAfter)
        $apps = @($owned | Where-Object { ([string]$_.Path).Equals($ApplicationPath,[StringComparison]::OrdinalIgnoreCase) })
        if ($apps.Count -eq 1) { return [pscustomobject]@{ ProcessId = [int]$apps[0].ProcessId; ProcessStartUtcTicks = [long]$apps[0].StartTicks; Owned = $owned } }
        if ($apps.Count -gt 1) { throw 'The WebDriver process tree contains multiple installed application processes.' }
        Start-Sleep -Milliseconds 150
    } while ([datetime]::UtcNow -lt $Deadline)
    throw 'The exact installed application process did not appear before the shared deadline.'
}

function Assert-PrintOwnedExecutables {
    param(
        [object[]]$Owned,
        [string]$ApplicationPath,
        [string]$EdgeDriverPath,
        [Parameter(Mandatory = $true)][int]$RootProcessId,
        [string]$SystemDirectory,
        [scriptblock]$ConsoleHostSignatureProvider,
        [scriptblock]$ConsoleHostVersionInfoProvider
    )
    $trustedConsoleHosts = Assert-TrustedConsoleHostTopology -Owned $Owned -RootProcessId $RootProcessId -SystemDirectory $SystemDirectory -SignatureProvider $ConsoleHostSignatureProvider -VersionInfoProvider $ConsoleHostVersionInfoProvider
    if ([int]$trustedConsoleHosts -lt 2) { throw 'The print verification console-host topology was incomplete.' }
    $app = 0; $edge = 0; $webviews = 0; $consoleHosts = 0
    foreach ($item in $Owned) {
        $path = [string]$item.Path
        if ($path.Equals($ApplicationPath,[StringComparison]::OrdinalIgnoreCase)) { $app++; continue }
        if ($path.Equals($EdgeDriverPath,[StringComparison]::OrdinalIgnoreCase)) { $edge++; continue }
        if ([IO.Path]::GetFileName($path).Equals('msedgewebview2.exe',[StringComparison]::OrdinalIgnoreCase)) { $webviews++; continue }
        if ([IO.Path]::GetFileName($path).Equals('conhost.exe',[StringComparison]::OrdinalIgnoreCase)) { $consoleHosts++; continue }
        throw 'The print verification process tree contains an unexpected executable.'
    }
    if ($app -ne 1 -or $edge -ne 1 -or $webviews -lt 1 -or $consoleHosts -ne [int]$trustedConsoleHosts) { throw 'The print verification process tree is incomplete or ambiguous.' }
}

function Invoke-RealInstalledPrintDialog {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,
        [Parameter(Mandatory = $true)][string]$SamplePath,
        [Parameter(Mandatory = $true)][string]$PdfiumPath,
        [Parameter(Mandatory = $true)][string]$TauriDriverPath,
        [Parameter(Mandatory = $true)][string]$EdgeDriverPath,
        [Parameter(Mandatory = $true)][string]$ProfileRoot,
        [Parameter(Mandatory = $true)][string]$SettingsRoot,
        [Parameter(Mandatory = $true)][string]$OutputPdfPath,
        [Parameter(Mandatory = $true)][string]$ExpectedEdgeDriverVersion,
        [Parameter(Mandatory = $true)]$PrinterFacts
    )
    $driverCapture = $null; $driver = $null; $sessionId = $null; $captured = @(); $result = $null
    $sessionDeleteOutcome = 'requestfailed'; $driverExited = $false
    $driverStopOutcome = 'not-invoked'; $processesQuiescent = $false; $residualCategory = 'multiple'
    $capturedOutcomes = [pscustomobject]@{ application='absent';tauriDriver='absent';edgeDriver='absent';webview='absent';ocrEngine='absent';other='absent' }
    $residualFacts = [pscustomobject]@{ ownership='none';application=$false;tauriDriver=$false;edgeDriver=$false;webview=$false;ocrEngine=$false;other=$false }
    try {
        $handoffDeadline = [datetime]::UtcNow.AddMilliseconds($script:PrintPins.PortHandoffTimeoutMilliseconds)
        $null = Wait-FixedWebDriverPortsFree -Deadline $handoffDeadline
        $deadline = [datetime]::UtcNow.AddMilliseconds($script:PrintPins.TotalTimeoutMilliseconds)
        $startedAfter = [datetime]::UtcNow
        $driverCapture = Start-BoundedDiscardProcess -Path $TauriDriverPath -Arguments @("--port=$($script:LaunchPins.WebDriverPort)","--native-port=$($script:LaunchPins.NativeDriverPort)","--native-driver=$EdgeDriverPath") -Environment (Get-MinimalWindowsProcessEnvironment)
        $driverCapture.Start(); $driver = $driverCapture.Process
        $status = $null
        do {
            try { $status = Invoke-BoundedLoopbackJson -Method GET -Path '/status' -Deadline $deadline } catch { }
            if ($null -ne $status -and [bool]$status.value.ready) { break }
            if ($driver.HasExited) { throw 'Pinned tauri-driver exited before print verification readiness.' }
            Start-Sleep -Milliseconds 200
        } while ([datetime]::UtcNow -lt $deadline)
        if ($null -eq $status -or -not [bool]$status.value.ready) { throw 'Pinned tauri-driver did not become ready for print verification.' }
        $nativeVersion = Wait-NativeDriverStatus -ExpectedVersion $ExpectedEdgeDriverVersion -Deadline $deadline -TauriDriver $driver
        $sessionBody = [ordered]@{ capabilities=[ordered]@{ alwaysMatch=[ordered]@{ browserName='wry';'tauri:options'=[ordered]@{application=$ApplicationPath;args=@();webviewOptions=[ordered]@{userDataFolder=$ProfileRoot}} } } }
        $session = Invoke-BoundedLoopbackJson -Method POST -Path '/session' -Body $sessionBody -Deadline $deadline
        if ([string]$session.value.sessionId -cnotmatch '^[A-Za-z0-9-]+$') { throw 'WebDriver did not return a bounded print session identifier.' }
        $sessionId = [string]$session.value.sessionId
        $capabilities = $session.value.capabilities
        $runtimeVersion = [string]$capabilities.browserVersion
        $vendor = $capabilities.PSObject.Properties['msedge.msedgedriverVersion']
        if ($null -ne $vendor -and ([string]$vendor.Value -split '\s+')[0] -cne $nativeVersion) { throw 'Print session EdgeDriver version disagrees with native status.' }
        $userData = $capabilities.PSObject.Properties['msedge.userDataDir']
        $ownedApp = Wait-OwnedPrintApplication -Driver $driver -StartedAfter $startedAfter -ApplicationPath $ApplicationPath -Deadline $deadline
        $appProcessId = [int]$ownedApp.ProcessId; $appProcessStartUtcTicks = [long]$ownedApp.ProcessStartUtcTicks; $captured += @($ownedApp.Owned)
        $trustedConsoleHostCount = Assert-TrustedConsoleHostTopology -Owned $captured -RootProcessId $driver.Id
        if ([int]$trustedConsoleHostCount -lt 2) { throw 'The print verification console-host topology was incomplete before UI Automation.' }
        $homeOracle = Wait-WebDriverOracle -SessionId $sessionId -Script "return {ready:document.readyState==='complete',title:document.title,sample:[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Explore a sample PDF').length};" -Deadline $deadline -Kind 'Installed print home UI' -Predicate { param($v) [bool]$v.ready -and [string]$v.title -ceq 'PDF Workstation' -and [int]$v.sample -eq 1 }
        $clicked = Invoke-WebDriverScript -SessionId $sessionId -Script "const b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Explore a sample PDF');if(b.length===1)b[0].click();return b.length===1;" -Deadline $deadline
        if ($clicked -isnot [bool] -or -not $clicked) { throw 'The installed sample-open control was unavailable.' }
        $sampleOracle = @'
const i=document.querySelector('img[alt="Page 1"]');
return {tab:[...document.querySelectorAll('button')].some(x=>x.textContent.includes('welcome.pdf')),pages:[...document.querySelectorAll('span')].some(x=>x.textContent.trim()==='/ 6'),image:!!i&&i.complete&&i.naturalWidth>0&&i.naturalHeight>0&&i.src.startsWith('blob:')};
'@
        $sample = Wait-WebDriverOracle -SessionId $sessionId -Script $sampleOracle -Deadline $deadline -Kind 'Installed sample before printing' -Predicate { param($v) [bool]$v.tab -and [bool]$v.pages -and [bool]$v.image }
        $printDialog = Invoke-WebDriverScript -SessionId $sessionId -Script "const b=[...document.querySelectorAll('button')].filter(x=>x.getAttribute('aria-label')==='Print');if(b.length===1)b[0].click();return b.length===1;" -Deadline $deadline
        if ($printDialog -isnot [bool] -or -not $printDialog) { throw 'The exact installed Print control was unavailable.' }
        $printDialogOracle = @'
return document.querySelectorAll('dialog[aria-labelledby="print-title"]').length===1;
'@
        $null = Wait-WebDriverOracle -SessionId $sessionId -Script $printDialogOracle -Deadline $deadline -Kind 'Installed print dialog' -Predicate { param($v) $v -is [bool] -and $v }

        $nativeDeadline = Get-PrintPhaseDeadline -TotalDeadline $deadline -MaximumMilliseconds $script:PrintPins.NativeDialogTimeoutMilliseconds
        $originalNativeBaseline = @(Get-ProcessTopLevelUiSnapshot -ProcessId $appProcessId -Deadline $nativeDeadline)
        $firstNativeWindowBaseline = @(Get-ProcessNativeWindowSnapshot -ProcessId $appProcessId -Deadline $nativeDeadline -TopLevelOnly)
        Open-NativePrintDialogFromWebView -SessionId $sessionId -Deadline $nativeDeadline
        $null = Cancel-NativePrintDialog -ProcessId $appProcessId -Baseline @() -NativeBaseline $firstNativeWindowBaseline -Stage 'first-print-dialog' -Deadline $nativeDeadline
        Wait-ProcessTopLevelUiBaselineRestored -ProcessId $appProcessId -Baseline $originalNativeBaseline -Stage 'first-native-cleanup' -Deadline $nativeDeadline
        $cancelStatus = Wait-WebPrintStatus -SessionId $sessionId -Prefix 'Printing canceled.' -Deadline $deadline

        $nativeDeadline = Get-PrintPhaseDeadline -TotalDeadline $deadline -MaximumMilliseconds $script:PrintPins.NativeDialogTimeoutMilliseconds
        $secondNativeBaseline = @(Get-ProcessTopLevelUiSnapshot -ProcessId $appProcessId -Deadline $nativeDeadline)
        Assert-ProcessTopLevelUiBaselineMatch -ProcessId $appProcessId -Expected $originalNativeBaseline -Actual $secondNativeBaseline -Deadline $nativeDeadline
        $secondNativeWindowBaseline = @(Get-ProcessNativeWindowSnapshot -ProcessId $appProcessId -Deadline $nativeDeadline -TopLevelOnly)
        Open-NativePrintDialogFromWebView -SessionId $sessionId -Deadline $nativeDeadline
        $output = [ordered]@{ status='explicit-unavailable';bytes=$null;sha256=$null;pages=$null;widthPoints=$null;heightPoints=$null;sourceFingerprintSha256=$null;outputFingerprintSha256=$null;correlation=$null;meanAbsoluteDifference=$null }
        if ([bool]$PrinterFacts.microsoftPrintToPdfAvailable) {
            Submit-NativePrintToPdf -ProcessId $appProcessId -ProcessStartUtcTicks $appProcessStartUtcTicks -Baseline @() -NativeBaseline $secondNativeWindowBaseline -OutputPath $OutputPdfPath -Deadline $nativeDeadline
            $item = Wait-StablePrintFile -Path $OutputPdfPath -Deadline (Get-PrintPhaseDeadline -TotalDeadline $deadline -MaximumMilliseconds $script:PrintPins.OutputTimeoutMilliseconds)
            $submitted = Wait-WebPrintStatus -SessionId $sessionId -Prefix '1 page submitted to the printer.' -Deadline $deadline
            $sourceProof = Get-PdfiumPrintProof -PdfiumPath $PdfiumPath -PdfPath $SamplePath
            $outputProof = Get-PdfiumPrintProof -PdfiumPath $PdfiumPath -PdfPath $OutputPdfPath
            $comparison = Compare-PdfiumPrintProof -Source $sourceProof -Output $outputProof
            $output = [ordered]@{
                status='verified';bytes=[uint64]$item.Length;sha256=Get-ExactSha256 -Path $OutputPdfPath
                pages=[int]$outputProof.Pages;widthPoints=[Math]::Round([double]$outputProof.WidthPoints,3);heightPoints=[Math]::Round([double]$outputProof.HeightPoints,3)
                sourceFingerprintSha256=[string]$sourceProof.FingerprintSha256;outputFingerprintSha256=[string]$outputProof.FingerprintSha256
                correlation=[double]$comparison.correlation;meanAbsoluteDifference=[double]$comparison.meanAbsoluteDifference
            }
        } else {
            $null = Cancel-NativePrintDialog -ProcessId $appProcessId -Baseline @() -NativeBaseline $secondNativeWindowBaseline -Stage 'second-print-dialog' -Deadline $nativeDeadline
            $null = Wait-WebPrintStatus -SessionId $sessionId -Prefix 'Printing canceled.' -Deadline $deadline
        }
        Close-WebPrintDialog -SessionId $sessionId -Deadline $deadline
        Wait-ProcessTopLevelUiBaselineRestored -ProcessId $appProcessId -Baseline $originalNativeBaseline -Stage 'final-native-cleanup' -Deadline (Get-PrintPhaseDeadline -TotalDeadline $deadline -MaximumMilliseconds 3000)
        $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter)
        $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured)
        Assert-PrintOwnedExecutables -Owned $captured -ApplicationPath $ApplicationPath -EdgeDriverPath $EdgeDriverPath -RootProcessId $driver.Id
        $profileBinding = if ($null -ne $userData -and -not [string]::IsNullOrWhiteSpace([string]$userData.Value)) { 'session-capability-' + (Get-ExactProfileBinding -Candidate ([string]$userData.Value) -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot) } else { Get-OwnedProfileBinding -Owned $captured -RequestedProfile $ProfileRoot -SettingsRoot $SettingsRoot }
        if ($driverCapture.Exceeded) { throw 'WebDriver diagnostic output exceeded its discarded character cap.' }
        $result = [pscustomobject]@{
            nativeDriverVersion=$nativeVersion;returnedRuntimeVersion=$runtimeVersion;profileBinding=$profileBinding
            nativeDialogOpenVerified=$true;nativeDialogCancelVerified=$true;cancelResultTransportVerified=([string]$cancelStatus.text).StartsWith('Printing canceled.');nativeDialogReopenVerified=$true
            webPrintDialogClosed=$true;output=[pscustomobject]$output
        }
    } finally {
        $cleanupDeadline = [datetime]::UtcNow.AddSeconds(10)
        if ($sessionId) { $sessionDeleteOutcome = Invoke-SessionDeleteOutcome -SessionId $sessionId -Deadline $cleanupDeadline }
        if ($driver) {
            try { $captured += @(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $startedAfter); $captured = @(Get-UniqueOwnedLaunchProcesses -Processes $captured) } catch { }
            $processCleanupDeadline = [datetime]::UtcNow.AddMilliseconds($script:LaunchPins.CleanupProcessTimeoutMilliseconds)
            $capturedOutcomes = Stop-OwnedLaunchProcesses -TauriDriver $driver -Captured $captured -Deadline $processCleanupDeadline
            $driverStopOutcome = [string]$capturedOutcomes.rootOutcome; $driverExited = [bool]$driver.HasExited
            $quiescence = Wait-LaunchProcessQuiescence -Deadline $processCleanupDeadline
            $processesQuiescent = [bool]$quiescence.stable
            $remaining = @($quiescence.processes); $residualCategory = Get-LaunchResidualCategory -Processes $remaining
            $residualFacts = Get-LaunchResidualFacts -Processes $remaining -Captured $captured
        }
        if ($driverCapture) { $driverCapture.Dispose() }
    }
    $clear = $processesQuiescent -and $residualCategory -ceq 'none'
    Assert-LaunchCleanupState -Result $result -SessionDeleteOutcome $sessionDeleteOutcome -DriverExited $driverExited -RelevantProcessesClear $clear -DriverStopOutcome $driverStopOutcome -ResidualCategory $residualCategory -CapturedApplication $capturedOutcomes.application -CapturedTauriDriver $capturedOutcomes.tauriDriver -CapturedEdgeDriver $capturedOutcomes.edgeDriver -CapturedWebView $capturedOutcomes.webview -CapturedOcrEngine $capturedOutcomes.ocrEngine -CapturedOther $capturedOutcomes.other -ResidualOwnership $residualFacts.ownership -ResidualApplication $residualFacts.application -ResidualTauriDriver $residualFacts.tauriDriver -ResidualEdgeDriver $residualFacts.edgeDriver -ResidualWebView $residualFacts.webview -ResidualOcrEngine $residualFacts.ocrEngine -ResidualOther $residualFacts.other
    $result | Add-Member sessionDeleted ($sessionDeleteOutcome -ceq 'verified')
    $result | Add-Member ownedProcessTreeStopped $driverExited
    $result | Add-Member relevantProcessesRemaining 0
    return $result
}

function Invoke-InstalledPrintDialogVerification {
    param(
        [Parameter(Mandatory = $true)][string]$ApplicationPath,[Parameter(Mandatory = $true)]$ApplicationReceipt,
        [Parameter(Mandatory = $true)][string]$SamplePath,[Parameter(Mandatory = $true)]$SampleReceipt,
        [Parameter(Mandatory = $true)][string]$PdfiumPath,[Parameter(Mandatory = $true)]$PdfiumReceipt,
        [Parameter(Mandatory = $true)][string]$WebDriverRoot,[Parameter(Mandatory = $true)][string]$ProfileRoot,
        [Parameter(Mandatory = $true)][string]$SettingsRoot,[Parameter(Mandatory = $true)][string]$OutputPdfPath,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher,[Parameter(Mandatory = $true)]$PrinterFacts,
        [scriptblock]$ProcessProvider
    )
    Assert-PrintCapabilityFacts -Facts $PrinterFacts
    Assert-FileReceipt -Path $ApplicationPath -Bytes ([uint64]$ApplicationReceipt.bytes) -Sha256 ([string]$ApplicationReceipt.sha256) -Kind 'Installed signed print application'
    $null = Assert-TrustedWindowsSignature -Path $ApplicationPath -ExpectedPublisher $ExpectedPublisher
    Assert-FileReceipt -Path $SamplePath -Bytes ([uint64]$SampleReceipt.bytes) -Sha256 ([string]$SampleReceipt.sha256) -Kind 'Installed print sample'
    Assert-FileReceipt -Path $PdfiumPath -Bytes ([uint64]$PdfiumReceipt.bytes) -Sha256 ([string]$PdfiumReceipt.sha256) -Kind 'Installed signed PDFium'
    $null = Assert-TrustedWindowsSignature -Path $PdfiumPath -ExpectedPublisher $ExpectedPublisher
    $receipt = Get-Content -LiteralPath (Join-Path $WebDriverRoot 'webdriver-receipt.json') -Raw -Encoding UTF8 | ConvertFrom-Json
    Assert-WebDriverReceipt -Receipt $receipt
    $tauri = Join-Path $WebDriverRoot 'tauri-driver-install/bin/tauri-driver.exe'; $edge = Join-Path $WebDriverRoot 'edge-driver/msedgedriver.exe'
    Assert-FileReceipt -Path $tauri -Bytes ([uint64]$receipt.tauriDriver.bytes) -Sha256 ([string]$receipt.tauriDriver.sha256) -Kind 'Pinned tauri-driver executable'
    Assert-FileReceipt -Path $edge -Bytes ([uint64]$receipt.edgeDriver.bytes) -Sha256 ([string]$receipt.edgeDriver.sha256) -Kind 'Exact EdgeDriver executable'
    $null = Assert-TrustedWindowsSignature -Path $edge -ExpectedPublisher $script:LaunchPins.EdgePublisher
    $expectedSettings = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\')
    $actualSettings = [IO.Path]::GetFullPath($SettingsRoot).TrimEnd('\')
    if (-not $actualSettings.Equals($expectedSettings,[StringComparison]::OrdinalIgnoreCase)) { throw 'Print settings root does not match the exact application identity.' }
    Assert-NoReparseAncestors -Path $actualSettings
    if (-not (Test-Path -LiteralPath $actualSettings -PathType Container) -or @(Get-ChildItem -LiteralPath $actualSettings -Force).Count -ne 0) { throw 'Print settings root must be an empty controlled directory before launch.' }
    if (Test-Path -LiteralPath $ProfileRoot) { throw 'Print WebView profile must be fresh.' }
    [IO.Directory]::CreateDirectory($ProfileRoot) | Out-Null
    Assert-NoReparseAncestors -Path $ProfileRoot
    if (@(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -ne 0) { throw 'Print WebView profile was not empty.' }
    if (Test-Path -LiteralPath $OutputPdfPath) { throw 'Printed PDF output path must be fresh.' }
    if (@(Get-LaunchProcessSnapshot).Count -ne 0) { throw 'A relevant application or WebDriver process existed before print verification.' }
    $result = if ($ProcessProvider) { & $ProcessProvider $ApplicationPath $SamplePath $PdfiumPath $tauri $edge $ProfileRoot $SettingsRoot $OutputPdfPath ([string]$receipt.edgeDriver.version) $PrinterFacts } else { Invoke-RealInstalledPrintDialog -ApplicationPath $ApplicationPath -SamplePath $SamplePath -PdfiumPath $PdfiumPath -TauriDriverPath $tauri -EdgeDriverPath $edge -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -OutputPdfPath $OutputPdfPath -ExpectedEdgeDriverVersion ([string]$receipt.edgeDriver.version) -PrinterFacts $PrinterFacts }
    Assert-PrintExactProperties -Value $result -Expected @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','nativeDialogOpenVerified','nativeDialogCancelVerified','cancelResultTransportVerified','nativeDialogReopenVerified','webPrintDialogClosed','output','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') -Kind 'Installed print result'
    Assert-PrintExactProperties -Value $result.output -Expected @('status','bytes','sha256','pages','widthPoints','heightPoints','sourceFingerprintSha256','outputFingerprintSha256','correlation','meanAbsoluteDifference') -Kind 'Installed print output result'
    $runtime = ([string]$result.returnedRuntimeVersion).Split('.'); $expectedRuntime = ([string]$receipt.webView2RuntimeVersion).Split('.')
    if ([string]$result.nativeDriverVersion -cne [string]$receipt.edgeDriver.version -or $runtime.Count -ne 4 -or ($runtime[0..2] -join '.') -cne ($expectedRuntime[0..2] -join '.') -or
        [string]$result.profileBinding -cnotmatch '^(session-capability-|owned-webview-)(requested-profile|requested-ebwebview|tauri-app-settings-ebwebview)$' -or
        -not [bool]$result.nativeDialogOpenVerified -or -not [bool]$result.nativeDialogCancelVerified -or -not [bool]$result.cancelResultTransportVerified -or -not [bool]$result.nativeDialogReopenVerified -or -not [bool]$result.webPrintDialogClosed -or
        -not [bool]$result.sessionDeleted -or -not [bool]$result.ownedProcessTreeStopped -or [int]$result.relevantProcessesRemaining -ne 0) { throw 'Installed print result did not satisfy native dialog, transport, reopen, and cleanup oracles.' }
    Assert-PrintCleanupResult -Result $result
    if ([bool]$PrinterFacts.microsoftPrintToPdfAvailable) {
        if ([string]$result.output.status -cne 'verified' -or [uint64]$result.output.bytes -eq 0 -or [string]$result.output.sha256 -cnotmatch '^[A-F0-9]{64}$' -or [int]$result.output.pages -ne 1 -or
            [string]$result.output.sourceFingerprintSha256 -cnotmatch '^[A-F0-9]{64}$' -or [string]$result.output.outputFingerprintSha256 -cnotmatch '^[A-F0-9]{64}$' -or
            [double]$result.output.correlation -lt $script:PrintPins.CorrelationMinimum -or [double]$result.output.meanAbsoluteDifference -gt $script:PrintPins.MeanAbsoluteDifferenceMaximum) { throw 'Available Microsoft Print to PDF output was not fully parsed, rendered, and correlated.' }
        Assert-FileReceipt -Path $OutputPdfPath -Bytes ([uint64]$result.output.bytes) -Sha256 ([string]$result.output.sha256) -Kind 'Printed PDF output'
    } elseif ([string]$result.output.status -cne 'explicit-unavailable' -or (Test-Path -LiteralPath $OutputPdfPath)) { throw 'Unavailable Microsoft Print to PDF was not reported explicitly without an output file.' }
    $applicationProfile = Join-Path $actualSettings 'EBWebView'
    $requestedChild = Join-Path $ProfileRoot 'EBWebView'
    $usesApplicationProfile = ([string]$result.profileBinding).EndsWith('tauri-app-settings-ebwebview',[StringComparison]::Ordinal)
    $usesRequestedChild = ([string]$result.profileBinding).EndsWith('requested-ebwebview',[StringComparison]::Ordinal)
    if ($usesApplicationProfile) {
        if (-not (Test-Path -LiteralPath $applicationProfile -PathType Container) -or @(Get-ChildItem -LiteralPath $applicationProfile -Force).Count -eq 0 -or @(Get-ChildItem -LiteralPath $actualSettings -Force).Count -ne 1 -or @(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -ne 0) { throw 'The print run did not exclusively populate its controlled application profile.' }
        Assert-NoReparseAncestors -Path $applicationProfile
    } elseif ($usesRequestedChild) {
        if (-not (Test-Path -LiteralPath $requestedChild -PathType Container) -or @(Get-ChildItem -LiteralPath $requestedChild -Force).Count -eq 0 -or @(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -ne 1 -or @(Get-ChildItem -LiteralPath $actualSettings -Force).Count -ne 0) { throw 'The print run did not exclusively populate its controlled requested child profile.' }
        Assert-NoReparseAncestors -Path $requestedChild
    } else {
        if (@(Get-ChildItem -LiteralPath $ProfileRoot -Force).Count -eq 0 -or @(Get-ChildItem -LiteralPath $actualSettings -Force).Count -ne 0) { throw 'The print run did not exclusively populate its exact requested profile.' }
    }
    return $result
}
