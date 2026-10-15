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

  it('refuses the system drive, drives only its own process and aborts when another app instance appears', () => {
    const script = readFileSync(path, 'utf8');
    expect(script).toContain('WorkRoot must not be on the system drive');
    expect(script).toContain("$AE::ProcessIdProperty, $script:AppPid");
    expect(script).toContain('SM3_FOREIGN_APP');
    expect(script).toContain('$script:Process.Kill($true)');
    expect(script).toContain('Close-AllDocuments');
  });
});
