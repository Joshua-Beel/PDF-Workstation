// Independent-reader check (annotation interchange design F1, slice P2-5): pdf.js, a second PDF reader with
// its own parser, reads the annotations this app writes and the other apps' annotations it keeps. It reads only
// the committed synthetic fixtures under src-tauri/tests/fixtures/annotations (written by the Rust test
// `write_interchange_fixtures`), never real work PDFs. Dev-only: pdfjs-dist is a devDependency and is not shipped.
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';
import { getDocument } from 'pdfjs-dist/legacy/build/pdf.mjs';

type Annotation = { subtype: string; id: string; rect: number[]; contentsObj?: { str: string }; titleObj?: { str: string }; quadPoints?: ArrayLike<number>; popupRef?: string | null; inReplyTo?: string; url?: string; creationDate?: string | null; modificationDate?: string | null };

const fixture = (name: string) => new Uint8Array(readFileSync(join(import.meta.dirname, '..', 'src-tauri', 'tests', 'fixtures', 'annotations', name)));

async function annotations(name: string): Promise<Annotation[][]> {
  const task = getDocument({ data: fixture(name), isEvalSupported: false, enableXfa: false, disableFontFace: true, useSystemFonts: false, stopAtErrors: true });
  const pdf = await task.promise;
  try {
    const pages: Annotation[][] = [];
    for (let number = 1; number <= pdf.numPages; number++) pages.push(await (await pdf.getPage(number)).getAnnotations() as Annotation[]);
    return pages;
  } finally { await task.destroy(); }
}

const near = (actual: ArrayLike<number>, expected: number[]) => { expect(actual.length).toBe(expected.length); expected.forEach((value, index) => expect(actual[index]).toBeCloseTo(value, 3)); };

describe('pdf.js reads the annotations the app writes', () => {
  it('reads the sticky note, its popup and both highlights with their geometry, text and dates', async () => {
    const [page] = await annotations('app-annotations.pdf');
    // pdf.js lists popups after every other annotation (so they draw on top); the file order is note, popup, ...
    expect(page.map(annotation => annotation.subtype)).toEqual(['Text', 'Highlight', 'Highlight', 'Popup']);
    const [note, area, text, popup] = page;
    expect(note.contentsObj?.str).toBe('App note');
    near(note.rect, [20, 150, 40, 170]);
    expect(note.popupRef).toBe(popup.id);
    near(popup.rect, [20, 50, 200, 170]);
    // The committed fixture's dates, written when it was generated.
    for (const annotation of [note, area, text]) {
      expect(annotation.creationDate).toBe('D:20261107192834Z');
      expect(annotation.modificationDate).toBe('D:20261107192834Z');
    }
    expect(note.titleObj?.str ?? '').toBe('');
    expect(area.contentsObj?.str).toBe('Area');
    near(area.rect, [60, 100, 140, 130]);
    near(area.quadPoints ?? [], [60, 130, 140, 130, 60, 100, 140, 100]);
    expect(text.contentsObj?.str).toBe('Text');
    near(text.rect, [20, 40, 120, 72]);
    near(text.quadPoints ?? [], [20, 72, 120, 72, 20, 60, 120, 60, 20, 52, 80, 52, 20, 40, 80, 40]);
  });

  it('still reads the other apps\' annotations, links and threads after the app adds a note beside them', async () => {
    const [first, second] = await annotations('foreign-plus-app.pdf');
    expect(first.map(annotation => annotation.subtype)).toEqual(['Text', 'Text', 'Link', 'Square', 'Text', 'Popup', 'Popup']);
    const [foreignNote, reply, link, square, mine] = first;
    const popupOf = (annotation: Annotation) => first.find(candidate => candidate.subtype === 'Popup' && candidate.id === annotation.popupRef);
    const [foreignPopup, minePopup] = [popupOf(foreignNote), popupOf(mine)];
    expect(Boolean(foreignPopup && minePopup && foreignPopup !== minePopup)).toBe(true);
    expect(foreignNote.titleObj?.str).toBe('Reviewer');
    expect(foreignNote.contentsObj?.str).toBe('Check this');
    expect(reply.inReplyTo).toBe(foreignNote.id);
    expect(reply.contentsObj?.str).toBe('Agreed');
    expect(link.url).toBe('https://example.invalid/');
    expect(square.titleObj?.str).toBe('Inline');
    expect(mine.contentsObj?.str).toBe('Mine');
    expect(second.map(annotation => [annotation.subtype, annotation.titleObj?.str])).toEqual([['Highlight', 'Other app']]);
  });
});

type InkAnnotation = Annotation & { inkLists?: ArrayLike<ArrayLike<number> | { x: number; y: number }[]>; color?: ArrayLike<number> | null; borderStyle?: { width: number } };

/** pdf.js has given ink strokes both as flat number arrays and as lists of points; this reads either. */
const strokes = (annotation: InkAnnotation) => Array.from(annotation.inkLists ?? [], stroke => Array.from(stroke as ArrayLike<number | { x: number; y: number }>).flatMap(point => typeof point === 'number' ? [point] : [point.x, point.y]));

describe('pdf.js reads drawings (P2-7g groundwork)', () => {
  it('reads the app drawing, the moved drawing and the restyled one with their strokes, colours and widths', async () => {
    const [page] = await annotations('ink.pdf') as InkAnnotation[][];
    expect(page.map(annotation => annotation.subtype)).toEqual(['Ink', 'Ink', 'Ink']);
    const [app, moved, restyled] = page;
    const appStrokes = strokes(app);
    expect(appStrokes.length).toBe(2);
    near(appStrokes[0], [60, 700, 120, 740, 180, 710]);
    near(appStrokes[1], [70, 690]);
    expect(Array.from(app.color ?? [])).toEqual([0, 102, 255]);
    expect(app.borderStyle?.width).toBeCloseTo(3, 3);
    expect(app.titleObj?.str ?? '').toBe('');
    near(moved.rect, [110, 180, 210, 240]);
    near(strokes(moved)[0], [120, 190, 160, 220, 200, 230]);
    expect(Array.from(moved.color ?? [])).toEqual([0, 0, 255]);
    near(restyled.rect, [100, 200, 200, 260]);
    expect(Array.from(restyled.color ?? [])).toEqual([255, 0, 0]);
    expect(restyled.borderStyle?.width).toBeCloseTo(2, 3);
    expect(restyled.titleObj?.str).toBe('Reviewer');
  });
});
