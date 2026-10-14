import { expect, it } from 'vitest';
import { displayPoint, pageTopOffset, planDestination, type BookmarkDestination, type DestinationMode, type DestinationView, type ViewState } from './bookmarkDestination';
import { fitPageScale, type PageSize } from './model';

const pages: PageSize[] = [{ width: 612, height: 792 }, { width: 612, height: 792 }, { width: 612, height: 792 }];
const bounds = { width: 900, height: 800 };
const upright: DestinationView = { left: 0, bottom: 0, width: 612, height: 792, rotation: 0 };
const dest = (mode: DestinationMode, extra: Partial<BookmarkDestination> = {}): BookmarkDestination => ({ mode, x: null, y: null, rect: null, zoom: null, view: upright, ...extra });
const hundred: ViewState = { fit: 'none', zoom: 100, scale: 96 / 72 };
const plan = (destination: BookmarkDestination | null, state = hundred, size = pages, index = 1) => planDestination(destination, size, index, bounds, state, Math.max(...size.map(page => page.width)));

it('maps user-space points to the displayed page for every rotation and a CropBox offset', () => {
  const cropped = (rotation: number): DestinationView => ({ left: 50, bottom: 100, width: rotation % 180 ? 600 : 400, height: rotation % 180 ? 400 : 600, rotation });
  const unrotated: DestinationView = { left: 50, bottom: 100, width: 400, height: 600, rotation: 0 };
  expect(displayPoint(unrotated, { width: 400, height: 600 }, 50, 700)).toMatchObject({ x: 0, y: 0 });
  expect(displayPoint(unrotated, { width: 400, height: 600 }, 450, 100)).toMatchObject({ x: 400, y: 600 });
  const box = { left: 50, bottom: 100, width: 400, height: 600 };
  const topLeft = [50, 700] as const;
  expect(displayPoint({ ...box, rotation: 90 }, { width: 600, height: 400 }, ...topLeft)).toMatchObject({ x: 600, y: 0 });
  expect(displayPoint({ ...box, rotation: 180 }, { width: 400, height: 600 }, ...topLeft)).toMatchObject({ x: 400, y: 600 });
  expect(displayPoint({ ...box, rotation: 270 }, { width: 600, height: 400 }, ...topLeft)).toMatchObject({ x: 0, y: 400 });
  expect(displayPoint({ ...box, rotation: 90 }, { width: 600, height: 400 }, 250, 400)).toMatchObject({ x: 300, y: 200 });
  expect(cropped(90).width).toBe(600);
});

it('tracks which displayed axis a user-space coordinate controls', () => {
  const turned: DestinationView = { left: 0, bottom: 0, width: 612, height: 792, rotation: 90 };
  const size = { width: 792, height: 612 };
  expect(displayPoint(upright, pages[0], 72, null)).toMatchObject({ hasX: true, hasY: false });
  expect(displayPoint(upright, pages[0], null, 700)).toMatchObject({ hasX: false, hasY: true });
  expect(displayPoint(turned, size, 72, null)).toMatchObject({ hasX: false, hasY: true });
  expect(displayPoint(turned, size, null, 700)).toMatchObject({ hasX: true, hasY: false });
});

it('clamps points outside the page to its edges', () => {
  expect(displayPoint(upright, pages[0], 1_000_000, -1_000_000)).toMatchObject({ x: 612, y: 792 });
  expect(displayPoint(upright, pages[0], -1_000_000, 1_000_000)).toMatchObject({ x: 0, y: 0 });
});

it('XYZ applies its zoom and puts the point at the window corner', () => {
  const result = plan(dest('xyz', { x: 72, y: 700, zoom: 1.5 }));
  expect(result).toMatchObject({ fit: 'none', zoom: 150 });
  expect(result.scale).toBeCloseTo(2);
  expect(result.top).toBeCloseTo(pageTopOffset(pages, 1, 2) + 92 * 2);
  expect(result.left).toBeCloseTo(Math.max(0, Math.max(900, 612 * 2 + 144) / 2 - 612 * 2 / 2 + 72 * 2));
});

it('XYZ with null zoom keeps the current fit and zoom, and null y goes to the page top', () => {
  const state: ViewState = { fit: 'width', zoom: 100, scale: 1.2 };
  const result = plan(dest('xyz', { x: null, y: null }), state);
  expect(result).toMatchObject({ fit: 'width', zoom: 100, left: null });
  expect(result.scale).toBe(1.2);
  expect(result.top).toBeCloseTo(pageTopOffset(pages, 1, 1.2) - 24);
  expect(plan(dest('xyz', { y: 700 }), state).top).toBeCloseTo(pageTopOffset(pages, 1, 1.2) + 92 * 1.2);
});

