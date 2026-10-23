import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { expect, it } from 'vitest';

const workflowPath = '.github/workflows/default-recovery-verification.yml';
const verifierPath = 'scripts/verify-default-recovery.ps1';

function retainedPowerShell(body: string) {
  const root = join('target', 'default-recovery-contract-' + randomUUID());
  mkdirSync(root, { recursive: true });
  const path = join(root, 'contract.ps1');
  writeFileSync(path, body, 'utf8');
  try {
    return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path], { encoding: 'utf8', timeout: 60_000 });
  } finally { rmSync(root, { recursive: true, force: true }); }
}

function absolute(relative: string) {
  return process.cwd().replaceAll("'", "''") + '\\' + relative.replaceAll('/', '\\');
}

function extractFunction(name: string, body: string) {
  return [
    "$ErrorActionPreference='Stop';Set-StrictMode -Version Latest",
    "$t=$null;$e=$null;$a=[Management.Automation.Language.Parser]::ParseFile('" + absolute(verifierPath) + "',[ref]$t,[ref]$e);if($e.Count){throw 'parse'}",
    "$n=$a.Find({param($x)$x-is[Management.Automation.Language.FunctionDefinitionAst]-and$x.Name-ceq'" + name + "'},$true);if(-not$n){throw 'missing'};Invoke-Expression $n.Extent.Text",
    body,
  ].join('\n');
}

function placeholderArguments(overrides: Record<string, string> = {}) {
  const revision = 'a'.repeat(40);
  const values: Record<string, string> = {
    ProjectRoot: 'placeholder-project', TargetSourceRoot: 'placeholder-target', DraftProofRoot: 'placeholder-proof',
    AssetRoot: 'placeholder-assets', WorkRoot: 'placeholder-work', OutputRoot: 'placeholder-output', WebDriverRoot: 'placeholder-drivers',
    WorkflowSourceRevision: revision, DraftProofWorkflowRevision: revision, DraftProofRunId: '1', DraftProofWorkflowId: '2',
    DraftProofArtifactId: '3', DraftProofArtifactName: 'default-draft-verification-' + revision, DraftProofArtifactBytes: '4',
    DraftProofArtifactDigest: 'sha256:' + 'b'.repeat(64), DraftProofReceiptBytes: '5', DraftProofReceiptSha256: 'C'.repeat(64),
    TargetVersion: '0.0.1', TargetTag: 'v0.0.1', TargetSourceRevision: revision, TargetReleaseId: '6', InstallerAssetId: '7',
    InstallerName: 'placeholder-setup.exe', InstallerBytes: '8', InstallerSha256: 'D'.repeat(64), SignatureAssetId: '9',
    SignatureBytes: '10', SignatureSha256: 'E'.repeat(64), ManifestAssetId: '11', ManifestBytes: '12', ManifestSha256: 'F'.repeat(64),
    ExpectedPublisher: 'Placeholder Publisher', ...overrides,
  };
  return Object.entries(values).flatMap(([name, value]) => ['-' + name, value]);
}

function dryRun(overrides: Record<string, string>, extraEnv: Record<string, string> = {}) {
  const env: NodeJS.ProcessEnv = { ...process.env, GITHUB_TOKEN: 'placeholder-token', GH_TOKEN: '', ...extraEnv };
  for (const name of Object.keys(env)) if (/^no_proxy$/i.test(name)) delete env[name];
  return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', absolute(verifierPath), ...placeholderArguments(overrides)], { encoding: 'utf8', timeout: 60_000, env });
}

it('is manual, candidate-bound, immutable-action pinned, and write-scoped only on the draft-reading job', () => {
  const workflow = readFileSync(workflowPath, 'utf8');
  expect(workflow).toContain('name: Manual default installed recovery verification');
  expect(workflow).toContain('workflow_dispatch:');
  expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
  for (const input of ['target_source_revision','target_release_id','installer_asset_id','installer_sha256','signature_asset_id','manifest_asset_id','draft_proof_run_id','draft_proof_workflow_id','draft_proof_artifact_id','draft_proof_workflow_revision','draft_proof_receipt_sha256']) expect(workflow).toContain(input + ':');
  const lf = workflow.replaceAll('\r\n', '\n');
  expect(lf.match(/contents:\s*read/g)?.length).toBe(1);
  expect(lf.match(/contents:\s*write/g)?.length).toBe(1);
  expect(lf.match(/^permissions:/gm)?.length).toBe(1);
  expect(lf.match(/^ +permissions:/gm)?.length).toBe(1);
  expect(lf).toContain('\npermissions:\n  contents: read\n  actions: read\n\njobs:\n  verify-installed-recovery:\n    permissions:\n      contents: write\n      actions: read\n    runs-on: windows-2022\n');
  expect(lf).not.toMatch(/actions:\s*write|id-token:\s*write|secrets\.|AZURE_|TAURI_SIGNING|gh release|git push/);
  expect(workflow.match(/actions\/checkout@11d5960a326750d5838078e36cf38b85af677262/g)?.length).toBe(2);
  expect(workflow).toContain('actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093');
  expect(workflow).toContain('actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02');
});

