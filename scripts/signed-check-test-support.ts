import { spawnSync } from 'node:child_process';

export const REV_NEW = '4a0f89c5b2f04004c77adbdd7ccee8442dd1c41d';
export const REV_OLD = '67d238218f4796ba7b8505d072868da0f397174a';
export const REV_PREFS_OLD = '11e5c2664f8d32353d08b824b06f6cdb66c447fe';

export function psRoot() {
  return process.cwd().replaceAll("'", "''");
}

export function runPwsh(script: string, timeout = 45_000) {
  return spawnSync('pwsh.exe', ['-NoProfile', '-NonInteractive', '-Command', script], { cwd: process.cwd(), encoding: 'utf8', timeout });
}

export function signedInputsSnippet(revision = REV_NEW, overrides: Record<string, string> = {}) {
  const values: Record<string, string> = {
    TargetVersion: '0.3.0',
    SourceRevision: revision,
    ArtifactRunId: '39969839829',
    ArtifactId: '12510717103',
    ArtifactName: `signed-ocr-installer-${revision}`,
    ArtifactBytes: '11182480',
    ArtifactDigest: `sha256:${'a'.repeat(64)}`,
    InstallerName: 'PDF Workstation_0.3.0_x64-setup.exe',
    InstallerBytes: '11201296',
    InstallerSha256: 'B'.repeat(64),
    RecordBytes: '7715',
    RecordSha256: 'C'.repeat(64),
    ...overrides,
  };
  const args = Object.entries(values).map(([key, value]) => `-${key} '${value.replaceAll("'", "''")}'`).join(' ');
  return String.raw`
    . '${psRoot()}\scripts\signed-check-inputs.ps1'
    $inputs = Get-SignedCheckInputs ${args}
  `;
}

export function importFunctions(scriptName: string, pinsVariable: string, functionNames: string[]) {
  return String.raw`
    $path=Join-Path '${psRoot()}' 'scripts/${scriptName}';$tokens=$null;$errors=$null
    $ast=[Management.Automation.Language.Parser]::ParseFile($path,[ref]$tokens,[ref]$errors);if($errors.Count){throw ($errors|Out-String)}
    $assignment=$ast.Find({param($node)$node-is[Management.Automation.Language.AssignmentStatementAst]-and$node.Left.Extent.Text-ceq'${pinsVariable}'},$true)
    Invoke-Expression $assignment.Extent.Text
    foreach($name in @(${functionNames.map((name) => `'${name}'`).join(',')})){$definition=$ast.Find({param($node)$node-is[Management.Automation.Language.FunctionDefinitionAst]-and$node.Name-ceq$name},$true);if(-not$definition){throw "Missing $name"};Invoke-Expression $definition.Extent.Text}
  `;
}

export const preamble = String.raw`$ErrorActionPreference='Stop';Set-StrictMode -Version Latest;Set-Location -LiteralPath '${psRoot()}'`;

export function guarded(body: string) {
  return String.raw`try { ${body} } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 1 }`;
}

export function runBlocks(workflow: string) {
  const lines = workflow.split(/\r?\n/);
  const blocks: string[] = [];
  for (let index = 0; index < lines.length; index += 1) {
    const match = /^(\s*)run:\s*(.*)$/.exec(lines[index]);
    if (!match) continue;
    const indent = match[1].length;
    const body = [match[2]];
    while (index + 1 < lines.length && (lines[index + 1].trim() === '' || lines[index + 1].search(/\S/) > indent)) {
      index += 1;
      body.push(lines[index]);
    }
    blocks.push(body.join('\n'));
  }
  return blocks;
}
