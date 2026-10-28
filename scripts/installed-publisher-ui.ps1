[CmdletBinding()]
param([string] $PublisherUiChildPayload)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$script:PublisherUiScriptPath = [IO.Path]::GetFullPath($MyInvocation.MyCommand.Path)
$script:PublisherUiPins = [ordered]@{
    Culture = 'en-US'
    DeadlineMilliseconds = 70000
    InteractionMilliseconds = 65000
    ChildDeadlineMilliseconds = 64000
    CleanupMilliseconds = 5000
    PollMilliseconds = 100
    ElementMaximum = 256
    StringLengthMaximum = 256
    StdoutMaximum = 4096
    StderrMaximum = 2048
    PropertyClass = '#32770'
    SignatureTab = 'Digital Signatures'
    DetailsButton = 'Details'
    DetailsTitle = 'Digital Signature Details'
    ValidStatus = 'This digital signature is OK.'
    FailureTokenPrefix = 'publisher-ui-child-failed:v1'
    FailureStages = @(
        'payload','culture','fresh-surface','open-properties','wait-properties','signature-tab','signature-grid',
        'signer-row','details-invoke','details-wait','details-status','details-signer','cleanup-details',
        'invoke-complete','cleanup-properties'
    )
    FailureCategories = @(
        'unexpected','deadline','invoke-running','invoke-completed','invoke-faulted','invoke-canceled',
        'modal-missing','modal-owner-mismatch','modal-ambiguous','cleanup'
    )
}
$script:PublisherUiChildStage = 'payload'
$script:PublisherUiChildCategory = 'unexpected'

function Initialize-PublisherUiInterop {
    if (-not ('Windows.Automation.AutomationElement' -as [type])) {
        Add-Type -AssemblyName UIAutomationClient
        Add-Type -AssemblyName UIAutomationTypes
    }
    if (-not ('Smacrobat.PublisherUi.NativeShell' -as [type])) {
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Runtime.InteropServices;

namespace Smacrobat.PublisherUi {
    public static class NativeShell {
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct SHELLEXECUTEINFO {
            public uint cbSize;
            public uint fMask;
            public IntPtr hwnd;
            [MarshalAs(UnmanagedType.LPWStr)] public string lpVerb;
            [MarshalAs(UnmanagedType.LPWStr)] public string lpFile;
            [MarshalAs(UnmanagedType.LPWStr)] public string lpParameters;
            [MarshalAs(UnmanagedType.LPWStr)] public string lpDirectory;
            public int nShow;
            public IntPtr hInstApp;
            public IntPtr lpIDList;
            [MarshalAs(UnmanagedType.LPWStr)] public string lpClass;
            public IntPtr hkeyClass;
            public uint dwHotKey;
            public IntPtr hIconOrMonitor;
            public IntPtr hProcess;
        }

        [DllImport("shell32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool ShellExecuteEx(ref SHELLEXECUTEINFO info);
        [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr hwnd);
        [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr hwnd, uint command);
        private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr state);
        [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr state);
        [DllImport("user32.dll", SetLastError = true)] private static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);

        private const uint SEE_MASK_INVOKEIDLIST = 0x0000000C;
        private const uint SEE_MASK_NOASYNC = 0x00000100;
        private const uint GW_OWNER = 4;
        private const uint WM_CLOSE = 0x0010;

        public static void ShowProperties(string path) {
            SHELLEXECUTEINFO info = new SHELLEXECUTEINFO();
            info.cbSize = (uint)Marshal.SizeOf(typeof(SHELLEXECUTEINFO));
            info.fMask = SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC;
            info.lpVerb = "properties";
            info.lpFile = path;
            info.nShow = 1;
            if (!ShellExecuteEx(ref info)) throw new Win32Exception(Marshal.GetLastWin32Error(), "Windows Shell refused the file properties surface.");
        }

        public static bool WindowExists(long handleValue) { return IsWindow(new IntPtr(handleValue)); }
        public static long OwnerOf(long handleValue) { return GetWindow(new IntPtr(handleValue), GW_OWNER).ToInt64(); }
        public static long[] OwnedBy(long handleValue) {
            IntPtr owner = new IntPtr(handleValue);
            List<long> matches = new List<long>();
            EnumWindows((hwnd, state) => {
                if (GetWindow(hwnd, GW_OWNER) == owner) matches.Add(hwnd.ToInt64());
                return true;
            }, IntPtr.Zero);
            return matches.ToArray();
        }
        public static void CloseWindow(long handleValue) {
            IntPtr handle = new IntPtr(handleValue);
            if (IsWindow(handle) && !PostMessage(handle, WM_CLOSE, IntPtr.Zero, IntPtr.Zero)) {
                throw new Win32Exception(Marshal.GetLastWin32Error(), "Windows refused bounded publisher UI cleanup.");
            }
        }
    }
}
'@
    }
    if (-not ('Smacrobat.PublisherUi.AutomationInvoke' -as [type])) {
        $automationReferences = @(
            [Windows.Automation.AutomationElement].Assembly.Location,
            [Windows.Automation.ControlType].Assembly.Location
        ) | Sort-Object -Unique
        Add-Type -ReferencedAssemblies $automationReferences -TypeDefinition @'
using System;
using System.Threading;
using System.Threading.Tasks;
using System.Windows.Automation;

namespace Smacrobat.PublisherUi {
    public static class AutomationInvoke {
        public static Task InvokeExactButtonAsync(long rootHandleValue, string rootTitle, string rootClass, string buttonName) {
            if (rootHandleValue == 0) throw new ArgumentOutOfRangeException(nameof(rootHandleValue));
            if (rootTitle == null) throw new ArgumentNullException(nameof(rootTitle));
            if (rootClass == null) throw new ArgumentNullException(nameof(rootClass));
            if (buttonName == null) throw new ArgumentNullException(nameof(buttonName));
            return Task.Factory.StartNew(() => {
                AutomationElement root = AutomationElement.FromHandle(new IntPtr(rootHandleValue));
                if (root == null || root.Current.NativeWindowHandle != rootHandleValue ||
                    !String.Equals(root.Current.Name, rootTitle, StringComparison.Ordinal) ||
                    !String.Equals(root.Current.ClassName, rootClass, StringComparison.Ordinal) ||
                    root.Current.ControlType != ControlType.Window || !root.Current.IsEnabled || root.Current.IsOffscreen) {
                    throw new InvalidOperationException("Publisher UI invocation root identity changed.");
                }
                Condition condition = new AndCondition(
                    new PropertyCondition(AutomationElement.NameProperty, buttonName),
                    new PropertyCondition(AutomationElement.ControlTypeProperty, ControlType.Button),
                    new PropertyCondition(AutomationElement.IsEnabledProperty, true));
                AutomationElementCollection matches = root.FindAll(TreeScope.Descendants, condition);
                if (matches.Count != 1) throw new InvalidOperationException("Publisher UI invocation action identity changed.");
                object pattern = matches[0].GetCurrentPattern(InvokePattern.Pattern);
                ((InvokePattern)pattern).Invoke();
            }, CancellationToken.None, TaskCreationOptions.LongRunning, TaskScheduler.Default);
        }
    }
}
'@
    }
}

