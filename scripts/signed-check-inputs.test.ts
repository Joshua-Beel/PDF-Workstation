import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';
import { describe, expect, it } from 'vitest';
import { REV_NEW, guarded, preamble, psRoot, runBlocks, runPwsh, signedInputsSnippet } from './signed-check-test-support';

const workflows = [
  'expanded-page-tools-verification',
  'ocr-installer-upgrade',
  'persistence-verification',
  'print-dialog-verification',
  'reading-tools-verification',
];
const inputNames = [
  'target_version', 'source_revision', 'artifact_run_id', 'artifact_id', 'artifact_name', 'artifact_bytes',
  'artifact_digest', 'installer_name', 'installer_bytes', 'installer_sha256', 'record_bytes', 'record_sha256',
];

describe('signed installed checks take the artifact from dispatch inputs', () => {
  for (const name of workflows) {
    it(`${name} declares the twelve inputs, validates them first, and never interpolates them into scripts`, () => {
      const workflow = readFileSync(`.github/workflows/${name}.yml`, 'utf8');
      for (const input of inputNames) expect(workflow).toContain(`      ${input}: { required: true, type: string }`);
      expect(workflow).toMatch(/^permissions:\r?\n  contents: read\r?\n  actions: read\r?\n/m);
      expect(workflow).not.toMatch(/secrets\.|AZURE_|TAURI_SIGNING|gh release/i);
      expect(workflow).toContain('./scripts/assert-signed-artifact-metadata.ps1');
      expect(workflow).toContain('artifact-ids: ${{ inputs.artifact_id }}');
      expect(workflow).toContain('run-id: ${{ inputs.artifact_run_id }}');
      expect(workflow).toContain('Get-SignedCheckInputs -TargetVersion $env:TARGET_VERSION -SourceRevision $env:SOURCE_REVISION');
      expect(workflow).not.toMatch(/\b(?:11005152678|36499724415|12510717103|39969839829)\b/);
      for (const run of runBlocks(workflow)) expect(run).not.toContain('${{');
      const jobEnv = workflow.slice(workflow.indexOf('    env:'), workflow.indexOf('    steps:'));
      for (const input of inputNames) expect(jobEnv).toContain(`\${{ inputs.${input} }}`);
      expect(jobEnv).not.toMatch(/GITHUB_TOKEN|github\.token/);
      expect(workflow.indexOf('Get-SignedCheckInputs')).toBeLessThan(workflow.indexOf('assert-signed-artifact-metadata.ps1'));
    });
  }

  it('the verifiers hard-code no artifact id, run id, source revision, blob, or installer hash', () => {
    for (const file of ['verify-signed-expanded-page-tools', 'verify-signed-ocr-upgrade', 'verify-signed-persistence', 'verify-signed-print-dialog', 'verify-signed-reading-tools']) {
      const text = readFileSync(`scripts/${file}.ps1`, 'utf8');
      expect(text, file).not.toMatch(/\b(?:11005152678|36499724415|12510717103|39969839829)\b/);
      expect(text.match(/'[a-f0-9]{40}'/g), file).toBeNull();
      expect(text, file).not.toMatch(/'(?:3DE99F11|8F7A167D|0383DAE8|4ED82057)/);
      expect(text, file).toContain('Get-SignedCheckInputs');
    }
  });

  it('rejects wrong or mixed inputs', () => {
    const cases: Array<[string, Record<string, string>]> = [
      ['version without patch', { TargetVersion: '0.3' }],
      ['version with prerelease', { TargetVersion: '0.3.0-rc1' }],
      ['uppercase revision', { SourceRevision: REV_NEW.toUpperCase(), ArtifactName: `signed-ocr-installer-${REV_NEW.toUpperCase()}` }],
      ['short revision', { SourceRevision: REV_NEW.slice(0, 12), ArtifactName: `signed-ocr-installer-${REV_NEW.slice(0, 12)}` }],
      ['artifact name from another revision', { ArtifactName: `signed-ocr-installer-${'1'.repeat(40)}` }],
      ['digest without sha256 prefix', { ArtifactDigest: 'a'.repeat(64) }],
      ['uppercase digest', { ArtifactDigest: `sha256:${'A'.repeat(64)}` }],
      ['installer name for another version', { InstallerName: 'PDF Workstation_0.2.6_x64-setup.exe' }],
      ['installer name with a path', { InstallerName: '..\\PDF Workstation_0.3.0_x64-setup.exe' }],
      ['lowercase installer hash', { InstallerSha256: 'b'.repeat(64) }],
      ['short record hash', { RecordSha256: 'C'.repeat(63) }],
      ['zero bytes', { InstallerBytes: '0' }],
      ['leading-zero run id', { ArtifactRunId: '039969839829' }],
      ['exponent artifact id', { ArtifactId: '1e10' }],
      ['signed artifact bytes', { ArtifactBytes: '-5' }],
      ['whitespace record bytes', { RecordBytes: ' 7715' }],
      ['over-long number', { ArtifactBytes: '99999999999999999999999' }],
    ];
    for (const [label, overrides] of cases) {
      const result = runPwsh(String.raw`${preamble}
        ${guarded(signedInputsSnippet(REV_NEW, overrides))}
        exit 0
      `);
      expect(result.status, `${label} must be rejected`).toBe(1);
    }
    const valid = runPwsh(String.raw`${preamble}
      ${guarded(signedInputsSnippet(REV_NEW))}
      if ($inputs.ArtifactId -ne [uint64]12510717103 -or $inputs.InstallerBytes -ne [uint64]11201296) { throw 'Typed values changed.' }
    `);
    expect(valid.status, valid.stderr).toBe(0);
  }, 120_000);

  it('derives the base resource targets from the tagged tauri.conf.json and refuses a different checkout', () => {
    const root = resolve(`target/signed-inputs-base-${randomUUID()}`);
    const git = (...args: string[]) => {
      const result = spawnSync('git', ['-C', root, ...args], { encoding: 'utf8' });
      expect(result.status, result.stderr).toBe(0);
      return result.stdout.trim();
    };
    mkdirSync(resolve(root, 'src-tauri/resources/pdfium/licenses'), { recursive: true });
    mkdirSync(resolve(root, 'src-tauri/resources/third-party-sources'), { recursive: true });
    writeFileSync(resolve(root, 'src-tauri/tauri.conf.json'), JSON.stringify({
      version: '9.8.7',
      bundle: { resources: ['resources/welcome.pdf', 'resources/pdfium/licenses/*', 'resources/third-party-sources/*'] },
    }));
    writeFileSync(resolve(root, 'src-tauri/resources/welcome.pdf'), 'a');
    for (const name of ['one.txt', 'two.txt']) writeFileSync(resolve(root, `src-tauri/resources/pdfium/licenses/${name}`), name);
    writeFileSync(resolve(root, 'src-tauri/resources/third-party-sources/src.tar'), 'x');
    spawnSync('git', ['init', '-q', root], { encoding: 'utf8' });
    git('add', '--', '.');
    git('-c', 'user.name=t', '-c', 'user.email=t@example.invalid', 'commit', '-q', '-m', 'fixture');
    const revision = git('rev-parse', 'HEAD');
    const run = (overrides: Record<string, string>, expectExpression: string) => runPwsh(String.raw`${preamble}
      ${guarded(String.raw`
      ${signedInputsSnippet(revision, { TargetVersion: '9.8.7', InstallerName: 'PDF Workstation_9.8.7_x64-setup.exe', ...overrides })}
      $targets = @(Get-SignedBaseResourceTargets -TargetSourceRoot '${root.replaceAll("'", "''")}' -Inputs $inputs)
      ${expectExpression}
      `)}
    `);
    const ok = run({}, "if ($targets.Count -ne 4 -or ($targets -join ',') -cne 'resources/pdfium/licenses/one.txt,resources/pdfium/licenses/two.txt,resources/third-party-sources/src.tar,resources/welcome.pdf') { throw ('Unexpected targets: ' + ($targets -join ',')) }");
    expect(ok.status, ok.stderr || ok.stdout).toBe(0);
    const wrongVersion = run({ TargetVersion: '9.8.8', InstallerName: 'PDF Workstation_9.8.8_x64-setup.exe' }, '');
    expect(wrongVersion.status).toBe(1);
    expect(wrongVersion.stderr).toContain('does not match the target version');
    const otherHead = runPwsh(String.raw`${preamble}
      ${guarded(String.raw`
      ${signedInputsSnippet(REV_NEW, { TargetVersion: '9.8.7', InstallerName: 'PDF Workstation_9.8.7_x64-setup.exe' })}
      Get-SignedBaseResourceTargets -TargetSourceRoot '${root.replaceAll("'", "''")}' -Inputs $inputs | Out-Null
      `)}
    `);
    expect(otherHead.status).toBe(1);
    expect(otherHead.stderr).toContain('not the signed source revision');
  }, 120_000);

  it('adds the same pin fields every verifier reads, including run-time leak ids', () => {
    const result = runPwsh(String.raw`${preamble}
      ${guarded(String.raw`
      ${signedInputsSnippet(REV_NEW)}
      $pins = [ordered]@{ Repository = 'x' }
      Add-SignedPinFields -Pins $pins -Inputs $inputs -BaseResourceTargets @('b','a')
      foreach ($key in @('TargetVersion','SignedArtifactId','SignedRunId','SignedSourceRevision','SignedInstallerName','SignedInstallerBytes','SignedInstallerSha256','SignedRecordName','SignedRecordBytes','SignedRecordSha256','LeakIds','BaseResourceTargets')) { if (-not $pins.Contains($key)) { throw "Missing $key" } }
      if ($pins.LeakIds -cne '12510717103|39969839829') { throw 'Leak ids must be the input ids.' }
      if ('12510717103' -cnotmatch ('(?i)(' + $pins.LeakIds + ')')) { throw 'Leak regex must match the input id.' }
      `)}
    `);
    expect(result.status, result.stderr).toBe(0);
    expect(psRoot().length).toBeGreaterThan(0);
  });
});
