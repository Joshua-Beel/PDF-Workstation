import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, expect, it, vi } from 'vitest';
import { dependencyNotices } from './bridge';
import DependencyNotices from './DependencyNotices';

vi.mock('./bridge', () => ({ dependencyNotices: vi.fn() }));

async function mount() {
  let ui!: ReactTestRenderer;
  await act(async () => {
    ui = create(<DependencyNotices close={vi.fn()} />, {
      createNodeMock: element => element.type === 'dialog' ? { showModal: vi.fn() } : null,
    });
  });
  return ui;
}

afterEach(() => vi.clearAllMocks());

it('presents the bundled notices, including the OCR engine records, byte-for-byte', async () => {
  const base = 'PDF Workstation third-party notices\npackage-a\nocr-engine: tesseract 5.5.3\nDeclared license: Apache-2.0\nocr-engine: leptonica 1.87.0\nDeclared license: BSD-2-Clause\n';
  vi.mocked(dependencyNotices).mockResolvedValue(base);
  const ui = await mount();
  const area = ui.root.findByType('textarea');
  expect(area.props.value).toBe(base);
  expect(ui.root.findAllByProps({ role: 'alert' })).toHaveLength(0);
  act(() => ui.unmount());
});

it('shows the native read error instead of partial notices', async () => {
  vi.mocked(dependencyNotices).mockRejectedValue('Could not read bundled notices: missing');
  const ui = await mount();
  expect(ui.root.findAllByType('textarea')).toHaveLength(0);
  expect(ui.root.findByProps({ role: 'alert' }).children).toEqual(['Could not read bundled notices: missing']);
  act(() => ui.unmount());
});
