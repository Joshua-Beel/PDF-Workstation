import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

const importers = ['verify-default-expanded-page-tools.ps1', 'verify-default-comments-forms.ps1', 'verify-default-recovery.ps1'];

const analysis = String.raw`
param([string]$Importers)
$ErrorActionPreference = 'Stop'
$importerList = @($Importers -split ',')
function Read-Ast([string]$Path) {
  $tokens = $null; $errors = $null
  $ast = [Management.Automation.Language.Parser]::ParseFile((Resolve-Path -LiteralPath $Path).Path, [ref]$tokens, [ref]$errors)
  if ($errors.Count) { throw "Parse error in $Path" }
  return $ast
}
function Get-Definitions($Ast) { @($Ast.FindAll({ param($n) $n -is [Management.Automation.Language.FunctionDefinitionAst] }, $true)) }
function Get-CommandNames($Ast) { @($Ast.FindAll({ param($n) $n -is [Management.Automation.Language.CommandAst] }, $true) | ForEach-Object { $_.GetCommandName() } | Where-Object { $_ }) }
$repoFunctions = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($file in Get-ChildItem -LiteralPath 'scripts' -Recurse -Filter '*.ps1') { foreach ($d in Get-Definitions (Read-Ast $file.FullName)) { $null = $repoFunctions.Add($d.Name) } }
function Add-DotSourced([string]$Path, $Available, $Visited) {
  $full = (Resolve-Path -LiteralPath $Path).Path
  if (-not $Visited.Add($full)) { return }
  $ast = Read-Ast $full
  foreach ($d in Get-Definitions $ast) { $null = $Available.Add($d.Name) }
  foreach ($m in [regex]::Matches($ast.Extent.Text, "(?m)^\s*\.\s+\(Join-Path\s+\`$PSScriptRoot\s+'([^']+\.ps1)'\)")) { Add-DotSourced (Join-Path (Split-Path -Parent $full) $m.Groups[1].Value) $Available $Visited }
}
$problems = @()
foreach ($importer in $importerList) {
  $path = Join-Path 'scripts' $importer
  $ast = Read-Ast $path
  $available = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
  Add-DotSourced $path $available ([Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase))
  $needed = [Collections.Generic.List[object]]::new()
  foreach ($name in Get-CommandNames $ast) { $needed.Add([pscustomobject]@{ Name = $name; From = $importer }) }
  $imports = [regex]::Matches($ast.Extent.Text, "(?m)^Import-[A-Za-z]+Functions\s+\(Join-Path\s+\`$PSScriptRoot\s+'([^']+\.ps1)'\)\s+@\(([^)]*)\)")
  if ($imports.Count -eq 0) { $problems += "$importer imports nothing (pattern changed?)" }
  foreach ($import in $imports) {
    $source = Join-Path 'scripts' $import.Groups[1].Value
    $sourceAst = Read-Ast $source
    $names = @([regex]::Matches($import.Groups[2].Value, "'([^']+)'") | ForEach-Object { $_.Groups[1].Value })
    foreach ($name in $names) {
      $null = $available.Add($name)
      $definition = @(Get-Definitions $sourceAst | Where-Object { $_.Name -ceq $name })
      if ($definition.Count -ne 1) { $problems += "$importer imports $name, which $($import.Groups[1].Value) does not define exactly once"; continue }
      foreach ($called in Get-CommandNames $definition[0].Body) { $needed.Add([pscustomobject]@{ Name = $called; From = "$name ($($import.Groups[1].Value))" }) }
    }
  }
  foreach ($item in $needed) {
    if ($repoFunctions.Contains($item.Name) -and -not $available.Contains($item.Name)) { $problems += "$importer cannot resolve $($item.Name), called by $($item.From)" }
  }
}
$problems | Sort-Object -Unique | ForEach-Object { Write-Output "MISSING $_" }
Write-Output 'ANALYSIS DONE'
`;

function runAnalysis(files: string[]) {
  const root = join('target', `verifier-imports-${randomUUID()}`);
  mkdirSync(root, { recursive: true });
  const path = join(root, 'analyse.ps1');
  try {
    writeFileSync(path, analysis, 'utf8');
    return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path, '-Importers', files.join(',')], { encoding: 'utf8', timeout: 90_000 });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

describe('installed-app verifier helper imports', () => {
  it('every repository function a default verifier or one of its imported helpers calls is defined, dot-sourced or imported', () => {
    const result = runAnalysis(importers);
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toContain('ANALYSIS DONE');
    expect(result.stdout.split(/\r?\n/).filter(line => line.startsWith('MISSING '))).toEqual([]);
  }, 120_000);

  it('finds a missing helper import (negative control)', () => {
    const source = readFileSync('scripts/verify-default-expanded-page-tools.ps1', 'utf8');
    const name = `zz-negative-control-${randomUUID()}.ps1`;
    const root = join('scripts', name);
    try {
      const broken = source.replace(",'Get-UniqueResource','Convert-RegistryFilePath')", ",'Get-UniqueResource')");
      expect(broken).not.toBe(source);
      writeFileSync(root, broken, 'utf8');
      const result = runAnalysis([name]);
      expect(result.status, result.stderr).toBe(0);
      expect(result.stdout).toMatch(/MISSING .* cannot resolve Convert-RegistryFilePath, called by Get-InstallFacts/);
    } finally {
      rmSync(root, { force: true });
    }
  }, 120_000);

  it('clicks only the toolbar Open control, because a fresh profile also shows an empty-state Open button', () => {
    const app = readFileSync('src/App.tsx', 'utf8');
    const toolbar = app.slice(app.indexOf('<div className={s.globalActions}>'), app.indexOf('</div>', app.indexOf('<div className={s.globalActions}>')));
    expect(toolbar).toContain('label="Toggle theme"');
    expect(toolbar).toContain("'Open a file'");
    expect(app).toMatch(/<button className=\{s\.openButton\} onClick=\{\(\) => void open\(\)\} disabled=\{busy\}>Open a file<\/button>/);
    const recovery = readFileSync('scripts/verify-default-recovery.ps1', 'utf8');
    const scoped = 'textContent.trim()==="Open a file"&&!x.disabled&&x.parentElement?.querySelector("button[aria-label=\\"Toggle theme\\"]")';
    expect(recovery.split(scoped).length - 1).toBe(2);
    expect(recovery).not.toMatch(/textContent\.trim\(\)==="Open a file"&&!x\.disabled\)/);
    expect(recovery.indexOf("'Recovery toolbar Open control'")).toBeLessThan(recovery.indexOf('Get-ReadingProcessUiSurfaceSnapshot -ApplicationProcessId $appPid'));
  });
});
