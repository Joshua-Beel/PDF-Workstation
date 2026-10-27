[CmdletBinding()]
param(
 [Parameter(Mandatory=$true)][string]$ProjectRoot,[Parameter(Mandatory=$true)][string]$TargetSourceRoot,[Parameter(Mandatory=$true)][string]$DraftProofRoot,[string]$AssetRoot,[Parameter(Mandatory=$true)][string]$WorkRoot,[Parameter(Mandatory=$true)][string]$OutputRoot,[Parameter(Mandatory=$true)][string]$WebDriverRoot,
 [Parameter(Mandatory=$true)][string]$WorkflowSourceRevision,[string]$DraftProofWorkflowRevision,[string]$DraftProofRunId,[string]$DraftProofWorkflowId,[string]$DraftProofArtifactId,[string]$DraftProofArtifactName,[string]$DraftProofArtifactBytes,[string]$DraftProofArtifactDigest,[string]$DraftProofReceiptBytes,[string]$DraftProofReceiptSha256,
 [Parameter(Mandatory=$true)][string]$TargetVersion,[string]$TargetTag,[Parameter(Mandatory=$true)][string]$TargetSourceRevision,[string]$TargetReleaseId,[string]$InstallerAssetId,[Parameter(Mandatory=$true)][string]$InstallerName,[Parameter(Mandatory=$true)][string]$InstallerBytes,[Parameter(Mandatory=$true)][string]$InstallerSha256,[string]$SignatureAssetId,[string]$SignatureBytes,[string]$SignatureSha256,[string]$ManifestAssetId,[string]$ManifestBytes,[string]$ManifestSha256,[Parameter(Mandatory=$true)][string]$ExpectedPublisher,
 [scriptblock]$RuntimeProvider,[ValidateSet('draft','artifact')][string]$ProofSource='draft',[string]$ArtifactRoot,[string]$ArtifactRunId,[string]$ArtifactId,[string]$ArtifactName,[string]$ArtifactBytes,[string]$ArtifactDigest
)
$ErrorActionPreference='Stop';Set-StrictMode -Version Latest
. (Join-Path $PSScriptRoot 'installed-image-page-tools.ps1')
. (Join-Path $PSScriptRoot 'ocr/installer-package.ps1')
. (Join-Path $PSScriptRoot 'ocr/windows-signing.ps1')

function Import-CommentsFormsFunctions { param([string]$Path,[string[]]$Names)
 $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile($Path,[ref]$tokens,[ref]$errors);if($errors.Count){throw 'A trusted comments/forms helper did not parse.'}
 foreach($name in $Names){$node=$ast.Find({param($value)$value-is[Management.Automation.Language.FunctionDefinitionAst]-and$value.Name-ceq$name},$true);if(-not$node){throw "Trusted comments/forms helper is missing: $name"};Set-Item -Path ("Function:script:"+$name) -Value $node.Body.GetScriptBlock()}
}
Import-CommentsFormsFunctions (Join-Path $PSScriptRoot 'verify-signed-ocr-upgrade.ps1') @('Assert-ExactProperties','Assert-FileReceipt','Resolve-RunnerPath','Assert-FreshInstallFacts','Get-FreshInstallFacts','Get-ConflictingProcessFacts','Invoke-BoundedSilentInstaller','Get-InstallFacts','Assert-InstallFacts','Assert-ReceiptValue','Assert-ReceiptFileObject','Get-UniqueResource','Convert-RegistryFilePath')
Import-CommentsFormsFunctions (Join-Path $PSScriptRoot 'verify-default-expanded-page-tools.ps1') @('Assert-DefaultDraftReceipt','Assert-InstalledDefaultResources','Invoke-OwnedCleanup','Invoke-SilentUninstall','Wait-OwnedUninstallComplete','Get-OwnedStateRemaining','ConvertTo-OwnedRelativeName','Get-OwnedInstallEntries','Save-OwnedInstallSnapshot','Write-OwnedInstallLeftoverDiagnostics','Get-OwnProcessModulesUnder','Clear-OwnProcessModules','Assert-EmptyProcessFacts','Assert-OwnedCleanupState','Complete-PrimaryAndCleanup','Assert-ProofSourceParameters','Assert-DefaultArtifactReceipt','Read-DefaultArtifactRehearsal')
Import-CommentsFormsFunctions (Join-Path $PSScriptRoot 'verify-default-draft.ps1') @('ConvertTo-ExactUInt64')

$script:CommentsFormsPaths=@('src/App.tsx','src/bridge.ts','src/CommentLayer.tsx','src/CommentEditor.tsx','src/CommentsPanel.tsx','src/FillFormsDialog.tsx','src-tauri/src/comments.rs','src-tauri/src/forms.rs','src-tauri/src/service.rs','src-tauri/src/main.rs','src-tauri/tests/fixtures/reportlab-mixed-fields.pdf','src-tauri/tests/fixtures/reportlab-mixed-fields.py','src-tauri/tests/fixtures/reportlab-radio-fields.pdf','src-tauri/tests/fixtures/reportlab-radio-fields.py','src-tauri/tests/fixtures/reportlab-choice-fields.pdf','src-tauri/tests/fixtures/reportlab-choice-fields.py','src-tauri/tauri.conf.json')

