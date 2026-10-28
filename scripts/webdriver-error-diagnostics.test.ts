import { describe, expect, it } from 'vitest';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { createServer, type Server } from 'node:http';
import { AddressInfo } from 'node:net';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

function functionsPreamble(names: string[]) {
  return String.raw`
    $ErrorActionPreference='Stop'; Set-StrictMode -Version Latest
    Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
    $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path './scripts/installed-app-launch.ps1'),[ref]$tokens,[ref]$errors)
    if($errors.Count){throw 'Script did not parse.'}
    foreach($name in @(${names.map((name) => `'${name}'`).join(',')})) {
      $function=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq$name},$true)
      if(-not$function){throw ('missing '+$name)};Invoke-Expression $function.Extent.Text
    }
  `;
}

function runPowerShellAsync(source: string): Promise<{ status: number | null; stdout: string; stderr: string }> {
  const root = mkdtempSync(join(tmpdir(), 'smacrobat-webdriver-error-'));
  const path = join(root, 'run.ps1');
  writeFileSync(path, source, 'utf8');
  return new Promise(resolve => {
    const child = spawn('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path]);
    let stdout = ''; let stderr = '';
    child.stdout.on('data', chunk => { stdout += String(chunk); });
    child.stderr.on('data', chunk => { stderr += String(chunk); });
    const timer = setTimeout(() => child.kill(), 60_000);
    child.on('close', status => { clearTimeout(timer); rmSync(root, { recursive: true, force: true }); resolve({ status, stdout, stderr }); });
  });
}

describe('WebDriver error diagnostics', () => {
  it('sanitizes W3C error bodies to a code and a path-free one-line message', async () => {
    const result = await runPowerShellAsync(functionsPreamble(['ConvertTo-SanitizedWebDriverError']) + String.raw`
      $body='{"value":{"error":"javascript error","message":"SyntaxError: Invalid or unexpected token at C:\\Users\\runneradmin\\AppData\\x.js\nstack line 2","stacktrace":"secret stack"}}'
      $s=ConvertTo-SanitizedWebDriverError -StatusCode 500 -BodyText $body
      if($s.status-ne500-or$s.error-cne'javascript error'-or$s.message-cne'SyntaxError: Invalid or unexpected token at <path>'){throw ('sanitized: '+($s|ConvertTo-Json -Compress))}
      $s=ConvertTo-SanitizedWebDriverError -StatusCode 404 -BodyText '{"value":{"error":"no such window","message":"Browsing context has been discarded"}}'
      if($s.error-cne'no such window'-or$s.message-cne'Browsing context has been discarded'){throw 'no such window'}
      $s=ConvertTo-SanitizedWebDriverError -StatusCode 500 -BodyText 'not json'
      if($s.error-cne'unparsed'-or$s.message-cne''){throw 'unparsed body'}
      $s=ConvertTo-SanitizedWebDriverError -StatusCode 500 -BodyText '{"value":{"error":"<script>bad</script>","message":"x"}}'
      if($s.error-cne'unrecognized'){throw 'unrecognized code'}
      $s=ConvertTo-SanitizedWebDriverError -StatusCode 500 -BodyText ('{"value":{"error":"unknown error","message":"'+('a'*500)+'"}}')
      if($s.message.Length-ne200){throw 'unbounded message'}
      $s=ConvertTo-SanitizedWebDriverError -StatusCode 500 -BodyText '{"value":{"error":"unknown error","message":"see \\\\server\\share\\file and D:/a/_temp/x"}}'
      if($s.message-match'server|share'){throw ('unc path leaked: '+$s.message)}
      $cases=[ordered]@{
        'open C:/Users/bob/app/x.js now'='open <path>'
        'open c:\Program Files\My App\x.js'='open <path>'
        'open C:/Program Files/My App/x.js'='open <path>'
        'at ''C:\Program Files\My App\x.js'' failed'='at ''<path>'' failed'
        'see //server/share/dir name/file'='see <path>'
        'see \\server\share\dir name\file'='see <path>'
        'see file:///C:/a b/c'='see <path>'
      }
      foreach($text in $cases.Keys){
        $body=(@{value=@{error='unknown error';message=$text}}|ConvertTo-Json -Compress)
        $s=ConvertTo-SanitizedWebDriverError -StatusCode 500 -BodyText $body
        if($s.message-cne$cases[$text]){throw ('path case ['+$text+'] -> ['+$s.message+']')}
      }
      $clean=@('Element not found: a/b ratio 1/2','navigate to http://localhost:4444/session/x','ratio 3:4 of a:b','element click intercepted: <div> at (10, 20)')
      foreach($text in $clean){
        $body=(@{value=@{error='unknown error';message=$text}}|ConvertTo-Json -Compress)
        $s=ConvertTo-SanitizedWebDriverError -StatusCode 500 -BodyText $body
        if($s.message-cne$text){throw ('negative control altered ['+$text+'] -> ['+$s.message+']')}
      }
      'PASS'
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 60_000);

  it('logs and throws the sanitized error for a real HTTP 500 WebDriver response instead of an opaque status', async () => {
    let server: Server | null = null;
    try {
      server = createServer((request, response) => {
        response.writeHead(500, { 'Content-Type': 'application/json; charset=utf-8' });
        response.end(JSON.stringify({ value: { error: 'javascript error', message: 'SyntaxError: Invalid or unexpected token in D:\\a\\_temp\\work\\x.js', stacktrace: 'frames' } }));
      });
      await new Promise<void>(resolve => server!.listen(0, '127.0.0.1', () => resolve()));
      const port = (server.address() as AddressInfo).port;
      const result = await runPowerShellAsync(functionsPreamble(['Assert-BoundedJsonShape', 'Get-LaunchRemainingMilliseconds', 'Get-LoopbackRequestTimeoutMilliseconds', 'Read-BoundedLoopbackResponseText', 'ConvertTo-SanitizedWebDriverError', 'Invoke-BoundedLoopbackJson']) + String.raw`
        $script:LaunchPins=[ordered]@{WebDriverPort=${port};NativeDriverPort=1;JsonDepthMaximum=12;JsonNodesMaximum=4096;RequestBytesMaximum=1MB;RequestTimeoutMilliseconds=10000;SessionCreationTimeoutMilliseconds=30000;CleanupProcessPollMilliseconds=100}
        $message=''
        try{Invoke-BoundedLoopbackJson -Method POST -Path '/session/abc-123/execute/sync' -Body ([ordered]@{script='return 1;';args=@()}) -Deadline ([datetime]::UtcNow.AddSeconds(20)) -Port ${port}|Out-Null}catch{$message=$_.Exception.Message}
        if($message-cne'WebDriver returned HTTP 500 (javascript error): SyntaxError: Invalid or unexpected token in <path>'){throw ('unexpected: '+$message)}
        'PASS'
      `);
      expect(result.status, result.stderr || result.stdout).toBe(0);
      expect(result.stdout).toContain('WEBDRIVER_ERROR_DIAG status=500 error=javascript error message=SyntaxError: Invalid or unexpected token in <path>');
      expect(result.stdout).not.toMatch(/_temp|frames/);
      expect(result.stdout).toContain('PASS');
    } finally {
      await new Promise<void>(resolve => (server ? server.close(() => resolve()) : resolve()));
    }
  }, 60_000);

  const backslashQuoteScan = String.raw`
    function Find-BackslashQuoteStrings([string]$Directory){
      $found=@()
      foreach($file in Get-ChildItem -LiteralPath $Directory -Recurse -Filter '*.ps1'){
        $tokens=$null;$errors=$null;[void][Management.Automation.Language.Parser]::ParseFile($file.FullName,[ref]$tokens,[ref]$errors)
        foreach($token in $tokens){ if(($token.Kind -eq 'StringExpandable' -or $token.Kind -eq 'StringLiteral') -and $token.Text.Length -gt 2 -and $token.Text.StartsWith('"') -and $token.Text.EndsWith('\"')){ $found+=('{0}:{1}' -f $file.Name,$token.Extent.StartLineNumber) } }
      }
      return ,$found
    }
  `;

  it('has no double-quoted PowerShell string in any script that ends in a backslash before its closing quote', async () => {
    const result = await runPowerShellAsync(String.raw`
      $ErrorActionPreference='Stop'; Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
    ` + backslashQuoteScan + String.raw`
      $found=Find-BackslashQuoteStrings 'scripts'
      if($found.Count){throw ('Backslash-quote PowerShell strings: '+($found -join ', '))}
      'PASS'
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 120_000);

  it('the backslash-quote scan detects a planted offender and ignores a planted clean string', async () => {
    const result = await runPowerShellAsync(String.raw`
      $ErrorActionPreference='Stop'
    ` + backslashQuoteScan + String.raw`
      $dir=Join-Path ([IO.Path]::GetTempPath()) ('scan-control-'+[guid]::NewGuid().ToString('N'))
      New-Item -ItemType Directory -Path $dir | Out-Null
      try {
        Set-Content -LiteralPath (Join-Path $dir 'bad.ps1') -Value '$a = "C:\x\"' -Encoding utf8
        Set-Content -LiteralPath (Join-Path $dir 'good.ps1') -Value '$a = "C:\x\y"' -Encoding utf8
        $found=Find-BackslashQuoteStrings $dir
        if($found.Count-ne1-or$found[0]-cne'bad.ps1:1'){throw ('scan control: '+($found -join ','))}
      } finally { Remove-Item -LiteralPath $dir -Recurse -Force }
      'PASS'
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 60_000);

  it('never builds a double-quoted PowerShell argument that ends in a backslash before its closing quote', () => {
    const source = readFileSync('scripts/installed-structural-page-tools.ps1', 'utf8');
    expect(source).not.toMatch(/\\"'\+/);
    expect(source).toContain("d=[...document.querySelectorAll('dialog')].filter(x=>x.getAttribute('aria-labelledby')===id);return d.length===1&&d[0].open;");
    expect(source).toContain("m=[...document.querySelectorAll('dialog[open] input')].filter(x=>x.getAttribute('aria-label')===a),i=m.length===1?m[0]:null;");
  });
});
