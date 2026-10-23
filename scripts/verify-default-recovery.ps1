[CmdletBinding()]
param(
 [Parameter(Mandatory=$true)][string]$ProjectRoot,[Parameter(Mandatory=$true)][string]$TargetSourceRoot,[Parameter(Mandatory=$true)][string]$DraftProofRoot,
 [Parameter(Mandatory=$true)][string]$AssetRoot,[Parameter(Mandatory=$true)][string]$WorkRoot,[Parameter(Mandatory=$true)][string]$OutputRoot,[Parameter(Mandatory=$true)][string]$WebDriverRoot,
 [Parameter(Mandatory=$true)][string]$WorkflowSourceRevision,[Parameter(Mandatory=$true)][string]$DraftProofWorkflowRevision,[Parameter(Mandatory=$true)][string]$DraftProofRunId,
 [Parameter(Mandatory=$true)][string]$DraftProofWorkflowId,[Parameter(Mandatory=$true)][string]$DraftProofArtifactId,[Parameter(Mandatory=$true)][string]$DraftProofArtifactName,
 [Parameter(Mandatory=$true)][string]$DraftProofArtifactBytes,[Parameter(Mandatory=$true)][string]$DraftProofArtifactDigest,[Parameter(Mandatory=$true)][string]$DraftProofReceiptBytes,
 [Parameter(Mandatory=$true)][string]$DraftProofReceiptSha256,[Parameter(Mandatory=$true)][string]$TargetVersion,[Parameter(Mandatory=$true)][string]$TargetTag,
 [Parameter(Mandatory=$true)][string]$TargetSourceRevision,[Parameter(Mandatory=$true)][string]$TargetReleaseId,[Parameter(Mandatory=$true)][string]$InstallerAssetId,
 [Parameter(Mandatory=$true)][string]$InstallerName,[Parameter(Mandatory=$true)][string]$InstallerBytes,[Parameter(Mandatory=$true)][string]$InstallerSha256,
 [Parameter(Mandatory=$true)][string]$SignatureAssetId,[Parameter(Mandatory=$true)][string]$SignatureBytes,[Parameter(Mandatory=$true)][string]$SignatureSha256,
 [Parameter(Mandatory=$true)][string]$ManifestAssetId,[Parameter(Mandatory=$true)][string]$ManifestBytes,[Parameter(Mandatory=$true)][string]$ManifestSha256,
 [Parameter(Mandatory=$true)][string]$ExpectedPublisher,[scriptblock]$RuntimeProvider
)
$ErrorActionPreference='Stop';Set-StrictMode -Version Latest

function Import-RecoveryFunctions { param([string]$Path,[string[]]$Names)
 $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($Path,[ref]$tokens,[ref]$errors);if($errors.Count){throw 'A trusted recovery helper did not parse.'}
 foreach($name in $Names){$node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true);if(-not$node){throw "Missing trusted helper: $name"};Set-Item -Path ("Function:script:"+$name) -Value $node.Body.GetScriptBlock()}
}
Import-RecoveryFunctions (Join-Path $PSScriptRoot 'verify-default-expanded-page-tools.ps1') @('Assert-DefaultDraftReceipt','Assert-IndependentReceiptParity','Assert-Step1ArtifactMetadata','Assert-InstalledDefaultResources')
Import-RecoveryFunctions (Join-Path $PSScriptRoot 'verify-signed-ocr-upgrade.ps1') @('Assert-ExactProperties','Assert-FileReceipt','Resolve-RunnerPath','Get-FreshInstallFacts','Assert-FreshInstallFacts','Get-ConflictingProcessFacts','Invoke-BoundedSilentInstaller','Get-InstallFacts','Assert-InstallFacts','Assert-ReceiptValue','Assert-ReceiptFileObject','Get-UniqueResource','Convert-RegistryFilePath')
Import-RecoveryFunctions (Join-Path $PSScriptRoot 'verify-default-draft.ps1') @('New-GitHubGetRequest','Assert-GitHubResponseSuccess','Invoke-GitHubJson','ConvertTo-ExactUInt64')
. (Join-Path $PSScriptRoot 'installed-image-page-tools.ps1')