function Set-PublisherUiChildStage {
    param([Parameter(Mandatory = $true)][string]$Stage)
    if ($script:PublisherUiPins.FailureStages -cnotcontains $Stage) { throw 'Windows publisher UI child stage is invalid.' }
    $script:PublisherUiChildStage = $Stage
    $script:PublisherUiChildCategory = 'unexpected'
}

function Set-PublisherUiChildCategory {
    param([Parameter(Mandatory = $true)][string]$Category)
    if ($script:PublisherUiPins.FailureCategories -cnotcontains $Category) { throw 'Windows publisher UI child category is invalid.' }
    $script:PublisherUiChildCategory = $Category
}

function New-PublisherUiChildFailureToken {
    param(
        [Parameter(Mandatory = $true)][string]$Stage,
        [Parameter(Mandatory = $true)][string]$Category
    )
    if ($script:PublisherUiPins.FailureStages -cnotcontains $Stage -or
        $script:PublisherUiPins.FailureCategories -cnotcontains $Category) {
        throw 'Windows publisher UI child failure token is invalid.'
    }
    return "$($script:PublisherUiPins.FailureTokenPrefix):$Stage`:$Category"
}

function Initialize-PublisherUiIsolation {
    if ('Smacrobat.PublisherUiIsolation.BoundedProcess' -as [type]) { return }
    if ($PSVersionTable.PSVersion.Major -lt 7) { throw 'Windows publisher UI helper isolation requires PowerShell 7 or newer.' }
    Add-Type -TypeDefinition @'
using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Diagnostics;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

namespace Smacrobat.PublisherUiIsolation {
    public sealed class BoundedProcessResult {
        public string Stdout { get; set; }
        public string Stderr { get; set; }
        public int ExitCode { get; set; }
        public int ElapsedMilliseconds { get; set; }
        public bool TimedOut { get; set; }
        public bool StdoutExceeded { get; set; }
        public bool StderrExceeded { get; set; }
        public bool JobAssigned { get; set; }
        public bool JobTerminated { get; set; }
        public bool ProcessStopped { get; set; }
        public int ProcessId { get; set; }
        public long ProcessStartTimeUtcTicks { get; set; }
    }

    internal sealed class BoundedCapture {
        private readonly object gate = new object();
        private readonly StringBuilder text;
        private readonly int maximum;
        public volatile bool Exceeded;
        public BoundedCapture(int maximum) { this.maximum = maximum; text = new StringBuilder(Math.Min(maximum, 4096)); }
        public void Append(char[] buffer, int count) {
            lock (gate) {
                int remaining = maximum - text.Length;
                if (count > remaining) {
                    if (remaining > 0) text.Append(buffer, 0, remaining);
                    Exceeded = true;
                } else { text.Append(buffer, 0, count); }
            }
        }
        public string Snapshot() { lock (gate) { return text.ToString(); } }
    }

    public static class BoundedProcess {
        [StructLayout(LayoutKind.Sequential)]
        private struct IO_COUNTERS {
            public ulong ReadOperationCount, WriteOperationCount, OtherOperationCount;
            public ulong ReadTransferCount, WriteTransferCount, OtherTransferCount;
        }
        [StructLayout(LayoutKind.Sequential)]
        private struct JOBOBJECT_BASIC_LIMIT_INFORMATION {
            public long PerProcessUserTimeLimit, PerJobUserTimeLimit;
            public uint LimitFlags;
            public UIntPtr MinimumWorkingSetSize, MaximumWorkingSetSize;
            public uint ActiveProcessLimit;
            public UIntPtr Affinity;
            public uint PriorityClass, SchedulingClass;
        }
        [StructLayout(LayoutKind.Sequential)]
        private struct JOBOBJECT_EXTENDED_LIMIT_INFORMATION {
            public JOBOBJECT_BASIC_LIMIT_INFORMATION BasicLimitInformation;
            public IO_COUNTERS IoInfo;
            public UIntPtr ProcessMemoryLimit, JobMemoryLimit, PeakProcessMemoryUsed, PeakJobMemoryUsed;
        }
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern IntPtr CreateJobObject(IntPtr attributes, string name);
        [DllImport("kernel32.dll", SetLastError = true)] private static extern bool SetInformationJobObject(IntPtr job, int infoClass, IntPtr info, uint length);
        [DllImport("kernel32.dll", SetLastError = true)] private static extern bool AssignProcessToJobObject(IntPtr job, IntPtr process);
        [DllImport("kernel32.dll", SetLastError = true)] private static extern bool TerminateJobObject(IntPtr job, uint exitCode);
        [DllImport("kernel32.dll", SetLastError = true)] private static extern bool CloseHandle(IntPtr handle);
        private const uint JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE = 0x00002000;

        private static Task ReadAsync(StreamReader reader, BoundedCapture capture) {
            return Task.Run(async () => {
                char[] buffer = new char[1024];
                try {
                    while (!capture.Exceeded) {
                        int read = await reader.ReadAsync(buffer, 0, buffer.Length).ConfigureAwait(false);
                        if (read == 0) break;
                        capture.Append(buffer, read);
                    }
                } catch { }
            });
        }
        private static void ConfigureJob(IntPtr job) {
            var limits = new JOBOBJECT_EXTENDED_LIMIT_INFORMATION();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            int size = Marshal.SizeOf(typeof(JOBOBJECT_EXTENDED_LIMIT_INFORMATION));
            IntPtr memory = Marshal.AllocHGlobal(size);
            try {
                Marshal.StructureToPtr(limits, memory, false);
                if (!SetInformationJobObject(job, 9, memory, (uint)size)) throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not configure publisher UI helper job.");
            } finally { Marshal.FreeHGlobal(memory); }
        }
        private static bool StopJob(IntPtr job, Process process) {
            bool terminated = false;
            try { terminated = TerminateJobObject(job, 125); } catch { }
            try { if (!process.HasExited) process.Kill(true); } catch { }
            try { process.WaitForExit(1000); } catch { }
            return terminated;
        }
        public static BoundedProcessResult Run(string filePath, string[] arguments, string workingDirectory,
            string[] environmentPairs, int timeoutMilliseconds, int maximumStdoutCharacters, int maximumStderrCharacters) {
            if (timeoutMilliseconds <= 0 || maximumStdoutCharacters <= 0 || maximumStderrCharacters <= 0) throw new ArgumentOutOfRangeException("Publisher UI helper bounds must be positive.");
            if (environmentPairs == null || environmentPairs.Length % 2 != 0) throw new ArgumentException("Publisher UI helper environment is malformed.");
            var start = new ProcessStartInfo {
                FileName = filePath, WorkingDirectory = workingDirectory, UseShellExecute = false, CreateNoWindow = true,
                RedirectStandardOutput = true, RedirectStandardError = true,
                StandardOutputEncoding = new UTF8Encoding(false, true), StandardErrorEncoding = new UTF8Encoding(false, true)
            };
            start.Environment.Clear();
            for (int index = 0; index < environmentPairs.Length; index += 2) start.Environment[environmentPairs[index]] = environmentPairs[index + 1];
            foreach (string argument in arguments) start.ArgumentList.Add(argument);

            IntPtr job = CreateJobObject(IntPtr.Zero, null);
            if (job == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Could not create publisher UI helper job.");
            try {
                ConfigureJob(job);
                using (var process = new Process { StartInfo = start }) {
                    if (!process.Start()) throw new InvalidOperationException("Publisher UI helper did not start.");
                    int processId = process.Id;
                    long processStartTicks = process.StartTime.ToUniversalTime().Ticks;
                    bool assigned = AssignProcessToJobObject(job, process.Handle);
                    if (!assigned) { int error = Marshal.GetLastWin32Error(); StopJob(job, process); throw new Win32Exception(error, "Could not bind publisher UI helper to its job."); }
                    var stdout = new BoundedCapture(maximumStdoutCharacters);
                    var stderr = new BoundedCapture(maximumStderrCharacters);
                    Task stdoutTask = ReadAsync(process.StandardOutput, stdout);
                    Task stderrTask = ReadAsync(process.StandardError, stderr);
                    var stopwatch = Stopwatch.StartNew();
                    bool timedOut = false, terminated = false;
                    while (!process.HasExited) {
                        if (stdout.Exceeded || stderr.Exceeded) { terminated = StopJob(job, process); break; }
                        if (stopwatch.ElapsedMilliseconds >= timeoutMilliseconds) { timedOut = true; terminated = StopJob(job, process); break; }
                        Thread.Sleep(10);
                    }
                    if (!process.HasExited) terminated = StopJob(job, process) || terminated;
                    try { Task.WaitAll(new[] { stdoutTask, stderrTask }, 1000); } catch { }
                    return new BoundedProcessResult {
                        Stdout = stdout.Snapshot(), Stderr = stderr.Snapshot(), ExitCode = process.HasExited ? process.ExitCode : -1,
                        ElapsedMilliseconds = checked((int)Math.Min(stopwatch.ElapsedMilliseconds, Int32.MaxValue)), TimedOut = timedOut,
                        StdoutExceeded = stdout.Exceeded, StderrExceeded = stderr.Exceeded, JobAssigned = assigned, JobTerminated = terminated,
                        ProcessStopped = process.HasExited, ProcessId = processId, ProcessStartTimeUtcTicks = processStartTicks
                    };
                }
            } finally { CloseHandle(job); }
        }
    }

    public sealed class WindowIdentity {
        public long Handle { get; set; }
        public uint ProcessId { get; set; }
        public long ProcessStartTimeUtcTicks { get; set; }
        public long Owner { get; set; }
        public string Title { get; set; }
        public string ClassName { get; set; }
    }

    public static class NativeWindow {
        private delegate bool EnumWindowsProc(IntPtr hwnd, IntPtr state);
        [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr state);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr hwnd, StringBuilder value, int maximum);
        [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(IntPtr hwnd, StringBuilder value, int maximum);
        [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr hwnd, out uint processId);
        [DllImport("user32.dll")] private static extern IntPtr GetWindow(IntPtr hwnd, uint command);
        [DllImport("user32.dll")] private static extern bool IsWindow(IntPtr hwnd);
        [DllImport("user32.dll")] private static extern bool IsWindowVisible(IntPtr hwnd);
        [DllImport("user32.dll")] private static extern bool IsWindowEnabled(IntPtr hwnd);
        [DllImport("user32.dll", SetLastError = true)] private static extern bool PostMessage(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);
        private const uint GW_OWNER = 4, WM_CLOSE = 0x0010;

        private static WindowIdentity Capture(IntPtr hwnd) {
            if (!IsWindow(hwnd) || !IsWindowVisible(hwnd) || !IsWindowEnabled(hwnd)) return null;
            var title = new StringBuilder(512); var className = new StringBuilder(256);
            GetWindowText(hwnd, title, title.Capacity); GetClassName(hwnd, className, className.Capacity);
            uint processId; GetWindowThreadProcessId(hwnd, out processId);
            try {
                using (Process process = Process.GetProcessById((int)processId)) {
                    return new WindowIdentity { Handle = hwnd.ToInt64(), ProcessId = processId, ProcessStartTimeUtcTicks = process.StartTime.ToUniversalTime().Ticks,
                        Owner = GetWindow(hwnd, GW_OWNER).ToInt64(), Title = title.ToString(), ClassName = className.ToString() };
                }
            } catch { return null; }
        }
        public static WindowIdentity[] Exact(string title, string className) {
            var result = new List<WindowIdentity>();
            EnumWindows((hwnd, state) => { WindowIdentity item = Capture(hwnd);
                if (item != null && String.Equals(item.Title, title, StringComparison.Ordinal) && String.Equals(item.ClassName, className, StringComparison.Ordinal)) result.Add(item);
                return true; }, IntPtr.Zero);
            return result.ToArray();
        }
        public static bool StillMatches(WindowIdentity expected) {
            WindowIdentity current = Capture(new IntPtr(expected.Handle));
            return current != null && current.ProcessId == expected.ProcessId && current.ProcessStartTimeUtcTicks == expected.ProcessStartTimeUtcTicks && current.Owner == expected.Owner &&
                String.Equals(current.Title, expected.Title, StringComparison.Ordinal) && String.Equals(current.ClassName, expected.ClassName, StringComparison.Ordinal);
        }
        public static bool CloseExact(WindowIdentity expected) {
            if (!StillMatches(expected)) return false;
            return PostMessage(new IntPtr(expected.Handle), WM_CLOSE, IntPtr.Zero, IntPtr.Zero);
        }
    }
}
'@
}

function Assert-PublisherUiDeadline {
    param([Parameter(Mandatory = $true)][datetime]$Deadline)
    if ([datetime]::UtcNow -ge $Deadline) {
        Set-PublisherUiChildCategory -Category 'deadline'
        throw 'Windows publisher UI verification exceeded its total deadline.'
    }
}

function Get-PublisherUiCultureFacts {
    param([scriptblock]$CultureProvider)
    $facts = if ($CultureProvider) {
        & $CultureProvider
    } else {
        [pscustomobject]@{
            Culture = [Globalization.CultureInfo]::CurrentCulture.Name
            UICulture = [Globalization.CultureInfo]::CurrentUICulture.Name
        }
    }
    $properties = @($facts.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if ($properties.Count -ne 2 -or (Compare-Object @('Culture','UICulture') $properties -CaseSensitive)) {
        throw 'Windows publisher UI culture facts are malformed.'
    }
    if ([string]$facts.Culture -cne $script:PublisherUiPins.Culture -or [string]$facts.UICulture -cne $script:PublisherUiPins.Culture) {
        throw 'Windows publisher UI verification requires exact en-US culture and UI culture.'
    }
    return $facts
}

function Get-PublisherUiElementName {
    param([Parameter(Mandatory = $true)]$Element)
    $name = [string]$Element.Current.Name
    if ($name.Length -gt $script:PublisherUiPins.StringLengthMaximum) { throw 'Windows publisher UI exposed an oversized accessible name.' }
    return $name
}

function Get-PublisherUiExactElements {
    param(
        [Parameter(Mandatory = $true)]$Root,
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)]$ControlType,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [Windows.Automation.TreeScope]$Scope = [Windows.Automation.TreeScope]::Descendants
    )
    Assert-PublisherUiDeadline -Deadline $Deadline
    if ($Name.Length -gt $script:PublisherUiPins.StringLengthMaximum) { throw 'Windows publisher UI expected name exceeds its bound.' }
    $condition = [Windows.Automation.AndCondition]::new([Windows.Automation.Condition[]]@(
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$Name),
        [Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::ControlTypeProperty,$ControlType)
    ))
    $found = @($Root.FindAll($Scope,$condition) | ForEach-Object { $_ })
    Assert-PublisherUiDeadline -Deadline $Deadline
    if ($found.Count -gt $script:PublisherUiPins.ElementMaximum) { throw 'Windows publisher UI exact-element result exceeded its bound.' }
    foreach ($match in $found) { $null = Get-PublisherUiElementName -Element $match }
    return $found
}

