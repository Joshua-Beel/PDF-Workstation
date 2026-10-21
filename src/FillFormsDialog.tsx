import { useEffect, useRef, useState } from 'react';
import type { DocumentFormFields, FormCheckboxField, FormChoiceField, FormField, FormPatch, FormRadioField, FormTextField, SavedCopy } from './bridge';
import type { DocumentInfo } from './model';
import s from './FillFormsDialog.module.css';

type DraftValue = string | boolean | null;

function initialValue(field: FormField): DraftValue { return field.kind === 'text' ? field.value : field.kind === 'checkbox' ? field.checked : field.selectedOptionId; }

function textDraft(field: FormTextField, values: Record<string, DraftValue>): string {
  const value = values[field.fieldId];
  return typeof value === 'string' ? value : field.value;
}

function checkboxDraft(field: FormCheckboxField, values: Record<string, DraftValue>): boolean {
  const value = values[field.fieldId];
  return typeof value === 'boolean' ? value : field.checked;
}

function choiceDraft(field: FormRadioField | FormChoiceField, values: Record<string, DraftValue>): string | null {
  const value = values[field.fieldId];
  return typeof value === 'string' || value === null ? value : field.selectedOptionId;
}

export function formPatches(fields: FormField[], values: Record<string, DraftValue>): FormPatch[] {
  const patches: FormPatch[] = [];
  for (const field of fields) {
    if (field.kind === 'text') {
      const value = textDraft(field, values);
      if (value !== field.value) patches.push({ fieldId: field.fieldId, kind: 'text', value });
    } else if (field.kind === 'checkbox') {
      const checked = checkboxDraft(field, values);
      if (checked !== field.checked) patches.push({ fieldId: field.fieldId, kind: 'checkbox', checked });
    } else {
      const optionId = choiceDraft(field, values);
      if (optionId === field.selectedOptionId) continue;
      if (field.kind === 'choice') patches.push({ fieldId: field.fieldId, kind: 'choice', optionId });
      else if (optionId !== null) patches.push({ fieldId: field.fieldId, kind: 'radio', optionId });
    }
  }
  return patches;
}

export function validateFormPatches(fields: FormField[], values: Record<string, DraftValue>, byteLimit: number): string | null {
  for (const field of fields) {
    const value = values[field.fieldId];
    if (value === undefined) continue;
    if (field.kind === 'radio' || field.kind === 'choice') {
      if (value === null && field.kind === 'radio' && field.selectedOptionId !== null) return `${field.name} keeps one option selected: the form author does not allow this radio group to be cleared.`;
      if (value !== null && (typeof value !== 'string' || !field.options.some(option => option.optionId === value))) return 'This form changed. Reload it and try again.';
    } else if (typeof value !== (field.kind === 'text' ? 'string' : 'boolean')) return 'This form changed. Reload it and try again.';
  }
  const patches = formPatches(fields, values);
  if (!patches.length) return 'Change at least one field before saving a new copy.';
  if (!Number.isSafeInteger(byteLimit) || byteLimit < 0) return 'This form has an invalid value limit.';
  for (const patch of patches) {
    if (patch.kind !== 'text') continue;
    const field = fields.find((item): item is FormTextField => item.fieldId === patch.fieldId && item.kind === 'text');
    if (!field) return 'This form changed. Reload it and try again.';
    if (!/^[\x20-\x7E\xA0-\xAC\xAE-\xFF]*$/.test(patch.value)) return `${field.name} accepts printable ASCII and Latin-1 characters only.`;
    if (new TextEncoder().encode(patch.value).byteLength > byteLimit) return `${field.name} exceeds the ${byteLimit.toLocaleString()}-byte value limit.`;
    if (field.maxLength !== null && patch.value.length > field.maxLength) return `${field.name} exceeds its ${field.maxLength.toLocaleString()}-character limit.`;
  }
  return null;
}

