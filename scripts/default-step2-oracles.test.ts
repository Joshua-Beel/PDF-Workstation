import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

function runWithFunctions(file: string, names: string[], body: string) {
  const root = join('target', `step2-oracles-${randomUUID()}`);
  mkdirSync(root, { recursive: true });
  const script = join(root, 'run.ps1');
  try {
    writeFileSync(script, [
      "$ErrorActionPreference = 'Stop'",
      '$tokens = $null; $errors = $null',
      `$ast = [Management.Automation.Language.Parser]::ParseFile('${join(process.cwd(), 'scripts', file).replaceAll("'", "''")}', [ref]$tokens, [ref]$errors)`,
      "if ($errors.Count) { throw 'parse' }",
      `foreach ($name in @(${names.map(name => `'${name}'`).join(',')})) { $node = $ast.Find({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] -and $n.Name -ceq $name }, $true); if (-not $node) { throw "missing $name" }; Invoke-Expression $node.Extent.Text }`,
      body,
      "Write-Output 'PASS'",
    ].join('\n'), 'utf8');
    return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', script], { encoding: 'utf8', timeout: 60_000 });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

describe('default Step 2 oracles', () => {
  it('recognises a quarter-turn render in fit-width and fixed-zoom layouts and refuses unrotated or half-turned renders', () => {
    const result = runWithFunctions('verify-default-recovery.ps1', ['Test-RecoveryQuarterTurnRender'], [
      "if (-not (Test-RecoveryQuarterTurnRender 1136 878 1136 1470)) { throw 'fit-width quarter turn refused' }",
      "if (-not (Test-RecoveryQuarterTurnRender 1470 1136 1136 1470)) { throw 'fixed-zoom quarter turn refused' }",
      "if (-not (Test-RecoveryQuarterTurnRender 1368 1058 1057 1368)) { throw 'rounded fixed-zoom quarter turn refused' }",
      "if (Test-RecoveryQuarterTurnRender 1136 1470 1136 1470) { throw 'unrotated render accepted' }",
      "if (Test-RecoveryQuarterTurnRender 1200 1000 1136 1470) { throw 'wrong aspect accepted' }",
      "if (Test-RecoveryQuarterTurnRender 0 0 1136 1470) { throw 'missing image accepted' }",
    ].join('\n'));
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 90_000);

  it('reports the last observed value when a recovery oracle times out, and passes the caller variables through', () => {
    const result = runWithFunctions('verify-default-recovery.ps1', ['Wait-RecoveryOracle'], [
      "function Wait-WebDriverOracle { param([string]$SessionId,[string]$Script,[datetime]$Deadline,[scriptblock]$Predicate,[string]$Kind) foreach ($value in @([pscustomobject]@{w=1;h=2},[pscustomobject]@{w=3;h=4})) { if (& $Predicate $value) { return $value } }; throw \"$Kind did not become true before the shared launch deadline.\" }",
      "function Invoke-Caller { param($PhaseA) Wait-RecoveryOracle ([pscustomobject]@{SessionId='s';Deadline=[datetime]::UtcNow.AddMinutes(1)}) 'script' {param($v)[int]$v.w -eq [int]$PhaseA.want} 'Probe' }",
      "$found = Invoke-Caller ([pscustomobject]@{want=3}); if ($found.w -ne 3) { throw 'caller variable did not reach the predicate' }",
      "$message = $null; try { Invoke-Caller ([pscustomobject]@{want=9}) | Out-Null } catch { $message = $_.Exception.Message }",
      "if ($message -notlike 'Probe did not become true*Last observed: {\"w\":3,\"h\":4}') { throw ('unexpected message: ' + $message) }",
    ].join('\n'));
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 90_000);

  it('uses the quarter-turn check for the Recovery B render and logs image-output diagnostics on a stable-file timeout', () => {
    const recovery = readFileSync('scripts/verify-default-recovery.ps1', 'utf8');
    expect(recovery).toContain("-and(Test-RecoveryQuarterTurnRender ([int]$v.w) ([int]$v.h) ([int]$PhaseA.originalWidth) ([int]$PhaseA.originalHeight))} 'Recovery B rotated render'");
    expect(recovery).not.toContain('[int]$v.w-eq[int]$PhaseA.originalHeight');
    const tools = readFileSync('scripts/installed-image-page-tools.ps1', 'utf8');
    expect(tools).toContain("catch { Write-ImagePageOutputDiagnostics -Path $CreatedPdfPath -Stage 'create-output' -SessionId $sessionId -Deadline $deadline; throw }");
    expect(tools).toContain("catch { Write-ImagePageOutputDiagnostics -Path $ExportedPngPath -Stage 'export-output' -SessionId $sessionId -Deadline $deadline; throw }");
  });

  it('waits for the Save dialog to settle its suggested name, then re-types the filename if the dialog replaces it', () => {
    const stubs = [
      "function Get-BoundNativeSaveRoleRecord { [pscustomobject]@{ HandleValue = 505 } }",
      "function Get-NativeSurfaceHandleValue { 500 }",
      "function Initialize-PrintNativeWindowInterop { throw 'real interop must not load in this test' }",
      "$binding = [pscustomobject]@{ surfaceRootIdentity = 'hwnd:500|native' }",
      "$value = 'D:\\proof\\created.pdf'",
      "function Run-Case([int]$OverwriteAfterSets) {",
      "  $script:edit = 'image'; $script:sets = 0; $script:reads = 0; $script:initial = @('image', 'image-source.pdf', 'image-source.pdf')",
      "  $set = { param($p, $t, $r, $e, $v, $d) $script:sets++; $script:edit = $v; $true }",
      "  $read = { param($p, $t, $r, $e, $d) $script:reads++; if ($script:sets -eq 0) { if ($script:initial.Count) { $script:edit = $script:initial[0]; $script:initial = @($script:initial | Select-Object -Skip 1) } } elseif ($script:sets -le $OverwriteAfterSets) { $script:edit = 'image-source.pdf' }; $script:edit }",
      "  Set-BoundNativeSaveFileNameExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -Value $value -DeadlineTickCount ([Environment]::TickCount64 + 20000) -SetProvider $set -ReadProvider $read -SettleMilliseconds 5",
      "}",
      "Run-Case 0; if ($script:sets -ne 1 -or $script:edit -cne $value) { throw ('settled case: sets=' + $script:sets) }",
      "Run-Case 1; if ($script:sets -ne 2 -or $script:edit -cne $value) { throw ('replaced-once case: sets=' + $script:sets) }",
      "$message = $null; try { Run-Case 99 } catch { $message = $_.Exception.Message }; if ($message -notlike '*did not stay set*' -or $script:sets -ne 4) { throw ('always-replaced case: ' + $message + ' sets=' + $script:sets) }",
    ].join('\n');
    const result = runWithFunctions('installed-print-dialog.ps1', ['Set-BoundNativeSaveFileNameExact'], stubs);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 90_000);

  it('waits for the application suggestion before typing, so a late SetFileName cannot win, and fails closed if it never appears', () => {
    const stubs = [
      "function Get-BoundNativeSaveRoleRecord { [pscustomobject]@{ HandleValue = 505 } }",
      "function Get-NativeSurfaceHandleValue { 500 }",
      "function Initialize-PrintNativeWindowInterop { throw 'real interop must not load in this test' }",
      "$binding = [pscustomobject]@{ surfaceRootIdentity = 'hwnd:500|native' }",
      "$value = 'D:\\proof\\created.pdf'",
      "function Run-Late([int]$EmptyReads, [string]$Suggested, [string]$Expect, [int]$TimeoutMs = 20000) {",
      "  $script:edit = ''; $script:sets = 0; $script:reads = 0; $script:typedBeforeSuggestion = $false",
      "  $set = { param($p, $t, $r, $e, $v, $d) if ($script:reads -le $EmptyReads) { $script:typedBeforeSuggestion = $true }; $script:sets++; $script:edit = $v; $true }",
      "  $read = { param($p, $t, $r, $e, $d) $script:reads++; if ($script:sets -eq 0) { $script:edit = if ($script:reads -le $EmptyReads) { '' } else { $Suggested } }; $script:edit }",
      "  Set-BoundNativeSaveFileNameExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -Value $value -DeadlineTickCount ([Environment]::TickCount64 + 30000) -SetProvider $set -ReadProvider $read -SettleMilliseconds 5 -AwaitSuggestion -SuggestedName $Expect -SuggestionTimeoutMilliseconds $TimeoutMs",
      "}",
      "Run-Late 12 'image-source.pdf' 'image-source.pdf'; if ($script:typedBeforeSuggestion -or $script:sets -ne 1 -or $script:edit -cne $value) { throw ('exact late suggestion: sets=' + $script:sets) }",
      "Run-Late 12 'combined.pdf' '' | Out-Null; if ($script:typedBeforeSuggestion -or $script:sets -ne 1) { throw 'any late suggestion' }",
      "$message = $null; try { Run-Late 100000 'never' '' 200 | Out-Null } catch { $message = $_.Exception.Message }; if ($message -notlike '*never showed*' -or $script:sets -ne 0) { throw ('missing suggestion: ' + $message) }",
      "$message = $null; try { Run-Late 3 'other.pdf' 'image-source.pdf' 200 | Out-Null } catch { $message = $_.Exception.Message }; if ($message -notlike '*never showed*' -or $script:sets -ne 0) { throw ('wrong suggestion: ' + $message) }",
      "$message = $null; try { Set-BoundNativeSaveFileNameExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -Value $value -DeadlineTickCount ([Environment]::TickCount64 + 30000) -SetProvider {$true} -ReadProvider {''} -SuggestedName 'C:\\x.pdf' } catch { $message = $_.Exception.Message }; if ($message -notlike '*bare file name*') { throw 'path suggestion accepted' }",
    ].join('\n');
    const result = runWithFunctions('installed-print-dialog.ps1', ['Set-BoundNativeSaveFileNameExact'], stubs);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
    expect(result.stdout).toMatch(/NATIVE_SAVE_DIAG t=\d+ phase=suggestion-seen name=suggested/);
    expect(result.stdout).toMatch(/NATIVE_SAVE_DIAG t=\d+ phase=typed-stable name=typed/);
    expect(result.stdout).toMatch(/NATIVE_SAVE_DIAG t=\d+ phase=suggestion-missing name=empty/);
    expect(result.stdout).toMatch(/NATIVE_SAVE_DIAG t=\d+ phase=suggestion-missing name=other:other\.pdf/);
    expect(result.stdout).not.toContain('proof');
  }, 90_000);

  it('re-reads the filename immediately before pressing Save and refuses a replaced name', () => {
    const stubs = [
      "function Get-BoundNativeSaveRoleRecord { param($ProcessId,$Binding,$Role) [pscustomobject]@{ HandleValue = $(if ($Role -ceq 'save') { 601 } else { 505 }) } }",
      "function Get-NativeSurfaceHandleValue { 500 }",
      "function Initialize-PrintNativeWindowInterop { throw 'real interop must not load in this test' }",
      "$binding = [pscustomobject]@{ surfaceRootIdentity = 'hwnd:500|native' }",
      "$script:clicks = 0; $click = { param($p, $t, $r, $b, $d) if ($b -ne 601) { throw 'wrong button' }; $script:clicks++; $true }",
      "Invoke-BoundNativeSaveButtonExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -DeadlineTickCount ([Environment]::TickCount64 + 20000) -ClickProvider $click -ExpectedFileName 'D:\\proof\\created.pdf' -ReadProvider { param($p,$t,$r,$e,$d) if ($e -ne 505) { throw 'wrong edit' }; 'D:\\proof\\created.pdf' }",
      "if ($script:clicks -ne 1) { throw 'matching name not clicked' }",
      "$message = $null; try { Invoke-BoundNativeSaveButtonExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -DeadlineTickCount ([Environment]::TickCount64 + 20000) -ClickProvider $click -ExpectedFileName 'D:\\proof\\created.pdf' -ReadProvider { 'image-source.pdf' } | Out-Null } catch { $message = $_.Exception.Message }",
      "if ($message -notlike '*replaced before the Save button*' -or $script:clicks -ne 1) { throw 'replaced name was clicked' }",
    ].join('\n');
    const result = runWithFunctions('installed-print-dialog.ps1', ['Invoke-BoundNativeSaveButtonExact'], stubs);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
    expect(result.stdout).toContain('NATIVE_SAVE_DIAG phase=pre-click name=typed');
    expect(result.stdout).toContain('NATIVE_SAVE_DIAG phase=pre-click name=replaced');
  }, 90_000);

  it('waits for the app suggestion in every app Save dialog flow and checks the name again before Save', () => {
    const image = readFileSync('scripts/installed-image-page-tools.ps1', 'utf8');
    expect(image).toContain('-Value $OutputPath -DeadlineTickCount $deadlineTick -AwaitSuggestion -SuggestedName $SuggestedName');
    expect(image).toContain('-Binding $surface -DeadlineTickCount $deadlineTick -ExpectedFileName $OutputPath');
    expect(image).toContain("-OutputPath $CreatedPdfPath -Deadline $Deadline -SuggestedName ([IO.Path]::GetFileNameWithoutExtension($SourceImagePath) + '.pdf')");
    const structural = readFileSync('scripts/installed-structural-page-tools.ps1', 'utf8');
    expect(structural).toContain('-DeadlineTickCount $actionDeadline -AwaitSuggestion');
    expect(structural).toContain('-DeadlineTickCount $actionDeadline -ExpectedFileName $OutputPath');
    const forms = readFileSync('scripts/verify-default-comments-forms.ps1', 'utf8');
    expect(forms).toContain('-DeadlineTickCount $tick -AwaitSuggestion');
    expect(forms).toContain('-DeadlineTickCount $tick -ExpectedFileName $OutputPath');
  });

  it('navigates Recovery A with a real React click, because React 18 handles onBlur through focusout and ignores a synthetic blur event', () => {
    const recovery = readFileSync('scripts/verify-default-recovery.ps1', 'utf8');
    expect(recovery).not.toMatch(/new Event\("blur"/);
    expect(recovery).toContain('const n=document.querySelectorAll("button[aria-label=\\"Next page\\"]");if(n.length!==1||n[0].disabled)return false;n[0].click();return true;');
    expect(recovery).toContain("{param($v)[string]$v.page-ceq'2'-and[bool]$v.previousEnabled} 'Recovery A page 2'");
    expect(recovery.indexOf("'Recovery A page 2'")).toBeLessThan(recovery.indexOf('"Organize pages"'));
    expect(recovery).toContain("[string]$v.page-ceq'2'-and[bool]$v.previousEnabled-and[bool]$v.undo-and[int]$v.dialogs-eq0");
    expect(recovery).toContain("[string]$v.page-ceq'2'-and[bool]$v.previousEnabled-and[bool]$v.undo-and(Test-RecoveryQuarterTurnRender");
    const reactDom = readFileSync('node_modules/react-dom/cjs/react-dom.development.js', 'utf8');
    expect(reactDom).toContain("registerSimpleEvent('focusout', 'onBlur');");
    const app = readFileSync('src/App.tsx', 'utf8');
    expect(app).toContain('<input aria-label="Page number" key={`${doc.id}-${page}`} type="number" min={1} max={doc.pages.length} defaultValue={page + 1}');
    expect(app).toContain('label="Previous page" disabled={page === 0}');
  });

  it('writes bounded file diagnostics for a missing image output without a session', () => {
    const root = join(process.cwd(), 'target', `image-diag-${randomUUID()}`);
    mkdirSync(root, { recursive: true });
    try {
      writeFileSync(join(root, 'image-source.png'), 'png');
      writeFileSync(join(root, 'image-source.pdf'), 'pdf!');
      const result = runWithFunctions('installed-image-page-tools.ps1', ['Write-ImagePageOutputDiagnostics'], `Write-ImagePageOutputDiagnostics -Path '${join(root, 'created.pdf').replaceAll("'", "''")}' -Stage 'create-output'`);
      expect(result.status, result.stderr || result.stdout).toBe(0);
      expect(result.stdout).toContain('IMAGE_PAGE_DIAG create-output output=created.pdf exists=false bytes=-1');
      expect(result.stdout).toContain('IMAGE_PAGE_DIAG create-output sibling=image-source.pdf bytes=4');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, 90_000);
});