it('XYZ zoom is held to the viewer range of 10 to 400 percent', () => {
  expect(plan(dest('xyz', { zoom: 4 })).zoom).toBe(400);
  expect(plan(dest('xyz', { zoom: 0.1 })).zoom).toBe(10);
  expect(plan(dest('xyz', { zoom: 12 })).zoom).toBe(400);
  expect(plan(dest('xyz', { zoom: 0 })).zoom).toBe(100);
});

it('Fit and FitB use the fit-page scale of the target page', () => {
  for (const mode of ['fit', 'fit_b'] as const) {
    const result = plan(dest(mode));
    expect(result).toMatchObject({ fit: 'page', left: null });
    expect(result.scale).toBeCloseTo(fitPageScale(pages[1], 900, 800));
    expect(result.top).toBeCloseTo(pageTopOffset(pages, 1, result.scale) - 24);
  }
});

it('FitH and FitBH fit the width and put the y coordinate at the top edge', () => {
  for (const mode of ['fit_h', 'fit_bh'] as const) {
    const result = plan(dest(mode, { y: 500 }));
    expect(result.fit).toBe('width');
    expect(result.scale).toBeCloseTo((900 - 144) / 612);
    expect(result.top).toBeCloseTo(pageTopOffset(pages, 1, result.scale) + 292 * result.scale);
  }
  const mixed: PageSize[] = [{ width: 1_000, height: 400 }, { width: 500, height: 700 }];
  const narrow = plan(dest('fit_h', { y: 700 }), hundred, mixed, 1);
  expect(narrow.fit).toBe('none');
  expect(narrow.zoom).toBe(Math.round((900 - 144) / 500 * 0.75 * 100));
});

it('FitV and FitBV fit the height and put the x coordinate at the left edge', () => {
  for (const mode of ['fit_v', 'fit_bv'] as const) {
    const result = plan(dest(mode, { x: 30 }));
    expect(result.fit).toBe('none');
    expect(result.zoom).toBe(71);
    expect(result.top).toBeCloseTo(pageTopOffset(pages, 1, result.scale) - 24);
    expect(result.left).not.toBeNull();
  }
});

it('FitR frames the rectangle, centering it in the window', () => {
  const result = plan(dest('fit_r', { rect: [100, 400, 300, 700] }));
  expect(result.fit).toBe('none');
  expect(result.zoom).toBe(188);
  const scale = 1.88 * 96 / 72;
  expect(result.scale).toBeCloseTo(scale);
  expect(result.top).toBeCloseTo(pageTopOffset(pages, 1, scale) + 92 * scale - Math.max(0, (800 - 300 * scale) / 2));
  expect(result.left).toBeCloseTo(Math.max(0, Math.max(900, 612 * scale + 144) / 2 - 612 * scale / 2 + 100 * scale - Math.max(0, (900 - 200 * scale) / 2)));
});

it('rotated and cropped pages position the point on the displayed page', () => {
  const view: DestinationView = { left: 50, bottom: 100, width: 400, height: 600, rotation: 90 };
  const rotated: PageSize[] = [{ width: 600, height: 400 }, { width: 600, height: 400 }];
  const result = planDestination(dest('xyz', { x: 50, y: 700, zoom: 1, view }), rotated, 1, bounds, hundred, 600);
  expect(result.top).toBeCloseTo(pageTopOffset(rotated, 1, 96 / 72) + 0);
  const centred = planDestination(dest('xyz', { x: 250, y: 400, zoom: 1, view }), rotated, 1, bounds, hundred, 600);
  expect(centred.top).toBeCloseTo(pageTopOffset(rotated, 1, 96 / 72) + 200 * 96 / 72);
});

it('falls back to the page top for malformed, unusable or unplaceable destinations', () => {
  const top = pageTopOffset(pages, 1, hundred.scale) - 24;
  for (const bad of [null, undefined, dest('top'), dest('xyz', { x: Number.NaN }), dest('xyz', { zoom: Number.POSITIVE_INFINITY }), dest('xyz', { y: 2_000_000 }), dest('fit_r', { rect: null }), dest('fit_r', { rect: [10, 10, 10, 10] }), dest('fit_r', { rect: [0, 0, Number.NaN, 5] })]) {
    expect(plan(bad as BookmarkDestination | null)).toMatchObject({ fit: 'none', zoom: 100, left: null, top: expect.closeTo(top) });
  }
  expect(plan(dest('xyz', { y: 700 }), hundred, pages, 9)).toMatchObject({ fit: 'none', zoom: 100, left: null });
  expect(planDestination(dest('fit'), pages, 1, { width: 0, height: 0 }, hundred, 612)).toMatchObject({ fit: 'none', zoom: 100 });
  const noView = plan(dest('xyz', { x: 72, y: 700, view: null }));
  expect(noView.left).toBeNull();
  expect(noView.top).toBeCloseTo(pageTopOffset(pages, 1, noView.scale) - 24);
});
