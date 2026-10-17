import { describe, expect, it } from 'vitest';
import { readFileSync } from 'node:fs';

const verifier = readFileSync('scripts/verify-native-page-dialogs.ps1', 'utf8');
const mainRs = readFileSync('src-tauri/src/main.rs', 'utf8');

describe('native page-dialog verifier', () => {
  it('drives the app only through accessibility patterns, never synthetic input', () => {
    expect(verifier).not.toMatch(/SendKeys|SendInput|mouse_event|keybd_event|SetCursorPos|CopyFromScreen/);
    expect(verifier).toContain('InvokePattern');
    expect(verifier).toContain('ValuePattern');
  });

  it('refuses a system-drive work root, redirects the web view data, and always stops the app', () => {
    expect(verifier).toContain('WorkRoot must not be on the system drive');
    expect(verifier).toContain('WEBVIEW2_USER_DATA_FOLDER');
    expect(verifier).toMatch(/finally \{[\s\S]*Kill\(\$true\)[\s\S]*app process closed/);
  });

  it('confines the work root to the sibling tmp folder, filters by process id, and checks Cancel writes nothing', () => {
    expect(verifier).toContain('WorkRoot must be under the sibling tmp folder');
    expect(verifier).toContain('StartsWith($allowedTmp, [StringComparison]::OrdinalIgnoreCase)');
    expect(verifier).not.toContain('$env:TEMP');
    expect(verifier).toContain('ProcessIdProperty, $script:AppPid');
    expect(verifier).toContain('$script:AppPid -gt 0');
    expect(verifier).toContain('($Before.files -ceq $After.files)');
    expect(verifier).toContain('no output written');
    expect(verifier).toContain('[switch]$Strict');
    expect(verifier).toMatch(/\$Strict -and \$notAutomatable\.Count -gt 0\) \{[^}]*exit 2/);
  });

  it('expects the dialog titles and default names that main.rs actually sets', () => {
    for (const title of ['Save combined PDF as a new file', 'Save PDF with inserted pages as a new file', 'Save PDF with replaced pages as a new file', 'Choose a folder for split PDFs', 'Save a copy as a new file', 'Choose one PNG or JPEG image', 'Save image as a new PDF', 'Choose PNG or JPEG images', 'Save images as a new PDF']) {
      expect(mainRs).toContain(`set_title("${title}")`);
      expect(verifier).toContain(`'${title}'`);
    }
    for (const name of ['combined.pdf', 'inserted-pages.pdf', 'replaced-pages.pdf', 'images.pdf']) {
      expect(mainRs).toContain(`set_file_name("${name}")`);
      expect(verifier).toContain(`'${name}'`);
    }
  });

  it('expects Split to use a real folder picker and a new Split PDFs folder inside it', () => {
    expect(mainRs).toContain('set_title("Choose a folder for split PDFs").pick_folder()');
    expect(mainRs).toContain('split::new_split_folder(&parent)');
    expect(verifier).toContain("Join-Path $parent 'Split PDFs'");
    expect(verifier).toContain('Complete-NativeDialog $dialog $parent');
    expect(verifier).not.toContain("'Save As'");
  });
});