function Get-PublisherUiExactWindows {
    param([Parameter(Mandatory = $true)][string]$Title,[Parameter(Mandatory = $true)][datetime]$Deadline)
    Initialize-PublisherUiInterop
    $windows = @(Get-PublisherUiExactElements -Root ([Windows.Automation.AutomationElement]::RootElement) -Name $Title -ControlType ([Windows.Automation.ControlType]::Window) -Scope ([Windows.Automation.TreeScope]::Children) -Deadline $Deadline)
    $found = @($windows | Where-Object {
        ([string]$_.Current.ClassName).Equals($script:PublisherUiPins.PropertyClass,[StringComparison]::Ordinal) -and
        [long]$_.Current.NativeWindowHandle -ne 0 -and [bool]$_.Current.IsEnabled -and -not [bool]$_.Current.IsOffscreen
    })
    if ($found.Count -gt $script:PublisherUiPins.ElementMaximum) { throw 'Windows publisher UI top-level result exceeded its bound.' }
    return $found
}

function Wait-PublisherUiExactWindow {
    param(
        [Parameter(Mandatory = $true)][string]$Title,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [long]$ExpectedOwner = 0
    )
    do {
        Assert-PublisherUiDeadline -Deadline $Deadline
        $found = @(Get-PublisherUiExactWindows -Title $Title -Deadline $Deadline)
        if ($ExpectedOwner -ne 0) {
            $found = @($found | Where-Object { [Smacrobat.PublisherUi.NativeShell]::OwnerOf([long]$_.Current.NativeWindowHandle) -eq $ExpectedOwner })
        }
        if ($found.Count -eq 1) { return $found[0] }
        if ($found.Count -gt 1) { throw 'Windows publisher UI top-level identity was ambiguous.' }
        Start-Sleep -Milliseconds $script:PublisherUiPins.PollMilliseconds
    } while ([datetime]::UtcNow -lt $Deadline)
    throw 'Windows publisher UI expected window did not appear before the deadline.'
}

