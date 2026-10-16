import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const workflow = readFileSync('.github/workflows/windows-validation.yml', 'utf8').replace(/\r\n?/g, '\n');

describe('Windows validation workflow', () => {
  it('is manual, read-only, bounded, pinned, and exact-source bound', () => {
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('permissions:\n  contents: read');
    expect(workflow).toContain('runs-on: windows-latest');
    expect(workflow).toContain('timeout-minutes: 90');
    expect(workflow).toContain('ref: ${{ github.sha }}');
    expect(workflow).toContain('fetch-depth: 0');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('$head -cne $env:GITHUB_SHA');
    expect(workflow.match(/git diff --quiet --/g)).toHaveLength(2);
    expect(workflow.match(/git diff --cached --quiet --/g)).toHaveLength(2);
    expect(workflow).toContain('if: ${{ always() }}');
    expect(workflow).toContain('actions/checkout@11d5960a326750d5838078e36cf38b85af677262');
    expect(workflow).toContain('actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020');
    expect(workflow).toContain('dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d');
    expect(workflow).toContain("node-version: '22'");
  });

  it('runs the exact release preparation and complete signing-free gate in order', () => {
    const ordered = [
      'actions/checkout@11d5960a326750d5838078e36cf38b85af677262',
      'Verify exact clean dispatched source',
      'actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020',
      'dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d',
      'Verify hosted cross-drive topology',
      'run: npm ci',
      'run: npm run fixtures',
      'run: ./scripts/setup-pdfium.ps1',
      'run: cargo fetch --locked --target x86_64-pc-windows-msvc --manifest-path src-tauri/Cargo.toml',
      'Verify bundled dependency notices are current',
      'run: node scripts/dependency-notices.mjs --check',
      'run: npm test -- --pool=threads --maxWorkers=1 --no-file-parallelism',
      'run: ./scripts/run-native-recovery-gates.ps1',
      'run: npm run build',
      'Build the pinned unsigned OCR setup',
      'run: ./scripts/setup-ocr.ps1 -OutputRoot $env:OCR_SETUP_ROOT -Jobs 4',
      'Build and verify the unsigned default installer with OCR',
      './scripts/build-installer.ps1 `',
      'Recheck tracked source',
    ];
    let position = -1;
    for (const marker of ordered) {
      const next = workflow.indexOf(marker, position + 1);
      expect(next, `missing or out-of-order marker: ${marker}`).toBeGreaterThan(position);
      position = next;
      expect(workflow.indexOf(marker, next + marker.length), `duplicate marker: ${marker}`).toBe(-1);
    }
  });

  it('keeps topology evidence Boolean-only and excludes release capabilities', () => {
    expect(workflow).toContain('[IO.Path]::GetTempPath()');
    expect(workflow).toContain('workspaceDriveSet=');
    expect(workflow).toContain('tempDriveSet=');
    expect(workflow).toContain('drivesDiffer=');
    expect(workflow).not.toContain('workspaceDrive={');
    expect(workflow).not.toContain('tempDrive={');
    expect(workflow).toContain('if (-not $drivesDiffer)');
    expect(workflow).not.toMatch(/actions\/setup-dotnet|artifact-signing|cargo install|npm run (?:installer|tauri)|AZURE_|TAURI_SIGNING|secrets\.|id-token|contents:\s*write/i);
    expect(workflow).not.toMatch(/upload-artifact|download-artifact|gh release|release create|release edit|tags:|target_version|expected_publisher|latest\.json|_x64-setup/i);
  });

  it('proves the unsigned default installer packages OCR from a fresh pinned setup, never a signed or updater build', () => {
    expect(workflow).toContain('OCR_SETUP_ROOT: target/ocr-validation-setup-${{ github.run_id }}-${{ github.run_attempt }}');
    expect(workflow).toContain('INSTALLER_PROOF_ROOT: target/ocr-validation-installer-${{ github.run_id }}-${{ github.run_attempt }}');
    expect(workflow.match(/build-installer\.ps1/g)).toHaveLength(1);
    expect(workflow.match(/setup-ocr\.ps1/g)).toHaveLength(1);
    const build = workflow.slice(workflow.indexOf('./scripts/build-installer.ps1'), workflow.indexOf('Recheck tracked source'));
    expect(build).toContain('-UnsignedLocal `');
    expect(build).toContain('-OcrSetupRoot $env:OCR_SETUP_ROOT `');
    expect(build).toContain('-OutputRoot $env:INSTALLER_PROOF_ROOT `');
    expect(build).toContain('-SourceRevision $env:GITHUB_SHA');
    expect(build).not.toMatch(/-AzureSigning|-ArtifactOnly|-ExpectedPublisher/);
    expect(build).toContain("$receipt.mode -cne 'unsigned-local-ocr'");
    expect(build).toContain('$receipt.sourceRevision -cne $env:GITHUB_SHA');
    expect(build).toContain('@($receipt.ocr.packagedResources).Count -ne 5');
    expect(workflow).not.toMatch(/PDF_WORKSTATION_OCR_SETUP_ROOT|CARGO_TARGET_DIR/);
  });
});
