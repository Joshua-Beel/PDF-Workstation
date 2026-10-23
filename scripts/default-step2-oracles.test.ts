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
