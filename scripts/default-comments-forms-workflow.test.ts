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
    expect(workflow).toContain('actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1');
    expect(workflow).toContain('actions/download-artifact@37930b1c2abaa49bbe596cd826c3c89aef350131');
    expect(workflow).toContain('actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9');
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

  it('pins exactly 19 product and fixture source blobs and only the four controlled form PDFs', () => {
    const source = readFileSync(verifierPath, 'utf8');
    const list = source.match(/\$script:CommentsFormsPaths=@\((.*?)\)\r?\n/s)?.[1] ?? '';
    expect([...list.matchAll(/'([^']+)'/g)].map(match => match[1])).toHaveLength(19);
    for (const fixture of ['reportlab-mixed-fields.pdf','reportlab-radio-fields.pdf','reportlab-choice-fields.pdf','reportlab-multi-combo-fields.pdf']) expect(source).toContain(fixture);
    expect(source).not.toContain('reportlab-choice-blank-fields.pdf');
    expect(source).toContain('featureBlobCount=19');
    expect(source).toContain('selectedTextHighlight=$false');
    expect(source).toContain('pixelAppearanceVerified=$false');
    expect(source).toContain('nativePdfSemanticVerified=$false');
  });

  it('rejects missing fresh-process reopen proof and any invented appearance or native semantic claim', () => {
    const result = runPowerShell(extractFunctions(['Assert-CommentsFormsRuntimeReceipt'], String.raw`
      function Assert-ExactProperties{param($Value,[string[]]$Expected,[string]$Kind)$a=@($Value.PSObject.Properties.Name|Sort-Object);$e=@($Expected|Sort-Object);if($a.Count-ne$e.Count-or(Compare-Object $a $e)){throw 'shape'}}
      function Receipt{[pscustomobject]@{bytes=[uint64]10;sha256='A'*64}}
      function Common{[ordered]@{nativeDriverVersion='1.2.3.4';returnedRuntimeVersion='1.2.3.9';profileBinding='session-capability-requested-profile'}}
      function New-Value{$pins=@{mixed=@('Original|false','Installed Mixed|true');radio=@('Preference: Choice B, page 1','Preference: Choice A, page 1');choice=@('South Hub|Email copy','North Hub|Print copy');multicombo=@('Pickup|Atlas Freight|Standard','Delivery,Insurance|Handled by ACME|Rush')};$forms=@();foreach($kind in @('mixed','radio','choice','multicombo')){$write=Common;$write.kind=$kind;$write.sourceValues=$pins[$kind][0];$write.expectedOutputValues=$pins[$kind][1];$write.output=Receipt;$write.saveDialogVerified=$true;$write.sessionDeleted=$true;$write.ownedProcessTreeStopped=$true;$write.relevantProcessesRemaining=0;$reopen=Common;$reopen.kind=$kind;$reopen.initial=$pins[$kind][1];$reopen.reopenedValuesMatched=$true;$reopen.pixelAppearanceVerified=$false;$reopen.nativePdfSemanticVerified=$false;$reopen.sessionDeleted=$true;$reopen.ownedProcessTreeStopped=$true;$reopen.relevantProcessesRemaining=0;$forms+=[pscustomobject][ordered]@{kind=$kind;source=Receipt;write=[pscustomobject]$write;reopen=[pscustomobject]$reopen}};$commentWrite=Common;$commentWrite.nativeFilenameControlCategory='edit-1001';$commentWrite.output=Receipt;$commentWrite.stickyCreated=$true;$commentWrite.areaHighlightCreated=$true;$commentWrite.sessionDeleted=$true;$commentWrite.ownedProcessTreeStopped=$true;$commentWrite.relevantProcessesRemaining=0;$commentReopen=Common;$commentReopen.stickyCount=1;$commentReopen.areaHighlightCount=1;$commentReopen.page=1;$commentReopen.pixelAppearanceVerified=$false;$commentReopen.nativePdfSemanticVerified=$false;$commentReopen.selectedTextHighlightVerified=$false;$commentReopen.sessionDeleted=$true;$commentReopen.ownedProcessTreeStopped=$true;$commentReopen.relevantProcessesRemaining=0;[pscustomobject]@{comments=[pscustomobject][ordered]@{source=Receipt;write=[pscustomobject]$commentWrite;reopen=[pscustomobject]$commentReopen};forms=$forms}}
      $null=Assert-CommentsFormsRuntimeReceipt (New-Value)
      function Reject($value,[string]$kind){$bad=$false;try{Assert-CommentsFormsRuntimeReceipt $value}catch{$bad=$true};if(-not$bad){throw ($kind+' was accepted')}}
      $missing=New-Value;$missing.forms[1].reopen.reopenedValuesMatched=$false;Reject $missing 'missing reopen'
      $pixels=New-Value;$pixels.comments.reopen.pixelAppearanceVerified=$true;Reject $pixels 'pixel claim'
      $native=New-Value;$native.forms[2].reopen.nativePdfSemanticVerified=$true;Reject $native 'native semantic claim'
      $selected=New-Value;$selected.comments.reopen.selectedTextHighlightVerified=$true;Reject $selected 'selected text claim'
      $wrong=New-Value;$wrong.forms[0].write.sourceValues='guessed';Reject $wrong 'guessed source value'
      $duplicate=New-Value;$duplicate.forms[2].kind='radio';$duplicate.forms[2].write.kind='radio';$duplicate.forms[2].reopen.kind='radio';Reject $duplicate 'duplicate form kind'
      $short=New-Value;$short.forms=@($short.forms[0],$short.forms[1],$short.forms[2]);Reject $short 'missing multi-select and combo fixture'
      $wrongMulti=New-Value;$wrongMulti.forms[3].reopen.initial='Delivery|Handled by ACME|Rush';Reject $wrongMulti 'partial multi-select reopen value'
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

  it('drives every comments/forms step through one polled helper with no one-shot WebDriver action left', () => {
    const source = readFileSync(verifierPath, 'utf8');
    const stages = source.slice(source.indexOf('function Invoke-CommentsFormsPoll'), source.indexOf('function Invoke-InstalledCommentsFormsRuntime'));
    expect(stages.match(/Invoke-WebDriverScript/g)?.length).toBe(1);
    expect(source).not.toContain("throw 'Exact sticky-comment creation controls were unavailable.'");
    expect(source).toContain("-ButtonLabel 'Save a copy' -ButtonMatch 'aria-label'");
    expect(source).toContain("-ButtonLabel 'Save filled copy' -ButtonMatch 'text'");
    const result = runPowerShell(extractFunctions(['Invoke-CommentsFormsPoll'], String.raw`
      $script:calls=0;$script:replies=@()
      function Invoke-WebDriverScript{param($SessionId,$Deadline,$Script)$script:calls++;$reply=$script:replies[[Math]::Min($script:calls,$script:replies.Count)-1];return ($reply|ConvertFrom-Json)}
      $allowed=@('page-image','comment-layer','enabled-button')
      $script:replies=@('{"done":false,"missing":["page-image","comment-layer"]}','{"done":false,"missing":["comment-layer"]}','{"done":false,"missing":["enabled-button"]}','{"done":true,"missing":[],"value":"x"}')
      $value=Invoke-CommentsFormsPoll -SessionId 's' -Deadline ([datetime]::UtcNow.AddSeconds(20)) -Script 'probe' -Allowed $allowed -Kind 'Exact sticky-comment creation controls';if($script:calls-ne4-or$value.value-cne'x'){throw ('Expected four polls and the final value, saw '+$script:calls)}
      $script:calls=0;$script:replies=@('{"done":false,"missing":["enabled-button"]}');$message=$null
      try{$null=Invoke-CommentsFormsPoll -SessionId 's' -Deadline ([datetime]::UtcNow.AddMilliseconds(900)) -Script 'probe' -Allowed $allowed -Kind 'Exact area-highlight creation controls'}catch{$message=$_.Exception.Message}
      if($message-cne'Exact area-highlight creation controls did not become ready before the session deadline. Missing: enabled-button.'){throw ('Unexpected timeout message: '+$message)};if($script:calls-lt2){throw 'Timeout path did not poll.'}
      foreach($bad in @('{"done":false,"missing":[]}','{"done":true,"missing":["page-image"]}','{"done":false,"missing":["C:\\Users\\x"]}','{"done":"yes","missing":[]}','{"missing":[]}','null')){$script:calls=0;$script:replies=@($bad);$message=$null;try{$null=Invoke-CommentsFormsPoll -SessionId 's' -Deadline ([datetime]::UtcNow.AddSeconds(5)) -Script 'probe' -Allowed $allowed -Kind 'Probe'}catch{$message=$_.Exception.Message};if($message-cne'Probe probe returned an unexpected shape.'){throw ('Bad probe reply accepted: '+$bad+' -> '+$message)}}
      'PASS'
    `));
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  });

  it('runs the generated WebDriver scripts against the 96b0521 control shapes', () => {
    const names = ['Get-CommentsFormsSaveControlScript', 'Get-CommentsPageOneScript', 'Get-CommentsCreationControlScript', 'Get-CommentsPlacementScript', 'Get-CommentsEditorSaveScript', 'Get-CommentsReopenScript', 'Get-FormSpec', 'Get-FormSessionScript'];
    const result = runPowerShell(extractFunctions(names, String.raw`
      $out=[ordered]@{saveAria=Get-CommentsFormsSaveControlScript 'Save a copy' 'aria-label';saveText=Get-CommentsFormsSaveControlScript 'Save filled copy' 'text';create=Get-CommentsCreationControlScript 'Add comment';place=Get-CommentsPlacementScript 'Comment text';edit=Get-CommentsEditorSaveScript 'Comment text' 'Installed sticky receipt' 'Save comment';reopen=Get-CommentsReopenScript}
      foreach($k in 'mixed','radio','choice','multicombo'){foreach($p in 'read','reopen','write'){$out["$k-$p"]=Get-FormSessionScript $k $p}}
      [Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);[Console]::Out.Write(($out|ConvertTo-Json -Compress))
    `));
    expect(result.status, result.stderr).toBe(0);
    const scripts: Record<string, string> = JSON.parse(result.stdout);
    class FakeEvent { constructor(public type: string) {} }
    class Input { attrs: Record<string, string>; events: string[] = []; _value = ''; checked = false; disabled = false; type: string; clicks = 0; group?: Input[]; textContent = '';
      constructor(attrs: Record<string, string>, type = 'text') { this.attrs = attrs; this.type = type; }
      getAttribute(name: string) { return this.attrs[name] ?? null; }
      dispatchEvent(event: FakeEvent) { this.events.push(event.type); return true; }
      click() { this.clicks++; if (this.type === 'checkbox') this.checked = !this.checked; if (this.type === 'radio') for (const peer of this.group ?? [this]) peer.checked = peer === this; }
    }
    Object.defineProperty(Input.prototype, 'value', { get(this: Input) { return this._value; }, set(this: Input, value: string) { this._value = value; } });
    class Select extends Input { options: { value: string; textContent: string }[]; selectedIndex: number;
      constructor(attrs: Record<string, string>, options: [string, string][], selected: number) { super(attrs, 'select'); this.options = [{ value: '', textContent: 'Choose an option' }, ...options.map(([value, textContent]) => ({ value, textContent }))]; this.selectedIndex = selected + 1; }
    }
    Object.defineProperty(Select.prototype, 'value', { get(this: Select) { return this.options[this.selectedIndex]?.value ?? ''; }, set(this: Select, value: string) { this.selectedIndex = this.options.findIndex(x => x.value === value); } });
    const run = (script: string, document: unknown) => new Function('document', 'HTMLInputElement', 'HTMLSelectElement', 'HTMLTextAreaElement', 'Event', 'PointerEvent', script)(document, Input, Select, Input, FakeEvent, FakeEvent);
    const page = (controls: Input[], radios: Input[] = [], open = true) => { const fill = new Input({ 'aria-label': 'Fill existing fields' }, 'button'); const dialog = { querySelectorAll: (selector: string) => selector === 'input,select' ? controls : selector === 'input[type=radio]' ? radios : [] };
      const doc = { opened: open, fill, querySelector: (selector: string) => selector === 'dialog[aria-labelledby="fill-forms-title"]' && doc.opened ? dialog : null, querySelectorAll: (selector: string) => selector === 'button' ? [fill] : [] }; return doc; };

    const closed = page([], [], false);
    expect(run(scripts['mixed-read'], closed)).toEqual({ done: false, missing: ['form-dialog'] });
    expect(closed.fill.clicks).toBe(1);
    expect(run(scripts['mixed-read'], page([]))).toEqual({ done: false, missing: ['Name', 'Consent, page 1'] });

    const name = new Input({ 'aria-label': 'Name' }); name.value = 'Original'; const consent = new Input({ 'aria-label': 'Consent, page 1' }, 'checkbox');
    const mixed = page([name, consent]);
    expect(run(scripts['mixed-read'], mixed)).toEqual({ done: true, missing: [], value: 'Original|false' });
    expect(run(scripts['mixed-reopen'], mixed)).toEqual({ done: false, missing: ['reopened-values'] });
    expect(run(scripts['mixed-write'], mixed)).toEqual({ done: false, missing: ['field-values'] });
    expect(run(scripts['mixed-write'], mixed)).toEqual({ done: true, missing: [], value: 'Installed Mixed|true' });
    expect([name.events, consent.clicks]).toEqual([['input'], 1]);
    expect(run(scripts['mixed-reopen'], mixed)).toEqual({ done: true, missing: [], value: 'Installed Mixed|true' });
    name.disabled = true; expect(run(scripts['mixed-read'], mixed)).toEqual({ done: false, missing: ['enabled-fields'] });

    const radios = ['A', 'B', 'C'].map(x => new Input({ 'aria-label': `Preference: Choice ${x}, page 1` }, 'radio')); radios.forEach(x => { x.group = radios; }); radios[1].checked = true;
    const radio = page(radios, radios);
    expect(run(scripts['radio-read'], radio)).toEqual({ done: true, missing: [], value: 'Preference: Choice B, page 1' });
    expect(run(scripts['radio-write'], radio).done).toBe(false);
    expect(run(scripts['radio-write'], radio)).toEqual({ done: true, missing: [], value: 'Preference: Choice A, page 1' });

    const destination = new Select({ 'aria-label': 'Destination, page 1' }, [['choice-7-0-0', 'North Hub'], ['choice-7-0-1', 'South Hub'], ['choice-7-0-2', 'Local Hub']], 1);
    const delivery = new Select({ 'aria-label': 'Delivery, page 1' }, [['choice-8-0-0', 'Print copy'], ['choice-8-0-1', 'Email copy'], ['choice-8-0-2', 'Local copy']], 1);
    const choice = page([destination, delivery]);
    expect(run(scripts['choice-read'], choice)).toEqual({ done: true, missing: [], value: 'South Hub|Email copy' });
    expect(run(scripts['choice-write'], choice).done).toBe(false);
    expect(run(scripts['choice-write'], choice)).toEqual({ done: true, missing: [], value: 'North Hub|Print copy' });
    expect([destination.value, delivery.value, destination.events, delivery.events]).toEqual(['choice-7-0-0', 'choice-8-0-0', ['change'], ['change']]);

    const service = (label: string, checked = false) => { const box = new Input({ 'aria-label': `Services: ${label}, page 1` }, 'checkbox'); box.checked = checked; return box; };
    const [pickup, deliveryService, storage, insurance] = [service('Pickup', true), service('Delivery'), service('Storage'), service('Insurance')];
    const carrier = new Input({ 'aria-label': 'Carrier, page 1' }); carrier.value = 'Atlas Freight'; const priority = new Input({ 'aria-label': 'Priority, page 1' }); priority.value = 'Standard';
    const multi = page([pickup, deliveryService, storage, insurance, carrier, priority]);
    expect(run(scripts['multicombo-read'], page([pickup, deliveryService, storage, insurance, carrier]))).toEqual({ done: false, missing: ['Priority, page 1'] });
    expect(run(scripts['multicombo-read'], multi)).toEqual({ done: true, missing: [], value: 'Pickup|Atlas Freight|Standard' });
    expect(run(scripts['multicombo-reopen'], multi)).toEqual({ done: false, missing: ['reopened-values'] });
    expect(run(scripts['multicombo-write'], multi)).toEqual({ done: false, missing: ['field-values'] });
    expect([pickup.checked, deliveryService.checked, storage.checked, insurance.checked]).toEqual([false, true, false, true]);
    expect([pickup.clicks, deliveryService.clicks, storage.clicks, insurance.clicks]).toEqual([1, 1, 0, 1]);
    expect([carrier.value, priority.value, carrier.events, priority.events]).toEqual(['Handled by ACME', 'Rush', ['input'], ['input']]);
    expect(run(scripts['multicombo-write'], multi)).toEqual({ done: true, missing: [], value: 'Delivery,Insurance|Handled by ACME|Rush' });
    expect([pickup.clicks, deliveryService.clicks, insurance.clicks, carrier.events.length, priority.events.length]).toEqual([1, 1, 1, 1, 1]);
    expect(run(scripts['multicombo-reopen'], multi)).toEqual({ done: true, missing: [], value: 'Delivery,Insurance|Handled by ACME|Rush' });
    storage.checked = true; expect(run(scripts['multicombo-reopen'], multi)).toEqual({ done: false, missing: ['reopened-values'] }); storage.checked = false;
    carrier.disabled = true; expect(run(scripts['multicombo-read'], multi)).toEqual({ done: false, missing: ['enabled-fields'] });

    const iconSave = new Input({ 'aria-label': 'Save a copy' }, 'button'); const filled = new Input({}, 'button'); filled.textContent = ' Save filled copy ';
    const buttons = (list: Input[]) => ({ querySelectorAll: (selector: string) => selector === 'button' ? list : [], querySelector: () => null });
    iconSave.disabled = true; expect(run(scripts.saveAria, buttons([iconSave]))).toEqual({ done: false, missing: ['enabled-save-control'] });
    iconSave.disabled = false; expect(run(scripts.saveAria, buttons([iconSave]))).toEqual({ done: true, missing: [] }); expect(iconSave.clicks).toBe(1);
    expect(run(scripts.saveText, buttons([iconSave]))).toEqual({ done: false, missing: ['save-control'] });
    expect(run(scripts.saveText, buttons([filled]))).toEqual({ done: true, missing: [] });

    const comments = new Input({ 'aria-label': 'Comments' }, 'button'); let aside: { querySelectorAll: () => { textContent: string }[] } | null = null;
    const reopenDoc = { querySelector: (selector: string) => selector === 'aside[aria-label="Comments"]' ? aside : null, querySelectorAll: (selector: string) => selector === 'button' ? [comments] : [] };
    expect(run(scripts.reopen, reopenDoc)).toEqual({ sticky: 0, area: 0 }); expect(comments.clicks).toBe(1);
    aside = { querySelectorAll: () => [{ textContent: 'Comment · Page 1Installed sticky receipt' }, { textContent: 'Highlight · Page 1Installed area receipt' }] };
    expect(run(scripts.reopen, reopenDoc)).toEqual({ sticky: 1, area: 1 }); expect(comments.clicks).toBe(1);

    const layer = new Input({ 'data-testid': 'comment-layer' }, 'div'); const image = { getBoundingClientRect: () => ({ left: 0, top: 0, width: 100, height: 200 }), parentElement: { querySelector: (selector: string) => selector === ':scope>[data-testid="comment-layer"]' ? layer : null } };
    const add = new Input({ 'aria-label': 'Add comment' }, 'button'); let editor: Input | null = null; let save = new Input({}, 'button'); save.textContent = 'Save comment';
    const commentDoc = { querySelector: (selector: string) => selector === 'img[alt="Page 1"]' ? image : null, querySelectorAll: (selector: string) => selector === 'button' ? [add, save] : selector === 'textarea' || selector === 'textarea[aria-label="Comment text"]' ? (editor ? [editor] : []) : [] };
    expect(run(scripts.create, { querySelector: () => null, querySelectorAll: () => [add] })).toEqual({ done: false, missing: ['page-image', 'comment-layer'] });
    expect(run(scripts.create, commentDoc)).toEqual({ done: true, missing: [] }); expect(add.clicks).toBe(1);
    expect(run(scripts.place, commentDoc)).toEqual({ done: false, missing: ['editor'] }); expect(layer.events).toEqual(['pointerdown']);
    editor = new Input({ 'aria-label': 'Comment text' }, 'textarea');
    expect(run(scripts.place, commentDoc)).toEqual({ done: true, missing: [] }); expect(layer.events.length).toBe(1);
    expect(run(scripts.edit, commentDoc)).toEqual({ done: false, missing: ['typed-text'] }); expect(save.clicks).toBe(0);
    save.disabled = true; expect(run(scripts.edit, commentDoc)).toEqual({ done: false, missing: ['enabled-save-button'] });
    save.disabled = false; expect(run(scripts.edit, commentDoc)).toEqual({ done: true, missing: [] }); expect([editor.value, save.clicks]).toEqual(['Installed sticky receipt', 1]);
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
