import type { Annotation, DocumentAnnotations, ViewOnlyAnnotation } from './bridge';
import type { DocumentInfo } from './model';
import s from './CommentsPanel.module.css';

const typeNames: Record<string, string> = { Text: 'Sticky note', FreeText: 'Text box', Highlight: 'Highlight', Underline: 'Underline', StrikeOut: 'Strikethrough', Squiggly: 'Squiggly underline', Ink: 'Drawing', Line: 'Line', Square: 'Rectangle', Circle: 'Oval', Polygon: 'Polygon', PolyLine: 'Polygon line', Stamp: 'Stamp', Caret: 'Inserted text', FileAttachment: 'Attachment', Redact: 'Redaction mark (not applied)', Sound: 'Media (not played)', Movie: 'Media (not played)', Screen: 'Media (not played)', RichMedia: 'Media (not played)', '3D': '3D content (not shown)', Watermark: 'Watermark', Projection: 'Projection' };

function ViewOnlyList({ items, truncated }: { items: ViewOnlyAnnotation[]; truncated: number }) {
  if (!items.length && !truncated) return null;
  return <>
    <p>{items.length + truncated === 1 ? 'This annotation is shown as saved.' : `These ${items.length + truncated} annotations are shown as saved.`} This build cannot change them yet.</p>
    <ul className={s.viewOnly} aria-label="Annotations (view only)">{items.map((item, index) => <li key={index}>
      <strong>{item.reply ? 'Reply · ' : ''}{typeNames[item.subtype] ?? 'Annotation'} · Page {item.page + 1}</strong>
      {(item.author || item.modified) && <small>{[item.author, item.modified].filter(Boolean).join(' · ')}</small>}
      {item.contents && <span>{item.contents}</span>}
    </li>)}</ul>
    {truncated > 0 && <p>{truncated} more not listed.</p>}
  </>;
}

export default function CommentsPanel({ document, page, annotations, error, selectedId, onAddComment, onAddHighlight, onSelect, close }: { document: DocumentInfo; page: number; annotations: DocumentAnnotations | null; error: string; selectedId: string | null; onAddComment: () => void; onAddHighlight: () => void; onSelect: (annotation: Annotation) => void; close: () => void }) {
  return <aside className={s.panel} aria-label="Comments">
    <div className={s.heading}><h2>Comments</h2><button aria-label="Close comments" onClick={close}>×</button></div>
    {error ? <p role="alert">{error}</p> : !annotations ? <p role="status">Loading comments…</p> : annotations.status === 'unsupported' ? <><p role="status">Commenting is unavailable. {annotations.reason || 'This PDF has annotations or structure this build does not change.'}</p><ViewOnlyList items={annotations.viewOnly ?? []} truncated={annotations.viewOnlyTruncated ?? 0} /></> : <>
      <button className={s.add} onClick={onAddComment}>Add comment on page {page + 1}</button><button className={s.add} onClick={onAddHighlight}>Add area highlight on page {page + 1}</button>
      {!annotations.annotations.length ? <p>No comments or highlights yet.</p> : <div className={s.notes}>{annotations.annotations.map(annotation => <button key={annotation.id} className={annotation.id === selectedId ? s.selected : ''} onClick={() => onSelect(annotation)}><strong>{annotation.kind === 'note' ? 'Comment' : 'Highlight'} · Page {annotation.page + 1}</strong><span>{annotation.rect ? annotation.contents || 'No description' : `${annotation.contents || 'No description'} (hidden by the current crop)`}</span></button>)}</div>}
    </>}
  </aside>;
}
