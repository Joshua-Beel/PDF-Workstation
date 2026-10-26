import { describe, expect, it } from 'vitest';
import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

function runPowerShell(source: string, timeout = 60_000) {
  const root = join(tmpdir(), `smacrobat-image-page-tools-${randomUUID()}`);
  mkdirSync(root, { recursive: true });
  const path = join(root, 'run.ps1');
  writeFileSync(path, source, 'utf8');
  try {
    return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path], {
      encoding: 'utf8', timeout, env: { ...process.env, IMAGE_PAGE_TEST_ROOT: root },
    });
  } finally { rmSync(root, { recursive: true, force: true }); }
}

function extractFunctions(names: string[], body: string) {
  return String.raw`
    $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
    $tokens=$null;$errors=$null
    $path='${process.cwd().replaceAll("'", "''")}\\scripts\\installed-image-page-tools.ps1'
    $ast=[Management.Automation.Language.Parser]::ParseFile($path,[ref]$tokens,[ref]$errors)
    if($errors.Count){throw 'Installed image page-tools script did not parse.'}
    foreach($name in @(${names.map(name => `'${name}'`).join(',')})){
      $function=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq$name},$true)
      if(-not$function){throw ('Missing function: '+$name)}
      Invoke-Expression $function.Extent.Text
    }
    ${body}
  `;
}