function Get-PublisherUiExactOwnedWindows {
    param(
        [Parameter(Mandatory = $true)][string]$Title,
        [Parameter(Mandatory = $true)][long]$ExpectedOwner,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    Assert-PublisherUiDeadline -Deadline $Deadline
    $ownedHandles = @([Smacrobat.PublisherUi.NativeShell]::OwnedBy($ExpectedOwner))
    if ($ownedHandles.Count -gt $script:PublisherUiPins.ElementMaximum) { throw 'Windows publisher UI owned-window result exceeded its bound.' }
    return @($ownedHandles | ForEach-Object {
        $element = [Windows.Automation.AutomationElement]::FromHandle([IntPtr]$_)
        if ($null -ne $element -and
            [string]$element.Current.Name -ceq $Title -and
            [string]$element.Current.ClassName -ceq $script:PublisherUiPins.PropertyClass -and
            $element.Current.ControlType -eq [Windows.Automation.ControlType]::Window -and
            [long]$element.Current.NativeWindowHandle -ne 0 -and
            [bool]$element.Current.IsEnabled -and
            -not [bool]$element.Current.IsOffscreen) {
            $element
        }
    })
}

function Wait-PublisherUiExactOwnedWindow {
    param(
        [Parameter(Mandatory = $true)][string]$Title,
        [Parameter(Mandatory = $true)][long]$ExpectedOwner,
        [Parameter(Mandatory = $true)][datetime]$Deadline,
        [Threading.Tasks.Task]$InvokeTask
    )
    do {
        if ($null -ne $InvokeTask) {
            $invokeState = Get-PublisherUiInvokeTaskState -Task $InvokeTask
            if ($invokeState -ceq 'faulted') {
                Set-PublisherUiChildCategory -Category 'invoke-faulted'
                throw 'Windows publisher UI details invocation faulted.'
            }
            if ($invokeState -ceq 'canceled') {
                Set-PublisherUiChildCategory -Category 'invoke-canceled'
                throw 'Windows publisher UI details invocation was canceled.'
            }
        }
        if ([datetime]::UtcNow -ge $Deadline) { break }
        $found = @(Get-PublisherUiExactOwnedWindows -Title $Title -ExpectedOwner $ExpectedOwner -Deadline $Deadline)
        if ($found.Count -eq 1) { return $found[0] }
        if ($found.Count -gt 1) {
            Set-PublisherUiChildCategory -Category 'modal-ambiguous'
            throw 'Windows publisher UI owned-window identity was ambiguous.'
        }
        $globalMatches = @(Get-PublisherUiExactWindows -Title $Title -Deadline $Deadline)
        if ($globalMatches.Count -gt 1) {
            Set-PublisherUiChildCategory -Category 'modal-ambiguous'
            throw 'Windows publisher UI details-window identity was ambiguous.'
        }
        if ($globalMatches.Count -eq 1) {
            Set-PublisherUiChildCategory -Category 'modal-owner-mismatch'
            throw 'Windows publisher UI details-window owner did not match.'
        }
        if ([datetime]::UtcNow -ge $Deadline) { break }
        Start-Sleep -Milliseconds $script:PublisherUiPins.PollMilliseconds
    } while ([datetime]::UtcNow -lt $Deadline)
    if ($null -eq $InvokeTask) {
        Set-PublisherUiChildCategory -Category 'modal-missing'
    } else {
        $invokeState = Get-PublisherUiInvokeTaskState -Task $InvokeTask
        if ($invokeState -ceq 'faulted') { Set-PublisherUiChildCategory -Category 'invoke-faulted' }
        elseif ($invokeState -ceq 'canceled') { Set-PublisherUiChildCategory -Category 'invoke-canceled' }
        elseif ($invokeState -ceq 'completed') { Set-PublisherUiChildCategory -Category 'invoke-completed' }
        else { Set-PublisherUiChildCategory -Category 'invoke-running' }
    }
    throw 'Windows publisher UI expected owned window did not appear before the deadline.'
}

function Invoke-PublisherUiSelection {
    param([Parameter(Mandatory = $true)]$Element,[Parameter(Mandatory = $true)][datetime]$Deadline)
    Assert-PublisherUiDeadline -Deadline $Deadline
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern([Windows.Automation.SelectionItemPattern]::Pattern,[ref]$pattern)) {
        throw 'Windows publisher UI selection did not expose SelectionItemPattern.'
    }
    ([Windows.Automation.SelectionItemPattern]$pattern).Select()
    Assert-PublisherUiDeadline -Deadline $Deadline
}