it('parses and assigns no PowerShell automatic or read-only variable', () => {
  const check = String.raw`
    $ErrorActionPreference='Stop'
    $tokens=$null;$errors=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile('${absolute(verifierPath)}',[ref]$tokens,[ref]$errors)
    if($errors.Count){throw ('Script did not parse: '+(($errors|ForEach-Object{$_.ToString()})-join'; '))}
    $forbidden=@('_','args','error','event','eventargs','eventsubscriber','executioncontext','false','foreach','home','host','input','iscoreclr','islinux','ismacos','iswindows','lastexitcode','matches','myinvocation','nestedpromptlevel','ofs','pid','psboundparameters','pscmdlet','pscommandpath','psculture','psdebugcontext','psedition','psitem','psscriptroot','psuiculture','psversiontable','pwd','sender','shellid','stacktrace','switch','this','true')
    $assigned=@($ast.FindAll({param($node)$node-is[Management.Automation.Language.AssignmentStatementAst]-and$node.Left-is[Management.Automation.Language.VariableExpressionAst]},$true)|ForEach-Object{$_.Left.VariablePath.UserPath.ToLowerInvariant()})
    $hits=@($assigned|Where-Object{$forbidden-contains$_}|Sort-Object -Unique)
    if($hits.Count){throw ('Automatic or read-only variable assignment found: '+($hits-join', '))}
    Write-Output PASS
  `;
  const result = retainedPowerShell(check);
  expect(result.status, result.stderr || result.stdout).toBe(0);
  expect(result.stdout).toContain('PASS');
}, 60_000);

it('resolves every command the verifier reaches through its own importer and dot-source', () => {
  const check = String.raw`
    $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
    $scripts='${absolute('scripts')}'
    $tokens=$null;$errors=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile('${absolute(verifierPath)}',[ref]$tokens,[ref]$errors)
    if($errors.Count){throw 'Script did not parse.'}
    $top=@($ast.EndBlock.Statements)
    foreach($statement in $top){if($statement-is[Management.Automation.Language.FunctionDefinitionAst]){Invoke-Expression $statement.Extent.Text}}
    $imports=0
    foreach($statement in $top){
      if($statement-isnot[Management.Automation.Language.PipelineAst]-or$statement.PipelineElements.Count-ne1){continue}
      $command=$statement.PipelineElements[0]
      if($command-isnot[Management.Automation.Language.CommandAst]){continue}
      if($command.InvocationOperator-eq[Management.Automation.Language.TokenKind]::Dot-or$command.GetCommandName()-ceq'Import-RecoveryFunctions'){
        Invoke-Expression $statement.Extent.Text.Replace('$PSScriptRoot',"'"+$scripts.Replace("'","''")+"'");$imports++
      }
    }
    if($imports-lt2){throw 'Verifier import statements were not found.'}
    $missing=[Collections.Generic.SortedSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $visited=[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $queue=[Collections.Generic.Queue[object]]::new();$queue.Enqueue($ast)
    while($queue.Count){
      $node=$queue.Dequeue()
      $local=[Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
      foreach($definition in $node.FindAll({param($x)$x-is[Management.Automation.Language.FunctionDefinitionAst]},$true)){$null=$local.Add($definition.Name)}
      foreach($call in $node.FindAll({param($x)$x-is[Management.Automation.Language.CommandAst]},$true)){
        $name=$call.GetCommandName()
        if([string]::IsNullOrEmpty($name)-or$local.Contains($name)){continue}
        $resolved=@(Get-Command -Name $name -ErrorAction SilentlyContinue)
        if(-not$resolved.Count){$null=$missing.Add($name);continue}
        if($resolved[0].CommandType-eq'Function'-and$visited.Add($name)){$queue.Enqueue($resolved[0].ScriptBlock.Ast)}
      }
    }
    if($missing.Count){throw ('Unresolved commands: '+(@($missing)-join', '))}
    if(-not$visited.Contains('Invoke-GitHubJson')-or-not$visited.Contains('Assert-Step1ArtifactMetadata')){throw 'Transitive walk did not reach the imported helpers.'}
    Write-Output ('PASS '+$visited.Count)
  `;
  const result = retainedPowerShell(check);
  expect(result.status, result.stderr || result.stdout).toBe(0);
  expect(result.stdout).toContain('PASS');
}, 120_000);

