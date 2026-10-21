import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { describe, expect, it, vi, type Mock } from 'vitest';
import FillFormsDialog, { formPatches, validateFormPatches } from './FillFormsDialog';
import type { DocumentFormFields, SavedCopy } from './bridge';
import type { DocumentInfo } from './model';

const document: DocumentInfo = { id: 4, name: 'form.pdf', path: 'C:/form.pdf', pages: [{ width: 612, height: 792 }], revision: 7, dirty: true, can_undo: true, can_redo: false };
const fields: DocumentFormFields = { documentId: 4, revision: 7, status: 'supported', reason: null, input: 'printable-latin1', valueByteLimit: 4096, fields: [{ kind: 'text', fieldId: 'name', name: 'Name', page: 0, value: 'Ada', maxLength: 12 }, { kind: 'text', fieldId: 'city', name: 'City', page: 0, value: '', maxLength: null }, { kind: 'checkbox', fieldId: 'approved', name: 'Approve terms', page: 1, checked: true }, { kind: 'checkbox', fieldId: 'updates', name: 'Receive updates', page: 1, checked: false }, { kind: 'radio', fieldId: 'contact', name: 'Contact method', page: 2, options: [{ optionId: 'contact-email', label: 'Email updates' }, { optionId: 'contact-post', label: 'Postal mail' }], selectedOptionId: 'contact-email' }, { kind: 'radio', fieldId: 'frequency', name: 'Contact frequency', page: 2, options: [{ optionId: 'frequency-weekly', label: 'Weekly' }, { optionId: 'frequency-monthly', label: 'Monthly' }], selectedOptionId: null }, { kind: 'choice', fieldId: 'shipping', name: 'Shipping method', page: 3, presentation: 'dropdown', options: [{ optionId: 'shipping-ground', label: 'Ground' }, { optionId: 'shipping-air', label: 'Air' }], selectedOptionId: 'shipping-ground' }, { kind: 'choice', fieldId: 'window', name: 'Delivery window', page: 3, presentation: 'list', options: [{ optionId: 'window-morning', label: 'Morning' }, { optionId: 'window-afternoon', label: 'Afternoon' }, { optionId: 'window-evening', label: 'Evening' }], selectedOptionId: null }] };
const output: SavedCopy = { path: 'C:/filled.pdf', document: { ...document, id: 8, name: 'filled.pdf', path: 'C:/filled.pdf', revision: 0, dirty: false, can_undo: false } };
const save = (ui: ReactTestRenderer) => ui.root.findAllByType('button').find(button => button.children.join('') === 'Save filled copy')!;

async function mount({ formFields = fields, error = '', busy = false, fill = vi.fn().mockResolvedValue(output), close = vi.fn() }: { formFields?: DocumentFormFields | null; error?: string; busy?: boolean; fill?: Mock<(...args: any[]) => any>; close?: Mock<(...args: any[]) => any> } = {}) {
  let ui!: ReactTestRenderer;
  await act(async () => { ui = create(<FillFormsDialog document={document} formFields={formFields} error={error} busy={busy} fill={fill} close={close} />, { createNodeMock: element => element.type === 'dialog' ? { showModal: vi.fn() } : null }); });
  return { ui, fill, close };
}

