import { describe, expect, it } from 'vitest';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { tmpdir } from 'node:os';
import { dirname, join } from 'node:path';

const workflow = readFileSync('.github/workflows/release.yml', 'utf8').replace(/\r\n/g, '\n');
const versionValidator = readFileSync('scripts/assert-release-version.ps1', 'utf8');
const serviceSource = readFileSync('src-tauri/src/service.rs', 'utf8');
const restartSource = readFileSync('src-tauri/src/recovery_restart_tests.rs', 'utf8');
const currentVersion: string = JSON.parse(readFileSync('package.json', 'utf8')).version;
const wrongVersion = '0.0.0';

function versionFixture(lineEnding: '\n' | '\r\n') {
  const root = mkdtempSync(join(tmpdir(), 'smacrobat-release-version-'));
  for (const path of ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock']) {
    const target = join(root, path);
    mkdirSync(dirname(target), { recursive: true });
    const source = readFileSync(path, 'utf8').replace(/\r?\n/g, lineEnding);
    writeFileSync(target, source, 'utf8');
  }
  return root;
}

function validateFixture(root: string) {
  return spawnSync('pwsh', ['-NoProfile', '-NonInteractive', '-File', 'scripts/assert-release-version.ps1', '-ProjectRoot', root, '-Version', currentVersion], {
    cwd: process.cwd(), encoding: 'utf8', timeout: 10_000,
  });
}