function Assert-CommentsFormsSourceParity {
 foreach($path in $script:CommentsFormsPaths){$target=(& git -C $ProjectRoot rev-parse "$TargetSourceRevision`:$path").Trim();$current=(& git -C $ProjectRoot rev-parse "HEAD`:$path").Trim();if($LASTEXITCODE-ne0-or$target-cne$current){throw "Installed comments/forms source parity failed: $path"}}
 $app=(& git -C $ProjectRoot show "$TargetSourceRevision`:src/App.tsx")-join"`n";$markers=@('createComment','createHighlight','documentAnnotations','saveCopy','documentFormFields','fillFormCopy','Fill forms','Add area highlight');foreach($marker in $markers){if(-not$app.Contains($marker,[StringComparison]::Ordinal)){throw 'Target application lacks an exact comments/forms selector marker.'}}
 return [ordered]@{exactFeatureBlobsMatched=$true;featureBlobCount=17;selectorMarkerCount=8}
}

function Get-CommentsFormsReceipt { param([string]$Path)
 $item=Get-Item -LiteralPath $Path -Force;if($item.Attributes-band[IO.FileAttributes]::ReparsePoint-or$item.Length-lt1){throw 'A comments/forms file receipt is unsafe.'};return [pscustomobject][ordered]@{bytes=[uint64]$item.Length;sha256=Get-ExactSha256 -Path $Path}
}

function Invoke-CommentsFormsPoll { param([string]$SessionId,[datetime]$Deadline,[string]$Script,[string[]]$Allowed,[string]$Kind)
 $missing=@('not-observed')
 while([datetime]::UtcNow-lt$Deadline){
  $value=Invoke-WebDriverScript -SessionId $SessionId -Deadline $Deadline -Script $Script
  if($null-eq$value-or$null-eq$value.PSObject.Properties['done']-or$null-eq$value.PSObject.Properties['missing']-or$value.done-isnot[bool]){throw "$Kind probe returned an unexpected shape."}
  $observed=@($value.missing|ForEach-Object{[string]$_});foreach($item in $observed){if($Allowed-cnotcontains$item){throw "$Kind probe returned an unexpected shape."}}
  if($value.done){if($observed.Count-ne0){throw "$Kind probe returned an unexpected shape."};return $value}
  if($observed.Count-lt1){throw "$Kind probe returned an unexpected shape."};$missing=$observed;Start-Sleep -Milliseconds 200
 }
 throw "$Kind did not become ready before the session deadline. Missing: $($missing-join', ')."
}

function Get-CommentsFormsSaveControlScript { param([ValidateSet('Save a copy','Save filled copy')][string]$ButtonLabel,[ValidateSet('aria-label','text')][string]$ButtonMatch)
 $test=if($ButtonMatch-ceq'aria-label'){"x.getAttribute('aria-label')===n"}else{'x.textContent.trim()===n'}
 return ("const n=__LABEL__,a=[...document.querySelectorAll('button')].filter(x=>__TEST__);if(a.length!==1)return {done:false,missing:['save-control']};if(a[0].disabled)return {done:false,missing:['enabled-save-control']};a[0].click();return {done:true,missing:[]};").Replace('__LABEL__',($ButtonLabel|ConvertTo-Json -Compress)).Replace('__TEST__',$test)
}

function Invoke-CommentsFormsNativeSave { param([string]$SessionId,[int]$ProcessId,[long]$ProcessStartUtcTicks,[string]$OutputPath,[string]$ButtonLabel,[ValidateSet('aria-label','text')][string]$ButtonMatch,[datetime]$Deadline)
 if(Test-Path -LiteralPath $OutputPath){throw 'A comments/forms output path must be fresh.'};Assert-NoReparseAncestors -Path $OutputPath
 $baseline=@(Get-ProcessNativeWindowSnapshot -ProcessId $ProcessId -Deadline $Deadline -TopLevelOnly);if($baseline.Count-lt1){throw 'Comments/forms native Save baseline was empty.'}
 $null=Invoke-CommentsFormsPoll -SessionId $SessionId -Deadline $Deadline -Script (Get-CommentsFormsSaveControlScript $ButtonLabel $ButtonMatch) -Allowed @('save-control','enabled-save-control') -Kind 'The exact comments/forms Save control'
 $binding=Wait-NewProcessNativeWindowSurface -ProcessId $ProcessId -Baseline $baseline -AnchorNames @('Save') -AnchorControlTypes @('ControlType.Button') -Stage 'save-output-dialog' -Deadline $Deadline
 $roles=Get-ValidatedBindingNativeRoles -Binding $binding;if($null-eq$roles-or$null-eq$roles.PSObject.Properties['save']-or$null-eq$roles.PSObject.Properties['filenameEdit']){throw 'Comments/forms Save dialog lacked exact native roles.'}
 $remaining=[int][Math]::Min(120000,[Math]::Max(1,[Math]::Ceiling(($Deadline-[datetime]::UtcNow).TotalMilliseconds)));$tick=[Environment]::TickCount64+$remaining
 Set-BoundNativeSaveFileNameExact -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Binding $binding -Value $OutputPath -DeadlineTickCount $tick -AwaitSuggestion -Transport keyboard
 Invoke-BoundNativeSaveButtonExact -ProcessId $ProcessId -ProcessStartUtcTicks $ProcessStartUtcTicks -Binding $binding -DeadlineTickCount $tick -ExpectedFileName $OutputPath
 Wait-BoundProcessUiSurfaceClosed -ProcessId $ProcessId -Binding $binding -Stage 'save-output-dialog' -Deadline $Deadline -ProcessStartUtcTicks $ProcessStartUtcTicks -ExpectedNativeSaveFileName $OutputPath -ActionTransport 'native-bm-click-delivered'
 $null=Wait-ImagePageStableFile -Path $OutputPath -Deadline $Deadline;return Get-CommentsFormsReceipt $OutputPath
}

