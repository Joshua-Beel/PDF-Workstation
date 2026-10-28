import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';

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
    expect(workflow).not.toMatch(/secrets\.|github\.token|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING/);
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
    expect(script).not.toMatch(/Authorization|Bearer|api[_-]?key|client[_-]?secret/i);
    expect(script).not.toMatch(/dangerousInsecureTransportProtocol|dangerousAcceptInvalid|allowDowngrades|\.endpoints\(/);
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

  it('drives WebDriver through the shared hosted-proven launch helpers', () => {
    const script = readFileSync(scriptPath, 'utf8');
    expect(script).toMatch(/Set-StrictMode -Version Latest\r?\n\r?\n\. \(Join-Path \$PSScriptRoot 'installed-app-launch\.ps1'\)/);
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
    expect(script).toContain('schemaVersion = 2');
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
    expect(script).toContain('GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING');
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