function Invoke-PublisherUiButton {
    param(
        [Parameter(Mandatory = $true)][long]$RootHandle,
        [Parameter(Mandatory = $true)][string]$RootTitle,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    Assert-PublisherUiDeadline -Deadline $Deadline
    $task = [Smacrobat.PublisherUi.AutomationInvoke]::InvokeExactButtonAsync(
        $RootHandle,
        $RootTitle,
        $script:PublisherUiPins.PropertyClass,
        $script:PublisherUiPins.DetailsButton
    )
    if ($null -eq $task) { throw 'Windows publisher UI button invocation did not start.' }
    return $task
}

function Get-PublisherUiInvokeTaskState {
    param([Parameter(Mandatory = $true)][Threading.Tasks.Task]$Task)
    if ($Task.IsCanceled) { return 'canceled' }
    if ($Task.IsFaulted) { return 'faulted' }
    if ($Task.IsCompletedSuccessfully) { return 'completed' }
    return 'running'
}

function Complete-PublisherUiButtonInvoke {
    param(
        [Parameter(Mandatory = $true)][Threading.Tasks.Task]$Task,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    while (-not $Task.IsCompleted) {
        if ([datetime]::UtcNow -ge $Deadline) {
            Set-PublisherUiChildCategory -Category 'invoke-running'
            throw 'Windows publisher UI button invocation exceeded its deadline.'
        }
        Start-Sleep -Milliseconds $script:PublisherUiPins.PollMilliseconds
    }
    $state = Get-PublisherUiInvokeTaskState -Task $Task
    if ($state -ceq 'canceled') {
        Set-PublisherUiChildCategory -Category 'invoke-canceled'
        throw 'Windows publisher UI button invocation was canceled.'
    }
    if ($state -ceq 'faulted') {
        $null = $Task.Exception
        Set-PublisherUiChildCategory -Category 'invoke-faulted'
        throw 'Windows publisher UI button invocation failed.'
    }
    $Task.GetAwaiter().GetResult()
    Assert-PublisherUiDeadline -Deadline $Deadline
}

function Get-PublisherUiValue {
    param([Parameter(Mandatory = $true)]$Element,[Parameter(Mandatory = $true)][datetime]$Deadline)
    Assert-PublisherUiDeadline -Deadline $Deadline
    $pattern = $null
    if (-not $Element.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern,[ref]$pattern)) {
        throw 'Windows publisher UI value field did not expose ValuePattern.'
    }
    $value = [string]([Windows.Automation.ValuePattern]$pattern).Current.Value
    if ($value.Length -gt $script:PublisherUiPins.StringLengthMaximum) { throw 'Windows publisher UI value exceeded its bound.' }
    if ($value.EndsWith("`r",[StringComparison]::Ordinal)) { $value = $value.Substring(0,$value.Length - 1) }
    if ($value.Contains("`r") -or $value.Contains("`n")) { throw 'Windows publisher UI value was not one normalized line.' }
    Assert-PublisherUiDeadline -Deadline $Deadline
    return $value
}

function Close-PublisherUiWindow {
    param([long]$HandleValue,[Parameter(Mandatory = $true)][datetime]$Deadline)
    if ($HandleValue -eq 0) { return }
    Assert-PublisherUiDeadline -Deadline $Deadline
    [Smacrobat.PublisherUi.NativeShell]::CloseWindow($HandleValue)
    while ([Smacrobat.PublisherUi.NativeShell]::WindowExists($HandleValue)) {
        Assert-PublisherUiDeadline -Deadline $Deadline
        Start-Sleep -Milliseconds $script:PublisherUiPins.PollMilliseconds
    }
}

function Invoke-WindowsShellPublisherSurface {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher,
        [int]$TimeoutMilliseconds = $script:PublisherUiPins.DeadlineMilliseconds
    )
    if ($TimeoutMilliseconds -le 0 -or $TimeoutMilliseconds -gt $script:PublisherUiPins.DeadlineMilliseconds) {
        throw 'Windows publisher UI timeout is outside its supported bound.'
    }
    Initialize-PublisherUiInterop
    $totalDeadline = [datetime]::UtcNow.AddMilliseconds($TimeoutMilliseconds)
    $interactionDeadline = $totalDeadline.AddMilliseconds(-$script:PublisherUiPins.CleanupMilliseconds)
    if ($interactionDeadline -le [datetime]::UtcNow) { throw 'Windows publisher UI timeout leaves no cleanup budget.' }
    $fileName = [IO.Path]::GetFileName($Path)
    $propertiesTitle = "$fileName Properties"
    Set-PublisherUiChildStage -Stage 'fresh-surface'
    if (@(Get-PublisherUiExactWindows -Title $propertiesTitle -Deadline $interactionDeadline).Count -ne 0 -or
        @(Get-PublisherUiExactWindows -Title $script:PublisherUiPins.DetailsTitle -Deadline $interactionDeadline).Count -ne 0) {
        throw 'Windows publisher UI verification requires a fresh matching desktop surface.'
    }

    [long]$propertiesHandle = 0
    [long]$detailsHandle = 0
    [Threading.Tasks.Task]$detailsInvokeTask = $null
    $propertiesInvoked = $false
    $detailsInvoked = $false
    $surface = $null
    $primaryFailure = $null
    $primaryFailureStage = $null
    $primaryFailureCategory = $null
    try {
        Set-PublisherUiChildStage -Stage 'open-properties'
        [Smacrobat.PublisherUi.NativeShell]::ShowProperties($Path)
        $propertiesInvoked = $true
        Set-PublisherUiChildStage -Stage 'wait-properties'
        $properties = Wait-PublisherUiExactWindow -Title $propertiesTitle -Deadline $interactionDeadline
        $propertiesHandle = [long]$properties.Current.NativeWindowHandle

        Set-PublisherUiChildStage -Stage 'signature-tab'
        $tabs = @(Get-PublisherUiExactElements -Root $properties -Name $script:PublisherUiPins.SignatureTab -ControlType ([Windows.Automation.ControlType]::TabItem) -Deadline $interactionDeadline)
        if ($tabs.Count -ne 1) { throw 'Windows publisher UI did not expose one exact Digital Signatures tab.' }
        Invoke-PublisherUiSelection -Element $tabs[0] -Deadline $interactionDeadline

        Set-PublisherUiChildStage -Stage 'signature-grid'
        $signatureGrids = @(Get-PublisherUiExactElements -Root $properties -Name 'Embedded Signatures' -ControlType ([Windows.Automation.ControlType]::DataGrid) -Deadline $interactionDeadline)
        if ($signatureGrids.Count -ne 1) { throw 'Windows publisher UI did not expose one exact embedded-signatures grid.' }
        Set-PublisherUiChildStage -Stage 'signer-row'
        $signers = @(Get-PublisherUiExactElements -Root $signatureGrids[0] -Name $ExpectedPublisher -ControlType ([Windows.Automation.ControlType]::DataItem) -Deadline $interactionDeadline)
        if ($signers.Count -ne 1) { throw 'Windows publisher UI did not expose one exact expected embedded signer row.' }
        Invoke-PublisherUiSelection -Element $signers[0] -Deadline $interactionDeadline

        Set-PublisherUiChildStage -Stage 'details-invoke'
        $buttons = @(Get-PublisherUiExactElements -Root $properties -Name $script:PublisherUiPins.DetailsButton -ControlType ([Windows.Automation.ControlType]::Button) -Deadline $interactionDeadline | Where-Object { [bool]$_.Current.IsEnabled })
        if ($buttons.Count -ne 1) { throw 'Windows publisher UI did not expose one enabled embedded-signature Details action.' }
        $buttons[0].SetFocus()
        Assert-PublisherUiDeadline -Deadline $interactionDeadline
        $detailsInvokeTask = Invoke-PublisherUiButton -RootHandle $propertiesHandle -RootTitle $propertiesTitle -Deadline $interactionDeadline
        $detailsInvoked = $true

        Set-PublisherUiChildStage -Stage 'details-wait'
        $details = Wait-PublisherUiExactOwnedWindow -Title $script:PublisherUiPins.DetailsTitle -ExpectedOwner $propertiesHandle -Deadline $interactionDeadline -InvokeTask $detailsInvokeTask
        $detailsHandle = [long]$details.Current.NativeWindowHandle
        Set-PublisherUiChildStage -Stage 'details-status'
        $status = @(Get-PublisherUiExactElements -Root $details -Name $script:PublisherUiPins.ValidStatus -ControlType ([Windows.Automation.ControlType]::Text) -Deadline $interactionDeadline)
        if ($status.Count -ne 1) { throw 'Windows publisher UI did not expose the exact valid-signature status.' }
        Set-PublisherUiChildStage -Stage 'details-signer'
        $signerGroups = @(Get-PublisherUiExactElements -Root $details -Name 'Signer information' -ControlType ([Windows.Automation.ControlType]::Group) -Deadline $interactionDeadline)
        if ($signerGroups.Count -ne 1) { throw 'Windows publisher UI details did not expose one exact signer-information group.' }
        $publisherFields = @(Get-PublisherUiExactElements -Root $details -Name 'Name:' -ControlType ([Windows.Automation.ControlType]::Edit) -Deadline $interactionDeadline)
        if ($publisherFields.Count -ne 1 -or (Get-PublisherUiValue -Element $publisherFields[0] -Deadline $interactionDeadline) -cne $ExpectedPublisher) {
            throw 'Windows publisher UI details did not expose the exact expected signer value.'
        }

        $surface = [pscustomobject]@{
            uiCulture = $script:PublisherUiPins.Culture
            shellPropertiesDialog = $true
            digitalSignaturesTab = $true
            signerRowMatched = $true
            detailsDialog = $true
            statusTextMatched = $true
            cleanupVerified = $true
            screenshotsUsed = $false
        }
    } catch {
        $primaryFailure = $_
        $primaryFailureStage = $script:PublisherUiChildStage
        $primaryFailureCategory = $script:PublisherUiChildCategory
    } finally {
        try {
            Set-PublisherUiChildStage -Stage 'cleanup-details'
            Set-PublisherUiChildCategory -Category 'cleanup'
            if ($propertiesHandle -eq 0 -and $propertiesInvoked) {
                $propertiesCleanupMatches = @(Get-PublisherUiExactWindows -Title $propertiesTitle -Deadline $totalDeadline)
                if ($propertiesCleanupMatches.Count -gt 1) { throw 'Windows publisher UI cleanup found ambiguous file properties surfaces.' }
                if ($propertiesCleanupMatches.Count -eq 1) { $propertiesHandle = [long]$propertiesCleanupMatches[0].Current.NativeWindowHandle }
            }
            if ($detailsHandle -eq 0 -and $detailsInvoked -and $propertiesHandle -ne 0) {
                $detailsCleanupMatches = @(Get-PublisherUiExactOwnedWindows -Title $script:PublisherUiPins.DetailsTitle -ExpectedOwner $propertiesHandle -Deadline $totalDeadline)
                if ($detailsCleanupMatches.Count -gt 1) { throw 'Windows publisher UI cleanup found ambiguous signature-details surfaces.' }
                if ($detailsCleanupMatches.Count -eq 1) { $detailsHandle = [long]$detailsCleanupMatches[0].Current.NativeWindowHandle }
            }
            if ($detailsHandle -ne 0) { Close-PublisherUiWindow -HandleValue $detailsHandle -Deadline $totalDeadline }
            Set-PublisherUiChildStage -Stage 'invoke-complete'
            if ($null -ne $detailsInvokeTask) { Complete-PublisherUiButtonInvoke -Task $detailsInvokeTask -Deadline $totalDeadline }
            Set-PublisherUiChildStage -Stage 'cleanup-properties'
            Set-PublisherUiChildCategory -Category 'cleanup'
            if ($propertiesHandle -ne 0) { Close-PublisherUiWindow -HandleValue $propertiesHandle -Deadline $totalDeadline }
        } catch {
            if ($null -eq $primaryFailure) {
                $primaryFailure = $_
                $primaryFailureStage = $script:PublisherUiChildStage
                $primaryFailureCategory = $script:PublisherUiChildCategory
            }
        }
    }
    if ($null -ne $primaryFailure) {
        $script:PublisherUiChildStage = $primaryFailureStage
        $script:PublisherUiChildCategory = $primaryFailureCategory
        throw $primaryFailure
    }
    return $surface
}

