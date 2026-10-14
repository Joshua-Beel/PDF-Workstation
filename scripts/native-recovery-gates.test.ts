import { mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';
import { expect, it } from 'vitest';

it('runs the isolated process and integration storage probes through the bounded receipt gate', () => {
  const helper = readFileSync('scripts/run-native-recovery-gates.ps1', 'utf8');
  expect(helper).toContain("Name = 'recovery_store::process_tests::cross_process_lock_serializes_admission_and_generations'; Target = @('--bin', 'pdf-workstation'); Ignored = $true");
  expect(helper).toContain("Name = 'service::tests::open_refuses_past_the_document_cap_and_non_files_without_loading_anything'; Target = @('--bin', 'pdf-workstation'); Ignored = $true");
  expect(helper).toContain("Name = 'aggregate_record_count_rejects_new_sources_but_allows_exact_replacement'; Target = @('--test', 'recovery_store'); Ignored = $false");
  expect(helper).toContain("if ([bool]$probe.Ignored) { $arguments += '--ignored' }");
  expect(helper).toContain("$arguments += @('--exact', '--test-threads=1')");
});

it('rejects a zero-test Cargo success receipt before accepting a required recovery probe', () => {
  const root = join('target', `native-recovery-gate-${randomUUID()}`);
  mkdirSync(root, { recursive: true });
  try {
    const fakeCargo = join(root, 'cargo.cmd');
    writeFileSync(fakeCargo, '@echo off\r\necho test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\r\nexit /b 0\r\n', 'utf8');
    const result = spawnSync('pwsh', ['-NoProfile', '-NonInteractive', '-File', 'scripts/run-native-recovery-gates.ps1', '-CargoPath', fakeCargo], { encoding: 'utf8', timeout: 50_000 });
    expect(result.error).toBeUndefined();
    expect(result.status).not.toBe(0);
    expect(`${result.stdout}\n${result.stderr}`).toContain('did not produce one exact passing receipt');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}, 60_000);
