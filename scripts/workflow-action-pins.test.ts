import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

const workflowDir = '.github/workflows';
const workflowFiles = readdirSync(workflowDir).filter(name => /\.ya?ml$/i.test(name)).sort();
const pinned = /^[\w.-]+\/[\w.-]+(?:\/[\w./-]+)?@[0-9a-f]{40}$/;
const local = /^\.\/[\w./-]+$/;

function usesReferences(source: string) {
  const lines = source.replace(/\r\n?/g, '\n').split('\n');
  const keyLines = lines.filter(line => /^\s*(?:-\s*)?['"]?uses['"]?\s*:/.test(line));
  const mentionLines = lines.filter(line => /\buses\s*:/.test(line));
  const refs = keyLines.map(line => {
    const value = line.replace(/^\s*(?:-\s*)?['"]?uses['"]?\s*:\s*/, '').replace(/\s+#.*$/, '').trim();
    return value.replace(/^(['"])(.*)\1$/, '$2');
  });
  return { refs, keyLines: keyLines.length, mentionLines: mentionLines.length };
}

describe('workflow action pins', () => {
  it('finds the workflow files', () => {
    expect(workflowFiles.length).toBeGreaterThan(0);
  });

  it.each(workflowFiles)('%s pins every uses: to a full commit SHA or a local action', file => {
    const { refs, keyLines, mentionLines } = usesReferences(readFileSync(join(workflowDir, file), 'utf8'));
    expect(mentionLines, 'a uses: reference is not on its own key line').toBe(keyLines);
    const unpinned = refs.filter(ref => !pinned.test(ref) && !local.test(ref));
    expect(unpinned).toEqual([]);
  });

  it.each(workflowFiles)('%s uses only reviewed Node 24 releases of actions/*', file => {
    const { refs } = usesReferences(readFileSync(join(workflowDir, file), 'utf8'));
    const unreviewed = refs.filter(ref => ref.startsWith('actions/') && !node24ActionPins.has(ref));
    expect(unreviewed).toEqual([]);
  });
});

const node24ActionPins = new Set([
  'actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1',
  'actions/upload-artifact@cf430e030ddbb5b0abf93d22962f4752f3646cd9',
  'actions/download-artifact@37930b1c2abaa49bbe596cd826c3c89aef350131',
  'actions/setup-node@949feb2413d6458794dcd2491c4babbbce0c15c1',
  'actions/setup-dotnet@a98b56852c35b8e3190ac28c8c2271da59106c68',
  'actions/cache/restore@55cc8345863c7cc4c66a329aec7e433d2d1c52a9',
  'actions/cache/save@55cc8345863c7cc4c66a329aec7e433d2d1c52a9',
]);
