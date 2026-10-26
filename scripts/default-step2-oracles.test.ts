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
      "foreach ($shown in @('image-source', 'Image-Source', 'IMAGE-SOURCE.PDF')) { Run-Late 4 $shown 'image-source.pdf' | Out-Null; if ($script:typedBeforeSuggestion -or $script:sets -ne 1 -or $script:edit -cne $value) { throw ('hidden-extension suggestion refused: ' + $shown) } }",
      "foreach ($near in @('image-source.png', 'image-sources', 'image-source.pdf.pdf', 'image')) { $message = $null; try { Run-Late 2 $near 'image-source.pdf' 200 | Out-Null } catch { $message = $_.Exception.Message }; if ($message -notlike '*never showed*' -or $script:sets -ne 0) { throw ('near-miss suggestion accepted: ' + $near) } }",
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

  it('accepts the app suggestion where it is deterministic, types like a keyboard where a specific name is needed, and binds outputs to the app source', () => {
    const image = readFileSync('scripts/installed-image-page-tools.ps1', 'utf8');
    expect(image).toContain('-Value $OutputPath -DeadlineTickCount $deadlineTick -AwaitSuggestion -SuggestedName $SuggestedName -AcceptSuggestion');
    expect(image).toContain('-Binding $surface -DeadlineTickCount $deadlineTick -ExpectedSuggestion $SuggestedName');
    expect(image).toContain("-OutputPath $CreatedPdfPath -Deadline $Deadline -SuggestedName ([IO.Path]::GetFileNameWithoutExtension($SourceImagePath) + '.pdf')");
    expect(image).toContain("-OutputPath $ExportedPngPath -Deadline $Deadline -SuggestedName ([IO.Path]::GetFileNameWithoutExtension($ExportSourcePdfPath) + '-page-1.png')");
    expect(image).toContain("throw 'Image page-tools outputs must be the application''s suggested names in the source image folder.'");
    const structural = readFileSync('scripts/installed-structural-page-tools.ps1', 'utf8');
    expect(structural).toContain('-DeadlineTickCount $actionDeadline -AwaitSuggestion -SuggestedName ([IO.Path]::GetFileName($OutputPath)) -AcceptSuggestion');
    expect(structural).toContain('-DeadlineTickCount $actionDeadline -ExpectedSuggestion ([IO.Path]::GetFileName($OutputPath))');
    expect(structural).toContain("-cne 'combined.pdf' -or [IO.Path]::GetFileName($InsertOutputPath) -cne 'inserted-pages.pdf' -or [IO.Path]::GetFileName($ReplaceOutputPath) -cne 'replaced-pages.pdf'");
    const forms = readFileSync('scripts/verify-default-comments-forms.ps1', 'utf8');
    expect(forms).toContain('-DeadlineTickCount $tick -AwaitSuggestion -Transport keyboard');
    expect(forms).toContain('-DeadlineTickCount $tick -ExpectedFileName $OutputPath');
    const main = readFileSync('src-tauri/src/main.rs', 'utf8');
    for (const name of ['combined.pdf', 'inserted-pages.pdf', 'replaced-pages.pdf']) expect(main).toContain(`.set_file_name("${name}")`);
    expect(main).toContain('let suggested = format!("{}.pdf", source.file_stem().unwrap_or_default().to_string_lossy());');
    expect(readFileSync('src-tauri/src/service.rs', 'utf8')).toContain('Ok(PageImagePreflight { suggested_name: format!("{stem}-page-{}.{}", usize::from(page) + 1, format.extension()) })');
    for (const verifier of ['scripts/verify-default-expanded-page-tools.ps1', 'scripts/verify-signed-expanded-page-tools.ps1']) {
      const text = readFileSync(verifier, 'utf8');
      for (const name of ['image-source.pdf', 'image-source-page-1.png', 'combined.pdf', 'inserted-pages.pdf', 'replaced-pages.pdf']) expect(text, verifier).toContain(`(Join-Path $work '${name}')`);
    }
  });

  it('accepts a settled suggestion without typing only in the expected folder, and the Save pre-check accepts the suggestion with its extension hidden', () => {
    const stubs = [
      "function Get-BoundNativeSaveRoleRecord { param($ProcessId,$Binding,$Role) [pscustomobject]@{ HandleValue = $(if ($Role -ceq 'save') { 601 } else { 505 }) } }",
      "function Get-NativeSurfaceHandleValue { 500 }",
      "function Initialize-PrintNativeWindowInterop { throw 'real interop must not load in this test' }",
      "$binding = [pscustomobject]@{ surfaceRootIdentity = 'hwnd:500|native' }",
      "$value = 'D:\\work\\image-source.pdf'",
      "function Run-Accept([string]$Shown, [string]$Folder, [string]$Output = $value) {",
      "  $script:sets = 0",
      "  $set = { param($p, $t, $r, $e, $v, $d) $script:sets++; $true }",
      "  $read = { param($p, $t, $r, $e, $d) $Shown }",
      "  $folderRead = { param($p, $t, $r, $d) $Folder }",
      "  Set-BoundNativeSaveFileNameExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -Value $Output -DeadlineTickCount ([Environment]::TickCount64 + 30000) -SetProvider $set -ReadProvider $read -FolderProvider $folderRead -SettleMilliseconds 5 -AwaitSuggestion -SuggestedName 'image-source.pdf' -AcceptSuggestion -SuggestionTimeoutMilliseconds 300",
      "}",
      "Run-Accept 'image-source' 'D:\\work'; if ($script:sets -ne 0) { throw 'accepting a suggestion typed a name' }",
      "Run-Accept 'image-source.pdf' 'D:\\work\\'; if ($script:sets -ne 0) { throw 'trailing slash folder' }",
      "Run-Accept 'image-source' ''; if ($script:sets -ne 0) { throw 'unavailable folder' }",
      "$message = $null; try { Run-Accept 'image-source' 'D:\\elsewhere' } catch { $message = $_.Exception.Message }; if ($message -notlike '*not in the expected output folder*') { throw 'folder mismatch accepted' }",
      "$message = $null; try { Run-Accept 'other.pdf' 'D:\\work' } catch { $message = $_.Exception.Message }; if ($message -notlike '*never showed*') { throw 'wrong suggestion accepted' }",
      "$message = $null; try { Run-Accept 'image-source' 'D:\\work' 'D:\\work\\created.pdf' } catch { $message = $_.Exception.Message }; if ($message -notlike '*named exactly as the suggestion*') { throw 'mismatched output accepted' }",
      "$script:clicks = 0; $click = { param($p, $t, $r, $b, $d) $script:clicks++; $true }",
      "foreach ($shown in @('image-source', 'IMAGE-SOURCE.pdf')) { Invoke-BoundNativeSaveButtonExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -DeadlineTickCount ([Environment]::TickCount64 + 20000) -ClickProvider $click -ExpectedSuggestion 'image-source.pdf' -ReadProvider ([scriptblock]::Create(\"'\" + $shown + \"'\")) }",
      "if ($script:clicks -ne 2) { throw 'suggestion pre-check refused' }",
      "$message = $null; try { Invoke-BoundNativeSaveButtonExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -DeadlineTickCount ([Environment]::TickCount64 + 20000) -ClickProvider $click -ExpectedSuggestion 'image-source.pdf' -ReadProvider { 'created.pdf' } } catch { $message = $_.Exception.Message }; if ($message -notlike '*replaced before*' -or $script:clicks -ne 2) { throw 'changed suggestion clicked' }",
      "$message = $null; try { Invoke-BoundNativeSaveButtonExact -ProcessId 7319 -ProcessStartUtcTicks 638500000000000000L -Binding $binding -DeadlineTickCount ([Environment]::TickCount64 + 20000) -ClickProvider $click -ExpectedSuggestion 'a.pdf' -ExpectedFileName 'b.pdf' -ReadProvider { 'a' } } catch { $message = $_.Exception.Message }; if ($message -notlike '*not both*') { throw 'both checks accepted' }",
    ].join('\n');
    const result = runWithFunctions('installed-print-dialog.ps1', ['Set-BoundNativeSaveFileNameExact', 'Invoke-BoundNativeSaveButtonExact'], stubs);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
    expect(result.stdout).toMatch(/phase=dialog-folder state=match/);
    expect(result.stdout).toMatch(/phase=dialog-folder state=unavailable/);
    expect(result.stdout).toMatch(/phase=dialog-folder state=mismatch/);
    expect(result.stdout).toMatch(/phase=accepted-suggestion name=suggested/);
    expect(result.stdout).toContain('NATIVE_SAVE_DIAG phase=pre-click name=suggested');
    expect(result.stdout).not.toMatch(/work|elsewhere/);
  }, 90_000);

  it('types a filename as keyboard characters through the bounded native helper', () => {
    const source = readFileSync('scripts/installed-print-dialog.ps1', 'utf8');
    const typed = source.slice(source.indexOf('public static bool TypeSaveFileNameExact'), source.indexOf('public static string ReadSaveFolderBounded'));
    expect(typed).toContain('Call(edit, EM_SETSEL, IntPtr.Zero, new IntPtr(-1), deadline);');
    expect(typed).toContain('Call(edit, WM_CHAR, new IntPtr(character), new IntPtr(1), deadline);');
    expect(typed).toContain('if (character < 0x20 || character == 0x7F) throw');
    expect(typed).toContain('ValidateSaveFileNameEdit(processId, root, edit);');
    expect(typed).toContain('was not exactly revalidated after keyboard input');
    expect(typed).not.toContain('WM_SETTEXT');
    expect(source).toContain("elseif ($Transport -ceq 'keyboard') {\n            [Smacrobat.PrintVerification.NativeWindows]::TypeSaveFileNameExact(".replace(/\n/g, source.includes('\r\n') ? '\r\n' : '\n'));
    const folder = source.slice(source.indexOf('public static string ReadSaveFolderBounded'), source.indexOf('public static string ReadSaveFileNameBounded'));
    expect(folder).toContain('GetDlgCtrlID(hwnd) == 1001 && ClassOf(GetParent(hwnd)).Equals("Breadcrumb Parent", StringComparison.Ordinal)');
    expect(folder).toContain('if (crumbs.Count != 1) return "";');
  });

  it('retries the pinned PDFium metadata and archive fetches a bounded number of times and still verifies them', () => {
    const result = runWithFunctions('prepare-default-draft-pdfium.ps1', ['Invoke-PdfiumFetchWithRetry'], [
      "$script:delays = @(); $sleep = { param($s) $script:delays += $s }",
      "$script:calls = 0; $value = Invoke-PdfiumFetchWithRetry -Kind 'metadata' -SleepProvider $sleep -Action { $script:calls++; if ($script:calls -lt 3) { throw 'Pinned PDFium metadata request failed with HTTP status 503.' }; 'ok' }",
      "if ($value -cne 'ok' -or $script:calls -ne 3 -or ($script:delays -join ',') -cne '5,15') { throw ('retry: ' + $script:calls + ' ' + ($script:delays -join ',')) }",
      "$script:calls = 0; $script:delays = @(); $message = $null; try { Invoke-PdfiumFetchWithRetry -Kind 'download' -SleepProvider $sleep -Action { $script:calls++; throw 'Pinned PDFium download failed with HTTP status 502.' } } catch { $message = $_.Exception.Message }",
      "if ($message -notlike '*HTTP status 502*' -or $script:calls -ne 3) { throw 'exhausted retry' }",
      "$script:calls = 0; $first = Invoke-PdfiumFetchWithRetry -Kind 'metadata' -SleepProvider $sleep -Action { $script:calls++; 'first' }; if ($first -cne 'first' -or $script:calls -ne 1) { throw 'success retried' }",
      "$message = $null; try { Invoke-PdfiumFetchWithRetry -Kind 'x' -DelaySeconds @(0,0,0,0,0,0) -Action { 1 } } catch { $message = $_.Exception.Message }; if ($message -notlike '*out of bounds*') { throw 'unbounded retries' }",
      "$message = $null; try { Invoke-PdfiumFetchWithRetry -Kind 'x' -DelaySeconds @(0,600) -SleepProvider $sleep -Action { throw 'boom' } } catch { $message = $_.Exception.Message }; if ($message -notlike '*out of bounds*') { throw 'unbounded delay' }",
    ].join('\n'));
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
    expect(result.stdout).toContain('PDFIUM_FETCH_DIAG kind=metadata attempt=1 failed=Pinned PDFium metadata request failed with HTTP status 503.');
    expect(result.stdout).toContain('PDFIUM_FETCH_DIAG kind=download attempt=3 failed=Pinned PDFium download failed with HTTP status 502.');
    const script = readFileSync('scripts/prepare-default-draft-pdfium.ps1', 'utf8');
    expect(script).toContain("$metadata=Invoke-PdfiumFetchWithRetry -Kind 'metadata' -Action {");
    expect(script).toContain("$null=Invoke-PdfiumFetchWithRetry -Kind 'download' -Action {");
    expect(script.indexOf("Invoke-PdfiumFetchWithRetry -Kind 'metadata'")).toBeLessThan(script.indexOf('Assert-PdfiumReleaseMetadata $metadata'));
    expect(script).toContain("if((Get-FileHash -Algorithm SHA256 -LiteralPath $archive).Hash-cne$assetSha256){throw 'Pinned PDFium archive hash differs from its receipt.'}");
    expect(script).toContain("if($total-ne$assetBytes){throw 'Pinned PDFium download ended before its receipt.'}");
  }, 90_000);

  it('navigates Recovery A with a real React click, because React 18 handles onBlur through focusout and ignores a synthetic blur event', () => {
    const recovery = readFileSync('scripts/verify-default-recovery.ps1', 'utf8');
    expect(recovery).not.toMatch(/new Event\("blur"/);
    expect(recovery).toContain('const n=document.querySelectorAll("button[aria-label=\\"Next page\\"]");if(n.length!==1||n[0].disabled)return false;n[0].click();return true;');
    expect(recovery).toContain("{param($v)[string]$v.page-ceq'2'-and[bool]$v.previousEnabled} 'Recovery A page 2'");
    expect(recovery.indexOf("'Recovery A page 2'")).toBeLessThan(recovery.indexOf('"Organize pages"'));
    expect(recovery).toContain("[string]$v.page-ceq'2'-and[bool]$v.previousEnabled-and[bool]$v.undo-and[int]$v.dialogs-eq0");
    expect(recovery).not.toContain('"Select page 1\\"]").click();document.querySelector("button[aria-label=\\"Rotate');
    expect(recovery).toContain("{param($v)[string]$v.selected-ceq'Select page 1'} 'Recovery A page 1 selected'");
    expect(recovery.indexOf("'Recovery A page 1 selected'")).toBeLessThan(recovery.indexOf('Rotate selected pages clockwise'));
    expect(recovery).toContain("-and[int]$v.p1w-gt[int]$v.p1h-and[int]$v.p1h-gt0-and[int]$v.p2h-gt[int]$v.p2w-and[int]$v.p2w-gt0} 'Recovery A acknowledged edit'");
    const organizer = readFileSync('src/Organizer.tsx', 'utf8');
    expect(organizer).toContain('useState<number[]>(() => [clampPage(currentPage, document.pages.length)])');
    expect(organizer).toContain("onClick={() => void edit({ kind: 'rotate', pages: selected, clockwise: true })}");
    expect(organizer).toContain('alt={`Page ${index + 1} preview`}');
    expect(recovery).toContain("[string]$v.page-ceq'2'-and[bool]$v.previousEnabled-and[bool]$v.undo-and(Test-RecoveryQuarterTurnRender");
    const reactDom = readFileSync('node_modules/react-dom/cjs/react-dom.development.js', 'utf8');
    expect(reactDom).toContain("registerSimpleEvent('focusout', 'onBlur');");
    const app = readFileSync('src/App.tsx', 'utf8');
    expect(app).toContain('<input aria-label="Page number" key={`${doc.id}-${page}`} type="number" min={1} max={doc.pages.length} defaultValue={page + 1}');
    expect(app).toContain('label="Previous page" disabled={page === 0}');
  });

  it('checks the full signed PDFium base-resource receipt in the structural stage instead of a bare bytes and SHA-256 pair', () => {
    const root = join(process.cwd(), 'target', `structural-pdfium-${randomUUID()}`);
    const bin = join(root, 'resources', 'pdfium', 'bin');
    mkdirSync(bin, { recursive: true });
    try {
      writeFileSync(join(bin, 'pdfium.dll'), 'signed pdfium bytes');
      writeFileSync(join(root, 'other.dll'), 'signed pdfium bytes');
      const dll = join(bin, 'pdfium.dll').replaceAll("'", "''");
      const other = join(root, 'other.dll').replaceAll("'", "''");
      const result = runWithFunctions('installed-structural-page-tools.ps1', ['Assert-StructuralExactProperties', 'Assert-StructuralFileReceipt', 'Assert-StructuralPdfiumReceipt'], [
        ". ./scripts/ocr/installer-package.ps1",
        `$dll='${dll}';$other='${other}';$sha=Get-ExactSha256 -Path $dll;$bytes=[uint64](Get-Item -LiteralPath $dll).Length`,
        "function New-Entry { [pscustomobject][ordered]@{ target='resources/pdfium/bin/pdfium.dll'; bytes=$bytes; sha256=$sha; unsignedBytes=[uint64]7; unsignedSha256=('F'*64) } }",
        "function Reject([scriptblock]$Action,[string]$Kind){$bad=$false;try{&$Action}catch{$bad=$true};if(-not$bad){throw ($Kind+' accepted')}}",
        "Assert-StructuralPdfiumReceipt -Path $dll -Receipt (New-Entry) -Kind 'PDFium'",
        "Reject { Assert-StructuralPdfiumReceipt -Path $dll -Receipt ([pscustomobject]@{bytes=$bytes;sha256=$sha}) -Kind 'PDFium' } 'bare receipt'",
        "$e=New-Entry;$e.target='resources/pdfium/LICENSE';Reject { Assert-StructuralPdfiumReceipt -Path $dll -Receipt $e -Kind 'PDFium' } 'other target'",
        "$e=New-Entry;$e.unsignedBytes=[uint64]0;Reject { Assert-StructuralPdfiumReceipt -Path $dll -Receipt $e -Kind 'PDFium' } 'zero unsigned bytes'",
        "$e=New-Entry;$e.unsignedSha256='abc';Reject { Assert-StructuralPdfiumReceipt -Path $dll -Receipt $e -Kind 'PDFium' } 'malformed unsigned hash'",
        "$e=New-Entry;$e.sha256=('A'*64);Reject { Assert-StructuralPdfiumReceipt -Path $dll -Receipt $e -Kind 'PDFium' } 'changed file'",
        "$e=New-Entry|Select-Object *,@{n='extra';e={1}};Reject { Assert-StructuralPdfiumReceipt -Path $dll -Receipt $e -Kind 'PDFium' } 'extra property'",
        "Reject { Assert-StructuralPdfiumReceipt -Path $other -Receipt (New-Entry) -Kind 'PDFium' } 'other path'",
      ].join('\n'));
      expect(result.status, result.stderr || result.stdout).toBe(0);
      expect(result.stdout).toContain('PASS');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
    const structural = readFileSync('scripts/installed-structural-page-tools.ps1', 'utf8');
    expect(structural).toContain("Assert-StructuralPdfiumReceipt -Path $PdfiumPath -Receipt $PdfiumReceipt -Kind 'Installed signed structural page-tools PDFium'");
    expect(structural).toContain("Assert-StructuralPdfiumReceipt -Path $PdfiumPath -Receipt $PdfiumReceipt -Kind 'Final installed signed structural page-tools PDFium'");
    expect(structural).toContain('-Receipt ([pscustomobject][ordered]@{ bytes = [uint64]$receipt.tauriDriver.bytes; sha256 = [string]$receipt.tauriDriver.sha256 })');
    expect(structural).toContain('-Receipt ([pscustomobject][ordered]@{ bytes = [uint64]$receipt.edgeDriver.bytes; sha256 = [string]$receipt.edgeDriver.sha256 })');
    expect(structural).not.toMatch(/-Receipt \$receipt\.(tauriDriver|edgeDriver) /);
    expect(structural.indexOf('Assert-WebDriverReceipt -Receipt $receipt')).toBeLessThan(structural.indexOf('$receipt.tauriDriver.bytes'));
    const launch = readFileSync('scripts/installed-app-launch.ps1', 'utf8');
    expect(launch).toContain("Assert-LaunchExactProperties -Value $Receipt.tauriDriver -Expected @('version','cargoVersion','rustcVersion','bytes','sha256')");
    expect(launch).toContain("Assert-LaunchExactProperties -Value $Receipt.edgeDriver -Expected @('version','bytes','sha256','signatureStatus','publisher','hasTimestamp')");
    expect(readFileSync('scripts/verify-default-expanded-page-tools.ps1', 'utf8')).toContain("Assert-StructuralPdfiumReceipt -Path $resources.Pdfium.FullName -Receipt $sets.Pdfium -Kind 'Final preserved PDFium'");
    expect(readFileSync('scripts/verify-signed-expanded-page-tools.ps1', 'utf8')).toContain("Assert-StructuralPdfiumReceipt -Path $resources.Pdfium.FullName -Receipt $receiptSets.Pdfium -Kind 'Final installed PDFium'");
    expect(readFileSync('scripts/verify-signed-ocr-upgrade.ps1', 'utf8')).toContain("@('target','bytes','sha256','unsignedBytes','unsignedSha256')");
  }, 90_000);

  it('writes the settings sentinel under the exact name the organizer and structural helpers require', () => {
    const expanded = readFileSync('scripts/verify-default-expanded-page-tools.ps1', 'utf8');
    expect(expanded).toContain("$sentinel=Join-Path $settingsRoot 'upgrade-sentinel.json'");
    for (const helper of ['scripts/installed-organizer-tools.ps1', 'scripts/installed-structural-page-tools.ps1']) {
      expect(readFileSync(helper, 'utf8'), helper).toContain("$settingsEntries[0].Name -cne 'upgrade-sentinel.json'");
    }
    expect(readFileSync('scripts/verify-signed-expanded-page-tools.ps1', 'utf8')).toContain("$settingsSentinel = Join-Path $settingsRoot 'upgrade-sentinel.json'");
  });

  it('binds the split folder picker by its Folder box and submits the existing parent, matching the app split contract', () => {
    const reading = readFileSync('scripts/installed-reading-tools.ps1', 'utf8');
    expect(reading).toContain("$className -ceq 'Edit' -and $controlId -in @(1001,1148,1152)");
    expect(reading).toContain("'edit-1152' { 1152 } default { throw 'The native picker filename control category is unknown.' }");
    const organizer = readFileSync('scripts/installed-organizer-tools.ps1', 'utf8');
    expect(organizer).toContain("SplitFolderName = 'Split PDFs'");
    expect(organizer).toContain('-Path ([IO.Path]::GetDirectoryName([IO.Path]::GetFullPath($TargetPath))) -Deadline $Deadline');
    expect(organizer).toContain("splitFilenameControlCategory -cne 'edit-1152'");
    expect(organizer).toContain("openFilenameControlCategory -notin @('edit-1001','edit-1148')");
    expect(readFileSync('scripts/verify-signed-expanded-page-tools.ps1', 'utf8')).toContain("splitFilenameControlCategory -cne 'edit-1152'");
    expect(reading).toContain("[ValidateSet('edit-1001','edit-1148')][string]$NativeControlCategory");
    const split = readFileSync('src-tauri/src/split.rs', 'utf8');
    expect(split).toContain('const SPLIT_FOLDER_NAME: &str = "Split PDFs";');
    expect(split).toContain('FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST');
    expect(split).toContain('let name = format!("pages-{:04}-{:04}.pdf", start + 1, end);');
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