function Open-CommentsFormsFile { param($Context,[string]$Path)
 $null=Wait-WebDriverOracle -SessionId $Context.SessionId -Deadline $Context.Deadline -Kind 'comments/forms home' -Script (Get-ReadingHomeScript) -Predicate {param($v)[bool]$v.ready-and[string]$v.title-ceq'PDF Workstation'}
 return Open-ReadingUserFile -SessionId $Context.SessionId -ApplicationProcessId $Context.ApplicationProcessId -ApplicationProcessStartUtcTicks $Context.ApplicationProcessStartUtcTicks -Path $Path -Deadline $Context.Deadline
}

function Get-CommentsPageOneScript { return "const p=document.querySelector('img[alt=`"Page 1`"]'),l=p?.parentElement?.querySelector(':scope>[data-testid=`"comment-layer`"]'),m=[];if(!p)m.push('page-image');if(!l)m.push('comment-layer');" }

function Get-CommentsCreationControlScript { param([ValidateSet('Add comment','Add area highlight')][string]$Label)
 return (Get-CommentsPageOneScript)+("const b=[...document.querySelectorAll('button')].filter(x=>x.getAttribute('aria-label')===__LABEL__&&!x.disabled);if(b.length!==1)m.push('enabled-button');if(m.length)return {done:false,missing:m};b[0].click();return {done:true,missing:[]};").Replace('__LABEL__',($Label|ConvertTo-Json -Compress))
}

function Get-CommentsPlacementScript { param([ValidateSet('Comment text','Highlight description')][string]$Editor)
 $place=if($Editor-ceq'Comment text'){"l.dispatchEvent(new PointerEvent('pointerdown',{bubbles:true,button:0,buttons:1,clientX:r.left+r.width*.25,clientY:r.top+r.height*.25,pointerId:11,pointerType:'mouse'}));"}else{"const e=(t,x,y)=>l.dispatchEvent(new PointerEvent(t,{bubbles:true,button:0,buttons:t==='pointerup'?0:1,clientX:x,clientY:y,pointerId:12,pointerType:'mouse'}));e('pointerdown',r.left+r.width*.55,r.top+r.height*.55);e('pointermove',r.left+r.width*.75,r.top+r.height*.7);e('pointerup',r.left+r.width*.75,r.top+r.height*.7);"}
 return ("if(document.querySelectorAll('textarea[aria-label=__EDITOR__]').length===1)return {done:true,missing:[]};").Replace('__EDITOR__',($Editor|ConvertTo-Json -Compress))+(Get-CommentsPageOneScript)+"if(m.length)return {done:false,missing:m};const r=p.getBoundingClientRect();$place return {done:false,missing:['editor']};"
}

function Get-CommentsEditorSaveScript { param([ValidateSet('Comment text','Highlight description')][string]$Editor,[ValidateSet('Installed sticky receipt','Installed area receipt')][string]$Text,[ValidateSet('Save comment','Save area highlight')][string]$Button)
 return ("const t=[...document.querySelectorAll('textarea')].filter(x=>x.getAttribute('aria-label')===__EDITOR__),v=__TEXT__;if(t.length!==1)return {done:false,missing:['editor']};if(t[0].value!==v){Object.getOwnPropertyDescriptor(HTMLTextAreaElement.prototype,'value').set.call(t[0],v);t[0].dispatchEvent(new Event('input',{bubbles:true}));return {done:false,missing:['typed-text']};}const b=[...document.querySelectorAll('button')].filter(x=>x.textContent.trim()===__BUTTON__);if(b.length!==1||b[0].disabled)return {done:false,missing:['enabled-save-button']};b[0].click();return {done:true,missing:[]};").Replace('__EDITOR__',($Editor|ConvertTo-Json -Compress)).Replace('__TEXT__',($Text|ConvertTo-Json -Compress)).Replace('__BUTTON__',($Button|ConvertTo-Json -Compress))
}

function Invoke-CommentsCreateAnnotation { param($Context,[ValidateSet('note','area')][string]$Type)
 if($Type-ceq'note'){$label='Add comment';$editor='Comment text';$text='Installed sticky receipt';$button='Save comment';$kind='sticky-comment'}else{$label='Add area highlight';$editor='Highlight description';$text='Installed area receipt';$button='Save area highlight';$kind='area-highlight'}
 $null=Invoke-CommentsFormsPoll -SessionId $Context.SessionId -Deadline $Context.Deadline -Script (Get-CommentsCreationControlScript $label) -Allowed @('page-image','comment-layer','enabled-button') -Kind "Exact $kind creation controls"
 $null=Invoke-CommentsFormsPoll -SessionId $Context.SessionId -Deadline $Context.Deadline -Script (Get-CommentsPlacementScript $editor) -Allowed @('page-image','comment-layer','editor') -Kind "Exact $kind placement"
 $null=Invoke-CommentsFormsPoll -SessionId $Context.SessionId -Deadline $Context.Deadline -Script (Get-CommentsEditorSaveScript $editor $text $button) -Allowed @('editor','typed-text','enabled-save-button') -Kind "Exact $kind editor"
 $textJson=$text|ConvertTo-Json -Compress
 $null=Wait-WebDriverOracle -SessionId $Context.SessionId -Deadline $Context.Deadline -Kind "$kind persisted" -Script "return [...document.querySelectorAll('aside[aria-label=`"Comments`"] span')].some(x=>x.textContent.trim()===$textJson);" -Predicate {param($v)$v-is[bool]-and$v}
}

function Invoke-CommentsCreateSession { param($Context,[string]$SourcePath,[string]$OutputPath)
 $category=Open-CommentsFormsFile $Context $SourcePath
 Invoke-CommentsCreateAnnotation $Context 'note'
 Invoke-CommentsCreateAnnotation $Context 'area'
 $receipt=Invoke-CommentsFormsNativeSave -SessionId $Context.SessionId -ProcessId $Context.ApplicationProcessId -ProcessStartUtcTicks $Context.ApplicationProcessStartUtcTicks -OutputPath $OutputPath -ButtonLabel 'Save a copy' -ButtonMatch 'aria-label' -Deadline $Context.Deadline
 return [pscustomobject][ordered]@{nativeFilenameControlCategory=$category;output=$receipt;stickyCreated=$true;areaHighlightCreated=$true}
}

function Get-CommentsReopenScript {
 return "const a=document.querySelector('aside[aria-label=`"Comments`"]');if(!a){const b=[...document.querySelectorAll('button')].filter(x=>x.getAttribute('aria-label')==='Comments'&&!x.disabled);if(b.length===1)b[0].click();return {sticky:0,area:0};}const rows=[...a.querySelectorAll('button')];return {sticky:rows.filter(x=>x.textContent.includes('Installed sticky receipt')&&x.textContent.includes('Comment · Page 1')).length,area:rows.filter(x=>x.textContent.includes('Installed area receipt')&&x.textContent.includes('Highlight · Page 1')).length};"
}