describe('FillFormsDialog', () => {
  it('uses tagged changed-only text, checkbox, and radio patches and states new-copy source preservation', async () => {
    const { ui, fill, close } = await mount();
    expect(JSON.stringify(ui.toJSON())).toContain('Your open source PDF stays unchanged.');
    act(() => ui.root.findByProps({ 'aria-label': 'Name' }).props.onChange({ target: { value: 'Ada Lovelace' } }));
    act(() => ui.root.findByProps({ 'aria-label': 'Approve terms, page 2' }).props.onChange({ target: { checked: false } }));
    act(() => ui.root.findByProps({ 'aria-label': 'Contact frequency: Monthly, page 3' }).props.onChange());
    act(() => ui.root.findByProps({ 'aria-label': 'Shipping method, page 4' }).props.onChange({ target: { value: 'shipping-air' } }));
    await act(async () => save(ui).props.onClick());
    expect(fill).toHaveBeenCalledWith(4, 7, [{ fieldId: 'name', kind: 'text', value: 'Ada Lovelace' }, { fieldId: 'approved', kind: 'checkbox', checked: false }, { fieldId: 'frequency', kind: 'radio', optionId: 'frequency-monthly' }, { fieldId: 'shipping', kind: 'choice', optionId: 'shipping-air' }]);
    expect(close).toHaveBeenCalledOnce();
    act(() => ui.unmount());
  });

  it('validates text without dropping blank, false, or blank-radio values and recognizes reverted inputs as unchanged', () => {
    const initial = { name: 'Ada', city: '', approved: true, updates: false, contact: 'contact-email', frequency: null, shipping: 'shipping-ground', window: null };
    expect(formPatches(fields.fields, initial)).toEqual([]);
    expect(formPatches(fields.fields, { ...initial, city: 'Paris', approved: false, updates: true, contact: 'contact-post', frequency: 'frequency-monthly', shipping: 'shipping-air', window: 'window-evening' })).toEqual([{ fieldId: 'city', kind: 'text', value: 'Paris' }, { fieldId: 'approved', kind: 'checkbox', checked: false }, { fieldId: 'updates', kind: 'checkbox', checked: true }, { fieldId: 'contact', kind: 'radio', optionId: 'contact-post' }, { fieldId: 'frequency', kind: 'radio', optionId: 'frequency-monthly' }, { fieldId: 'shipping', kind: 'choice', optionId: 'shipping-air' }, { fieldId: 'window', kind: 'choice', optionId: 'window-evening' }]);
    expect(validateFormPatches(fields.fields, initial, 4096)).toContain('Change at least one');
    expect(validateFormPatches(fields.fields, { ...initial, name: '世界' }, 4096)).toContain('Latin-1');
    expect(validateFormPatches(fields.fields, { ...initial, name: 'Åda Café' }, 4096)).toBeNull();
    expect(validateFormPatches(fields.fields, { ...initial, name: 'soft­hyphen' }, 4096)).toContain('Latin-1');
    expect(validateFormPatches(fields.fields, { ...initial, contact: null }, 4096)).toContain('does not allow this radio group to be cleared');
    expect(formPatches(fields.fields, { ...initial, contact: null, frequency: null })).toEqual([]);
    expect(validateFormPatches(fields.fields, { ...initial, frequency: 'unknown' }, 4096)).toContain('form changed');
    expect(validateFormPatches(fields.fields, { ...initial, shipping: null }, 4096)).toBeNull();
    expect(formPatches(fields.fields, { ...initial, shipping: null, window: null })).toEqual([{ fieldId: 'shipping', kind: 'choice', optionId: null }]);
    expect(validateFormPatches(fields.fields, { ...initial, window: 'unknown' }, 4096)).toContain('form changed');
    const name = fields.fields[0];
    if (name.kind !== 'text') throw new Error('fixture field changed');
    expect(validateFormPatches([{ ...name, maxLength: 3 }], { name: 'ABCD' }, 4096)).toContain('3-character');
    expect(validateFormPatches([{ ...name, maxLength: null }], { name: 'ABCD' }, 3)).toContain('3-byte');
  });

  it('shows unsupported native reasons without partial fields', async () => {
    const { ui } = await mount({ formFields: { ...fields, status: 'unsupported', reason: 'Unsupported field appearance.', fields: [] } });
    expect(ui.root.findByProps({ role: 'status' }).children.join('')).toContain('Unsupported field appearance.');
    expect(ui.root.findAllByProps({ 'aria-label': 'Name' })).toHaveLength(0);
    expect(save(ui).props.disabled).toBe(true);
    act(() => ui.unmount());
  });

  it('uses a keyboard-accessible source-named checkbox with its page and preserves its cancellation draft', async () => {
    const { ui, fill, close } = await mount({ fill: vi.fn().mockResolvedValue(null) });
    const checkbox = ui.root.findByProps({ 'aria-label': 'Receive updates, page 2' });
    expect(checkbox.props.type).toBe('checkbox');
    act(() => checkbox.props.onChange({ target: { checked: true } }));
    await act(async () => save(ui).props.onClick());
    expect(fill).toHaveBeenCalledWith(4, 7, [{ fieldId: 'updates', kind: 'checkbox', checked: true }]);
    expect(close).not.toHaveBeenCalled();
    expect(ui.root.findByProps({ 'aria-label': 'Receive updates, page 2' }).props.checked).toBe(true);
    act(() => ui.unmount());
  });

  it('uses independent source-named radio groups, including a supported blank group', async () => {
    const { ui, fill, close } = await mount({ fill: vi.fn().mockResolvedValue(null) });
    const email = ui.root.findByProps({ 'aria-label': 'Contact method: Email updates, page 3' });
    const post = ui.root.findByProps({ 'aria-label': 'Contact method: Postal mail, page 3' });
    const weekly = ui.root.findByProps({ 'aria-label': 'Contact frequency: Weekly, page 3' });
    expect(email.props.type).toBe('radio');
    expect(email.props.name).toBe('contact');
    expect(email.props.checked).toBe(true);
    expect(post.props.checked).toBe(false);
    expect(weekly.props.name).toBe('frequency');
    expect(weekly.props.checked).toBe(false);
    act(() => post.props.onChange());
    act(() => weekly.props.onChange());
    await act(async () => save(ui).props.onClick());
    expect(fill).toHaveBeenCalledWith(4, 7, [{ fieldId: 'contact', kind: 'radio', optionId: 'contact-post' }, { fieldId: 'frequency', kind: 'radio', optionId: 'frequency-weekly' }]);
    expect(close).not.toHaveBeenCalled();
    act(() => ui.unmount());
  });

  it('clears dropdown and list selections with a labelled keyboard button and sends a null choice patch, but offers no radio clear', async () => {
    const { ui, fill, close } = await mount({ fill: vi.fn().mockResolvedValue(null) });
    const clearShipping = ui.root.findByProps({ 'aria-label': 'Clear selection: Shipping method, page 4' });
    const clearWindow = ui.root.findByProps({ 'aria-label': 'Clear selection: Delivery window, page 4' });
    expect(clearShipping.type).toBe('button');
    expect(clearShipping.props.type).toBe('button');
    expect(clearShipping.children.join('')).toBe('Clear selection');
    expect(clearShipping.props.disabled).toBe(false);
    expect(clearWindow.props.disabled).toBe(true);
    expect(ui.root.findAllByType('button').filter(button => String(button.props['aria-label'] ?? '').includes('Contact'))).toHaveLength(0);
    expect(ui.root.findAllByType('button').filter(button => button.children.join('') === 'Clear selection')).toHaveLength(2);
    act(() => ui.root.findByProps({ 'aria-label': 'Delivery window, page 4' }).props.onChange({ target: { value: 'window-morning' } }));
    expect(ui.root.findByProps({ 'aria-label': 'Clear selection: Delivery window, page 4' }).props.disabled).toBe(false);
    act(() => ui.root.findByProps({ 'aria-label': 'Delivery window, page 4' }).props.onChange({ target: { value: '' } }));
    act(() => clearShipping.props.onClick());
    expect(ui.root.findByProps({ 'aria-label': 'Shipping method, page 4' }).props.value).toBe('');
    expect(ui.root.findByProps({ 'aria-label': 'Clear selection: Shipping method, page 4' }).props.disabled).toBe(true);
    await act(async () => save(ui).props.onClick());
    expect(fill).toHaveBeenCalledWith(4, 7, [{ fieldId: 'shipping', kind: 'choice', optionId: null }]);
    expect(close).not.toHaveBeenCalled();
    expect(ui.root.findByProps({ 'aria-label': 'Shipping method, page 4' }).props.value).toBe('');
    act(() => ui.unmount());
  });

  it('disables choice clearing while busy', async () => {
    const { ui } = await mount({ busy: true });
    expect(ui.root.findByProps({ 'aria-label': 'Clear selection: Shipping method, page 4' }).props.disabled).toBe(true);
    act(() => ui.unmount());
  });

  it('uses source-named dropdown and list choices without an inferred clear value', async () => {
    const { ui, fill, close } = await mount({ fill: vi.fn().mockResolvedValue(null) });
    const dropdown = ui.root.findByProps({ 'aria-label': 'Shipping method, page 4' });
    const list = ui.root.findByProps({ 'aria-label': 'Delivery window, page 4' });
    expect(dropdown.type).toBe('select');
    expect(dropdown.props.value).toBe('shipping-ground');
    expect(dropdown.props.size).toBeUndefined();
    expect(list.type).toBe('select');
    expect(list.props.value).toBe('');
    expect(list.props.size).toBe(3);
    expect(list.findAllByType('option').map(option => option.children.join(''))).toEqual(['Choose an option', 'Morning', 'Afternoon', 'Evening']);
    act(() => dropdown.props.onChange({ target: { value: 'shipping-air' } }));
    act(() => list.props.onChange({ target: { value: 'window-evening' } }));
    await act(async () => save(ui).props.onClick());
    expect(fill).toHaveBeenCalledWith(4, 7, [{ fieldId: 'shipping', kind: 'choice', optionId: 'shipping-air' }, { fieldId: 'window', kind: 'choice', optionId: 'window-evening' }]);
    expect(close).not.toHaveBeenCalled();
    expect(ui.root.findByProps({ 'aria-label': 'Delivery window, page 4' }).props.value).toBe('window-evening');
    act(() => ui.unmount());
  });

  it('keeps drafts after native cancellation or failure and blocks Escape and duplicate submissions while busy', async () => {
    const canceled = await mount({ fill: vi.fn().mockResolvedValue(null) });
    act(() => canceled.ui.root.findByProps({ 'aria-label': 'City' }).props.onChange({ target: { value: 'Paris' } }));
    await act(async () => save(canceled.ui).props.onClick());
    expect(canceled.close).not.toHaveBeenCalled();
    expect(canceled.ui.root.findByProps({ 'aria-label': 'City' }).props.value).toBe('Paris');
    expect(canceled.ui.root.findByProps({ role: 'status' }).children.join('')).toContain('Saving was canceled');
    act(() => canceled.ui.unmount());

    let reject!: (reason: Error) => void;
    const fill = vi.fn().mockImplementation(() => new Promise<SavedCopy | null>((_, fail) => { reject = fail; }));
    const pending = await mount({ fill });
    act(() => pending.ui.root.findByProps({ 'aria-label': 'City' }).props.onChange({ target: { value: 'Paris' } }));
    act(() => { save(pending.ui).props.onClick(); save(pending.ui).props.onClick(); });
    expect(fill).toHaveBeenCalledOnce();
    act(() => pending.ui.root.findByType('dialog').props.onCancel({ preventDefault: vi.fn() }));
    expect(pending.close).not.toHaveBeenCalled();
    await act(async () => reject(new Error('Document changed.')));
    expect(pending.ui.root.findByProps({ role: 'alert' }).children.join('')).toContain('Document changed');
    expect(pending.ui.root.findByProps({ 'aria-label': 'City' }).props.value).toBe('Paris');
    act(() => pending.ui.unmount());
  });
});
