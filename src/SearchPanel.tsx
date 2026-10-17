import { useEffect, useRef, useState } from 'react';
import { pageText } from './bridge';
import type { DocumentInfo } from './model';
import { literalMatches, type SearchHighlightQuery } from './searchHighlights';
import s from './SearchPanel.module.css';

const PREFETCH_PAGES = 8;
const STATUS_INTERVAL_MS = 120;
type Hit ={ page: number; before: string; match: string; after: string };
export type ActiveSearch = SearchHighlightQuery & { documentId: number; revision: number; pages: number[] };
export default function SearchPanel({ document, go, close, onHighlights }: { document: DocumentInfo; go: (page: number) => void; close: () => void; onHighlights?: (search: ActiveSearch | null) => void }) {
  const [query, setQuery] = useState('');
  const [matchCase, setMatchCase] = useState(false);
  const [hits, setHits] = useState<Hit[]>([]);
  const [selected, setSelected] = useState(-1);
  const [status, setStatus] = useState('Search embedded text. Scanned images need OCR first.');
  const [running, setRunning] = useState(false);
  const [error, setError] = useState('');
  const generation = useRef(0);
  useEffect(() => () => { generation.current++; onHighlights?.(null); }, [onHighlights]);
  const cancel = () => { generation.current++; setRunning(false); setStatus('Search stopped. Results below may be incomplete.'); };
  const navigate = (index: number) => { if (hits[index]) { setSelected(index); go(hits[index].page); } };
  const search = async () => {
    const needle = query.trim();
    if (!needle) return;
    const request = ++generation.current;
    setHits([]); setSelected(-1); setError(''); setRunning(true);
    onHighlights?.(null);
    const found: Hit[] = [];
    try {
      const pending = new Map<number, Promise<string>>();
      const fetchAhead = (from: number) => {
        for (let ahead = from; ahead < Math.min(document.pages.length, from + PREFETCH_PAGES); ahead++) {
          if (pending.has(ahead)) continue;
          const text = pageText(document.id, ahead, document.revision);
          text.catch(() => undefined);
          pending.set(ahead, text);
        }
      };
      let lastStatus = 0;
      for (let page = 0; page < document.pages.length; page++) {
        if (request !== generation.current) return;
        const now = Date.now();
        if (page === 0 || now - lastStatus >= STATUS_INTERVAL_MS) { lastStatus = now; setStatus(`Searching page ${page + 1} of ${document.pages.length}…`); }
        fetchAhead(page);
        const text = await pending.get(page)!;
        pending.delete(page);
        if (request !== generation.current) return;
        const match = literalMatches(text, needle, matchCase, 1)[0];
        if (match) {
          const start = Math.max(0, match.start - 70);
          const end = Math.min(text.length, match.end + 110);
          found.push({ page, before: `${start ? '…' : ''}${text.slice(start, match.start)}`, match: text.slice(match.start, match.end), after: `${text.slice(match.end, end)}${end < text.length ? '…' : ''}` });
          setHits([...found]);
          onHighlights?.({ documentId: document.id, revision: document.revision, query: needle, matchCase, pages: found.map(hit => hit.page) });
        }
        if (found.length === 500) { setStatus('Showing the first 500 matching pages. Narrow your search to see fewer results.'); return; }
      }
      setStatus(found.length ? `${found.length} matching ${found.length === 1 ? 'page' : 'pages'}.` : 'No matching text found. Scanned images need OCR first.');
    } catch (e) {
      if (request === generation.current) { setError(String(e)); setStatus('Search could not finish. Results below may be incomplete.'); }
    } finally { if (request === generation.current) setRunning(false); }
  };
  return <aside className={s.panel} aria-label="Find in document">
    <div className={s.heading}><h2>Find text</h2><button aria-label="Close search" onClick={close}>×</button></div>
    <form onSubmit={event => { event.preventDefault(); void search(); }}>
      <input autoFocus aria-label="Find in document" value={query} maxLength={500} onChange={event => { cancel(); onHighlights?.(null); setQuery(event.target.value); setHits([]); setSelected(-1); setError(''); setStatus('Press Enter to search.'); }} />
      <label><input type="checkbox" checked={matchCase} onChange={event => { cancel(); onHighlights?.(null); setMatchCase(event.target.checked); setHits([]); setSelected(-1); setError(''); setStatus('Press Enter to search.'); }} /> Match case</label>
      <div className={s.actions}><button type="submit" disabled={!query.trim()}>Search</button>{running && <button type="button" onClick={cancel}>Stop</button>}</div>
    </form>
    <p role="status">{status}</p>
    {error && <p role="alert">{error}</p>}
    <div className={s.actions}><button disabled={!hits.length} onClick={() => navigate(selected <= 0 ? hits.length - 1 : selected - 1)}>Previous matching page</button><button disabled={!hits.length} onClick={() => navigate((selected + 1) % hits.length)}>Next matching page</button></div>
    {selected >= 0 && <p aria-live="polite">Matching page {selected + 1} of {hits.length}</p>}
    <div className={s.results}>{hits.map((hit, index) => <section key={hit.page}><button aria-current={selected === index ? 'location' : undefined} onClick={() => navigate(index)}>Go to page {hit.page + 1}</button><p>{hit.before}<mark>{hit.match}</mark>{hit.after}</p></section>)}</div>
  </aside>;
}
