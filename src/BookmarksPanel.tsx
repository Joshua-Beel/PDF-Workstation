import { useEffect, useState } from 'react';
import { documentBookmarks, type BookmarkList } from './bridge';
import type { BookmarkDestination } from './bookmarkDestination';
import type { DocumentInfo } from './model';
import s from './SearchPanel.module.css';

export default function BookmarksPanel({ document, go, close }: { document: DocumentInfo; go: (page: number, destination?: BookmarkDestination | null) => void; close: () => void }) {
  const [bookmarks, setBookmarks] = useState<BookmarkList | null>(null);
  const [error, setError] = useState('');
  useEffect(() => {
    let disposed = false;
    documentBookmarks(document.id, document.revision).then(value => { if (!disposed) setBookmarks(value); }).catch(e => { if (!disposed) setError(String(e)); });
    return () => { disposed = true; };
  }, [document.id, document.revision]);
  return <aside className={s.panel} aria-label="Document bookmarks">
    <div className={s.heading}><h2>Bookmarks</h2><button aria-label="Close bookmarks" onClick={close}>×</button></div>
    {error ? <p role="alert">{error}</p> : !bookmarks ? <p role="status">Loading bookmarks…</p> : <>
      {!bookmarks.items.length && <p>This document has no bookmarks.</p>}
      {bookmarks.truncated && <p role="status">Showing the first 1,000 bookmarks.</p>}
      <div className={s.results}>{bookmarks.items.map((bookmark, index) => <section key={index} style={{ paddingLeft: Math.min(bookmark.depth, 8) * 12 }}>
        <button disabled={bookmark.page === null} onClick={() => { if (bookmark.page !== null) { if (bookmark.destination) go(bookmark.page, bookmark.destination); else go(bookmark.page); } }}>{bookmark.title}</button>
        <p>{bookmark.page === null ? 'No supported destination in this document.' : `Page ${bookmark.page + 1}`}</p>
      </section>)}</div>
    </>}
  </aside>;
}
