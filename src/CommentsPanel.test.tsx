import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { describe, expect, it, vi } from 'vitest';
import CommentsPanel from './CommentsPanel';
import type { DocumentAnnotations } from './bridge';

const document = { id: 2, name: 'commented.pdf', path: 'C:/commented.pdf', pages: [{ width: 612, height: 792 }, { width: 612, height: 792 }], revision: 4, dirty: true, can_undo: true, can_redo: false };
const annotations: DocumentAnnotations = { documentId: 2, revision: 4, status: 'supported', reason: null, annotations: [{ id: 'visible', kind: 'note', page: 0, rect: { x: .1, y: .2, width: .03, height: .03 }, contents: 'Visible' }, { id: 'hidden', kind: 'highlight', page: 1, rect: null, contents: null }], viewOnly: [], viewOnlyTruncated: 0 };

describe('CommentsPanel', () => {
  it('lists hidden notes for editing instead of guessing an on-page point', () => {
    const select = vi.fn();
    let ui!: ReactTestRenderer;
    act(() => { ui = create(<CommentsPanel document={document} page={0} annotations={annotations} error="" selectedId={null} onAddComment={vi.fn()} onAddHighlight={vi.fn()} onSelect={select} close={vi.fn()} />); });
    expect(JSON.stringify(ui.toJSON())).toContain('hidden by the current crop');
    const hidden = ui.root.findAllByType('button').at(-1)!;
    act(() => hidden.props.onClick());
    expect(select).toHaveBeenCalledWith(expect.objectContaining({ id: 'hidden', rect: null }));
  });

  it('reports native commenting refusal without offering a create action', () => {
    let ui!: ReactTestRenderer;
    act(() => { ui = create(<CommentsPanel document={document} page={0} annotations={{ ...annotations, status: 'unsupported', reason: 'Foreign annotations are unsupported.', annotations: [] }} error="" selectedId={null} onAddComment={vi.fn()} onAddHighlight={vi.fn()} onSelect={vi.fn()} close={vi.fn()} />); });
    expect(JSON.stringify(ui.toJSON())).toContain('Foreign annotations are unsupported.');
    expect(ui.root.findAllByType('button').some(button => button.children.join('').includes('Add comment'))).toBe(false);
  });

  it('lists annotations it cannot edit as plain text, with no edit actions', () => {
    const hostile = '<img src=x onerror="alert(1)"><script>alert(2)</script></style>javascript:alert(3)';
    const viewOnly = [
      { page: 0, subtype: 'Text', author: hostile, subject: 'Sticky Note', contents: `${hostile}\nsecond line`, modified: '2024-03-05 14:15', reply: false },
      { page: 0, subtype: 'Text', author: 'Other', subject: null, contents: 'A reply', modified: null, reply: true },
      { page: 1, subtype: 'Ink', author: null, subject: null, contents: null, modified: null, reply: false },
      { page: 1, subtype: 'SomethingNew', author: null, subject: null, contents: null, modified: null, reply: false },
    ];
    let ui!: ReactTestRenderer;
    act(() => { ui = create(<CommentsPanel document={document} page={0} annotations={{ ...annotations, status: 'unsupported', reason: 'The PDF has a foreign annotation. Commenting is unavailable.', annotations: [], viewOnly, viewOnlyTruncated: 2 }} error="" selectedId={null} onAddComment={vi.fn()} onAddHighlight={vi.fn()} onSelect={vi.fn()} close={vi.fn()} />); });
    const list = ui.root.findByProps({ 'aria-label': 'Annotations (view only)' });
    expect(list.findAllByType('li')).toHaveLength(4);
    const types = new Set<string>();
    for (const node of ui.root.findAll(() => true)) {
      if (typeof node.type === 'string') types.add(node.type);
      expect(node.props.dangerouslySetInnerHTML).toBeUndefined();
    }
    expect([...types].filter(type => !['aside', 'div', 'h2', 'button', 'p', 'ul', 'li', 'strong', 'small', 'span'].includes(type))).toEqual([]);
    const paragraphs = ui.root.findAllByType('p').map(node => node.children.join(''));
    expect(paragraphs).toContain('These 6 annotations are shown as saved. This build cannot change them yet.');
    expect(paragraphs).toContain('2 more not listed.');
    const labels = list.findAllByType('strong').map(node => node.children.join(''));
    expect(labels).toEqual(['Sticky note · Page 1', 'Reply · Sticky note · Page 1', 'Drawing · Page 2', 'Annotation · Page 2']);
    expect(list.findAllByType('span')[0].children.join('')).toBe(`${hostile}\nsecond line`);
    expect(ui.root.findAllByType('button').map(button => button.props['aria-label'])).toEqual(['Close comments']);
  });

  it('offers an area highlight and identifies optional descriptions clearly', () => {
    const addHighlight = vi.fn();
    let ui!: ReactTestRenderer;
    act(() => { ui = create(<CommentsPanel document={document} page={0} annotations={annotations} error="" selectedId={null} onAddComment={vi.fn()} onAddHighlight={addHighlight} onSelect={vi.fn()} close={vi.fn()} />); });
    expect(JSON.stringify(ui.toJSON())).toContain('Highlight');
    expect(JSON.stringify(ui.toJSON())).toContain('No description');
    act(() => ui.root.findAllByType('button').find(button => button.children.join('') === 'Add area highlight on page 1')!.props.onClick());
    expect(addHighlight).toHaveBeenCalledOnce();
  });
});