describe('release workflow contract', () => {
  it('pins every action to an immutable full commit SHA', () => {
    const uses = [...workflow.matchAll(/^\s*-?\s*uses:\s*([^\s#]+)\s*(?:#.*)?$/gm)].map(match => match[1]);
    expect(uses).toEqual([
      'actions/checkout@11d5960a326750d5838078e36cf38b85af677262',
      'actions/setup-node@49933ea5288caeca8642d1e84afbd3f7d6820020',
      'dtolnay/rust-toolchain@89b12181fb390509a0842a86cc55eeb8eb928c1d',
      'actions/setup-dotnet@67a3573c9a986a3f9c594539f4ab511d57bb3ce9',
      'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02',
      'actions/upload-artifact@ea165f8d65b6e75b540449e92b4886f43607fa02',
      'actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093',
      'actions/download-artifact@d3f86a106a0bac45b974a628896c90dbdf5c8093',
    ]);
    for (const use of uses) expect(use).toMatch(/^[\w.-]+\/[\w.-]+@[0-9a-f]{40}$/);
    expect(workflow).not.toMatch(/uses:\s*[^\s]+@(?:v\d+|stable|main|master)\b/);
  });

  it('binds an exact stable tag, every version source, checkout, and draft target to one commit', () => {
    expect(workflow).toContain('fetch-depth: 0');
    expect(workflow).toContain('ref: ${{ github.sha }}');
    expect(workflow).toContain("'^v(?<version>\\d+\\.\\d+\\.\\d+)$'");
    expect(workflow).toContain('./scripts/assert-release-version.ps1 -ProjectRoot (Get-Location).Path -Version $version');
    for (const source of ['package.json', 'package-lock.json', 'src-tauri/tauri.conf.json', 'src-tauri/Cargo.toml', 'src-tauri/Cargo.lock']) expect(versionValidator).toContain(source);
    expect(workflow).toContain('git rev-parse HEAD');
    expect(workflow).toContain('git rev-parse "$($env:GITHUB_REF_NAME)^{commit}"');
    expect(workflow).toContain('$head -cne $env:GITHUB_SHA -or $tagCommit -cne $env:GITHUB_SHA');
    expect(workflow).toContain('--verify-tag --draft --target $env:GITHUB_SHA');
    expect(workflow).toContain('-not $release.isDraft');
    expect(workflow).toContain('$release.targetCommitish -cne $env:GITHUB_SHA');
    expect(workflow).not.toMatch(/gh release (?:edit|upload).*--latest|gh release edit.*--draft=false/);
  });

  it('signs only reviewed master commits behind the release environment, with least-privilege tokens', () => {
    const publishStart = workflow.indexOf('\n  publish:\n');
    expect(publishStart).toBeGreaterThan(0);
    const installer = workflow.slice(0, publishStart);
    const publish = workflow.slice(publishStart);
    expect(workflow).toMatch(/^permissions: \{\}$/m);
    expect(workflow.match(/^\s+environment:/gm)).toHaveLength(1);
    expect(installer).toContain('    environment: release\n    permissions:\n      contents: read\n');
    expect(installer).toContain('persist-credentials: false');
    expect(installer).not.toContain('contents: write');
    expect(installer).not.toContain('github.token');
    expect(installer).not.toContain('gh release');
    const ancestor = installer.indexOf('git merge-base --is-ancestor $env:GITHUB_SHA refs/remotes/origin/master');
    expect(ancestor).toBeGreaterThan(installer.indexOf('$head -cne $env:GITHUB_SHA -or $tagCommit -cne $env:GITHUB_SHA'));
    expect(ancestor).toBeLessThan(installer.indexOf('- run: npm ci'));
    expect(installer).toContain("if ($LASTEXITCODE -ne 0) { throw 'The release tag must point to a commit that is already on origin/master.' }");
    expect(installer).toContain('git rev-parse --verify --quiet refs/remotes/origin/master');
    expect(publish).toContain('    needs: installer\n    runs-on: windows-latest\n    permissions:\n      contents: write\n');
    for (const forbidden of ['actions/checkout', 'npm ', 'cargo ', 'scripts/', 'secrets.', 'node ']) expect(publish).not.toContain(forbidden);
    expect(workflow.match(/GH_TOKEN: \$\{\{ github\.token \}\}/g)).toHaveLength(1);
    const createStep = publish.indexOf('- name: Create draft release');
    expect(publish.indexOf('GH_TOKEN: ${{ github.token }}')).toBeGreaterThan(createStep);
    expect(publish).toContain('--notes-file release-notes/release-notes.md');
    expect(publish).toContain("throw 'Downloaded updater manifest is not bound to the exact version, installer URL, detached signature, and release notes.'");
    expect(publish).toContain("$manifest.platforms.'windows-x86_64'.url -cne $expectedUrl");
    expect(publish).toContain('$expectedUrl = "https://github.com/Joshua-Beel/PDF-Workstation/releases/download/v$($env:RELEASE_VERSION)/$($installerName.Replace(\' \', \'.\'))"');
    expect(publish).toContain('$signedFiles[0] -cne $installerName');
    expect(publish.indexOf("throw 'Downloaded updater signature does not name the exact installer in its trusted comment.'")).toBeGreaterThan(0);
    expect(publish.indexOf("throw 'Downloaded updater signature does not name the exact installer in its trusted comment.'")).toBeLessThan(publish.indexOf('- name: Create draft release'));
  });

  it('gives the updater key only to the signing build step and checks the signed installer name', () => {
    expect(workflow.match(/secrets\.TAURI_SIGNING_PRIVATE_KEY\b/g)).toHaveLength(2);
    expect(workflow.match(/TAURI_SIGNING_PRIVATE_KEY: \$\{\{ secrets\.TAURI_SIGNING_PRIVATE_KEY \}\}/g)).toHaveLength(1);
    expect(workflow).toContain("UPDATER_KEY_PRESENT: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY != '' }}");
    const build = workflow.indexOf('- name: Build publisher-signed installer and signed update manifest');
    const key = workflow.indexOf('TAURI_SIGNING_PRIVATE_KEY: ${{ secrets.TAURI_SIGNING_PRIVATE_KEY }}');
    expect(key).toBeGreaterThan(build);
    expect(key).toBeLessThan(workflow.indexOf('- name: Verify exact release asset inventory'));
    expect(workflow).not.toContain('secrets.TAURI_SIGNING_PRIVATE_KEY_PASSWORD');
    expect(workflow).toContain("TAURI_SIGNING_PRIVATE_KEY_PASSWORD: ''");
    expect(workflow).toContain('$signedFiles[0] -cne $installerName');
    expect(workflow).toContain("throw 'Updater signature does not name the exact installer in its trusted comment; installed clients would refuse this update.'");
  });

  it('executes the exact six-source validator for LF, CRLF, mismatches, missing roots, and duplicate Cargo packages', () => {
    expect(currentVersion).toMatch(/^\d+\.\d+\.\d+$/);
    expect(currentVersion).not.toBe(wrongVersion);
    expect(versionValidator).toContain('ConvertFrom-Json -AsHashtable');
    expect(versionValidator).toContain("$lockRoot = $packageLock['packages']['']");
    const roots: string[] = [];
    try {
      for (const ending of ['\n', '\r\n'] as const) {
        const root = versionFixture(ending); roots.push(root);
        const result = validateFixture(root);
        expect(result.status, result.stderr || result.stdout).toBe(0);
      }
      for (const mutate of [
        (root: string) => { const value = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')); value.version = wrongVersion; writeFileSync(join(root, 'package.json'), JSON.stringify(value), 'utf8'); },
        (root: string) => { const value = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8')); value.version = wrongVersion; writeFileSync(join(root, 'package-lock.json'), JSON.stringify(value), 'utf8'); },
        (root: string) => { const value = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8')); value.packages[''].version = wrongVersion; writeFileSync(join(root, 'package-lock.json'), JSON.stringify(value), 'utf8'); },
        (root: string) => { const value = JSON.parse(readFileSync(join(root, 'src-tauri/tauri.conf.json'), 'utf8')); value.version = wrongVersion; writeFileSync(join(root, 'src-tauri/tauri.conf.json'), JSON.stringify(value), 'utf8'); },
        (root: string) => writeFileSync(join(root, 'src-tauri/Cargo.toml'), readFileSync(join(root, 'src-tauri/Cargo.toml'), 'utf8').replace(`version = "${currentVersion}"`, `version = "${wrongVersion}"`), 'utf8'),
        (root: string) => writeFileSync(join(root, 'src-tauri/Cargo.lock'), readFileSync(join(root, 'src-tauri/Cargo.lock'), 'utf8').replace(`name = "pdf-workstation"\nversion = "${currentVersion}"`, `name = "pdf-workstation"\nversion = "${wrongVersion}"`), 'utf8'),
        (root: string) => { const value = JSON.parse(readFileSync(join(root, 'package-lock.json'), 'utf8')); delete value.packages['']; writeFileSync(join(root, 'package-lock.json'), JSON.stringify(value), 'utf8'); },
        (root: string) => writeFileSync(join(root, 'src-tauri/Cargo.lock'), `${readFileSync(join(root, 'src-tauri/Cargo.lock'), 'utf8')}\n[[package]]\nname = "pdf-workstation"\nversion = "${currentVersion}"\n`, 'utf8'),
      ]) {
        const root = versionFixture('\n'); roots.push(root); mutate(root);
        expect(validateFixture(root).status).not.toBe(0);
      }
    } finally {
      for (const root of roots) rmSync(root, { recursive: true, force: true });
    }
  }, 30_000);

  it('fails closed on signing and publishes only the explicit three-asset draft inventory', () => {
    expect(workflow.indexOf('run: node scripts/dependency-notices.mjs --check')).toBeGreaterThan(workflow.indexOf('run: cargo fetch --locked --target x86_64-pc-windows-msvc --manifest-path src-tauri/Cargo.toml'));
    const frontendGate = '- run: npm test -- --pool=threads --maxWorkers=1 --no-file-parallelism';
    expect(workflow.indexOf('run: node scripts/dependency-notices.mjs --check')).toBeLessThan(workflow.indexOf(frontendGate));
    expect(workflow).toContain(frontendGate);
    expect(workflow).toContain('- run: ./scripts/run-native-recovery-gates.ps1\n        shell: pwsh');
    expect(workflow).not.toMatch(/run: cargo test/);
    expect(serviceSource).toContain('fn recovery_checkpoint_ack_is_durable_and_explicit_restore_replays_exact_state()');
    expect(serviceSource).toContain('fn explicit_discard_is_idempotent_and_resolves_post_publish_lock()');
    expect(restartSource).toContain('fn restart_recovery_survives_fresh_workers()');
    for (const secret of [
      'TAURI_SIGNING_PRIVATE_KEY',
      'AZURE_TENANT_ID',
      'AZURE_CLIENT_ID',
      'AZURE_CLIENT_SECRET',
      'AZURE_SIGNING_ENDPOINT',
      'AZURE_SIGNING_ACCOUNT',
      'AZURE_SIGNING_PROFILE',
    ]) {
      expect(workflow).toContain(`secrets.${secret}`);
    }
    expect(workflow).toContain('./scripts/build-installer.ps1 -AzureSigning');
    expect(workflow).toContain("$expected = @($installerName, \"$installerName.sig\", 'latest.json')");
    expect(workflow).toContain('$actual.Count -ne 3');
    expect(workflow).toContain('Updater manifest is not bound to the exact version, installer, detached signature, and release notes.');
    expect(workflow).toContain('src-tauri/target/release/bundle/nsis/PDF Workstation_${{ env.RELEASE_VERSION }}_x64-setup.exe');
    expect(workflow).toContain('src-tauri/target/release/bundle/nsis/PDF Workstation_${{ env.RELEASE_VERSION }}_x64-setup.exe.sig');
    expect(workflow).toContain('src-tauri/target/release/bundle/nsis/latest.json');
    expect(workflow).not.toMatch(/bundle\/nsis\/\*|Get-ChildItem[^\n]+Where-Object[^\n]+\.(?:exe|sig|json)/);
  });

  it('keeps version metadata, updater trust, and release limitations exact', () => {
    const pkg = JSON.parse(readFileSync('package.json', 'utf8'));
    const npmLock = JSON.parse(readFileSync('package-lock.json', 'utf8'));
    const tauri = JSON.parse(readFileSync('src-tauri/tauri.conf.json', 'utf8'));
    const cargo = readFileSync('src-tauri/Cargo.toml', 'utf8').match(/^version = "([^"]+)"$/m)?.[1];
    const cargoLock = readFileSync('src-tauri/Cargo.lock', 'utf8').match(/\[\[package\]\]\r?\nname = "pdf-workstation"\r?\nversion = "([^"]+)"/)?.[1];
    expect([pkg.version, npmLock.version, npmLock.packages[''].version, tauri.version, cargo, cargoLock]).toEqual(Array(6).fill(currentVersion));
    expect(tauri.plugins.updater).toEqual({
      pubkey: 'dW50cnVzdGVkIGNvbW1lbnQ6IG1pbmlzaWduIHB1YmxpYyBrZXk6IDg1OTcwQzM0QUVBQkQ3NzUKUldSMTE2dXVOQXlYaFdZN0k1Mk1CcSt2NS81bzNRTERRbU16ejYxTDNiS3V6b1RTZ0tuTlRjZVEK',
      endpoints: ['https://github.com/Joshua-Beel/PDF-Workstation/releases/latest/download/latest.json'],
      windows: { installMode: 'passive' },
    });
    const notes = readFileSync('docs/release-notes.md', 'utf8').replace(/\r\n?/g, '\n');
    expect(notes.startsWith(`## ${currentVersion}\n`)).toBe(true);
    expect(notes.match(/^#{1,2} /gm), 'latest.json carries the whole file, so it holds only the released version').toHaveLength(1);
    expect(notes).toContain('tested in source builds');
    expect(notes).toContain(`unverified in an installed ${currentVersion} build`);
    expect(notes).toContain('Accuracy on real scans is unmeasured.');
    expect(notes).toContain('PDF signing, certificate signatures or signature validation');
    expect(notes).toContain('every save writes a new copy');
    expect(notes.length).toBeLessThan(4096);
    expect(readFileSync('.gitattributes', 'utf8').replace(/\r\n?/g, '\n').split('\n')).toContain('docs/release-notes.md text eol=lf');
    expect(readFileSync('docs/release-notes.md', 'utf8'), 'latest.json notes must not carry CRLF from a Windows checkout').not.toContain('\r');
  });
});
