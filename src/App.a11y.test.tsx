const fsModule = 'node:fs';
import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import App from './App';
import { editPages, openDocument, saveCopy } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ native: false, openDocument: vi.fn(), reopenDocument: vi.fn(), closeDocument: vi.fn(), editPages: vi.fn(), saveCopy: vi.fn(), createPdfFromImage: vi.fn(), documentPageLabels: vi.fn() }));
vi.mock('./Viewer', () => ({ default: () => <div>Rendered document</div> }));

const document: DocumentInfo = { id: 9, name: 'one.pdf', path: 'C:/docs/one.pdf', pages: [{ width: 612, height: 792 }, { width: 792, height: 612 }], revision: 0, dirty: false, can_undo: false, can_redo: false };
let keydown: (event: KeyboardEvent) => void;
let ui: ReactTestRenderer | undefined;
const focus = vi.fn();

beforeEach(() => {
  vi.clearAllMocks();
  vi.stubGlobal('window', {
    localStorage: { getItem: () => null, setItem: vi.fn() },
    addEventListener: (name: string, callback: typeof keydown) => { if (name === 'keydown') keydown = callback; },
    removeEventListener: vi.fn(),
  });
  vi.mocked(openDocument).mockResolvedValue({ status: 'opened', document });
  vi.mocked(editPages).mockResolvedValue(document);
  vi.mocked(saveCopy).mockResolvedValue(null);
});
afterEach(() => { if (ui) act(() => ui!.unmount()); ui = undefined; vi.unstubAllGlobals(); });

const mount = async () => { await act(async () => { ui = create(<App />, { createNodeMock: () => ({ focus }) }); }); };
const openSample = async () => act(async () => ui!.root.findAllByType('button').find(button => button.children.includes('Explore a sample PDF '))!.props.onClick());
const named = (label: string) => ui!.root.findByProps({ 'aria-label': label });
const menuButton = () => ui!.root.findAllByType('button').find(button => button.props['aria-controls'] === 'app-menu')!;

it('links the Menu disclosure button to its popover and closes it on Escape, returning focus to the button', async () => {
  await mount();
  expect(menuButton().props['aria-expanded']).toBe(false);
  await act(async () => menuButton().props.onClick());
  expect(menuButton().props['aria-expanded']).toBe(true);
  expect(ui!.root.findAll(node => node.type === 'div' && node.props.id === 'app-menu')).toHaveLength(1);
  const preventDefault = vi.fn();
  const target = { closest: () => null, matches: () => false };
  await act(async () => keydown({ key: 'Escape', target, preventDefault } as unknown as KeyboardEvent));
  expect(preventDefault).toHaveBeenCalledOnce();
  expect(focus).toHaveBeenCalledOnce();
  expect(menuButton().props['aria-expanded']).toBe(false);
  expect(ui!.root.findAll(node => node.type === 'div' && node.props.id === 'app-menu')).toHaveLength(0);
});

it('does not swallow Escape for the menu when focus is inside a dialog', async () => {
  await mount();
  await act(async () => menuButton().props.onClick());
  const target = { closest: (selector: string) => selector === 'dialog' ? {} : null, matches: () => false };
  await act(async () => keydown({ key: 'Escape', target, preventDefault: vi.fn() } as unknown as KeyboardEvent));
  expect(menuButton().props['aria-expanded']).toBe(true);
});

it('exposes toggle and selected state to assistive technology, not only by colour', async () => {
  await mount();
  expect(named('Home').props['aria-current']).toBe('page');
  const sidebar = ui!.root.findAllByType('button').filter(button => button.props['aria-current'] === 'true');
  expect(sidebar).toHaveLength(1);
  expect(sidebar[0].children).toContain('Recent');
  await openSample();
  expect(named('Home').props['aria-current']).toBeUndefined();
  const tab = ui!.root.findAllByType('button').find(button => button.props['aria-current'] === 'page' && button.findAllByType('span').some(span => span.children.includes('one.pdf')))!;
  expect(tab).toBeTruthy();
  expect(named('Fit width').props['aria-pressed']).toBe(true);
  expect(named('Fit page').props['aria-pressed']).toBe(false);
  expect(named('Comments').props['aria-pressed']).toBe(false);
  expect(named('Rotate view').props['aria-pressed']).toBeUndefined();
  expect(named('Undo').props['aria-pressed']).toBeUndefined();
  await act(async () => named('Fit page').props.onClick());
  expect(named('Fit page').props['aria-pressed']).toBe(true);
  expect(named('Fit width').props['aria-pressed']).toBe(false);
});

it('labels landmarks and gives the document view a heading', async () => {
  await mount();
  expect(named('Primary').type).toBe('nav');
  expect(named('Home navigation').type).toBe('aside');
  await openSample();
  expect(ui!.root.findAllByType('h1').map(heading => heading.children.join(''))).toEqual(['one.pdf']);
  expect(named('Panels and page controls').type).toBe('aside');
  const status = ui!.root.findAll(node => node.type === 'span' && node.props['aria-live'] === 'polite');
  expect(status.some(node => JSON.stringify(node.children).includes('Source preserved'))).toBe(true);
});

it('keeps a visible focus indicator on the search fields whose own outline is removed', async () => {
  const { readFileSync } = await import(/* @vite-ignore */ fsModule) as { readFileSync: (url: URL, encoding: string) => string };
  const css = readFileSync(new URL('./Workspace.module.css', import.meta.url), 'utf8');
  const rule = (selector: RegExp) => css.match(new RegExp(selector.source + '\\s*\\{([^}]*)\\}'))?.[1] ?? '';
  expect(rule(/\.search(?:_\w+)? input/)).toMatch(/outline:\s*0/);
  expect(rule(/\.search(?:_\w+)?:focus-within/)).toMatch(/outline:\s*2px solid/);
  expect(rule(/\.viewport(?:_\w+)?:focus-visible/)).toMatch(/outline:\s*2px solid/);
  expect(rule(/\.srOnly(?:_\w+)?/)).toMatch(/position:\s*absolute/);
});