const refusedProxy = 'http://127.0.0.1:9';
const refusedProxyEnv = { HTTPS_PROXY: refusedProxy, HTTP_PROXY: refusedProxy, ALL_PROXY: refusedProxy };

it('binds every placeholder argument and stops at input validation before any request', () => {
  const result = dryRun({ DraftProofRunId: 'placeholder' }, refusedProxyEnv);
  expect(result.status).not.toBe(0);
  expect(result.stderr + result.stdout).toContain('Step 1 run id must be one positive base-10 integer.');
  expect(result.stderr + result.stdout).not.toMatch(/CommandNotFoundException|is not recognized|ParameterBindingException|Cannot overwrite variable/);
  expect(result.stderr + result.stdout).not.toContain('127.0.0.1:9');
}, 60_000);

it('passes input validation with well-formed placeholders and fails at the first GitHub request', () => {
  const result = dryRun({}, refusedProxyEnv);
  const output = result.stderr + result.stdout;
  expect(result.status).not.toBe(0);
  expect(output).toContain('GetResult');
  expect(output).toContain('127.0.0.1:9');
  expect(output).not.toMatch(/must be one positive base-10 integer|identity input is malformed|Target identity is malformed|CommandNotFoundException|is not recognized/);
}, 60_000);

it('binds the owned process identity into every process-bound open-dialog helper', () => {
  const body = String.raw`
    $script:calls=[Collections.Generic.List[object]]::new()
    $stubs=@(
      @('verify-default-recovery.ps1','Wait-RecoveryOracle','1'),
      @('installed-reading-tools.ps1','Get-ReadingProcessUiSurfaceSnapshot','@([pscustomobject]@{surface=1})'),
      @('installed-reading-tools.ps1','Assert-ReadingSurfaceSnapshot','''surface-identities'''),
      @('installed-reading-tools.ps1','Get-ReadingPickerTargetSnapshot','@([pscustomobject]@{target=1})'),
      @('installed-reading-tools.ps1','Assert-ReadingPickerTargetSnapshot','$null'),
      @('installed-app-launch.ps1','Invoke-WebDriverScript','$true'),
      @('installed-reading-tools.ps1','Wait-ReadingProcessBoundPickerTargets','[pscustomobject]@{binding=1}'),
      @('installed-reading-tools.ps1','Submit-ProcessBoundOpenDialog','$null'))
    foreach($stub in $stubs){
      $st=$null;$se=$null;$sa=[Management.Automation.Language.Parser]::ParseFile('${absolute('scripts')}\'+$stub[0],[ref]$st,[ref]$se);if($se.Count){throw 'helper parse'}
      $name=$stub[1];$definition=$sa.Find({param($x)$x-is[Management.Automation.Language.FunctionDefinitionAst]-and$x.Name-ceq$name},$true)
      if(-not$definition-or-not$definition.Body.ParamBlock){throw ('Missing helper signature: '+$name)}
      $record='$h=@{};foreach($k in $PSBoundParameters.Keys){$h[$k]=$PSBoundParameters[$k]};$script:calls.Add([pscustomobject]@{name='''+$name+''';bound=$h})'
      Invoke-Expression ('function '+$name+'{'+[Environment]::NewLine+$definition.Body.ParamBlock.Extent.Text+[Environment]::NewLine+$record+[Environment]::NewLine+$stub[2]+'}')
    }
    $context=[pscustomobject]@{ApplicationProcessId=4242;ApplicationProcessStartUtcTicks=[long]638000000000000001;Deadline=[datetime]::UtcNow.AddMinutes(5);SessionId='session-1'}
    Open-RecoveryFixture $context 'C:\fixture\controlled-source.pdf'
    $expected=@('Wait-RecoveryOracle','Get-ReadingProcessUiSurfaceSnapshot','Assert-ReadingSurfaceSnapshot','Get-ReadingPickerTargetSnapshot','Assert-ReadingPickerTargetSnapshot','Invoke-WebDriverScript','Wait-ReadingProcessBoundPickerTargets','Submit-ProcessBoundOpenDialog')
    if((@($script:calls|ForEach-Object name)-join',')-cne($expected-join',')){throw ('Unexpected helper sequence: '+(@($script:calls|ForEach-Object name)-join','))}
    foreach($call in $script:calls){
      if($call.bound.ContainsKey('ApplicationProcessId')-and($call.bound.ApplicationProcessId-isnot[int]-or$call.bound.ApplicationProcessId-ne4242)){throw ($call.name+' received the wrong process id.')}
      if($call.bound.ContainsKey('ApplicationProcessStartUtcTicks')-and$call.bound.ApplicationProcessStartUtcTicks-ne638000000000000001){throw ($call.name+' received the wrong start ticks.')}
      if($call.name-cne'Invoke-WebDriverScript'-and$call.name-cne'Wait-RecoveryOracle'-and-not$call.bound.ContainsKey('ApplicationProcessId')){throw ($call.name+' was not process-bound.')}
    }
    if($script:calls[0].bound.Kind-cne'Recovery toolbar Open control'-or-not(& $script:calls[0].bound.Predicate 1)-or(& $script:calls[0].bound.Predicate 2)-or$script:calls[0].bound.Script-notmatch'Toggle theme'){throw 'The toolbar Open wait was not bound to exactly one scoped control.'}
    if($script:calls[2].bound.Kind-cne'Recovery open baseline'-or$script:calls[4].bound.Kind-cne'Recovery picker baseline'){throw 'Snapshot kinds were not bound.'}
    if($script:calls[5].bound.SessionId-cne'session-1'-or$script:calls[5].bound.Script-notmatch'Toggle theme'-or$script:calls[7].bound.Path-cne'C:\fixture\controlled-source.pdf'){throw 'Session, scoped click or fixture path was not bound.'}
    Write-Output PASS
  `;
  const result = retainedPowerShell(extractFunction('Open-RecoveryFixture', body));
  expect(result.status, result.stderr || result.stdout).toBe(0);
  expect(result.stdout).toContain('PASS');
}, 60_000);

