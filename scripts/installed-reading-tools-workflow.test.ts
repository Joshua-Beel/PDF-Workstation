import { describe, expect, it } from 'vitest';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

function normalizeNewlines(value: string) {
  return value.replace(/\r\n?/g, '\n');
}

function runPowerShell(source: string) {
  const root = mkdtempSync(join(tmpdir(), 'smacrobat-reading-tools-'));
  const path = join(root, 'run.ps1');
  try {
    writeFileSync(path, source, 'utf8');
    return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path], {
      encoding: 'utf8',
      timeout: 30_000,
      env: { ...process.env, SMACROBAT_TEST_ROOT: root },
    });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function extractFunctions(path: string, names: string[], body: string) {
  return String.raw`
    $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
    $tokens=$null;$errors=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\\${path.replaceAll('/', '\\')}',[ref]$tokens,[ref]$errors)
    if($errors.Count){throw 'Script did not parse.'}
    foreach($name in @(${names.map(name => `'${name}'`).join(',')})){
      $fn=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true)
      if(-not$fn){throw ('Missing function: '+$name)}
      Invoke-Expression $fn.Extent.Text
    }
    ${body}
  `;
}

describe('installed signed reading-tools verifier', () => {
  it('normalizes LF and Windows CRLF before exact multiline source assertions', () => {
    expect(normalizeNewlines('first\nsecond')).toBe('first\nsecond');
    expect(normalizeNewlines('first\r\nsecond\r\n')).toBe('first\nsecond\n');
  }, 30_000);

  it('parses and keeps all native interaction process-bound without coordinates or global SendKeys', () => {
    const source = readFileSync('scripts/installed-reading-tools.ps1', 'utf8');
    expect(source).toContain('EnumWindows(EnumWindowsProc callback');
    expect(source).toContain('EnumChildWindows(IntPtr parent');
    expect(source).toContain('GetWindowThreadProcessId');
    expect(source).toContain('GetDlgCtrlID');
    expect(source).toContain('SendMessageTimeoutText');
    expect(source).toContain('SendMessageTimeout($Handle,0x00F5');
    expect(source).toContain('Wait-ReadingProcessUiSurfaceClosed');
    expect(source).toContain('Wait-ReadingProcessBoundPickerTargets');
    expect(source).toContain('baselineTargetIdentities');
    expect(source).toContain("$className -ceq 'Edit' -and $controlId -in @(1001,1148)");
    expect(source).toContain("$className -ceq 'Button' -and $controlId -eq 1");
    expect(source).not.toMatch(/TreeScope\]::Descendants|\.FindAll\s*\(/);
    expect(source).not.toContain('UIAutomationClient');
    expect(source.indexOf('$baseline = @(Get-ReadingProcessUiSurfaceSnapshot')).toBeLessThan(source.indexOf('$baselineTargets = @(Get-ReadingPickerTargetSnapshot'));
    expect(source.indexOf('$baselineTargets = @(Get-ReadingPickerTargetSnapshot')).toBeLessThan(source.indexOf('$clicked = Invoke-WebDriverScript'));
    expect(source).not.toMatch(/SendKeys|mouse_event|SetCursorPos|click_input|screenX|screenY|__TAURI_INTERNALS__/i);
    const nativeApi = runPowerShell(extractFunctions('scripts/installed-reading-tools.ps1', ['Initialize-ReadingNativePickerApi'], String.raw`
      $tokens=$null;$errors=$null
      [Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\scripts\installed-reading-tools.ps1',[ref]$tokens,[ref]$errors)|Out-Null
      if($errors.Count){$errors|ForEach-Object ToString;exit 1}
      Initialize-ReadingNativePickerApi
      if($null-eq('ReadingNativePickerApi'-as[type])){throw 'The native picker API did not compile.'}
    `));
    expect(nativeApi.status, nativeApi.stderr || nativeApi.stdout).toBe(0);
  }, 30_000);

  it('binds a new target pair inside one of two stable owned surfaces and tracks stable close', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Assert-ReadingApplicationProcessIdentity', 'Assert-ReadingSurfaceSnapshot', 'Assert-ReadingPickerTargetSnapshot', 'Wait-ReadingProcessBoundPickerTargets', 'Assert-ReadingSurfaceBinding', 'Wait-ReadingProcessUiSurfaceClosed'], String.raw`
      $script:ReadingPins=[ordered]@{UiAutomationPollMilliseconds=1}
      $ticks=638500000000000000;$identity={param($id,$expected,$deadline)[pscustomobject]@{processId=$id;startUtcTicks=$expected}}
      function New-Surface([string]$identity,[int]$processId){[pscustomobject]@{runtimeIdentity=$identity;controlType='Hwnd';isEnabled=$true;isOffscreen=$false;processId=$processId;handle=[IntPtr][int64]$identity}}
      function New-Target([string]$identity,[string]$surface,[string]$kind,[int]$processId){$category=if($kind-ceq'filename'){'edit-1001'}else{'button-idok'};[pscustomobject]@{runtimeIdentity=$identity;surfaceRuntimeIdentity=$surface;targetKind=$kind;nativeControlCategory=$category;isEnabled=$true;isOffscreen=$false;processId=$processId;handle=[IntPtr][int64]$identity}}
      $window=New-Surface '101' 42;$pane=New-Surface '102' 42;$baseline=@($window,$pane)
      $baselineControl=New-Target '109' '101' 'open' 42;$baselineTargets=@($baselineControl)
      $filename=New-Target '120' '102' 'filename' 42;$open=New-Target '121' '102' 'open' 42
      $script:now=[datetime]::UtcNow;$clock={$script:now};$sleep={param($milliseconds)$script:now=$script:now.AddMilliseconds($milliseconds)}
      $bound=Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces $baseline -BaselineTargets $baselineTargets -Deadline $script:now.AddSeconds(1) -SurfaceSnapshotProvider { $baseline } -TargetSnapshotProvider { @($baselineControl,$filename,$open) } -ProcessIdentityProvider $identity -SleepProvider $sleep -UtcNowProvider $clock
      if($bound.surfaceRuntimeIdentity-cne'102'-or[bool]$bound.dedicatedNewSurface-or$bound.filenameRuntimeIdentity-cne'120'-or$bound.filenameControlCategory-cne'edit-1001'-or$bound.openButtonRuntimeIdentity-cne'121'){throw 'Stable-surface picker targets were not bound exactly.'}
      $movedFilename=New-Target '120' '101' 'filename' 42;$movedOpen=New-Target '121' '101' 'open' 42;$movedRejected=$false
      try{Wait-ReadingProcessUiSurfaceClosed -Binding $bound -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -Deadline $script:now.AddSeconds(1) -SurfaceSnapshotProvider { $baseline } -TargetSnapshotProvider { @($baselineControl,$movedFilename,$movedOpen) } -ProcessIdentityProvider $identity -SleepProvider $sleep -UtcNowProvider $clock}catch{$movedRejected=$_.Exception.Message-ceq'A bound native picker target moved or changed kind before closing.'}
      if(-not$movedRejected){throw 'Bound target identities moved to another baseline surface were accepted as closed.'}
      Wait-ReadingProcessUiSurfaceClosed -Binding $bound -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -Deadline $script:now.AddSeconds(1) -SurfaceSnapshotProvider { $baseline } -TargetSnapshotProvider { @($baselineControl) } -ProcessIdentityProvider $identity -SleepProvider $sleep -UtcNowProvider $clock
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('rejects baseline, ambiguous, cross-surface, sibling-surface, and replaced picker observations', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Assert-ReadingApplicationProcessIdentity', 'Assert-ReadingSurfaceSnapshot', 'Assert-ReadingPickerTargetSnapshot', 'Wait-ReadingProcessBoundPickerTargets'], String.raw`
      $script:ReadingPins=[ordered]@{UiAutomationPollMilliseconds=1}
      $ticks=638500000000000000;$identity={param($id,$expected,$deadline)[pscustomobject]@{processId=$id;startUtcTicks=$expected}}
      function New-Surface([string]$identity,[int]$processId){[pscustomobject]@{runtimeIdentity=$identity;controlType='Hwnd';isEnabled=$true;isOffscreen=$false;processId=$processId;handle=[IntPtr][int64]$identity}}
      function New-Target([string]$identity,[string]$surface,[string]$kind,[int]$processId){$category=if($kind-ceq'filename'){'edit-1001'}else{'button-idok'};[pscustomobject]@{runtimeIdentity=$identity;surfaceRuntimeIdentity=$surface;targetKind=$kind;nativeControlCategory=$category;isEnabled=$true;isOffscreen=$false;processId=$processId;handle=[IntPtr][int64]$identity}}
      $a=New-Surface '101' 42;$b=New-Surface '102' 42;$c=New-Surface '103' 42;$baseline=@($a,$b)
      $filename=New-Target '120' '102' 'filename' 42;$open=New-Target '121' '102' 'open' 42;$otherFilename=New-Target '122' '102' 'filename' 42;$crossOpen=New-Target '123' '101' 'open' 42
      $script:now=[datetime]::UtcNow;$clock={$script:now};$sleep={param($milliseconds)$script:now=$script:now.AddSeconds(10)};$deadline=$script:now.AddSeconds(5)
      $baselineRejected=$false;try{Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces $baseline -BaselineTargets @($filename,$open) -Deadline $deadline -SurfaceSnapshotProvider { $baseline } -TargetSnapshotProvider { @($filename,$open) } -ProcessIdentityProvider $identity -SleepProvider $sleep -UtcNowProvider $clock}catch{$baselineRejected=$_.Exception.Message-like'No single new process-bound native picker target pair appeared*'}
      if(-not$baselineRejected){throw 'Pre-click target controls were rebound as a picker.'}
      $script:now=[datetime]::UtcNow;$deadline=$script:now.AddSeconds(1);$ambiguous=$false;try{Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces $baseline -BaselineTargets @() -Deadline $deadline -SurfaceSnapshotProvider { $baseline } -TargetSnapshotProvider { @($filename,$otherFilename,$open) } -ProcessIdentityProvider $identity -SleepProvider $sleep -UtcNowProvider $clock}catch{$ambiguous=$_.Exception.Message-ceq'The new native picker targets were ambiguous.'}
      if(-not$ambiguous){throw 'Ambiguous picker targets were accepted.'}
      $cross=$false;try{Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces $baseline -BaselineTargets @() -Deadline $deadline -SurfaceSnapshotProvider { $baseline } -TargetSnapshotProvider { @($filename,$crossOpen) } -ProcessIdentityProvider $identity -SleepProvider $sleep -UtcNowProvider $clock}catch{$cross=$_.Exception.Message-ceq'The new native picker targets appeared on different owned surfaces.'}
      if(-not$cross){throw 'Cross-surface picker targets were accepted.'}
      $sibling=$false;try{Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces $baseline -BaselineTargets @() -Deadline $deadline -SurfaceSnapshotProvider { @($baseline+$c) } -TargetSnapshotProvider { @($filename,$open) } -ProcessIdentityProvider $identity -SleepProvider $sleep -UtcNowProvider $clock}catch{$sibling=$_.Exception.Message-ceq'A sibling native surface appeared outside the bound picker target surface.'}
      if(-not$sibling){throw 'A sibling new surface outside the target pair was accepted.'}
      $replaced=$false;try{Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces $baseline -BaselineTargets @() -Deadline $deadline -SurfaceSnapshotProvider { @($b) } -TargetSnapshotProvider { @($filename,$open) } -ProcessIdentityProvider $identity -SleepProvider $sleep -UtcNowProvider $clock}catch{$replaced=$_.Exception.Message-ceq'The pre-click top-level UI baseline was replaced while opening the native picker.'}
      if(-not$replaced){throw 'Baseline surface replacement was accepted.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('fails closed when bounded HWND target enumeration reaches the deadline and keeps a dedicated surface until it closes', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Assert-ReadingApplicationProcessIdentity', 'Assert-ReadingSurfaceSnapshot', 'Assert-ReadingPickerTargetSnapshot', 'Wait-ReadingProcessBoundPickerTargets', 'Assert-ReadingSurfaceBinding', 'Wait-ReadingProcessUiSurfaceClosed'], String.raw`
      $script:ReadingPins=[ordered]@{UiAutomationPollMilliseconds=1}
      $ticks=638500000000000000;$identity={param($id,$expected,$deadline)[pscustomobject]@{processId=$id;startUtcTicks=$expected}}
      function New-Surface([string]$identity,[int]$processId){[pscustomobject]@{runtimeIdentity=$identity;controlType='Hwnd';isEnabled=$true;isOffscreen=$false;processId=$processId;handle=[IntPtr][int64]$identity}}
      function New-Target([string]$identity,[string]$surface,[string]$kind,[int]$processId){$category=if($kind-ceq'filename'){'edit-1001'}else{'button-idok'};[pscustomobject]@{runtimeIdentity=$identity;surfaceRuntimeIdentity=$surface;targetKind=$kind;nativeControlCategory=$category;isEnabled=$true;isOffscreen=$false;processId=$processId;handle=[IntPtr][int64]$identity}}
      $base=New-Surface '101' 42;$dedicated=New-Surface '102' 42;$filename=New-Target '120' '102' 'filename' 42;$open=New-Target '121' '102' 'open' 42
      $script:now=[datetime]::UtcNow;$deadline=$script:now.AddSeconds(5);$clock={$script:now};$probes=0
      $late=$false;try{Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces @($base) -BaselineTargets @() -Deadline $deadline -SurfaceSnapshotProvider { @($base,$dedicated) } -TargetSnapshotProvider { $script:probes++;$script:now=$deadline;@($filename,$open) } -ProcessIdentityProvider $identity -SleepProvider { throw 'Late read reached sleep.' } -UtcNowProvider $clock}catch{$late=$_.Exception.Message-like'No single new process-bound native picker target pair appeared*'}
      if(-not$late-or$probes-ne1){throw 'A post-deadline target snapshot was accepted.'}
      $script:now=[datetime]::UtcNow;$deadline=$script:now.AddSeconds(1)
      $bound=Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces @($base) -BaselineTargets @() -Deadline $deadline -SurfaceSnapshotProvider { @($base,$dedicated) } -TargetSnapshotProvider { @($filename,$open) } -ProcessIdentityProvider $identity -SleepProvider { } -UtcNowProvider $clock
      $script:closeProbe=0
      Wait-ReadingProcessUiSurfaceClosed -Binding $bound -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -Deadline $deadline -SurfaceSnapshotProvider { $script:closeProbe++;if($script:closeProbe-eq1){@($base,$dedicated)}else{@($base)} } -TargetSnapshotProvider { @() } -ProcessIdentityProvider $identity -SleepProvider { } -UtcNowProvider $clock
      if($closeProbe-ne2){throw 'Dedicated surface close accepted mere target disappearance.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('does not send native picker mutations after the deadline and sends timely mutations exactly once', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Set-ReadingNativePickerValueBeforeDeadline', 'Invoke-ReadingNativePickerButtonBeforeDeadline'], String.raw`
      $script:valueWrites=0;$script:buttonInvokes=0
      $script:now=[datetime]'2026-01-01T00:00:00Z';$deadline=$script:now;$clock={$script:now}
      $valueRejected=$false
      try{Set-ReadingNativePickerValueBeforeDeadline -Handle ([IntPtr]120) -Value 'fixture.pdf' -Deadline $deadline -UtcNowProvider $clock -MessageProvider { $script:valueWrites++;[pscustomobject]@{delivered=$true;result=[IntPtr]1} }}catch{$valueRejected=$_.Exception.Message-ceq'The process-bound filename mutation deadline expired.'}
      if(-not$valueRejected-or$valueWrites-ne0){throw 'Filename mutation was sent after the deadline.'}
      $invokeRejected=$false
      try{Invoke-ReadingNativePickerButtonBeforeDeadline -Handle ([IntPtr]121) -Deadline $deadline -UtcNowProvider $clock -MessageProvider { $script:buttonInvokes++;[pscustomobject]@{delivered=$true;result=[IntPtr]0} }}catch{$invokeRejected=$_.Exception.Message-ceq'The process-bound native Open mutation deadline expired.'}
      if(-not$invokeRejected-or$buttonInvokes-ne0){throw 'Open mutation was sent after the deadline.'}
      $script:now=[datetime]'2026-01-01T00:00:00Z';$deadline=$script:now.AddSeconds(1)
      Set-ReadingNativePickerValueBeforeDeadline -Handle ([IntPtr]120) -Value 'fixture.pdf' -Deadline $deadline -UtcNowProvider $clock -MessageProvider { param($handle,$value,$timeout)$script:valueWrites++;[pscustomobject]@{delivered=$true;result=[IntPtr]1} }
      Invoke-ReadingNativePickerButtonBeforeDeadline -Handle ([IntPtr]121) -Deadline $deadline -UtcNowProvider $clock -MessageProvider { param($handle,$timeout)$script:buttonInvokes++;[pscustomobject]@{delivered=$true;result=[IntPtr]0} }
      if($valueWrites-ne1-or$buttonInvokes-ne1){throw 'Timely native picker mutations did not execute exactly once.'}
      $writeRejected=$false
      try{
        Set-ReadingNativePickerValueBeforeDeadline -Handle ([IntPtr]120) -Value 'fixture.pdf' -Deadline $deadline -UtcNowProvider $clock -MessageProvider { $script:valueWrites++;[pscustomobject]@{delivered=$true;result=[IntPtr]0} }
        Invoke-ReadingNativePickerButtonBeforeDeadline -Handle ([IntPtr]121) -Deadline $deadline -UtcNowProvider $clock -MessageProvider { $script:buttonInvokes++;[pscustomobject]@{delivered=$true;result=[IntPtr]0} }
      }catch{$writeRejected=$_.Exception.Message-ceq'The process-bound filename HWND reported that WM_SETTEXT did not set the value.'}
      if(-not$writeRejected-or$valueWrites-ne2-or$buttonInvokes-ne1){throw 'A delivered but rejected filename write reached the Open action.'}
      $clickRejected=$false
      try{Invoke-ReadingNativePickerButtonBeforeDeadline -Handle ([IntPtr]121) -Deadline $deadline -UtcNowProvider $clock -MessageProvider { [pscustomobject]@{delivered=$false;result=[IntPtr]1} }}catch{$clickRejected=$_.Exception.Message-ceq'The process-bound native Open HWND did not accept bounded BM_CLICK.'}
      if(-not$clickRejected){throw 'An undelivered BM_CLICK was accepted from its message result alone.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('revalidates process start ticks after the filename write and refuses Open after PID reuse', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Assert-ReadingApplicationProcessIdentity', 'Submit-ProcessBoundOpenDialog'], String.raw`
      Add-Type -TypeDefinition @'
using System;
public static class ReadingNativePickerApi {
  public static bool IsWindowEnabled(IntPtr handle) { return true; }
  public static bool IsWindowVisible(IntPtr handle) { return true; }
}
'@
      $ticks=638500000000000000;$script:replaced=$false;$script:writes=0;$script:clicks=0
      $identity={param($id,$expected,$deadline)[pscustomobject]@{processId=$id;startUtcTicks=if($script:replaced){$expected+1}else{$expected}}}
      function Assert-ReadingSurfaceBinding {param($Binding,$ApplicationProcessId,$ApplicationProcessStartUtcTicks,$Deadline,$ProcessIdentityProvider) Assert-ReadingApplicationProcessIdentity -ApplicationProcessId $ApplicationProcessId -ApplicationProcessStartUtcTicks $ApplicationProcessStartUtcTicks -Deadline $Deadline -ProcessIdentityProvider $ProcessIdentityProvider}
      function Set-ReadingNativePickerValueBeforeDeadline {param($Handle,$Value,$Deadline)$script:writes++;$script:replaced=$true}
      function Invoke-ReadingNativePickerButtonBeforeDeadline {param($Handle,$Deadline)$script:clicks++}
      $binding=[pscustomobject]@{filenameHandle=[IntPtr]120;openButtonHandle=[IntPtr]121}
      $rejected=$false
      try{Submit-ProcessBoundOpenDialog -Binding $binding -ApplicationProcessId 42 -ApplicationProcessStartUtcTicks $ticks -Path 'fixture.pdf' -Deadline ([datetime]::UtcNow.AddSeconds(5)) -ProcessIdentityProvider $identity}catch{$rejected=$_.Exception.Message-ceq'The native picker application process identity changed.'}
      if(-not$rejected-or$writes-ne1-or$clicks-ne0){throw 'PID reuse after WM_SETTEXT reached the Open mutation.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('rejects a native picker target record from a mismatched process', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Assert-ReadingPickerTargetSnapshot'], String.raw`
      $surfaces=[Collections.Generic.HashSet[string]]::new([StringComparer]::Ordinal);$null=$surfaces.Add('101')
      $good=[pscustomobject]@{runtimeIdentity='120';surfaceRuntimeIdentity='101';targetKind='filename';nativeControlCategory='edit-1001';isEnabled=$true;isOffscreen=$false;processId=42;handle=[IntPtr]120}
      Assert-ReadingPickerTargetSnapshot -Snapshot @($good) -ApplicationProcessId 42 -SurfaceIdentities $surfaces -Kind 'The test picker targets'
      $bad=[pscustomobject]@{runtimeIdentity='121';surfaceRuntimeIdentity='101';targetKind='open';nativeControlCategory='button-idok';isEnabled=$true;isOffscreen=$false;processId=43;handle=[IntPtr]121}
      $rejected=$false;try{Assert-ReadingPickerTargetSnapshot -Snapshot @($bad) -ApplicationProcessId 42 -SurfaceIdentities $surfaces -Kind 'The test picker targets'}catch{$rejected=$true}
      if(-not$rejected){throw 'Mismatched picker child process was accepted.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('avoids PowerShell automatic variables and enforces fresh exclusive profile binding', () => {
    for (const path of ['scripts/installed-reading-tools.ps1', 'scripts/verify-signed-reading-tools.ps1']) {
      const check = String.raw`
        $tokens=$null;$errors=$null
        $ast=[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\\${path.replaceAll('/', '\\')}',[ref]$tokens,[ref]$errors)
        if($errors.Count){throw 'Script did not parse.'}
        $forbidden=@('args','error','executioncontext','foreach','home','host','input','lastexitcode','matches','myinvocation','nestedpromptlevel','ofs','pid','pscommandpath','psscriptroot','psversiontable','pwd','shellid','stacktrace','this')
        $assigned=@($ast.FindAll({param($node)$node-is[Management.Automation.Language.AssignmentStatementAst]-and$node.Left-is[Management.Automation.Language.VariableExpressionAst]},$true)|ForEach-Object{$_.Left.VariablePath.UserPath.ToLowerInvariant()})
        if(@($assigned|Where-Object{$forbidden-contains$_}).Count){throw 'Automatic or read-only variable assignment found.'}
      `;
      const result = runPowerShell(check);
      expect(result.status, result.stderr || result.stdout).toBe(0);
    }
    const source = readFileSync('scripts/installed-reading-tools.ps1', 'utf8');
    expect(source).toContain('Assert-ReadingProfileScope');
    expect(source).toContain("requested-profile|requested-ebwebview");
    expect(source).toContain("upgrade-sentinel.json");
    expect(source).toContain('stale, linked, or unrelated application-settings siblings');
    expect(source).toContain('requested WebView profile contains a reparse point');
  }, 30_000);

  it('rejects rogue descendants and binds runtime and session profile capabilities', () => {
    const source = readFileSync('scripts/installed-reading-tools.ps1', 'utf8');
    expect(source).toContain("$capabilities.PSObject.Properties['msedge.msedgedriverVersion']");
    expect(source).toContain('$capabilities.browserVersion');
    expect(source).toContain("$capabilities.'msedge.userDataDir'");
    expect(source).toContain('Get-OwnedProfileBinding');
    expect(source).toContain('Assert-TrustedConsoleHostTopology -Owned $Owned -RootProcessId $RootProcessId');
    expect(source).not.toContain('Assert-TrustedConsoleHostTopology -Owned $Owned -RootProcessId $RootProcessId -AllowAbsent');
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Get-ReadingSha256', 'Get-ReadingOwnedExecutableDiagnostic', 'Assert-ReadingOwnedExecutables'], String.raw`
      $script:ReadingPins=[ordered]@{OwnedProcessMaximum=128;UnknownExecutableHashMaximum=32}
      $script:trustedConsoleCalls=0;$script:rejectConsole=$false
      function Assert-TrustedConsoleHostTopology{param([object[]]$Owned,[int]$RootProcessId,[string]$SystemDirectory,[scriptblock]$SignatureProvider,[scriptblock]$VersionInfoProvider);$script:trustedConsoleCalls++;if($script:rejectConsole){throw 'Untrusted console topology.'};if($RootProcessId-ne900){throw 'Wrong root process.'};2}
      $app='C:\\Program Files\\PDF Workstation\\pdf-workstation.exe';$edge='C:\\drivers\\msedgedriver.exe'
      $valid=@([pscustomobject]@{ProcessId=101;ParentProcessId=900;Path=$app},[pscustomobject]@{ProcessId=102;ParentProcessId=900;Path=$edge},[pscustomobject]@{ProcessId=103;ParentProcessId=101;Path='C:\\WebView\\msedgewebview2.exe'},[pscustomobject]@{ProcessId=104;ParentProcessId=900;Path='C:\\Windows\\System32\\conhost.exe'},[pscustomobject]@{ProcessId=105;ParentProcessId=103;Path='C:\\Windows\\System32\\conhost.exe'})
      Assert-ReadingOwnedExecutables -Owned $valid -ApplicationPath $app -EdgeDriverPath $edge -RootProcessId 900
      if($trustedConsoleCalls-ne1){throw 'Reading topology did not invoke the trusted console-host validator.'}
      $rejected=$false;try{Assert-ReadingOwnedExecutables -Owned @($valid+[pscustomobject]@{ProcessId=106;ParentProcessId=900;Path='C:\\Windows\\notepad.exe'}) -ApplicationPath $app -EdgeDriverPath $edge -RootProcessId 900}catch{$rejected=$true}
      if(-not$rejected){throw 'Rogue descendant was accepted.'}
      $script:rejectConsole=$true;$consoleRejected=$false;try{Assert-ReadingOwnedExecutables -Owned $valid -ApplicationPath $app -EdgeDriverPath $edge -RootProcessId 900}catch{$consoleRejected=$_.Exception.Message-ceq'Untrusted console topology.'}
      if(-not$consoleRejected){throw 'Reading topology accepted a console host rejected by the shared validator.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('uses only bounded exact WebDriver action payloads for Ctrl+C and Ctrl+F', () => {
    const source = readFileSync('scripts/installed-reading-tools.ps1', 'utf8');
    expect(source).toContain('"/session/$SessionId/actions"');
    expect(source).toContain("[ValidateSet('c','f')][string]$Key");
    expect(source).toContain("[string][char]0xE009");
    expect(source).toContain("type = 'keyDown'");
    expect(source).toContain("type = 'keyUp'");
    expect(source).toContain('RequestBytesMaximum = 1MB');
    expect(source).toContain('LaunchTimeoutMilliseconds = 180000');
    expect(source).toContain('NativePickerTimeoutMilliseconds = 180000');
    expect(source).toContain('ProductTimeoutMilliseconds = 180000');
    expect(source).toContain('PostOpenTimeoutMilliseconds = 30000');
    expect(source).toContain('PortReleaseTimeoutMilliseconds = 180000');
    expect(source).toContain('$null = Wait-FixedWebDriverPortsFree -Deadline');
    expect(source).not.toContain('function Wait-ReadingWebDriverPortsFree');
    expect(source.indexOf('Wait-FixedWebDriverPortsFree -Deadline')).toBeLessThan(source.indexOf('$launchDeadline = New-ReadingPhaseDeadline -Phase launch'));
    expect(source).toContain('Assert-BoundedJsonShape');
  }, 30_000);

  it('starts a fresh capped product phase after native open when the launch deadline is exhausted', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['New-ReadingPhaseDeadline', 'Assert-ReadingPhaseTransition'], String.raw`
      $script:ReadingPins=[ordered]@{LaunchTimeoutMilliseconds=180000;NativePickerTimeoutMilliseconds=180000;ProductTimeoutMilliseconds=180000}
      $script:now=[datetime]::SpecifyKind([datetime]'2026-01-01T00:00:00',[DateTimeKind]::Utc);$clock={$script:now}
      $launchDeadline=New-ReadingPhaseDeadline -Phase launch -UtcNowProvider $clock
      $script:ownedValidated=$false;$script:profileValidated=$false
      function Assert-MockedReadingOwnership{$script:ownedValidated=$true;$script:now=$launchDeadline}
      function Assert-MockedReadingProfile{$script:profileValidated=$true}
      $script:pickerStarts=0;$transitionRejected=$false
      try{Assert-MockedReadingOwnership;Assert-MockedReadingProfile;Assert-ReadingPhaseTransition -Deadline $launchDeadline -Phase launch -UtcNowProvider $clock;$script:pickerStarts++;$null=New-ReadingPhaseDeadline -Phase native-picker -UtcNowProvider $clock}catch{$transitionRejected=$_.Exception.Message-ceq'The reading-tools launch phase expired before its next phase.'}
      if(-not$ownedValidated-or-not$profileValidated-or-not$transitionRejected-or$pickerStarts-ne0){throw 'Delayed ownership or profile validation started a picker after the launch deadline.'}
      $script:now=$launchDeadline.AddMilliseconds(-1)
      Assert-ReadingPhaseTransition -Deadline $launchDeadline -Phase launch -UtcNowProvider $clock
      $pickerDeadline=New-ReadingPhaseDeadline -Phase native-picker -UtcNowProvider $clock
      $productDeadline=New-ReadingPhaseDeadline -Phase product -UtcNowProvider $clock
      if(($pickerDeadline-$script:now).TotalMilliseconds-ne180000-or($productDeadline-$script:now).TotalMilliseconds-ne180000){throw 'A timely transition did not receive its exact fresh bounded phase.'}
      $script:ReadingPins.ProductTimeoutMilliseconds=180001;$capRejected=$false
      try{New-ReadingPhaseDeadline -Phase product -UtcNowProvider $clock}catch{$capRejected=$_.Exception.Message-ceq'A reading-tools phase timeout was outside its exact cap.'}
      if(-not$capRejected){throw 'A reading phase exceeded its explicit timeout cap.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const scriptResult = runPowerShell(extractFunctions('scripts/installed-reading-tools.ps1', ['Get-ReadingPostOpenScript'], String.raw`
      (Get-ReadingPostOpenScript -ExpectedName 'reading-source.pdf')|ConvertTo-Json -Compress
    `));
    expect(scriptResult.status, scriptResult.stderr || scriptResult.stdout).toBe(0);
    const postOpenScript = JSON.parse(scriptResult.stdout.trim()) as string;
    expect(() => new Function('document', postOpenScript)).not.toThrow();
    expect(postOpenScript).not.toMatch(/\.path|textContent\s*[,}]/);
    const source = normalizeNewlines(readFileSync('scripts/installed-reading-tools.ps1', 'utf8'));
    const open = source.indexOf('Open-ReadingUserFile -SessionId $sessionId -ApplicationProcessId $applicationProcessId -ApplicationProcessStartUtcTicks $applicationProcessStartUtcTicks -Path $PlainFixture -Deadline $plainPickerDeadline');
    const product = source.indexOf('$plainProductDeadline = New-ReadingPhaseDeadline -Phase product');
    const layer = source.indexOf("$layer = Wait-WebDriverOracle -SessionId $sessionId -Deadline $plainProductDeadline -Kind 'embedded text layer'");
    expect(open).toBeGreaterThan(-1);
    expect(product).toBeGreaterThan(open);
    expect(layer).toBeGreaterThan(product);
    expect(source).not.toContain("-Deadline $launchDeadline -Kind 'embedded text layer'");
    const functionStart = source.indexOf('function Invoke-RealInstalledReadingTools');
    const functionEnd = source.indexOf('function Invoke-InstalledReadingTools', functionStart);
    const invokeSource = source.slice(functionStart, functionEnd);
    expect((invokeSource.match(/New-ReadingPhaseDeadline -Phase /g) || []).length).toBe(9);
    expect((invokeSource.match(/Assert-ReadingPhaseTransition -Deadline /g) || []).length).toBe(8);
    expect(invokeSource).toContain('Assert-ReadingProfileScope -ProfileRoot $ProfileRoot -SettingsRoot $SettingsRoot -Binding $profileBinding\n\n        Assert-ReadingPhaseTransition -Deadline $launchDeadline -Phase launch\n        $plainPickerDeadline = New-ReadingPhaseDeadline -Phase native-picker');
  }, 30_000);

  it('proves the allowlisted document identity before render waits and emits only bounded post-open facts', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Assert-ReadingPostOpenState', 'Wait-ReadingPostOpenState'], String.raw`
      $script:ReadingPins=[ordered]@{PostOpenTimeoutMilliseconds=30000;UiAutomationPollMilliseconds=1}
      function New-State([int]$tabs,[int]$active,[bool]$matched,[int]$inputs,[int]$pages,[int]$rendered,[int]$images,[int]$loaded,[int]$layers,[int]$glyphs,[int]$statuses,[int]$dialogs,[int]$alerts){[pscustomobject]@{schemaVersion=[long]1;documentTabCount=[long]$tabs;activeTabCount=[long]$active;activeFilenameMatches=$matched;pageInputCount=[long]$inputs;pageCount=[long]$pages;renderedPageCount=[long]$rendered;imageCount=[long]$images;loadedImageCount=[long]$loaded;textLayerCount=[long]$layers;pageOneGlyphCount=[long]$glyphs;pageTextStatusCount=[long]$statuses;passwordDialogCount=[long]$dialogs;alertCount=[long]$alerts}}
      $script:now=[datetime]::SpecifyKind([datetime]'2026-01-01T00:00:00',[DateTimeKind]::Utc);$clock={$script:now};$sleep={param($milliseconds)$script:now=$script:now.AddMilliseconds($milliseconds)};$script:probes=0
      $receipt=Wait-ReadingPostOpenState -SessionId 'session' -ExpectedName 'reading-source.pdf' -ExpectedPages 6 -NativeControlCategory 'edit-1001' -Deadline $script:now.AddSeconds(5) -StateProvider {$script:probes++;if($script:probes-eq1){New-State 0 0 $false 0 0 0 0 0 0 0 0 0 0}else{New-State 1 1 $true 1 6 1 1 0 0 0 0 0 0}} -SleepProvider $sleep -UtcNowProvider $clock
      if($probes-ne2-or$receipt.nativeFilenameControlCategory-cne'edit-1001'-or$receipt.documentTabCount-ne1-or-not$receipt.activeFilenameMatches-or$receipt.pageCount-ne6-or$receipt.renderedPageCount-ne1-or$receipt.textLayerCount-ne0){throw 'The exact post-open receipt was not retained.'}
      $script:probes=0;$wrongRejected=$false;$wrongMessage=''
      try{Wait-ReadingPostOpenState -SessionId 'session' -ExpectedName 'reading-source.pdf' -ExpectedPages 6 -NativeControlCategory 'edit-1148' -Deadline $script:now.AddSeconds(5) -StateProvider {$script:probes++;New-State 1 1 $false 1 6 1 1 1 0 0 0 0 0} -SleepProvider $sleep -UtcNowProvider $clock}catch{$wrongMessage=$_.Exception.Message;$wrongRejected=$wrongMessage-like'The native picker did not open the exact allowlisted plain fixture*'}
      if(-not$wrongRejected-or$probes-ne1-or$wrongMessage-notmatch'"nativeFilenameControlCategory":"edit-1148"'-or$wrongMessage-notmatch'"activeFilenameMatches":false'-or$wrongMessage-match'(?i)(reading-source|[A-Z]:\\|A place for your PDFs)'){throw 'Wrong-document rejection was late or leaked raw state.'}
      $script:probes=0;$script:sleeps=0;$deadline=$script:now.AddMilliseconds(5);$lateRejected=$false
      try{Wait-ReadingPostOpenState -SessionId 'session' -ExpectedName 'reading-source.pdf' -ExpectedPages 6 -NativeControlCategory 'edit-1001' -Deadline $deadline -StateProvider {$script:probes++;$script:now=$deadline;New-State 1 1 $true 1 6 1 1 1 1 30 0 0 0} -SleepProvider {$script:sleeps++} -UtcNowProvider $clock}catch{$lateRejected=$_.Exception.Message-like'The exact allowlisted plain fixture did not appear*'}
      if(-not$lateRejected-or$probes-ne1-or$sleeps-ne0){throw 'A post-deadline document identity read was accepted.'}
      $script:ReadingPins.PostOpenTimeoutMilliseconds=30001;$capRejected=$false
      try{Wait-ReadingPostOpenState -SessionId 'session' -ExpectedName 'reading-source.pdf' -ExpectedPages 6 -NativeControlCategory 'edit-1001' -Deadline $script:now.AddSeconds(60) -StateProvider {throw 'The over-cap timeout reached a state read.'} -UtcNowProvider $clock}catch{$capRejected=$_.Exception.Message-ceq'The post-open document-state timeout exceeded its exact cap.'}
      if(-not$capRejected){throw 'An over-cap post-open timeout was accepted.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const source = readFileSync('scripts/installed-reading-tools.ps1', 'utf8');
    expect(source).toContain('activeFilenameMatches:name===expected');
    expect(source).not.toContain('activeFilename:name');
    expect(source.indexOf("$postOpen = Wait-ReadingPostOpenState")).toBeLessThan(source.indexOf('$null = Wait-ReadingDocument -SessionId $sessionId -Pages 6'));
  }, 30_000);

  it('activates the unique signed text-selection mode after render and before waiting for its layer', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Enable-ReadingTextSelection'], String.raw`
      $script:now=[datetime]::SpecifyKind([datetime]'2026-01-01T00:00:00',[DateTimeKind]::Utc);$clock={$script:now};$deadline=$script:now.AddSeconds(1);$script:actions=0
      Enable-ReadingTextSelection -SessionId 'session' -Deadline $deadline -StateProvider {$script:actions++;[pscustomobject]@{count=[long]1;clicked=$true}} -UtcNowProvider $clock
      if($actions-ne1){throw 'The unique text-selection action did not execute exactly once.'}
      $ambiguous=$false;try{Enable-ReadingTextSelection -SessionId 'session' -Deadline $deadline -StateProvider {[pscustomobject]@{count=[long]2;clicked=$false}} -UtcNowProvider $clock}catch{$ambiguous=$_.Exception.Message-ceq'The exact installed text-selection mode control was unavailable.'}
      if(-not$ambiguous){throw 'Ambiguous text-selection controls were accepted.'}
      $script:now=$deadline;$script:actions=0;$expired=$false
      try{Enable-ReadingTextSelection -SessionId 'session' -Deadline $deadline -StateProvider {$script:actions++;[pscustomobject]@{count=[long]1;clicked=$true}} -UtcNowProvider $clock}catch{$expired=$_.Exception.Message-ceq'The text-selection mode deadline expired before its exact action.'}
      if(-not$expired-or$actions-ne0){throw 'An expired text-selection action was invoked.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const scriptResult = runPowerShell(extractFunctions('scripts/installed-reading-tools.ps1', ['Get-ReadingEnableTextSelectionScript'], String.raw`
      (Get-ReadingEnableTextSelectionScript)|ConvertTo-Json -Compress
    `));
    expect(scriptResult.status, scriptResult.stderr || scriptResult.stdout).toBe(0);
    const selectionScript = JSON.parse(scriptResult.stdout.trim()) as string;
    expect(() => new Function('document', selectionScript)).not.toThrow();
    let clicks = 0;
    const button = { disabled: false, click: () => { clicks += 1; } };
    const document = { querySelectorAll: (selector: string) => selector === 'button[aria-label="Select text on page"]' ? [button] : [] };
    expect(new Function('document', selectionScript)(document)).toEqual({ count: 1, clicked: true });
    expect(clicks).toBe(1);
    const source = readFileSync('scripts/installed-reading-tools.ps1', 'utf8');
    const render = source.indexOf('$null = Wait-ReadingDocument -SessionId $sessionId -Pages 6');
    const activate = source.indexOf('Enable-ReadingTextSelection -SessionId $sessionId -Deadline $plainProductDeadline');
    const layer = source.indexOf("$layer = Wait-WebDriverOracle -SessionId $sessionId -Deadline $plainProductDeadline -Kind 'embedded text layer'");
    expect(render).toBeGreaterThan(-1);
    expect(activate).toBeGreaterThan(render);
    expect(layer).toBeGreaterThan(activate);
    expect(readFileSync('src/preferences.ts', 'utf8')).toContain('hand: true');
    expect(readFileSync('src/Viewer.tsx', 'utf8')).toContain('selectable={!hand && !commentMode && !highlightMode && redactionPage === null}');
  }, 30_000);

  it('uses the signed Home marker and the unique global Open action when Home has two Open buttons', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Get-ReadingHomeScript', 'Get-ReadingOpenFileScript'], String.raw`
      [ordered]@{home=Get-ReadingHomeScript;open=Get-ReadingOpenFileScript}|ConvertTo-Json -Compress
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const scripts = JSON.parse(result.stdout.trim()) as { home: string; open: string };
    expect(() => new Function('document', scripts.home)).not.toThrow();
    expect(() => new Function('document', scripts.open)).not.toThrow();
    const globalParent = { querySelector: (selector: string) => selector === 'button[aria-label="Toggle theme"]' ? {} : null };
    const emptyParent = { querySelector: () => null };
    const clicked: string[] = [];
    const buttons = [
      { textContent: 'Open a file', disabled: false, parentElement: globalParent, click: () => clicked.push('global') },
      { textContent: 'Open a file', disabled: false, parentElement: emptyParent, click: () => clicked.push('empty') },
      { textContent: 'Explore a sample PDF', disabled: false, parentElement: emptyParent, click: () => clicked.push('sample') },
    ];
    const document = { readyState: 'complete', title: 'PDF Workstation', querySelectorAll: (selector: string) => selector === 'button' ? buttons : [] };
    expect(new Function('document', scripts.home)(document)).toEqual({ ready: true, title: 'PDF Workstation', sample: 1 });
    expect(new Function('document', scripts.open)(document)).toBe(true);
    expect(clicked).toEqual(['global']);
    const checkedOutApp = readFileSync('src/App.tsx', 'utf8');
    expect((checkedOutApp.match(/Open a file/g) || []).length).toBeGreaterThan(1);
    expect(checkedOutApp).toContain('Explore a sample PDF');
  }, 30_000);

  it('waits boundedly for fixed ports released by the prior installed-app proof', () => {
    const check = extractFunctions('scripts/installed-app-launch.ps1', ['Wait-FixedWebDriverPortsFree'], String.raw`
      $script:LaunchPins=[ordered]@{CleanupProcessPollMilliseconds=100}
      $script:now=[datetime]::SpecifyKind([datetime]'2026-01-01T00:00:00',[DateTimeKind]::Utc);$attempts=0
      $clock={$script:now};$sleep={param($milliseconds)$script:now=$script:now.AddMilliseconds($milliseconds)}
      Wait-FixedWebDriverPortsFree -Deadline $script:now.AddMilliseconds(500) -ProbeProvider { $script:attempts++;if($script:attempts-lt3){throw 'A fixed loopback WebDriver port is already in use.'} } -SleepProvider $sleep -UtcNowProvider $clock|Out-Null
      if($attempts-ne3){throw 'Shared port-release probe did not retry exactly to success.'}
      $expiredProbes=0;$rejected=$false
      try{Wait-FixedWebDriverPortsFree -Deadline $script:now -ProbeProvider { $script:expiredProbes++ } -SleepProvider $sleep -UtcNowProvider $clock|Out-Null}catch{$rejected=$_.Exception.Message-ceq'Fixed loopback WebDriver ports did not become free before the handoff deadline.'}
      if(-not$rejected){throw 'Expired shared port-release wait did not fail closed.'}
      if($expiredProbes-ne0){throw 'Expired shared port-release wait invoked its probe.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('suppresses the shared port status and leaves one reading result object on the return pipeline', () => {
    const check = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      $tokens=$null;$errors=$null
      $ast=[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\\scripts\\installed-reading-tools.ps1',[ref]$tokens,[ref]$errors)
      if($errors.Count){throw 'Reading script did not parse.'}
      $function=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq'Invoke-RealInstalledReadingTools'},$true)
      $waits=@($function.FindAll({param($node)$node-is[Management.Automation.Language.CommandAst]-and$node.GetCommandName()-ceq'Wait-FixedWebDriverPortsFree'},$true))
      if($waits.Count-ne1){throw 'Reading function did not contain one shared fixed-port handoff.'}
      $assignment=$waits[0]
      while($null-ne$assignment-and$assignment-isnot[Management.Automation.Language.AssignmentStatementAst]){$assignment=$assignment.Parent}
      if($null-eq$assignment-or$assignment.Left-isnot[Management.Automation.Language.VariableExpressionAst]-or$assignment.Left.VariablePath.UserPath-cne'null'){throw 'Shared fixed-port handoff output was not assigned to null.'}
      $returns=@($function.FindAll({param($node)$node-is[Management.Automation.Language.ReturnStatementAst]-and$node.Extent.Text.Trim()-ceq'return $result'},$true))
      if($returns.Count-ne1){throw 'Reading function did not expose one exact result return.'}
      function Wait-FixedWebDriverPortsFree{param([datetime]$Deadline);'free'}
      $script:ReadingPins=[ordered]@{PortReleaseTimeoutMilliseconds=180000}
      $result=[pscustomobject][ordered]@{schemaVersion=1;kind='reading-tools'}
      $statements=$assignment.Extent.Text+[Environment]::NewLine+$returns[0].Extent.Text
      $output=@(&([scriptblock]::Create($statements)))
      if($output.Count-ne1-or$output[0]-isnot[Management.Automation.PSCustomObject]-or($output[0].PSObject.Properties.Name-join',')-cne'schemaVersion,kind'){throw 'Shared fixed-port status contaminated the reading result pipeline.'}
    `;
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('reports deterministic bounded privacy-safe unknown descendant diagnostics', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Get-ReadingSha256', 'Get-ReadingOwnedExecutableDiagnostic', 'Assert-ReadingOwnedExecutables'], String.raw`
      $script:ReadingPins=[ordered]@{OwnedProcessMaximum=128;UnknownExecutableHashMaximum=32}
      $app='C:\\Program Files\\PDF Workstation\\pdf-workstation.exe';$edge='C:\\drivers\\msedgedriver.exe'
      $valid=@([pscustomobject]@{Path=$app},[pscustomobject]@{Path=$edge},[pscustomobject]@{Path='C:\\WebView\\msedgewebview2.exe'})
      $unknownA=[pscustomobject]@{Path='C:\\private\\NoTePad.EXE';ProcessId=98765;CommandLine='secret argument'}
      $unknownB=[pscustomobject]@{Path='D:\\sensitive\\ALPHA.exe';ProcessId=87654;CommandLine='private switch'}
      $missing=[pscustomobject]@{Path=$null;ProcessId=76543;CommandLine='hidden'}
      $first=Get-ReadingOwnedExecutableDiagnostic -Owned @($valid+$unknownA+$unknownB+$missing) -ApplicationPath $app -EdgeDriverPath $edge
      $second=Get-ReadingOwnedExecutableDiagnostic -Owned @(@($missing)+@($unknownB)+$valid+@($unknownA)) -ApplicationPath $app -EdgeDriverPath $edge
      $firstJson=$first|ConvertTo-Json -Depth 4 -Compress;$secondJson=$second|ConvertTo-Json -Depth 4 -Compress
      if($firstJson-cne$secondJson){throw 'Unknown descendant diagnostic was not deterministic.'}
      $expectedProperties='schemaVersion,capturedCount,applicationCount,edgeDriverCount,webViewCount,consoleHostCount,unknownCount,missingPathCount,unknownLeafHashCount,unknownLeafHashes,unknownLeafHashesTruncated,parentCategoryCountsAvailable,parentCategoryCounts'
      if(($first.PSObject.Properties.Name-join',')-cne$expectedProperties){throw 'Unknown descendant diagnostic schema drifted.'}
      if($first.capturedCount-ne6-or$first.applicationCount-ne1-or$first.edgeDriverCount-ne1-or$first.webViewCount-ne1-or$first.unknownCount-ne3-or$first.missingPathCount-ne1){throw 'Unknown descendant counts were incorrect.'}
      if($first.unknownLeafHashCount-ne2-or$first.unknownLeafHashes.Count-ne2-or$first.unknownLeafHashesTruncated-or$first.parentCategoryCountsAvailable-or$first.parentCategoryCounts.Count-ne0){throw 'Unknown descendant privacy fields were incorrect.'}
      if(@($first.unknownLeafHashes|Where-Object{$_-cnotmatch'^[A-F0-9]{64}$'}).Count){throw 'Unknown descendant leaf hash was malformed.'}
      function Assert-TrustedConsoleHostTopology{param([object[]]$Owned,[int]$RootProcessId,[string]$SystemDirectory,[scriptblock]$SignatureProvider,[scriptblock]$VersionInfoProvider);2}
      $console=@([pscustomobject]@{Path='C:\\Windows\\System32\\conhost.exe'},[pscustomobject]@{Path='C:\\Windows\\System32\\conhost.exe'})
      $message='';try{Assert-ReadingOwnedExecutables -Owned @($valid+$console+$unknownA+$missing) -ApplicationPath $app -EdgeDriverPath $edge -RootProcessId 900}catch{$message=$_.Exception.Message}
      if($message-cnotmatch'^Reading-tools captured an unexpected descendant executable; diagnostic='){throw 'Strict unknown descendant rejection was lost.'}
      foreach($forbidden in @('notepad.exe','private','sensitive','98765','secret argument','C:\\','D:\\')){if($message.ToLowerInvariant().Contains($forbidden.ToLowerInvariant())){throw 'Unknown descendant diagnostic leaked a raw value.'}}
      $many=@($valid);for($index=0;$index-lt35;$index++){$many+=[pscustomobject]@{Path=('C:\\private\\rogue-{0:d2}.exe'-f$index)}}
      $bounded=Get-ReadingOwnedExecutableDiagnostic -Owned $many -ApplicationPath $app -EdgeDriverPath $edge
      if($bounded.unknownCount-ne35-or$bounded.unknownLeafHashCount-ne32-or$bounded.unknownLeafHashes.Count-ne32-or-not$bounded.unknownLeafHashesTruncated){throw 'Unknown descendant hash cap was not enforced.'}
      $over=@();for($index=0;$index-lt129;$index++){$over+=[pscustomobject]@{Path=$app}}
      $capRejected=$false;try{Get-ReadingOwnedExecutableDiagnostic -Owned $over -ApplicationPath $app -EdgeDriverPath $edge|Out-Null}catch{$capRejected=$_.Exception.Message-ceq'Reading-tools owned executable inventory exceeded its process-count cap.'}
      if(-not$capRejected){throw 'Owned executable process cap did not fail closed.'}
      $firstJson
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const diagnostic = JSON.parse(result.stdout.trim()) as { unknownLeafHashes: string[] };
    const expectedNotepadHash = createHash('sha256').update('notepad.exe', 'utf8').digest('hex').toUpperCase();
    expect(diagnostic.unknownLeafHashes).toEqual([...diagnostic.unknownLeafHashes].sort());
    expect(diagnostic.unknownLeafHashes).toContain(expectedNotepadHash);
    expect(result.stdout).not.toMatch(/notepad\.exe|private|sensitive|98765|secret argument|[A-Z]:\\/i);
  }, 30_000);

  it('loads and executes the production receipt helper in a standalone reading script process', () => {
    const source = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      . '${process.cwd().replaceAll("'", "''")}\scripts\installed-reading-tools.ps1'
      $root=Join-Path $env:SMACROBAT_TEST_ROOT ('reading-receipt-'+[Guid]::NewGuid().ToString('N'))
      [IO.Directory]::CreateDirectory($root)|Out-Null
      $path=Join-Path $root 'receipt.bin';[IO.File]::WriteAllBytes($path,[byte[]](1,2,3,4))
      $sha=Get-ExactSha256 -Path $path
      Assert-ReadingFileReceipt -Path $path -Bytes 4 -Sha256 $sha -Kind 'Standalone receipt fixture'
      [IO.File]::WriteAllBytes($path,[byte[]](1,2,3,5))
      $rejected=$false;try{Assert-ReadingFileReceipt -Path $path -Bytes 4 -Sha256 $sha -Kind 'Standalone receipt fixture'}catch{$rejected=$true}
      if(-not$rejected){throw 'The production receipt helper accepted changed bytes.'}
    `;
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('generates the exact deterministic encrypted PDF without a helper binary or network', () => {
    const script = readFileSync('scripts/verify-signed-reading-tools.ps1', 'utf8');
    expect(script).not.toMatch(/cargo|rustc|Invoke-WebRequest|Invoke-RestMethod|https?:\/\//i);
    const source = extractFunctions('scripts/verify-signed-reading-tools.ps1', [
      'Join-ReadingByteArrays', 'Get-ReadingMd5', 'Invoke-ReadingRc4',
      'Get-ReadingPasswordPad', 'New-ProtectedReadingFixture',
    ], String.raw`
      . '${process.cwd().replaceAll("'", "''")}\scripts\installed-reading-tools.ps1'
      $script:ReadingVerificationPins=[ordered]@{ProtectedFixtureBytes=[uint64]912;ProtectedFixtureSha256='9BE85022232671CFD796C711E0B8B06847E7020D49AA07FA20DBD6DADE168A37'}
      $root=Join-Path $env:SMACROBAT_TEST_ROOT ('reading-fixture-'+[Guid]::NewGuid().ToString('N'))
      [IO.Directory]::CreateDirectory($root)|Out-Null
      New-ProtectedReadingFixture -Destination (Join-Path $root 'first.pdf')
      New-ProtectedReadingFixture -Destination (Join-Path $root 'second.pdf')
      $a=[IO.File]::ReadAllBytes((Join-Path $root 'first.pdf'));$b=[IO.File]::ReadAllBytes((Join-Path $root 'second.pdf'))
      if(-not[System.Linq.Enumerable]::SequenceEqual[byte]($a,$b)){throw 'Fixture generation was not deterministic.'}
      $ascii=[Text.Encoding]::ASCII.GetString($a)
      if($ascii-notmatch'^%PDF-1\.4'-or$ascii-notmatch'/Filter /Standard /V 1 /R 2'-or$ascii-notmatch'/Encrypt 6 0 R'-or$ascii-match'Protected reading fixture'){throw 'Encrypted fixture structure was not exact.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);

  it('retains only hashed or Boolean reading evidence and gates cleanup at zero processes', () => {
    const installed = readFileSync('scripts/installed-reading-tools.ps1', 'utf8');
    const verify = readFileSync('scripts/verify-signed-reading-tools.ps1', 'utf8');
    expect(verify).toContain("reading-tools-verification.json");
    expect(verify).toContain('selectedTextSha256');
    expect(verify).toContain('clipboardSha256');
    expect(verify).toContain("method = 'programmatic-dom-range-plus-real-windows-clipboard'");
    expect(verify).toContain('programmaticDomSelectionVerified');
    expect(verify).toContain('textSelectionModeActivated=[bool]$UiResult.textSelectionModeActivated');
    expect(verify).toContain('nativeFilenameControlCategory=[string]$UiResult.nativeFilenameControlCategory');
    expect(verify).toContain('activeFilenameMatched=[bool]$UiResult.postOpenActiveFilenameMatched');
    expect(verify).toContain('pageOneGlyphCount=[int]$UiResult.postOpenPageOneGlyphCount');
    expect(verify).toContain('pageTextStatusCount=[int]$UiResult.postOpenPageTextStatusCount');
    expect(verify).toContain('uiReentryRequiredAfterCloseAndReopen');
    expect(verify).toContain('passwordStoragePersistenceInspected=$false');
    expect(verify).toContain('artifactIdentifierIncluded = $false');
    expect(verify).toContain('[string]$script:ReadingVerificationPins.LeakIds');
    expect(verify).toContain('The reading-tools record contains a raw path, text fixture value, password');
    expect(installed).toContain('Invoke-ReadingClipboardSta -Operation clear');
    expect(installed).toContain('$clipboardCleared');
    expect(installed).toContain('Post-flow plain reading fixture');
    expect(installed).toContain('The reading fixture inventory changed or contains a reparse point.');
    expect(installed).toContain('Assert-LaunchCleanupState');
    expect(installed).toContain('Wait-LaunchProcessQuiescence -Deadline $processCleanupDeadline');
    expect(installed).toContain('-not $processesQuiescent');
    expect(installed).toContain("$sessionDeleteOutcome -cne 'verified'");
    expect(installed).toContain('$remaining -ne 0');
    expect(installed).toContain('-NotePropertyName relevantProcessesRemaining -NotePropertyValue 0');
    expect(verify).not.toMatch(/plainFixture\s*=\s*\[ordered\]@\{[^}]*path/);
  }, 30_000);

  it('retries transient clipboard cleanup and accepts only the exact empty receipt before deadline', () => {
    const check = extractFunctions('scripts/installed-reading-tools.ps1', ['Get-ReadingSha256', 'Get-ReadingTextReceipt', 'Clear-ReadingClipboardAndVerify'], String.raw`
      $script:ReadingPins=[ordered]@{ClipboardPollMilliseconds=1}
      $script:now=[datetime]'2026-01-01T00:00:00Z';$clock={$script:now};$sleep={param($milliseconds)$script:now=$script:now.AddMilliseconds($milliseconds)}
      $script:clears=0;$script:receipts=0
      $provider={param($operation)
        if($operation-ceq'clear'){$script:clears++;if($script:clears-eq1){throw 'clipboard busy'};return}
        $script:receipts++
        if($script:receipts-eq1){return Get-ReadingTextReceipt -Text 'stale'}
        return Get-ReadingTextReceipt -Text ''
      }
      $verified=Clear-ReadingClipboardAndVerify -Deadline $script:now.AddSeconds(1) -OperationProvider $provider -SleepProvider $sleep -UtcNowProvider $clock
      if(-not$verified-or$script:clears-ne3-or$script:receipts-ne2){throw 'Transient clipboard cleanup was not retried to the exact empty receipt.'}

      $script:expiredCalls=0;$expired=$false
      try{Clear-ReadingClipboardAndVerify -Deadline $script:now -OperationProvider {$script:expiredCalls++} -SleepProvider $sleep -UtcNowProvider $clock}catch{$expired=$_.Exception.Message-ceq'The Windows clipboard did not clear to its exact empty receipt before the cleanup deadline.'}
      if(-not$expired-or$script:expiredCalls-ne0){throw 'Expired clipboard cleanup performed an operation or did not fail closed.'}

      $script:now=[datetime]'2026-01-01T00:00:00Z';$script:persistentCalls=0;$persistent=$false
      try{Clear-ReadingClipboardAndVerify -Deadline $script:now.AddMilliseconds(2) -OperationProvider {param($operation)$script:persistentCalls++;if($operation-ceq'receipt'){Get-ReadingTextReceipt -Text 'stale'}} -SleepProvider $sleep -UtcNowProvider $clock}catch{$persistent=$_.Exception.Message-ceq'The Windows clipboard did not clear to its exact empty receipt before the cleanup deadline.'}
      if(-not$persistent-or$script:persistentCalls-lt2){throw 'Persistent non-empty clipboard state did not fail closed after bounded retries.'}
    `);
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 30_000);
});
