import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

const path = 'scripts/verify-native-annotate-dialogs.ps1';

describe('native annotate dialog verifier', () => {
  it('parses and never injects keyboard or mouse input', () => {
    const script = readFileSync(path, 'utf8');
    expect(script).not.toMatch(/SendKeys|SendInput|mouse_event|SetCursorPos|keybd_event|PostMessage|SendMessage/);
    const probe = `$e=$null;$null=[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\\${path.replaceAll('/', '\\')}',[ref]$null,[ref]$e);if($e.Count){exit 1}`;
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', probe], { encoding: 'utf8', timeout: 60_000 });
    expect(result.status).toBe(0);
  });

  it('confines every path: tmp prefix with a trailing separator, no UNC, repo or shared target for binaries', () => {
    const script = readFileSync(path, 'utf8');
    expect(script).not.toContain('$env:TEMP');
    expect(script).not.toContain('Remove-Item -LiteralPath $dir -Recurse');
    expect(script).not.toMatch(/Remove-Item[^\n]*-Recurse/);
    expect(script).toContain("if ($full.StartsWith('" + '\\\\' + "')) { return $false }");
    expect(script).toContain('[char]92) + [char]92');
    expect(script).toContain('WorkRoot must be a folder under the sibling tmp folder');
    expect(script).toContain('sm3-dialogs' + '\\' + 'run-');
    expect(script).toContain('FixtureDir must be under the sibling tmp folder or the repo');
    expect(script).toContain('AppPath must be under the repo or the shared cargo target');
    expect(script).toContain('PdfiumPath must be under the repo');
    expect(script).toContain("EnvironmentVariables['WEBVIEW2_USER_DATA_FOLDER'] = (Join-Path $script:RunDir 'webview')");
  });

  it('drives only its own process and aborts when another dev build appears', () => {
    const script = readFileSync(path, 'utf8');
    expect(script).toContain('$AE::ProcessIdProperty, $script:AppPid');
    expect(script).toContain('if ($element.Current.ProcessId -eq $script:AppPid)');
    expect(script).toContain('function Assert-AppPid');
    expect(script).toContain('$script:AppPid -le 4');
    expect(script).toContain('SM3_FOREIGN_APP');
    expect(script).toContain('function Get-OtherDevApps');
    expect(script).toContain('$script:Process.Kill($true)');
  });

  it('waits at most 30 minutes, supports -Strict, and Cancel is asserted to write no file', () => {
    const script = readFileSync(path, 'utf8');
    expect(script).toContain('[int]$WaitMinutes = 30');
    expect(script).toContain('WaitMinutes must be between 1 and 30');
    expect(script).toContain('[switch]$Strict');
    expect(script).toMatch(/if \(\$Strict -and \$notAutomatable\.Count -gt 0\) \{[^}]*exit 2/);
    expect(script).toContain('Cancel leaves the app unchanged (same tabs, no output written');
  });

  it('snapshots and refuses on foreign recovery records, closes only its own tabs and removes only its own records', () => {
    const script = readFileSync(path, 'utf8');
    expect(script).toContain('Refusing to run:');
    expect(script).toContain('local.pdfworkstation.desktop' + '\\' + 'recovery');
    expect(script).toContain('$recoverySnapshot -notcontains $_');
    expect(script).toContain('function Close-OwnDocuments');
    expect(script).toContain("@($script:OpenedByRun.Keys)");
    expect(script).not.toContain('Close *.pdf');
    expect(script).toContain('$tracked -ceq $full');
    expect(script).toContain("-Filter '*.smacrec'");
    expect(script).toContain('.EndsWith(\'.smacrec\'');
  });
});
