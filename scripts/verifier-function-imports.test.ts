import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

const defaultWorkflows = [
  '.github/workflows/default-draft-verification.yml',
  '.github/workflows/default-expanded-page-tools-verification.yml',
  '.github/workflows/default-comments-forms-verification.yml',
  '.github/workflows/default-recovery-verification.yml',
];

function analyse(extraEntries: string[] = [], workflows = defaultWorkflows) {
  const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', 'scripts/verifier-reachability.ps1', '-Workflows', workflows.join(','), '-ExtraEntries', extraEntries.join(',')], { encoding: 'utf8', timeout: 120_000 });
  const lines = result.stdout.split(/\r?\n/);
  return { result, problems: lines.filter(line => line.startsWith('PROBLEM ')), entries: lines.find(line => line.startsWith('ENTRIES ')) ?? '' };
}

function withTemporaryScript(content: string, run: (name: string) => void) {
  const name = `zz-reachability-control-${randomUUID()}.ps1`;
  const path = join('scripts', name);
  try {
    writeFileSync(path, content, 'utf8');
    run(name);
  } finally {
    rmSync(path, { force: true });
  }
}

describe('installed-app verifier reachability', () => {
  it('resolves every repository function reachable from the four default verifier workflows, transitively, and has no blind closures', () => {
    const { result, problems, entries } = analyse();
    expect(result.status, result.stderr).toBe(0);
    expect(result.stdout).toContain('ANALYSIS DONE');
    expect(problems).toEqual([]);
    for (const script of ['verify-default-draft.ps1', 'verify-default-expanded-page-tools.ps1', 'verify-default-comments-forms.ps1', 'verify-default-recovery.ps1', 'assert-updater-signing-key.ps1', 'setup-installed-app-webdriver.ps1', 'prepare-default-draft-pdfium.ps1']) {
      expect(entries).toContain(script);
    }
  }, 150_000);

  it('reports a helper that an imported helper calls but nobody imports (negative control)', () => {
    const source = readFileSync('scripts/verify-default-expanded-page-tools.ps1', 'utf8');
    const broken = source.replace(",'Get-UniqueResource','Convert-RegistryFilePath')", ",'Get-UniqueResource')");
    expect(broken).not.toBe(source);
    withTemporaryScript(broken, name => {
      const { result, problems } = analyse([name], []);
      expect(result.status, result.stderr).toBe(0);
      expect(problems.join('\n')).toMatch(/cannot resolve Convert-RegistryFilePath, called by Get-InstallFacts in verify-signed-ocr-upgrade\.ps1/);
    });
  }, 150_000);

  it('reports a GetNewClosure() callback that calls script functions (negative control)', () => {
    const control = [
      ". (Join-Path $PSScriptRoot 'installed-image-page-tools.ps1')",
      "function Invoke-ControlCallback { $callback = { param($c) Wait-ImagePageWebDriverOracle -SessionId 'x' }.GetNewClosure(); & $callback 1 }",
    ].join('\n');
    withTemporaryScript(control, name => {
      const { result, problems } = analyse([name], []);
      expect(result.status, result.stderr).toBe(0);
      expect(problems.join('\n')).toContain(`${name}:2 GetNewClosure() scriptblock calls script functions`);
      expect(problems.join('\n')).toContain('Wait-ImagePageWebDriverOracle');
    });
  }, 150_000);

  it('reproduces why GetNewClosure() breaks under the hosted runner invocation while a plain callback works', () => {
    const root = join('target', `closure-scope-${randomUUID()}`);
    mkdirSync(root, { recursive: true });
    try {
      writeFileSync(join(root, 'helper.ps1'), [
        'function Inner-Helper { "inner" }',
        'function Run-Callback { param([scriptblock]$Callback) & $Callback }',
        'function Use-Closure { param($Value) Run-Callback ({ "$Value " + (Inner-Helper) }.GetNewClosure()) }',
        'function Use-Plain { param($Value) Run-Callback { "$Value " + (Inner-Helper) } }',
      ].join('\n'), 'utf8');
      writeFileSync(join(root, 'entry.ps1'), [
        "$ErrorActionPreference = 'Stop'",
        ". (Join-Path $PSScriptRoot 'helper.ps1')",
        'try { Use-Closure "closure" } catch { "closure failed" }',
        'Use-Plain "plain"',
      ].join('\n'), 'utf8');
      writeFileSync(join(root, 'runner.ps1'), `& '${join(process.cwd(), root, 'entry.ps1').replaceAll("'", "''")}'`, 'utf8');
      const runner = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', `. '${join(process.cwd(), root, 'runner.ps1').replaceAll("'", "''")}'`], { encoding: 'utf8', timeout: 60_000 });
      expect(runner.status, runner.stderr).toBe(0);
      expect(runner.stdout).toContain('closure failed');
      expect(runner.stdout).toContain('plain inner');
    } finally {
      rmSync(root, { recursive: true, force: true });
    }
  }, 90_000);

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
    expect(recovery).toContain("[Console]::Out.WriteLine(\"RECOVERY_STAGE oracle: $Kind\")");
  });

  it('accepts a failed session delete only after the callback reports an intentional abrupt termination', () => {
    const helper = readFileSync('scripts/installed-image-page-tools.ps1', 'utf8');
    expect(helper).toContain("if ($sessionDeleteOutcome -cne 'verified' -and $null -ne $result -and $null -ne $result.PSObject.Properties['abruptTermination'] -and $result.abruptTermination -is [bool] -and $result.abruptTermination) { $assertedDeleteOutcome = 'verified' }");
    expect(helper).toContain('Assert-LaunchCleanupState -Result $result -SessionDeleteOutcome $assertedDeleteOutcome');
    expect(helper).toContain("$result | Add-Member sessionDeleted ($sessionDeleteOutcome -ceq 'verified')");
    expect(readFileSync('scripts/verify-default-recovery.ps1', 'utf8')).toContain('abruptTermination=$killed');
  });
});