function Get-PublisherUiMinimalEnvironment {
    $required = @('SystemRoot','SystemDrive','WINDIR','ProgramData','TEMP','TMP','USERPROFILE','LOCALAPPDATA','APPDATA','COMSPEC')
    $pairs = [Collections.Generic.List[string]]::new()
    foreach ($name in $required) {
        $value = [Environment]::GetEnvironmentVariable($name)
        if ([string]::IsNullOrWhiteSpace($value)) { throw "Windows publisher UI helper requires $name." }
        $pairs.Add($name)
        $pairs.Add($value)
    }
    $pairs.Add('Path')
    $pairs.Add("$PSHOME;$env:SystemRoot\System32;$env:SystemRoot")
    $pairs.Add('POWERSHELL_TELEMETRY_OPTOUT')
    $pairs.Add('1')
    $pairs.Add('DOTNET_CLI_TELEMETRY_OPTOUT')
    $pairs.Add('1')
    return $pairs.ToArray()
}

function Invoke-PublisherUiExactCleanup {
    param(
        [Parameter(Mandatory = $true)][string]$PropertiesTitle,
        [Parameter(Mandatory = $true)][datetime]$Deadline
    )
    foreach ($title in @($script:PublisherUiPins.DetailsTitle,$PropertiesTitle)) {
        $found = @([Smacrobat.PublisherUiIsolation.NativeWindow]::Exact($title,$script:PublisherUiPins.PropertyClass))
        if ($found.Count -gt 1) { throw 'Windows publisher UI cleanup topology was ambiguous.' }
        if ($found.Count -eq 1) {
            $identity = $found[0]
            if (-not [Smacrobat.PublisherUiIsolation.NativeWindow]::CloseExact($identity)) { throw 'Windows publisher UI cleanup identity changed.' }
            while ([Smacrobat.PublisherUiIsolation.NativeWindow]::StillMatches($identity)) {
                if ([datetime]::UtcNow -ge $Deadline) { throw 'Windows publisher UI cleanup exceeded its deadline.' }
                Start-Sleep -Milliseconds $script:PublisherUiPins.PollMilliseconds
            }
        }
    }
    if (@([Smacrobat.PublisherUiIsolation.NativeWindow]::Exact($PropertiesTitle,$script:PublisherUiPins.PropertyClass)).Count -ne 0 -or
        @([Smacrobat.PublisherUiIsolation.NativeWindow]::Exact($script:PublisherUiPins.DetailsTitle,$script:PublisherUiPins.PropertyClass)).Count -ne 0) {
        throw 'Windows publisher UI cleanup was incomplete.'
    }
}