function Invoke-CommentsReopenSession { param($Context,[string]$Path)
 $null=Open-CommentsFormsFile $Context $Path;$state=Wait-WebDriverOracle -SessionId $Context.SessionId -Deadline $Context.Deadline -Kind 'reopened comments' -Script (Get-CommentsReopenScript) -Predicate {param($v)[int]$v.sticky-eq1-and[int]$v.area-eq1}
 return [pscustomobject][ordered]@{stickyCount=[int]$state.sticky;areaHighlightCount=[int]$state.area;page=1;pixelAppearanceVerified=$false;nativePdfSemanticVerified=$false;selectedTextHighlightVerified=$false}
}

function Get-FormSpec { param([ValidateSet('mixed','radio','choice')][string]$Kind)
 $specs=@{
 mixed=[ordered]@{controls=@('Name','Consent, page 1');read="c[0].value+'|'+c[1].checked";write="if(c[0].value!=='Installed Mixed'){Object.getOwnPropertyDescriptor(HTMLInputElement.prototype,'value').set.call(c[0],'Installed Mixed');c[0].dispatchEvent(new Event('input',{bubbles:true}));}if(!c[1].checked)c[1].click();";source='Original|false';expected='Installed Mixed|true'}
 radio=[ordered]@{controls=@('Preference: Choice A, page 1');read="[...d.querySelectorAll('input[type=radio]')].filter(x=>x.checked).map(x=>x.getAttribute('aria-label')).join('|')";write='if(!c[0].checked)c[0].click();';source='Preference: Choice B, page 1';expected='Preference: Choice A, page 1'}
 choice=[ordered]@{controls=@('Destination, page 1','Delivery, page 1');read="c.map(x=>x.selectedIndex<0?'':x.options[x.selectedIndex].textContent).join('|')";write="[['North Hub',0],['Print copy',1]].forEach(([n,i])=>{const o=[...c[i].options].filter(x=>x.textContent===n);if(o.length===1&&c[i].value!==o[0].value){Object.getOwnPropertyDescriptor(HTMLSelectElement.prototype,'value').set.call(c[i],o[0].value);c[i].dispatchEvent(new Event('change',{bubbles:true}));}});";source='South Hub|Email copy';expected='North Hub|Print copy'}
 }
 return $specs[$Kind]
}

