import { readFileSync } from 'node:fs';
import { describe, expect, it } from 'vitest';

const workflow = readFileSync('.github/workflows/hosted-npm-diagnostic.yml', 'utf8').replace(/\r\n?/g, '\n');

describe('hosted npm diagnostic workflow', () => {
  it('is a bounded manual read-only current-HEAD diagnostic', () => {
    expect(workflow).toContain('workflow_dispatch:');
    expect(workflow).not.toMatch(/^\s*(push|pull_request|schedule):/m);
    expect(workflow).toContain('permissions:\n  contents: read');
    expect(workflow).toContain('runs-on: windows-latest');
    expect(workflow).toContain('timeout-minutes: 15');
    expect(workflow).toContain('ref: ${{ github.sha }}');
    expect(workflow).toContain('persist-credentials: false');
    expect(workflow).toContain('$head -cne $env:GITHUB_SHA');
    expect(workflow).toContain('git diff --quiet --');
    expect(workflow).toContain('git diff --cached --quiet --');
    expect(workflow).toContain('actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1');
    expect(workflow).toContain('actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1');
    expect(workflow).toContain("node-version: '22'");
  });

  it('reproduces the cross-drive temp topology and runs only the targeted npm test', () => {
    expect(workflow).toContain('[IO.Path]::GetTempPath()');
    expect(workflow).toContain('workspaceDriveSet=');
    expect(workflow).toContain('tempDriveSet=');
    expect(workflow).toContain('drivesDiffer=');
    expect(workflow).not.toContain('workspaceDrive={');
    expect(workflow).not.toContain('tempDrive={');
    expect(workflow).toContain('if (-not $drivesDiffer)');
    expect(workflow).toContain('npm ci');
    expect(workflow).toContain('npx vitest run scripts/installed-app-launch-workflow.test.ts --maxWorkers=1 --reporter=verbose');
    expect(workflow.match(/vitest run/g)).toHaveLength(1);
  });

  it('cannot build, sign, publish, mutate releases, or upload evidence', () => {
    expect(workflow).not.toMatch(/cargo|rust-toolchain|setup-pdfium|npm run (?:build|fixtures|installer|tauri)|artifact-signing|AZURE_|TAURI_SIGNING|secrets\.|id-token|contents:\s*write/i);
    expect(workflow).not.toMatch(/upload-artifact|download-artifact|gh release|release create|release edit|tags:|target_version|expected_publisher/i);
  });
});
