import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

function runPowerShell(source: string) {
  return spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(source, 'utf16le').toString('base64')], {
    encoding: 'utf8',
    timeout: 30_000,
  });
}

describe('installer source revision binding', () => {
  it('requires explicit source identity for unsigned proofs and records it without trusting ambient CI state', () => {
    const installer = readFileSync('scripts/build-installer.ps1', 'utf8');
    expect(installer).toContain("if ($UnsignedLocal -and [string]::IsNullOrWhiteSpace($SourceRevision))");
    expect(installer).toContain('if ($SignedOcr -or $UnsignedLocal) { Assert-InstallerSourceRevision');
    expect(installer).toContain("if ($ReleaseOcr -and [string]::IsNullOrWhiteSpace($SourceRevision))");
    expect(installer).toContain('sourceRevision = $SourceRevision');
    expect(installer).not.toMatch(/sourceRevision\s*=\s*if \(\$(?:ArtifactOnly|SignedOcr|ReleaseOcr)\)/);
    expect(installer).not.toMatch(/GITHUB_(SHA|REF)/);
  });

  it('accepts only the exact clean local HEAD and rejects forged environment, mismatches, and tracked dirt', () => {
    const check = String.raw`
      $ErrorActionPreference = 'Stop'
      $scriptPath = '${process.cwd().replaceAll("'", "''")}\\scripts\\build-installer.ps1'
      $tokens=$null;$errors=$null
      $ast=[Management.Automation.Language.Parser]::ParseFile($scriptPath,[ref]$tokens,[ref]$errors)
      $function=$ast.Find({param($node)$node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq 'Assert-InstallerSourceRevision'},$true)
      if(-not $function){throw 'Source revision guard is missing.'}
      Invoke-Expression $function.Extent.Text
      $repository=Join-Path ([IO.Path]::GetTempPath()) ('smacrobat-installer-revision-'+[Guid]::NewGuid().ToString('N'))
      [IO.Directory]::CreateDirectory($repository)|Out-Null
      git -C $repository init --quiet
      git -C $repository config user.name 'Smacrobat Test'
      git -C $repository config user.email 'smacrobat-test@example.invalid'
      [IO.File]::WriteAllText((Join-Path $repository 'tracked.txt'),'clean',[Text.UTF8Encoding]::new($false))
      git -C $repository add -- tracked.txt
      git -C $repository commit --quiet -m initial
      $head=(git -C $repository rev-parse HEAD).Trim()
      $env:GITHUB_SHA='0'*40
      Assert-InstallerSourceRevision -ProjectRoot $repository -SourceRevision $head
      foreach($bad in @('', ('A'*40), ('0'*40))) {
        $rejected=$false; try { Assert-InstallerSourceRevision -ProjectRoot $repository -SourceRevision $bad } catch { $rejected=$true }
        if(-not $rejected){throw 'Invalid or mismatched source revision was accepted.'}
      }
      [IO.File]::WriteAllText((Join-Path $repository 'tracked.txt'),'dirty',[Text.UTF8Encoding]::new($false))
      $rejected=$false; try { Assert-InstallerSourceRevision -ProjectRoot $repository -SourceRevision $head } catch { $rejected=$true }
      if(-not $rejected){throw 'Dirty tracked tree was accepted.'}
      git -C $repository checkout -- tracked.txt
      [IO.File]::WriteAllText((Join-Path $repository 'tracked.txt'),'staged',[Text.UTF8Encoding]::new($false)); git -C $repository add -- tracked.txt
      $rejected=$false; try { Assert-InstallerSourceRevision -ProjectRoot $repository -SourceRevision $head } catch { $rejected=$true }
      if(-not $rejected){throw 'Dirty tracked index was accepted.'}
      Write-Output 'Bound installer proof to exact clean local HEAD.'
    `;
    const result = runPowerShell(check);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('exact clean local HEAD');
  }, 35_000);
});
