import { fitPageScale, type PageSize } from './model';
import type { FitMode } from './preferences';

export type DestinationMode = 'top' | 'xyz' | 'fit' | 'fit_h' | 'fit_v' | 'fit_r' | 'fit_b' | 'fit_bh' | 'fit_bv';
export type DestinationView = { left: number; bottom: number; width: number; height: number; rotation: number };
export type BookmarkDestination = { mode: DestinationMode; x: number | null; y: number | null; rect: [number, number, number, number] | null; zoom: number | null; view: DestinationView | null };
export type ViewState = { fit: FitMode; zoom: number; scale: number };
export type DestinationPlan = { fit: FitMode; zoom: number; scale: number; top: number; left: number | null };

const pagePadding = 24;
const toScale = (zoom: number) => zoom / 100 * 96 / 72;
const toZoom = (scale: number) => Math.max(10, Math.min(400, Math.round(scale * 72 / 96 * 100)));
const finite = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value);
const limit = 1_000_000;

export function pageTopOffset(pages: PageSize[], index: number, scale: number) {
  let offset = pagePadding;
  for (let page = 0; page < index && page < pages.length; page++) offset += pages[page].height * scale + pagePadding;
  return offset;
}

export function displayPoint(view: DestinationView, size: PageSize, x: number | null, y: number | null) {
  const rotation = ((Math.round(view.rotation / 90) % 4) + 4) % 4;
  const upright = rotation % 2 === 0;
  const hasX = upright ? x !== null : y !== null;
  const hasY = upright ? y !== null : x !== null;
  const ux = Math.max(0, Math.min(view.width, (x ?? view.left) - view.left));
  const uy = Math.max(0, Math.min(view.height, view.bottom + view.height - (y ?? view.bottom + view.height)));
  const [dx, dy] = rotation === 0 ? [ux, uy] : rotation === 1 ? [view.height - uy, ux] : rotation === 2 ? [view.width - ux, view.height - uy] : [uy, view.width - ux];
  return { x: Math.max(0, Math.min(size.width, dx)), y: Math.max(0, Math.min(size.height, dy)), hasX, hasY };
}

function usable(destination: BookmarkDestination | null | undefined): destination is BookmarkDestination {
  if (!destination || destination.mode === 'top') return false;
  const values = [destination.x, destination.y, destination.zoom, ...(destination.rect ?? [])];
  return values.every(value => value === null || (finite(value) && Math.abs(value) <= limit));
}

export function planDestination(destination: BookmarkDestination | null | undefined, pages: PageSize[], index: number, bounds: { width: number; height: number }, current: ViewState, maxWidth: number): DestinationPlan {
  const size = pages[index];
  const keep = (scale = current.scale): DestinationPlan => ({ fit: current.fit, zoom: current.zoom, scale, top: pageTopOffset(pages, index, scale) - pagePadding, left: null });
  if (!size || !usable(destination) || !finite(bounds.width) || !finite(bounds.height) || bounds.width <= 0 || bounds.height <= 0) return keep();
  const explicit = (zoom: number): Pick<DestinationPlan, 'fit' | 'zoom' | 'scale'> => ({ fit: 'none', zoom, scale: toScale(zoom) });
  const view = destination.view;
  const place = (state: Pick<DestinationPlan, 'fit' | 'zoom' | 'scale'>, x: number | null, y: number | null): DestinationPlan => {
    const base = pageTopOffset(pages, index, state.scale);
    const point = view && (x !== null || y !== null) ? displayPoint(view, size, x, y) : null;
    const stackWidth = Math.max(bounds.width, maxWidth * state.scale + 144);
    const pageLeft = stackWidth / 2 - size.width * state.scale / 2;
    return { ...state, top: point?.hasY ? base + point.y * state.scale : base - pagePadding, left: point?.hasX ? Math.max(0, pageLeft + point.x * state.scale) : null };
  };
  const fitPage = () => ({ fit: 'page' as FitMode, zoom: current.zoom, scale: fitPageScale(size, bounds.width, bounds.height) });
  const fitWidth = () => Math.abs(size.width - maxWidth) < 0.5
    ? { fit: 'width' as FitMode, zoom: current.zoom, scale: Math.max(0.1, (bounds.width - 144) / maxWidth) }
    : explicit(toZoom((bounds.width - 144) / size.width));
  const fitHeight = () => explicit(toZoom((bounds.height - 48) / size.height));
  switch (destination.mode) {
    case 'xyz': {
      const state = destination.zoom !== null && destination.zoom > 0 ? explicit(Math.max(10, Math.min(400, Math.round(destination.zoom * 100)))) : { fit: current.fit, zoom: current.zoom, scale: current.scale };
      return place(state, destination.x, destination.y);
    }
    case 'fit': case 'fit_b': return place(fitPage(), null, null);
    case 'fit_h': case 'fit_bh': return place(fitWidth(), null, destination.y);
    case 'fit_v': case 'fit_bv': return place(fitHeight(), destination.x, null);
    case 'fit_r': {
      if (!view || !destination.rect) return keep();
      const [left, bottom, right, top] = destination.rect;
      const a = displayPoint(view, size, left, top), b = displayPoint(view, size, right, bottom);
      const x = Math.min(a.x, b.x), y = Math.min(a.y, b.y), width = Math.abs(a.x - b.x), height = Math.abs(a.y - b.y);
      if (width < 1 || height < 1) return keep();
      const state = explicit(toZoom(Math.min((bounds.width - 48) / width, (bounds.height - 48) / height)));
      const base = pageTopOffset(pages, index, state.scale);
      const pageLeft = Math.max(bounds.width, maxWidth * state.scale + 144) / 2 - size.width * state.scale / 2;
      return { ...state, top: base + y * state.scale - Math.max(0, (bounds.height - height * state.scale) / 2), left: Math.max(0, pageLeft + x * state.scale - Math.max(0, (bounds.width - width * state.scale) / 2)) };
    }
    default: return keep();
  }
}