export default function FillFormsDialog({ document, formFields, error: loadError, busy, fill, close }: { document: DocumentInfo; formFields: DocumentFormFields | null; error: string; busy: boolean; fill: (id: number, revision: number, values: FormPatch[]) => Promise<SavedCopy | null>; close: () => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const inFlight = useRef(false);
  const initialized = useRef('');
  const selects = useRef<Record<string, HTMLSelectElement | null>>({});
  const [values, setValues] = useState<Record<string, DraftValue>>({});
  const [error, setError] = useState('');
  const [notice, setNotice] = useState('');
  const [submitting, setSubmitting] = useState(false);
  useEffect(() => { dialog.current?.showModal(); }, []);
  const fields = formFields?.status === 'supported' ? formFields.fields : [];
  const identity = formFields ? `${formFields.documentId}:${formFields.revision}:${fields.map(field => `${field.fieldId}:${field.kind}`).join(',')}` : '';
  useEffect(() => {
    if (!formFields || formFields.status !== 'supported' || initialized.current === identity) return;
    initialized.current = identity;
    setValues(Object.fromEntries(formFields.fields.map(field => [field.fieldId, initialValue(field)])));
  }, [formFields, identity]);
  const working = busy || submitting;
  const available = !!formFields && formFields.status === 'supported';
  const change = (fieldId: string, value: DraftValue) => { setValues(current => ({ ...current, [fieldId]: value })); setError(''); setNotice(''); };
  const submit = async () => {
    if (working || inFlight.current || !formFields || formFields.status !== 'supported') return;
    const validation = validateFormPatches(fields, values, formFields.valueByteLimit);
    if (validation) { setError(validation); return; }
    inFlight.current = true; setSubmitting(true); setError(''); setNotice('');
    try {
      const result = await fill(document.id, document.revision, formPatches(fields, values));
      if (result) close();
      else setNotice('Saving was canceled. Your entries are still here.');
    } catch (reason) { setError(String(reason)); }
    finally { inFlight.current = false; setSubmitting(false); }
  };
  return <dialog ref={dialog} className={s.dialog} aria-labelledby="fill-forms-title" onCancel={event => { event.preventDefault(); if (!working) close(); }}>
    <h2 id="fill-forms-title">Fill existing fields</h2>
    <p>Edit the supported fields below. Text fields accept printable ASCII and Latin-1 characters (such as accented letters). Windows will ask where to save a new PDF. Your open source PDF stays unchanged.</p>
    {loadError ? <p role="alert">{loadError}</p> : !formFields ? <p role="status">Checking this PDF for supported fields…</p> : formFields.status === 'unsupported' ? <p role="status">Filling is unavailable. {formFields.reason || 'This PDF has form fields this build does not change.'}</p> : !fields.length ? <p role="status">This PDF has no supported fields.</p> : <div className={s.fields}>{fields.map(field => {
      if (field.kind === 'text') return <label key={field.fieldId}><span>{field.name}<small>Page {field.page + 1}{field.maxLength === null ? '' : ` · ${field.maxLength} characters`}</small></span><input aria-label={field.name} value={textDraft(field, values)} disabled={working} maxLength={field.maxLength ?? undefined} onChange={event => change(field.fieldId, event.target.value)} /></label>;
      if (field.kind === 'checkbox') return <label className={s.checkbox} key={field.fieldId}><input type="checkbox" aria-label={`${field.name}, page ${field.page + 1}`} checked={checkboxDraft(field, values)} disabled={working} onChange={event => change(field.fieldId, event.target.checked)} /><span>{field.name}<small>Page {field.page + 1}</small></span></label>;
      const selected = choiceDraft(field, values);
      if (field.kind === 'radio') return <fieldset className={s.radio} key={field.fieldId}><legend>{field.name}<small>Page {field.page + 1}</small></legend>{field.options.map(option => <label key={option.optionId}><input type="radio" name={field.fieldId} aria-label={`${field.name}: ${option.label}, page ${field.page + 1}`} checked={selected === option.optionId} disabled={working} onChange={() => change(field.fieldId, option.optionId)} /><span>{option.label}</span></label>)}</fieldset>;
      return <div className={s.choice} key={field.fieldId}><label><span>{field.name}<small>Page {field.page + 1}</small></span><select ref={node => { selects.current[field.fieldId] = node; }} aria-label={`${field.name}, page ${field.page + 1}`} value={selected ?? ''} size={field.presentation === 'list' ? field.options.length : undefined} disabled={working} onChange={event => change(field.fieldId, event.target.value || null)}><option value="" disabled hidden>Choose an option</option>{field.options.map(option => <option key={option.optionId} value={option.optionId}>{option.label}</option>)}</select></label><button type="button" aria-label={`Clear selection: ${field.name}, page ${field.page + 1}`} disabled={working || selected === null} onClick={() => { change(field.fieldId, null); selects.current[field.fieldId]?.focus(); }}>Clear selection</button></div>;
    })}</div>}
    {error && <p role="alert">{error}</p>}
    {notice && <p role="status">{notice}</p>}
    {working && <p role="status">Choosing an output location and validating the filled PDF…</p>}
    <div className={s.actions}><button disabled={working} onClick={close}>Cancel</button><button className={s.confirm} disabled={working || !available || !fields.length} onClick={() => void submit()}>Save filled copy</button></div>
  </dialog>;
}
