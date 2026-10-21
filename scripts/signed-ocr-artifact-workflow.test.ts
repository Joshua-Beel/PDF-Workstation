import { describe, expect, it } from 'vitest';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';

function runPowerShell(source: string) {
  const root = `target/ocr-export-test-script-${randomUUID()}`;
  mkdirSync(root, { recursive: true });
  const path = `${root}/run.ps1`;
  writeFileSync(path, source, 'utf8');
  return spawnSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-File', path], {
    encoding: 'utf8',
    timeout: 45_000,
  });
}

describe('manual signed OCR artifact workflow', () => {
  it('is manual, master-bound, read-only, exact-revision, and isolated from releases and updater secrets', () => {
    const workflow = readFileSync('.github/workflows/ocr-signed-artifact.yml', 'utf8').replace(/\r\n/g, '\n');
    const release = readFileSync('.github/workflows/release.yml', 'utf8');
    const builder = readFileSync('scripts/build-installer.ps1', 'utf8');

    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).toContain('runs-on: windows-2022');
    expect(workflow).not.toContain('runs-on: windows-latest');
    expect(workflow).not.toMatch(/^\s*push:/m);
    expect(workflow).toContain('expected_publisher:');
    expect(workflow).toContain('required: true');
    expect(workflow).toContain('contents: read');
    expect(workflow).toContain("$env:GITHUB_REF -cne 'refs/heads/master'");
    expect(workflow).toContain('ref: ${{ github.sha }}');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('$headRevision -cne $env:GITHUB_SHA');
    expect(workflow.indexOf('Require a manual master dispatch')).toBeLessThan(workflow.indexOf('AZURE_TENANT_ID'));
    expect(workflow.indexOf('cargo fetch --locked')).toBeLessThan(workflow.indexOf('npm test'));
    expect(workflow).toContain('- run: npm test -- --pool=threads --maxWorkers=1 --no-file-parallelism');
    expect(workflow).toContain('- run: ./scripts/run-native-recovery-gates.ps1\n        shell: pwsh');
    expect(workflow).not.toMatch(/run: cargo test/);

    const secretNames = [...workflow.matchAll(/secrets\.(AZURE_[A-Z_]+)/g)].map((match) => match[1]);
    expect(new Set(secretNames)).toEqual(new Set([
      'AZURE_TENANT_ID',
      'AZURE_CLIENT_ID',
      'AZURE_CLIENT_SECRET',
      'AZURE_SIGNING_ENDPOINT',
      'AZURE_SIGNING_ACCOUNT',
      'AZURE_SIGNING_PROFILE',
    ]));
    expect(secretNames).toHaveLength(6);
    expect(workflow).not.toContain('TAURI_SIGNING_PRIVATE_KEY');
    expect(workflow).not.toContain('GH_TOKEN');
    expect(workflow).not.toContain('gh release');
    expect(workflow).not.toContain('Create draft release');
    expect(workflow).toContain('-ArtifactOnly');
    expect(workflow).toContain('-SourceRevision $env:GITHUB_SHA');
    expect(workflow).toContain('${{ env.EXPORT_ROOT }}/*_x64-setup.exe');
    expect(workflow).toContain('${{ env.EXPORT_ROOT }}/artifact-verification.json');
    expect(workflow.indexOf('actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02')).toBeGreaterThan(0);
    expect(workflow.slice(workflow.indexOf('actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02'))).not.toContain('BUILD_PROOF_ROOT');

    expect(builder).toContain("if ($SourceRevision -and -not ($SignedOcr -or $UnsignedLocal))");
    expect(builder).toContain('SourceRevision does not match the current git HEAD.');
    expect(builder).toContain('sourceRevision = $SourceRevision');
    expect(release).not.toContain('ocr-signed-artifact');
    expect(release).not.toContain('ArtifactOnly');
  });

  it('exports only a receipt-bound installer and sanitized record and rejects unsafe or incomplete proofs', () => {
    const check = String.raw`
      $ErrorActionPreference = 'Stop'
      Set-StrictMode -Version Latest
      Set-Location -LiteralPath '${process.cwd().replaceAll("'", "''")}'
      . ./scripts/ocr/installer-package.ps1
      . ./scripts/ocr/windows-signing.ps1
      $scriptPath = (Resolve-Path ./scripts/export-signed-ocr-artifact.ps1).Path
      $tokens=$null;$errors=$null
      $ast=[Management.Automation.Language.Parser]::ParseFile($scriptPath,[ref]$tokens,[ref]$errors)
      if($errors.Count){throw 'Exporter script did not parse.'}
      foreach($name in @('Assert-ExactJsonProperties','Assert-ReceiptValue','Resolve-ArtifactRoot','Resolve-SafeReceiptFile','Assert-LiveReceiptFile','Close-ArtifactReadLocks','Export-SignedOcrArtifact')){
        $function=$ast.Find({param($node)$node -is [Management.Automation.Language.FunctionDefinitionAst] -and $node.Name -ceq $name},$true)
        if(-not $function){throw ('Missing exporter function: '+$name)}
        Invoke-Expression $function.Extent.Text
      }

      $head=(git rev-parse HEAD).Trim()
      if($LASTEXITCODE -ne 0 -or $head -cnotmatch '^[a-f0-9]{40}$'){throw 'Could not resolve test HEAD.'}
      $publisher='Verified Test Publisher'
      $signatureProvider={param($path)[pscustomobject]@{Status='Valid';Publisher=$publisher;HasTimestamp=$true}}.GetNewClosure()
      $target=(Resolve-Path target).Path
      $secretSentinel='AZURE_ACCOUNT_SENTINEL_MUST_NOT_EXPORT'

      function Write-Bytes {
        param([string]$Path,[string]$Text)
        [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($Path))|Out-Null
        [IO.File]::WriteAllBytes($Path,[Text.Encoding]::UTF8.GetBytes($Text))
      }
      function Get-TextSha256 {
        param([string]$Text)
        $algorithm=[Security.Cryptography.SHA256]::Create()
        try{return ([BitConverter]::ToString($algorithm.ComputeHash([Text.Encoding]::UTF8.GetBytes($Text)))).Replace('-','')}
        finally{$algorithm.Dispose()}
      }
      function New-FileReceipt {
        param([string]$Path)
        $item=Get-Item -LiteralPath $Path
        return [ordered]@{path=$null;bytes=[uint64]$item.Length;sha256=Get-ExactSha256 -Path $Path}
      }
      function Save-Receipt {
        param([string]$Root,$Receipt)
        [IO.File]::WriteAllText((Join-Path $Root 'installer-verification.json'),($Receipt|ConvertTo-Json -Depth 20),[Text.UTF8Encoding]::new($false))
      }
      function New-Proof {
        param([string]$Name)
        $root=Join-Path $target ($Name+'-'+[Guid]::NewGuid().ToString('N'))
        [IO.Directory]::CreateDirectory($root)|Out-Null
        $installer=Join-Path $root 'cargo-target/release/bundle/nsis/PDF Workstation_0.2.6_x64-setup.exe'
        $application=Join-Path $root 'extracted-installer/pdf-workstation.exe'
        $inventory=Join-Path $root 'installer-inventory.txt'
        Write-Bytes $installer 'signed-installer'
        Write-Bytes $application 'signed-application'
        Write-Bytes $inventory 'verified inventory'
        $identity=(Get-TextSha256 'signed-engine').ToLowerInvariant()
        $baseSpecs=@(
          @('resources/pdfium/bin/pdfium.dll','signed-pdfium'),
          @('resources/welcome.pdf','welcome')
        )
        $base=@()
        $canonicalEntries=@()
        foreach($spec in $baseSpecs){
          $path=Join-Path $root ('extracted-installer/'+$spec[0]);Write-Bytes $path $spec[1]
          $entry=New-FileReceipt $path;$entry.path=$spec[0]
          if($spec[0] -ceq 'resources/pdfium/bin/pdfium.dll'){
            $entry.unsignedBytes=[uint64]7;$entry.unsignedSha256='B'*64
            $canonicalReceipt=[pscustomobject]@{Bytes=[uint64]7;Sha256=('B'*64)}
          }else{
            $canonicalReceipt=[pscustomobject]@{Bytes=[uint64]$entry.bytes;Sha256=[string]$entry.sha256}
          }
          $canonicalEntries+=[pscustomobject]@{Target=$spec[0];Receipt=$canonicalReceipt}
          $base+=[pscustomobject]$entry
        }
        $ocrSpecs=@(
          @('bin/tesseract.exe','engine','signed-engine'),
          @('tessdata/eng.traineddata','model','model'),
          @('licenses/Tesseract-Apache-2.0.txt','license','tesseract-license'),
          @('licenses/Leptonica-BSD-2-Clause.txt','license','leptonica-license'),
          @('licenses/eng-fast-Apache-2.0.txt','license','model-license')
        )
        $ocr=@()
        foreach($spec in $ocrSpecs){
          $relative='resources/ocr/'+$identity+'/'+$spec[0]
          $path=Join-Path $root ('extracted-installer/'+$relative);Write-Bytes $path $spec[2]
          $entry=New-FileReceipt $path;$entry.path=$relative;$entry.role=$spec[1]
          $ocr+=[pscustomobject]$entry
        }
        $installerReceipt=New-FileReceipt $installer;$installerReceipt.path='cargo-target/release/bundle/nsis/PDF Workstation_0.2.6_x64-setup.exe'
        $applicationReceipt=New-FileReceipt $application;$applicationReceipt.path='extracted-installer/pdf-workstation.exe'
        $inventoryReceipt=New-FileReceipt $inventory;$inventoryReceipt.path='installer-inventory.txt'
        $original=[ordered]@{bytes=[uint64]11;sha256='C'*64}
        $receipt=[ordered]@{
          schemaVersion=1
          scope='Artifact-only Azure OCR installer extraction proof; no install, launch, update, workflow, or release claim.'
          mode='azure-signed-artifact-only-ocr'
          sourceRevision=$head
          installer=[pscustomobject]$installerReceipt
          application=[pscustomobject]$applicationReceipt
          archiveInventory=[pscustomobject]$inventoryReceipt
          baseResources=$base
          ocr=[ordered]@{
            enabled=$true
            identity=$identity
            setupManifestSha256='D'*64
            originalEngine=$original
            packagedResources=$ocr
            licensesArePackagedSidecarsNotNoticeDialogContent=$true
          }
          signatures=[ordered]@{expectedPublisher=$publisher;installer='Valid';application='Valid';engine='Valid';originalEngine='NotSigned'}
          config=[ordered]@{ephemeral=$true;retained=$false;updaterArtifacts=@()}
        }
        [IO.Directory]::CreateDirectory((Join-Path $root 'logs'))|Out-Null
        [IO.File]::WriteAllText((Join-Path $root 'logs/raw.log'),$secretSentinel)
        Save-Receipt $root $receipt
        $canonical=[pscustomobject]@{Count=$canonicalEntries.Count;Entries=$canonicalEntries}
        return [pscustomobject]@{Root=$root;Receipt=$receipt;Installer=$installer;Identity=$identity;CanonicalBase=$canonical}
      }
      function Assert-Rejected {
        param([scriptblock]$Action,[string]$Kind)
        $rejected=$false
        try{& $Action}catch{$rejected=$true}
        if(-not $rejected){throw ($Kind+' was accepted.')}
      }
      function Invoke-Export {
        param($Proof,[string]$OutputName,[string]$Expected=$publisher)
        $canonical=$Proof.CanonicalBase
        $baseMapProvider={param($projectRoot)$canonical}.GetNewClosure()
        Export-SignedOcrArtifact -ProjectRoot (Get-Location) -SourceRoot $Proof.Root -OutputRoot (Join-Path $target ($OutputName+'-'+[Guid]::NewGuid().ToString('N'))) -ExpectedPublisher $Expected -SourceRevision $head -SignatureProvider $signatureProvider -BaseMapProvider $baseMapProvider
      }

      $success=New-Proof 'ocr-export-success'
      $result=Invoke-Export $success 'ocr-export-output'
      $outputFiles=@(Get-ChildItem -LiteralPath ([IO.Path]::GetDirectoryName($result.Record)) -File)
      if($outputFiles.Count -ne 2 -or -not (Test-Path -LiteralPath $result.Installer) -or -not (Test-Path -LiteralPath $result.Record)){throw 'Sanitized output file set changed.'}
      $recordText=Get-Content -LiteralPath $result.Record -Raw -Encoding UTF8
      $record=$recordText|ConvertFrom-Json
      if($record.sourceRevision -cne $head -or $record.installer.sha256 -cne (Get-ExactSha256 $result.Installer)){throw 'Sanitized output lost its source or installer binding.'}
      if($recordText.Contains($success.Root) -or $recordText.Contains($secretSentinel) -or $recordText.Contains($success.Identity) -or
         $recordText -match 'AZURE_(TENANT|CLIENT|SIGNING|ACCOUNT|PROFILE|ENDPOINT)' -or $recordText -match 'extracted-installer|cargo-target|signing-config|raw\.log'){
        throw 'Sanitized record leaked a source path, identifier, setup identity, configuration, or raw log.'
      }

      $traversal=New-Proof 'ocr-export-traversal';$traversal.Receipt.installer.path='../escape.exe';Save-Receipt $traversal.Root $traversal.Receipt
      Assert-Rejected {Invoke-Export $traversal 'ocr-export-traversal-out'} 'Traversal receipt'
      $alias=New-Proof 'ocr-export-alias';$alias.Receipt.installer.path='cargo-target/release/bundle/nsis/../nsis/PDF Workstation_0.2.6_x64-setup.exe';Save-Receipt $alias.Root $alias.Receipt
      Assert-Rejected {Invoke-Export $alias 'ocr-export-alias-out'} 'Aliased installer path'
      $extra=New-Proof 'ocr-export-extra';Write-Bytes (Join-Path $extra.Root 'cargo-target/release/bundle/nsis/extra.exe') 'extra';Save-Receipt $extra.Root $extra.Receipt
      Assert-Rejected {Invoke-Export $extra 'ocr-export-extra-out'} 'Unexpected bundle asset'
      $hidden=New-Proof 'ocr-export-hidden';$hiddenPath=Join-Path $hidden.Root 'cargo-target/release/bundle/nsis/hidden.exe';Write-Bytes $hiddenPath 'hidden';(Get-Item -LiteralPath $hiddenPath).Attributes=[IO.FileAttributes]::Hidden;Save-Receipt $hidden.Root $hidden.Receipt
      Assert-Rejected {Invoke-Export $hidden 'ocr-export-hidden-out'} 'Hidden bundle asset'
      foreach($updater in @('LATEST.JSON','update.SiG')){
        $proof=New-Proof 'ocr-export-updater';Write-Bytes (Join-Path $proof.Root $updater) 'updater';Save-Receipt $proof.Root $proof.Receipt
        Assert-Rejected {Invoke-Export $proof 'ocr-export-updater-out'} ('Updater artifact '+$updater)
      }
      $mismatch=New-Proof 'ocr-export-mismatch';$mismatch.Receipt.installer.sha256='E'*64;Save-Receipt $mismatch.Root $mismatch.Receipt
      Assert-Rejected {Invoke-Export $mismatch 'ocr-export-mismatch-out'} 'Mismatched installer hash'
      $incomplete=New-Proof 'ocr-export-incomplete';$incomplete.Receipt.application.PSObject.Properties.Remove('bytes');Save-Receipt $incomplete.Root $incomplete.Receipt
      Assert-Rejected {Invoke-Export $incomplete 'ocr-export-incomplete-out'} 'Incomplete application receipt'
      $missingBase=New-Proof 'ocr-export-missing-base';$missingBase.Receipt.baseResources=@($missingBase.Receipt.baseResources|Select-Object -Skip 1);Save-Receipt $missingBase.Root $missingBase.Receipt
      Assert-Rejected {Invoke-Export $missingBase 'ocr-export-missing-base-out'} 'Incomplete base-resource receipt'
      foreach($booleanCase in @('enabled','licenses','ephemeral')){
        $proof=New-Proof ('ocr-export-bool-'+$booleanCase)
        if($booleanCase -ceq 'enabled'){$proof.Receipt.ocr.enabled='false'}
        elseif($booleanCase -ceq 'licenses'){$proof.Receipt.ocr.licensesArePackagedSidecarsNotNoticeDialogContent='false'}
        else{$proof.Receipt.config.ephemeral='false'}
        Save-Receipt $proof.Root $proof.Receipt
        Assert-Rejected {Invoke-Export $proof 'ocr-export-bool-out'} ('Malformed boolean '+$booleanCase)
      }
      $identityMismatch=New-Proof 'ocr-export-identity';$engineEntry=@($identityMismatch.Receipt.ocr.packagedResources|Where-Object role -CEQ 'engine')[0]
      $enginePath=Join-Path $identityMismatch.Root ('extracted-installer/'+$engineEntry.path);Write-Bytes $enginePath 'different-signed-engine'
      $engineEntry.bytes=[uint64](Get-Item $enginePath).Length;$engineEntry.sha256=Get-ExactSha256 $enginePath;Save-Receipt $identityMismatch.Root $identityMismatch.Receipt
      Assert-Rejected {Invoke-Export $identityMismatch 'ocr-export-identity-out'} 'Mismatched OCR identity'
      $unknown=New-Proof 'ocr-export-unknown';$unknown.Receipt|Add-Member -NotePropertyName azureSigningAccount -NotePropertyValue $secretSentinel;Save-Receipt $unknown.Root $unknown.Receipt
      Assert-Rejected {Invoke-Export $unknown 'ocr-export-unknown-out'} 'Unknown receipt property'
      $wrongPublisher=New-Proof 'ocr-export-publisher';$wrongPublisher.Receipt.signatures.expectedPublisher='Someone Else';Save-Receipt $wrongPublisher.Root $wrongPublisher.Receipt
      Assert-Rejected {Invoke-Export $wrongPublisher 'ocr-export-publisher-out'} 'Wrong expected publisher'
      $wrongRevision=New-Proof 'ocr-export-revision';$wrongRevision.Receipt.sourceRevision='0'*40;Save-Receipt $wrongRevision.Root $wrongRevision.Receipt
      Assert-Rejected {Invoke-Export $wrongRevision 'ocr-export-revision-out'} 'Stale source revision'
      $stale=New-Proof 'ocr-export-stale';[IO.File]::SetLastWriteTimeUtc($stale.Installer,(Get-Date).ToUniversalTime().AddMinutes(5))
      Assert-Rejected {Invoke-Export $stale 'ocr-export-stale-out'} 'Stale verification receipt'
      $junction=New-Proof 'ocr-export-junction';$junctionPath=Join-Path $junction.Root ('extracted-installer/resources/ocr/'+$junction.Identity+'/licenses')
      $junctionTarget=Join-Path ([IO.Path]::GetTempPath()) ('smacrobat-ocr-junction-'+[Guid]::NewGuid().ToString('N'))
      Move-Item -LiteralPath $junctionPath -Destination $junctionTarget
      $null=New-Item -ItemType Junction -Path $junctionPath -Target $junctionTarget
      Save-Receipt $junction.Root $junction.Receipt
      Assert-Rejected {Invoke-Export $junction 'ocr-export-junction-out'} 'Extracted-resource junction'
      $outside=New-Proof 'ocr-export-outside'
      Assert-Rejected {Export-SignedOcrArtifact -ProjectRoot (Get-Location) -SourceRoot $outside.Root -OutputRoot (Join-Path (Get-Location) 'outside-export') -ExpectedPublisher $publisher -SourceRevision $head -SignatureProvider $signatureProvider} 'Outside-target output'

      Write-Output ('PASS '+$result.Record)
    `;
    const result = runPowerShell(check);
    expect(result.error).toBeUndefined();
    expect(result.status, result.stderr || result.stdout).toBe(0);
    expect(result.stdout).toContain('PASS');
  }, 50_000);
});
