import { describe, expect, it } from 'vitest';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';

const approvedActionUses = [
  'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1',
  'actions/download-artifact@37930b1c2abaa49bbe596cd826c3c89aef350131',
  'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02',
];

function assertApprovedImmutableActionUses(workflow: string) {
  const actionUses = [...workflow.matchAll(/^\s*-?\s*uses:\s*([^\s#]+)(?:\s+#.*)?$/gm)].map((match) => match[1]);
  for (const actionUse of actionUses) {
    if (!/@[0-9a-f]{40}$/.test(actionUse)) throw new Error(`Action does not use an immutable full SHA: ${actionUse}`);
    if (!approvedActionUses.includes(actionUse)) throw new Error(`Action is not approved: ${actionUse}`);
  }
  if (actionUses.length !== approvedActionUses.length + 1) throw new Error('Workflow action count changed.');
  return actionUses;
}

function runPowerShell(source: string) {
  const root = `target/ocr-upgrade-test-script-${randomUUID()}`;
  mkdirSync(root, { recursive: true });
  const path = `${root}/run.ps1`;
  writeFileSync(path, source, 'utf8');
  return spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-File', path], {
    encoding: 'utf8',
    timeout: 45_000,
  });
}

function runPowerShell7(source: string) {
  const root = `target/ocr-upgrade-pwsh7-${randomUUID()}`;
  mkdirSync(root, { recursive: true });
  const path = `${root}/run.ps1`;
  writeFileSync(path, source, 'utf8');
  return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path], {
    encoding: 'utf8',
    timeout: 45_000,
  });
}

function workflowRunBlock(workflow: string, stepName: string) {
  const lines = workflow.split(/\r?\n/);
  const step = lines.findIndex((line) => line.trim() === `- name: ${stepName}`);
  if (step < 0) throw new Error(`Missing workflow step: ${stepName}`);
  const run = lines.findIndex((line, index) => index > step && line.trim() === 'run: |');
  if (run < 0) throw new Error(`Missing run block: ${stepName}`);
  const body: string[] = [];
  for (let index = run + 1; index < lines.length; index += 1) {
    if (/^ {6}- (name:|uses:)/.test(lines[index])) break;
    if (lines[index] === '') break;
    if (!lines[index].startsWith('          ')) throw new Error(`Unexpected workflow indentation in ${stepName}`);
    body.push(lines[index].slice(10));
  }
  return `${body.join('\n')}\n`;
}

function functionHarness(names: string[], body: string, loadDependencies = true) {
  const dependencies = loadDependencies ? String.raw`
    . ./scripts/ocr/installer-package.ps1
    . ./scripts/ocr/windows-signing.ps1
    . ./scripts/installed-publisher-ui.ps1
  ` : '';
  return String.raw`
    $ErrorActionPreference = 'Stop'
    Set-StrictMode -Version Latest
    Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
    ${dependencies}
    $scriptPath = (Resolve-Path ./scripts/verify-signed-ocr-upgrade.ps1).Path
    $tokens = $null; $errors = $null
    $ast = [Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw 'Upgrade verifier did not parse.' }
    foreach ($name in @(${names.map((name) => `'${name}'`).join(',')})) {
      $function = $ast.Find({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name }, $true)
      if (-not $function) { throw ('Missing verifier function: ' + $name) }
      Invoke-Expression $function.Extent.Text
    }
    $script:Pins = [ordered]@{
      Repository = 'Joshua-Beel/PDF-Workstation'
      BaselineName = 'PDF.Workstation_0.2.0_x64-setup.exe'
      BaselineBytes = [uint64]6418229
      BaselineSha256 = '17788CB82BB42DEAC35EA197A385A9BAFC8CD331422446A2B834E115A90C4D2E'
      TargetVersion = '0.3.0'
      LeakIds = '12510717103|39969839829'
      SignedSourceRevision = '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d'
      SignedInstallerName = 'PDF Workstation_0.3.0_x64-setup.exe'
      SignedInstallerBytes = [uint64]11201296
      SignedInstallerSha256 = '3DE99F111652727BB59F9C17FF4E3CCA42ED48E9DE5E0614920280FFB0BA9FE5'
      ExpectedPublisher = 'Joshua Beel'
      SmokeWidth = 1200
      SmokeHeight = 240
      SmokeInputBytesMaximum = 16MB
      SmokeStdoutCharactersMaximum = 1MB
      SmokeStderrCharactersMaximum = 64KB
      SmokeTimeoutMilliseconds = 30000
      SmokeExpectedText = @('Receipt #A-17: Coffee & Tea, $12.50.','Mixed case: 3rd Avenue; ready.') -join [string][char]10
    }
    ${body}
  `;
}