it('binds exact recovery sources, draft proof, candidate, token clearing, and retained roots', () => {
  const source = readFileSync(verifierPath, 'utf8');
  for (const path of ['src/App.tsx','src/RecoveryOfferDialog.tsx','src/recoveryErrors.ts','src/bridge.ts','src/Organizer.tsx','src-tauri/src/recovery_journal.rs','src-tauri/src/recovery_store.rs','src-tauri/src/recovery_commands.rs','src-tauri/src/service.rs','src-tauri/src/main.rs','src-tauri/tauri.conf.json','src-tauri/resources/welcome.pdf']) expect(source).toContain("'" + path + "'");
  for (const marker of ['Assert-Step1ArtifactMetadata','Assert-DefaultDraftReceipt','Assert-IndependentReceiptParity','Invoke-BoundedSilentInstaller','Assert-InstallFacts','Assert-InstalledDefaultResources','$env:GITHUB_TOKEN=$null','$env:GH_TOKEN=$null']) expect(source).toContain(marker);
  expect(source).not.toMatch(/Remove-Item|rmSync|Invoke-OwnedCleanup|silentUninstall|sessionDeleted=\$true/);
  expect(source).toContain('retained=[ordered]@{profiles=$true;settings=$true;fixtures=$true;results=$true}');
  expect(source).toContain("[IO.FileMode]::CreateNew");
  expect(source).toContain('$stream.Flush($true)');
});

it('requires acknowledged A, exact B recovery and rendered rotation, then clean C', () => {
  const source = readFileSync(verifierPath, 'utf8');
  for (const marker of ['Invoke-RecoveryPhaseA','Invoke-RecoveryPhaseB','Invoke-RecoveryPhaseC','Recovered edits are available','unsaved revision 1','Recovered edits are open. The source PDF was not changed.','renderedRotationVerified=$true','undoToClean=$true','staleOfferAbsent=$true']) expect(source).toContain(marker);
  expect(source).toContain('[int]$v.w-eq[int]$PhaseA.originalHeight');
  expect(source).toContain('[int]$v.h-eq[int]$PhaseA.originalWidth');
  for (const profile of ['profile-a','profile-b','profile-c']) expect(source).toContain("Join-Path $Work '" + profile + "'");
});