function Assert-PublisherUiSurfaceProof {
    param([Parameter(Mandatory = $true)]$Surface)
    $expected = @('cleanupVerified','detailsDialog','digitalSignaturesTab','screenshotsUsed','shellPropertiesDialog','signerRowMatched','statusTextMatched','uiCulture') | Sort-Object -CaseSensitive
    $actual = @($Surface.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if ($actual.Count -ne $expected.Count -or (Compare-Object $expected $actual -CaseSensitive) -or
        [string]$Surface.uiCulture -cne $script:PublisherUiPins.Culture -or
        $Surface.shellPropertiesDialog -isnot [bool] -or -not $Surface.shellPropertiesDialog -or
        $Surface.digitalSignaturesTab -isnot [bool] -or -not $Surface.digitalSignaturesTab -or
        $Surface.signerRowMatched -isnot [bool] -or -not $Surface.signerRowMatched -or
        $Surface.detailsDialog -isnot [bool] -or -not $Surface.detailsDialog -or
        $Surface.statusTextMatched -isnot [bool] -or -not $Surface.statusTextMatched -or
        $Surface.cleanupVerified -isnot [bool] -or -not $Surface.cleanupVerified -or
        $Surface.screenshotsUsed -isnot [bool] -or $Surface.screenshotsUsed) {
        throw 'Windows publisher UI surface proof is incomplete or malformed.'
    }
}

function ConvertFrom-PublisherUiChildFailureToken {
    param([Parameter(Mandatory = $true)][string]$Token)
    if ([string]::IsNullOrEmpty($Token) -or $Token.Length -gt 160 -or $Token -cne $Token.Trim() -or
        $Token.Contains("`r") -or $Token.Contains("`n")) {
        throw 'Windows publisher UI child failure token framing is invalid.'
    }
    $parts = @($Token.Split(':'))
    if ($parts.Count -ne 4 -or $parts[0] -cne 'publisher-ui-child-failed' -or $parts[1] -cne 'v1' -or
        $script:PublisherUiPins.FailureStages -cnotcontains $parts[2] -or
        $script:PublisherUiPins.FailureCategories -cnotcontains $parts[3] -or
        $Token -cne (New-PublisherUiChildFailureToken -Stage $parts[2] -Category $parts[3])) {
        throw 'Windows publisher UI child failure token is invalid.'
    }
    return [pscustomobject][ordered]@{ stage = $parts[2]; category = $parts[3] }
}

function ConvertFrom-PublisherUiHelperOutput {
    param([Parameter(Mandatory = $true)]$Result)
    $expectedResult = @(
        'ElapsedMilliseconds','ExitCode','JobAssigned','JobTerminated','ProcessId','ProcessStartTimeUtcTicks',
        'ProcessStopped','Stderr','StderrExceeded','Stdout','StdoutExceeded','TimedOut'
    ) | Sort-Object -CaseSensitive
    $actualResult = @($Result.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if ($actualResult.Count -ne $expectedResult.Count -or (Compare-Object $expectedResult $actualResult -CaseSensitive) -or
        $Result.TimedOut -isnot [bool] -or $Result.StdoutExceeded -isnot [bool] -or $Result.StderrExceeded -isnot [bool] -or
        $Result.JobAssigned -isnot [bool] -or $Result.JobTerminated -isnot [bool] -or $Result.ProcessStopped -isnot [bool]) {
        throw 'Windows publisher UI helper result is malformed.'
    }
    if ($Result.TimedOut) { throw 'Windows publisher UI helper exceeded its bounded deadline.' }
    $boundaryUnsafe = $Result.StdoutExceeded -or $Result.StderrExceeded -or -not $Result.JobAssigned -or -not $Result.ProcessStopped
    $childFailure = $null
    if (-not $boundaryUnsafe -and [int]$Result.ExitCode -eq 1 -and [string]::IsNullOrEmpty([string]$Result.Stdout)) {
        try { $childFailure = ConvertFrom-PublisherUiChildFailureToken -Token ([string]$Result.Stderr) } catch { }
    }
    if ($null -ne $childFailure) {
        throw "Windows publisher UI helper failed at fixed stage '$($childFailure.stage)' with category '$($childFailure.category)'."
    }
    if ($boundaryUnsafe -or [int]$Result.ExitCode -ne 0 -or -not [string]::IsNullOrEmpty([string]$Result.Stderr)) {
        throw 'Windows publisher UI helper process did not complete safely.'
    }
    $stdout = [string]$Result.Stdout
    if ([string]::IsNullOrEmpty($stdout) -or $stdout.Length -gt $script:PublisherUiPins.StdoutMaximum -or
        $stdout -cne $stdout.Trim() -or $stdout.Contains("`r") -or $stdout.Contains("`n") -or
        -not $stdout.StartsWith('{',[StringComparison]::Ordinal) -or -not $stdout.EndsWith('}',[StringComparison]::Ordinal)) {
        throw 'Windows publisher UI helper output framing is invalid.'
    }
    try { $surface = $stdout | ConvertFrom-Json -Depth 4 } catch { throw 'Windows publisher UI helper output is not one JSON object.' }
    Assert-PublisherUiSurfaceProof -Surface $surface
    return $surface
}

function Invoke-PublisherUiHelperProcess {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher
    )
    Initialize-PublisherUiIsolation
    $totalDeadline = [datetime]::UtcNow.AddMilliseconds($script:PublisherUiPins.DeadlineMilliseconds)
    $propertiesTitle = "$(Split-Path -Leaf $Path) Properties"
    if (@([Smacrobat.PublisherUiIsolation.NativeWindow]::Exact($propertiesTitle,$script:PublisherUiPins.PropertyClass)).Count -ne 0 -or
        @([Smacrobat.PublisherUiIsolation.NativeWindow]::Exact($script:PublisherUiPins.DetailsTitle,$script:PublisherUiPins.PropertyClass)).Count -ne 0) {
        throw 'Windows publisher UI verification requires a fresh exact desktop surface.'
    }
    $payloadJson = [pscustomobject][ordered]@{
        path = $Path
        expectedPublisher = $ExpectedPublisher
        timeoutMilliseconds = $script:PublisherUiPins.ChildDeadlineMilliseconds
    } | ConvertTo-Json -Compress
    $payload = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($payloadJson))
    $arguments = @(
        '-NoLogo','-NoProfile','-NonInteractive','-Sta','-File',$script:PublisherUiScriptPath,
        '-PublisherUiChildPayload',$payload
    )
    $result = $null
    try {
        $result = [Smacrobat.PublisherUiIsolation.BoundedProcess]::Run(
            (Join-Path $PSHOME 'pwsh.exe'),
            $arguments,
            (Split-Path -Parent $script:PublisherUiScriptPath),
            (Get-PublisherUiMinimalEnvironment),
            $script:PublisherUiPins.InteractionMilliseconds,
            $script:PublisherUiPins.StdoutMaximum,
            $script:PublisherUiPins.StderrMaximum
        )
    } finally {
        Invoke-PublisherUiExactCleanup -PropertiesTitle $propertiesTitle -Deadline $totalDeadline
    }
    return ConvertFrom-PublisherUiHelperOutput -Result $result
}

