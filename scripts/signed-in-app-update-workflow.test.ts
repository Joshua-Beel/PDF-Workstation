import { describe, expect, it } from 'vitest';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';

function runPwshFile(source: string, env: NodeJS.ProcessEnv = process.env) {
  const root = `target/in-app-update-test-${randomUUID()}`;
  mkdirSync(root, { recursive: true });
  const path = `${root}/run.ps1`;
  writeFileSync(path, source, 'utf8');
  try {
    return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path], { encoding: 'utf8', timeout: 60_000, env });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

const workflowPath = '.github/workflows/in-app-update-verification.yml';
const scriptPath = 'scripts/verify-in-app-update.ps1';

function normalizeNewlines(value: string) {
  return value.replace(/\r\n?/g, '\n');
}

function verifierHarness(names: string[], body: string, preamble = '') {
  return String.raw`
    $ErrorActionPreference='Stop'
    Set-StrictMode -Version Latest
    Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
    ${preamble}
    $tokens=$null;$errors=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path '${scriptPath}'),[ref]$tokens,[ref]$errors)
    if($errors.Count){throw 'In-app updater verifier did not parse.'}
    foreach($name in @(${names.map(name => "'" + name + "'").join(',')})){
      $function=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq$name},$true)
      if(-not$function){throw('Missing verifier function: '+$name)}
      Invoke-Expression $function.Extent.Text
    }
    ${body}
  `;
}