it('rejects tampered lifecycle receipts', () => {
  const body = [
    "function Good{[pscustomobject]@{a=[pscustomobject]@{editAcknowledged=$true;revision=1;currentPage=2;abruptTermination=$true;closeActionInvoked=$false};b=[pscustomobject]@{offerRevision=1;keepChosen=$true;recoveredCurrentPage=2;renderedRotationVerified=$true;undoToClean=$true;cleanTabClosed=$true};c=[pscustomobject]@{staleOfferAbsent=$true;cleanOriginal=$true;cleanTabClosed=$true};profilesRetained=$true}}",
    '$null=Assert-RecoveryRuntimeReceipt (Good)',
    "function Reject($v){$failed=$false;try{Assert-RecoveryRuntimeReceipt $v}catch{$failed=$true};if(-not$failed){throw 'tamper accepted'}}",
    '$v=Good;$v.a.closeActionInvoked=$true;Reject $v',
    '$v=Good;$v.a.revision=0;Reject $v',
    '$v=Good;$v.b.renderedRotationVerified=$false;Reject $v',
    '$v=Good;$v.c.staleOfferAbsent=$false;Reject $v',
    'Write-Output PASS',
  ].join('\n');
  const result = retainedPowerShell(extractFunction('Assert-RecoveryRuntimeReceipt', body));
  expect(result.status, result.stderr || result.stdout).toBe(0);
  expect(result.stdout).toContain('PASS');
}, 60_000);

it('force-terminates only a revalidated PID, creation time, and executable path', () => {
  const body = String.raw`
    $script:killed=[Collections.Generic.List[string]]::new()
    $start=[datetime]::new(2026,10,7,12,0,0,[DateTimeKind]::Utc);$app='C:\Program Files\PDF Workstation\pdf-workstation.exe'
    function New-FakeProcess([int]$Id,[datetime]$Start,[string]$Path,[bool]$Exits){
      $p=[pscustomobject]@{Id=$Id;StartTime=$Start;Path=$Path;Exits=$Exits}
      $p|Add-Member ScriptMethod Kill {param($tree)$script:killed.Add([string]$this.Id+':'+[string]$tree)}
      $p|Add-Member ScriptMethod WaitForExit {param($ms)if($ms-ne10000){throw 'unexpected wait bound'};$this.Exits}
      $p
    }
    $script:table=@{
      101=(New-FakeProcess 101 $start $app $true)
      102=(New-FakeProcess 102 $start.AddTicks(1) $app $true)
      103=(New-FakeProcess 103 $start 'C:\Program Files\PDF Workstation\other.exe' $true)
      104=(New-FakeProcess 104 $start $app.ToUpperInvariant() $true)
      105=(New-FakeProcess 105 $start $app $false)
    }
    function Get-Process{[CmdletBinding()]param([int]$Id);if(-not$script:table.ContainsKey($Id)){throw ('No process with id '+$Id)};$script:table[$Id]}
    $ticks=$start.Ticks
    if((Stop-ExactRecoveryApplication -ProcessId 101 -ProcessStartUtcTicks $ticks -ApplicationPath $app)-ne$true){throw 'exact match not accepted'}
    function Reject($id,$expect){$message=$null;try{$null=Stop-ExactRecoveryApplication -ProcessId $id -ProcessStartUtcTicks $ticks -ApplicationPath $app}catch{$message=$_.Exception.Message};if($null-eq$message-or-not$message.Contains($expect)){throw ('process '+$id+' not rejected as expected: '+$message)}}
    Reject 102 'Owned application identity changed.'
    Reject 103 'Owned application identity changed.'
    Reject 104 'Owned application identity changed.'
    Reject 999 'No process with id 999'
    Reject 105 'Owned application termination timed out.'
    if(($script:killed-join',')-cne'101:True,105:True'){throw ('unexpected kills: '+($script:killed-join','))}
    Write-Output PASS
  `;
  const result = retainedPowerShell(extractFunction('Stop-ExactRecoveryApplication', body));
  expect(result.status, result.stderr || result.stdout).toBe(0);
  expect(result.stdout).toContain('PASS');
  const source = readFileSync(verifierPath, 'utf8');
  const fn = source.slice(source.indexOf('function Stop-ExactRecoveryApplication'), source.indexOf('function Open-RecoveryFixture'));
  expect(fn).not.toMatch(/Get-Process\s+-Name|taskkill|Stop-Process/);
}, 60_000);

it('emits only bounded controlled-fixture claims and retains all owned data', () => {
  const source = readFileSync(verifierPath, 'utf8');
  expect(source).toContain('[Text.Encoding]::UTF8.GetByteCount($json)-gt65536');
  for (const marker of ['powerLoss=$false','parentDirectorySync=$false','rollbackAttackResistance=$false','arbitraryPdf=$false','controlledFixture=$true']) expect(source).toContain(marker);
  expect(source).toContain('\\.smacrec');
  expect(source).not.toMatch(/journalPath|sourcePath|password|rawRuntime/);
});