describe('manual signed OCR installer upgrade workflow', () => {
  it('uses an exact manual hosted-runner contract with step-scoped download credentials', () => {
    const workflow = readFileSync('.github/workflows/ocr-installer-upgrade.yml', 'utf8');

    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).toContain('timeout-minutes: 45');
    expect(workflow).toContain('contents: read');
    expect(workflow).toContain('actions: read');
    const jobHeader = workflow.slice(workflow.indexOf('jobs:'), workflow.indexOf('    steps:'));
    expect(jobHeader).not.toContain('${{ runner.temp }}');
    expect(jobHeader).toMatch(/^\s{4}env:/m);
    expect(jobHeader).not.toMatch(/GITHUB_TOKEN|github\.token|secrets\./);
    expect(workflow).toContain('- name: Initialize fresh runner paths');
    expect(workflow).toContain('$env:RUNNER_TEMP');
    expect(workflow).toContain('[IO.StreamWriter]::new($env:GITHUB_ENV, $true, [Text.UTF8Encoding]::new($false))');
    expect(assertApprovedImmutableActionUses(workflow)).toEqual([approvedActionUses[0], approvedActionUses[0], approvedActionUses[1], approvedActionUses[2]]);
    const initStep = workflow.slice(workflow.indexOf('- name: Initialize fresh runner paths'), workflow.indexOf(`- uses: ${approvedActionUses[0]}`));
    expect(initStep).toContain('IsNullOrWhiteSpace($env:RUNNER_TEMP)');
    expect(initStep).toContain('IsNullOrWhiteSpace($env:GITHUB_ENV)');
    expect(initStep.match(/^[ ]{12}"(BASELINE_ROOT|SIGNED_ARTIFACT_ROOT|UPGRADE_WORK_ROOT|UPGRADE_OUTPUT_ROOT|WEBDRIVER_ROOT|WEBVIEW_PROFILE_ROOT)=/gm)).toHaveLength(6);
    expect(workflow).toContain('./scripts/setup-installed-app-webdriver.ps1 -OutputRoot $env:WEBDRIVER_ROOT');
    expect(workflow).toContain('-WebDriverRoot $env:WEBDRIVER_ROOT');
    expect(workflow).toContain('-WebViewProfileRoot $env:WEBVIEW_PROFILE_ROOT');
    expect(workflow).toContain('-PublisherUiMode HostedNonInteractive');
    const verifier = readFileSync('scripts/verify-signed-ocr-upgrade.ps1', 'utf8');
    expect(verifier).toContain('-ApplicationSettingsRoot $settingsRoot');
    expect(verifier).toContain('-SettingsSentinelPath $settingsSentinel');
    expect(verifier).toContain('-SettingsSentinelBytes $settingsSentinelBytes');
    expect(verifier).toContain('-SettingsSentinelSha256 $settingsSentinelSha256');
    expect(workflow).toContain("$env:GITHUB_REPOSITORY -cne 'Joshua-Beel/PDF-Workstation'");
    expect(workflow).toContain("$env:GITHUB_REF -cne 'refs/heads/master'");
    expect(workflow).toContain('ref: ${{ github.sha }}');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('git diff --quiet --');
    expect(workflow).toContain('git diff --cached --quiet --');
    expect(workflow).toContain("Test-Path -LiteralPath $env:SIGNED_ARTIFACT_ROOT");
    expect(workflow).toContain('artifact-ids: ${{ inputs.artifact_id }}');
    expect(workflow).toContain('run-id: ${{ inputs.artifact_run_id }}');
    expect(workflow).toContain('merge-multiple: true');
    expect(workflow).toContain('./scripts/assert-signed-artifact-metadata.ps1');
    expect(workflow).toContain("digest -cne 'sha256:17788cb82bb42deac35ea197a385a9bafc8cd331422446a2b834e115a90c4d2e'");
    expect(workflow).toContain("[uint64]$asset.size -ne 6418229");
    expect(workflow).not.toMatch(/secrets\.|AZURE_|TAURI_SIGNING|gh release|create.*release/i);

    const verifyStep = workflow.slice(workflow.indexOf('- name: Verify isolated silent install'), workflow.indexOf('- name: Upload only'));
    expect(verifyStep).not.toMatch(/github\.token|GITHUB_TOKEN|GH_TOKEN/);
    const uploadStep = workflow.slice(workflow.indexOf('- name: Upload only'));
    expect(uploadStep).toContain('/upgrade-verification.json');
    expect(uploadStep).not.toMatch(/\.exe|SIGNED_ARTIFACT_ROOT|BASELINE_ROOT|UPGRADE_WORK_ROOT/);
  });

  it('rejects mutable action refs and unapproved external actions', () => {
    const workflow = readFileSync('.github/workflows/ocr-installer-upgrade.yml', 'utf8');
    const mutable = workflow.replace(approvedActionUses[0], 'actions/checkout@v4');
    const unapproved = workflow.replace(approvedActionUses[0], `third-party/checkout@${'a'.repeat(40)}`);

    expect(() => assertApprovedImmutableActionUses(mutable)).toThrow('Action does not use an immutable full SHA');
    expect(() => assertApprovedImmutableActionUses(unapproved)).toThrow('Action is not approved');
  });

  it('executes the exact path initialization block under PowerShell 7', () => {
    const workflow = readFileSync('.github/workflows/ocr-installer-upgrade.yml', 'utf8');
    const block = workflowRunBlock(workflow, 'Initialize fresh runner paths');
    const root = resolve(`target/ocr-upgrade-init-${randomUUID()}`);
    const runnerTemp = resolve(root, 'runner-temp');
    const githubEnv = resolve(root, 'github-env');
    mkdirSync(runnerTemp, { recursive: true });
    const result = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-EncodedCommand', Buffer.from(block, 'utf16le').toString('base64')], {
      encoding: 'utf8',
      timeout: 30_000,
      env: { ...process.env, RUNNER_TEMP: runnerTemp, GITHUB_ENV: githubEnv },
    });
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const bytes = readFileSync(githubEnv);
    expect([...bytes.slice(0, 3)]).not.toEqual([0xef, 0xbb, 0xbf]);
    expect(bytes.toString('utf8').trimEnd().split(/\r?\n/)).toEqual([
      `BASELINE_ROOT=${runnerTemp}\\pdf-workstation-baseline`,
      `SIGNED_ARTIFACT_ROOT=${runnerTemp}\\pdf-workstation-signed-artifact`,
      `UPGRADE_WORK_ROOT=${runnerTemp}\\pdf-workstation-upgrade-work`,
      `UPGRADE_OUTPUT_ROOT=${runnerTemp}\\pdf-workstation-upgrade-output`,
      `WEBDRIVER_ROOT=${runnerTemp}\\pdf-workstation-webdriver`,
      `WEBVIEW_PROFILE_ROOT=${runnerTemp}\\pdf-workstation-webview-profile`,
    ]);
  });

  it('keeps installer execution bounded, hidden, silent, and runner-only', () => {
    const script = readFileSync('scripts/verify-signed-ocr-upgrade.ps1', 'utf8');
    expect(script).toContain("runnerEnvironment -cne 'github-hosted'");
    expect(script).toContain("repository -cne $script:Pins.Repository");
    expect(script).toContain("imageOs -cne 'win22'");
    expect(script).toContain("ArgumentList.Add('/S')");
    expect(script).toContain('CreateNoWindow = $true');
    expect(script).toContain('WindowStyle = [Diagnostics.ProcessWindowStyle]::Hidden');
    expect(script).toContain('WaitForExit($TimeoutMilliseconds)');
    expect(script).toContain('Kill($true)');
    expect(script).toContain("Registry::HKEY_CURRENT_USER\\Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\PDF Workstation");
    expect(script).toContain("Join-Path $env:LOCALAPPDATA 'PDF Workstation'");
    expect(script).not.toMatch(/Start-Process|Remove-Item|\.Delete\(|uninstall\.exe['\"]?\s+\/S/i);
    expect(script).toContain('applicationProcessStarted = $true');
    expect(script).toContain('applicationLaunchVerified = $true');
    expect(script).toContain('webViewDomVerified = $true');
    expect(script).toContain('nativeWindowVisualVerified = $false');
    expect(script).toContain("[ValidateSet('HostedNonInteractive')][string]$PublisherUiMode");
    expect(script).toContain("$PublisherUiMode -cne 'HostedNonInteractive'");
    expect(script).toContain("publisherUiMode = 'hosted-noninteractive'");
    expect(script).toContain('publisherUiAttempted = $false');
    expect(script).toContain('installerShellPublisherUiVerified = $false');
    expect(script).toContain('installedApplicationShellPublisherUiVerified = $false');
    expect(script).toContain('publisherUiScreenshotsUsed = $false');
    expect(script).toContain('New-HostedPublisherUiNotAttemptedEvidence');
    expect(script).not.toContain('= Invoke-InstalledPublisherUiProof');
    expect(script).toContain('uacPublisherPromptVerified = $false');
    expect(script).toContain('smartScreenPublisherPromptVerified = $false');
    expect(script).toContain('realPreferencesVerified = $false');
    expect(script).toContain('installedEngineOcrSmokeVerified = $true');
    expect(script).toContain('applicationOcrIntegrationVerified = $true');
    expect(script).toContain('applicationCurrentPageOcrCompleted = $true');
    expect(script).toContain('applicationOcrRecognitionVerified = $true');
    expect(script).toContain('applicationOcrAccuracyVerified = $false');
    expect(script).not.toContain('ocrExecutionVerified');
    expect(script).toContain('inAppUpdaterVerified = $false');
    expect(script).toContain('public static class OcrBoundedProcess');
    expect(readFileSync('scripts/setup-ocr.ps1', 'utf8')).toContain('process.Kill(true)');
    const generatorGuard = script.indexOf('Assert-NoReparseAncestors -Path $smokeGenerator');
    const generatorReceipt = script.indexOf('$smokeGeneratorReceipt = [pscustomobject]');
    const generatorExecution = script.indexOf('$smokeMetadata = & $smokeGenerator');
    expect(generatorGuard).toBeGreaterThan(-1);
    expect(generatorReceipt).toBeGreaterThan(generatorGuard);
    expect(generatorExecution).toBeGreaterThan(generatorReceipt);
  });

  it('rejects untrusted runner state, ambient credentials, and nonfresh machines', () => {
    const check = functionHarness(
      ['Assert-ExactProperties', 'Assert-HostedRunnerFacts', 'Assert-FreshInstallFacts'],
      String.raw`
        $facts = [pscustomobject]@{
          githubActions='true'; runnerEnvironment='github-hosted'; runnerOs='Windows'; imageOs='win22'
          eventName='workflow_dispatch'; ref='refs/heads/master'; repository='Joshua-Beel/PDF-Workstation'
          githubSha=('a' * 40); headSha=('a' * 40); trackedClean=$true; indexClean=$true
          githubTokenPresent=$false; ghTokenPresent=$false; ambientSigningConfigurationPresent=$false
        }
        Assert-HostedRunnerFacts -Facts $facts
        foreach ($mutation in @(
          @{name='repository';value='someone/fork'}, @{name='runnerEnvironment';value='self-hosted'},
          @{name='trackedClean';value=$false}, @{name='indexClean';value=$false},
          @{name='githubTokenPresent';value=$true}, @{name='ambientSigningConfigurationPresent';value=$true}
        )) {
          $copy = $facts.PSObject.Copy(); $copy.($mutation.name) = $mutation.value
          $rejected=$false; try { Assert-HostedRunnerFacts -Facts $copy } catch { $rejected=$true }
          if (-not $rejected) { throw ('Unsafe runner mutation was accepted: ' + $mutation.name) }
        }
        $fresh=[pscustomobject]@{registryPresent=$false;installRootPresent=$false;settingsRootPresent=$false;applicationProcessPresent=$false;installerProcessPresent=$false}
        Assert-FreshInstallFacts -Facts $fresh
        foreach($name in $fresh.PSObject.Properties.Name) {
          $copy=$fresh.PSObject.Copy(); $copy.$name=$true
          $rejected=$false; try { Assert-FreshInstallFacts -Facts $copy } catch { $rejected=$true }
          if(-not $rejected){throw ('Nonfresh machine state was accepted: '+$name)}
        }
      `,
      false,
    );
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);

  it('rejects malformed signed receipts and validates the exact five OCR roles', () => {
    const check = functionHarness(
      ['Assert-ExactProperties', 'Assert-ReceiptValue', 'Assert-SignedArtifactReceipt'],
      String.raw`
        function New-Value { param([string]$Sha=('A'*64)); [pscustomobject]@{bytes=[uint64]1;sha256=$Sha} }
        $base=@()
        for($i=0;$i -lt 27;$i++){$base += [pscustomobject]@{target=('resources/base/file-'+$i+'.txt');bytes=[uint64]1;sha256=('A'*64)}}
        $base += [pscustomobject]@{target='resources/pdfium/bin/pdfium.dll';bytes=[uint64]2;sha256=('B'*64);unsignedBytes=[uint64]1;unsignedSha256=('C'*64)}
        $ocr=@(
          [pscustomobject]@{suffix='bin/tesseract.exe';role='engine';bytes=[uint64]1;sha256=('D'*64)},
          [pscustomobject]@{suffix='tessdata/eng.traineddata';role='model';bytes=[uint64]1;sha256=('E'*64)},
          [pscustomobject]@{suffix='licenses/Tesseract-Apache-2.0.txt';role='license';bytes=[uint64]1;sha256=('F'*64)},
          [pscustomobject]@{suffix='licenses/Leptonica-BSD-2-Clause.txt';role='license';bytes=[uint64]1;sha256=('1'*64)},
          [pscustomobject]@{suffix='licenses/eng-fast-Apache-2.0.txt';role='license';bytes=[uint64]1;sha256=('2'*64)}
        )
        $receipt=[pscustomobject]@{
          schemaVersion=1;scope='Manual artifact-only signed OCR installer verification; no installation, launch, update, or release behavior is established.'
          mode='azure-signed-artifact-only-ocr';sourceRevision=$script:Pins.SignedSourceRevision
          installer=[pscustomobject]@{fileName=$script:Pins.SignedInstallerName;bytes=$script:Pins.SignedInstallerBytes;sha256=$script:Pins.SignedInstallerSha256}
          packagedApplication=(New-Value);baseResources=$base
          ocr=[pscustomobject]@{originalEngine=(New-Value);packagedResources=$ocr;licensesArePackagedSidecarsNotNoticeDialogContent=$true}
          signatures=[pscustomobject]@{expectedPublisher=$script:Pins.ExpectedPublisher;installer='Valid';application='Valid';engine='Valid';pdfium='Valid';originalEngine='NotSigned';trustedTimestampsRequired=$true}
          verification=[pscustomobject]@{archiveInventory=(New-Value);extractedResourcesMatched=$true;bundleContainsOnlyInstaller=$true;updaterArtifacts=@();ephemeralSigningConfigurationRetained=$false}
        }
        $script:Pins['BaseResourceTargets']=@($base|ForEach-Object{[string]$_.target}|Sort-Object -CaseSensitive)
        $sets=Assert-SignedArtifactReceipt -Receipt $receipt
        if($sets.Base.Count -ne 28 -or $sets.Ocr.Count -ne 5){throw 'Valid exact receipt failed.'}
        $saved=$script:Pins['BaseResourceTargets']
        $script:Pins['BaseResourceTargets']=@($saved[0..26])
        $rejected=$false;try{Assert-SignedArtifactReceipt -Receipt $receipt}catch{$rejected=$true};if(-not $rejected){throw 'A base-resource count mismatch was accepted.'}
        $script:Pins['BaseResourceTargets']=@($saved[0..26])+@('resources/base/other.txt')
        $rejected=$false;try{Assert-SignedArtifactReceipt -Receipt $receipt}catch{$rejected=$true};if(-not $rejected){throw 'A base-resource target mismatch was accepted.'}
        $script:Pins['BaseResourceTargets']=$saved
        $wrong=$receipt.PSObject.Copy();$wrong.sourceRevision=('9'*40)
        $rejected=$false;try{Assert-SignedArtifactReceipt -Receipt $wrong}catch{$rejected=$true};if(-not $rejected){throw 'Wrong source revision accepted.'}
        $wrong=$receipt.PSObject.Copy();$wrong.ocr=$receipt.ocr.PSObject.Copy();$wrong.ocr.packagedResources=@($ocr[0..3])
        $rejected=$false;try{Assert-SignedArtifactReceipt -Receipt $wrong}catch{$rejected=$true};if(-not $rejected){throw 'Incomplete OCR receipt accepted.'}
        $wrong=$receipt.PSObject.Copy();$wrong.signatures=$receipt.signatures.PSObject.Copy();$wrong.signatures.expectedPublisher='Wrong Publisher'
        $rejected=$false;try{Assert-SignedArtifactReceipt -Receipt $wrong}catch{$rejected=$true};if(-not $rejected){throw 'Wrong publisher accepted.'}
        $wrong=$receipt.PSObject.Copy();$wrong.verification=$receipt.verification.PSObject.Copy();$wrong.verification.bundleContainsOnlyInstaller='true'
        $rejected=$false;try{Assert-SignedArtifactReceipt -Receipt $wrong}catch{$rejected=$true};if(-not $rejected){throw 'Malformed Boolean accepted.'}
      `,
    );
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('accepts only the receipt-bound payload and proven 7-Zip 24 or 26 NSIS inventory', () => {
    const check = functionHarness(
      ['Assert-ExtractedInstallerInventory'],
      String.raw`
        $target=(Resolve-Path target).Path
        $identity=('B'*64).ToLowerInvariant()
        $receipt=[pscustomobject]@{ocr=[pscustomobject]@{originalEngine=[pscustomobject]@{sha256=('A'*64)}}}
        $sets=[pscustomobject]@{
          Base=@([pscustomobject]@{target='resources/base/a.txt'},[pscustomobject]@{target='resources/base/b.txt'})
          Ocr=@([pscustomobject]@{suffix='bin/tesseract.exe'},[pscustomobject]@{suffix='tessdata/eng.traineddata'})
          Engine=[pscustomobject]@{sha256=('B'*64)}
        }
        $required=@(
          'pdf-workstation.exe','resources/base/a.txt','resources/base/b.txt',
          ('resources/ocr/'+$identity+'/bin/tesseract.exe'),('resources/ocr/'+$identity+'/tessdata/eng.traineddata'),
          '$PLUGINSDIR/modern-wizard.bmp','$PLUGINSDIR/nsDialogs.dll','$PLUGINSDIR/nsis_tauri_utils.dll',
          '$PLUGINSDIR/NSISdl.dll','$PLUGINSDIR/StartMenu.dll','$PLUGINSDIR/System.dll'
        )
        function New-Inventory {
          param([string]$Name,[bool]$IncludeUninstaller=$false,[string]$Extra,[string]$Omit)
          $root=Join-Path $target ($Name+'-'+[Guid]::NewGuid().ToString('N'))
          [IO.Directory]::CreateDirectory($root)|Out-Null
          foreach($relative in $required){
            if($relative -ceq $Omit){continue}
            $path=Join-Path $root $relative
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path))|Out-Null
            [IO.File]::WriteAllText($path,'fixture',[Text.UTF8Encoding]::new($false))
          }
          if($IncludeUninstaller){[IO.File]::WriteAllText((Join-Path $root 'uninstall.exe'),'fixture',[Text.UTF8Encoding]::new($false))}
          if($Extra){
            $path=Join-Path $root $Extra
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path))|Out-Null
            [IO.File]::WriteAllText($path,'fixture',[Text.UTF8Encoding]::new($false))
          }
          return [pscustomobject]@{Root=$root;Files=@(Get-ChildItem -LiteralPath $root -Recurse -File -Force)}
        }
        $v24=New-Inventory -Name 'v24'
        Assert-ExtractedInstallerInventory -Files $v24.Files -ExtractionRoot $v24.Root -Receipt $receipt -ReceiptSets $sets
        $v26=New-Inventory -Name 'v26' -IncludeUninstaller $true
        Assert-ExtractedInstallerInventory -Files $v26.Files -ExtractionRoot $v26.Root -Receipt $receipt -ReceiptSets $sets
        foreach($case in @(
          (New-Inventory -Name 'extra-root' -Extra 'surprise.exe'),
          (New-Inventory -Name 'extra-plugin' -Extra '$PLUGINSDIR/surprise.dll'),
          (New-Inventory -Name 'updater-json' -Extra 'latest.json'),
          (New-Inventory -Name 'updater-signature' -Extra 'setup.SIG'),
          (New-Inventory -Name 'missing' -Omit '$PLUGINSDIR/System.dll')
        )){
          $rejected=$false;try{Assert-ExtractedInstallerInventory -Files $case.Files -ExtractionRoot $case.Root -Receipt $receipt -ReceiptSets $sets}catch{$rejected=$true}
          if(-not $rejected){throw 'Unsafe or incomplete extracted inventory was accepted.'}
        }
      `,
    );
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('reuses the bounded setup runner and enforces its live timeout and output cap', () => {
    const check = functionHarness(
      ['Import-BoundedOcrProcess'],
      String.raw`
        Import-BoundedOcrProcess -SetupScriptPath (Resolve-Path ./scripts/setup-ocr.ps1)
        $pwsh=(Get-Command pwsh.exe).Source
        $empty=[string[]]@()
        $echo=[OcrBoundedProcess]::Run($pwsh,[string[]]@('-NoProfile','-NonInteractive','-Command','$value=[Console]::In.ReadToEnd();[Console]::Out.Write($value)'),$null,$empty,5000,64,64,[Text.Encoding]::UTF8.GetBytes('probe'))
        if($echo.TimedOut -or $echo.StdoutExceeded -or $echo.StderrExceeded -or $echo.ExitCode -ne 0 -or $echo.Stdout -cne 'probe' -or $echo.Stderr){throw 'Bounded stdin/stdout probe failed.'}
        $cap=[OcrBoundedProcess]::Run($pwsh,[string[]]@('-NoProfile','-NonInteractive','-Command','[Console]::Out.Write((''X''*1024));Start-Sleep -Seconds 30'),$null,$empty,5000,32,64,$null)
        if(-not $cap.StdoutExceeded -or $cap.TimedOut -or $cap.ElapsedMilliseconds -ge 5000){throw 'Live stdout cap did not terminate the fake child.'}
        $timeout=[OcrBoundedProcess]::Run($pwsh,[string[]]@('-NoProfile','-NonInteractive','-Command','Start-Sleep -Seconds 30'),$null,$empty,200,64,64,$null)
        if(-not $timeout.TimedOut -or $timeout.ElapsedMilliseconds -ge 5000){throw 'Live timeout did not terminate the fake child.'}
      `,
    );
    const result = runPowerShell7(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);

  it('binds direct installed-engine smoke to exact trusted assets, oracle, profile, and caps', () => {
    const check = functionHarness(
      ['Assert-ExactProperties', 'Assert-FileReceipt', 'Get-Utf8TextSha256', 'Normalize-OcrSmokeText', 'Invoke-InstalledEngineOcrSmoke'],
      String.raw`
        $root=Join-Path (Resolve-Path target) ('installed-engine-smoke-'+[Guid]::NewGuid().ToString('N'))
        $engine=Join-Path $root 'resources/ocr/identity/bin/tesseract.exe'
        $model=Join-Path $root 'resources/ocr/identity/tessdata/eng.traineddata'
        $input=Join-Path $root 'smoke/known-text.pnm'
        foreach($path in @($engine,$model,$input)){[IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($path))|Out-Null}
        [IO.File]::WriteAllText($engine,'engine',[Text.UTF8Encoding]::new($false))
        [IO.File]::WriteAllText($model,'model',[Text.UTF8Encoding]::new($false))
        $generator=(Resolve-Path ./scripts/ocr/generate-smoke.ps1).Path
        $metadata=& $generator -OutputPath $input
        $engineItem=Get-Item $engine;$modelItem=Get-Item $model;$generatorItem=Get-Item $generator
        $engineReceipt=[pscustomobject]@{bytes=[uint64]$engineItem.Length;sha256=Get-ExactSha256 $engine}
        $modelReceipt=[pscustomobject]@{bytes=[uint64]$modelItem.Length;sha256=Get-ExactSha256 $model}
        $generatorReceipt=[pscustomobject]@{bytes=[uint64]$generatorItem.Length;sha256=Get-ExactSha256 $generator}
        $signature={param($path)[pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$true}}
        $calls=0
        $provider={param($request)$script:calls++;$expectedArguments='stdin|stdout|--tessdata-dir|'+[IO.Path]::GetDirectoryName($model)+'|-l|eng|--oem|1|--psm|6|--dpi|150|--loglevel|ERROR';if(($request.Arguments -join '|') -cne $expectedArguments){throw 'OCR smoke arguments changed.'};if($request.TimeoutMilliseconds-ne30000-or$request.MaximumStdoutCharacters-ne1MB-or$request.MaximumStderrCharacters-ne64KB-or$request.StandardInput.Length-gt16MB-or-not([Text.Encoding]::ASCII.GetString($request.StandardInput,0,2)-ceq'P6')){throw 'OCR smoke caps or input changed.'};[pscustomobject]@{Stdout=($script:Pins.SmokeExpectedText+[string][char]13+[char]10);Stderr='';ExitCode=[int]0;ElapsedMilliseconds=[int]20;TimedOut=$false;StdoutExceeded=$false;StderrExceeded=$false}}
        $parameters=@{EnginePath=$engine;ModelPath=$model;InputPath=$input;GeneratorPath=$generator;RepositoryRoot=(Get-Location).Path;GeneratorReceipt=$generatorReceipt;GeneratorMetadata=$metadata;EngineReceipt=$engineReceipt;ModelReceipt=$modelReceipt;SignatureProvider=$signature;ProcessProvider=$provider}
        $proof=Invoke-InstalledEngineOcrSmoke @parameters
        if($calls-ne1-or-not$proof.matched-or-not$proof.stderrEmpty-or$proof.expectedTextSha256-cne$proof.actualTextSha256-or$proof.profile.language-cne'eng'-or$proof.limits.timeoutMilliseconds-ne30000){throw 'Installed-engine smoke proof mismatch.'}
        $badResults=@(
          [pscustomobject]@{Stdout=$script:Pins.SmokeExpectedText;Stderr='';ExitCode=[int]0;ElapsedMilliseconds=[int]20;TimedOut=$true;StdoutExceeded=$false;StderrExceeded=$false},
          [pscustomobject]@{Stdout=$script:Pins.SmokeExpectedText;Stderr='';ExitCode=[int]0;ElapsedMilliseconds=[int]20;TimedOut=$false;StdoutExceeded=$true;StderrExceeded=$false},
          [pscustomobject]@{Stdout=$script:Pins.SmokeExpectedText;Stderr='warning';ExitCode=[int]0;ElapsedMilliseconds=[int]20;TimedOut=$false;StdoutExceeded=$false;StderrExceeded=$false},
          [pscustomobject]@{Stdout='wrong';Stderr='';ExitCode=[int]0;ElapsedMilliseconds=[int]20;TimedOut=$false;StdoutExceeded=$false;StderrExceeded=$false},
          [pscustomobject]@{Stdout=$script:Pins.SmokeExpectedText;Stderr='';ExitCode=[int]7;ElapsedMilliseconds=[int]20;TimedOut=$false;StdoutExceeded=$false;StderrExceeded=$false}
        )
        foreach($bad in $badResults){$badParameters=$parameters.Clone();$badParameters.ProcessProvider={param($request)$bad}.GetNewClosure();$rejected=$false;try{Invoke-InstalledEngineOcrSmoke @badParameters}catch{$rejected=$true};if(-not$rejected){throw 'Unsafe installed-engine smoke result accepted.'}}
        $callsBeforeValidation=$calls
        $badMetadata=[pscustomobject]@{path=$metadata.path;width=$metadata.width;height=$metadata.height;font=$metadata.font;expectedText='wrong oracle'}
        $badMetadataParameters=$parameters.Clone();$badMetadataParameters.GeneratorMetadata=$badMetadata
        $rejected=$false;try{Invoke-InstalledEngineOcrSmoke @badMetadataParameters}catch{$rejected=$true};if(-not$rejected-or$calls-ne$callsBeforeValidation){throw 'Generator metadata mismatch was not rejected before OCR execution.'}
        $alternateGenerator=Join-Path $root 'alternate-generator.ps1'
        [IO.File]::Copy($generator,$alternateGenerator)
        $alternateGeneratorItem=Get-Item $alternateGenerator
        $alternateGeneratorParameters=$parameters.Clone();$alternateGeneratorParameters.GeneratorPath=$alternateGenerator;$alternateGeneratorParameters.GeneratorReceipt=[pscustomobject]@{bytes=[uint64]$alternateGeneratorItem.Length;sha256=Get-ExactSha256 $alternateGenerator}
        $rejected=$false;try{Invoke-InstalledEngineOcrSmoke @alternateGeneratorParameters}catch{$rejected=$true};if(-not$rejected-or$calls-ne$callsBeforeValidation){throw 'Alternate generator path was not rejected before OCR execution.'}
        [IO.File]::WriteAllText($model,'tampered',[Text.UTF8Encoding]::new($false))
        $rejected=$false;try{Invoke-InstalledEngineOcrSmoke @parameters}catch{$rejected=$true};if(-not$rejected-or$calls-ne$callsBeforeValidation){throw 'Tampered model passed immediate pre-execution receipt check.'}
        [IO.File]::WriteAllText($model,'model',[Text.UTF8Encoding]::new($false))
        if((Get-ExactSha256 $model)-cne$modelReceipt.sha256){throw 'Model fixture restoration failed.'}
        [IO.File]::WriteAllText($engine,'tampered',[Text.UTF8Encoding]::new($false))
        $rejected=$false;try{Invoke-InstalledEngineOcrSmoke @parameters}catch{$rejected=$true};if(-not$rejected-or$calls-ne$callsBeforeValidation){throw 'Tampered engine passed immediate pre-execution receipt check.'}
      `,
    );
    const result = runPowerShell7(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);

  it('isolates Explorer publisher UI behind a hard process deadline and accepts only one sanitized JSON object', () => {
    const helper = readFileSync('scripts/installed-publisher-ui.ps1', 'utf8');
    expect(helper).toContain('SEE_MASK_INVOKEIDLIST | SEE_MASK_NOASYNC');
    expect(helper).toContain('info.lpVerb = "properties"');
    expect(helper).toContain("SignatureTab = 'Digital Signatures'");
    expect(helper).toContain("DetailsTitle = 'Digital Signature Details'");
    expect(helper).toContain("ValidStatus = 'This digital signature is OK.'");
    expect(helper).toContain('[Windows.Automation.SelectionItemPattern]::Pattern');
    expect(helper).toContain('GetCurrentPattern(InvokePattern.Pattern)');
    expect(helper).toContain('TaskCreationOptions.LongRunning');
    expect(helper).toContain('InvokeExactButtonAsync(long rootHandleValue');
    expect(helper).toContain('AutomationElement.FromHandle(new IntPtr(rootHandleValue))');
    expect(helper).toContain('root.Current.NativeWindowHandle != rootHandleValue');
    expect(helper).toContain('return Task.Factory.StartNew(() =>');
    expect(helper).toContain('[Windows.Automation.ValuePattern]::Pattern');
    expect(helper).toContain("-Name 'Embedded Signatures' -ControlType ([Windows.Automation.ControlType]::DataGrid)");
    expect(helper).toContain('-Root $signatureGrids[0] -Name $ExpectedPublisher -ControlType ([Windows.Automation.ControlType]::DataItem)');
    expect(helper).toContain('Where-Object { [bool]$_.Current.IsEnabled }');
    expect(helper).toContain('$buttons[0].SetFocus()');
    expect(helper).toContain('NativeShell]::OwnedBy($ExpectedOwner)');
    expect(helper).toContain('Wait-PublisherUiExactOwnedWindow');
    expect(helper).toContain("-Root $details -Name 'Name:' -ControlType ([Windows.Automation.ControlType]::Edit)");
    expect(helper).toContain('$value.EndsWith("`r",[StringComparison]::Ordinal)');
    expect(helper).toContain('ElementMaximum = 256');
    expect(helper).toContain('DeadlineMilliseconds = 70000');
    expect(helper).toContain('InteractionMilliseconds = 65000');
    expect(helper).toContain('ChildDeadlineMilliseconds = 64000');
    expect(helper).toContain('CleanupMilliseconds = 5000');
    expect(helper).toContain('ExpectedOwner $propertiesHandle');
    expect(helper).toContain('$propertiesCleanupMatches');
    expect(helper).toContain('$detailsCleanupMatches');
    expect(helper).toContain('Close-PublisherUiWindow');
    expect(helper).toContain('$detailsInvokeTask = Invoke-PublisherUiButton -RootHandle $propertiesHandle -RootTitle $propertiesTitle');
    expect(helper).toContain('-InvokeTask $detailsInvokeTask');
    expect(helper).toContain('Complete-PublisherUiButtonInvoke -Task $detailsInvokeTask');
    expect(helper).toContain('JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE');
    expect(helper).toContain('TerminateJobObject(job, 125)');
    expect(helper).toContain('start.Environment.Clear()');
    expect(helper).toContain("'SystemRoot','SystemDrive','WINDIR','ProgramData'");
    expect(helper).toContain('StdoutMaximum = 4096');
    expect(helper).toContain('StderrMaximum = 2048');
    expect(helper).toContain("FailureTokenPrefix = 'publisher-ui-child-failed:v1'");
    expect(helper).toContain('New-PublisherUiChildFailureToken -Stage $script:PublisherUiChildStage -Category $script:PublisherUiChildCategory');
    expect(helper).toContain('ConvertFrom-PublisherUiChildFailureToken');
    expect(helper).toContain('$script:PublisherUiChildStage = $primaryFailureStage');
    expect(helper).toContain('$script:PublisherUiChildCategory = $primaryFailureCategory');
    expect(helper).not.toContain('[Delegate]::CreateDelegate');
    expect(helper).not.toContain('[scriptblock]$SurfaceProvider');
    const interactiveProof = helper.slice(helper.indexOf('function Invoke-InstalledPublisherUiProof'), helper.indexOf('function New-HostedPublisherUiNotAttemptedEvidence'));
    const hostedEvidence = helper.slice(helper.indexOf('function New-HostedPublisherUiNotAttemptedEvidence'), helper.indexOf('function Invoke-PublisherUiChild'));
    expect(interactiveProof).toContain('Invoke-PublisherUiHelperProcess');
    expect(hostedEvidence).not.toContain('Invoke-PublisherUiHelperProcess');
    for (const body of [interactiveProof, hostedEvidence]) {
      expect(body.match(/Assert-PublisherUiFileReceipt -Path \$canonical/g)).toHaveLength(2);
      expect(body.match(/Assert-TrustedWindowsSignature -Path \$canonical/g)).toHaveLength(2);
    }
    expect(helper.match(/Assert-PublisherUiFileReceipt -Path \$canonical/g)).toHaveLength(4);
    expect(helper.match(/Assert-TrustedWindowsSignature -Path \$canonical/g)).toHaveLength(4);
    expect(helper).not.toMatch(/AZURE_|TAURI_SIGNING|CopyFromScreen|Bitmap|Save\(/i);

    const parentStart = helper.indexOf('function Invoke-PublisherUiHelperProcess');
    const parentEnd = helper.indexOf('function Get-PublisherUiSha256', parentStart);
    const parentBoundary = helper.slice(parentStart, parentEnd);
    expect(parentBoundary).toContain('BoundedProcess]::Run');
    expect(parentBoundary).not.toMatch(/Windows\.Automation|ShellExecuteEx|FindAll\(|TryGetCurrentPattern|\.Select\(\)|\.Invoke\(\)/);

    const detailsClose = helper.indexOf('if ($detailsHandle -ne 0) { Close-PublisherUiWindow');
    const invokeComplete = helper.indexOf('Complete-PublisherUiButtonInvoke -Task $detailsInvokeTask', detailsClose);
    const propertiesClose = helper.indexOf('if ($propertiesHandle -ne 0) { Close-PublisherUiWindow', invokeComplete);
    expect(detailsClose).toBeGreaterThan(-1);
    expect(invokeComplete).toBeGreaterThan(detailsClose);
    expect(propertiesClose).toBeGreaterThan(invokeComplete);

    const check = String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/installed-publisher-ui.ps1
      $null=Get-PublisherUiCultureFacts -CultureProvider {[pscustomobject]@{Culture='en-US';UICulture='en-US'}}
      $rejected=$false;try{Get-PublisherUiCultureFacts -CultureProvider {[pscustomobject]@{Culture='fr-FR';UICulture='fr-FR'}}|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Non-en-US publisher UI culture was accepted.'}
      $fixture=Join-Path (Resolve-Path target) ('publisher-ui-'+[Guid]::NewGuid().ToString('N')+'.exe')
      [IO.File]::WriteAllText($fixture,'signed fixture',[Text.UTF8Encoding]::new($false))
      $item=Get-Item -LiteralPath $fixture
      $receipt=[pscustomobject]@{bytes=[uint64]$item.Length;sha256=Get-PublisherUiSha256 -Path $fixture}
      Assert-PublisherUiFileReceipt -Path $fixture -Receipt $receipt
      $rejected=$false;try{Assert-PublisherUiFileReceipt -Path $fixture -Receipt ([pscustomobject]@{bytes=$receipt.bytes;sha256=('A'*64)})}catch{$rejected=$true};if(-not$rejected){throw 'Wrong publisher UI receipt was accepted.'}
      Initialize-PublisherUiIsolation
      Initialize-PublisherUiInterop
      $runningSource=[Threading.Tasks.TaskCompletionSource[bool]]::new()
      if((Get-PublisherUiInvokeTaskState -Task $runningSource.Task)-cne'running'){throw 'Running publisher UI task state was misclassified.'}
      $runningSource.SetResult($true)
      if((Get-PublisherUiInvokeTaskState -Task $runningSource.Task)-cne'completed'){throw 'Completed publisher UI task state was misclassified.'}
      $canceled=[Threading.Tasks.Task]::FromCanceled([Threading.CancellationToken]::new($true))
      if((Get-PublisherUiInvokeTaskState -Task $canceled)-cne'canceled'){throw 'Canceled publisher UI task state was misclassified.'}
      $faulted=[Threading.Tasks.Task]::FromException([InvalidOperationException]::new('controlled'))
      if((Get-PublisherUiInvokeTaskState -Task $faulted)-cne'faulted'){throw 'Faulted publisher UI task state was misclassified.'}
      foreach($taskCase in @(@{task=$canceled;category='invoke-canceled'},@{task=$faulted;category='invoke-faulted'})){
        $script:PublisherUiChildCategory='unexpected';$rejected=$false
        try{Complete-PublisherUiButtonInvoke -Task $taskCase.task -Deadline ([datetime]::UtcNow.AddSeconds(1))}catch{$rejected=$true}
        if(-not$rejected-or$script:PublisherUiChildCategory-cne$taskCase.category){throw 'Terminal publisher UI task failure was not categorized.'}
      }
      $neverCompletes=[Threading.Tasks.TaskCompletionSource[bool]]::new()
      $script:PublisherUiChildCategory='unexpected';$rejected=$false
      try{Complete-PublisherUiButtonInvoke -Task $neverCompletes.Task -Deadline ([datetime]::UtcNow.AddMilliseconds(-1))}catch{$rejected=$true}
      if(-not$rejected-or$script:PublisherUiChildCategory-cne'invoke-running'){throw 'Running publisher UI task deadline was not categorized.'}
      $encoded=[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('[Console]::Out.Write($PID);[Console]::Out.Flush();Start-Sleep -Seconds 30'))
      $arguments=@('-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand',$encoded)
      $result=[Smacrobat.PublisherUiIsolation.BoundedProcess]::Run((Join-Path $PSHOME 'pwsh.exe'),$arguments,(Get-Location).Path,(Get-PublisherUiMinimalEnvironment),300,4096,2048)
      if(-not$result.TimedOut-or-not$result.JobAssigned-or-not$result.JobTerminated-or-not$result.ProcessStopped-or$result.ElapsedMilliseconds-gt2500){throw 'Hung publisher UI helper was not killed inside its hard deadline.'}
      $sameProcess=$false
      try{$process=Get-Process -Id $result.ProcessId -ErrorAction Stop;$sameProcess=$process.StartTime.ToUniversalTime().Ticks-eq$result.ProcessStartTimeUtcTicks}catch{}
      if($sameProcess){throw 'Timed-out publisher UI helper identity is still running.'}

      $overflow=[Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes('[Console]::Out.Write((''X''*8192));[Console]::Out.Flush();Start-Sleep -Seconds 30'))
      $overflowResult=[Smacrobat.PublisherUiIsolation.BoundedProcess]::Run((Join-Path $PSHOME 'pwsh.exe'),@('-NoLogo','-NoProfile','-NonInteractive','-EncodedCommand',$overflow),(Get-Location).Path,(Get-PublisherUiMinimalEnvironment),5000,128,128)
      if(-not$overflowResult.StdoutExceeded-or-not$overflowResult.ProcessStopped-or$overflowResult.Stdout.Length-ne128){throw 'Publisher UI helper stdout cap was not enforced.'}

      $json=([pscustomobject][ordered]@{uiCulture='en-US';shellPropertiesDialog=$true;digitalSignaturesTab=$true;signerRowMatched=$true;detailsDialog=$true;statusTextMatched=$true;cleanupVerified=$true;screenshotsUsed=$false}|ConvertTo-Json -Compress)
      function New-HelperResult([string]$stdout,[string]$stderr='',[int]$exitCode=0){[pscustomobject][ordered]@{Stdout=$stdout;Stderr=$stderr;ExitCode=$exitCode;ElapsedMilliseconds=[int]10;TimedOut=$false;StdoutExceeded=$false;StderrExceeded=$false;JobAssigned=$true;JobTerminated=$false;ProcessStopped=$true;ProcessId=[int]1;ProcessStartTimeUtcTicks=[long]1}}
      $surface=ConvertFrom-PublisherUiHelperOutput -Result (New-HelperResult $json)
      if(-not$surface.cleanupVerified-or$surface.screenshotsUsed){throw 'Exact sanitized publisher UI JSON was rejected.'}
      foreach($bad in @('{"uiCulture":',($json+$json),($json+[Environment]::NewLine))){$rejected=$false;try{ConvertFrom-PublisherUiHelperOutput -Result (New-HelperResult $bad)|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Partial or extra publisher UI output was accepted.'}}
      $screenshot=$json.Replace('"screenshotsUsed":false','"screenshotsUsed":true')
      $rejected=$false;try{ConvertFrom-PublisherUiHelperOutput -Result (New-HelperResult $screenshot)|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Screenshot-backed publisher UI output was accepted.'}
      foreach($stage in $script:PublisherUiPins.FailureStages){
        foreach($category in $script:PublisherUiPins.FailureCategories){
          $token=New-PublisherUiChildFailureToken -Stage $stage -Category $category
          $parsed=ConvertFrom-PublisherUiChildFailureToken -Token $token
          if($parsed.stage-cne$stage-or$parsed.category-cne$category){throw 'Allowlisted publisher UI child failure token did not round-trip.'}
        }
      }
      $fixedToken=New-PublisherUiChildFailureToken -Stage 'details-wait' -Category 'invoke-running'
      $fixedMessage=$null;try{ConvertFrom-PublisherUiHelperOutput -Result (New-HelperResult '' $fixedToken 1)|Out-Null}catch{$fixedMessage=$_.Exception.Message}
      if($fixedMessage-cne"Windows publisher UI helper failed at fixed stage 'details-wait' with category 'invoke-running'."){throw 'Allowlisted publisher UI child failure was not reported deterministically.'}
      foreach($badToken in @(
        'publisher-ui-child-failed:v1:unknown:unexpected',
        'publisher-ui-child-failed:v1:details-wait:unknown',
        ('publisher-ui-child-failed:v1:details-wait:unexpected'+[Environment]::NewLine),
        'publisher-ui-child-failed:v1:details-wait:unexpected:C:\runner',
        ('X'*161),
        'raw hosted exception text'
      )){
        $badMessage=$null;try{ConvertFrom-PublisherUiHelperOutput -Result (New-HelperResult '' $badToken 1)|Out-Null}catch{$badMessage=$_.Exception.Message}
        if($badMessage-cne'Windows publisher UI helper process did not complete safely.'-or$badMessage.Contains($badToken)){throw 'Unallowlisted publisher UI child failure data escaped its boundary.'}
      }
    `;
    const result = runPowerShell7(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);

  it('enforces bounded process and installed-registry facts and writes one sanitized record', () => {
    const check = functionHarness(
      ['Assert-ExactProperties', 'Invoke-BoundedSilentInstaller', 'Assert-InstallFacts', 'Write-SanitizedUpgradeRecord'],
      String.raw`
        $called=$false
        $provider={param($path,$arguments,$timeout)$script:called=$true;if($arguments -cne '/S' -or $timeout -ne 1234){throw 'Process provider contract changed.'};[pscustomobject]@{TimedOut=$false;ExitCode=[int]0}}
        Invoke-BoundedSilentInstaller -Path 'fixture.exe' -TimeoutMilliseconds 1234 -ProcessProvider $provider
        if(-not $called){throw 'Bounded provider was not invoked.'}
        foreach($bad in @([pscustomobject]@{TimedOut=$true;ExitCode=[int]0},[pscustomobject]@{TimedOut=$false;ExitCode=[int]1})){
          $rejected=$false;try{Invoke-BoundedSilentInstaller -Path 'fixture.exe' -ProcessProvider {param($p,$a,$t)$bad}.GetNewClosure()}catch{$rejected=$true};if(-not $rejected){throw 'Unsafe process result accepted.'}
        }
        $root=[IO.Path]::GetFullPath((Join-Path (Resolve-Path target) 'mock-install'))
        $facts=[pscustomobject]@{displayName='PDF Workstation';displayVersion='0.3.0';installLocation=$root;uninstallPath=(Join-Path $root 'uninstall.exe');appBytes=[uint64]10;appSha256=('A'*64);fileVersion='0.3.0';productVersion='0.3.0';signatureStatus='Valid';publisher='Joshua Beel';hasTimestamp=$true}
        $app=[pscustomobject]@{bytes=[uint64]10;sha256=('A'*64)}
        Assert-InstallFacts -Facts $facts -ExpectedVersion '0.3.0' -ExpectedInstallRoot $root -ExpectedApplication $app -ExpectedSignatureStatus 'Valid' -ExpectedPublisher 'Joshua Beel' -ExpectedTimestamp $true
        $wrong=$facts.PSObject.Copy();$wrong.uninstallPath=(Join-Path $root 'other.exe')
        $rejected=$false;try{Assert-InstallFacts -Facts $wrong -ExpectedVersion '0.3.0' -ExpectedInstallRoot $root -ExpectedApplication $app -ExpectedSignatureStatus 'Valid' -ExpectedPublisher 'Joshua Beel' -ExpectedTimestamp $true}catch{$rejected=$true};if(-not $rejected){throw 'Wrong uninstall registry value accepted.'}
        $evidencePath=Join-Path (Resolve-Path target) ('hosted-publisher-'+[Guid]::NewGuid().ToString('N')+'.exe')
        [IO.File]::WriteAllText($evidencePath,'signed fixture',[Text.UTF8Encoding]::new($false))
        $evidenceItem=Get-Item -LiteralPath $evidencePath
        $evidenceReceipt=[pscustomobject]@{bytes=[uint64]$evidenceItem.Length;sha256=Get-PublisherUiSha256 -Path $evidencePath}
        $signatureCalls=0
        $signatureProvider={param($path)$script:signatureCalls++;[pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$true}}
        $hostedEvidence=New-HostedPublisherUiNotAttemptedEvidence -Path $evidencePath -Receipt $evidenceReceipt -Kind installer -ExpectedPublisher 'Joshua Beel' -SignatureProvider $signatureProvider
        if($signatureCalls-ne2-or$hostedEvidence.authenticodeStatus-cne'Valid'-or-not$hostedEvidence.trustedTimestampVerified-or$hostedEvidence.shellPublisherUiAttempted-or$hostedEvidence.shellPublisherUiVerified-or$hostedEvidence.notAttemptedReason-cne'github-hosted-noninteractive-session'){throw 'Hosted publisher evidence did not preserve exact signature checks and false UI claims.'}
        $rejected=$false;try{New-HostedPublisherUiNotAttemptedEvidence -Path $evidencePath -Receipt $evidenceReceipt -Kind installer -ExpectedPublisher 'Joshua Beel' -SignatureProvider {param($path)[pscustomobject]@{Status='Valid';Publisher='Joshua Beel';HasTimestamp=$false}}|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Hosted publisher evidence accepted a missing timestamp.'}
        $output=Join-Path (Resolve-Path target) ('ocr-upgrade-output-'+[Guid]::NewGuid().ToString('N'))
        $receipt=[pscustomobject]@{packagedApplication=$app}
        $smoke=[ordered]@{generator=[ordered]@{target='scripts/ocr/generate-smoke.ps1';bytes=[uint64]1;sha256=('B'*64)};input=[ordered]@{format='P6';width=1200;height=240;bytes=[uint64]1;sha256=('C'*64)};engine=[ordered]@{bytes=[uint64]1;sha256=('D'*64)};model=[ordered]@{bytes=[uint64]1;sha256=('E'*64)};profile=[ordered]@{language='eng';engineMode=1;pageSegmentationMode=6;dpi=150;logLevel='ERROR'};limits=[ordered]@{inputBytesMaximum=16777216;stdoutCharactersMaximum=1048576;stderrCharactersMaximum=65536;timeoutMilliseconds=30000};expectedTextSha256=('F'*64);actualTextSha256=('F'*64);exitCode=0;stderrEmpty=$true;matched=$true}
        $launch=[ordered]@{drivers=[ordered]@{tauriDriver=[ordered]@{version='2.0.6';bytes=[uint64]1;sha256=('1'*64);sourceSha256=('2'*64)};webView2RuntimeVersion='151.0.1.2';edgeDriver=[ordered]@{version='151.0.1.3';bytes=[uint64]1;sha256=('3'*64);publisher='Microsoft Corporation';trustedTimestamp=$true}};profile=[ordered]@{state='controlled-runner-owned';binding='owned-webview-tauri-app-settings-ebwebview';prelaunchSettingsEntries=1;sentinelPreserved=$true};title='PDF Workstation';homeButton='Explore a sample PDF';ocrCapabilityStatus='Available';sample=[ordered]@{name='welcome.pdf';pages=6;firstPageDecoded=$true;naturalWidth=100;naturalHeight=100;source='blob:'};currentPageOcr=[ordered]@{sampleName='welcome.pdf';physicalPage=1;dialogTitle='Recognize text on page 1';status='recognized';language='eng';textUtf8Bytes=12;textSha256=('4'*64);sourceUiPreserved=$true;sourceFileReceiptPreserved=$true;accuracyVerified=$false};cleanup=[ordered]@{sessionDeleted=$true;ownedProcessTreeStopped=$true;relevantProcessesRemaining=0}}
        $publisherEvidence=[pscustomobject][ordered]@{kind='installer';fileName=$script:Pins.SignedInstallerName;bytes=$script:Pins.SignedInstallerBytes;sha256=$script:Pins.SignedInstallerSha256;publisher='Joshua Beel';authenticodeStatus='Valid';trustedTimestampVerified=$true;shellPublisherUiAttempted=$false;shellPublisherUiVerified=$false;notAttemptedReason='github-hosted-noninteractive-session';screenshotsUsed=$false}
        $applicationPublisherEvidence=$publisherEvidence.PSObject.Copy();$applicationPublisherEvidence.kind='installed-application';$applicationPublisherEvidence.fileName='pdf-workstation.exe';$applicationPublisherEvidence.bytes=$app.bytes;$applicationPublisherEvidence.sha256=$app.sha256
        $path=Write-SanitizedUpgradeRecord -OutputRoot $output -WorkflowSourceRevision ('b'*40) -Receipt $receipt -InstalledEngineSmoke $smoke -InstalledAppLaunch $launch -InstallerPublisherEvidence $publisherEvidence -ApplicationPublisherEvidence $applicationPublisherEvidence
        $json=Get-Content -LiteralPath $path -Raw -Encoding UTF8
        $value=$json|ConvertFrom-Json
        $verificationProperties=@('applicationCurrentPageOcrCompleted','applicationLaunchVerified','applicationOcrAccuracyVerified','applicationOcrIntegrationVerified','applicationOcrRecognitionVerified','applicationProcessStarted','baselineSilentInstall','documentSentinelPreserved','ephemeral','inAppUpdaterVerified','installedApplicationShellPublisherUiVerified','installedEngineOcrSmokeVerified','installedReceiptsMatched','installerShellPublisherUiVerified','launchProcessCleanupVerified','nativeDragDropVerified','nativeFilePickerVerified','nativeOcrCapabilityVerified','nativeWindowVisualVerified','printingVerified','publisherUiAttempted','publisherUiMode','publisherUiScreenshotsUsed','realPreferencesVerified','registryVersionUpdated','runner','samplePdfiumRenderVerified','samplePdfOpened','settingsSentinelPreserved','signedSilentManualUpgrade','smartScreenPublisherPromptVerified','uacPublisherPromptVerified','updaterArtifacts','userPdfVerified','webViewDomVerified')
        $actualVerification=@($value.verification.PSObject.Properties.Name|Sort-Object -CaseSensitive);$expectedVerification=@($verificationProperties|Sort-Object -CaseSensitive);if($actualVerification.Count-ne$expectedVerification.Count-or(Compare-Object $expectedVerification $actualVerification -CaseSensitive)){throw 'Sanitized verification claim set changed.'}
        $ocrProperties=@('accuracyVerified','dialogTitle','language','physicalPage','sampleName','sourceFileReceiptPreserved','sourceUiPreserved','status','textSha256','textUtf8Bytes');$actualOcr=@($value.upgrade.installedApplicationLaunch.currentPageOcr.PSObject.Properties.Name|Sort-Object -CaseSensitive);$expectedOcr=@($ocrProperties|Sort-Object -CaseSensitive);if($actualOcr.Count-ne$expectedOcr.Count-or(Compare-Object $expectedOcr $actualOcr -CaseSensitive)){throw 'Sanitized current-page OCR receipt set changed.'}
        if($value.schemaVersion-ne6-or@(Get-ChildItem -LiteralPath $output -File -Force).Count -ne 1 -or-not$value.verification.installedEngineOcrSmokeVerified -or-not$value.verification.applicationLaunchVerified -or-not$value.verification.webViewDomVerified -or-not$value.verification.nativeOcrCapabilityVerified -or-not$value.verification.applicationOcrIntegrationVerified -or-not$value.verification.applicationCurrentPageOcrCompleted -or-not$value.verification.applicationOcrRecognitionVerified -or$value.verification.applicationOcrAccuracyVerified -ne $false -or$value.verification.publisherUiMode-cne'hosted-noninteractive'-or$value.verification.publisherUiAttempted-ne$false-or$value.verification.installerShellPublisherUiVerified-ne$false-or$value.verification.installedApplicationShellPublisherUiVerified-ne$false-or$value.verification.publisherUiScreenshotsUsed-ne$false-or$value.verification.uacPublisherPromptVerified-ne$false-or$value.verification.smartScreenPublisherPromptVerified-ne$false-or$value.verification.nativeWindowVisualVerified-ne$false-or$value.verification.nativeFilePickerVerified-ne$false-or$value.verification.userPdfVerified-ne$false-or$value.verification.printingVerified-ne$false-or$value.verification.nativeDragDropVerified-ne$false-or$value.verification.realPreferencesVerified-ne$false-or$value.verification.inAppUpdaterVerified-ne$false-or-not$value.upgrade.installedEngineOcrSmoke.matched-or$value.upgrade.publisherUi.mode-cne'hosted-noninteractive'-or$value.upgrade.publisherUi.attempted-ne$false-or$value.upgrade.publisherUi.verified-ne$false-or$value.upgrade.publisherUi.reason-cne'github-hosted-noninteractive-session'-or$value.upgrade.publisherUi.installer.kind-cne'installer'-or$value.upgrade.publisherUi.installedApplication.kind-cne'installed-application'-or$value.upgrade.publisherUi.installer.authenticodeStatus-cne'Valid'-or-not$value.upgrade.publisherUi.installer.trustedTimestampVerified-or$value.upgrade.publisherUi.installer.shellPublisherUiAttempted-ne$false-or$value.upgrade.publisherUi.installer.shellPublisherUiVerified-ne$false-or$value.upgrade.publisherUi.installer.screenshotsUsed-ne$false-or$value.upgrade.installedApplicationLaunch.sample.pages-ne6-or$value.upgrade.installedApplicationLaunch.currentPageOcr.physicalPage-ne1-or$value.upgrade.installedApplicationLaunch.currentPageOcr.accuracyVerified-ne$false){throw 'Sanitized output contract failed.'}
        if($json -match 'fixed private OCR output|recognized text sample'){throw 'Sanitized output included raw OCR text.'}
        if($json -match '(?i)([A-Z]:\\|\\Users\\|12510717103|39969839829|569181842|GITHUB_TOKEN|AZURE_|codesigning\.azure\.net|signingAccount|signingProfile|tenantId|clientId)'){throw 'Sanitized output leaked restricted evidence.'}
      `,
    );
    const result = runPowerShell(check);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 60_000);
});