describe('installed image page-tools verifier', () => {
  it('enforces the caller-selected PowerShell timeout', () => {
    const result = runPowerShell('Start-Sleep -Seconds 30', 100);
    expect(result.error && (result.error as NodeJS.ErrnoException).code).toBe('ETIMEDOUT');
  });

  it('parses and composes the reviewed process-bound Open and exact native Save seams', () => {
    const source = readFileSync('scripts/installed-image-page-tools.ps1', 'utf8');
    const parsed = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command',
      `$t=$null;$e=$null;[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\\scripts\\installed-image-page-tools.ps1',[ref]$t,[ref]$e)|Out-Null;if($e.Count){$e|% ToString;exit 1}`,
    ], { encoding: 'utf8', timeout: 15_000 });
    expect(parsed.status, parsed.stderr || parsed.stdout).toBe(0);
    expect(source).toContain(". (Join-Path $PSScriptRoot 'installed-reading-tools.ps1')");
    expect(source).toContain(". (Join-Path $PSScriptRoot 'installed-print-dialog.ps1')");
    expect(source).toContain('Wait-ReadingProcessBoundPickerTargets');
    expect(source).toContain('Submit-ProcessBoundOpenDialog');
    expect(source).toContain("Wait-NewProcessNativeWindowSurface -ProcessId $ProcessId -Baseline $NativeBaseline");
    expect(source).toContain('Set-BoundNativeSaveFileNameExact');
    expect(source).toContain('Invoke-BoundNativeSaveButtonExact');
    expect(source).toContain("-ActionTransport 'native-bm-click-delivered'");
    expect(source).toContain('-ApplicationProcessStartUtcTicks $ProcessStartUtcTicks');
    expect(source).toContain('-Environment (Get-MinimalWindowsProcessEnvironment)');
    expect(source).not.toContain('\\"');
    expect(source).not.toMatch(/\bAssert-FileReceipt\b/);
    expect(source).not.toMatch(/SendKeys|mouse_event|SetCursorPos|ClickInput|pyautogui|__TAURI_INTERNALS__/i);
    expect(source.indexOf('$nativeBaseline = @(Get-ProcessNativeWindowSnapshot')).toBeLessThan(source.indexOf("textContent.trim()==='Create PDF'"));
    expect(source.indexOf('Submit-ProcessBoundOpenDialog')).toBeLessThan(source.indexOf('Invoke-ImagePageNativeSave -ProcessId'));
  });

  it('passes complete Create and Export dialog selector scripts to WebDriver', () => {
    const result = runPowerShell(String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/installed-image-page-tools.ps1
      $script:captured=[Collections.Generic.List[string]]::new()
      function Invoke-WebDriverScript{param($SessionId,$Script,$Deadline)$script:captured.Add([string]$Script);return $false}
      function Get-ReadingProcessUiSurfaceSnapshot{@()}
      function Assert-ReadingSurfaceSnapshot{@()}
      function Get-ReadingPickerTargetSnapshot{@()}
      function Assert-ReadingPickerTargetSnapshot{}
      function Get-ProcessNativeWindowSnapshot{@()}
      try{Invoke-ImagePageCreateNativeFlow -SessionId 'session' -ProcessId 42 -ProcessStartUtcTicks 10 -SourceImagePath 'C:\proof\source.png' -CreatedPdfPath 'C:\proof\source.pdf' -Deadline ([datetime]::UtcNow.AddSeconds(5))}catch{}
      try{Invoke-ImagePageExportNativeFlow -SessionId 'session' -ProcessId 42 -ProcessStartUtcTicks 10 -ExportedPngPath 'C:\proof\source-page-1.png' -ExportSourcePdfPath 'C:\proof\source.pdf' -Deadline ([datetime]::UtcNow.AddSeconds(5))}catch{}
      $expectedCreate=@'
const d=document.querySelector('dialog[aria-labelledby="create-pdf-title"]');const b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Create PDF'&&!x.disabled):[];if(b.length===1)setTimeout(()=>b[0].click(),0);return b.length===1;
'@
      $expectedExport=@'
const d=document.querySelector('dialog[aria-labelledby="export-image-title"]');const b=d?[...d.querySelectorAll('button')].filter(x=>x.textContent.trim()==='Export PNG'&&!x.disabled):[];if(b.length===1)setTimeout(()=>b[0].click(),0);return b.length===1;
'@
      if($script:captured.Count-ne2-or$script:captured[0]-cne$expectedCreate-or$script:captured[1]-cne$expectedExport){throw ('WebDriver selector scripts were truncated or changed: '+($script:captured|ConvertTo-Json -Compress))}
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path './scripts/installed-image-page-tools.ps1'),[ref]$tokens,[ref]$errors)
      $dialogScripts=@($ast.FindAll({param($node)$node-is[Management.Automation.Language.StringConstantExpressionAst]-and$node.Value-like'*aria-labelledby*'},$true)|ForEach-Object Value)
      $createDefaults=@($dialogScripts|Where-Object{$_-like'*Page size*'});$exportDefaults=@($dialogScripts|Where-Object{$_-like'*Image resolution*'})
      if($errors.Count-or$dialogScripts.Count-ne6-or$createDefaults.Count-ne1-or-not$createDefaults[0].Contains('dialog[aria-labelledby="create-pdf-title"]')-or-not$createDefaults[0].Contains('select[aria-label="Page size"]')-or$exportDefaults.Count-ne1-or-not$exportDefaults[0].Contains('dialog[aria-labelledby="export-image-title"]')-or-not$exportDefaults[0].Contains('select[aria-label="Image resolution"]')){throw 'One or more evaluated image WebDriver dialog scripts are truncated or changed.'}
      $script:captured|ConvertTo-Json -Compress
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    const scripts = JSON.parse(result.stdout.trim()) as string[];
    for (const [index, buttonText] of ['Create PDF', 'Export PNG'].entries()) {
      const timers: Array<() => void> = [], events: string[] = [];
      const button = { textContent: buttonText, disabled: false, click: () => events.push('click') };
      const document = { querySelector: () => ({ querySelectorAll: () => [button] }) };
      const receipt = new Function('document', 'setTimeout', scripts[index])(document, (callback: () => void) => timers.push(callback));
      expect(receipt).toBe(true);
      expect(events).toEqual([]);
      expect(timers).toHaveLength(1);
      timers[0]();
      expect(events).toEqual(['click']);
    }
  });

  it('reports only fixed WebDriver stage and failure categories', () => {
    const result = runPowerShell(extractFunctions(['Get-ImagePageWebDriverFailureCategory', 'Invoke-ImagePageWebDriverScript', 'Wait-ImagePageWebDriverOracle'], String.raw`
      function Invoke-WebDriverScript{throw 'WebDriver response headers exceeded their deadline. ambient-secret'}
      $invokeMessage='';try{Invoke-ImagePageWebDriverScript -SessionId session -Script 'return true' -Deadline ([datetime]::UtcNow.AddSeconds(1)) -Stage submit-create}catch{$invokeMessage=$_.Exception.Message}
      if($invokeMessage-cne'Installed image page-tools WebDriver stage failed: submit-create/response-headers-timeout.'-or$invokeMessage-like'*ambient-secret*'){throw 'Invoke stage diagnostic was missing or disclosed the source exception.'}
      function Wait-WebDriverOracle{throw 'The remote server returned an HTTP 500 response containing ambient-secret'}
      $waitMessage='';try{Wait-ImagePageWebDriverOracle -SessionId session -Script 'return true' -Deadline ([datetime]::UtcNow.AddSeconds(1)) -Predicate {$true} -Stage export-defaults}catch{$waitMessage=$_.Exception.Message}
      if($waitMessage-cne'Installed image page-tools WebDriver stage failed: export-defaults/http-failure.'-or$waitMessage-like'*ambient-secret*'){throw 'Oracle stage diagnostic was missing or disclosed the source exception.'}
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('launches the verifier with only the exact allowlisted environment and excludes ambient tokens', () => {
    const result = runPowerShell(String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/installed-app-launch.ps1
      $env:GITHUB_TOKEN='ambient-github-token';$env:GH_TOKEN='ambient-gh-token';$env:AZURE_CLIENT_SECRET='ambient-azure-secret';$env:TAURI_SIGNING_PRIVATE_KEY='ambient-signing-key'
      $minimal=Get-MinimalWindowsProcessEnvironment
      $expected=@('COMSPEC','SystemRoot','WINDIR','LOCALAPPDATA','APPDATA','USERPROFILE','TEMP','TMP','PATH','PATHEXT')|Sort-Object -CaseSensitive
      $actual=@($minimal.Keys|ForEach-Object{[string]$_}|Sort-Object -CaseSensitive)
      if(($actual-join',')-cne($expected-join',')){throw 'Minimal verifier environment names changed.'}
      foreach($name in @('GITHUB_TOKEN','GH_TOKEN','AZURE_CLIENT_SECRET','TAURI_SIGNING_PRIVATE_KEY')){if($minimal.Contains($name)){throw "Ambient token-like variable escaped: $name"}}
      $capture=Start-BoundedDiscardProcess -Path $env:COMSPEC -Arguments @('/d','/c','exit 0') -Environment $minimal
      try{
        $childNames=@($capture.Process.StartInfo.Environment.Keys|ForEach-Object{[string]$_}|Sort-Object -CaseSensitive)
        if(($childNames-join',')-cne($expected-join',')){throw 'Verifier ProcessStartInfo did not replace the inherited environment.'}
        $capture.Start();if(-not$capture.Process.WaitForExit(5000)){throw 'Minimal-environment verifier probe did not exit.'}
      }finally{$capture.Dispose()}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('performs an exact native Save mutation in bind, set, click, close order', () => {
    const result = runPowerShell(extractFunctions(['Invoke-ImagePageNativeSave'], String.raw`
      $script:events=[Collections.Generic.List[string]]::new()
      function Wait-NewProcessNativeWindowSurface{$script:events.Add('bind');[pscustomobject]@{nativeRoles=[pscustomobject]@{cancel='a';save='b';filenameEdit='c'}}}
      function Get-ValidatedBindingNativeRoles{param($Binding)$script:events.Add('validate');$Binding.nativeRoles}
      function Set-BoundNativeSaveFileNameExact{$script:events.Add('set')}
      function Invoke-BoundNativeSaveButtonExact{$script:events.Add('click')}
      function Wait-BoundProcessUiSurfaceClosed{$script:events.Add('close')}
      $ok=Invoke-ImagePageNativeSave -ProcessId 42 -ProcessStartUtcTicks 10 -NativeBaseline @([pscustomobject]@{runtimeIdentity='hwnd:1|native'}) -OutputPath 'C:\proof\output.pdf' -SuggestedName 'output.pdf' -Deadline ([datetime]::UtcNow.AddSeconds(5))
      if(-not$ok-or($script:events-join',')-cne'bind,validate,set,click,close'){throw ('Unexpected native Save order: '+($script:events-join','))}
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('runs a generic callback inside one fresh owned session and returns lifecycle fields in the original order', () => {
    const result = runPowerShell(extractFunctions(['Invoke-FreshInstalledImagePageToolsSession'], String.raw`
      $script:events=[Collections.Generic.List[string]]::new();$script:driver=[pscustomobject]@{Id=42;HasExited=$false}
      $script:PrintPins=@{PortHandoffTimeoutMilliseconds=1000};$script:LaunchPins=@{WebDriverPort=4444;NativeDriverPort=4445;CleanupProcessTimeoutMilliseconds=1000};$script:ImagePagePins=@{TotalTimeoutMilliseconds=5000}
      function Wait-FixedWebDriverPortsFree{$script:events.Add('ports')}
      function Get-MinimalWindowsProcessEnvironment{@{SystemRoot='C:\Windows'}}
      $capture=[pscustomobject]@{Process=$script:driver;Exceeded=$false}
      $capture|Add-Member -MemberType ScriptMethod -Name Start -Value {$script:events.Add('start')}
      $capture|Add-Member -MemberType ScriptMethod -Name Dispose -Value {$script:events.Add('dispose')}
      function Start-BoundedDiscardProcess{$capture}
      function Invoke-BoundedLoopbackJson{param($Method,$Path,$Body,$Deadline)if($Path-ceq'/status'){[pscustomobject]@{value=[pscustomobject]@{ready=$true}}}else{[pscustomobject]@{value=[pscustomobject]@{sessionId='session-1';capabilities=[pscustomobject]@{browserVersion='1.2.3.9';'msedge.msedgedriverVersion'='1.2.3.4';'msedge.userDataDir'='C:\profile'}}}}}
      function Wait-NativeDriverStatus{'1.2.3.4'}
      function Wait-OwnedPrintApplication{[pscustomobject]@{ProcessId=77;ProcessStartUtcTicks=88;Owned=@([pscustomobject]@{id=77})}}
      function Get-OwnedLaunchProcesses{@([pscustomobject]@{id=77})}
      function Get-UniqueOwnedLaunchProcesses{param($Processes)@($Processes)}
      function Assert-PrintOwnedExecutables{$script:events.Add('owned')}
      function Get-ExactProfileBinding{'requested-profile'}
      function Invoke-SessionDeleteOutcome{$script:events.Add('delete');'verified'}
      function Stop-OwnedLaunchProcesses{param($TauriDriver)$script:events.Add('stop');$TauriDriver.HasExited=$true;[pscustomobject]@{rootOutcome='verified';application='stopped';tauriDriver='stopped';edgeDriver='stopped';webview='stopped';ocrEngine='absent';other='absent'}}
      function Wait-LaunchProcessQuiescence{[pscustomobject]@{stable=$true;processes=@()}}
      function Get-LaunchResidualCategory{'none'}
      function Get-LaunchResidualFacts{[pscustomobject]@{ownership='none';application=$false;tauriDriver=$false;edgeDriver=$false;webview=$false;ocrEngine=$false;other=$false}}
      function Assert-LaunchCleanupState{param($Result)$script:events.Add('assert');if($Result.payload-cne'ok'){throw 'Cleanup received the wrong callback result.'}}
      $callback={param($context)$script:events.Add('callback');if($context.SessionId-cne'session-1'-or$context.ApplicationProcessId-ne77-or$context.ApplicationProcessStartUtcTicks-ne88-or$context.Deadline-le[datetime]::UtcNow){throw 'Fresh-session callback context changed.'};[pscustomobject]@{payload='ok'}}
      $value=Invoke-FreshInstalledImagePageToolsSession -ApplicationPath 'C:\app.exe' -TauriDriverPath 'C:\tauri.exe' -EdgeDriverPath 'C:\edge.exe' -ProfileRoot 'C:\profile' -SettingsRoot 'C:\settings' -ExpectedEdgeDriverVersion '1.2.3.4' -SessionCallback $callback
      $names=@($value.PSObject.Properties.Name)
      if(($names-join',')-cne'nativeDriverVersion,returnedRuntimeVersion,profileBinding,payload,sessionDeleted,ownedProcessTreeStopped,relevantProcessesRemaining'){throw ('Fresh-session result order changed: '+($names-join','))}
      if(($script:events-join',')-cne'ports,start,callback,owned,delete,stop,dispose,assert'){throw ('Fresh-session order changed: '+($script:events-join','))}
      if(-not$value.sessionDeleted-or-not$value.ownedProcessTreeStopped-or$value.relevantProcessesRemaining-ne0){throw 'Fresh-session cleanup receipt changed.'}
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('cleans up a failed generic callback while preserving the callback error', () => {
    const result = runPowerShell(extractFunctions(['Invoke-FreshInstalledImagePageToolsSession'], String.raw`
      $script:events=[Collections.Generic.List[string]]::new();$script:driver=[pscustomobject]@{Id=42;HasExited=$false}
      $script:PrintPins=@{PortHandoffTimeoutMilliseconds=1000};$script:LaunchPins=@{WebDriverPort=4444;NativeDriverPort=4445;CleanupProcessTimeoutMilliseconds=1000};$script:ImagePagePins=@{TotalTimeoutMilliseconds=5000}
      function Wait-FixedWebDriverPortsFree{}
      function Get-MinimalWindowsProcessEnvironment{@{}}
      $capture=[pscustomobject]@{Process=$script:driver;Exceeded=$false};$capture|Add-Member ScriptMethod Start {};$capture|Add-Member ScriptMethod Dispose {$script:events.Add('dispose')}
      function Start-BoundedDiscardProcess{$capture}
      function Invoke-BoundedLoopbackJson{param($Path)if($Path-ceq'/status'){[pscustomobject]@{value=[pscustomobject]@{ready=$true}}}else{[pscustomobject]@{value=[pscustomobject]@{sessionId='session-1';capabilities=[pscustomobject]@{browserVersion='1.2.3.9'}}}}}
      function Wait-NativeDriverStatus{'1.2.3.4'}
      function Wait-OwnedPrintApplication{[pscustomobject]@{ProcessId=77;ProcessStartUtcTicks=88;Owned=@()}}
      function Invoke-SessionDeleteOutcome{$script:events.Add('delete');'requestfailed'}
      function Get-OwnedLaunchProcesses{@()}
      function Get-UniqueOwnedLaunchProcesses{param($Processes)@($Processes)}
      function Stop-OwnedLaunchProcesses{param($TauriDriver)$script:events.Add('stop');$TauriDriver.HasExited=$true;[pscustomobject]@{rootOutcome='verified';application='absent';tauriDriver='stopped';edgeDriver='absent';webview='absent';ocrEngine='absent';other='absent'}}
      function Wait-LaunchProcessQuiescence{[pscustomobject]@{stable=$true;processes=@()}}
      function Get-LaunchResidualCategory{'none'}
      function Get-LaunchResidualFacts{[pscustomobject]@{ownership='none';application=$false;tauriDriver=$false;edgeDriver=$false;webview=$false;ocrEngine=$false;other=$false}}
      function Assert-LaunchCleanupState{throw 'cleanup error must not replace callback error'}
      $message='';try{Invoke-FreshInstalledImagePageToolsSession -ApplicationPath 'C:\app.exe' -TauriDriverPath 'C:\tauri.exe' -EdgeDriverPath 'C:\edge.exe' -ProfileRoot 'C:\profile' -SettingsRoot 'C:\settings' -ExpectedEdgeDriverVersion '1.2.3.4' -SessionCallback {$script:events.Add('callback');throw 'callback failure'}}catch{$message=$_.Exception.Message}
      if($message-cne'callback failure'){throw ('Callback error precedence changed: '+$message)}
      if(($script:events-join',')-cne'callback,delete,stop,dispose'){throw ('Failed callback cleanup order changed: '+($script:events-join','))}
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('requires one exact 5906 pixels-per-metre PNG density chunk and matching decoded pixels', () => {
    const result = runPowerShell(String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/installed-image-page-tools.ps1
      Add-Type -AssemblyName System.Drawing
      $root=$env:IMAGE_PAGE_TEST_ROOT
      $path=Join-Path $root 'rgb.png'
      $bitmap=[Drawing.Bitmap]::new(12,8,[Drawing.Imaging.PixelFormat]::Format24bppRgb)
      try{
        $graphics=[Drawing.Graphics]::FromImage($bitmap)
        try{$graphics.Clear([Drawing.Color]::White);$graphics.FillRectangle([Drawing.Brushes]::Blue,0,0,6,8);$graphics.FillRectangle([Drawing.Brushes]::Red,6,0,6,8)}finally{$graphics.Dispose()}
        $bitmap.SetResolution(150,150)
        $bitmap.Save($path,[Drawing.Imaging.ImageFormat]::Png)
      }finally{$bitmap.Dispose()}
      function Find-Chunk([byte[]]$png,[string]$wanted){$offset=33;while($offset-lt$png.Length){$length=[Net.IPAddress]::NetworkToHostOrder([BitConverter]::ToInt32($png,$offset));$name=[Text.Encoding]::ASCII.GetString($png,$offset+4,4);if($name-ceq$wanted){return [pscustomobject]@{offset=$offset;total=$length+12}};$offset+=$length+12};throw 'Chunk missing.'}
      Add-Type -TypeDefinition 'public static class ImagePagePngCrc { public static uint Compute(byte[] bytes,int offset,int count){uint crc=0xffffffff;for(int i=offset;i<offset+count;i++){crc^=bytes[i];for(int bit=0;bit<8;bit++)crc=(crc&1)!=0?0xedb88320^(crc>>1):crc>>1;}return ~crc;} }'
      $bytes=[IO.File]::ReadAllBytes($path);$physicalRecord=@(Find-Chunk $bytes 'pHYs')[-1];$physicalOffset=[int]$physicalRecord.offset;$physicalTotal=[int]$physicalRecord.total;$meter=[uint32]5906
      foreach($base in @(($physicalOffset+8),($physicalOffset+12))){for($index=0;$index-lt4;$index++){$bytes[$base+$index]=[byte](($meter-shr(24-8*$index))-band255)}};$bytes[$physicalOffset+16]=1
      $crc=[ImagePagePngCrc]::Compute($bytes,$physicalOffset+4,13);for($index=0;$index-lt4;$index++){$bytes[$physicalOffset+17+$index]=[byte](($crc-shr(24-8*$index))-band255)};[IO.File]::WriteAllBytes($path,$bytes)
      $proof=Get-ImagePagePngProof -Path $path
      if($proof.format-cne'png'-or$proof.width-ne12-or$proof.height-ne8-or$proof.dpi-ne150-or$proof.pixelsPerMeter-ne5906-or$proof.bytes-lt33-or[string]$proof.sha256-cnotmatch'^[A-F0-9]{64}$'){throw 'RGB PNG proof was not exact.'}
      Initialize-ImagePageRasterProof
      $expected=[byte[]]::new(12*8*3);for($y=0;$y-lt8;$y++){for($x=0;$x-lt12;$x++){$i=($y*12+$x)*3;if($x-lt6){$expected[$i]=0;$expected[$i+1]=0;$expected[$i+2]=255}else{$expected[$i]=255;$expected[$i+1]=0;$expected[$i+2]=0}}}
      $comparison=[ImagePagePdfiumRasterProof]::ComparePng($expected,$path,12,8)
      if($comparison.MeanChannelDifference-ne0-or$comparison.MaximumChannelDifference-ne0-or$comparison.DifferentChannelFraction-ne0){throw 'Decoded PNG pixels differed from their expected raster.'}
      $bytes=[IO.File]::ReadAllBytes($path);$bytes[25]=6;$headerCrc=[ImagePagePngCrc]::Compute($bytes,12,17);for($index=0;$index-lt4;$index++){$bytes[29+$index]=[byte](($headerCrc-shr(24-8*$index))-band255)};[IO.File]::WriteAllBytes((Join-Path $root 'rgba-header.png'),$bytes)
      $rejected=$false;try{Get-ImagePagePngProof -Path (Join-Path $root 'rgba-header.png')}catch{$rejected=$_.Exception.Message-ceq'Exported PNG is not an exact non-interlaced 8-bit RGB image.'}
      if(-not$rejected){throw 'A non-RGB PNG header was accepted.'}
      $bytes=[IO.File]::ReadAllBytes($path)
      $missing=[byte[]]::new($bytes.Length-$physicalTotal);[Array]::Copy($bytes,0,$missing,0,$physicalOffset);[Array]::Copy($bytes,$physicalOffset+$physicalTotal,$missing,$physicalOffset,$bytes.Length-$physicalOffset-$physicalTotal);[IO.File]::WriteAllBytes((Join-Path $root 'missing-density.png'),$missing)
      $missingRejected=$false;try{Get-ImagePagePngProof -Path (Join-Path $root 'missing-density.png')}catch{$missingRejected=$_.Exception.Message-ceq'Exported PNG must contain exactly one pHYs chunk.'}
      $wrong=[byte[]]$bytes.Clone();$wrong[$physicalOffset+11]=[byte]0x11;$wrongCrc=[ImagePagePngCrc]::Compute($wrong,$physicalOffset+4,13);for($index=0;$index-lt4;$index++){$wrong[$physicalOffset+17+$index]=[byte](($wrongCrc-shr(24-8*$index))-band255)};[IO.File]::WriteAllBytes((Join-Path $root 'wrong-density.png'),$wrong)
      $wrongRejected=$false;try{Get-ImagePagePngProof -Path (Join-Path $root 'wrong-density.png')}catch{$wrongRejected=$_.Exception.Message-ceq'Exported PNG pHYs density is not the exact 150 DPI metre value.'}
      $corruptCrc=[byte[]]$bytes.Clone();$corruptCrc[$physicalOffset+17]=[byte]($corruptCrc[$physicalOffset+17] -bxor 1);[IO.File]::WriteAllBytes((Join-Path $root 'corrupt-crc.png'),$corruptCrc)
      $crcRejected=$false;try{Get-ImagePagePngProof -Path (Join-Path $root 'corrupt-crc.png')}catch{$crcRejected=$_.Exception.Message-like'*Exported PNG chunk CRC is invalid.*'}
      $duplicate=[byte[]]::new($bytes.Length+$physicalTotal);[Array]::Copy($bytes,0,$duplicate,0,$physicalOffset);[Array]::Copy($bytes,$physicalOffset,$duplicate,$physicalOffset,$physicalTotal);[Array]::Copy($bytes,$physicalOffset,$duplicate,$physicalOffset+$physicalTotal,$physicalTotal);[Array]::Copy($bytes,$physicalOffset+$physicalTotal,$duplicate,$physicalOffset+2*$physicalTotal,$bytes.Length-$physicalOffset-$physicalTotal);[IO.File]::WriteAllBytes((Join-Path $root 'duplicate-density.png'),$duplicate)
      $duplicateRejected=$false;try{Get-ImagePagePngProof -Path (Join-Path $root 'duplicate-density.png')}catch{$duplicateRejected=$_.Exception.Message-ceq'Exported PNG must contain exactly one pHYs chunk.'}
      $iendRecord=@(Find-Chunk $bytes 'IEND')[-1];$iendOffset=[int]$iendRecord.offset
      $withoutPhysical=[byte[]]::new($bytes.Length-$physicalTotal);[Array]::Copy($bytes,0,$withoutPhysical,0,$physicalOffset);[Array]::Copy($bytes,$physicalOffset+$physicalTotal,$withoutPhysical,$physicalOffset,$bytes.Length-$physicalOffset-$physicalTotal)
      $movedOffset=$iendOffset-$physicalTotal;$moved=[byte[]]::new($bytes.Length);[Array]::Copy($withoutPhysical,0,$moved,0,$movedOffset);[Array]::Copy($bytes,$physicalOffset,$moved,$movedOffset,$physicalTotal);[Array]::Copy($withoutPhysical,$movedOffset,$moved,$movedOffset+$physicalTotal,$withoutPhysical.Length-$movedOffset);[IO.File]::WriteAllBytes((Join-Path $root 'late-density.png'),$moved)
      $lateRejected=$false;try{Get-ImagePagePngProof -Path (Join-Path $root 'late-density.png')}catch{$lateRejected=$_.Exception.Message-ceq'Exported PNG pHYs chunk must precede the first IDAT chunk.'}
      if(-not$missingRejected-or-not$wrongRejected-or-not$duplicateRejected-or-not$crcRejected-or-not$lateRejected){throw "PNG regressions failed: missing=$missingRejected wrong=$wrongRejected duplicate=$duplicateRejected crc=$crcRejected late=$lateRejected"}
    `, 12_000);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  }, 15_000);

  it('requires the deterministic source-color regions in the independent signed-PDFium raster', () => {
    const result = runPowerShell(String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/installed-image-page-tools.ps1
      Initialize-ImagePageRasterProof
      Add-Type -TypeDefinition @'
public static class ExpectedImagePageFixture {
  public static byte[] Raster(bool corruptBlue) {
    const int width=1275,height=1650;var rgb=new byte[width*height*3];for(int i=0;i<rgb.Length;i++)rgb[i]=255;
    for(int y=75;y<1575;y++)for(int x=138;x<1138;x++){int i=(y*width+x)*3;if(x<471){rgb[i]=218;rgb[i+1]=71;rgb[i+2]=64;}else if(x<805){rgb[i]=41;rgb[i+1]=139;rgb[i+2]=98;}else{rgb[i]=(byte)(corruptBlue?120:49);rgb[i+1]=(byte)(corruptBlue?30:94);rgb[i+2]=(byte)(corruptBlue?120:171);}}
    return rgb;
  }
}
'@
      $proof=[ImagePagePdfiumRasterProof]::ProveExpectedRegions([ExpectedImagePageFixture]::Raster($false),1275,1650)
      if($proof.RedFraction-lt0.99-or$proof.GreenFraction-lt0.99-or$proof.BlueFraction-lt0.99-or$proof.WhiteFraction-lt0.99-or[string]$proof.Sha256-cnotmatch'^[A-F0-9]{64}$'){throw 'Deterministic source-color proof was not exact.'}
      $rejected=$false;try{[ImagePagePdfiumRasterProof]::ProveExpectedRegions([ExpectedImagePageFixture]::Raster($true),1275,1650)}catch{$rejected=$_.Exception.Message-like'*deterministic source-color regions*'}
      if(-not$rejected){throw 'A corrupted source-color region was accepted.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('pins one exact default Create result and one exact 150 DPI PNG export result', () => {
    const source = readFileSync('scripts/installed-image-page-tools.ps1', 'utf8');
    expect(source).toContain("pageSize -ceq 'letter'");
    expect(source).toContain("orientation -ceq 'auto'");
    expect(source).toContain("margin -ceq '36'");
    expect(source).toContain("format -ceq 'png'");
    expect(source).toContain("dpi -ceq '150'");
    expect(source).toContain('ExpectedPdfWidthPoints = 612.0');
    expect(source).toContain('ExpectedPdfHeightPoints = 792.0');
    expect(source).toContain('ExpectedExportWidth = 1275');
    expect(source).toContain('ExpectedExportHeight = 1650');
    expect(source).toContain("Get-PdfiumPrintProof -PdfiumPath $PdfiumPath -PdfPath $CreatedPdfPath");
    expect(source).toContain("Get-ImagePagePdfiumRasterProof -PdfiumPath $PdfiumPath -PdfPath $CreatedPdfPath");
    expect(source).toContain('[ImagePagePdfiumRasterProof]::ComparePng');
    expect(source).toContain('ExpectedPixelsPerMeter = 5906');
    expect(source).toContain("Get-ImagePagePngProof -Path $ExportedPngPath");
    expect(source).toContain("'nativeDriverVersion','returnedRuntimeVersion','profileBinding','sourcePickerControlCategory','createSaveDialogVerified','createdPdf','exportSaveDialogVerified','exportedImage','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining'");
    expect(source).toContain("'bytes','sha256','pages','widthPoints','heightPoints','workspaceOpened'");
    expect(source).toContain("'bytes','sha256','format','width','height','dpi','uiCompleted'");
  });

  it('enforces the deterministic entry/result contract around a supplied process runner', () => {
    const result = runPowerShell(String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/installed-image-page-tools.ps1
      function Assert-TrustedWindowsSignature{param($Path,$ExpectedPublisher)$true}
      function Assert-WebDriverReceipt{param($Receipt)}
      function Get-LaunchProcessSnapshot{@()}
      function New-Receipt([string]$path){$item=Get-Item -LiteralPath $path;[pscustomobject]@{bytes=[uint64]$item.Length;sha256=Get-ExactSha256 -Path $path}}
      $root=$env:IMAGE_PAGE_TEST_ROOT
      $env:LOCALAPPDATA=$root
      $settings=Join-Path $root 'local.pdfworkstation.desktop';[IO.Directory]::CreateDirectory($settings)|Out-Null
      $profile=Join-Path $root 'profile';$drivers=Join-Path $root 'drivers'
      [IO.Directory]::CreateDirectory((Join-Path $drivers 'tauri-driver-install/bin'))|Out-Null
      [IO.Directory]::CreateDirectory((Join-Path $drivers 'edge-driver'))|Out-Null
      $tauri=Join-Path $drivers 'tauri-driver-install/bin/tauri-driver.exe';$edge=Join-Path $drivers 'edge-driver/msedgedriver.exe';[IO.File]::WriteAllBytes($tauri,[byte[]](7));[IO.File]::WriteAllBytes($edge,[byte[]](8));$tauriReceipt=New-Receipt $tauri;$edgeReceipt=New-Receipt $edge
      $driverRecord=[ordered]@{tauriDriver=[ordered]@{bytes=$tauriReceipt.bytes;sha256=$tauriReceipt.sha256};edgeDriver=[ordered]@{bytes=$edgeReceipt.bytes;sha256=$edgeReceipt.sha256;version='1.2.3.4'};webView2RuntimeVersion='1.2.3.5'}|ConvertTo-Json -Depth 4 -Compress
      [IO.File]::WriteAllText((Join-Path $drivers 'webdriver-receipt.json'),$driverRecord,[Text.UTF8Encoding]::new($false))
      $application=Join-Path $root 'PDF Workstation.exe';$pdfium=Join-Path $root 'pdfium.dll';$source=Join-Path $root 'source.png'
      [IO.File]::WriteAllBytes($application,[byte[]](1));[IO.File]::WriteAllBytes($pdfium,[byte[]](2));[IO.File]::WriteAllBytes($source,[byte[]](3))
      $created=Join-Path $root 'source.pdf';$exported=Join-Path $root 'source-page-1.png'
      $provider={param($app,$pdfiumPath,$tauri,$edge,$profileRoot,$settingsRoot,$sourcePath,$createdPath,$exportedPath,$edgeVersion)
        [IO.File]::WriteAllBytes($createdPath,[byte[]](4));[IO.File]::WriteAllBytes($exportedPath,[byte[]](5));$createdReceipt=New-Receipt $createdPath;$exportedReceipt=New-Receipt $exportedPath
        [IO.File]::WriteAllText((Join-Path $profileRoot 'profile.marker'),'controlled',[Text.UTF8Encoding]::new($false))
        [pscustomobject]@{
          nativeDriverVersion=$edgeVersion;returnedRuntimeVersion='1.2.3.9';profileBinding='session-capability-requested-profile';sourcePickerControlCategory='edit-1001';createSaveDialogVerified=$true
          createdPdf=[pscustomobject]@{bytes=$createdReceipt.bytes;sha256=$createdReceipt.sha256;pages=1;widthPoints=612.0;heightPoints=792.0;workspaceOpened=$true}
          exportSaveDialogVerified=$true;exportedImage=[pscustomobject]@{bytes=$exportedReceipt.bytes;sha256=$exportedReceipt.sha256;format='png';width=1275;height=1650;dpi=150;uiCompleted=$true}
          sessionDeleted=$true;ownedProcessTreeStopped=$true;relevantProcessesRemaining=0
        }
      }
      $value=Invoke-InstalledImagePageTools -ApplicationPath $application -ApplicationReceipt (New-Receipt $application) -PdfiumPath $pdfium -PdfiumReceipt (New-Receipt $pdfium) -WebDriverRoot $drivers -ProfileRoot $profile -SettingsRoot $settings -SourceImagePath $source -SourceImageReceipt (New-Receipt $source) -CreatedPdfPath $created -ExportedPngPath $exported -ExpectedPublisher 'Publisher' -ProcessProvider $provider
      if($value.createdPdf.pages-ne1-or$value.exportedImage.width-ne1275-or$value.relevantProcessesRemaining-ne0){throw 'Installed entry/result contract changed.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('rejects extra or missing result fields', () => {
    const result = runPowerShell(extractFunctions(['Assert-ImagePageExactProperties'], String.raw`
      Assert-ImagePageExactProperties -Value ([pscustomobject]@{a=1;b=2}) -Expected @('a','b') -Kind 'Exact value'
      $extra=$false;try{Assert-ImagePageExactProperties -Value ([pscustomobject]@{a=1;b=2;c=3}) -Expected @('a','b') -Kind 'Exact value'}catch{$extra=$_.Exception.Message-ceq'Exact value has an unexpected shape.'}
      $missing=$false;try{Assert-ImagePageExactProperties -Value ([pscustomobject]@{a=1}) -Expected @('a','b') -Kind 'Exact value'}catch{$missing=$_.Exception.Message-ceq'Exact value has an unexpected shape.'}
      if(-not$extra-or-not$missing){throw 'Exact result shape was not enforced.'}
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });
});