function Get-PublisherUiSha256 {
    param([Parameter(Mandatory = $true)][string]$Path)
    $stream = [IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    try {
        $algorithm = [Security.Cryptography.SHA256]::Create()
        try { return ([BitConverter]::ToString($algorithm.ComputeHash($stream))).Replace('-','') }
        finally { $algorithm.Dispose() }
    } finally { $stream.Dispose() }
}

function Assert-PublisherUiFileReceipt {
    param([Parameter(Mandatory = $true)][string]$Path,[Parameter(Mandatory = $true)]$Receipt)
    $properties = @($Receipt.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if ($properties.Count -ne 2 -or (Compare-Object @('bytes','sha256') $properties -CaseSensitive) -or
        [uint64]$Receipt.bytes -eq 0 -or [string]$Receipt.sha256 -cnotmatch '^[A-F0-9]{64}$') {
        throw 'Windows publisher UI file receipt is malformed.'
    }
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf) -or [IO.Path]::GetExtension($Path) -cne '.exe') {
        throw 'Windows publisher UI input must be one existing executable file.'
    }
    if (Get-Command Assert-NoReparseAncestors -ErrorAction SilentlyContinue) { Assert-NoReparseAncestors -Path $Path }
    $item = Get-Item -LiteralPath $Path
    if ([uint64]$item.Length -ne [uint64]$Receipt.bytes -or (Get-PublisherUiSha256 -Path $Path) -cne [string]$Receipt.sha256) {
        throw 'Windows publisher UI input does not match its exact receipt.'
    }
}

function Invoke-InstalledPublisherUiProof {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)]$Receipt,
        [Parameter(Mandatory = $true)][ValidateSet('installer','installed-application')][string]$Kind,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher,
        [scriptblock]$SignatureProvider,
        [scriptblock]$CultureProvider
    )
    if ([string]::IsNullOrWhiteSpace($ExpectedPublisher) -or $ExpectedPublisher -cnotmatch "^[A-Za-z0-9][A-Za-z0-9 .,&'()/-]{0,127}$") { throw 'Windows publisher UI expected publisher is invalid.' }
    $canonical = [IO.Path]::GetFullPath($Path)
    $null = Get-PublisherUiCultureFacts -CultureProvider $CultureProvider
    Assert-PublisherUiFileReceipt -Path $canonical -Receipt $Receipt
    if (-not (Get-Command Assert-TrustedWindowsSignature -ErrorAction SilentlyContinue)) { throw 'Windows signature verifier is unavailable.' }
    $null = Assert-TrustedWindowsSignature -Path $canonical -SignatureProvider $SignatureProvider -ExpectedPublisher $ExpectedPublisher

    $lock = [IO.File]::Open($canonical,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    try {
        $surface = Invoke-PublisherUiHelperProcess -Path $canonical -ExpectedPublisher $ExpectedPublisher
        Assert-PublisherUiSurfaceProof -Surface $surface
    } finally {
        $lock.Dispose()
    }

    Assert-PublisherUiFileReceipt -Path $canonical -Receipt $Receipt
    $null = Assert-TrustedWindowsSignature -Path $canonical -SignatureProvider $SignatureProvider -ExpectedPublisher $ExpectedPublisher
    return [pscustomobject][ordered]@{
        kind = $Kind
        fileName = [IO.Path]::GetFileName($canonical)
        bytes = [uint64]$Receipt.bytes
        sha256 = [string]$Receipt.sha256
        publisher = $ExpectedPublisher
        uiCulture = $script:PublisherUiPins.Culture
        shellPropertiesDialog = $true
        digitalSignaturesTab = $true
        signerRowMatched = $true
        detailsDialog = $true
        statusTextMatched = $true
        trustedTimestampVerified = $true
        cleanupVerified = $true
        screenshotsUsed = $false
    }
}

function New-HostedPublisherUiNotAttemptedEvidence {
    param(
        [Parameter(Mandatory = $true)][string]$Path,
        [Parameter(Mandatory = $true)]$Receipt,
        [Parameter(Mandatory = $true)][ValidateSet('installer','installed-application')][string]$Kind,
        [Parameter(Mandatory = $true)][string]$ExpectedPublisher,
        [scriptblock]$SignatureProvider
    )
    $canonical = [IO.Path]::GetFullPath($Path)
    Assert-PublisherUiFileReceipt -Path $canonical -Receipt $Receipt
    $null = Assert-TrustedWindowsSignature -Path $canonical -SignatureProvider $SignatureProvider -ExpectedPublisher $ExpectedPublisher
    Assert-PublisherUiFileReceipt -Path $canonical -Receipt $Receipt
    $null = Assert-TrustedWindowsSignature -Path $canonical -SignatureProvider $SignatureProvider -ExpectedPublisher $ExpectedPublisher
    return [pscustomobject][ordered]@{
        kind = $Kind
        fileName = [IO.Path]::GetFileName($canonical)
        bytes = [uint64]$Receipt.bytes
        sha256 = [string]$Receipt.sha256
        publisher = $ExpectedPublisher
        authenticodeStatus = 'Valid'
        trustedTimestampVerified = $true
        shellPublisherUiAttempted = $false
        shellPublisherUiVerified = $false
        notAttemptedReason = 'github-hosted-noninteractive-session'
        screenshotsUsed = $false
    }
}

function Invoke-PublisherUiChild {
    param([Parameter(Mandatory = $true)][string]$Payload)
    Set-PublisherUiChildStage -Stage 'payload'
    $decoded = [Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($Payload)) | ConvertFrom-Json -Depth 3
    $properties = @($decoded.PSObject.Properties.Name | Sort-Object -CaseSensitive)
    if ($properties.Count -ne 3 -or (Compare-Object @('expectedPublisher','path','timeoutMilliseconds') $properties -CaseSensitive) -or
        [string]::IsNullOrWhiteSpace([string]$decoded.path) -or
        [string]$decoded.expectedPublisher -cnotmatch "^[A-Za-z0-9][A-Za-z0-9 .,&'()/-]{0,127}$" -or
        [int]$decoded.timeoutMilliseconds -ne $script:PublisherUiPins.ChildDeadlineMilliseconds) {
        throw 'Windows publisher UI child payload is malformed.'
    }
    Set-PublisherUiChildStage -Stage 'culture'
    $null = Get-PublisherUiCultureFacts
    return Invoke-WindowsShellPublisherSurface -Path ([IO.Path]::GetFullPath([string]$decoded.path)) `
        -ExpectedPublisher ([string]$decoded.expectedPublisher) -TimeoutMilliseconds ([int]$decoded.timeoutMilliseconds)
}

if (-not [string]::IsNullOrWhiteSpace($PublisherUiChildPayload)) {
    try {
        $ProgressPreference = 'SilentlyContinue'
        $WarningPreference = 'SilentlyContinue'
        $InformationPreference = 'SilentlyContinue'
        $childProof = Invoke-PublisherUiChild -Payload $PublisherUiChildPayload
        [Console]::Out.Write(($childProof | ConvertTo-Json -Compress -Depth 3))
        exit 0
    } catch {
        $failureToken = "$($script:PublisherUiPins.FailureTokenPrefix):payload:unexpected"
        try { $failureToken = New-PublisherUiChildFailureToken -Stage $script:PublisherUiChildStage -Category $script:PublisherUiChildCategory } catch { }
        [Console]::Error.Write($failureToken)
        exit 1
    }
}