describe('published in-app update verification infrastructure', () => {
  it('normalizes LF and Windows CRLF before exact workflow block assertions', () => {
    expect(normalizeNewlines('permissions:\n  contents: read')).toBe('permissions:\n  contents: read');
    expect(normalizeNewlines('permissions:\r\n  contents: read\r\n')).toBe('permissions:\n  contents: read\n');
  });

  it('is manual, read-only, exact-input driven, and carries no release or signing credential', () => {
    const workflow = normalizeNewlines(readFileSync(workflowPath, 'utf8'));
    const requiredInputs = [
      'target_version', 'target_tag', 'target_source_revision', 'target_release_id',
      'target_installer_receipt', 'target_signature_receipt', 'target_manifest_receipt', 'expected_publisher',
    ];
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    for (const input of requiredInputs) expect(workflow).toContain('      ' + input + ':');
    expect(requiredInputs).toHaveLength(8);
    expect(workflow).toContain("Assert-ReceiptProperties $installer @('id','name','bytes','sha256') 'Installer'");
    expect(workflow).toContain("Assert-ReceiptProperties $signature @('id','bytes','sha256') 'Signature'");
    expect(workflow).toContain("Assert-ReceiptProperties $manifest @('id','bytes','sha256') 'Manifest'");
    expect(workflow).toContain('permissions:\n  contents: read');
    expect(workflow).not.toMatch(/contents:\s*write|actions:\s*write|id-token:\s*write/);
    const tokenLine = '          IN_APP_UPDATE_API_TOKEN: ${{ github.token }}\n';
    expect(workflow.split(tokenLine)).toHaveLength(2);
    const exerciseStep = workflow.slice(workflow.indexOf('- name: Exercise the exact published in-app update'), workflow.indexOf('- name: Upload only the sanitized'));
    expect(exerciseStep).toContain(tokenLine);
    expect(workflow.replace(tokenLine, '')).not.toMatch(/secrets\.|github\.token|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|IN_APP_UPDATE_API_TOKEN/);
    expect(workflow.match(/^ {2}[a-z-]+: (read|write)$/gm)).toEqual(['  contents: read']);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('TARGET_TAG -cne "v$env:TARGET_VERSION"');
    expect(workflow).toContain('git rev-list -n 1 $env:TARGET_TAG');
    expect(workflow).toContain('$tagRevision -cne $env:TARGET_SOURCE_REVISION');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('./scripts/verify-in-app-update.ps1');
    expect(workflow).toContain('-PublisherUiMode HostedNonInteractive `');
    expect(workflow.match(/^ {6}[a-z_]+:\n {8}description:/gm)).toHaveLength(requiredInputs.length);
    expect(workflow).toContain('in-app-update-verification.json');
    expect(workflow).not.toMatch(/gh release|release create|release edit|--draft=false|--latest/);
  });

  it('pins public artifacts and fails closed on release, asset, manifest, and signature identity', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toContain("BaselineVersion = '0.2.0'");
    expect(script).toContain('BaselineBytes = [uint64]6418229');
    expect(script).toContain("BaselineSha256 = '17788CB82BB42DEAC35EA197A385A9BAFC8CD331422446A2B834E115A90C4D2E'");
    expect(script).toContain('releases/latest/download/latest.json');
    expect(script).toContain('/releases/tags/$TargetTag');
    expect(script).toContain('[uint64]$latestRelease.id -ne $targetReleaseIdValue');
    expect(script).toContain('[bool]$release.draft');
    expect(script).toContain('[bool]$release.prerelease');
    expect(script.match(/Assert-ExactAsset -Release \$release/g)).toHaveLength(3);
    expect(script).toContain("$manifest.platforms.'windows-x86_64'");
    expect(script).toContain('[string]$platform.signature -cne $signatureText');
    expect(script).toContain('[string]$platform.url -cne [string]$installerAsset.browser_download_url');
    expect(script).toContain('Assert-TrustedWindowsSignature -Path $targetInstallerPath');
    expect(script).toContain('Assert-TrustedWindowsSignature -Path $extractedApp.FullName');
    expect(script).toContain('^PDF(?:\\.| )Workstation_');
    const apiReader = script.slice(script.indexOf('function Invoke-GitHubReleaseApi'), script.indexOf('function Invoke-PublicDownload'));
    expect(apiReader).toContain("[Net.Http.Headers.AuthenticationHeaderValue]::new('Bearer',$Token)");
    expect(script.replace(apiReader, '')).not.toMatch(/Authorization|Bearer|api[_-]?key|client[_-]?secret/i);
    expect(script.match(/Invoke-GitHubReleaseApi -Path "\/repos\/\$\(\$script:UpdatePins\.Repository\)\/releases\/(tags\/\$TargetTag|latest)" -Destination \$(metadataPath|latestMetadataPath) -Token \$script:ApiToken/g)).toHaveLength(2);
    expect(script).not.toMatch(/Invoke-PublicDownload -Uri "https:\/\/api\.github\.com/);
    expect(script).not.toMatch(/dangerousInsecureTransportProtocol|dangerousAcceptInvalid|allowDowngrades|\.endpoints\(/);
  });

  it('authenticates only exact release API reads, never follows their redirects, and never leaks the token', () => {
    const fakeToken = 'ghs_' + 'Q7'.repeat(18);
    const source = verifierHarness(
      ['Read-ApiTokenFromEnvironment', 'New-ApiHttpHandler', 'Get-RateLimitHeaderValue', 'Invoke-GitHubReleaseApi', 'Invoke-PublicDownload'],
      String.raw`
        $script:UpdatePins=[ordered]@{Repository='Joshua-Beel/PDF-Workstation';JsonBytesMaximum=[uint64]1MB;NetworkBytesMaximum=[uint64]1MB}
        Add-Type -TypeDefinition @'
using System; using System.Collections.Generic; using System.Net; using System.Net.Http; using System.Threading; using System.Threading.Tasks;
public sealed class InAppRecordingHandler : HttpMessageHandler {
  public readonly List<string> Log = new List<string>();
  public int Status = 200; public string Location; public string Body = "{}";
  public readonly Dictionary<string,string> Headers = new Dictionary<string,string>();
  protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) {
    Log.Add(request.RequestUri.Host + "|" + (request.Headers.Authorization == null ? "none" : request.Headers.Authorization.Scheme));
    var response = new HttpResponseMessage((HttpStatusCode)Status) { RequestMessage = request, Content = new StringContent(Body) };
    if (Location != null) response.Headers.Location = new Uri(Location);
    foreach (var header in Headers) response.Headers.TryAddWithoutValidation(header.Key, header.Value);
    return Task.FromResult(response);
  }
}
'@
        $root=Join-Path (Resolve-Path target) ('in-app-api-'+[Guid]::NewGuid().ToString('N'))
        [IO.Directory]::CreateDirectory($root)|Out-Null
        try{
          $token=Read-ApiTokenFromEnvironment -Name 'IN_APP_UPDATE_API_TOKEN'
          if($null-ne[Environment]::GetEnvironmentVariable('IN_APP_UPDATE_API_TOKEN')-or(Test-Path env:IN_APP_UPDATE_API_TOKEN)){throw 'Token stayed in the process environment.'}
          $child=& pwsh.exe -NoProfile -NonInteractive -Command '[string][Environment]::GetEnvironmentVariable(''IN_APP_UPDATE_API_TOKEN'')'
          if(-not[string]::IsNullOrEmpty(($child-join''))){throw 'A child process inherited the token.'}
          foreach($bad in @('','short','has space in it 1234567890',('ghs_'+('x'*20)+'!'),('x'*256))){
            [Environment]::SetEnvironmentVariable('IN_APP_UPDATE_API_TOKEN',$bad,'Process')
            $rejected=$false;try{$null=Read-ApiTokenFromEnvironment -Name 'IN_APP_UPDATE_API_TOKEN'}catch{$rejected=$true}
            if(-not$rejected){throw ('Malformed token was accepted: length '+$bad.Length)}
            if($null-ne[Environment]::GetEnvironmentVariable('IN_APP_UPDATE_API_TOKEN')){throw 'Malformed token was left in the environment.'}
          }
          if((New-ApiHttpHandler).AllowAutoRedirect){throw 'API handler follows redirects.'}
          $following={$h=[Net.Http.HttpClientHandler]::new();$h.AllowAutoRedirect=$true;$h}
          $rejected=$false;try{Invoke-GitHubReleaseApi -Path '/repos/Joshua-Beel/PDF-Workstation/releases/latest' -Destination (Join-Path $root 'never.json') -Token $token -HandlerFactory $following}catch{$rejected=$true}
          if(-not$rejected){throw 'A redirect-following API handler was accepted.'}
          $ok=[InAppRecordingHandler]::new()
          Invoke-GitHubReleaseApi -Path '/repos/Joshua-Beel/PDF-Workstation/releases/tags/v0.3.1' -Destination (Join-Path $root 'ok.json') -Token $token -HandlerFactory {$ok}.GetNewClosure()
          if(($ok.Log-join',')-cne'api.github.com|Bearer'){throw ('API request log changed: '+($ok.Log-join','))}
          $redirect=[InAppRecordingHandler]::new();$redirect.Status=302;$redirect.Location='https://objects.example.invalid/release.json'
          $message='';try{Invoke-GitHubReleaseApi -Path '/repos/Joshua-Beel/PDF-Workstation/releases/latest' -Destination (Join-Path $root 'redirect.json') -Token $token -HandlerFactory {$redirect}.GetNewClosure()}catch{$message=$_.Exception.Message}
          if(-not$message.Contains('redirect was not followed')-or$redirect.Log.Count-ne1){throw 'API redirect was followed or not refused.'}
          foreach($path in @('/repos/Other/Repo/releases/latest','/repos/Joshua-Beel/PDF-Workstation/releases/latest?per_page=1','/repos/Joshua-Beel/PDF-Workstation/releases','/repos/Joshua-Beel/PDF-Workstation/releases/tags/latest','//evil.example/repos/Joshua-Beel/PDF-Workstation/releases/latest')){
            $never=[InAppRecordingHandler]::new()
            $rejected=$false;try{Invoke-GitHubReleaseApi -Path $path -Destination (Join-Path $root 'bad.json') -Token $token -HandlerFactory {$never}.GetNewClosure()}catch{$rejected=$true}
            if(-not$rejected-or$never.Log.Count-ne0){throw ('API path was not refused before sending: '+$path)}
          }
          $limited=[InAppRecordingHandler]::new();$limited.Status=403;$limited.Headers['X-RateLimit-Remaining']='0';$limited.Headers['X-RateLimit-Reset']='1700000000';$limited.Body=('echo '+$token)
          $message='';try{Invoke-GitHubReleaseApi -Path '/repos/Joshua-Beel/PDF-Workstation/releases/latest' -Destination (Join-Path $root 'limited.json') -Token $token -HandlerFactory {$limited}.GetNewClosure()}catch{$message=$_.Exception.Message}
          if($message-cne'GitHub API release metadata request failed with status 403 (rate limit remaining 0, reset 1700000000).'){throw ('Rate-limit failure message changed: '+$message)}
          Write-Output ('CAUGHT '+$message)
          $tooMany=[InAppRecordingHandler]::new();$tooMany.Status=429;$tooMany.Headers['X-RateLimit-Remaining']=$token
          $message='';try{Invoke-GitHubReleaseApi -Path '/repos/Joshua-Beel/PDF-Workstation/releases/latest' -Destination (Join-Path $root 'many.json') -Token $token -HandlerFactory {$tooMany}.GetNewClosure()}catch{$message=$_.Exception.Message}
          if($message-cne'GitHub API release metadata request failed with status 429 (rate limit remaining invalid, reset absent).'){throw ('429 failure message changed: '+$message)}
          Write-Output ('CAUGHT '+$message)
          $public=[InAppRecordingHandler]::new();$public.Body='public bytes'
          $null=Invoke-PublicDownload -Uri 'https://github.com/Joshua-Beel/PDF-Workstation/releases/latest/download/latest.json' -Destination (Join-Path $root 'latest.json') -HandlerFactory {$public}.GetNewClosure()
          if(($public.Log-join',')-cne'github.com|none'){throw 'Public download carried an Authorization header.'}
          $rejected=$false;try{$null=Invoke-PublicDownload -Uri 'https://api.github.com/repos/Joshua-Beel/PDF-Workstation/releases/latest' -Destination (Join-Path $root 'api.json') -HandlerFactory {[InAppRecordingHandler]::new()}}catch{$rejected=$true}
          if(-not$rejected){throw 'Public downloader accepted the API host.'}
          Write-Output 'API-AUTH-CHECKS-PASSED'
        } finally { Remove-Item -LiteralPath $root -Recurse -Force -ErrorAction SilentlyContinue }
      `,
    );
    const result = runPwshFile(source, { ...process.env, IN_APP_UPDATE_API_TOKEN: fakeToken });
    const output = `${result.stdout}\n${result.stderr}`;
    expect(result.status, output).toBe(0);
    expect(result.stdout).toContain('API-AUTH-CHECKS-PASSED');
    expect(result.stdout).toContain('GITHUB_API_DIAG status=403 rateLimitRemaining=0 rateLimitReset=1700000000');
    expect(result.stdout).toContain('GITHUB_API_DIAG status=429 rateLimitRemaining=invalid rateLimitReset=absent');
    expect(output).not.toContain(fakeToken);
    expect(output).not.toContain('Q7Q7Q7');
  });

  it('reads and clears the API token before any dot-source, download or child process', () => {
    const script = readFileSync(scriptPath, 'utf8');
    const read = script.indexOf("$script:ApiToken = Read-ApiTokenFromEnvironment -Name 'IN_APP_UPDATE_API_TOKEN'");
    expect(read).toBeGreaterThan(0);
    for (const later of [". (Join-Path $PSScriptRoot 'installed-app-launch.ps1')", '\nAssert-HostedRunner', 'Invoke-GitHubReleaseApi -Path "', '$null = Invoke-PublicDownload -Uri', 'Expand-TargetInstaller -Installer $targetInstallerPath', 'Invoke-SilentInstaller -Path $baselinePath', '$session = Start-WebDriverSession']) {
      expect(script.indexOf(later), later).toBeGreaterThan(read);
    }
    expect(script).toMatch(/-Token \$script:ApiToken\r?\n\$script:ApiToken = \$null\r?\n/);
    expect(script.match(/\$script:ApiToken/g)).toHaveLength(4);
    expect(script).toContain('IN_APP_UPDATE_API_TOKEN|gh[pousr]_[A-Za-z0-9_]{16,}');
  });

  it('uses the real updater UI with deterministic blocked-download recovery and verifies relaunch', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toContain("textContent.trim()==='Check for updates…'");
    expect(script).toContain("textContent.trim()==='Install update and restart'");
    expect(script).toContain("'Installed version: 0.2.0'");
    expect(script).toContain('New-NetFirewallRule');
    expect(script).toContain('-Direction Outbound -Action Block -Program ([string]$baselineFacts.appPath)');
    expect(script).toContain("'Your installed version is unchanged. You can retry.'");
    expect(script).toContain('Remove-NetFirewallRule -DisplayName $firewallName -ErrorAction Stop');
    expect(script).toContain("Get-NetFirewallRule -DisplayName $firewallName -ErrorAction SilentlyContinue");
    expect(script).toMatch(/finally \{[\s\S]*Remove-NetFirewallRule -DisplayName \$firewallName -ErrorAction SilentlyContinue/);
    expect(script.match(/Invoke-InstallButton -SessionId \$session\.SessionId/g)).toHaveLength(2);
    expect(script).toContain("if (-not $sawDownloadState)");
    expect(script).toContain("if (-not $sessionClosedForInstall)");
    expect(script).toContain('exact baseline application process did not exit');
    expect(script).toContain('Assert-InstallFacts -Facts $unchanged');
    expect(script).toContain('Assert-InstallFacts -Facts $upgraded');
    expect(script).toContain('automatically relaunch');
    expect(script).toContain('Wait-CurrentVersionUi');
    expect(script).toContain("'You have the latest released version.'");
    expect(script).toContain('settingsSentinelPreserved = $true');
    expect(script).toContain('documentSentinelPreserved = $true');
  });

  it('requires the automatic relaunch whenever an Explorer shell window exists and labels the verifier launch', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toMatch(/Assert-FirewallEnforcing\r?\n#[^\n]*RunAsUser[^\n]*\r?\n\$shellWindowBeforeUpdate = Test-ShellWindowPresent\r?\n\$session = \$null/);
    expect(script).toMatch(/\$shellWindowAtRelaunch = Test-ShellWindowPresent\r?\n\s+\$relaunchOutcome = Resolve-AutomaticRelaunch -Observed \(\$null -ne \$automaticRelaunch\) -ShellWindowBeforeUpdate \$shellWindowBeforeUpdate -ShellWindowAtRelaunch \$shellWindowAtRelaunch/);
    expect(script).toContain('shellWindowBeforeUpdate = [bool]$shellWindowBeforeUpdate; shellWindowAtRelaunch = [bool]$shellWindowAtRelaunch');
    expect(script).toContain('automaticRelaunchVerified = [bool]$relaunchOutcome.verified');
    expect(script).toContain('automaticRelaunchObserved = [bool]$relaunchOutcome.observed; automaticRelaunchNotObservedReason = $relaunchOutcome.notObservedReason');
    expect(script).toContain("postUpdateLaunchedBy = 'verifier-webdriver'");
    expect(script).not.toMatch(/automaticRelaunch(Verified|Observed)\s*=\s*\$true/);
    expect(script.indexOf('Resolve-AutomaticRelaunch -Observed')).toBeLessThan(script.indexOf('$postSession = Start-WebDriverSession'));
    const source = verifierHarness(
      ['Test-ShellWindowPresent', 'Resolve-AutomaticRelaunch'],
      String.raw`
        $observed=Resolve-AutomaticRelaunch -Observed $true -ShellWindowBeforeUpdate $false -ShellWindowAtRelaunch $false
        if(-not$observed.observed-or-not$observed.verified-or$null-ne$observed.notObservedReason){throw 'Observed relaunch was not recorded as verified.'}
        $observed=Resolve-AutomaticRelaunch -Observed $true -ShellWindowBeforeUpdate $true -ShellWindowAtRelaunch $true
        if(-not$observed.verified){throw 'Observed relaunch with a shell was not verified.'}
        foreach($shell in @(@($true,$false),@($false,$true),@($true,$true))){
          $message='';try{$null=Resolve-AutomaticRelaunch -Observed $false -ShellWindowBeforeUpdate $shell[0] -ShellWindowAtRelaunch $shell[1]}catch{$message=$_.Exception.Message}
          if($message-cne'The updater did not automatically relaunch the exact installed target application.'){throw 'A missing relaunch was excused although a shell window existed.'}
        }
        $missing=Resolve-AutomaticRelaunch -Observed $false -ShellWindowBeforeUpdate $false -ShellWindowAtRelaunch $false
        if($missing.observed-or$missing.verified-or$missing.notObservedReason-cne'github-hosted-no-explorer-shell'){throw 'No-shell outcome was not recorded honestly.'}
        if((Test-ShellWindowPresent -Provider {$true})-ne$true-or(Test-ShellWindowPresent -Provider {$false})-ne$false){throw 'Shell probe provider was not honored.'}
        $rejected=$false;try{$null=Test-ShellWindowPresent -Provider {'yes'}}catch{$rejected=$true};if(-not$rejected){throw 'Non-boolean shell probe was accepted.'}
        if((Test-ShellWindowPresent)-isnot[bool]){throw 'Live shell probe did not return a boolean.'}
      `,
    );
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', source], { encoding: 'utf8', timeout: 30_000 });
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('drives WebDriver through the shared hosted-proven launch helpers', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toMatch(/\r?\n\r?\n\. \(Join-Path \$PSScriptRoot 'installed-app-launch\.ps1'\)\r?\n\r?\n\$script:UpdatePins = \[ordered\]@\{/);
    for (const helper of [
      'Wait-FixedWebDriverPortsFree -Deadline', 'Start-BoundedDiscardProcess -Path $TauriDriverPath', '-Environment (Get-MinimalWindowsProcessEnvironment)',
      "Invoke-BoundedLoopbackJson -Method GET -Path '/status'", 'Wait-NativeDriverStatus -ExpectedVersion $ExpectedEdgeDriverVersion',
      "Invoke-BoundedLoopbackJson -Method POST -Path '/session'", 'Get-OwnedLaunchProcesses -RootProcessId $Driver.Id',
      'Invoke-SessionDeleteOutcome -SessionId', 'Stop-OwnedLaunchProcesses -TauriDriver $Session.Driver', 'Assert-WebDriverReceipt -Receipt $driverReceipt',
      '$script:LaunchPins.TotalTimeoutMilliseconds', '$script:LaunchPins.CleanupProcessTimeoutMilliseconds', 'PortHandoffTimeoutMilliseconds = 300000',
      '[int]$baselineProcesses[0].Id -ne [int]$session.ApplicationProcessId',
    ]) expect(script).toContain(helper);
    expect(script).not.toMatch(/Invoke-RestMethod|Invoke-LoopbackJson|TimeoutSec|DriverDeadlineSeconds|NativeDriverPort = 9515|-ReturnProcess/);
    expect(script.match(/-ExpectedEdgeDriverVersion \$expectedEdgeDriverVersion/g)).toHaveLength(2);
    const launch = readFileSync('scripts/installed-app-launch.ps1', 'utf8');
    expect(launch).toContain('RequestTimeoutMilliseconds = 10000');
    expect(launch).toContain('SessionCreationTimeoutMilliseconds = 30000');
    expect(launch).toContain('TotalTimeoutMilliseconds = 60000');
  });

  it('waits a bounded time for updater setup processes to exit before the post-update session', () => {
    const script = readFileSync(scriptPath, 'utf8');
    const wait = script.indexOf('    Wait-UpdaterSetupExit -StartedAfter $retryStarted');
    expect(wait).toBeGreaterThan(script.indexOf('$relaunchOutcome = Resolve-AutomaticRelaunch'));
    expect(wait).toBeLessThan(script.indexOf('$postSession = Start-WebDriverSession'));
    expect(script).toContain("-TempRoots @([string]$env:TEMP,[IO.Path]::GetTempPath(),(Join-Path $env:LOCALAPPDATA 'Temp'))");
    expect(script).toContain('UpdaterSetupExitDeadlineSeconds = 60');
    expect(script).toContain('updaterSetupExitVerified = $true');
    const source = verifierHarness(['Get-UpdaterSetupProcesses', 'Wait-UpdaterSetupExit'], String.raw`
      $start=[datetime]::UtcNow
      $temp='C:\Users\runner\AppData\Local\Temp'
      $old=[pscustomobject]@{ExecutablePath=$temp+'\old-setup.exe';CreationDate=$start.AddMinutes(-5)}
      $app=[pscustomobject]@{ExecutablePath='C:\Users\runner\AppData\Local\PDF Workstation\pdf-workstation.exe';CreationDate=$start.AddSeconds(5)}
      $sibling=[pscustomobject]@{ExecutablePath='C:\Users\runner\AppData\Local\Temp2\x.exe';CreationDate=$start.AddSeconds(5)}
      $setup=[pscustomobject]@{ExecutablePath=$temp+'\PDF Workstation-0.3.1-updater-abc\PDF Workstation-0.3.1-installer.exe';CreationDate=$start.AddSeconds(3)}
      $found=@(Get-UpdaterSetupProcesses -StartedAfter $start -TempRoots @($temp,'') -SnapshotProvider {@($old,$app,$sibling,$setup)}.GetNewClosure())
      if($found.Count-ne1-or$found[0]-ne$setup){throw 'Updater setup detection is not exact.'}
      $calls=[ref]0
      Wait-UpdaterSetupExit -StartedAfter $start -TempRoots @($temp) -Deadline ([datetime]::UtcNow.AddSeconds(10)) -SnapshotProvider {$calls.Value++;if($calls.Value-lt3){@($setup)}else{@($old,$app)}}.GetNewClosure()
      if($calls.Value-ne3){throw 'Wait did not poll until the setup process exited.'}
      $message='';try{Wait-UpdaterSetupExit -StartedAfter $start -TempRoots @($temp) -Deadline ([datetime]::UtcNow.AddMilliseconds(600)) -SnapshotProvider {@($setup)}.GetNewClosure()}catch{$message=$_.Exception.Message}
      if($message-cne'Updater setup processes did not exit before the post-update check: count=1, names=PDF Workstation-0.3.1-installer.exe.'){throw ('Remaining setup process was not reported exactly: '+$message)}
      if($message.Contains('Users')){throw 'Remaining setup diagnostic leaked a path.'}
    `);
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', source], { encoding: 'utf8', timeout: 30_000 });
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('fails the main flow on incomplete WebDriver cleanup but keeps error-path cleanup best effort', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toContain('$stopped = Stop-OwnedLaunchProcesses -TauriDriver $Session.Driver');
    expect(script).toContain('if (-not $AllowIncomplete) { Assert-WebDriverSessionStopped -Outcome $stopped }');
    expect(script.match(/^\s+Stop-WebDriverSession -Session \$(session|postSession)\r?$/gm)).toHaveLength(2);
    expect(script).toContain('try { Stop-WebDriverSession -Session $postSession -AllowIncomplete } catch { }');
    expect(script).toContain('try { Stop-WebDriverSession -Session $session -AllowIncomplete } catch { }');
    expect(script).toMatch(/Captured = @\(\) \}\) -AllowIncomplete\r?\n\s+throw/);
    const source = verifierHarness(['Assert-WebDriverSessionStopped'], String.raw`
      function New-Outcome($root,$app='all-exited'){[pscustomobject]@{rootOutcome=$root;application=$app;tauriDriver='absent';edgeDriver='all-exited';webview='all-exited';ocrEngine='absent';other='absent'}}
      foreach($root in @('already-exited','tree-kill-exited','fallback-exited')){Assert-WebDriverSessionStopped -Outcome (New-Outcome $root)}
      foreach($bad in @((New-Outcome 'deadline'),(New-Outcome 'kill-failed'),(New-Outcome 'not-invoked'),(New-Outcome 'tree-kill-exited' 'incomplete'))){
        $message='';try{Assert-WebDriverSessionStopped -Outcome $bad}catch{$message=$_.Exception.Message}
        if(-not$message.StartsWith('WebDriver session cleanup was incomplete: root=')){throw 'Incomplete cleanup was accepted.'}
      }
    `);
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', source], { encoding: 'utf8', timeout: 30_000 });
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('does not redefine shared helper functions and fails fast when the firewall cannot enforce the cutoff', () => {
    const source = String.raw`
      $ErrorActionPreference='Stop'
      Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      function Get-FunctionNames([string]$path){
        $tokens=$null;$errors=$null
        $ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path $path),[ref]$tokens,[ref]$errors)
        if($errors.Count){throw ('Did not parse: '+$path)}
        @($ast.FindAll({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]},$true)|ForEach-Object{$_.Name.ToLowerInvariant()})
      }
      $own=Get-FunctionNames 'scripts/verify-in-app-update.ps1'
      foreach($shared in @('scripts/installed-app-launch.ps1','scripts/installed-publisher-ui.ps1')){
        $overlap=@($own|Where-Object{(Get-FunctionNames $shared)-contains$_})
        if($overlap.Count){throw ('Verifier redefines shared functions from '+$shared+': '+($overlap-join', '))}
      }
      . ./scripts/installed-app-launch.ps1
      $tokens=$null;$errors=$null
      $ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path 'scripts/verify-in-app-update.ps1'),[ref]$tokens,[ref]$errors)
      Invoke-Expression $ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq'Assert-FirewallEnforcing'},$true).Extent.Text
      $running={[pscustomobject]@{Status='Running'}}
      $all={@('Domain','Private','Public')|ForEach-Object{[pscustomobject]@{Name=$_;Enabled='True'}}}
      Assert-FirewallEnforcing -ServiceProvider $running -ProfileProvider $all
      foreach($case in @(
        @{service={[pscustomobject]@{Status='Stopped'}};profiles=$all;expect='service=Stopped'},
        @{service=$running;profiles={@('Domain','Private','Public')|ForEach-Object{[pscustomobject]@{Name=$_;Enabled=$(if($_-ceq'Public'){'False'}else{'True'})}}};expect='disabled=Public'},
        @{service=$running;profiles={@([pscustomobject]@{Name='Domain';Enabled='True'})};expect='profiles=1'})){
        $message='';try{Assert-FirewallEnforcing -ServiceProvider $case.service -ProfileProvider $case.profiles}catch{$message=$_.Exception.Message}
        if(-not$message.StartsWith('Windows Firewall is not enforcing on this runner')-or-not$message.Contains($case.expect)){throw ('Firewall precheck did not fail closed: '+$case.expect+' got '+$message)}
      }
    `;
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', source], { encoding: 'utf8', timeout: 30_000 });
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('requires hosted noninteractive publisher mode and records false shell-UI claims', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toContain("[ValidateSet('HostedNonInteractive')][string]$PublisherUiMode");
    expect(script).toContain("$PublisherUiMode -cne 'HostedNonInteractive'");
    expect(script).not.toContain('Invoke-InstalledPublisherUiProof');
    expect(script.match(/= New-HostedPublisherUiNotAttemptedEvidence -Path/g)).toHaveLength(2);
    expect(script.match(/Assert-HostedPublisherUiEvidence -Value \$/g)).toHaveLength(2);
    expect(script).toContain('schemaVersion = 3');
    expect(script).toContain("mode = 'hosted-noninteractive'");
    expect(script).toContain("reason = 'github-hosted-noninteractive-session'");
    expect(script).toContain("publisherUiMode = 'hosted-noninteractive'; publisherUiAttempted = $false");
    expect(script).toContain('installerShellPublisherUiVerified = $false; installedApplicationShellPublisherUiVerified = $false');
    expect(script).toContain('publisherUiScreenshotsUsed = $false');
    expect(script).not.toMatch(/ShellPublisherUiVerified\s*=\s*\$true|publisherUiAttempted\s*=\s*\$true/);
    expect(script).toContain('does not attempt or verify Windows Explorer Digital Signatures UI');
    expect(script).toMatch(/\nAssert-HostedRunner\r?\nif \(\$PublisherUiMode -cne 'HostedNonInteractive'\) \{/);
    expect(script.indexOf("$PublisherUiMode -cne 'HostedNonInteractive'")).toBeLessThan(script.indexOf('$null = Invoke-PublicDownload -Uri'));
  });

  it('keeps exact receipts and trusted timestamped signatures in hosted publisher evidence', () => {
    const fixture = `target/in-app-hosted-publisher-${process.pid}-${Date.now()}.exe`;
    const source = verifierHarness(
      ['Assert-NoReparseAncestors', 'Get-WindowsSignatureFacts', 'Assert-TrustedWindowsSignature', 'Assert-HostedPublisherUiEvidence'],
      String.raw`
        $path=[IO.Path]::GetFullPath('${fixture}')
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path))|Out-Null
        [IO.File]::WriteAllText($path,'signed fixture',[Text.UTF8Encoding]::new($false))
        try{
          $receipt=[pscustomobject]@{bytes=[uint64](Get-Item -LiteralPath $path).Length;sha256=Get-PublisherUiSha256 -Path $path}
          $script:calls=0
          $good={param($p)$script:calls++;[pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$true}}
          $evidence=New-HostedPublisherUiNotAttemptedEvidence -Path $path -Receipt $receipt -Kind installer -ExpectedPublisher 'Joshua Beel' -SignatureProvider $good
          if($script:calls-ne2){throw 'Hosted evidence did not verify the signature twice.'}
          Assert-HostedPublisherUiEvidence -Value $evidence -Kind installer -FileName ([IO.Path]::GetFileName($path)) -Bytes $receipt.bytes -Sha256 $receipt.sha256 -Publisher 'Joshua Beel'
          foreach($bad in @(
            [pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$false},
            [pscustomobject]@{Status='Valid';Publisher='Someone Else';HasTimestamp=$true},
            [pscustomobject]@{Status='NotSigned';Publisher='Joshua Beel';HasTimestamp=$true})){
            $rejected=$false;try{$null=New-HostedPublisherUiNotAttemptedEvidence -Path $path -Receipt $receipt -Kind installer -ExpectedPublisher 'Joshua Beel' -SignatureProvider {param($p)$bad}.GetNewClosure()}catch{$rejected=$true}
            if(-not$rejected){throw 'Hosted evidence accepted an untrusted signature.'}
          }
          $wrongReceipt=[pscustomobject]@{bytes=$receipt.bytes;sha256=('A'*64)}
          $rejected=$false;try{$null=New-HostedPublisherUiNotAttemptedEvidence -Path $path -Receipt $wrongReceipt -Kind installer -ExpectedPublisher 'Joshua Beel' -SignatureProvider $good}catch{$rejected=$true}
          if(-not$rejected){throw 'Hosted evidence accepted a wrong byte receipt.'}
          foreach($mutation in @('shellPublisherUiAttempted','shellPublisherUiVerified','screenshotsUsed')){
            $copy=$evidence.PSObject.Copy();$copy.$mutation=$true
            $rejected=$false;try{Assert-HostedPublisherUiEvidence -Value $copy -Kind installer -FileName ([IO.Path]::GetFileName($path)) -Bytes $receipt.bytes -Sha256 $receipt.sha256 -Publisher 'Joshua Beel'}catch{$rejected=$true}
            if(-not$rejected){throw ('Hosted evidence accepted a true '+$mutation+' claim.')}
          }
          $copy=$evidence.PSObject.Copy();$copy.notAttemptedReason='other'
          $rejected=$false;try{Assert-HostedPublisherUiEvidence -Value $copy -Kind installer -FileName ([IO.Path]::GetFileName($path)) -Bytes $receipt.bytes -Sha256 $receipt.sha256 -Publisher 'Joshua Beel'}catch{$rejected=$true}
          if(-not$rejected){throw 'Hosted evidence accepted another reason.'}
          $rejected=$false;try{Assert-HostedPublisherUiEvidence -Value $evidence -Kind installed-application -FileName ([IO.Path]::GetFileName($path)) -Bytes $receipt.bytes -Sha256 $receipt.sha256 -Publisher 'Joshua Beel'}catch{$rejected=$true}
          if(-not$rejected){throw 'Hosted evidence accepted the wrong kind.'}
          $copy=$evidence.PSObject.Copy();$copy|Add-Member -NotePropertyName extra -NotePropertyValue 1
          $rejected=$false;try{Assert-HostedPublisherUiEvidence -Value $copy -Kind installer -FileName ([IO.Path]::GetFileName($path)) -Bytes $receipt.bytes -Sha256 $receipt.sha256 -Publisher 'Joshua Beel'}catch{$rejected=$true}
          if(-not$rejected){throw 'Hosted evidence accepted an extra property.'}
        } finally { [IO.File]::Delete($path) }
      `,
      '. ./scripts/installed-publisher-ui.ps1',
    );
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', source], { encoding: 'utf8', timeout: 30_000 });
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('rechecks PID identity and limits interruption claims to observed evidence', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toContain('$samePath = [IO.Path]::GetFullPath([string]$process.MainModule.FileName)');
    expect(script).toContain('$sameStart = [long]$process.StartTime.ToUniversalTime().Ticks -eq [long]$entry.StartTicks');
    expect(script).toContain('standaloneSignedInstallerRepairVerified = $true');
    expect(script).toContain('partialByteDownloadInterruptionVerified = $false');
    expect(script).toContain('appKillMidDownloadVerified = $false');
    expect(script).toContain('installerStageInterruptionVerified = $false');
    expect(script).toContain('installerTransactionalityVerified = $false');
    expect(script).toContain("[IO.File]::Move([string]$upgraded.appPath,$quarantineApp,$false)");
    expect(script).toContain('Controlled damage did not remove the installed application executable.');
    expect(script).toContain("damageKind = 'missing-application-executable'");
    expect(script).toContain('Assert-FileReceipt -Path $quarantineApp');
    expect(script).toContain('Repair changed the isolated quarantine inventory.');
    expect(script).not.toMatch(/appKillMidDownloadVerified\s*=\s*\$true|installerTransactionalityVerified\s*=\s*\$true/);
  });

  it('writes exactly one sanitized receipt-only JSON record', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toContain("Join-Path $output 'in-app-update-verification.json'");
    expect(script).toContain('$outputFiles.Count -ne 1');
    expect(script).toContain('notes = $manifestNotesReceipt');
    expect(script).not.toContain('notes = [string]$manifest.notes');
    expect(script).toContain('GITHUB_TOKEN|GH_TOKEN|IN_APP_UPDATE_API_TOKEN|gh[pousr]_[A-Za-z0-9_]{16,}|AZURE_|TAURI_SIGNING');
    expect(script).toContain('release-assets\\.githubusercontent\\.com');
    expect(script).not.toMatch(/Write-Host\s+\$json|Write-Output\s+\$json/);
  });

  it('never assigns automatic variables or leaks WaitForExit output in the verifier and its dot-sourced helper', () => {
    const source = String.raw`
      $ErrorActionPreference='Stop'
      Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      $automatic=@('_','args','ConsoleFileName','EnabledExperimentalFeatures','Error','Event','EventArgs','EventSubscriber','ExecutionContext','false','foreach','HOME','Host','input','IsCoreCLR','IsLinux','IsMacOS','IsWindows','LASTEXITCODE','Matches','MyInvocation','NestedPromptLevel','PID','PROFILE','PSBoundParameters','PSCmdlet','PSCommandPath','PSCulture','PSDebugContext','PSEdition','PSHOME','PSItem','PSScriptRoot','PSSenderInfo','PSUICulture','PSVersionTable','PWD','Sender','ShellId','StackTrace','switch','this','true')
      function Get-ParsedAst([string]$text){
        $tokens=$null;$errors=$null
        $ast=[Management.Automation.Language.Parser]::ParseInput($text,[ref]$tokens,[ref]$errors)
        if($errors.Count){throw 'Source did not parse.'}
        return $ast
      }
      function Get-AutomaticAssignments($ast){
        $nodes=$ast.FindAll({param($n)$n-is[Management.Automation.Language.AssignmentStatementAst]-or$n-is[Management.Automation.Language.ForEachStatementAst]-or$n-is[Management.Automation.Language.ParameterAst]},$true)
        foreach($node in $nodes){
          $target=if($node-is[Management.Automation.Language.AssignmentStatementAst]){$node.Left}elseif($node-is[Management.Automation.Language.ForEachStatementAst]){$node.Variable}else{$node.Name}
          foreach($variable in @($target.FindAll({param($n)$n-is[Management.Automation.Language.VariableExpressionAst]},$true))){
            if($automatic-contains$variable.VariablePath.UserPath){ 'line '+$variable.Extent.StartLineNumber+' $'+$variable.VariablePath.UserPath }
          }
        }
      }
      function Get-WaitForExitLeaks($ast){
        @($ast.FindAll({param($n)
          $n-is[Management.Automation.Language.PipelineAst]-and$n.PipelineElements.Count-eq1-and
          $n.PipelineElements[0]-is[Management.Automation.Language.CommandExpressionAst]-and
          $n.PipelineElements[0].Expression-is[Management.Automation.Language.InvokeMemberExpressionAst]-and
          $n.PipelineElements[0].Expression.Member.Extent.Text-ceq'WaitForExit'-and
          $null-ne$n.PipelineElements[0].Expression.Arguments-and
          ($n.Parent-is[Management.Automation.Language.NamedBlockAst]-or$n.Parent-is[Management.Automation.Language.StatementBlockAst])
        },$true)|ForEach-Object{'line '+$_.Extent.StartLineNumber})
      }
      $seed=Get-ParsedAst 'function f { $home = 1; [object[]]$Matches = @(); foreach ($input in 1) { } }; function g($PID) { }; function h($p) { $null = $p.WaitForExit(1); if ($p.WaitForExit(1)) { }; $p.WaitForExit(1) }'
      $seeded=@(Get-AutomaticAssignments $seed)
      if($seeded.Count-ne4){throw ('Automatic-variable scan missed seeded assignments: '+($seeded-join'; '))}
      $seededLeaks=@(Get-WaitForExitLeaks $seed)
      if($seededLeaks.Count-ne1){throw ('WaitForExit leak scan did not find exactly the seeded leak: '+($seededLeaks-join'; '))}
      foreach($path in @('scripts/verify-in-app-update.ps1','scripts/installed-publisher-ui.ps1','scripts/installed-app-launch.ps1')){
        $ast=Get-ParsedAst ([IO.File]::ReadAllText((Resolve-Path $path)))
        $hits=@(Get-AutomaticAssignments $ast)
        if($hits.Count){throw ($path+' assigns automatic variables: '+($hits-join'; '))}
        $leaks=@(Get-WaitForExitLeaks $ast)
        if($leaks.Count){throw ($path+' leaks WaitForExit output: '+($leaks-join'; '))}
      }
    `;
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', source], { encoding: 'utf8', timeout: 30_000 });
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('parses and rejects ambiguous numeric and installed-file receipts', () => {
    const source = verifierHarness(
      ['ConvertTo-ExactUInt64', 'Get-TextReceipt', 'Get-InstallLocationForm', 'Test-InstallLocation', 'Get-SafeFactValue', 'Get-InstallFactMismatches', 'Assert-InstallFacts'],
      String.raw`
        if((ConvertTo-ExactUInt64 -Value '42' -Kind id)-ne[uint64]42){throw 'Exact integer failed.'}
        foreach($bad in @('0','-1',' 1','1.0','1e2','18446744073709551616')){
          $rejected=$false;try{$null=ConvertTo-ExactUInt64 -Value $bad -Kind id}catch{$rejected=$true}
          if(-not$rejected){throw 'Ambiguous integer passed.'}
        }
        $text=Get-TextReceipt -Value 'é'
        if($text.bytes-ne2-or$text.sha256-cnotmatch'^[A-F0-9]{64}$'){throw 'UTF-8 text receipt failed.'}
        $root=[IO.Path]::GetFullPath('target/mock-installed')
        $receipt=[pscustomobject]@{bytes=[uint64]10;sha256=('A'*64)}
        $facts=[pscustomobject]@{displayName='PDF Workstation';displayVersion='1.2.3';installLocation=('"'+$root+'"');appBytes=[uint64]10;appSha256=('A'*64);fileVersion='1.2.3';productVersion='1.2.3';signatureStatus='Valid';publisher='Joshua Beel';hasTimestamp=$true}
        Assert-InstallFacts -Facts $facts -Version '1.2.3' -InstallRoot $root -ApplicationReceipt $receipt -Publisher 'Joshua Beel' -SignatureStatus 'Valid'
        $wrong=$facts.PSObject.Copy();$wrong.appSha256=('B'*64)
        $rejected=$false;try{Assert-InstallFacts -Facts $wrong -Version '1.2.3' -InstallRoot $root -ApplicationReceipt $receipt -Publisher 'Joshua Beel' -SignatureStatus 'Valid'}catch{$rejected=$true}
        if(-not$rejected){throw 'Wrong installed receipt passed.'}
        function Get-Failure($f,[string]$version='1.2.3',[string]$status='Valid'){try{Assert-InstallFacts -Facts $f -Version $version -InstallRoot $root -ApplicationReceipt $receipt -Publisher 'Joshua Beel' -SignatureStatus $status;return ''}catch{return $_.Exception.Message}}
        $unsigned=$facts.PSObject.Copy();$unsigned.signatureStatus='NotSigned';$unsigned.publisher='';$unsigned.hasTimestamp=$false
        if((Get-Failure $unsigned '1.2.3' 'NotSigned')-ne''){throw 'Exact unsigned baseline facts were rejected.'}
        $mixed=$facts.PSObject.Copy();$mixed.installLocation=('"'+$root.ToUpperInvariant()+'\"')
        if((Get-Failure $mixed)-ne''){throw 'Case-only or trailing-separator install root difference was rejected.'}
        foreach($case in @(
          @{field='installLocation';value=$root;expect='installLocation(form=unquoted)'},
          @{field='installLocation';value=('"'+$root+'\other"');expect='installLocation(form=quoted)'},
          @{field='installLocation';value=('""'+$root+'""');expect='installLocation(form=other)'},
          @{field='installLocation';value=('" '+$root+'"');expect='installLocation(form=quoted)'},
          @{field='installLocation';value='"relative\PDF Workstation"';expect='installLocation(form=quoted)'},
          @{field='displayName';value='PDF Workstation ';expect='displayName'},
          @{field='displayVersion';value='1.2.3.0';expect='displayVersion(actual=1.2.3.0)'},
          @{field='fileVersion';value='1.2.3.0';expect='fileVersion(actual=1.2.3.0)'},
          @{field='productVersion';value='C:\secret path';expect='productVersion(actual=<unprintable>)'},
          @{field='signatureStatus';value='HashMismatch';expect='signatureStatus(actual=HashMismatch)'})){
          $copy=$facts.PSObject.Copy();$copy.($case.field)=$case.value
          $message=Get-Failure $copy
          if(-not$message.StartsWith('Installed application facts do not match the exact expected version and receipt: ')-or-not$message.Contains($case.expect)){throw ('Mismatch was not named: '+$case.expect+' got '+$message)}
          if($message.Contains($root)-or$message.Contains('secret')){throw 'Mismatch message leaked a path or raw value.'}
        }
        $two=$facts.PSObject.Copy();$two.displayVersion='9.9.9';$two.appBytes=[uint64]11
        $message=Get-Failure $two
        if(-not$message.Contains('displayVersion(actual=9.9.9)')-or-not$message.Contains('appBytes')-or$message.Contains('fileVersion')){throw 'Multiple mismatches were not listed exactly.'}
        $noTs=$facts.PSObject.Copy();$noTs.hasTimestamp=$false
        if((Get-Failure $noTs)-cne'Installed application publisher signature facts are incomplete: hasTimestamp.'){throw 'Missing timestamp was not named.'}
      `,
    );
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', source], { encoding: 'utf8', timeout: 20_000 });
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });
});