$script:RecoveryPaths=@('src/App.tsx','src/RecoveryOfferDialog.tsx','src/recoveryErrors.ts','src/bridge.ts','src/Organizer.tsx','src-tauri/src/recovery_journal.rs','src-tauri/src/recovery_store.rs','src-tauri/src/recovery_commands.rs','src-tauri/src/service.rs','src-tauri/src/main.rs','src-tauri/tauri.conf.json','src-tauri/resources/welcome.pdf')
function Assert-RecoverySourceBinding {
 foreach($relative in $script:RecoveryPaths){$a=Join-Path $ProjectRoot $relative;$b=Join-Path $TargetSourceRoot $relative;Assert-FileReceipt $b ([uint64](Get-Item $a).Length) (Get-ExactSha256 $a) 'Recovery feature source'}
 $all=(Get-Content (Join-Path $TargetSourceRoot 'src/App.tsx') -Raw -Encoding UTF8)+(Get-Content (Join-Path $TargetSourceRoot 'src/RecoveryOfferDialog.tsx') -Raw -Encoding UTF8)+(Get-Content (Join-Path $TargetSourceRoot 'src-tauri/src/recovery_commands.rs') -Raw -Encoding UTF8)
 foreach($marker in @('Recovered edits are available','Keep recovered edits','Open original','Recovered edits are open. The source PDF was not changed.','keep_recovered_edits','open_original')){if(-not$all.Contains($marker,[StringComparison]::Ordinal)){throw 'Recovery feature marker changed.'}}
 [ordered]@{featureBlobCount=$script:RecoveryPaths.Count;workflowSourceRevision=$WorkflowSourceRevision;targetSourceRevision=$TargetSourceRevision}
}
function Get-RecoveryFileReceipt { param([string]$Path)
 Assert-NoReparseAncestors -Path $Path;$item=Get-Item $Path -Force;if($item.PSIsContainer-or$item.Attributes-band[IO.FileAttributes]::ReparsePoint-or$item.Length-lt1-or$item.Length-gt32MB){throw 'Recovery fixture is unsafe.'}
 [ordered]@{bytes=[uint64]$item.Length;sha256=Get-ExactSha256 $Path}
}
function Initialize-OwnedRecoveryRoot { param([string]$Path)
 [IO.Directory]::CreateDirectory($Path)|Out-Null;Assert-NoReparseAncestors -Path $Path
 $nonce=[guid]::NewGuid().ToString('N');$marker=Join-Path $Path '.owned';$stream=[IO.File]::Open($marker,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
 try{$bytes=[Text.Encoding]::UTF8.GetBytes($nonce);$stream.Write($bytes,0,$bytes.Length);$stream.Flush($true)}finally{$stream.Dispose()}
 Assert-FileReceipt $marker ([uint64]$bytes.Length) (Get-ExactSha256 $marker) 'Recovery ownership marker'
}
function Stop-ExactRecoveryApplication { param([int]$ProcessId,[long]$ProcessStartUtcTicks,[string]$ApplicationPath)
 $p=Get-Process -Id $ProcessId -ErrorAction Stop;if($p.StartTime.ToUniversalTime().Ticks-ne$ProcessStartUtcTicks-or[IO.Path]::GetFullPath($p.Path)-cne[IO.Path]::GetFullPath($ApplicationPath)){throw 'Owned application identity changed.'}
 $p.Kill($true);if(-not$p.WaitForExit(10000)){throw 'Owned application termination timed out.'};$true
}
function Open-RecoveryFixture { param($Context,[string]$Path)
 $appPid=[int]$Context.ApplicationProcessId;$ticks=[long]$Context.ApplicationProcessStartUtcTicks;$deadline=[datetime]$Context.Deadline
 $null=Wait-RecoveryOracle $Context 'return [...document.querySelectorAll("button")].filter(x=>x.textContent.trim()==="Open a file"&&!x.disabled&&x.parentElement?.querySelector("button[aria-label=\"Toggle theme\"]")).length;' {param($v)[int]$v-eq1} 'Recovery toolbar Open control'
 $surfaces=@(Get-ReadingProcessUiSurfaceSnapshot -ApplicationProcessId $appPid -ApplicationProcessStartUtcTicks $ticks -Deadline $deadline);$ids=Assert-ReadingSurfaceSnapshot -Snapshot $surfaces -ApplicationProcessId $appPid -Kind 'Recovery open baseline'
 $targets=@(Get-ReadingPickerTargetSnapshot -Surfaces $surfaces -ApplicationProcessId $appPid -ApplicationProcessStartUtcTicks $ticks -Deadline $deadline);$null=Assert-ReadingPickerTargetSnapshot -Snapshot $targets -SurfaceIdentities $ids -ApplicationProcessId $appPid -Kind 'Recovery picker baseline'
 $clicked=Invoke-WebDriverScript -SessionId $Context.SessionId -Script 'const b=[...document.querySelectorAll("button")].filter(x=>x.textContent.trim()==="Open a file"&&!x.disabled&&x.parentElement?.querySelector("button[aria-label=\"Toggle theme\"]"));if(b.length===1)setTimeout(()=>b[0].click(),0);return b.length===1;' -Deadline $deadline
 if($clicked-isnot[bool]-or-not$clicked){throw 'Exact Open control was unavailable.'};$picker=Wait-ReadingProcessBoundPickerTargets -ApplicationProcessId $appPid -ApplicationProcessStartUtcTicks $ticks -BaselineSurfaces $surfaces -BaselineTargets $targets -Deadline $deadline
 Submit-ProcessBoundOpenDialog -Binding $picker -ApplicationProcessId $appPid -ApplicationProcessStartUtcTicks $ticks -Path $Path -Deadline $deadline
}
function Wait-RecoveryOracle { param($Context,[string]$Script,[scriptblock]$Predicate,[string]$Kind)
 Wait-WebDriverOracle -SessionId $Context.SessionId -Script $Script -Deadline $Context.Deadline -Kind $Kind -Predicate $Predicate
}
function Invoke-RecoveryPhaseA { param($Context,[string]$Fixture,[string]$Application)
 Open-RecoveryFixture $Context $Fixture
 $before=Wait-RecoveryOracle $Context 'const p=document.querySelector("input[aria-label=\"Page number\"]"),i=document.querySelector("img[alt=\"Page 1\"]");return {clean:document.body.innerText.includes("Source preserved"),page:p?.value||"",w:i?.naturalWidth||0,h:i?.naturalHeight||0};' {param($v)[bool]$v.clean-and[int]$v.w-gt0-and[int]$v.h-gt0} 'Recovery A open'
 $null=Invoke-WebDriverScript -SessionId $Context.SessionId -Deadline $Context.Deadline -Script 'const p=document.querySelector("input[aria-label=\"Page number\"]");p.value="2";p.dispatchEvent(new Event("blur",{bubbles:true}));[...document.querySelectorAll("button")].find(x=>x.textContent.trim()==="Menu").click();[...document.querySelectorAll("button")].find(x=>x.textContent.trim()==="Organize pages").click();return true;'
 $null=Wait-RecoveryOracle $Context 'return {ready:!!document.querySelector("section[aria-label=\"Organize pages workspace\"]")};' {param($v)[bool]$v.ready} 'Recovery A organizer'
 $null=Invoke-WebDriverScript -SessionId $Context.SessionId -Deadline $Context.Deadline -Script 'document.querySelector("button[aria-label=\"Select page 1\"]").click();document.querySelector("button[aria-label=\"Rotate selected pages clockwise\"]").click();return true;'
 $null=Wait-RecoveryOracle $Context 'const p=document.querySelector("input[aria-label=\"Page number\"]"),u=document.querySelector("button[aria-label=\"Undo\"]");return {dirty:document.body.innerText.includes("Unsaved changes"),page:p?.value||"",undo:!!u&&!u.disabled,dialogs:document.querySelectorAll("dialog").length,alerts:document.querySelectorAll("[role=alert]").length};' {param($v)[bool]$v.dirty-and[string]$v.page-ceq'2'-and[bool]$v.undo-and[int]$v.dialogs-eq0-and[int]$v.alerts-eq0} 'Recovery A acknowledged edit'
 $killed=Stop-ExactRecoveryApplication -ProcessId $Context.ApplicationProcessId -ProcessStartUtcTicks $Context.ApplicationProcessStartUtcTicks -ApplicationPath $Application
 [pscustomobject]@{editAcknowledged=$true;currentPage=2;revision=1;originalWidth=[int]$before.w;originalHeight=[int]$before.h;abruptTermination=$killed;closeActionInvoked=$false}
}
function Invoke-RecoveryPhaseB { param($Context,[string]$Fixture,$PhaseA)
 Open-RecoveryFixture $Context $Fixture
 $null=Wait-RecoveryOracle $Context 'const d=document.querySelector("dialog[aria-labelledby=\"recovery-offer-title\"]"),k=d?[...d.querySelectorAll("button")].filter(x=>x.textContent.trim()==="Keep recovered edits"&&!x.disabled):[],o=d?[...d.querySelectorAll("button")].filter(x=>x.textContent.trim()==="Open original"&&!x.disabled):[];return {count:document.querySelectorAll("dialog[aria-labelledby=\"recovery-offer-title\"]").length,heading:d?.querySelector("h2")?.textContent||"",text:d?.innerText||"",keep:k.length,original:o.length,dirty:document.body.innerText.includes("Unsaved changes")};' {param($v)[int]$v.count-eq1-and[string]$v.heading-ceq'Recovered edits are available'-and[string]$v.text-like'*unsaved revision 1*'-and[int]$v.keep-eq1-and[int]$v.original-eq1-and-not[bool]$v.dirty} 'Recovery B offer'
 $null=Invoke-WebDriverScript -SessionId $Context.SessionId -Deadline $Context.Deadline -Script 'const d=document.querySelector("dialog[aria-labelledby=\"recovery-offer-title\"]");[...d.querySelectorAll("button")].find(x=>x.textContent.trim()==="Keep recovered edits").click();return true;'
 $kept=Wait-RecoveryOracle $Context 'const p=document.querySelector("input[aria-label=\"Page number\"]"),u=document.querySelector("button[aria-label=\"Undo\"]"),i=document.querySelector("img[alt=\"Page 1\"]");return {notice:document.body.innerText.includes("Recovered edits are open. The source PDF was not changed."),dirty:document.body.innerText.includes("Unsaved changes"),page:p?.value||"",undo:!!u&&!u.disabled,w:i?.naturalWidth||0,h:i?.naturalHeight||0};' {param($v)[bool]$v.notice-and[bool]$v.dirty-and[string]$v.page-ceq'2'-and[bool]$v.undo-and[int]$v.w-eq[int]$PhaseA.originalHeight-and[int]$v.h-eq[int]$PhaseA.originalWidth} 'Recovery B rotated render'
 $null=Invoke-WebDriverScript -SessionId $Context.SessionId -Deadline $Context.Deadline -Script 'document.querySelector("button[aria-label=\"Undo\"]").click();return true;'
 $null=Wait-RecoveryOracle $Context 'const u=document.querySelector("button[aria-label=\"Undo\"]");return {clean:document.body.innerText.includes("Source preserved"),undo:!!u&&u.disabled,offer:document.querySelectorAll("dialog[aria-labelledby=\"recovery-offer-title\"]").length};' {param($v)[bool]$v.clean-and[bool]$v.undo-and[int]$v.offer-eq0} 'Recovery B tombstone'
 $closed=Invoke-WebDriverScript -SessionId $Context.SessionId -Deadline $Context.Deadline -Script 'const b=document.querySelector("button[aria-label=\"Close controlled-source.pdf\"]");if(b)b.click();return !!b;';if($closed-isnot[bool]-or-not$closed){throw 'Recovery B clean tab did not close.'}
 [pscustomobject]@{offerRevision=1;keepChosen=$true;recoveredCurrentPage=2;recoveredDirty=$true;renderedRotationVerified=$true;undoToClean=$true;cleanTabClosed=$true}
}
function Invoke-RecoveryPhaseC { param($Context,[string]$Fixture)
 Open-RecoveryFixture $Context $Fixture
 $null=Wait-RecoveryOracle $Context 'const u=document.querySelector("button[aria-label=\"Undo\"]"),i=document.querySelector("img[alt=\"Page 1\"]");return {ready:!!i&&i.complete&&i.naturalWidth>0,offer:document.querySelectorAll("dialog[aria-labelledby=\"recovery-offer-title\"]").length,clean:document.body.innerText.includes("Source preserved"),undo:!!u&&u.disabled,notice:document.body.innerText.includes("Recovered edits are open.")};' {param($v)[bool]$v.ready-and[int]$v.offer-eq0-and[bool]$v.clean-and[bool]$v.undo-and-not[bool]$v.notice} 'Recovery C clean reopen'
 $closed=Invoke-WebDriverScript -SessionId $Context.SessionId -Deadline $Context.Deadline -Script 'const b=document.querySelector("button[aria-label=\"Close controlled-source.pdf\"]");if(b)b.click();return !!b;';if($closed-isnot[bool]-or-not$closed){throw 'Recovery C clean tab did not close.'}
 [pscustomobject]@{staleOfferAbsent=$true;cleanOriginal=$true;cleanTabClosed=$true}
}
function Invoke-InstalledRecoveryRuntime { param([string]$Application,[string]$TauriDriver,[string]$EdgeDriver,[string]$Settings,[string]$Work,[string]$Fixture,[string]$EdgeVersion)
 $a=Invoke-FreshInstalledImagePageToolsSession -ApplicationPath $Application -TauriDriverPath $TauriDriver -EdgeDriverPath $EdgeDriver -ProfileRoot (Join-Path $Work 'profile-a') -SettingsRoot $Settings -ExpectedEdgeDriverVersion $EdgeVersion -SessionCallback {param($c)Invoke-RecoveryPhaseA $c $Fixture $Application}
 $b=Invoke-FreshInstalledImagePageToolsSession -ApplicationPath $Application -TauriDriverPath $TauriDriver -EdgeDriverPath $EdgeDriver -ProfileRoot (Join-Path $Work 'profile-b') -SettingsRoot $Settings -ExpectedEdgeDriverVersion $EdgeVersion -SessionCallback {param($c)Invoke-RecoveryPhaseB $c $Fixture $a}
 $c=Invoke-FreshInstalledImagePageToolsSession -ApplicationPath $Application -TauriDriverPath $TauriDriver -EdgeDriverPath $EdgeDriver -ProfileRoot (Join-Path $Work 'profile-c') -SettingsRoot $Settings -ExpectedEdgeDriverVersion $EdgeVersion -SessionCallback {param($x)Invoke-RecoveryPhaseC $x $Fixture}
 [pscustomobject]@{a=$a;b=$b;c=$c;profilesRetained=$true}
}
function Assert-RecoveryRuntimeReceipt { param($v)
 if(-not$v.a.editAcknowledged-or$v.a.revision-ne1-or$v.a.currentPage-ne2-or-not$v.a.abruptTermination-or$v.a.closeActionInvoked-or$v.b.offerRevision-ne1-or-not$v.b.keepChosen-or$v.b.recoveredCurrentPage-ne2-or-not$v.b.renderedRotationVerified-or-not$v.b.undoToClean-or-not$v.b.cleanTabClosed-or-not$v.c.staleOfferAbsent-or-not$v.c.cleanOriginal-or-not$v.c.cleanTabClosed-or-not$v.profilesRetained){throw 'Recovery runtime receipt is incomplete.'}
}

if($TargetVersion-cnotmatch'^\d+\.\d+\.\d+$'-or$TargetTag-cne('v'+$TargetVersion)){throw 'Target identity is malformed.'}
$token=$env:GITHUB_TOKEN;if([string]::IsNullOrWhiteSpace($token)){throw 'GitHub token is unavailable.'};Assert-Step1ArtifactMetadata $token
$proof=Resolve-RunnerPath $DraftProofRoot $env:RUNNER_TEMP -MustExist;$assets=Resolve-RunnerPath $AssetRoot $env:RUNNER_TEMP -MustBeFresh;$work=Resolve-RunnerPath $WorkRoot $env:RUNNER_TEMP -MustBeFresh;$output=Resolve-RunnerPath $OutputRoot $env:RUNNER_TEMP -MustBeFresh;$drivers=Resolve-RunnerPath $WebDriverRoot $env:RUNNER_TEMP -MustExist
Initialize-OwnedRecoveryRoot $work
$proofFiles=@(Get-ChildItem $proof -Recurse -File -Force);if($proofFiles.Count-ne1){throw 'Step 1 artifact shape changed.'};[uint64]$proofBytes=0;if(-not[uint64]::TryParse($DraftProofReceiptBytes,[ref]$proofBytes)){throw 'Step 1 bytes are invalid.'};Assert-FileReceipt $proofFiles[0].FullName $proofBytes $DraftProofReceiptSha256 'Step 1 receipt';$receipt=Get-Content $proofFiles[0].FullName -Raw -Encoding UTF8|ConvertFrom-Json;$sets=Assert-DefaultDraftReceipt $receipt
$sourceBinding=Assert-RecoverySourceBinding
& (Join-Path $PSScriptRoot 'verify-default-draft.ps1') -ProjectRoot $ProjectRoot -TargetSourceRoot $TargetSourceRoot -AssetRoot $assets -WorkRoot (Join-Path $work 'draft-work') -OutputRoot (Join-Path $work 'draft-output') -WorkflowSourceRevision $WorkflowSourceRevision -TargetVersion $TargetVersion -TargetTag $TargetTag -TargetSourceRevision $TargetSourceRevision -TargetReleaseId $TargetReleaseId -InstallerAssetId $InstallerAssetId -InstallerName $InstallerName -InstallerBytes $InstallerBytes -InstallerSha256 $InstallerSha256 -SignatureAssetId $SignatureAssetId -SignatureBytes $SignatureBytes -SignatureSha256 $SignatureSha256 -ManifestAssetId $ManifestAssetId -ManifestBytes $ManifestBytes -ManifestSha256 $ManifestSha256 -ExpectedPublisher $ExpectedPublisher
$independent=Get-Content (Join-Path $work 'draft-output/default-draft-verification.json') -Raw -Encoding UTF8|ConvertFrom-Json;Assert-IndependentReceiptParity $receipt $independent
$env:GITHUB_TOKEN=$null;$env:GH_TOKEN=$null;$token=$null
$installer=Join-Path $assets $InstallerName;$installRoot=[IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'PDF Workstation')).TrimEnd('\');$settings=[IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\');$registry='Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDF Workstation'
$fresh=Get-FreshInstallFacts $installRoot $settings $registry @($installer);Assert-FreshInstallFacts $fresh;$baseline=Get-ConflictingProcessFacts @($installer);if($baseline.applicationProcessPresent-or$baseline.installerProcessPresent){throw 'Recovery baseline is not fresh.'}
Invoke-BoundedSilentInstaller $installer;$facts=Get-InstallFacts $registry $installRoot;Assert-InstallFacts $facts $TargetVersion $installRoot $receipt.packagedApplication 'Valid' $ExpectedPublisher $true;$resources=Assert-InstalledDefaultResources $installRoot $receipt $sets
[IO.Directory]::CreateDirectory($work)|Out-Null;[IO.Directory]::CreateDirectory($settings)|Out-Null;$fixture=Join-Path $work 'controlled-source.pdf';Copy-Item $resources.Welcome.FullName $fixture;$fixtureReceipt=Get-RecoveryFileReceipt $fixture;$driverReceipt=Get-Content (Join-Path $drivers 'webdriver-receipt.json') -Raw|ConvertFrom-Json
$runtime=if($RuntimeProvider){&$RuntimeProvider $resources $drivers $settings $work $fixture}else{Invoke-InstalledRecoveryRuntime $resources.Application.FullName (Join-Path $drivers 'tauri-driver-install/bin/tauri-driver.exe') (Join-Path $drivers 'edge-driver/msedgedriver.exe') $settings $work $fixture ([string]$driverReceipt.edgeDriver.version)}
Assert-RecoveryRuntimeReceipt $runtime;Assert-FileReceipt $fixture $fixtureReceipt.bytes $fixtureReceipt.sha256 'Preserved recovery source'
[IO.Directory]::CreateDirectory($output)|Out-Null;$record=[ordered]@{schemaVersion=1;scope='Controlled installed recovery reconstruction after exact owned-process termination; no arbitrary-PDF, power-loss, parent-directory-sync, or rollback-attack claim.';workflowSourceRevision=$WorkflowSourceRevision;targetSourceRevision=$TargetSourceRevision;targetVersion=$TargetVersion;sourceBinding=$sourceBinding;fixture=$fixtureReceipt;a=[ordered]@{editAcknowledged=$true;currentPage=2;abruptTermination=$true;sourcePreserved=$true};b=[ordered]@{offerRevision=1;keepChosen=$true;recoveredCurrentPage=2;recoveredDirty=$true;renderedRotationVerified=$true;undoToClean=$true;sourcePreserved=$true};c=[ordered]@{staleOfferAbsent=$true;cleanOriginal=$true;sourcePreserved=$true};retained=[ordered]@{profiles=$true;settings=$true;fixtures=$true;results=$true};limits=[ordered]@{controlledFixture=$true;powerLoss=$false;parentDirectorySync=$false;rollbackAttackResistance=$false;arbitraryPdf=$false}}
$json=$record|ConvertTo-Json -Depth 12;if([Text.Encoding]::UTF8.GetByteCount($json)-gt65536-or$json-match'(?i)([A-Z]:\\|\\Users\\|runneradmin|RUNNER_TEMP|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|\.smacrec|raw\.log)'){throw 'Recovery receipt is unsafe.'};[IO.File]::WriteAllText((Join-Path $output 'default-recovery-verification.json'),$json,[Text.UTF8Encoding]::new($false))