function Get-FormSessionScript { param([ValidateSet('mixed','radio','choice')][string]$Kind,[ValidateSet('read','reopen','write')][string]$Phase)
 $spec=Get-FormSpec $Kind;$finish=switch($Phase){'read'{'return {done:true,missing:[],value:v};'}'reopen'{"if(v!==e)return {done:false,missing:['reopened-values']};return {done:true,missing:[],value:v};"}'write'{"if(v===e)return {done:true,missing:[],value:v};$($spec.write)return {done:false,missing:['field-values']};"}}
 return ("const d=document.querySelector('dialog[aria-labelledby=`"fill-forms-title`"]');if(!d){const b=[...document.querySelectorAll('button')].filter(x=>x.getAttribute('aria-label')==='Fill existing fields'&&!x.disabled);if(b.length===1)b[0].click();return {done:false,missing:['form-dialog']};}const names=__NAMES__,e=__EXPECTED__,c=names.map(n=>{const a=[...d.querySelectorAll('input,select')].filter(x=>x.getAttribute('aria-label')===n);return a.length===1?a[0]:null;});if(c.some(x=>!x))return {done:false,missing:names.filter((n,i)=>!c[i])};if(c.some(x=>x.disabled))return {done:false,missing:['enabled-fields']};const v=").Replace('__NAMES__',(ConvertTo-Json -InputObject @($spec.controls) -Compress)).Replace('__EXPECTED__',($spec.expected|ConvertTo-Json -Compress))+$spec.read+';'+$finish
}

function Invoke-FormSession { param($Context,[string]$SourcePath,[string]$OutputPath,[ValidateSet('mixed','radio','choice')][string]$Kind,[bool]$Reopen)
 $null=Open-CommentsFormsFile $Context $SourcePath
 $spec=Get-FormSpec $Kind;$allowed=@('form-dialog','enabled-fields','reopened-values','field-values')+@($spec.controls)
 $state=Invoke-CommentsFormsPoll -SessionId $Context.SessionId -Deadline $Context.Deadline -Script (Get-FormSessionScript $Kind $(if($Reopen){'reopen'}else{'read'})) -Allowed $allowed -Kind "Exact $Kind form fields"
 if($Reopen){return [pscustomobject][ordered]@{kind=$Kind;initial=[string]$state.value;reopenedValuesMatched=$true;pixelAppearanceVerified=$false;nativePdfSemanticVerified=$false}}
 $null=Invoke-CommentsFormsPoll -SessionId $Context.SessionId -Deadline $Context.Deadline -Script (Get-FormSessionScript $Kind 'write') -Allowed $allowed -Kind "Exact $Kind form entry"
 $receipt=Invoke-CommentsFormsNativeSave -SessionId $Context.SessionId -ProcessId $Context.ApplicationProcessId -ProcessStartUtcTicks $Context.ApplicationProcessStartUtcTicks -OutputPath $OutputPath -ButtonLabel 'Save filled copy' -ButtonMatch 'text' -Deadline $Context.Deadline
 return [pscustomobject][ordered]@{kind=$Kind;sourceValues=[string]$state.value;expectedOutputValues=[string]$spec.expected;output=$receipt;saveDialogVerified=$true}
}

function Invoke-InstalledCommentsFormsRuntime { param([string]$ApplicationPath,[string]$TauriDriverPath,[string]$EdgeDriverPath,[string]$SettingsRoot,[string]$Work,[string]$ExpectedEdgeDriverVersion,$CommentFixture,[object[]]$Fixtures)
 $commentSource=[string]$CommentFixture.path;$commentOutput=Join-Path $Work 'comments-saved.pdf'
 $created=Invoke-FreshInstalledImagePageToolsSession -ApplicationPath $ApplicationPath -TauriDriverPath $TauriDriverPath -EdgeDriverPath $EdgeDriverPath -ProfileRoot (Join-Path $Work 'comments-create-profile') -SettingsRoot $SettingsRoot -ExpectedEdgeDriverVersion $ExpectedEdgeDriverVersion -SessionCallback ({param($c)Invoke-CommentsCreateSession $c $commentSource $commentOutput})
 $reopened=Invoke-FreshInstalledImagePageToolsSession -ApplicationPath $ApplicationPath -TauriDriverPath $TauriDriverPath -EdgeDriverPath $EdgeDriverPath -ProfileRoot (Join-Path $Work 'comments-reopen-profile') -SettingsRoot $SettingsRoot -ExpectedEdgeDriverVersion $ExpectedEdgeDriverVersion -SessionCallback ({param($c)Invoke-CommentsReopenSession $c $commentOutput})
 $forms=@();foreach($fixture in $Fixtures){$output=Join-Path $Work ("form-$($fixture.kind)-saved.pdf");$write=Invoke-FreshInstalledImagePageToolsSession -ApplicationPath $ApplicationPath -TauriDriverPath $TauriDriverPath -EdgeDriverPath $EdgeDriverPath -ProfileRoot (Join-Path $Work ("form-$($fixture.kind)-write-profile")) -SettingsRoot $SettingsRoot -ExpectedEdgeDriverVersion $ExpectedEdgeDriverVersion -SessionCallback ({param($c)Invoke-FormSession $c $fixture.path $output $fixture.kind $false});$read=Invoke-FreshInstalledImagePageToolsSession -ApplicationPath $ApplicationPath -TauriDriverPath $TauriDriverPath -EdgeDriverPath $EdgeDriverPath -ProfileRoot (Join-Path $Work ("form-$($fixture.kind)-read-profile")) -SettingsRoot $SettingsRoot -ExpectedEdgeDriverVersion $ExpectedEdgeDriverVersion -SessionCallback ({param($c)Invoke-FormSession $c $output $output $fixture.kind $true});$forms+=[pscustomobject][ordered]@{kind=$fixture.kind;source=$fixture.receipt;write=$write;reopen=$read}}
 return [pscustomobject][ordered]@{comments=[pscustomobject][ordered]@{source=$CommentFixture.receipt;write=$created;reopen=$reopened};forms=$forms}
}

function Assert-CommentsFormsRuntimeReceipt { param($Value)
 Assert-ExactProperties $Value @('comments','forms') 'Comments/forms runtime receipt';if(@($Value.forms).Count-ne3){throw 'Exactly three form fixture results are required.'}
 function Assert-BoundedFileReceipt($Receipt,[string]$Kind){Assert-ExactProperties $Receipt @('bytes','sha256') $Kind;if([uint64]$Receipt.bytes-lt1-or[uint64]$Receipt.bytes-gt256MB-or[string]$Receipt.sha256-cnotmatch'^[A-F0-9]{64}$'){throw "$Kind is malformed."}}
 function Assert-SessionCleanup($Session,[string]$Kind){if(-not[bool]$Session.sessionDeleted-or-not[bool]$Session.ownedProcessTreeStopped-or[int]$Session.relevantProcessesRemaining-ne0){throw "$Kind cleanup was not exact."}}
 Assert-ExactProperties $Value.comments @('source','write','reopen') 'Comments runtime';Assert-BoundedFileReceipt $Value.comments.source 'Comments source receipt'
 Assert-ExactProperties $Value.comments.write @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','nativeFilenameControlCategory','output','stickyCreated','areaHighlightCreated','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') 'Comments write session';Assert-BoundedFileReceipt $Value.comments.write.output 'Comments output receipt';Assert-SessionCleanup $Value.comments.write 'Comments write session'
 Assert-ExactProperties $Value.comments.reopen @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','stickyCount','areaHighlightCount','page','pixelAppearanceVerified','nativePdfSemanticVerified','selectedTextHighlightVerified','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') 'Comments reopen session';Assert-SessionCleanup $Value.comments.reopen 'Comments reopen session'
 if(-not[bool]$Value.comments.write.stickyCreated-or-not[bool]$Value.comments.write.areaHighlightCreated-or[bool]$Value.comments.reopen.pixelAppearanceVerified-or[bool]$Value.comments.reopen.nativePdfSemanticVerified-or[bool]$Value.comments.reopen.selectedTextHighlightVerified-or[int]$Value.comments.reopen.stickyCount-ne1-or[int]$Value.comments.reopen.areaHighlightCount-ne1){throw 'Comments runtime receipt exceeded or missed its exact claim.'}
 $expected=@{mixed=@('Original|false','Installed Mixed|true');radio=@('Preference: Choice B, page 1','Preference: Choice A, page 1');choice=@('South Hub|Email copy','North Hub|Print copy')}
 $kinds=New-Object 'Collections.Generic.HashSet[string]' ([StringComparer]::Ordinal)
 foreach($form in @($Value.forms)){
  Assert-ExactProperties $form @('kind','source','write','reopen') 'Form runtime';Assert-BoundedFileReceipt $form.source 'Form source receipt'
  Assert-ExactProperties $form.write @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','kind','sourceValues','expectedOutputValues','output','saveDialogVerified','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') 'Form write session';Assert-BoundedFileReceipt $form.write.output 'Form output receipt';Assert-SessionCleanup $form.write 'Form write session'
  Assert-ExactProperties $form.reopen @('nativeDriverVersion','returnedRuntimeVersion','profileBinding','kind','initial','reopenedValuesMatched','pixelAppearanceVerified','nativePdfSemanticVerified','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining') 'Form reopen session';Assert-SessionCleanup $form.reopen 'Form reopen session'
  $pin=$expected[[string]$form.kind];if($null-eq$pin-or-not$kinds.Add([string]$form.kind)-or[string]$form.write.kind-cne[string]$form.kind-or[string]$form.reopen.kind-cne[string]$form.kind-or[string]$form.write.sourceValues-cne$pin[0]-or[string]$form.write.expectedOutputValues-cne$pin[1]-or[string]$form.reopen.initial-cne$pin[1]-or-not[bool]$form.write.saveDialogVerified-or-not[bool]$form.reopen.reopenedValuesMatched-or[bool]$form.reopen.pixelAppearanceVerified-or[bool]$form.reopen.nativePdfSemanticVerified){throw 'Form runtime receipt exceeded or missed its exact controlled-fixture claim.'}
 }
 if($kinds.Count-ne3){throw 'The exact controlled form kind set was not verified.'}
}

function ConvertTo-CommentsFormsPublicRuntime { param($Value)
 return [ordered]@{
  comments=[ordered]@{sourceBytes=[uint64]$Value.comments.source.bytes;sourceSha256=[string]$Value.comments.source.sha256;outputBytes=[uint64]$Value.comments.write.output.bytes;outputSha256=[string]$Value.comments.write.output.sha256;stickyCount=[int]$Value.comments.reopen.stickyCount;areaHighlightCount=[int]$Value.comments.reopen.areaHighlightCount;saveDialogVerified=$true;freshProcessReopenVerified=$true;sourcePreserved=$true;pixelAppearanceVerified=$false;nativePdfSemanticVerified=$false;selectedTextHighlightVerified=$false}
  forms=@($Value.forms|ForEach-Object{[ordered]@{kind=[string]$_.kind;sourceBytes=[uint64]$_.source.bytes;sourceSha256=[string]$_.source.sha256;outputBytes=[uint64]$_.write.output.bytes;outputSha256=[string]$_.write.output.sha256;saveDialogVerified=$true;freshProcessReopenVerified=$true;sourcePreserved=$true;pixelAppearanceVerified=$false;nativePdfSemanticVerified=$false}})
 }
}

Assert-ProofSourceParameters
if($ProofSource-ceq'draft'){
if($TargetVersion-cnotmatch'^\d+\.\d+\.\d+$'-or$TargetTag-cne('v'+$TargetVersion)){throw 'Target version and stable tag are malformed or inconsistent.'}
$root=Resolve-RunnerPath $WorkRoot $env:RUNNER_TEMP -MustBeFresh;$output=Resolve-RunnerPath $OutputRoot $env:RUNNER_TEMP -MustBeFresh;$assets=Resolve-RunnerPath $AssetRoot $env:RUNNER_TEMP -MustBeFresh;$drivers=Resolve-RunnerPath $WebDriverRoot $env:RUNNER_TEMP -MustExist
$expandedOutput=Join-Path $root 'expanded-proof-output';$expandedWork=Join-Path $root 'expanded-proof-work'
$forward=@{ProjectRoot=$ProjectRoot;TargetSourceRoot=$TargetSourceRoot;DraftProofRoot=$DraftProofRoot;AssetRoot=$assets;WorkRoot=$expandedWork;OutputRoot=$expandedOutput;WebDriverRoot=$drivers;WorkflowSourceRevision=$WorkflowSourceRevision;DraftProofWorkflowRevision=$DraftProofWorkflowRevision;DraftProofRunId=$DraftProofRunId;DraftProofWorkflowId=$DraftProofWorkflowId;DraftProofArtifactId=$DraftProofArtifactId;DraftProofArtifactName=$DraftProofArtifactName;DraftProofArtifactBytes=$DraftProofArtifactBytes;DraftProofArtifactDigest=$DraftProofArtifactDigest;DraftProofReceiptBytes=$DraftProofReceiptBytes;DraftProofReceiptSha256=$DraftProofReceiptSha256;TargetVersion=$TargetVersion;TargetTag=$TargetTag;TargetSourceRevision=$TargetSourceRevision;TargetReleaseId=$TargetReleaseId;InstallerAssetId=$InstallerAssetId;InstallerName=$InstallerName;InstallerBytes=$InstallerBytes;InstallerSha256=$InstallerSha256;SignatureAssetId=$SignatureAssetId;SignatureBytes=$SignatureBytes;SignatureSha256=$SignatureSha256;ManifestAssetId=$ManifestAssetId;ManifestBytes=$ManifestBytes;ManifestSha256=$ManifestSha256;ExpectedPublisher=$ExpectedPublisher}
}else{
if($TargetVersion-cnotmatch'^\d+\.\d+\.\d+$'-or$WorkflowSourceRevision-cnotmatch'^[a-f0-9]{40}$'-or$TargetSourceRevision-cnotmatch'^[a-f0-9]{40}$'){throw 'Rehearsal target identity is malformed.'}
$root=Resolve-RunnerPath $WorkRoot $env:RUNNER_TEMP -MustBeFresh;$output=Resolve-RunnerPath $OutputRoot $env:RUNNER_TEMP -MustBeFresh;$assets=Resolve-RunnerPath $ArtifactRoot $env:RUNNER_TEMP -MustExist;$drivers=Resolve-RunnerPath $WebDriverRoot $env:RUNNER_TEMP -MustExist
$expandedOutput=Join-Path $root 'expanded-proof-output';$expandedWork=Join-Path $root 'expanded-proof-work'
$forward=@{ProjectRoot=$ProjectRoot;TargetSourceRoot=$TargetSourceRoot;DraftProofRoot=$DraftProofRoot;WorkRoot=$expandedWork;OutputRoot=$expandedOutput;WebDriverRoot=$drivers;WorkflowSourceRevision=$WorkflowSourceRevision;TargetVersion=$TargetVersion;TargetSourceRevision=$TargetSourceRevision;InstallerName=$InstallerName;InstallerBytes=$InstallerBytes;InstallerSha256=$InstallerSha256;ExpectedPublisher=$ExpectedPublisher;ProofSource='artifact';ArtifactRoot=$assets;ArtifactRunId=$ArtifactRunId;ArtifactId=$ArtifactId;ArtifactName=$ArtifactName;ArtifactBytes=$ArtifactBytes;ArtifactDigest=$ArtifactDigest}
}
& (Join-Path $PSScriptRoot 'verify-default-expanded-page-tools.ps1') @forward
$expandedReceipt=Join-Path $expandedOutput $(if($ProofSource-ceq'draft'){'default-expanded-page-tools-verification.json'}else{'default-expanded-page-tools-rehearsal.json'});$expandedBinding=Get-CommentsFormsReceipt $expandedReceipt
$env:GITHUB_TOKEN=$null;$env:GH_TOKEN=$null
if($env:GITHUB_TOKEN-or$env:GH_TOKEN){throw 'GitHub tokens remained before comments/forms installation or application launch.'}
$sourceBinding=Assert-CommentsFormsSourceParity
if($ProofSource-ceq'draft'){
$proofFile=@(Get-ChildItem -LiteralPath $DraftProofRoot -Recurse -File -Force);if($proofFile.Count-ne1){throw 'Step 1 proof artifact shape changed.'};$receipt=Get-Content $proofFile[0].FullName -Raw -Encoding UTF8|ConvertFrom-Json;$sets=Assert-DefaultDraftReceipt $receipt
}else{$rehearsal=Read-DefaultArtifactRehearsal (Resolve-RunnerPath $DraftProofRoot $env:RUNNER_TEMP -MustExist) $assets;$receipt=$rehearsal.Receipt;$sets=$rehearsal.Sets}
$installer=Join-Path $assets $InstallerName;$installRoot=[IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'PDF Workstation')).TrimEnd('\');$settings=[IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA 'local.pdfworkstation.desktop')).TrimEnd('\');$registry='Registry::HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Uninstall\PDF Workstation'
$fresh=Get-FreshInstallFacts $installRoot $settings $registry @($installer);Assert-FreshInstallFacts $fresh;$baseline=Get-ConflictingProcessFacts @($installer);Assert-EmptyProcessFacts $baseline 'Baseline'
$installed=$false;$primary=$null;$cleanup=$null;$final=$null
try{
 Invoke-BoundedSilentInstaller $installer;$installed=$true;$facts=Get-InstallFacts $registry $installRoot;Assert-InstallFacts $facts $TargetVersion $installRoot $receipt.packagedApplication 'Valid' $ExpectedPublisher $true;$resources=Assert-InstalledDefaultResources $installRoot $receipt $sets;Save-OwnedInstallSnapshot $installRoot
 [IO.Directory]::CreateDirectory($settings)|Out-Null;$fixtureRoot=Join-Path $root 'fixtures';[IO.Directory]::CreateDirectory($fixtureRoot)|Out-Null
 $commentPath=Join-Path $fixtureRoot 'comments-source.pdf';Copy-Item -LiteralPath $resources.Welcome.FullName -Destination $commentPath;$commentFixture=[pscustomobject]@{path=$commentPath;receipt=Get-CommentsFormsReceipt $commentPath}
 $specs=@(@('mixed','reportlab-mixed-fields.pdf'),@('radio','reportlab-radio-fields.pdf'),@('choice','reportlab-choice-fields.pdf'));$fixtures=@();foreach($spec in $specs){$source=Join-Path $TargetSourceRoot ('src-tauri/tests/fixtures/'+$spec[1]);$copy=Join-Path $fixtureRoot $spec[1];Copy-Item -LiteralPath $source -Destination $copy;$fixtures+=[pscustomobject]@{kind=$spec[0];path=$copy;receipt=Get-CommentsFormsReceipt $copy}}
 $runtime=if($RuntimeProvider){&$RuntimeProvider $resources $drivers $settings $root $commentFixture $fixtures $receipt}else{Invoke-InstalledCommentsFormsRuntime -ApplicationPath $resources.Application.FullName -TauriDriverPath (Join-Path $drivers 'tauri-driver-install/bin/tauri-driver.exe') -EdgeDriverPath (Join-Path $drivers 'edge-driver/msedgedriver.exe') -SettingsRoot $settings -Work $root -ExpectedEdgeDriverVersion ([string](Get-Content (Join-Path $drivers 'webdriver-receipt.json') -Raw|ConvertFrom-Json).edgeDriver.version) -CommentFixture $commentFixture -Fixtures $fixtures};Assert-CommentsFormsRuntimeReceipt $runtime
 foreach($fixture in @($commentFixture)+$fixtures){Assert-FileReceipt $fixture.path $fixture.receipt.bytes $fixture.receipt.sha256 'Preserved comments/forms source fixture'}
}catch{$primary=$_}finally{try{Invoke-OwnedCleanup $installed $installRoot $settings;$final=Get-ConflictingProcessFacts @($installer);Assert-OwnedCleanupState $registry $installRoot $settings $final}catch{$cleanup=$_}}
Complete-PrimaryAndCleanup $primary $cleanup
if($ProofSource-ceq'draft'){
[IO.Directory]::CreateDirectory($output)|Out-Null;$record=[ordered]@{schemaVersion=1;scope='Installed sticky-comment, area-highlight, and three controlled form-fixture save/reopen verification. DOM persistence only; no selected-text highlight, native PDF semantic, arbitrary form, or pixel-appearance claim.';workflowSourceRevision=$WorkflowSourceRevision;targetSourceRevision=$TargetSourceRevision;targetVersion=$TargetVersion;step1=[ordered]@{workflowSourceRevision=$DraftProofWorkflowRevision;workflowId=[uint64]$DraftProofWorkflowId;runId=[uint64]$DraftProofRunId;artifactId=[uint64]$DraftProofArtifactId;artifactDigest=$DraftProofArtifactDigest;receiptBytes=[uint64]$DraftProofReceiptBytes;receiptSha256=$DraftProofReceiptSha256};expandedProof=$expandedBinding;sourceBinding=$sourceBinding;runtime=ConvertTo-CommentsFormsPublicRuntime $runtime;limits=[ordered]@{stickyComment=$true;areaHighlight=$true;selectedTextHighlight=$false;controlledFormFixtureCount=3;nativePdfSemanticVerified=$false;pixelAppearanceVerified=$false};cleanup=[ordered]@{silentUninstallCompleted=$true;baseline=$baseline;final=$final;ownedSettingsRemoved=$true}}
$json=$record|ConvertTo-Json -Depth 30;if($json-match'(?i)([A-Z]:\\|\\Users\\|runneradmin|RUNNER_TEMP|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log)'){throw 'Comments/forms receipt contains host or credential data.'};[IO.File]::WriteAllText((Join-Path $output 'default-comments-forms-verification.json'),$json,[Text.UTF8Encoding]::new($false))
}else{
[IO.Directory]::CreateDirectory($output)|Out-Null;$record=[ordered]@{schemaVersion=1;scope='Pre-tag rehearsal on a signed release-candidate artifact rather than a release draft: installed sticky-comment, area-highlight, and three controlled form-fixture save/reopen verification. DOM persistence only; no selected-text highlight, native PDF semantic, arbitrary form, or pixel-appearance claim, and no release, draft or updater claim.';workflowSourceRevision=$WorkflowSourceRevision;targetSourceRevision=$TargetSourceRevision;targetVersion=$TargetVersion;rehearsal=$rehearsal.Binding;expandedProof=$expandedBinding;sourceBinding=$sourceBinding;runtime=ConvertTo-CommentsFormsPublicRuntime $runtime;limits=[ordered]@{stickyComment=$true;areaHighlight=$true;selectedTextHighlight=$false;controlledFormFixtureCount=3;nativePdfSemanticVerified=$false;pixelAppearanceVerified=$false};cleanup=[ordered]@{silentUninstallCompleted=$true;baseline=$baseline;final=$final;ownedSettingsRemoved=$true}}
$json=$record|ConvertTo-Json -Depth 30;if($json-match'(?i)([A-Z]:\\|\\Users\\|runneradmin|RUNNER_TEMP|GITHUB_TOKEN|GH_TOKEN|AZURE_|TAURI_SIGNING|raw\.log)'){throw 'Comments/forms receipt contains host or credential data.'};[IO.File]::WriteAllText((Join-Path $output 'default-comments-forms-rehearsal.json'),$json,[Text.UTF8Encoding]::new($false))
}
