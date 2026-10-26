import { describe, expect, it } from 'vitest';
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';
import { tmpdir } from 'node:os';

function runPowerShell(source: string) {
  const root = mkdtempSync(join(tmpdir(), 'smacrobat-installed-launch-'));
  const path = join(root, 'run.ps1');
  try {
    writeFileSync(path, source, 'utf8');
    return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path], {
      encoding: 'utf8', timeout: 60_000, env: { ...process.env, SMACROBAT_TEST_ROOT: root },
    });
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function extractFunctions(script: string, names: string[], body: string) {
  return String.raw`
    $ErrorActionPreference='Stop'; Set-StrictMode -Version Latest
    Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
    . ./scripts/ocr/installer-package.ps1
    . ./scripts/ocr/windows-signing.ps1
    foreach($pair in @(${script.split(',').map((path) => `'${path}'`).join(',')})) {
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path $pair),[ref]$tokens,[ref]$errors)
      if($errors.Count){throw ('Script did not parse: '+$pair)}
      foreach($name in @(${names.map((name) => `'${name}'`).join(',')})) {
        $function=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq$name},$true)
        if($function){Invoke-Expression $function.Extent.Text}
      }
    }
    ${body}
  `;
}

describe('hosted installed signed application launch proof', () => {
  it('starts verifier children with only the exact minimal environment and excludes ambient tokens', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Get-MinimalWindowsProcessEnvironment', 'Start-BoundedDiscardProcess'], String.raw`
      $env:SMACROBAT_AMBIENT_TOKEN='must-not-cross-process-boundary'
      $minimal=Get-MinimalWindowsProcessEnvironment
      $expected=@('COMSPEC','SystemRoot','WINDIR','LOCALAPPDATA','APPDATA','USERPROFILE','TEMP','TMP','PATH','PATHEXT')
      if((@($minimal.Keys|Sort-Object -CaseSensitive)-join ',')-cne(@($expected|Sort-Object -CaseSensitive)-join ',')){throw 'Minimal environment allowlist changed.'}
      $pwsh=(Get-Process -Id $PID).Path
      $command='if($env:SMACROBAT_AMBIENT_TOKEN){exit 41};if([string]::IsNullOrWhiteSpace($env:SystemRoot)-or[string]::IsNullOrWhiteSpace($env:TEMP)){exit 42};exit 0'
      $capture=Start-BoundedDiscardProcess -Path $pwsh -Arguments @('-NoProfile','-NonInteractive','-Command',$command) -Environment $minimal
      try{$capture.Start();if(-not$capture.Process.WaitForExit(10000)){throw 'Minimal-environment child did not exit.'};$capture.Process.WaitForExit();if($capture.Process.ExitCode-ne0){throw ('Minimal-environment child rejected its environment: '+$capture.Process.ExitCode)}}finally{$capture.Dispose()}
      $hostile=[ordered]@{};foreach($name in $expected){$hostile[$name]=$minimal[$name]};$hostile['SMACROBAT_AMBIENT_TOKEN']='secret'
      $rejected=$false;try{Start-BoundedDiscardProcess -Path $pwsh -Arguments @('-NoProfile') -Environment $hostile|Out-Null}catch{$rejected=$_.Exception.Message-ceq'The child-process environment did not match the exact minimal allowlist.'}
      if(-not$rejected){throw 'Unexpected child-process environment entry was accepted.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 20_000);

  it('supplies the exact minimal environment to every production bounded child launch', () => {
    const result = runPowerShell(String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      $calls=0
      foreach($file in @(Get-ChildItem -LiteralPath ./scripts -Recurse -File -Filter '*.ps1')){
        $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($file.FullName,[ref]$tokens,[ref]$errors)
        if($errors.Count){throw ('Production script did not parse: '+$file.Name)}
        foreach($command in @($ast.FindAll({param($node)$node-is[Management.Automation.Language.CommandAst]-and$node.GetCommandName()-ceq'Start-BoundedDiscardProcess'},$true))){
          $calls++;$text=$command.Extent.Text
          if($text-cnotmatch '(?s)-Environment\s+\(Get-MinimalWindowsProcessEnvironment\)'){throw ('Production bounded child launch inherited ambient environment: '+$file.Name)}
        }
      }
      if($calls-lt7){throw 'Production bounded child launch inventory was unexpectedly incomplete.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('bounds trickled response bodies by one deadline and rejects late oracle results', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Get-LaunchRemainingMilliseconds', 'Read-BoundedLoopbackResponseText', 'Wait-WebDriverOracle'], String.raw`
      $script:LaunchPins=[ordered]@{SessionCreationTimeoutMilliseconds=180000;RequestBytesMaximum=1024}
      Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Threading;
using System.Threading.Tasks;
public sealed class TrickleResponseStream : MemoryStream {
    public int Reads;
    public bool Canceled;
    public override async Task<int> ReadAsync(byte[] buffer,int offset,int count,CancellationToken token) {
        Reads++;
        using (token.Register(() => Canceled=true)) {
            await Task.Delay(300,token);
            buffer[offset]=32;
            return 1;
        }
    }
}
public sealed class NonCooperativeResponseStream : MemoryStream {
    public override Task<int> ReadAsync(byte[] buffer,int offset,int count,CancellationToken token) {
        return new TaskCompletionSource<int>().Task;
    }
}
'@
      $stream=[TrickleResponseStream]::new();$rejected=$false;$watch=[Diagnostics.Stopwatch]::StartNew()
      try{Read-BoundedLoopbackResponseText -Stream $stream -Deadline ([datetime]::UtcNow.AddMilliseconds(800))|Out-Null}catch{$rejected=$_.Exception.Message-match'deadline'}finally{$stream.Dispose()}
      if(-not$rejected-or$stream.Reads-lt2-or$watch.ElapsedMilliseconds-gt3000){throw 'Trickled response reset or escaped its absolute deadline.'}
      $stream=[NonCooperativeResponseStream]::new();$rejected=$false;$watch.Restart()
      try{Read-BoundedLoopbackResponseText -Stream $stream -Deadline ([datetime]::UtcNow.AddMilliseconds(60))|Out-Null}catch{$rejected=$_.Exception.Message-match'deadline'}finally{$stream.Dispose()}
      if(-not$rejected-or$watch.ElapsedMilliseconds-gt2000){throw 'A response ignoring cancellation blocked beyond the bounded wait.'}
      $stream=[IO.MemoryStream]::new([Text.Encoding]::UTF8.GetBytes('{"value":true}'))
      try{$text=Read-BoundedLoopbackResponseText -Stream $stream -Deadline ([datetime]::UtcNow.AddSeconds(2));if($text-cne'{"value":true}'){throw 'Timely response was changed.'}}finally{$stream.Dispose()}
      $script:LaunchPins.RequestBytesMaximum=1;$stream=[IO.MemoryStream]::new([byte[]](32,32));$rejected=$false
      try{Read-BoundedLoopbackResponseText -Stream $stream -Deadline ([datetime]::UtcNow.AddSeconds(2))|Out-Null}catch{$rejected=$_.Exception.Message-ceq'WebDriver response exceeded its live byte limit.'}finally{$stream.Dispose()}
      if(-not$rejected){throw 'Response byte cap was bypassed.'}
      $script:oracleCalls=0;$script:predicateCalls=0
      function Invoke-WebDriverScript {param($SessionId,$Script,$Deadline)$script:oracleCalls++;Start-Sleep -Milliseconds 100;[pscustomobject]@{ready=$true}}
      $rejected=$false
      try{Wait-WebDriverOracle -SessionId test -Script test -Deadline ([datetime]::UtcNow.AddMilliseconds(30)) -Kind test -Predicate {param($value)$script:predicateCalls++;$value.ready}|Out-Null}catch{$rejected=$_.Exception.Message-like'test did not become true*'}
      if(-not$rejected-or$oracleCalls-ne1-or$predicateCalls-ne0){throw 'Late response reached or passed the oracle predicate.'}
      function Invoke-WebDriverScript {param($SessionId,$Script,$Deadline)[pscustomobject]@{ready=$true}}
      $rejected=$false
      try{Wait-WebDriverOracle -SessionId test -Script test -Deadline ([datetime]::UtcNow.AddMilliseconds(30)) -Kind test -Predicate {param($value)Start-Sleep -Milliseconds 100;$value.ready}|Out-Null}catch{$rejected=$_.Exception.Message-like'test did not become true*'}
      if(-not$rejected){throw 'Late predicate returned successful proof.'}
      $value=Wait-WebDriverOracle -SessionId test -Script test -Deadline ([datetime]::UtcNow.AddSeconds(2)) -Kind test -Predicate {param($value)$value.ready}
      if(-not$value.ready){throw 'Timely oracle failed.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const production = readFileSync('scripts/installed-app-launch.ps1', 'utf8');
    expect(production).toContain('Read-BoundedLoopbackResponseText -Stream $stream -Deadline $requestDeadline');
    expect(production).toContain('$request.Abort()');
  }, 15_000);

  it('keeps the workflow manual, runner-only, token-scoped, and clean after driver preparation', () => {
    const workflow = readFileSync('.github/workflows/ocr-installer-upgrade.yml', 'utf8');
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('timeout-minutes: 45');
    expect(workflow).toContain('./scripts/setup-installed-app-webdriver.ps1 -OutputRoot $env:WEBDRIVER_ROOT');
    const setup = workflow.slice(workflow.indexOf('- name: Prepare exact hosted WebDriver inputs'), workflow.indexOf('- name: Recheck clean source after driver preparation'));
    expect(setup).not.toMatch(/GITHUB_TOKEN|github\.token|secrets\./);
    const recheck = workflow.slice(workflow.indexOf('- name: Recheck clean source after driver preparation'), workflow.indexOf('- name: Download the exact published'));
    expect(recheck).toContain('git rev-parse HEAD');
    expect(recheck).toContain('git diff --quiet --');
    expect(recheck).toContain('git diff --cached --quiet --');
    expect(workflow.indexOf('- name: Recheck clean source after driver preparation')).toBeLessThan(workflow.indexOf('GITHUB_TOKEN: ${{ github.token }}'));
    expect(workflow).not.toMatch(/secrets\.|AZURE_|TAURI_SIGNING|gh release|create.*release/i);
  });

  it('pins official driver supply chain, archive bounds, isolated Cargo state, and signatures', () => {
    const setup = readFileSync('scripts/setup-installed-app-webdriver.ps1', 'utf8');
    expect(setup).toContain("TauriDriverVersion = '2.0.6'");
    expect(setup).toContain("TauriDriverPackageSha256 = '24DC39BD26A65361C1C8E067636BBFF1D9DD7E2FC58FF874EDFBA33ACAB0E6D0'");
    expect(setup).toContain("TauriDriverPackageOrigin = 'static.crates.io'");
    expect(setup).toContain("EdgeDriverDownloadOrigins = @('msedgedriver.microsoft.com','msedgewebdriverstorage.blob.core.windows.net')");
    expect(setup).toContain('$response.RequestMessage.RequestUri');
    expect(setup).toContain('Assert-SafeCrateEntries');
    expect(setup).toContain("$_[0] -notin @('-','d')");
    expect(setup).toContain('[IO.FileAttributes]::ReparsePoint');
    expect(setup).toContain("$shape -match '/\\.cargo/config(?:\\.toml)?$'");
    expect(setup).toContain("Push-Location -LiteralPath $Source");
    expect(setup).toContain("@('install','--path','.','--locked','--root',$InstallRoot)");
    expect(setup).toContain("EdgePublisher = 'Microsoft Corporation'");
    expect(setup).not.toMatch(/Remove-Item|\.Delete\(|Start-Process/);
  });

  it('rejects unsafe crate paths, extracted reparses, and repository Cargo configuration inheritance', () => {
    const source = extractFunctions(
      'scripts/setup-installed-app-webdriver.ps1',
      ['Assert-SafeCrateEntries', 'Assert-ExtractedCrateTree', 'Invoke-IsolatedCargoInstall'],
      String.raw`
        $script:DriverPins=[ordered]@{TauriDriverVersion='2.0.6'}
        Assert-SafeCrateEntries -Entries @('tauri-driver-2.0.6/Cargo.toml','tauri-driver-2.0.6/Cargo.lock','tauri-driver-2.0.6/src/main.rs')
        foreach($bad in @(
          @('tauri-driver-2.0.6/Cargo.toml','tauri-driver-2.0.6/Cargo.lock','tauri-driver-2.0.6/../escape'),
          @('tauri-driver-2.0.6/Cargo.toml','tauri-driver-2.0.6/Cargo.lock','tauri-driver-2.0.6/src/main.rs','tauri-driver-2.0.6/.cargo/config.toml')
        )){$rejected=$false;try{Assert-SafeCrateEntries -Entries $bad}catch{$rejected=$true};if(-not$rejected){throw 'Unsafe crate entry accepted.'}}
        $root=Join-Path $env:SMACROBAT_TEST_ROOT ('crate-tree-'+[Guid]::NewGuid().ToString('N'));$source=Join-Path $root 'source';$outside=Join-Path $root 'outside'
        [IO.Directory]::CreateDirectory($source)|Out-Null;[IO.Directory]::CreateDirectory($outside)|Out-Null
        New-Item -ItemType Junction -Path (Join-Path $source 'link') -Target $outside|Out-Null
        $rejected=$false;try{Assert-ExtractedCrateTree -Source $source}catch{$rejected=$true};if(-not$rejected){throw 'Extracted reparse point accepted.'}
        $hostile=Join-Path $root 'hostile-repo';$cargoFolder=Join-Path $hostile '.cargo';[IO.Directory]::CreateDirectory($cargoFolder)|Out-Null
        [IO.File]::WriteAllText((Join-Path $cargoFolder 'config.toml'),'[source.crates-io] replace-with="hostile"',[Text.UTF8Encoding]::new($false))
        $driveRoot=Join-Path $root 'drive-root';$cleanPhysical=Join-Path $driveRoot 'clean-source';[IO.Directory]::CreateDirectory($cleanPhysical)|Out-Null
        $driveName='SmacrobatCargoFixture';New-PSDrive -Name $driveName -PSProvider FileSystem -Root $driveRoot|Out-Null;$cleanSource=$driveName+':\clean-source'
        $cargoHome=Join-Path $root 'cargo-home';$install=Join-Path $root 'install';$marker=Join-Path $root 'cargo-provider-called'
        Push-Location $hostile
        try{Invoke-IsolatedCargoInstall -Cargo 'cargo.exe' -Source $cleanSource -InstallRoot $install -CargoHome $cargoHome -ProcessProvider {param($cargo,[Parameter(ValueFromRemainingArguments=$true)][object[]]$arguments)[IO.File]::WriteAllText($marker,'called');$cargoExact=$cargo-ceq'cargo.exe';$locationExact=(Get-Location).Path.Equals($cleanSource,[StringComparison]::OrdinalIgnoreCase);$homeExact=$env:CARGO_HOME-ceq$cargoHome;$countExact=$arguments.Count-eq6;$orderExact=($arguments-join ' ')-ceq('install --path . --locked --root '+$install);if(-not($cargoExact-and$locationExact-and$homeExact-and$countExact-and$orderExact)){throw ('Cargo isolation contract changed; cargoExact={0}; locationExact={1}; homeExact={2}; countExact={3}; orderExact={4}.'-f$cargoExact,$locationExact,$homeExact,$countExact,$orderExact)};0}.GetNewClosure()}finally{Pop-Location;Remove-PSDrive -Name $driveName}
        if(-not(Test-Path -LiteralPath $marker -PathType Leaf)){throw 'Isolated Cargo provider was not invoked.'}
      `,
    );
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('rejects EdgeDriver archive traversal and duplicate canonical executables', () => {
    const source = extractFunctions('scripts/setup-installed-app-webdriver.ps1', ['Expand-ExactEdgeDriver'], String.raw`
      Add-Type -AssemblyName System.IO.Compression.FileSystem
      $script:DriverPins=[ordered]@{MaximumEdgeEntries=16;MaximumEdgeArchiveBytes=32MB;MaximumEdgeExpandedBytes=64MB}
      $root=Join-Path $env:SMACROBAT_TEST_ROOT ('edge-archive-'+[Guid]::NewGuid().ToString('N'));[IO.Directory]::CreateDirectory($root)|Out-Null
      function New-TestArchive([string]$path,[string[]]$names){$zip=[IO.Compression.ZipFile]::Open($path,[IO.Compression.ZipArchiveMode]::Create);try{foreach($name in $names){$entry=$zip.CreateEntry($name);$stream=$entry.Open();try{$stream.WriteByte(1)}finally{$stream.Dispose()}}}finally{$zip.Dispose()}}
      foreach($case in @(
        [pscustomobject]@{name='traversal';entries=@('../msedgedriver.exe')},
        [pscustomobject]@{name='duplicate';entries=@('msedgedriver.exe','msedgedriver.exe')}
      )){$archive=Join-Path $root ($case.name+'.zip');New-TestArchive $archive $case.entries;$rejected=$false;try{Expand-ExactEdgeDriver -Archive $archive -Destination (Join-Path $root $case.name)|Out-Null}catch{$rejected=$true};if(-not$rejected){throw ('Unsafe EdgeDriver archive accepted: '+$case.name)}}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('requires exact driver receipt origins, toolchain versions, runtime matching, and Microsoft trust', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Assert-LaunchExactProperties', 'Assert-LaunchReceiptValue', 'Assert-WebDriverReceipt'], String.raw`
      $script:LaunchPins=[ordered]@{TauriDriverVersion='2.0.6';TauriDriverPackageSha256='24DC39BD26A65361C1C8E067636BBFF1D9DD7E2FC58FF874EDFBA33ACAB0E6D0';EdgePublisher='Microsoft Corporation';OcrUiTextBytesMaximum=65536}
      $receipt=[pscustomobject]@{schemaVersion=1;tauriDriverSource=[pscustomobject]@{version='2.0.6';origin='static.crates.io';bytes=[uint64]1;sha256=$script:LaunchPins.TauriDriverPackageSha256};tauriDriver=[pscustomobject]@{version='2.0.6';cargoVersion='cargo 1.91.0 (abc 2026-01-01)';rustcVersion='rustc 1.91.0 (abc 2026-01-01)';bytes=[uint64]2;sha256=('A'*64)};webView2RuntimeVersion='151.0.4129.50';edgeDriverArchive=[pscustomobject]@{origin='msedgedriver.microsoft.com';bytes=[uint64]3;sha256=('B'*64)};edgeDriver=[pscustomobject]@{version='151.0.4129.78';bytes=[uint64]4;sha256=('C'*64);signatureStatus='Valid';publisher='Microsoft Corporation';hasTimestamp=$true}}
      Assert-WebDriverReceipt $receipt
      foreach($mutation in @('origin','runtime','publisher')){$copy=$receipt|ConvertTo-Json -Depth 8|ConvertFrom-Json;if($mutation-eq'origin'){$copy.edgeDriverArchive.origin='example.invalid'}elseif($mutation-eq'runtime'){$copy.edgeDriver.version='150.0.1.1'}else{$copy.edgeDriver.publisher='Someone'};$rejected=$false;try{Assert-WebDriverReceipt $copy}catch{$rejected=$true};if(-not$rejected){throw ('Unsafe receipt accepted: '+$mutation)}}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('binds the live native-driver status and fresh profile when vendor capabilities are absent', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Assert-NativeDriverStatus', 'Wait-NativeDriverStatus', 'Get-ExactProfileBinding', 'Get-OwnedProfileBinding', 'Assert-TrustedConsoleHostTopology', 'Assert-OwnedLaunchExecutables', 'Get-UniqueOwnedLaunchProcesses'], String.raw`
      $script:LaunchPins=[ordered]@{OwnedProcessMaximum=128;ConsoleHostPublisher='Microsoft Windows'}
      $version='151.0.4129.78';$status=[pscustomobject]@{value=[pscustomobject]@{ready=$true;build=[pscustomobject]@{version=($version+' (trusted-build)')}}}
      if((Assert-NativeDriverStatus -Status $status -ExpectedVersion $version)-cne$version){throw 'Native status version was not bound.'}
      foreach($bad in @(
        [pscustomobject]@{value=[pscustomobject]@{ready=$true;build=[pscustomobject]@{version='150.0.1.1'}}},
        [pscustomobject]@{value=[pscustomobject]@{ready=$false;build=[pscustomobject]@{version=$version}}},
        [pscustomobject]@{value=[pscustomobject]@{ready=$true}}
      )){$rejected=$false;try{Assert-NativeDriverStatus -Status $bad -ExpectedVersion $version}catch{$rejected=$true};if(-not$rejected){throw 'Unsafe native status accepted.'}}
      $responses=[Collections.Generic.Queue[object]]::new();$responses.Enqueue('not-ready');$responses.Enqueue($status);$provider={param($deadline)$next=$responses.Dequeue();if($next-is[string]){throw $next};$next}.GetNewClosure()
      if((Wait-NativeDriverStatus -ExpectedVersion $version -Deadline ([datetime]::UtcNow.AddSeconds(2)) -TauriDriver ([pscustomobject]@{HasExited=$false}) -StatusProvider $provider)-cne$version-or$responses.Count-ne0){throw 'Native readiness retry changed.'}
      $profile=Join-Path $env:SMACROBAT_TEST_ROOT ('profile-binding-'+[Guid]::NewGuid().ToString('N'));$settings=Join-Path $env:SMACROBAT_TEST_ROOT ('settings-binding-'+[Guid]::NewGuid().ToString('N'));$applicationProfile=Join-Path $settings 'EBWebView'
      if((Get-ExactProfileBinding -Candidate $profile -RequestedProfile $profile -SettingsRoot $settings)-cne'requested-profile'){throw 'Exact requested profile was not bound.'}
      if((Get-ExactProfileBinding -Candidate $applicationProfile -RequestedProfile $profile -SettingsRoot $settings)-cne'tauri-app-settings-ebwebview'){throw 'Exact Tauri application profile was not bound.'}
      $requestedWebView=Join-Path $profile 'EBWebView';if((Get-ExactProfileBinding -Candidate $requestedWebView -RequestedProfile $profile -SettingsRoot $settings)-cne'requested-ebwebview'){throw 'Exact evidence-backed requested EBWebView profile was not bound.'}
      foreach($otherRequested in @((Join-Path $profile 'Other'),(Join-Path $profile 'Nested/Child'),(Join-Path $profile 'ebwebview'))){$rejected=$false;try{Get-ExactProfileBinding -Candidate $otherRequested -RequestedProfile $profile -SettingsRoot $settings}catch{$rejected=$true;if($_.Exception.Message-cnotmatch "relation 'requested-root-other'"-or$_.Exception.Message-match[regex]::Escape($otherRequested)){throw 'Generic requested-root relation diagnostic changed or leaked a path.'}};if(-not$rejected){throw 'Sibling, nested, or wrong-case requested-root descendant was accepted.'}}
      foreach($badProfile in @($settings,(Join-Path $settings 'Other'),'C:\wrong')){$rejected=$false;try{Get-ExactProfileBinding -Candidate $badProfile -RequestedProfile $profile -SettingsRoot $settings}catch{$rejected=$true;if($_.Exception.Message-match[regex]::Escape($badProfile)){throw 'Profile mismatch diagnostic leaked a path.'}};if(-not$rejected){throw 'Inexact controlled profile was accepted.'}}
      $owned=@([pscustomobject]@{Path='C:\Program Files (x86)\Microsoft\EdgeWebView\Application\msedgewebview2.exe';CommandLine=('msedgewebview2.exe --user-data-dir="'+$applicationProfile+'" --type=renderer')},[pscustomobject]@{Path='C:\Program Files (x86)\Microsoft\EdgeWebView\Application\msedgewebview2.exe';CommandLine='msedgewebview2.exe --type=gpu-process'})
      if((Get-OwnedProfileBinding -Owned $owned -RequestedProfile $profile -SettingsRoot $settings)-cne'owned-webview-tauri-app-settings-ebwebview'){throw 'Owned Tauri profile was not bound.'}
      foreach($badOwned in @(@(),@([pscustomobject]@{Path='C:\msedgewebview2.exe';CommandLine='msedgewebview2.exe --type=renderer'}),@([pscustomobject]@{Path='C:\msedgewebview2.exe';CommandLine='msedgewebview2.exe --user-data-dir=C:\wrong'}),@([pscustomobject]@{Path='C:\msedgewebview2.exe';CommandLine=('msedgewebview2.exe --user-data-dir="'+$profile+'" --user-data-dir="'+$profile+'"')}),@([pscustomobject]@{Path='C:\msedgewebview2.exe';CommandLine=('msedgewebview2.exe --user-data-dir="'+$profile+'"')},[pscustomobject]@{Path='C:\msedgewebview2.exe';CommandLine=('msedgewebview2.exe --user-data-dir="'+$applicationProfile+'"')}))){$rejected=$false;try{Get-OwnedProfileBinding -Owned $badOwned -RequestedProfile $profile -SettingsRoot $settings}catch{$rejected=$true};if(-not$rejected){throw 'Missing, wrong, duplicate, or inconsistent owned profile evidence accepted.'}}
      $app=Join-Path $profile 'pdf-workstation.exe';$edge=Join-Path $profile 'msedgedriver.exe';$engine=Join-Path $profile 'tesseract.exe';$executables=@([pscustomobject]@{Path=$app},[pscustomobject]@{Path=$edge},[pscustomobject]@{Path=$engine})
      Assert-OwnedLaunchExecutables -Owned $executables -ApplicationPath $app -EdgeDriverPath $edge -OcrEnginePath $engine
      foreach($badExecutables in @(@([pscustomobject]@{Path=$app},[pscustomobject]@{Path='C:\wrong\msedgedriver.exe'}),@([pscustomobject]@{Path=$app},[pscustomobject]@{Path=$edge},[pscustomobject]@{Path=$edge}),@([pscustomobject]@{Path=$app},[pscustomobject]@{Path=$edge},[pscustomobject]@{Path='C:\wrong\tesseract.exe'}))){$rejected=$false;try{Assert-OwnedLaunchExecutables -Owned $badExecutables -ApplicationPath $app -EdgeDriverPath $edge -OcrEnginePath $engine}catch{$rejected=$true};if(-not$rejected){throw 'Wrong or duplicate owned executable accepted.'}}
      $started=[datetime]::UtcNow.ToUniversalTime().Ticks;$edgeOne=[pscustomobject]@{ProcessId=41;Path=$edge;StartTicks=$started};$edgeTwo=[pscustomobject]@{ProcessId=42;Path=$edge;StartTicks=$started};$deduped=@(Get-UniqueOwnedLaunchProcesses -Processes @($edgeOne,$edgeOne,$edgeTwo));if($deduped.Count-ne2){throw 'Repeated exact process snapshots were not identity-deduplicated.'};$rejected=$false;try{Assert-OwnedLaunchExecutables -Owned @([pscustomobject]@{Path=$app},$deduped,[pscustomobject]@{Path=$engine}) -ApplicationPath $app -EdgeDriverPath $edge -OcrEnginePath $engine}catch{$rejected=$true};if(-not$rejected){throw 'Two distinct same-path process identities were collapsed or accepted.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('allows only the exact signed System32 console-host root and WebView topology', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Assert-TrustedConsoleHostTopology', 'Assert-OwnedLaunchExecutables'], String.raw`
      $script:LaunchPins=[ordered]@{OwnedProcessMaximum=128;ConsoleHostPublisher='Microsoft Windows'}
      $system=[Environment]::GetFolderPath([Environment+SpecialFolder]::System);$conhost=Join-Path $system 'conhost.exe'
      $signature={param($path)[pscustomobject]@{Status='Valid';Publisher='Microsoft Windows';HasTimestamp=$true}}
      $version={param($path)[pscustomobject]@{CompanyName='Microsoft Corporation';ProductName=('Microsoft'+[char]0x00AE+' Windows'+[char]0x00AE+' Operating System');InternalName='ConHost';FileDescription='Console Window Host'}}
      $app='C:\fixture\pdf-workstation.exe';$edge='C:\fixture\msedgedriver.exe';$ocr='C:\fixture\tesseract.exe';$web='C:\fixture\msedgewebview2.exe'
      $core=@([pscustomobject]@{ProcessId=101;ParentProcessId=900;Path=$app},[pscustomobject]@{ProcessId=102;ParentProcessId=900;Path=$edge},[pscustomobject]@{ProcessId=103;ParentProcessId=101;Path=$web},[pscustomobject]@{ProcessId=104;ParentProcessId=101;Path=$ocr})
      $console=@([pscustomobject]@{ProcessId=105;ParentProcessId=900;Path=$conhost},[pscustomobject]@{ProcessId=106;ParentProcessId=103;Path=$conhost})
      $ocrConsole=[pscustomobject]@{ProcessId=109;ParentProcessId=104;Path=$conhost}
      $valid=@($core+$console)
      $sequentialRoot=@($core+$console[0]+[pscustomobject]@{ProcessId=107;ParentProcessId=900;Path=$conhost})
      $sequentialWebView=@($core+$console[1]+[pscustomobject]@{ProcessId=108;ParentProcessId=103;Path=$conhost})
      $sequentialBoth=@($valid+[pscustomobject]@{ProcessId=107;ParentProcessId=900;Path=$conhost},[pscustomobject]@{ProcessId=108;ParentProcessId=103;Path=$conhost})
      if((Assert-TrustedConsoleHostTopology -Owned $valid -RootProcessId 900 -SystemDirectory $system -SignatureProvider $signature -VersionInfoProvider $version)-ne2){throw 'Trusted console-host topology was not accepted.'}
      if((Assert-TrustedConsoleHostTopology -Owned $valid -RootProcessId 900 -SystemDirectory $system)-ne2){throw 'The installed System32 console-host identity was not accepted.'}
      Assert-OwnedLaunchExecutables -Owned $valid -ApplicationPath $app -EdgeDriverPath $edge -OcrEnginePath $ocr -RootProcessId 900 -SystemDirectory $system -ConsoleHostSignatureProvider $signature -ConsoleHostVersionInfoProvider $version
      Assert-OwnedLaunchExecutables -Owned @($valid+$ocrConsole) -ApplicationPath $app -EdgeDriverPath $edge -OcrEnginePath $ocr -RootProcessId 900 -SystemDirectory $system -ConsoleHostSignatureProvider $signature -ConsoleHostVersionInfoProvider $version
      $requiredAdditionalRejected=$false;try{Assert-TrustedConsoleHostTopology -Owned @($valid+$ocrConsole) -RootProcessId 900 -SystemDirectory $system -SignatureProvider $signature -VersionInfoProvider $version -AdditionalTrustedParentPath $ocr -AdditionalTrustedParentCategory 'ocr-engine'|Out-Null}catch{$requiredAdditionalRejected=$true};if(-not$requiredAdditionalRejected){throw 'Required root and WebView mode accepted an additional parent category.'}
      Assert-OwnedLaunchExecutables -Owned $core -ApplicationPath $app -EdgeDriverPath $edge -OcrEnginePath $ocr
      foreach($subset in @(
        [pscustomobject]@{owned=@($core);expected=0},
        [pscustomobject]@{owned=@($core+$console[0]);expected=1},
        [pscustomobject]@{owned=@($core+$console[1]);expected=1},
        [pscustomobject]@{owned=$valid;expected=2},
        [pscustomobject]@{owned=$sequentialRoot;expected=2},
        [pscustomobject]@{owned=$sequentialWebView;expected=2},
        [pscustomobject]@{owned=$sequentialBoth;expected=4}
      )){
        if((Assert-TrustedConsoleHostTopology -Owned $subset.owned -RootProcessId 900 -AllowAbsent -SystemDirectory $system -SignatureProvider $signature -VersionInfoProvider $version)-ne$subset.expected){throw 'A permitted optional console-host subset was rejected.'}
        Assert-OwnedLaunchExecutables -Owned $subset.owned -ApplicationPath $app -EdgeDriverPath $edge -OcrEnginePath $ocr -RootProcessId 900 -SystemDirectory $system -ConsoleHostSignatureProvider $signature -ConsoleHostVersionInfoProvider $version
      }
      foreach($oneHost in @(@($core+$console[0]),@($core+$console[1]))){$rejected=$false;try{Assert-TrustedConsoleHostTopology -Owned $oneHost -RootProcessId 900 -SystemDirectory $system -SignatureProvider $signature -VersionInfoProvider $version|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Required mode accepted an incomplete console-host pair.'}}
      if((Assert-TrustedConsoleHostTopology -Owned $sequentialBoth -RootProcessId 900 -SystemDirectory $system -SignatureProvider $signature -VersionInfoProvider $version)-ne4){throw 'Required mode rejected sequential trusted console-host identities.'}
      $cases=@(
        @($core+[pscustomobject]@{ProcessId=105;ParentProcessId=999;Path=$conhost},$console[1]),
        @($core+[pscustomobject]@{ProcessId=105;ParentProcessId=900;Path='C:\fixture\conhost.exe'},$console[1]),
        @($core+[pscustomobject]@{ProcessId=105;ParentProcessId=900;Path=$conhost},[pscustomobject]@{ProcessId=105;ParentProcessId=103;Path=$conhost}),
        @($core+[pscustomobject]@{ProcessId=0;ParentProcessId=900;Path=$conhost},$console[1]),
        @($valid+[pscustomobject]@{ProcessId=110;ParentProcessId=101;Path='C:\fixture\foreign.exe'},[pscustomobject]@{ProcessId=111;ParentProcessId=110;Path=$conhost})
      )
      foreach($case in $cases){$rejected=$false;try{Assert-TrustedConsoleHostTopology -Owned $case -RootProcessId 900 -AllowAbsent -SystemDirectory $system -SignatureProvider $signature -VersionInfoProvider $version -AdditionalTrustedParentPath $ocr -AdditionalTrustedParentCategory 'ocr-engine'|Out-Null}catch{$rejected=$true;if($_.Exception.Message-match'fixture|999|900|105|106|107|109|110|111'){throw 'Console-host rejection leaked captured identity.'}};if(-not$rejected){throw 'Unsafe optional console-host topology was accepted.'}}
      foreach($providerCase in @(
        [pscustomobject]@{signature={param($path)[pscustomobject]@{Status='Valid';Publisher='Other';HasTimestamp=$true}};version=$version},
        [pscustomobject]@{signature=$signature;version={param($path)[pscustomobject]@{CompanyName='Microsoft Corporation';ProductName='Other';InternalName='ConHost';FileDescription='Console Window Host'}}}
      )){$rejected=$false;try{Assert-TrustedConsoleHostTopology -Owned $valid -RootProcessId 900 -SystemDirectory $system -SignatureProvider $providerCase.signature -VersionInfoProvider $providerCase.version|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Untrusted console-host signature or product identity was accepted.'}}
      $boundedNoHost=@(1..128|ForEach-Object{[pscustomobject]@{ProcessId=1000+$_;ParentProcessId=900;Path=('C:\fixture\worker-'+$_+'.exe')}})
      if((Assert-TrustedConsoleHostTopology -Owned $boundedNoHost -RootProcessId 900 -AllowAbsent -SystemDirectory $system -SignatureProvider $signature -VersionInfoProvider $version)-ne0){throw 'The pinned no-host inventory boundary was rejected.'}
      foreach($count in @(129,130)){$oversizedNoHost=@(1..$count|ForEach-Object{[pscustomobject]@{ProcessId=1000+$_;ParentProcessId=900;Path=('C:\fixture\worker-'+$_+'.exe')}});$rejected=$false;try{Assert-TrustedConsoleHostTopology -Owned $oversizedNoHost -RootProcessId 900 -AllowAbsent -SystemDirectory $system -SignatureProvider $signature -VersionInfoProvider $version|Out-Null}catch{$rejected=$true};if(-not$rejected){throw ('The pinned process cap accepted a no-host inventory of '+$count+'.')}}
      $rejected=$false;try{Assert-OwnedLaunchExecutables -Owned @($valid+[pscustomobject]@{ProcessId=108;ParentProcessId=900;Path='C:\private\helper.exe'}) -ApplicationPath $app -EdgeDriverPath $edge -OcrEnginePath $ocr -RootProcessId 900 -SystemDirectory $system -ConsoleHostSignatureProvider $signature -ConsoleHostVersionInfoProvider $version}catch{$rejected=$true};if(-not$rejected){throw 'An unrelated owned executable was accepted.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const launchSource = readFileSync('scripts/installed-app-launch.ps1', 'utf8');
    expect(launchSource.match(/-AdditionalTrustedParentPath \$OcrEnginePath -AdditionalTrustedParentCategory 'ocr-engine'/g)).toHaveLength(1);
    for (const path of ['scripts/installed-reading-tools.ps1', 'scripts/installed-persistence.ps1', 'scripts/installed-print-dialog.ps1']) {
      expect(readFileSync(path, 'utf8')).not.toContain('AdditionalTrustedParent');
    }
  }, 15_000);

  it('validates exact UI/runtime/cleanup results before producing sanitized launch evidence', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Assert-LaunchExactProperties', 'Assert-LaunchReceiptValue', 'Assert-WebDriverReceipt', 'Invoke-InstalledAppLaunchSmoke'], String.raw`
      function Assert-FileReceipt{param([string]$Path,[uint64]$Bytes,[string]$Sha256,[string]$Kind)};function Assert-TrustedWindowsSignature{param([string]$Path,[string]$ExpectedPublisher);[pscustomobject]@{}};function Get-LaunchProcessSnapshot{return @()}
      $script:LaunchPins=[ordered]@{TauriDriverVersion='2.0.6';TauriDriverPackageSha256='24DC39BD26A65361C1C8E067636BBFF1D9DD7E2FC58FF874EDFBA33ACAB0E6D0';EdgePublisher='Microsoft Corporation';OcrUiTextBytesMaximum=65536}
      $root=Join-Path $env:SMACROBAT_TEST_ROOT ('launch-wrapper-'+[Guid]::NewGuid().ToString('N'));$drivers=Join-Path $root 'drivers';[IO.Directory]::CreateDirectory((Join-Path $drivers 'tauri-driver-install/bin'))|Out-Null;[IO.Directory]::CreateDirectory((Join-Path $drivers 'edge-driver'))|Out-Null
      $receipt=[ordered]@{schemaVersion=1;tauriDriverSource=[ordered]@{version='2.0.6';origin='static.crates.io';bytes=[uint64]1;sha256=$script:LaunchPins.TauriDriverPackageSha256};tauriDriver=[ordered]@{version='2.0.6';cargoVersion='cargo 1.91.0 (abc 2026-01-01)';rustcVersion='rustc 1.91.0 (abc 2026-01-01)';bytes=[uint64]2;sha256=('A'*64)};webView2RuntimeVersion='151.0.4129.50';edgeDriverArchive=[ordered]@{origin='msedgedriver.microsoft.com';bytes=[uint64]3;sha256=('B'*64)};edgeDriver=[ordered]@{version='151.0.4129.78';bytes=[uint64]4;sha256=('C'*64);signatureStatus='Valid';publisher='Microsoft Corporation';hasTimestamp=$true}}
      [IO.File]::WriteAllText((Join-Path $drivers 'webdriver-receipt.json'),($receipt|ConvertTo-Json -Depth 8),[Text.UTF8Encoding]::new($false))
      function Invoke-TestLaunch([string]$label,[scriptblock]$provider,[bool]$extraSettings=$false,[bool]$preexistingChild=$false,[bool]$reparseAncestor=$false){
        $env:LOCALAPPDATA=Join-Path $root ('local-'+$label);if($reparseAncestor){$outside=Join-Path $root ('outside-'+$label);[IO.Directory]::CreateDirectory($outside)|Out-Null;New-Item -ItemType Junction -Path $env:LOCALAPPDATA -Target $outside|Out-Null}
        $settings=Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop';[IO.Directory]::CreateDirectory($settings)|Out-Null
        $sentinel=Join-Path $settings 'upgrade-sentinel.json';[IO.File]::WriteAllText($sentinel,'sentinel',[Text.UTF8Encoding]::new($false));if($extraSettings){$extra=Join-Path $settings '.hidden-extra';[IO.File]::WriteAllText($extra,'extra',[Text.UTF8Encoding]::new($false));[IO.File]::SetAttributes($extra,[IO.FileAttributes]::Hidden)};if($preexistingChild){$child=Join-Path (Join-Path $root ('profile-'+$label)) 'EBWebView';[IO.Directory]::CreateDirectory($child)|Out-Null;[IO.File]::SetAttributes($child,[IO.FileAttributes]::Directory-bor[IO.FileAttributes]::Hidden)}
        Invoke-InstalledAppLaunchSmoke -ApplicationPath (Join-Path $root 'app.exe') -ApplicationReceipt ([pscustomobject]@{bytes=[uint64]5;sha256=('D'*64)}) -OcrEnginePath (Join-Path $root 'tesseract.exe') -OcrEngineReceipt ([pscustomobject]@{bytes=[uint64]6;sha256=('6'*64)}) -WelcomePath (Join-Path $root 'welcome.pdf') -WelcomeReceipt ([pscustomobject]@{bytes=[uint64]7;sha256=('7'*64)}) -WebDriverRoot $drivers -ProfileRoot (Join-Path $root ('profile-'+$label)) -ApplicationSettingsRoot $settings -SettingsSentinelPath $sentinel -SettingsSentinelBytes ([uint64]8) -SettingsSentinelSha256 ('E'*64) -RunnerTemp $root -ExpectedPublisher 'Joshua Beel' -ProcessProvider $provider
      }
      function New-TestLaunchResult([string]$binding,[string]$expectedVersion){[pscustomobject]@{nativeDriverVersion=$expectedVersion;driverVersionBinding='native-status';returnedRuntimeVersion='151.0.4129.50';profileBindingMethod=$binding;sessionCapabilityKeys=@('browserName','browserVersion');title='PDF Workstation';homeButton=$true;ocrCapabilityStatus='Available';sampleName='welcome.pdf';samplePages=6;renderedPageWidth=800;renderedPageHeight=1000;renderedPageBlob=$true;ocrPage=1;ocrDialogTitle='Recognize text on page 1';ocrTextUtf8Bytes=12;ocrTextSha256=('F'*64);ocrSourceUiPreserved=$true;sessionDeleted=$true;ownedProcessTreeStopped=$true;relevantProcessesRemaining=0}}
      $provider={param($app,$tauri,$edge,$ocrEngine,$profile,$settings,$applicationProfile,$expectedVersion)[IO.Directory]::CreateDirectory($applicationProfile)|Out-Null;[IO.File]::WriteAllText((Join-Path $applicationProfile 'state'),'active',[Text.UTF8Encoding]::new($false));New-TestLaunchResult 'owned-webview-tauri-app-settings-ebwebview' $expectedVersion}
      $result=Invoke-TestLaunch -label 'good' -provider $provider
      if($result.profile.state-cne'controlled-runner-owned'-or$result.profile.binding-cne'owned-webview-tauri-app-settings-ebwebview'-or$result.profile.prelaunchSettingsEntries-ne1-or-not$result.profile.sentinelPreserved-or$result.drivers.edgeDriver.versionBinding-cne'native-status'-or$result.sample.pages-ne6-or-not$result.currentPageOcr.sourceUiPreserved-or$result.currentPageOcr.textUtf8Bytes-ne12-or$result.currentPageOcr.accuracyVerified-ne$false-or-not$result.cleanup.sessionDeleted-or($result|ConvertTo-Json -Depth 10)-match'raw recognized'){throw 'Sanitized launch result changed.'}
      $requestedProvider={param($app,$tauri,$edge,$ocrEngine,$profile,$settings,$applicationProfile,$expectedVersion)$child=Join-Path $profile 'EBWebView';[IO.Directory]::CreateDirectory($child)|Out-Null;[IO.File]::WriteAllText((Join-Path $child 'state'),'active',[Text.UTF8Encoding]::new($false));New-TestLaunchResult 'owned-webview-requested-ebwebview' $expectedVersion}
      $requestedResult=Invoke-TestLaunch -label 'requested-good' -provider $requestedProvider
      if($requestedResult.profile.binding-cne'owned-webview-requested-ebwebview'-or$requestedResult.profile.state-cne'controlled-runner-owned'){throw 'Evidence-backed requested EBWebView result changed.'}
      $rejected=$false;try{Invoke-TestLaunch -label 'extra' -provider $provider -extraSettings $true}catch{$rejected=$true};if(-not$rejected){throw 'Unexpected hidden settings asset was accepted.'}
      $rejected=$false;try{Invoke-TestLaunch -label 'preexisting-child' -provider $requestedProvider -preexistingChild $true}catch{$rejected=$true};if(-not$rejected){throw 'Preexisting hidden requested EBWebView child was accepted.'}
      $hiddenSiblingProvider={param($app,$tauri,$edge,$ocrEngine,$profile,$settings,$applicationProfile,$expectedVersion)$child=Join-Path $profile 'EBWebView';[IO.Directory]::CreateDirectory($child)|Out-Null;[IO.File]::WriteAllText((Join-Path $child 'state'),'active',[Text.UTF8Encoding]::new($false));$extra=Join-Path $profile '.hidden-extra';[IO.File]::WriteAllText($extra,'extra',[Text.UTF8Encoding]::new($false));[IO.File]::SetAttributes($extra,[IO.FileAttributes]::Hidden);New-TestLaunchResult 'owned-webview-requested-ebwebview' $expectedVersion}
      $emptyChildProvider={param($app,$tauri,$edge,$ocrEngine,$profile,$settings,$applicationProfile,$expectedVersion)[IO.Directory]::CreateDirectory((Join-Path $profile 'EBWebView'))|Out-Null;New-TestLaunchResult 'owned-webview-requested-ebwebview' $expectedVersion}
      $reparseChildProvider={param($app,$tauri,$edge,$ocrEngine,$profile,$settings,$applicationProfile,$expectedVersion)$outside=Join-Path $root 'reparse-child-outside';[IO.Directory]::CreateDirectory($outside)|Out-Null;[IO.File]::WriteAllText((Join-Path $outside 'state'),'active',[Text.UTF8Encoding]::new($false));New-Item -ItemType Junction -Path (Join-Path $profile 'EBWebView') -Target $outside|Out-Null;New-TestLaunchResult 'owned-webview-requested-ebwebview' $expectedVersion}
      $settingsExtraProvider={param($app,$tauri,$edge,$ocrEngine,$profile,$settings,$applicationProfile,$expectedVersion)$child=Join-Path $profile 'EBWebView';[IO.Directory]::CreateDirectory($child)|Out-Null;[IO.File]::WriteAllText((Join-Path $child 'state'),'active',[Text.UTF8Encoding]::new($false));[IO.File]::WriteAllText((Join-Path $settings 'unexpected'),'extra',[Text.UTF8Encoding]::new($false));New-TestLaunchResult 'owned-webview-requested-ebwebview' $expectedVersion}
      foreach($negative in @([pscustomobject]@{label='hidden-sibling';provider=$hiddenSiblingProvider},[pscustomobject]@{label='empty-child';provider=$emptyChildProvider},[pscustomobject]@{label='reparse-child';provider=$reparseChildProvider},[pscustomobject]@{label='settings-extra';provider=$settingsExtraProvider})){$rejected=$false;try{Invoke-TestLaunch -label $negative.label -provider $negative.provider}catch{$rejected=$true};if(-not$rejected){throw ('Unsafe requested EBWebView state was accepted: '+$negative.label)}}
      $rejected=$false;try{Invoke-TestLaunch -label 'reparse-ancestor' -provider $requestedProvider -reparseAncestor $true}catch{$rejected=$true};if(-not$rejected){throw 'Reparse-backed settings ancestor was accepted.'}
      foreach($mutation in @('runtime','native-version','profile','ocr-page','ocr-empty','ocr-oversize','ocr-dirty')){$bad={param($app,$tauri,$edge,$ocrEngine,$profile,$settings,$applicationProfile,$expectedVersion)[IO.Directory]::CreateDirectory($applicationProfile)|Out-Null;[IO.File]::WriteAllText((Join-Path $applicationProfile 'state'),'active',[Text.UTF8Encoding]::new($false));$value=New-TestLaunchResult 'owned-webview-tauri-app-settings-ebwebview' $expectedVersion;if($mutation-ceq'runtime'){$value.returnedRuntimeVersion='150.0.1.1'}elseif($mutation-ceq'native-version'){$value.nativeDriverVersion='150.0.1.1'}elseif($mutation-ceq'profile'){$value.profileBindingMethod='missing'}elseif($mutation-ceq'ocr-page'){$value.ocrPage=2}elseif($mutation-ceq'ocr-empty'){$value.ocrTextUtf8Bytes=0}elseif($mutation-ceq'ocr-oversize'){$value.ocrTextUtf8Bytes=65537}else{$value.ocrSourceUiPreserved=$false};$value}.GetNewClosure();$rejected=$false;try{Invoke-TestLaunch -label ('bad-'+$mutation) -provider $bad}catch{$rejected=$true};if(-not$rejected){throw ('Unsafe launch result accepted: '+$mutation)}}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('keeps the real WebDriver client loopback-only, bounded, exact-oracle, and truth-bound on cleanup', () => {
    const launch = readFileSync('scripts/installed-app-launch.ps1', 'utf8');
    expect(launch).toContain('CleanupProcessTimeoutMilliseconds = 30000');
    expect(launch).toContain('Assert-FixedWebDriverPortsFree');
    expect(launch).toContain("--native-driver=$EdgeDriverPath");
    expect(launch).toContain("webviewOptions = [ordered]@{ userDataFolder = $ProfileRoot }");
    expect(launch).toContain("-Port $script:LaunchPins.NativeDriverPort");
    expect(launch).toContain('Assert-NativeDriverStatus');
    expect(launch).toContain("$capabilities.browserVersion");
    expect(launch).toContain("$capabilities.PSObject.Properties['msedge.userDataDir']");
    expect(launch).toContain('Get-OwnedProfileBinding');
    expect(launch).toContain('$homeOracle = Wait-WebDriverOracle');
    expect(launch).toContain('title = [string]$homeOracle.title');
    expect(launch).toContain('homeButton = [bool]$homeOracle.home');
    expect(launch).not.toMatch(/\$home\b/i);
    expect(launch).toContain("x.textContent.trim()==='Scan & OCR'");
    expect(launch).toContain("o?.nextElementSibling");
    expect(launch).toContain("img[alt=\"Page 1\"]");
    expect(launch).toContain("i.src.startsWith('blob:')");
    expect(launch).toContain("x.textContent.trim()==='Recognize current page…'");
    expect(launch).toContain("dialog[aria-labelledby=\"page-ocr-title\"]");
    expect(launch).toContain("textarea[aria-label=\"Recognized text on page 1\"]");
    expect(launch).toContain("x.textContent.trim()==='Close'&&!x.disabled");
    expect(launch).toContain("utf8Bytes>65536");
    expect(launch).toContain('Get-LaunchOcrTextReceipt');
    expect(launch).toContain('Get-BoundedWebDriverErrorCode');
    expect(launch).toContain("[ValidateSet('menu-open','menu-ready','recognition-start','result-poll','dialog-close','dialog-closed','source-preservation')]");
    expect(launch).toContain("$names = @('pdf-workstation','tauri-driver','msedgedriver','msedgewebview2','tesseract')");
    expect(launch).toContain("$ocrResult = Invoke-OcrWebDriverScript -SessionId $sessionId -Script $ocrResultScript -Deadline $deadline -Stage 'result-poll'");
    expect(launch).not.toContain('OcrDeadline');
    const rejectedOcrStates = launch.indexOf("if ([string]$ocrResult.state -in @('error','empty','overflow'))");
    const ocrHash = launch.indexOf('$ocrTextReceipt = Get-LaunchOcrTextReceipt', rejectedOcrStates);
    const closeScriptStart = launch.indexOf("$closeOcrScript = @'", rejectedOcrStates);
    const ocrClose = launch.indexOf('$closeOcr = Invoke-OcrWebDriverScript', closeScriptStart);
    expect(rejectedOcrStates).toBeGreaterThan(-1);
    expect(ocrHash).toBeGreaterThan(rejectedOcrStates);
    expect(ocrClose).toBeGreaterThan(ocrHash);
    const postOcr = launch.indexOf('$postOcrScript', ocrClose);
    const closeFlow = launch.slice(closeScriptStart, postOcr);
    expect(closeFlow).toContain("if(d.length===1&&b.length===1)setTimeout(()=>b[0].click(),0);return d.length===1&&b.length===1;");
    expect(closeFlow).toContain('-Script $closeOcrScript');
    expect(closeFlow).toContain('-Script $dialogClosedScript');
    expect(closeFlow).toContain("-Stage 'dialog-closed'");
    expect(closeFlow).toContain("document.querySelectorAll('dialog[aria-labelledby=\"page-ocr-title\"]').length===0");
    expect(closeFlow).not.toContain('\\"');
    expect(closeFlow).not.toMatch(/\.close\(|\.remove\(|removeChild\(/);
    expect(launch).toContain('BeginOutputReadLine');
    expect(launch).toContain('BeginErrorReadLine');
    expect(launch).toContain("$null -eq $delete.value");
    expect(launch).toContain('Get-OwnedLaunchProcesses');
    expect(launch).toContain('relevantProcessesRemaining -ne 0');
    expect(launch.match(/Invoke-BoundedLoopbackJson -Method POST -Path '\/session'/g)).toHaveLength(1);
    expect(launch).not.toMatch(/Start-Process|Remove-Item|\.Delete\(/);
    const verifier = readFileSync('scripts/verify-signed-ocr-upgrade.ps1', 'utf8');
    expect(verifier).toContain('Get-SignedCheckInputs -TargetVersion $TargetVersion -SourceRevision $SourceRevision');
    expect(verifier.match(/'[a-f0-9]{40}'/g)).toBeNull();
    expect(launch).toContain("sampleName = 'welcome.pdf'; physicalPage = 1; dialogTitle = 'Recognize text on page 1'; status = 'recognized'; language = 'eng'");
    expect(readFileSync('scripts/installed-app-launch-workflow.test.ts', 'utf8')).not.toMatch(/git\s+show|spawnSync\('git'/);
  });

  it('passes each OCR close script as one exact literal PowerShell string', () => {
    const launch = readFileSync('scripts/installed-app-launch.ps1', 'utf8');
    const closeScript = launch.match(/\$closeOcrScript = @'\r?\n([^\r\n]+)\r?\n'@/)?.[1];
    const closedScript = launch.match(/\$dialogClosedScript = @'\r?\n([^\r\n]+)\r?\n'@/)?.[1];
    expect(closeScript).toBe("const d=[...document.querySelectorAll('dialog[aria-labelledby=\"page-ocr-title\"]')];const b=d.length===1?[...d[0].querySelectorAll('button')].filter(x=>x.textContent.trim()==='Close'&&!x.disabled):[];if(d.length===1&&b.length===1)setTimeout(()=>b[0].click(),0);return d.length===1&&b.length===1;");
    expect(closedScript).toBe("return document.querySelectorAll('dialog[aria-labelledby=\"page-ocr-title\"]').length===0;");
    expect(() => new Function(closeScript!)).not.toThrow();
    expect(() => new Function(closedScript!)).not.toThrow();
    expect(`${closeScript}\n${closedScript}`).not.toContain('\\"');
    expect(`${closeScript}\n${closedScript}`).not.toMatch(/\.close\(|\.remove\(|removeChild\(/);

    const source = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\\scripts\\installed-app-launch.ps1',[ref]$tokens,[ref]$errors)
      if($errors.Count){throw 'Installed launch script did not parse.'}
      $function=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq'Invoke-RealInstalledAppLaunch'},$true)
      $expected=[ordered]@{
        closeOcrScript=@'
const d=[...document.querySelectorAll('dialog[aria-labelledby="page-ocr-title"]')];const b=d.length===1?[...d[0].querySelectorAll('button')].filter(x=>x.textContent.trim()==='Close'&&!x.disabled):[];if(d.length===1&&b.length===1)setTimeout(()=>b[0].click(),0);return d.length===1&&b.length===1;
'@
        dialogClosedScript=@'
return document.querySelectorAll('dialog[aria-labelledby="page-ocr-title"]').length===0;
'@
      }
      foreach($name in $expected.Keys){
        $assignments=@($function.FindAll({param($node)$node-is[Management.Automation.Language.AssignmentStatementAst]-and$node.Left.Extent.Text-ceq('$'+$name)},$true))
        if($assignments.Count-ne1-or$assignments[0].Right-isnot[Management.Automation.Language.CommandExpressionAst]){throw ('OCR close script assignment changed: '+$name)}
        $literalTokens=@($tokens|Where-Object{$_.Extent.StartOffset-ge$assignments[0].Right.Extent.StartOffset-and$_.Extent.EndOffset-le$assignments[0].Right.Extent.EndOffset})
        if($literalTokens.Count-ne1-or$literalTokens[0].Kind-cne[Management.Automation.Language.TokenKind]::HereStringLiteral){throw ('OCR close script was not one literal token: '+$name)}
        Invoke-Expression $assignments[0].Extent.Text
        $captured=(Get-Variable -Name $name -ValueOnly)
        if($captured-isnot[string]-or$captured-cne$expected[$name]-or$captured.Contains('\"')){throw ('OCR close script runtime value changed: '+$name)}
      }
    `;
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('reports only fixed OCR stages and bounded allowlisted WebDriver error codes', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Get-BoundedWebDriverErrorCode', 'Invoke-OcrWebDriverScript'], String.raw`
      Add-Type -TypeDefinition @'
using System;
using System.IO;
using System.Net;
using System.Text;
public sealed class FixedWebResponse : WebResponse {
  private readonly byte[] body;
  private readonly long declaredLength;
  public bool Closed { get; private set; }
  public bool StreamClosed { get; private set; }
  public FixedWebResponse(string value, long length) { body = Encoding.UTF8.GetBytes(value); declaredLength = length; }
  public override long ContentLength { get { return declaredLength < -1 ? body.Length : declaredLength; } set { } }
  public override Stream GetResponseStream() { return new FixedMemoryStream(body, () => StreamClosed = true); }
  public override void Close() { Closed = true; }
}
public sealed class FixedMemoryStream : MemoryStream {
  private readonly Action closed;
  public FixedMemoryStream(byte[] value, Action onClosed) : base(value, false) { closed = onClosed; }
  protected override void Dispose(bool disposing) { if (disposing) closed(); base.Dispose(disposing); }
}
'@
      $script:LaunchPins=[ordered]@{WebDriverErrorBytesMaximum=4096}
      function New-TestWebException([string]$body,[long]$length=-2){$response=[FixedWebResponse]::new($body,$length);[pscustomobject]@{response=$response;exception=[Net.WebException]::new('private transport message',$null,[Net.WebExceptionStatus]::ProtocolError,$response)}}
      foreach($code in @('javascript error','unknown error','script timeout','stale element reference','no such window','invalid session id','unexpected alert open')){$case=New-TestWebException ('{"value":{"error":"'+$code+'","message":"private text","stacktrace":"private stack"}}');if((Get-BoundedWebDriverErrorCode -Exception $case.exception)-cne$code-or-not$case.response.Closed-or-not$case.response.StreamClosed){throw ('Allowlisted WebDriver error was not reduced or disposed: '+$code)}}
      $other=New-TestWebException '{"value":{"error":"private code","message":"private text"}}';if((Get-BoundedWebDriverErrorCode -Exception $other.exception)-cne'other'-or-not$other.response.Closed){throw 'Unknown WebDriver error was not reduced or disposed.'}
      foreach($case in @((New-TestWebException 'not-json'),(New-TestWebException ('{"value":{"error":"javascript error","message":"'+('x'*5000)+'"}}') -1))){if((Get-BoundedWebDriverErrorCode -Exception $case.exception)-cne'unavailable'-or-not$case.response.Closed-or-not$case.response.StreamClosed){throw 'Unreadable or live-oversized WebDriver error was not bounded or disposed.'}}
      $declaredOversize=New-TestWebException ('x'*5000) 5000;if((Get-BoundedWebDriverErrorCode -Exception $declaredOversize.exception)-cne'unavailable'-or-not$declaredOversize.response.Closed-or$declaredOversize.response.StreamClosed){throw 'Declared-oversized WebDriver error was read or not disposed.'}
      if((Get-BoundedWebDriverErrorCode -Exception ([Exception]::new('private local failure')))-cne'unavailable'){throw 'Response-free failure did not remain unavailable.'}
      foreach($stage in @('menu-open','menu-ready','recognition-start','result-poll','dialog-close','dialog-closed','source-preservation')){
        $script:nextException=(New-TestWebException '{"value":{"error":"javascript error","message":"private text","stacktrace":"private stack","data":"private path session-id recognized text"}}').exception
        function Invoke-WebDriverScript { throw $script:nextException }
        $message='';try{Invoke-OcrWebDriverScript -SessionId 'private-session' -Script 'private-script' -Deadline ([datetime]::UtcNow.AddSeconds(1)) -Stage $stage}catch{$message=$_.Exception.Message}
        if($message-cne("Installed current-page OCR WebDriver stage failed: $stage; w3cError=javascript error.")-or$message-match'private|session|path|stack|recognized'){throw 'OCR WebDriver diagnostic changed or leaked response data.'}
      }
      $rejected=$false;try{Invoke-OcrWebDriverScript -SessionId 'id' -Script 'script' -Deadline ([datetime]::UtcNow.AddSeconds(1)) -Stage 'private-stage'}catch{if($_.Exception.Message-match'ValidateSet'){$rejected=$true}};if(-not$rejected){throw 'Unfixed OCR WebDriver stage was accepted.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('hashes only bounded nonblank OCR text and never returns the text', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Get-LaunchOcrTextReceipt'], String.raw`
      $script:LaunchPins=[ordered]@{OcrUiTextBytesMaximum=65536}
      $text='fixed private OCR output';$bytes=[Text.Encoding]::UTF8.GetByteCount($text);$receipt=Get-LaunchOcrTextReceipt -Text $text -ReportedUtf8Bytes $bytes
      if($receipt.utf8Bytes-ne$bytes-or[string]$receipt.sha256-cnotmatch'^[A-F0-9]{64}$'-or$receipt.PSObject.Properties.Name-ccontains'text'-or($receipt|ConvertTo-Json -Compress)-match'private OCR'){throw 'OCR receipt was not bounded and sanitized.'}
      foreach($case in @([pscustomobject]@{text=' ';bytes=1},[pscustomobject]@{text='abc';bytes=2},[pscustomobject]@{text=('x'*65537);bytes=65537})){$rejected=$false;try{Get-LaunchOcrTextReceipt -Text $case.text -ReportedUtf8Bytes $case.bytes}catch{$rejected=$true};if(-not$rejected){throw 'Empty, mismatched, or oversized OCR text was accepted.'}}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('does not assign to PowerShell automatic or read-only variables in the real launch path', () => {
    const source = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\\scripts\\installed-app-launch.ps1',[ref]$tokens,[ref]$errors)
      if($errors.Count){throw 'Launch helper did not parse.'}
      $function=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq'Invoke-RealInstalledAppLaunch'},$true)
      if($null-eq$function){throw 'Real launch function was not found.'}
      $forbidden=@('args','error','executioncontext','foreach','home','host','input','lastexitcode','matches','myinvocation','nestedpromptlevel','ofs','pid','pscommandpath','psscriptroot','psversiontable','pwd','shellid','stacktrace','this')
      $assigned=@($function.FindAll({param($node)$node-is[Management.Automation.Language.AssignmentStatementAst]-and$node.Left-is[Management.Automation.Language.VariableExpressionAst]},$true)|ForEach-Object{$_.Left.VariablePath.UserPath.ToLowerInvariant()})
      $collisions=@($assigned|Where-Object{$forbidden-contains$_})
      if($collisions.Count){throw ('Real launch assigns automatic/read-only variables: '+($collisions-join','))}
    `;
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('reports only fixed cleanup outcomes and booleans while keeping every failure rejected', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Invoke-SessionDeleteOutcome', 'Assert-LaunchCleanupState'], String.raw`
      $verified=Invoke-SessionDeleteOutcome -SessionId 'session-id' -Deadline ([datetime]::UtcNow.AddSeconds(1)) -RequestProvider {param($id,$deadline)[pscustomobject]@{value=$null}}
      $requestFailed=Invoke-SessionDeleteOutcome -SessionId 'session-id' -Deadline ([datetime]::UtcNow.AddSeconds(1)) -RequestProvider {throw 'private failure detail'}
      $invalidResponse=Invoke-SessionDeleteOutcome -SessionId 'session-id' -Deadline ([datetime]::UtcNow.AddSeconds(1)) -RequestProvider {param($id,$deadline)[pscustomobject]@{value='unexpected private body'}}
      if($verified-cne'verified'-or$requestFailed-cne'requestfailed'-or$invalidResponse-cne'invalidresponse'){throw 'Session delete outcome classification changed.'}
      $result=[pscustomobject]@{complete=$true};Assert-LaunchCleanupState -Result $result -SessionDeleteOutcome verified -DriverExited $true -RelevantProcessesClear $true -DriverStopOutcome tree-kill-exited -ResidualCategory none -CapturedApplication absent -CapturedTauriDriver absent -CapturedEdgeDriver absent -CapturedWebView absent -CapturedOcrEngine absent -CapturedOther absent -ResidualOwnership none -ResidualApplication $false -ResidualTauriDriver $false -ResidualEdgeDriver $false -ResidualWebView $false -ResidualOcrEngine $false -ResidualOther $false
      $cases=@(
        [pscustomobject]@{result=$result;outcome='requestfailed';driver=$true;clear=$true;stop='tree-kill-exited';residual='none';ownership='none';presence=@($false,$false,$false,$false,$false,$false)},
        [pscustomobject]@{result=$result;outcome='invalidresponse';driver=$true;clear=$true;stop='fallback-exited';residual='none';ownership='none';presence=@($false,$false,$false,$false,$false,$false)},
        [pscustomobject]@{result=$result;outcome='verified';driver=$false;clear=$true;stop='wait-failed';residual='none';ownership='none';presence=@($false,$false,$false,$false,$false,$false)},
        [pscustomobject]@{result=$result;outcome='verified';driver=$true;clear=$false;stop='tree-kill-exited';residual='ocr-engine';ownership='uncaptured';presence=@($false,$false,$false,$false,$true,$false)},
        [pscustomobject]@{result=$null;outcome='verified';driver=$true;clear=$true;stop='not-invoked';residual='multiple';ownership='mixed';presence=@($true,$false,$true,$false,$true,$true)}
      )
      foreach($case in $cases){$rejected=$false;try{Assert-LaunchCleanupState -Result $case.result -SessionDeleteOutcome $case.outcome -DriverExited $case.driver -RelevantProcessesClear $case.clear -DriverStopOutcome $case.stop -ResidualCategory $case.residual -CapturedApplication absent -CapturedTauriDriver all-exited -CapturedEdgeDriver all-exited -CapturedWebView incomplete -CapturedOcrEngine all-exited -CapturedOther absent -ResidualOwnership $case.ownership -ResidualApplication $case.presence[0] -ResidualTauriDriver $case.presence[1] -ResidualEdgeDriver $case.presence[2] -ResidualWebView $case.presence[3] -ResidualOcrEngine $case.presence[4] -ResidualOther $case.presence[5]}catch{$rejected=$true;$prefix="Installed application launch cleanup state is unverified: sessionDeleteOutcome=$($case.outcome);driverExited=$(([string]$case.driver).ToLowerInvariant());relevantProcessesClear=$(([string]$case.clear).ToLowerInvariant());";if(-not$_.Exception.Message.StartsWith($prefix,[StringComparison]::Ordinal)-or$_.Exception.Message-cnotmatch'captured=application:absent,tauri-driver:all-exited,edge-driver:all-exited,webview:incomplete,ocr-engine:all-exited,other:absent;residualOwnership=(none|uncaptured|mixed);residualPresence=application:(true|false),tauri-driver:(true|false),edge-driver:(true|false),webview:(true|false),ocr-engine:(true|false),other:(true|false)\.$'-or$_.Exception.Message-match'private|session-id|unexpected'){throw 'Cleanup diagnostic leaked or changed.'}};if(-not$rejected){throw 'Unverified cleanup state was accepted.'}}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('waits boundedly for an inert owned process tree and never kills mismatched identities', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Wait-BoundedOwnedProcessExit', 'Get-LaunchProcessCategory', 'Get-CapturedCategoryOutcome', 'Stop-OwnedLaunchProcesses'], String.raw`
      $root=Join-Path $env:SMACROBAT_TEST_ROOT ('owned-cleanup-'+[Guid]::NewGuid().ToString('N'));[IO.Directory]::CreateDirectory($root)|Out-Null
      $pidPath=Join-Path $root 'child.pid';$rootScript=Join-Path $root 'root.ps1';$pwsh=(Get-Process -Id $PID).Path
      $rootSource=@'
param([string]$ChildPidPath,[string]$PwshPath)
$info=[Diagnostics.ProcessStartInfo]::new();$info.FileName=$PwshPath;$info.UseShellExecute=$false;$info.CreateNoWindow=$true;$info.ArgumentList.Add('-NoProfile');$info.ArgumentList.Add('-NonInteractive');$info.ArgumentList.Add('-Command');$info.ArgumentList.Add('Start-Sleep -Seconds 30')
$child=[Diagnostics.Process]::Start($info);[IO.File]::WriteAllText($ChildPidPath,[string]$child.Id,[Text.UTF8Encoding]::new($false));Start-Sleep -Seconds 30
'@
      [IO.File]::WriteAllText($rootScript,$rootSource,[Text.UTF8Encoding]::new($false))
      $rootInfo=[Diagnostics.ProcessStartInfo]::new();$rootInfo.FileName=$pwsh;$rootInfo.UseShellExecute=$false;$rootInfo.CreateNoWindow=$true;foreach($argument in @('-NoProfile','-NonInteractive','-File',$rootScript,$pidPath,$pwsh)){$rootInfo.ArgumentList.Add($argument)}
      $rootProcess=[Diagnostics.Process]::Start($rootInfo);$childProcess=$null
      try{
        $ready=[datetime]::UtcNow.AddSeconds(5);while(-not(Test-Path -LiteralPath $pidPath) -and [datetime]::UtcNow -lt $ready){Start-Sleep -Milliseconds 50};if(-not(Test-Path -LiteralPath $pidPath)){throw 'Inert child PID receipt was not created.'}
        $childProcess=Get-Process -Id ([int](Get-Content -LiteralPath $pidPath -Raw));$captured=@([pscustomobject]@{ProcessId=$childProcess.Id;Path=[string]$childProcess.Path;StartTicks=$childProcess.StartTime.ToUniversalTime().Ticks})
        $rootOutcome=Stop-OwnedLaunchProcesses -TauriDriver $rootProcess -Captured $captured -Deadline ([datetime]::UtcNow.AddSeconds(8))
        if($rootOutcome.rootOutcome-cne'tree-kill-exited'-or$rootOutcome.other-cne'all-exited'){throw 'Owned root stop outcome was not exact.'}
        if(-not$rootProcess.HasExited){throw 'Owned root did not exit within its bounded cleanup deadline.'};$childAlive=$true;try{$null=Get-Process -Id $childProcess.Id -ErrorAction Stop}catch{$childAlive=$false};if($childAlive){throw 'Inert owned child remained alive.'}
      }finally{try{if(-not$rootProcess.HasExited){$rootProcess.Kill($true);$rootProcess.WaitForExit(5000)|Out-Null}}catch{};try{if($childProcess -and -not $childProcess.HasExited){$childProcess.Kill();$childProcess.WaitForExit(5000)|Out-Null}}catch{}}
      $script:fakeKills=0;$started=[datetime]::UtcNow;$fake=[pscustomobject]@{HasExited=$false;Path='C:\fixture\owned.exe';StartTime=$started};$fake|Add-Member ScriptMethod Kill {param([bool]$tree)$script:fakeKills++};$fake|Add-Member ScriptMethod WaitForExit {param([int]$milliseconds)$false}
      $provider={param($id)$fake}.GetNewClosure();$deadline=[datetime]::UtcNow.AddSeconds(1)
      Stop-OwnedLaunchProcesses -TauriDriver $null -Captured @([pscustomobject]@{ProcessId=77;Path='C:\fixture\other.exe';StartTicks=$started.ToUniversalTime().Ticks}) -Deadline $deadline -ProcessProvider $provider
      Stop-OwnedLaunchProcesses -TauriDriver $null -Captured @([pscustomobject]@{ProcessId=77;Path='C:\fixture\owned.exe';StartTicks=($started.ToUniversalTime().Ticks+1)}) -Deadline $deadline -ProcessProvider $provider
      if($script:fakeKills-ne0){throw 'A path/start identity mismatch was killed.'}
      if((Wait-BoundedOwnedProcessExit -Process $fake -Deadline ([datetime]::UtcNow.AddMilliseconds(-1)))-cne'deadline'-or$script:fakeKills-ne0){throw 'Expired cleanup deadline killed or accepted a live process.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('snapshots real driver exit before disposing its bounded capture', () => {
    const launchSource = readFileSync('scripts/installed-app-launch.ps1', 'utf8');
    const snapshotOffset = launchSource.indexOf('$driverExited = [bool]$driver.HasExited');
    const disposeOffset = launchSource.indexOf('$driverCapture.Dispose()', snapshotOffset);
    expect(snapshotOffset).toBeGreaterThan(0);
    expect(disposeOffset).toBeGreaterThan(snapshotOffset);
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Start-BoundedDiscardProcess', 'Wait-BoundedOwnedProcessExit', 'Get-LaunchProcessCategory', 'Get-CapturedCategoryOutcome', 'Stop-OwnedLaunchProcesses'], String.raw`
      $pwsh=(Get-Process -Id $PID).Path;$capture=Start-BoundedDiscardProcess -Path $pwsh -Arguments @('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30');$process=$capture.Process
      try{$capture.Start();$outcome=Stop-OwnedLaunchProcesses -TauriDriver $process -Captured @() -Deadline ([datetime]::UtcNow.AddSeconds(5));$driverExited=[bool]$process.HasExited;if(-not$driverExited-or$outcome.rootOutcome-cne'tree-kill-exited'){throw 'Exact bounded capture did not stop before disposal.'};$capture.Dispose();$capture=$null;if($null-ne$process.HasExited){throw 'Disposed Process unexpectedly retained a reliable HasExited value.'};if(-not$driverExited){throw 'The pre-disposal exit snapshot was lost.'}}finally{try{if($capture){$capture.Dispose()}}catch{}}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('requires two fresh empty cleanup snapshots and fails closed on late or persistent residuals', () => {
    const launchSource = readFileSync('scripts/installed-app-launch.ps1', 'utf8');
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Wait-LaunchProcessQuiescence', 'Get-LaunchProcessCategory', 'Get-LaunchResidualCategory', 'Get-LaunchResidualFacts', 'Assert-LaunchCleanupState'], String.raw`
      $script:LaunchPins=[ordered]@{CleanupProcessPollMilliseconds=1}
      $started=[datetime]::UtcNow;$path='C:\\fixture\\msedgedriver.exe';$captured=[pscustomobject]@{ProcessId=71;Path=$path;StartTicks=$started.ToUniversalTime().Ticks}
      $residual=[pscustomobject]@{Id=71;ProcessName='msedgedriver';Path=$path;StartTime=$started}
      $transient=[Collections.Generic.Queue[object]]::new();$transient.Enqueue($residual);$transient.Enqueue($null);$transient.Enqueue($null);$transientCalls=0
      $transientResult=Wait-LaunchProcessQuiescence -Deadline ([datetime]::UtcNow.AddSeconds(2)) -SnapshotProvider {$script:transientCalls++;$next=$transient.Dequeue();if($null-ne$next){$next}}
      if(-not$transientResult.stable-or$transientResult.processes.Count-ne0-or$transientCalls-ne3){throw 'Transient captured residual did not require two later fresh empty probes.'}
      $capturedOtherRejected=$false;try{Assert-LaunchCleanupState -Result ([pscustomobject]@{complete=$true}) -SessionDeleteOutcome verified -DriverExited $true -RelevantProcessesClear $transientResult.stable -DriverStopOutcome tree-kill-exited -ResidualCategory none -CapturedApplication absent -CapturedTauriDriver absent -CapturedEdgeDriver absent -CapturedWebView absent -CapturedOcrEngine absent -CapturedOther incomplete -ResidualOwnership none -ResidualApplication $false -ResidualTauriDriver $false -ResidualEdgeDriver $false -ResidualWebView $false -ResidualOcrEngine $false -ResidualOther $false}catch{$capturedOtherRejected=$_.Exception.Message-cmatch'captured=application:absent,tauri-driver:absent,edge-driver:absent,webview:absent,ocr-engine:absent,other:incomplete;'}
      if(-not$capturedOtherRejected){throw 'Two empty global snapshots accepted an incomplete captured console-host or other identity.'}
      $late=[Collections.Generic.Queue[object]]::new();$late.Enqueue($null);$late.Enqueue($residual);$late.Enqueue($null);$late.Enqueue($null);$lateCalls=0
      $lateResult=Wait-LaunchProcessQuiescence -Deadline ([datetime]::UtcNow.AddSeconds(2)) -SnapshotProvider {$script:lateCalls++;$next=$late.Dequeue();if($null-ne$next){$next}}
      if(-not$lateResult.stable-or$lateResult.processes.Count-ne0-or$lateCalls-ne4){throw 'A late residual did not reset cleanup quiescence.'}
      $persistentCalls=0;$persistentResult=Wait-LaunchProcessQuiescence -Deadline ([datetime]::UtcNow.AddMilliseconds(500)) -SnapshotProvider {$script:persistentCalls++;$residual}
      if($persistentResult.stable-or$persistentResult.processes.Count-ne1-or$persistentCalls-lt1){throw 'Persistent cleanup residual was accepted or its final facts were lost.'}
      $capturedFacts=Get-LaunchResidualFacts -Processes $persistentResult.processes -Captured @($captured)
      $reused=[pscustomobject]@{Id=71;ProcessName='msedgedriver';Path=$path;StartTime=$started.AddTicks(1)}
      $reusedResult=Wait-LaunchProcessQuiescence -Deadline ([datetime]::UtcNow.AddMilliseconds(500)) -SnapshotProvider {$reused}
      $reusedFacts=Get-LaunchResidualFacts -Processes $reusedResult.processes -Captured @($captured)
      if($capturedFacts.ownership-cne'captured'-or$reusedFacts.ownership-cne'uncaptured'-or-not$capturedFacts.edgeDriver-or-not$reusedFacts.edgeDriver){throw 'Final captured or PID-reuse residual facts were not identity-bound.'}
      foreach($case in @([pscustomobject]@{result=$persistentResult;facts=$capturedFacts},[pscustomobject]@{result=$reusedResult;facts=$reusedFacts})){
        $message='';try{Assert-LaunchCleanupState -Result ([pscustomobject]@{complete=$true}) -SessionDeleteOutcome verified -DriverExited $true -RelevantProcessesClear $case.result.stable -DriverStopOutcome tree-kill-exited -ResidualCategory (Get-LaunchResidualCategory -Processes $case.result.processes) -CapturedApplication absent -CapturedTauriDriver absent -CapturedEdgeDriver all-exited -CapturedWebView absent -CapturedOcrEngine absent -CapturedOther absent -ResidualOwnership $case.facts.ownership -ResidualApplication $case.facts.application -ResidualTauriDriver $case.facts.tauriDriver -ResidualEdgeDriver $case.facts.edgeDriver -ResidualWebView $case.facts.webview -ResidualOcrEngine $case.facts.ocrEngine -ResidualOther $case.facts.other}catch{$message=$_.Exception.Message}
        if($message-cnotmatch('residualOwnership='+$case.facts.ownership)-or$message-match'fixture|msedgedriver\.exe|71'){throw 'Persistent cleanup rejection lost sanitized final identity facts.'}
      }
      $expiredCalls=0;$expiredRejected=$false;try{Wait-LaunchProcessQuiescence -Deadline ([datetime]::UtcNow.AddMilliseconds(-1)) -SnapshotProvider {$script:expiredCalls++}|Out-Null}catch{$expiredRejected=$_.Exception.Message-ceq'Installed application cleanup quiescence deadline expired before its first process probe.'}
      if(-not$expiredRejected-or$expiredCalls-ne0){throw 'Expired cleanup quiescence probed or did not fail closed.'}
      $providerRejected=$false;try{Wait-LaunchProcessQuiescence -Deadline ([datetime]::UtcNow.AddSeconds(1)) -SnapshotProvider {throw 'snapshot failure'}|Out-Null}catch{$providerRejected=$_.Exception.Message-ceq'snapshot failure'}
      if(-not$providerRejected){throw 'Cleanup snapshot provider failure was accepted.'}
    `);
    expect(launchSource.match(/function Wait-LaunchProcessQuiescence[\s\S]*?\n}/)?.[0]).not.toMatch(/\.Kill\(|Stop-Process/);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('uses one-microsecond process timestamp precision without capturing stale parent-PID collisions', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Get-OwnedLaunchProcesses'], String.raw`
      $script:LaunchPins=[ordered]@{ProcessTimestampToleranceTicks=10}
      $base=[datetime]::SpecifyKind([datetime]'2026-01-01T00:00:00',[DateTimeKind]::Utc)
      $records=@(
        [pscustomobject]@{ProcessId=100;ParentProcessId=1;CreationDate=$base;CommandLine='root'},
        [pscustomobject]@{ProcessId=101;ParentProcessId=100;CreationDate=$base.AddTicks(-9);CommandLine='rounded-child'},
        [pscustomobject]@{ProcessId=102;ParentProcessId=101;CreationDate=$base.AddTicks(20);CommandLine='grandchild'},
        [pscustomobject]@{ProcessId=200;ParentProcessId=100;CreationDate=$base.AddTicks(-11);CommandLine='stale'},
        [pscustomobject]@{ProcessId=201;ParentProcessId=200;CreationDate=$base.AddTicks(30);CommandLine='stale-child'}
      )
      $live=@{
        100=[pscustomobject]@{Id=100;StartTime=$base.AddTicks(9);Path='C:\fixture\root.exe'}
        101=[pscustomobject]@{Id=101;StartTime=$base;Path='C:\fixture\child.exe'}
        102=[pscustomobject]@{Id=102;StartTime=$base.AddTicks(29);Path='C:\fixture\grand.exe'}
        200=[pscustomobject]@{Id=200;StartTime=$base.AddTicks(-2);Path='C:\fixture\stale.exe'}
        201=[pscustomobject]@{Id=201;StartTime=$base.AddTicks(39);Path='C:\fixture\stale-child.exe'}
      }
      $owned=@(Get-OwnedLaunchProcesses -RootProcessId 100 -StartedAfter $base.AddTicks(10) -CimProvider {$records}.GetNewClosure() -ProcessProvider {param($id)$live[[int]$id]}.GetNewClosure())
      if(($owned.ProcessId-join',')-cne'101,102'-or($owned.ParentProcessId-join',')-cne'100,101'){throw 'Temporal parent-chain filtering changed.'}
      foreach($bad in @(
        [pscustomobject]@{records=$records;live=@{100=[pscustomobject]@{Id=100;StartTime=$base.AddTicks(9);Path='root'}};started=$base.AddTicks(20);message='predates its launch boundary'},
        [pscustomobject]@{records=$records;live=@{100=[pscustomobject]@{Id=100;StartTime=$base.AddTicks(11);Path='root'}};started=$base;message='identity changed during capture'}
      )){
        if($bad.message-ceq'identity changed during capture'){$bad.records=@($records|ForEach-Object{if($_.ProcessId-eq100){[pscustomobject]@{ProcessId=100;ParentProcessId=1;CreationDate=$base.AddTicks(-1);CommandLine='root'}}else{$_}})}
        $rejected=$false;try{Get-OwnedLaunchProcesses -RootProcessId 100 -StartedAfter $bad.started -CimProvider {$bad.records}.GetNewClosure() -ProcessProvider {param($id)$bad.live[[int]$id]}.GetNewClosure()|Out-Null}catch{if($_.Exception.Message-match[regex]::Escape($bad.message)){$rejected=$true}}
        if(-not$rejected){throw ('Timestamp boundary was not rejected: '+$bad.message)}
      }
      $reused=@{}+$live;$reused[102]=[pscustomobject]@{Id=102;StartTime=$base.AddTicks(31);Path='C:\fixture\reused.exe'}
      $rejected=$false;try{Get-OwnedLaunchProcesses -RootProcessId 100 -StartedAfter $base -CimProvider {$records}.GetNewClosure() -ProcessProvider {param($id)$reused[[int]$id]}.GetNewClosure()|Out-Null}catch{if($_.Exception.Message-ceq'A launch descendant identity changed during capture.'){$rejected=$true}}
      if(-not$rejected){throw 'A live/CIM PID-reuse mismatch was accepted.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('classifies only fixed residual process categories and fails closed for unknown or multiple entries', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Get-LaunchProcessCategory', 'Get-LaunchResidualCategory', 'Get-LaunchResidualFacts'], String.raw`
      $cases=@(
        [pscustomobject]@{items=@();expected='none'},
        [pscustomobject]@{items=@([pscustomobject]@{ProcessName='pdf-workstation'});expected='application'},
        [pscustomobject]@{items=@([pscustomobject]@{ProcessName='tauri-driver'});expected='tauri-driver'},
        [pscustomobject]@{items=@([pscustomobject]@{ProcessName='msedgedriver'});expected='edge-driver'},
        [pscustomobject]@{items=@([pscustomobject]@{ProcessName='msedgewebview2'});expected='webview'},
        [pscustomobject]@{items=@([pscustomobject]@{ProcessName='tesseract'});expected='ocr-engine'},
        [pscustomobject]@{items=@([pscustomobject]@{ProcessName='private-name'});expected='multiple'},
        [pscustomobject]@{items=@([pscustomobject]@{ProcessName='tauri-driver'},[pscustomobject]@{ProcessName='msedgewebview2'});expected='multiple'}
      );foreach($case in $cases){$actual=Get-LaunchResidualCategory -Processes $case.items;if($actual-cne$case.expected-or$actual-match'private'){throw 'Residual category escaped its fixed allowlist.'}}
      $started=[datetime]::UtcNow;$captured=[pscustomobject]@{ProcessId=71;Path='C:\fixture\msedgedriver.exe';StartTicks=$started.ToUniversalTime().Ticks};$exact=[pscustomobject]@{Id=71;ProcessName='msedgedriver';Path='C:\fixture\msedgedriver.exe';StartTime=$started};$engine=[pscustomobject]@{Id=73;ProcessName='tesseract';Path='C:\private\tesseract.exe';StartTime=$started};$unknown=[pscustomobject]@{Id=72;ProcessName='private-name';Path='C:\private\secret.exe';StartTime=$started};$facts=Get-LaunchResidualFacts -Processes @($exact,$engine,$unknown) -Captured @($captured);if($facts.ownership-cne'mixed'-or-not$facts.edgeDriver-or-not$facts.ocrEngine-or-not$facts.other-or$facts.application-or$facts.tauriDriver-or$facts.webview){throw 'Residual identity facts escaped the fixed contract.'};if(($facts|ConvertTo-Json -Compress)-match'private|secret|fixture|71|72|73'){throw 'Residual facts leaked identifying data.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('reports a newly orphaned descendant as uncaptured without weakening identity-bound cleanup', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Get-OwnedLaunchProcesses', 'Wait-BoundedOwnedProcessExit', 'Get-LaunchProcessCategory', 'Get-CapturedCategoryOutcome', 'Stop-OwnedLaunchProcesses', 'Get-LaunchResidualFacts', 'Assert-LaunchCleanupState'], String.raw`
      $script:LaunchPins=[ordered]@{ProcessTimestampToleranceTicks=10}
      $root=Join-Path $env:SMACROBAT_TEST_ROOT ('orphan-diagnostic-'+[Guid]::NewGuid().ToString('N'));[IO.Directory]::CreateDirectory($root)|Out-Null;$gate=Join-Path $root 'spawn.gate';$middlePid=Join-Path $root 'middle.pid';$grandPid=Join-Path $root 'grand.pid';$middleScript=Join-Path $root 'middle.ps1';$rootScript=Join-Path $root 'root.ps1';$pwsh=(Get-Process -Id $PID).Path
      $middleSource=@'
param([string]$Gate,[string]$GrandPid,[string]$Pwsh)
while(-not(Test-Path -LiteralPath $Gate)){Start-Sleep -Milliseconds 25}
$info=[Diagnostics.ProcessStartInfo]::new();$info.FileName=$Pwsh;$info.UseShellExecute=$false;$info.CreateNoWindow=$true;$info.ArgumentList.Add('-NoProfile');$info.ArgumentList.Add('-NonInteractive');$info.ArgumentList.Add('-Command');$info.ArgumentList.Add('Start-Sleep -Seconds 30');$grand=[Diagnostics.Process]::Start($info);[IO.File]::WriteAllText($GrandPid,[string]$grand.Id,[Text.UTF8Encoding]::new($false))
'@
      $rootSource=@'
param([string]$MiddleScript,[string]$Gate,[string]$MiddlePid,[string]$GrandPid,[string]$Pwsh)
$info=[Diagnostics.ProcessStartInfo]::new();$info.FileName=$Pwsh;$info.UseShellExecute=$false;$info.CreateNoWindow=$true;foreach($argument in @('-NoProfile','-NonInteractive','-File',$MiddleScript,$Gate,$GrandPid,$Pwsh)){$info.ArgumentList.Add($argument)};$middle=[Diagnostics.Process]::Start($info);[IO.File]::WriteAllText($MiddlePid,[string]$middle.Id,[Text.UTF8Encoding]::new($false));Start-Sleep -Seconds 30
'@
      [IO.File]::WriteAllText($middleScript,$middleSource,[Text.UTF8Encoding]::new($false));[IO.File]::WriteAllText($rootScript,$rootSource,[Text.UTF8Encoding]::new($false));$info=[Diagnostics.ProcessStartInfo]::new();$info.FileName=$pwsh;$info.UseShellExecute=$false;$info.CreateNoWindow=$true;foreach($argument in @('-NoProfile','-NonInteractive','-File',$rootScript,$middleScript,$gate,$middlePid,$grandPid,$pwsh)){$info.ArgumentList.Add($argument)};$driver=[Diagnostics.Process]::Start($info);$grand=$null
      try{$ready=[datetime]::UtcNow.AddSeconds(5);while(-not(Test-Path -LiteralPath $middlePid)-and[datetime]::UtcNow-lt$ready){Start-Sleep -Milliseconds 25};if(-not(Test-Path -LiteralPath $middlePid)){throw 'Intermediate receipt was not created.'};$captured=@(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $driver.StartTime.ToUniversalTime().AddSeconds(-1));[IO.File]::WriteAllText($gate,'go',[Text.UTF8Encoding]::new($false));while(-not(Test-Path -LiteralPath $grandPid)-and[datetime]::UtcNow-lt$ready){Start-Sleep -Milliseconds 25};if(-not(Test-Path -LiteralPath $grandPid)){throw 'Grandchild receipt was not created.'};$grand=Get-Process -Id ([int](Get-Content -LiteralPath $grandPid -Raw));Start-Sleep -Milliseconds 300;$captured+=@(Get-OwnedLaunchProcesses -RootProcessId $driver.Id -StartedAfter $driver.StartTime.ToUniversalTime().AddSeconds(-1));if(@($captured.ProcessId)-contains$grand.Id){throw 'The reproduction did not create an orphan outside the root-only traversal.'};$stop=Stop-OwnedLaunchProcesses -TauriDriver $driver -Captured $captured -Deadline ([datetime]::UtcNow.AddSeconds(5));$residual=@(Get-Process -Id $grand.Id -ErrorAction Stop);$facts=Get-LaunchResidualFacts -Processes $residual -Captured $captured;if($stop.rootOutcome-cne'tree-kill-exited'-or$stop.other-cne'all-exited'-or$facts.ownership-cne'uncaptured'-or-not$facts.other){throw 'Orphan diagnostics did not distinguish captured cleanup from the uncaptured residual.'};$rejected=$false;try{Assert-LaunchCleanupState -Result ([pscustomobject]@{complete=$true}) -SessionDeleteOutcome verified -DriverExited $true -RelevantProcessesClear $false -DriverStopOutcome $stop.rootOutcome -ResidualCategory multiple -CapturedApplication $stop.application -CapturedTauriDriver $stop.tauriDriver -CapturedEdgeDriver $stop.edgeDriver -CapturedWebView $stop.webview -CapturedOcrEngine $stop.ocrEngine -CapturedOther $stop.other -ResidualOwnership $facts.ownership -ResidualApplication $facts.application -ResidualTauriDriver $facts.tauriDriver -ResidualEdgeDriver $facts.edgeDriver -ResidualWebView $facts.webview -ResidualOcrEngine $facts.ocrEngine -ResidualOther $facts.other}catch{if($_.Exception.Message-match'residualOwnership=uncaptured'-and$_.Exception.Message-match'other:true'){$rejected=$true}};if(-not$rejected){throw 'The uncaptured orphan escaped the global zero-process gate.'}}
      finally{try{if(-not$driver.HasExited){$driver.Kill($true);$driver.WaitForExit(3000)|Out-Null}}catch{};try{if($grand-and-not$grand.HasExited){$grand.Kill();$grand.WaitForExit(3000)|Out-Null}}catch{}}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('does not retry a timed-out session POST and still stops the owned process tree', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Get-MinimalWindowsProcessEnvironment', 'Invoke-RealInstalledAppLaunch'], String.raw`
      $script:LaunchPins=[ordered]@{TotalTimeoutMilliseconds=60000;CleanupProcessTimeoutMilliseconds=30000;CleanupProcessPollMilliseconds=1;WebDriverPort=4444;NativeDriverPort=4445}
      $script:sessionPosts=0;$script:deleteRequests=0;$script:driverStopped=$false
      $driver=[pscustomobject]@{HasExited=$false;Id=1234}
      $capture=[pscustomobject]@{Process=$driver;Exceeded=$false};$capture|Add-Member ScriptMethod Start {};$capture|Add-Member ScriptMethod Dispose {}
      function Assert-FixedWebDriverPortsFree{}
      function Start-BoundedDiscardProcess{param([string]$Path,[object[]]$Arguments,[Collections.IDictionary]$Environment);if($Environment.Count-ne10){throw 'Minimal launch environment was not supplied.'};$capture}
      function Invoke-BoundedLoopbackJson{param([string]$Method,[string]$Path,$Body,[datetime]$Deadline,[int]$Port=4444);if($Method-ceq'GET'-and$Path-ceq'/status'){return [pscustomobject]@{value=[pscustomobject]@{ready=$true}}};if($Method-ceq'POST'-and$Path-ceq'/session'){$script:sessionPosts++;throw 'session-timeout'};if($Method-ceq'DELETE'){$script:deleteRequests++};throw 'unexpected-loopback-request'}
      function Wait-NativeDriverStatus{param([string]$ExpectedVersion,[datetime]$Deadline,$TauriDriver);$ExpectedVersion}
      function Get-OwnedLaunchProcesses{param([int]$RootProcessId,[datetime]$StartedAfter);@()}
      function Stop-OwnedLaunchProcesses{param($TauriDriver,[object[]]$Captured,[datetime]$Deadline);$script:driverStopped=$true;$TauriDriver.HasExited=$true;[pscustomobject]@{rootOutcome='tree-kill-exited';application='absent';tauriDriver='absent';edgeDriver='absent';webview='absent';ocrEngine='absent';other='absent'}}
      function Get-LaunchProcessSnapshot{@()}
      function Wait-LaunchProcessQuiescence{param([datetime]$Deadline);[pscustomobject]@{stable=$true;processes=@()}}
      function Get-LaunchResidualCategory{param([object[]]$Processes);'none'}
      function Get-LaunchResidualFacts{param([object[]]$Processes,[object[]]$Captured);[pscustomobject]@{ownership='none';application=$false;tauriDriver=$false;edgeDriver=$false;webview=$false;ocrEngine=$false;other=$false}}
      $rejected=$false;try{Invoke-RealInstalledAppLaunch -ApplicationPath 'app.exe' -TauriDriverPath 'tauri-driver.exe' -EdgeDriverPath 'msedgedriver.exe' -OcrEnginePath 'tesseract.exe' -ProfileRoot 'profile' -SettingsRoot 'settings' -ExpectedEdgeDriverVersion '151.0.1.2'}catch{if($_.Exception.Message-ceq'session-timeout'){$rejected=$true}else{throw}}
      if(-not$rejected-or$script:sessionPosts-ne1-or$script:deleteRequests-ne0-or-not$script:driverStopped-or-not$driver.HasExited){throw 'Session-timeout cleanup or no-retry contract changed.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('rejects occupied ports, over-deep JSON, and every native endpoint except fixed GET status', () => {
    const source = extractFunctions('scripts/installed-app-launch.ps1', ['Assert-BoundedJsonShape', 'Get-LaunchRemainingMilliseconds', 'Get-LoopbackRequestTimeoutMilliseconds', 'Invoke-BoundedLoopbackJson', 'Assert-FixedWebDriverPortsFree', 'Wait-FixedWebDriverPortsFree'], String.raw`
      $script:LaunchPins=[ordered]@{WebDriverPort=4444;NativeDriverPort=4445;JsonDepthMaximum=2;JsonNodesMaximum=8;RequestBytesMaximum=1MB;RequestTimeoutMilliseconds=10000;SessionCreationTimeoutMilliseconds=30000;CleanupProcessPollMilliseconds=100}
      $nodes=0;$rejected=$false;try{Assert-BoundedJsonShape -Value ([pscustomobject]@{a=[pscustomobject]@{b=[pscustomobject]@{c=1}}}) -Nodes ([ref]$nodes)}catch{$rejected=$true};if(-not$rejected){throw 'Deep JSON accepted.'}
      $farDeadline=[datetime]::UtcNow.AddSeconds(45)
      if((Get-LoopbackRequestTimeoutMilliseconds -Method POST -Path '/session' -Port 4444 -Deadline $farDeadline)-ne30000){throw 'Exact session creation did not receive its bounded cold-start allowance.'}
      foreach($ordinary in @([pscustomobject]@{method='GET';path='/status';port=4444},[pscustomobject]@{method='GET';path='/status';port=4445},[pscustomobject]@{method='POST';path='/session/id/execute/sync';port=4444},[pscustomobject]@{method='DELETE';path='/session/id';port=4444})){if((Get-LoopbackRequestTimeoutMilliseconds -Method $ordinary.method -Path $ordinary.path -Port $ordinary.port -Deadline $farDeadline)-ne10000){throw 'An ordinary loopback operation escaped the 10-second cap.'}}
      $nearDeadline=[datetime]::UtcNow.AddMilliseconds(750);$clamped=Get-LoopbackRequestTimeoutMilliseconds -Method POST -Path '/session' -Port 4444 -Deadline $nearDeadline;if($clamped-le0-or$clamped-gt750){throw 'Shared remaining deadline did not clamp session creation.'}
      foreach($request in @([pscustomobject]@{method='POST';path='/status';port=4445},[pscustomobject]@{method='GET';path='/session';port=4445},[pscustomobject]@{method='GET';path='/status';port=4446})){$rejected=$false;try{Invoke-BoundedLoopbackJson -Method $request.method -Path $request.path -Port $request.port -Deadline ([datetime]::UtcNow.AddSeconds(1))}catch{if($_.Exception.Message -ceq'WebDriver requests are restricted to the fixed loopback endpoint and command allowlist.'){$rejected=$true}};if(-not$rejected){throw 'Unsafe native endpoint escaped its allowlist.'}}
      $allowed=$false;try{Invoke-BoundedLoopbackJson -Method GET -Path '/status' -Port 4445 -Deadline ([datetime]::UtcNow.AddSeconds(1))}catch{if($_.Exception.Message -cne'WebDriver requests are restricted to the fixed loopback endpoint and command allowlist.'){$allowed=$true}};if(-not$allowed){throw 'Exact native status endpoint was not allowed.'}
      $listener=[Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback,4444);$listener.Start();try{$rejected=$false;try{Assert-FixedWebDriverPortsFree}catch{$rejected=$true};if(-not$rejected){throw 'Occupied fixed port accepted.'}}finally{$listener.Stop()}
      $script:waitNow=[datetime]::SpecifyKind([datetime]'2026-01-01T00:00:00',[DateTimeKind]::Utc);$script:waitProbes=0;$script:waitSleeps=0
      $clock={$script:waitNow};$sleep={param($milliseconds)$script:waitSleeps++;$script:waitNow=$script:waitNow.AddMilliseconds($milliseconds)}
      $occupiedThenFree={if(++$script:waitProbes-eq1){throw 'A fixed loopback WebDriver port is already in use.'}}
      if((Wait-FixedWebDriverPortsFree -Deadline $script:waitNow.AddMilliseconds(500) -ProbeProvider $occupiedThenFree -SleepProvider $sleep -UtcNowProvider $clock)-cne'free'-or$script:waitProbes-ne2-or$script:waitSleeps-ne1){throw 'Bounded fixed-port handoff did not retry once and succeed.'}
      $script:waitProbes=0;$rejected=$false;try{Wait-FixedWebDriverPortsFree -Deadline $script:waitNow -ProbeProvider {++$script:waitProbes} -SleepProvider $sleep -UtcNowProvider $clock|Out-Null}catch{if($_.Exception.Message-ceq'Fixed loopback WebDriver ports did not become free before the handoff deadline.'){$rejected=$true}};if(-not$rejected-or$script:waitProbes-ne0){throw 'Expired fixed-port handoff probed or was accepted.'}
      $script:waitProbes=0;$script:waitSleeps=0;$handoffDeadline=$script:waitNow.AddMilliseconds(100);$rejected=$false;try{Wait-FixedWebDriverPortsFree -Deadline $handoffDeadline -ProbeProvider {$script:waitProbes++;throw 'A fixed loopback WebDriver port is already in use.'} -SleepProvider {param($milliseconds)$script:waitSleeps++;$script:waitNow=$handoffDeadline} -UtcNowProvider $clock|Out-Null}catch{if($_.Exception.Message-ceq'Fixed loopback WebDriver ports did not become free before the handoff deadline.'){$rejected=$true}};if(-not$rejected-or$script:waitProbes-ne1-or$script:waitSleeps-ne1){throw 'Fixed-port handoff probed again after deadline expiry.'}
    `);
    const result = runPowerShell(source);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);
});
