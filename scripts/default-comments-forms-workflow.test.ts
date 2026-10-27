import { describe, expect, it } from 'vitest';
import { readFileSync, writeFileSync, mkdirSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

const verifierPath = 'scripts/verify-default-comments-forms.ps1';
const workflowPath = '.github/workflows/default-comments-forms-verification.yml';

function runPowerShell(source: string) {
  const root = join(tmpdir(), `smacrobat-comments-forms-${randomUUID()}`);
  mkdirSync(root, { recursive: true });
  const path = join(root, 'run.ps1');
  writeFileSync(path, source, 'utf8');
  try { return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-File', path], { encoding: 'utf8', timeout: 60_000 }); }
  finally { rmSync(root, { recursive: true, force: true }); }
}

function extractFunctions(names: string[], body: string) {
  return String.raw`
    $ErrorActionPreference='Stop';Set-StrictMode -Version Latest
    $tokens=$null;$errors=$null;$path='${process.cwd().replaceAll("'", "''")}\\${verifierPath.replaceAll('/', '\\')}'
    $ast=[Management.Automation.Language.Parser]::ParseFile($path,[ref]$tokens,[ref]$errors);if($errors.Count){throw 'Verifier did not parse.'}
    foreach($name in @(${names.map(name => `'${name}'`).join(',')})){$node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq$name},$true);if(-not$node){throw ('Missing function: '+$name)};Invoke-Expression $node.Extent.Text}
    ${body}`;
}

describe('default comments and forms workflow', () => {
  it('is a manual master-only workflow with job-scoped draft visibility and exact immutable actions', () => {
    const workflow = readFileSync(workflowPath, 'utf8');
    expect(workflow).toContain('name: Manual default installed comments and forms verification');
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).toContain("$env:GITHUB_REF -cne 'refs/heads/master'");
    expect(workflow).toContain('contents: read');
    expect(workflow).toContain('actions: read');
    expect(workflow.match(/contents:\s*write/g)).toHaveLength(1);
    expect(workflow.indexOf('contents: write')).toBeGreaterThan(workflow.indexOf('verify-installed-comments-forms:'));
    expect(workflow).not.toMatch(/actions:\s*write|id-token:\s*write|pull_request|push:|schedule:|PAT|GH_TOKEN/);
    expect(workflow).toContain('actions/checkout@11d5960a326750d5838078e36cf38b85af677262');
    expect(workflow).toContain('actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093');
    expect(workflow).toContain('actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02');
    expect(workflow).toContain('./scripts/verify-default-comments-forms.ps1');
    expect(workflow).toContain('default-comments-forms-verification.json');
    expect(workflow.match(/git diff --cached --quiet --/g)).toHaveLength(2);
    expect(workflow.match(/git -C target-source diff --cached --quiet --/g)).toHaveLength(2);
    const pdfiumPrep = "./scripts/prepare-default-draft-pdfium.ps1 -TargetSourceRoot (Resolve-Path target-source).Path -WorkRoot (Join-Path $env:RUNNER_TEMP 'default-installed-pdfium-prep')";
    expect(workflow).toContain(pdfiumPrep);
    expect(workflow.indexOf(pdfiumPrep)).toBeLessThan(workflow.indexOf('./scripts/verify-default-comments-forms.ps1'));
  });

  it('retains exact imported helpers from both trusted verifier scripts', () => {
    const check=String.raw`
      $ErrorActionPreference='Stop';Set-StrictMode -Version Latest;Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      $tokens=$null;$errors=$null;$ast=[Management.Automation.Language.Parser]::ParseFile((Resolve-Path ./scripts/verify-default-comments-forms.ps1),[ref]$tokens,[ref]$errors);if($errors.Count){throw ($errors|Out-String)}
      $node=$ast.Find({param($n)$n-is[Management.Automation.Language.FunctionDefinitionAst]-and$n.Name-ceq'Import-CommentsFormsFunctions'},$true);Invoke-Expression $node.Extent.Text
      Import-CommentsFormsFunctions (Resolve-Path ./scripts/verify-signed-ocr-upgrade.ps1) @('Assert-ReceiptValue')
      Import-CommentsFormsFunctions (Resolve-Path ./scripts/verify-default-expanded-page-tools.ps1) @('Assert-EmptyProcessFacts')
      foreach($name in @('Assert-ReceiptValue','Assert-EmptyProcessFacts')){if(-not(Get-Command $name -CommandType Function -ErrorAction SilentlyContinue)){throw ('Imported helper did not persist: '+$name)}}
      Assert-ReceiptValue ([pscustomobject]@{bytes=[uint64]1;sha256=('A'*64)}) 'test'
      Assert-EmptyProcessFacts ([pscustomobject]@{applicationProcessPresent=$false;installerProcessPresent=$false}) 'test'
      Write-Output PASS
    `;
    const result=runPowerShell(check);expect(result.error).toBeUndefined();expect(result.status,result.stderr||result.stdout).toBe(0);expect(result.stdout).toContain('PASS');
  });

  it('parses and pins Step 1 identity, independent Step 2 reproof, token clearing, exact install checks, and bounded cleanup', () => {
    const source = readFileSync(verifierPath, 'utf8');
    const parsed = spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', `$t=$null;$e=$null;[Management.Automation.Language.Parser]::ParseFile('${process.cwd().replaceAll("'", "''")}\\scripts\\verify-default-comments-forms.ps1',[ref]$t,[ref]$e)|Out-Null;if($e.Count){$e|% ToString;exit 1}`], { encoding: 'utf8' });
    expect(parsed.status, parsed.stderr || parsed.stdout).toBe(0);
    for (const marker of ['DraftProofWorkflowRevision','DraftProofRunId','DraftProofWorkflowId','DraftProofArtifactId','DraftProofArtifactDigest','DraftProofReceiptSha256','verify-default-expanded-page-tools.ps1','Invoke-BoundedSilentInstaller','Assert-InstallFacts','Assert-InstalledDefaultResources','Invoke-OwnedCleanup','Assert-OwnedCleanupState','Complete-PrimaryAndCleanup']) expect(source).toContain(marker);
    expect(source).toContain('$env:GITHUB_TOKEN=$null');
    expect(source).toContain('$env:GH_TOKEN=$null');
    expect(source.indexOf('$env:GITHUB_TOKEN=$null')).toBeLessThan(source.indexOf('Invoke-BoundedSilentInstaller $installer'));
    expect(source).not.toMatch(/SUPABASE|AZURE_CLIENT_SECRET|TAURI_SIGNING_PRIVATE_KEY|gh auth|git push/i);
  });

  it('pins exactly 17 product and fixture source blobs and only the three controlled form PDFs', () => {
    const source = readFileSync(verifierPath, 'utf8');
    const list = source.match(/\$script:CommentsFormsPaths=@\((.*?)\)\r?\n/s)?.[1] ?? '';
    expect([...list.matchAll(/'([^']+)'/g)].map(match => match[1])).toHaveLength(17);
    for (const fixture of ['reportlab-mixed-fields.pdf','reportlab-radio-fields.pdf','reportlab-choice-fields.pdf']) expect(source).toContain(fixture);
    expect(source).not.toContain('reportlab-choice-blank-fields.pdf');
    expect(source).toContain('featureBlobCount=17');
    expect(source).toContain('selectedTextHighlight=$false');
    expect(source).toContain('pixelAppearanceVerified=$false');
    expect(source).toContain('nativePdfSemanticVerified=$false');
  });

  it('rejects missing fresh-process reopen proof and any invented appearance or native semantic claim', () => {
    const result = runPowerShell(extractFunctions(['Assert-CommentsFormsRuntimeReceipt'], String.raw`
      function Assert-ExactProperties{param($Value,[string[]]$Expected,[string]$Kind)$a=@($Value.PSObject.Properties.Name|Sort-Object);$e=@($Expected|Sort-Object);if($a.Count-ne$e.Count-or(Compare-Object $a $e)){throw 'shape'}}
      function Receipt{[pscustomobject]@{bytes=[uint64]10;sha256='A'*64}}
      function Common{[ordered]@{nativeDriverVersion='1.2.3.4';returnedRuntimeVersion='1.2.3.9';profileBinding='session-capability-requested-profile'}}
      function New-Value{$pins=@{mixed=@('Original|false','Installed Mixed|true');radio=@('Preference: Choice B, page 1','Preference: Choice A, page 1');choice=@('south-002|email-002','north-001|print-001')};$forms=@();foreach($kind in @('mixed','radio','choice')){$write=Common;$write.kind=$kind;$write.sourceValues=$pins[$kind][0];$write.expectedOutputValues=$pins[$kind][1];$write.output=Receipt;$write.saveDialogVerified=$true;$write.sessionDeleted=$true;$write.ownedProcessTreeStopped=$true;$write.relevantProcessesRemaining=0;$reopen=Common;$reopen.kind=$kind;$reopen.initial=$pins[$kind][1];$reopen.reopenedValuesMatched=$true;$reopen.pixelAppearanceVerified=$false;$reopen.nativePdfSemanticVerified=$false;$reopen.sessionDeleted=$true;$reopen.ownedProcessTreeStopped=$true;$reopen.relevantProcessesRemaining=0;$forms+=[pscustomobject][ordered]@{kind=$kind;source=Receipt;write=[pscustomobject]$write;reopen=[pscustomobject]$reopen}};$commentWrite=Common;$commentWrite.nativeFilenameControlCategory='edit-1001';$commentWrite.output=Receipt;$commentWrite.stickyCreated=$true;$commentWrite.areaHighlightCreated=$true;$commentWrite.sessionDeleted=$true;$commentWrite.ownedProcessTreeStopped=$true;$commentWrite.relevantProcessesRemaining=0;$commentReopen=Common;$commentReopen.stickyCount=1;$commentReopen.areaHighlightCount=1;$commentReopen.page=1;$commentReopen.pixelAppearanceVerified=$false;$commentReopen.nativePdfSemanticVerified=$false;$commentReopen.selectedTextHighlightVerified=$false;$commentReopen.sessionDeleted=$true;$commentReopen.ownedProcessTreeStopped=$true;$commentReopen.relevantProcessesRemaining=0;[pscustomobject]@{comments=[pscustomobject][ordered]@{source=Receipt;write=[pscustomobject]$commentWrite;reopen=[pscustomobject]$commentReopen};forms=$forms}}
      $null=Assert-CommentsFormsRuntimeReceipt (New-Value)
      function Reject($value,[string]$kind){$bad=$false;try{Assert-CommentsFormsRuntimeReceipt $value}catch{$bad=$true};if(-not$bad){throw ($kind+' was accepted')}}
      $missing=New-Value;$missing.forms[1].reopen.reopenedValuesMatched=$false;Reject $missing 'missing reopen'
      $pixels=New-Value;$pixels.comments.reopen.pixelAppearanceVerified=$true;Reject $pixels 'pixel claim'
      $native=New-Value;$native.forms[2].reopen.nativePdfSemanticVerified=$true;Reject $native 'native semantic claim'
      $selected=New-Value;$selected.comments.reopen.selectedTextHighlightVerified=$true;Reject $selected 'selected text claim'
      $wrong=New-Value;$wrong.forms[0].write.sourceValues='guessed';Reject $wrong 'guessed source value'
      $duplicate=New-Value;$duplicate.forms[2].kind='radio';$duplicate.forms[2].write.kind='radio';$duplicate.forms[2].reopen.kind='radio';Reject $duplicate 'duplicate form kind'
      $badHash=New-Value;$badHash.forms[1].write.output.sha256='bad';Reject $badHash 'malformed output hash'
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });

  it('uses distinct fresh profiles for write and reopen and preserves source hashes after native saves', () => {
    const source = readFileSync(verifierPath, 'utf8');
    for (const marker of ['comments-create-profile','comments-reopen-profile','form-$($fixture.kind)-write-profile','form-$($fixture.kind)-read-profile']) expect(source).toContain(marker);
    expect(source.match(/Invoke-FreshInstalledImagePageToolsSession/g)?.length).toBeGreaterThanOrEqual(4);
    expect(source).toContain("Assert-FileReceipt $fixture.path $fixture.receipt.bytes $fixture.receipt.sha256 'Preserved comments/forms source fixture'");
    expect(source).toContain("-ActionTransport 'native-bm-click-delivered'");
    expect(source).toContain("-ButtonLabel 'Save a copy'");
    expect(source).toContain("-ButtonLabel 'Save filled copy'");
  });

  it('polls for the page image, comment layer and enabled creation button before clicking, and names what is missing on timeout', () => {
    const source = readFileSync(verifierPath, 'utf8');
    expect(source).not.toContain("throw 'Exact sticky-comment creation controls were unavailable.'");
    expect(source).toContain("Invoke-CommentsCreationControl $Context 'Add comment' 'sticky-comment'");
    expect(source).toContain("Invoke-CommentsCreationControl $Context 'Add area highlight' 'area-highlight'");
    expect(source).toContain("-Kind 'sticky comment placement'");
    expect(source).toContain("-Kind 'area highlight placement'");
    const result = runPowerShell(extractFunctions(['Get-CommentsCreationControlScript', 'Invoke-CommentsCreationControl'], String.raw`
      $probe=Get-CommentsCreationControlScript 'Add comment'
      foreach($marker in @('img[alt="Page 1"]','[data-testid="comment-layer"]','x.getAttribute(''aria-label'')==="Add comment"','!x.disabled','b.length!==1','b[0].click()')){if(-not$probe.Contains($marker,[StringComparison]::Ordinal)){throw ('Probe lacks '+$marker)}}
      if($probe.IndexOf('b[0].click()')-lt$probe.IndexOf('if(m.length)return')){throw 'Probe clicks before all three conditions hold.'}
      $script:calls=0;$script:replies=@()
      function Invoke-WebDriverScript{param($SessionId,$Deadline,$Script)$script:calls++;$reply=$script:replies[[Math]::Min($script:calls,$script:replies.Count)-1];return ($reply|ConvertFrom-Json)}
      $context=[pscustomobject]@{SessionId='s';Deadline=[datetime]::UtcNow.AddSeconds(20)}
      $script:replies=@('{"clicked":false,"missing":["page-image","comment-layer"]}','{"clicked":false,"missing":["comment-layer"]}','{"clicked":false,"missing":["enabled-button"]}','{"clicked":true,"missing":[]}')
      Invoke-CommentsCreationControl $context 'Add comment' 'sticky-comment';if($script:calls-ne4){throw ('Expected four polls, saw '+$script:calls)}
      $script:calls=0;$script:replies=@('{"clicked":false,"missing":["enabled-button"]}');$context.Deadline=[datetime]::UtcNow.AddMilliseconds(900);$message=$null
      try{Invoke-CommentsCreationControl $context 'Add area highlight' 'area-highlight'}catch{$message=$_.Exception.Message}
      if($message-cne'Exact area-highlight creation controls were unavailable. Missing: enabled-button.'){throw ('Unexpected timeout message: '+$message)};if($script:calls-lt2){throw 'Timeout path did not poll.'}
      foreach($bad in @('{"clicked":false,"missing":[]}','{"clicked":true,"missing":["page-image"]}','{"clicked":false,"missing":["C:\\Users\\x"]}','{"clicked":"yes","missing":[]}','null')){$script:calls=0;$script:replies=@($bad);$context.Deadline=[datetime]::UtcNow.AddSeconds(5);$message=$null;try{Invoke-CommentsCreationControl $context 'Add comment' 'sticky-comment'}catch{$message=$_.Exception.Message};if($message-cne'Exact sticky-comment creation control probe returned an unexpected shape.'){throw ('Bad probe reply accepted: '+$bad+' -> '+$message)}}
      'PASS'
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  });

  it('projects the validated runtime to bounded public counts, booleans, categories, and hashes only', () => {
    const source = readFileSync(verifierPath, 'utf8');
    expect(source).toContain('runtime=ConvertTo-CommentsFormsPublicRuntime $runtime');
    expect(source).not.toContain('runtime=$runtime;limits=');
    const result = runPowerShell(extractFunctions(['ConvertTo-CommentsFormsPublicRuntime'], String.raw`
      function Receipt([string]$hash){[pscustomobject]@{bytes=[uint64]17;sha256=$hash}}
      function Session([string]$kind,[string]$sourceValue,[string]$expected,[string]$hash){[pscustomobject]@{nativeDriverVersion='FORBIDDEN-DRIVER';returnedRuntimeVersion='FORBIDDEN-RUNTIME';profileBinding='FORBIDDEN-PROFILE';kind=$kind;sourceValues=$sourceValue;expectedOutputValues=$expected;initial=$expected;output=Receipt $hash;saveDialogVerified=$true;reopenedValuesMatched=$true;pixelAppearanceVerified=$false;nativePdfSemanticVerified=$false;sessionDeleted=$true;ownedProcessTreeStopped=$true;relevantProcessesRemaining=0}}
      $commentWrite=Session 'comment' 'FORBIDDEN-COMMENT-SOURCE' 'FORBIDDEN-COMMENT-EXPECTED' ('B'*64);$commentWrite|Add-Member stickyCreated $true;$commentWrite|Add-Member areaHighlightCreated $true
      $commentRead=Session 'comment' 'FORBIDDEN-COMMENT-READ' 'FORBIDDEN-COMMENT-INITIAL' ('C'*64);$commentRead|Add-Member stickyCount 1;$commentRead|Add-Member areaHighlightCount 1;$commentRead|Add-Member selectedTextHighlightVerified $false
      $forms=@();foreach($kind in @('mixed','radio','choice')){$forms+=[pscustomobject]@{kind=$kind;source=Receipt ('D'*64);write=Session $kind ('FORBIDDEN-'+$kind+'-SOURCE') ('FORBIDDEN-'+$kind+'-EXPECTED') ('E'*64);reopen=Session $kind ('FORBIDDEN-'+$kind+'-READ') ('FORBIDDEN-'+$kind+'-INITIAL') ('F'*64)}}
      $runtime=[pscustomobject]@{comments=[pscustomobject]@{source=Receipt ('A'*64);write=$commentWrite;reopen=$commentRead};forms=$forms}
      $value=ConvertTo-CommentsFormsPublicRuntime $runtime
      function Exact($item,[string[]]$names){$actual=@($item.PSObject.Properties.Name|Sort-Object);$expected=@($names|Sort-Object);if($actual.Count-ne$expected.Count-or(Compare-Object $actual $expected)){throw ('Projected shape changed: '+($actual-join','))}}
      Exact ([pscustomobject]$value) @('comments','forms');Exact ([pscustomobject]$value.comments) @('sourceBytes','sourceSha256','outputBytes','outputSha256','stickyCount','areaHighlightCount','saveDialogVerified','freshProcessReopenVerified','sourcePreserved','pixelAppearanceVerified','nativePdfSemanticVerified','selectedTextHighlightVerified')
      if(@($value.forms).Count-ne3){throw 'Projected form count changed.'};foreach($form in @($value.forms)){Exact ([pscustomobject]$form) @('kind','sourceBytes','sourceSha256','outputBytes','outputSha256','saveDialogVerified','freshProcessReopenVerified','sourcePreserved','pixelAppearanceVerified','nativePdfSemanticVerified');if($form.sourceBytes-ne17-or$form.outputBytes-ne17-or-not$form.saveDialogVerified-or-not$form.freshProcessReopenVerified-or-not$form.sourcePreserved-or$form.pixelAppearanceVerified-or$form.nativePdfSemanticVerified){throw 'Projected proof values changed.'}}
      if($value.comments.stickyCount-ne1-or$value.comments.areaHighlightCount-ne1-or$value.comments.sourceSha256-cne('A'*64)-or$value.comments.outputSha256-cne('B'*64)){throw 'Projected comment counts or hashes changed.'}
      $json=$value|ConvertTo-Json -Depth 10 -Compress;foreach($forbidden in @('FORBIDDEN-','nativeDriverVersion','returnedRuntimeVersion','profileBinding','sourceValues','expectedOutputValues','initial','sessionDeleted','ownedProcessTreeStopped','relevantProcessesRemaining')){if($json.Contains($forbidden,[StringComparison]::Ordinal)){throw ('Forbidden public runtime field escaped: '+$forbidden)}}
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
  });
});
