import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { describe, expect, it } from 'vitest';

const installed = readFileSync('scripts/installed-organizer-tools.ps1', 'utf8');

function runPowerShell(script: string) {
  return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', script], {
    cwd: process.cwd(), encoding: 'utf8', timeout: 30_000,
  });
}

const root = process.cwd().replaceAll("'", "''");

describe('installed organizer tools workflow', () => {
  it('parses and exposes one deterministic signed-installed entry contract', () => {
    const result = runPowerShell(String.raw`
      $tokens=$null;$errors=$null
      $ast=[Management.Automation.Language.Parser]::ParseFile('${root}\scripts\installed-organizer-tools.ps1',[ref]$tokens,[ref]$errors)
      if($errors.Count){throw ($errors|Out-String)}
      $entry=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq'Invoke-InstalledOrganizerTools'},$true)
      if($null-eq$entry){throw 'Organizer entry was missing.'}
      $parameters=@($entry.Body.ParamBlock.Parameters|ForEach-Object{$_.Name.VariablePath.UserPath})
      $expected='ApplicationPath,ApplicationReceipt,PdfiumPath,PdfiumReceipt,WebDriverRoot,ProfileRoot,SettingsRoot,FixturePath,FixtureReceipt,SplitParentRoot,ExpectedPublisher'
      if(($parameters-join',')-cne$expected){throw 'Organizer entry parameters drifted.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(installed).toContain(". (Join-Path $PSScriptRoot 'installed-reading-tools.ps1')");
    expect(installed).toContain("[IO.Path]::GetFileName($FixturePath) -cne 'organizer-source.pdf'");
    expect(installed).not.toMatch(/Invoke-WebRequest|Invoke-RestMethod|https?:\/\//i);
  });

  it('accepts only the exact path-free organizer result schema and verified values', () => {
    const result = runPowerShell(String.raw`
      . '${root}\scripts\installed-organizer-tools.ps1'
      Initialize-OrganizerPdfiumProof;if($null-eq('OrganizerPdfiumProof'-as[type])){throw 'Organizer PDFium proof type did not compile.'}
      $hashes=[string[]](1..6|ForEach-Object{$_.ToString('X').PadLeft(64,'0')})
      $good=[pscustomobject][ordered]@{
        nativeDriverVersion='151.0.1.2';returnedRuntimeVersion='151.0.1.3';profileBinding='session-capability-requested-profile'
        processBoundOpenPickerVerified=$true;openFilenameControlCategory='edit-1148';organizerWorkspaceVerified=$true
        cropInteractionVerified=$true;croppedPageWidth=540;croppedPageHeight=720;resetCropInteractionVerified=$true
        restoredPageWidth=612;restoredPageHeight=792;processBoundSplitPickerVerified=$true;splitFilenameControlCategory='edit-1152'
        splitOutputFileCount=3;splitOutputBytes=[uint64]12345;splitOutputPageCounts=[int[]]@(2,2,2);sourcePageFingerprintSha256=$hashes;splitPageFingerprintSha256=[string[]]$hashes.Clone();splitPageFingerprintOrderVerified=$true;splitFolderCreatedVerified=$true;sourceFixturePreserved=$true
        sessionDeleted=$true;ownedProcessTreeStopped=$true;relevantProcessesRemaining=0
      }
      Assert-OrganizerResult -Result $good
      foreach($name in @('processBoundOpenPickerVerified','organizerWorkspaceVerified','cropInteractionVerified','resetCropInteractionVerified','processBoundSplitPickerVerified','splitPageFingerprintOrderVerified','splitFolderCreatedVerified','sourceFixturePreserved','sessionDeleted','ownedProcessTreeStopped')){
        $bad=$good|Select-Object *;$bad.$name=$false;$rejected=$false;try{Assert-OrganizerResult -Result $bad}catch{$rejected=$true};if(-not$rejected){throw ('Unverified claim accepted: '+$name)}
      }
      foreach($mutation in @(
        {param($x)$x.croppedPageWidth=541},{param($x)$x.croppedPageHeight=719},{param($x)$x.restoredPageWidth=611},
        {param($x)$x.restoredPageHeight=793},{param($x)$x.splitOutputFileCount=2},{param($x)$x.splitOutputBytes=[uint64]0},
        {param($x)$x.relevantProcessesRemaining=1},{param($x)$x.openFilenameControlCategory='unknown'},
        {param($x)$x.splitFilenameControlCategory='unknown'},{param($x)$x.profileBinding='foreign'}
      )){$bad=$good|Select-Object *;&$mutation $bad;$rejected=$false;try{Assert-OrganizerResult -Result $bad}catch{$rejected=$true};if(-not$rejected){throw 'Unsafe organizer result mutation was accepted.'}}
      $bad=$good|Select-Object *;$bad.splitOutputPageCounts=[int[]]@(2,1,3);$rejected=$false;try{Assert-OrganizerResult -Result $bad}catch{$rejected=$true};if(-not$rejected){throw 'Wrong split page counts were accepted.'}
      $bad=$good|Select-Object *;$changed=[string[]]$hashes.Clone();$changed[5]=$hashes[0];$bad.splitPageFingerprintSha256=$changed;$rejected=$false;try{Assert-OrganizerResult -Result $bad}catch{$rejected=$true};if(-not$rejected){throw 'Wrong split fingerprint order was accepted.'}
      $extra=$good|Select-Object *;$extra|Add-Member leak 'C:\private\output';$rejected=$false;try{Assert-OrganizerResult -Result $extra}catch{$rejected=$true};if(-not$rejected){throw 'Organizer result accepted an extra path-bearing field.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('fingerprints the fixed full-page BGRA raster so translation and proportional scale remain distinct', () => {
    const result = runPowerShell(String.raw`
      . '${root}\scripts\installed-organizer-tools.ps1'
      Initialize-OrganizerPdfiumProof
      function New-PageRaster([int]$left,[int]$top,[int]$edge,[byte]$blue,[byte]$green,[byte]$red){
        $size=64;$stride=$size*4;$raw=[byte[]]::new($stride*$size)
        for($pixel=0;$pixel-lt$size*$size;$pixel++){$offset=$pixel*4;$raw[$offset]=255;$raw[$offset+1]=255;$raw[$offset+2]=255;$raw[$offset+3]=255}
        for($y=$top;$y-lt$top+$edge;$y++){for($x=$left;$x-lt$left+$edge;$x++){$offset=$y*$stride+$x*4;$raw[$offset]=$blue;$raw[$offset+1]=$green;$raw[$offset+2]=$red;$raw[$offset+3]=255}}
        return ,$raw
      }
      $base=New-PageRaster 8 8 16 17 91 203
      $translated=New-PageRaster 24 8 16 17 91 203
      $scaled=New-PageRaster 8 8 24 17 91 203
      $recolored=New-PageRaster 8 8 16 203 91 17
      $hashes=@(
        [OrganizerPdfiumProof]::Fingerprint($base,256,64),
        [OrganizerPdfiumProof]::Fingerprint($translated,256,64),
        [OrganizerPdfiumProof]::Fingerprint($scaled,256,64),
        [OrganizerPdfiumProof]::Fingerprint($recolored,256,64)
      )
      if(@($hashes|Where-Object{$_-cnotmatch'^[A-F0-9]{64}$'}).Count-ne0-or@($hashes|Sort-Object -Unique).Count-ne4){throw 'Full-page BGRA fingerprint lost position, scale, or channel identity.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(installed).toContain('fullPageBgra');
    expect(installed).not.toContain('minX=renderSize');
  });

  it('bounds phase deadlines and fails closed before an expired transition', () => {
    const result = runPowerShell(String.raw`
      . '${root}\scripts\installed-organizer-tools.ps1'
      $script:now=[datetime]'2026-01-01T00:00:00Z';$clock={$script:now}
      foreach($phase in @('launch','native-picker','product')){
        $deadline=New-OrganizerPhaseDeadline -Phase $phase -UtcNowProvider $clock
        if(($deadline-$script:now).TotalMilliseconds-ne180000){throw 'Organizer phase deadline drifted.'}
        Assert-OrganizerPhaseTransition -Deadline $deadline -Phase $phase -UtcNowProvider $clock
        $script:now=$deadline;$rejected=$false;try{Assert-OrganizerPhaseTransition -Deadline $deadline -Phase $phase -UtcNowProvider $clock}catch{$rejected=$true};if(-not$rejected){throw 'Expired organizer phase transition was accepted.'}
        $script:now=[datetime]'2026-01-01T00:00:00Z'
      }
      $script:OrganizerPins.ProductTimeoutMilliseconds=180001;$rejected=$false;try{New-OrganizerPhaseDeadline -Phase product -UtcNowProvider $clock|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Over-cap organizer phase timeout was accepted.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('initializes driver status and reports a bounded readiness refusal', () => {
    const result = runPowerShell(String.raw`
      . '${root}\scripts\installed-organizer-tools.ps1'
      $driver=[pscustomobject]@{HasExited=$false};$script:now=[datetime]'2026-01-01T00:00:00Z';$script:calls=0
      $clock={$script:now};$sleep={param($milliseconds)$script:now=$script:now.AddMilliseconds($milliseconds)}
      $message='';try{Wait-OrganizerDriverReady -Driver $driver -Deadline $script:now.AddMilliseconds(450) -StatusProvider {$script:calls++;throw 'refused'} -SleepProvider $sleep -UtcNowProvider $clock|Out-Null}catch{$message=$_.Exception.Message}
      if($message-cne'Pinned tauri-driver did not become ready for organizer verification before its bounded deadline.'-or$script:calls-ne3-or$script:now-ne[datetime]'2026-01-01T00:00:00.450Z'){throw 'Organizer readiness refusal was not exact and bounded.'}
      if((Get-Content -LiteralPath '${root}\scripts\installed-organizer-tools.ps1' -Raw)-cnotmatch'\$status = \$null'){throw 'Organizer driver status was not initialized.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('launches through the minimal Windows environment without ambient token-like variables', () => {
    const result = runPowerShell(String.raw`
      $env:ORGANIZER_ADVERSARIAL_TOKEN='must-not-cross-process-boundary'
      . '${root}\scripts\installed-organizer-tools.ps1'
      $environment=Get-MinimalWindowsProcessEnvironment
      $expected='APPDATA,COMSPEC,LOCALAPPDATA,PATH,PATHEXT,SystemRoot,TEMP,TMP,USERPROFILE,WINDIR'
      $actual=@($environment.Keys|ForEach-Object{[string]$_}|Sort-Object -CaseSensitive)
      if(($actual-join',')-cne$expected){throw ('Minimal organizer environment drifted: '+($actual-join','))}
      if($environment.Contains('ORGANIZER_ADVERSARIAL_TOKEN')-or@($actual|Where-Object{$_-match'(?i)(token|secret|key|credential)'}).Count){throw 'Ambient token-like environment variable crossed the organizer process boundary.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(installed).toContain('-Environment (Get-MinimalWindowsProcessEnvironment)');
  });

  it('waits for exact baseline, cropped, and restored workspace states and rejects schema drift', () => {
    const result = runPowerShell(String.raw`
      . '${root}\scripts\installed-organizer-tools.ps1'
      $make={param([int]$width,[int]$height,[bool]$dirty)
        [pscustomobject][ordered]@{schemaVersion=1;workspaceCount=1;headingCount=1;pageCount=6;selectedCount=1;selectedPageOne=$true;firstWidth=$width;firstHeight=$height;dirtyIndicator=$dirty;unsavedIndicator=$dirty;cropReady=$true;resetReady=$true;splitReady=$true;dialogCount=0;alertCount=0}
      }
      $script:now=[datetime]'2026-01-01T00:00:00Z';$clock={$script:now};$sleep={param($milliseconds)$script:now=$script:now.AddMilliseconds($milliseconds)}
      $script:states=@((&$make 612 792 $false),(&$make 540 720 $true));$provider={$value=$script:states[0];$script:states=@($script:states|Select-Object -Skip 1);$value}
      $cropped=Wait-OrganizerWorkspaceState -SessionId fixture -ExpectedWidth 540 -ExpectedHeight 720 -ExpectedDirty $true -Deadline $script:now.AddSeconds(1) -StateProvider $provider -SleepProvider $sleep -UtcNowProvider $clock
      if($cropped.firstWidth-ne540-or$cropped.firstHeight-ne720-or-not$cropped.dirtyIndicator){throw 'Cropped organizer state was not exact.'}
      $restored=Wait-OrganizerWorkspaceState -SessionId fixture -ExpectedWidth 612 -ExpectedHeight 792 -ExpectedDirty $false -Deadline $script:now.AddSeconds(1) -StateProvider {&$make 612 792 $false} -SleepProvider $sleep -UtcNowProvider $clock
      if($restored.firstWidth-ne612-or$restored.firstHeight-ne792-or$restored.dirtyIndicator){throw 'Restored organizer state was not exact.'}
      $bad=&$make 612 792 $false;$bad|Add-Member unexpected 1;$rejected=$false;try{Wait-OrganizerWorkspaceState -SessionId fixture -ExpectedWidth 612 -ExpectedHeight 792 -ExpectedDirty $false -Deadline $script:now.AddSeconds(1) -StateProvider {$bad} -SleepProvider $sleep -UtcNowProvider $clock|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Workspace schema drift was accepted.'}
      $alert=&$make 612 792 $false;$alert.alertCount=1;$rejected=$false;try{Wait-OrganizerWorkspaceState -SessionId fixture -ExpectedWidth 612 -ExpectedHeight 792 -ExpectedDirty $false -Deadline $script:now.AddSeconds(1) -StateProvider {$alert} -SleepProvider $sleep -UtcNowProvider $clock|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Workspace alert state was accepted.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('binds split folder selection to the exact app process and native HWND path', () => {
    const result = runPowerShell(String.raw`
      . '${root}\scripts\installed-organizer-tools.ps1'
      $script:order=@();$baseline=[pscustomobject]@{runtimeIdentity='10';controlType='Hwnd';isEnabled=$true;isOffscreen=$false;processId=71;handle=[IntPtr]10}
      function Get-ReadingProcessUiSurfaceSnapshot{param([int]$ApplicationProcessId,[long]$ApplicationProcessStartUtcTicks,[datetime]$Deadline);if($ApplicationProcessStartUtcTicks-ne987654321){throw 'start identity lost'};$script:order+='surface';@($baseline)}
      function Assert-ReadingSurfaceSnapshot{param([object[]]$Snapshot,[int]$ApplicationProcessId,[string]$Kind);$script:order+='surface-gate';$set=[Collections.Generic.HashSet[string]]::new();$null=$set.Add('10');,$set}
      function Get-ReadingPickerTargetSnapshot{param([int]$ApplicationProcessId,[long]$ApplicationProcessStartUtcTicks,[object[]]$Surfaces,[datetime]$Deadline);if($ApplicationProcessStartUtcTicks-ne987654321){throw 'start identity lost'};$script:order+='targets';@()}
      function Assert-ReadingPickerTargetSnapshot{param([object[]]$Snapshot,$SurfaceIdentities,[int]$ApplicationProcessId,[string]$Kind);$script:order+='target-gate'}
      function Invoke-WebDriverScript{param([string]$SessionId,[datetime]$Deadline,[string]$Script);$script:order+='dom-submit';[pscustomobject]@{count=1;clicked=$true}}
      function Wait-ReadingProcessBoundPickerTargets{param([int]$ApplicationProcessId,[long]$ApplicationProcessStartUtcTicks,[object[]]$BaselineSurfaces,[object[]]$BaselineTargets,[datetime]$Deadline);if($ApplicationProcessStartUtcTicks-ne987654321){throw 'start identity lost'};$script:order+='native-bind';[pscustomobject]@{filenameControlCategory='edit-1148'}}
      function Submit-ProcessBoundOpenDialog{param($Binding,[int]$ApplicationProcessId,[long]$ApplicationProcessStartUtcTicks,[string]$Path,[datetime]$Deadline);if($ApplicationProcessStartUtcTicks-ne987654321){throw 'start identity lost'};$script:order+='native-submit';$script:submitted=$Path}
      function Wait-ReadingProcessUiSurfaceClosed{param($Binding,[int]$ApplicationProcessId,[long]$ApplicationProcessStartUtcTicks,[datetime]$Deadline);if($ApplicationProcessStartUtcTicks-ne987654321){throw 'start identity lost'};$script:order+='native-close'}
      function Wait-WebDriverOracle{param([string]$SessionId,[datetime]$Deadline,[string]$Kind,[string]$Script,[scriptblock]$Predicate);$script:order+='product-complete';[pscustomobject]@{}}
      $tempRoot=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\');$testRoot=Join-Path $tempRoot ('smacrobat-organizer-native-'+[Guid]::NewGuid().ToString('N'));[IO.Directory]::CreateDirectory($testRoot)|Out-Null;$target=Join-Path $testRoot 'Split PDFs'
      try{
        $category=Invoke-OrganizerProcessBoundSplit -SessionId fixture -ApplicationProcessId 71 -ApplicationProcessStartUtcTicks 987654321 -TargetPath $target -Deadline ([datetime]::UtcNow.AddSeconds(1))
        if($category-cne'edit-1148'-or$script:submitted-cne$testRoot-or($script:order-join',')-cne'surface,surface-gate,targets,target-gate,dom-submit,native-bind,native-submit,native-close,product-complete'){throw 'Organizer split picker identity sequence drifted.'}
        [IO.Directory]::CreateDirectory($target)|Out-Null;$rejected=$false;try{Invoke-OrganizerProcessBoundSplit -SessionId fixture -ApplicationProcessId 71 -ApplicationProcessStartUtcTicks 987654321 -TargetPath $target -Deadline ([datetime]::UtcNow.AddSeconds(1))|Out-Null}catch{$rejected=$_.Exception.Message-ceq'The organizer split target must be fresh.'};if(-not$rejected){throw 'Existing organizer split target was accepted.'}
      }finally{if([IO.Path]::GetFullPath($testRoot).StartsWith($tempRoot+'\',[StringComparison]::OrdinalIgnoreCase)-and[IO.Directory]::Exists($testRoot)){[IO.Directory]::Delete($testRoot,$true)}}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(installed.indexOf('$baseline = @(Get-ReadingProcessUiSurfaceSnapshot')).toBeLessThan(installed.indexOf("$clicked = Invoke-WebDriverScript -SessionId $SessionId"));
    for (const marker of ['Wait-ReadingProcessBoundPickerTargets', 'Submit-ProcessBoundOpenDialog', 'Wait-ReadingProcessUiSurfaceClosed']) expect(installed).toContain(marker);
  });

  it('independently requires two pages per split file and exact ordered page fingerprints', () => {
    const result = runPowerShell(String.raw`
      . '${root}\scripts\installed-organizer-tools.ps1'
      $hashes=[string[]](1..6|ForEach-Object{$_.ToString('X').PadLeft(64,'0')})
      $page={param([int]$index)[pscustomobject]@{WidthPoints=612.0;HeightPoints=792.0;InkPixels=100;FingerprintSha256=$hashes[$index]}}
      $source=[pscustomobject]@{Pages=6;PageProofs=[object[]](0..5|ForEach-Object{&$page $_})}
      $splits=@{}
      for($file=0;$file-lt3;$file++){$splits['split'+$file]=[pscustomobject]@{Pages=2;PageProofs=[object[]]@((&$page ($file*2)),(&$page ($file*2+1)))}}
      $script:calls=@();$provider={param($path,$maximum)$script:calls+=($path+':'+$maximum);if($path-ceq'source'){return $source};$splits[$path]}
      $proof=Assert-OrganizerSplitPdfProof -PdfiumPath signed.dll -SourcePath source -SplitPaths @('split0','split1','split2') -ProofProvider $provider
      if(($proof.splitOutputPageCounts-join',')-cne'2,2,2'-or($proof.sourcePageFingerprintSha256-join',')-cne($hashes-join',')-or($proof.splitPageFingerprintSha256-join',')-cne($hashes-join',')-or-not$proof.splitPageFingerprintOrderVerified-or($script:calls-join',')-cne'source:6,split0:2,split1:2,split2:2'){throw 'Exact independent split PDF proof was incomplete.'}
      $splits.split1.Pages=1;$rejected=$false;try{Assert-OrganizerSplitPdfProof -PdfiumPath signed.dll -SourcePath source -SplitPaths @('split0','split1','split2') -ProofProvider $provider|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Wrong independent split page count was accepted.'};$splits.split1.Pages=2
      $splits.split2.PageProofs[1].FingerprintSha256=$hashes[0];$rejected=$false;try{Assert-OrganizerSplitPdfProof -PdfiumPath signed.dll -SourcePath source -SplitPaths @('split0','split1','split2') -ProofProvider $provider|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Wrong independent split page order was accepted.'}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(installed).toContain('Signed PDFium did not independently parse exactly two pages');
  });

  it('accepts only the exact three-file bounded split inventory under the fresh parent', () => {
    const result = runPowerShell(String.raw`
      . '${root}\scripts\installed-organizer-tools.ps1'
      $tempRoot=[IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\');$testRoot=Join-Path $tempRoot ('smacrobat-organizer-inventory-'+[Guid]::NewGuid().ToString('N'));$parent=Join-Path $testRoot 'valid';$target=Join-Path $parent 'Split PDFs';$badParent=Join-Path $testRoot 'invalid';$badTarget=Join-Path $badParent 'Split PDFs'
      try{
        [IO.Directory]::CreateDirectory($target)|Out-Null;foreach($name in @('pages-0001-0002.pdf','pages-0003-0004.pdf','pages-0005-0006.pdf')){[IO.File]::WriteAllBytes((Join-Path $target $name),[byte[]](1,2,3))}
        $inventory=Get-OrganizerSplitInventory -SplitParentRoot $parent -TargetPath $target;if($inventory.fileCount-ne3-or$inventory.totalBytes-ne9){throw 'Exact organizer split inventory was not accepted.'}
        [IO.Directory]::CreateDirectory($badTarget)|Out-Null;[IO.File]::WriteAllBytes((Join-Path $badTarget 'pages-0001-0002.pdf'),[byte[]](1));$rejected=$false;try{Get-OrganizerSplitInventory -SplitParentRoot $badParent -TargetPath $badTarget|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Partial organizer split inventory was accepted.'}
        $escape=Join-Path $testRoot 'escape';$rejected=$false;try{Get-OrganizerSplitInventory -SplitParentRoot $parent -TargetPath $escape|Out-Null}catch{$rejected=$true};if(-not$rejected){throw 'Escaped organizer split target was accepted.'}
      }finally{if([IO.Path]::GetFullPath($testRoot).StartsWith($tempRoot+'\',[StringComparison]::OrdinalIgnoreCase)-and[IO.Directory]::Exists($testRoot)){[IO.Directory]::Delete($testRoot,$true)}}
    `);
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('preserves receipts and uses the shared zero-process cleanup gate', () => {
    for (const marker of [
      'Assert-WebDriverReceipt -Receipt $receipt',
      'Assert-TrustedWindowsSignature -Path $ApplicationPath',
      'Assert-ReadingFileReceipt -Path $FixturePath',
      'Assert-ReadingFileReceipt -Path $PdfiumPath',
      'Assert-ReadingOwnedExecutables -Owned $captured',
      'Assert-ReadingProfileScope -ProfileRoot $ProfileRoot',
      'Assert-LaunchCleanupState -Result $result',
      'Wait-LaunchProcessQuiescence -Deadline $processCleanupDeadline',
      "$sessionDeleteOutcome -cne 'verified'",
      '$remaining -ne 0',
      'Post-flow organizer source fixture',
      'Post-flow organizer settings sentinel',
    ]) expect(installed).toContain(marker);
    expect(installed).toContain('$null = Wait-FixedWebDriverPortsFree');
    expect(installed).toContain('-Environment (Get-MinimalWindowsProcessEnvironment)');
    expect(installed).toContain('return $result');
  });
});
