import { useEffect, useRef, useState } from 'react';
import type { SavedCopy } from './bridge';
import { validateSearchableOcrCapability, validateSearchableOcrRequest, type CancelSearchableOcr, type CreateSearchableOcrCopy, type SearchableOcrCapability, type SearchableOcrTarget } from './searchableOcr';
import s from './CombineDialog.module.css';

export default function SearchableOcrDialog({ capability, target, createRequestId, create, cancel, close, opened }: { capability: SearchableOcrCapability; target: SearchableOcrTarget; createRequestId: () => string; create: CreateSearchableOcrCopy; cancel: CancelSearchableOcr; close: () => void; opened: (copy: SavedCopy) => void }) {
  const dialog = useRef<HTMLDialogElement>(null);
  const active = useRef(false);
  const activeRequest = useRef<string | null>(null);
  const attempt = useRef(0);
  const usedRequestIds = useRef(new Set<string>());
  const cancelSent = useRef(false);
  const [working,setWorking]=useState(false),[cancelling,setCancelling]=useState(false),[error,setError]=useState(''),[notice,setNotice]=useState('');
  useEffect(()=>{dialog.current?.showModal();},[]);
  const capabilityError=validateSearchableOcrCapability(capability),targetError=validateSearchableOcrRequest({requestId:'00000000-0000-4000-8000-000000000000',...target}),blocked=capabilityError||targetError;
  const submit=async()=>{if(active.current||blocked)return;let requestId:string;try{requestId=createRequestId();}catch(reason){setError(reason instanceof Error?reason.message:String(reason));return;}const request={requestId,...target},requestError=validateSearchableOcrRequest(request);if(requestError){setError(requestError);return;}if(usedRequestIds.current.has(requestId)){setError('Searchable OCR requires a fresh request UUID for every attempt.');return;}usedRequestIds.current.add(requestId);const current=++attempt.current;active.current=true;activeRequest.current=requestId;cancelSent.current=false;setWorking(true);setError('');setNotice('');try{const result=await create(request);if(attempt.current!==current)return;if(result){opened(result);close();}else setNotice('Save canceled. The source document and this dialog are unchanged.');}catch(reason){if(attempt.current===current)setError(reason instanceof Error?reason.message:String(reason));}finally{if(attempt.current===current){active.current=false;activeRequest.current=null;cancelSent.current=false;setWorking(false);setCancelling(false);}}};
  const stop=async()=>{const requestId=activeRequest.current,current=attempt.current;if(!active.current||!requestId||cancelSent.current)return;cancelSent.current=true;setCancelling(true);setError('');try{const ack=await cancel(requestId);if(attempt.current!==current||activeRequest.current!==requestId)return;if(ack.requestId!==requestId)throw new Error('OCR cancellation returned the wrong request ID.');setNotice(ack.status==='cancelled'?'Cancellation requested. Waiting for OCR to stop.':ack.status==='already_cancelled'?'Cancellation was already requested.':'OCR has already finished.');}catch(reason){if(attempt.current===current&&activeRequest.current===requestId)setError(reason instanceof Error?reason.message:String(reason));}};
  return <dialog ref={dialog} className={s.dialog} aria-labelledby="searchable-ocr-title" onCancel={event=>{event.preventDefault();if(!working)close();}}>
    <h2 id="searchable-ocr-title">Create searchable OCR copy</h2>
    <p>Creates a new copy from every current page at fixed 150 DPI using the verified local English OCR engine. The source document and its history stay unchanged.</p>
    <p className={s.preview}>Every page becomes a white-flattened raster with invisible searchable text carrying the engine's Unicode words, without control or whitespace characters. Multi-column reading order follows the engine and is not reordered. Editable text, vectors, forms, annotations, metadata, bookmarks, and attachments are not preserved as editable PDF structures.</p>
    <p className={s.preview}>Limited to 32 pages, 33,554,432 pixels across the document, 100,000 words, and 8 MiB of reconstructed text, 512 characters per word and 8,192 distinct characters. Control, invisible or private-use characters, protected or signed files, malformed input, stale revisions, timeouts, and existing output paths are refused without publishing a file.</p>
    {blocked&&<p role="alert">{blocked}</p>}{error&&<p role="alert">{error}</p>}{notice&&<p role="status">{notice}</p>}{working&&<p role="status">{cancelling?'Stopping OCR…':'Choosing an output location or processing OCR…'}</p>}
    <div className={s.actions}><button disabled={working} onClick={close}>Close</button>{working?<button className={s.confirm} disabled={cancelling} onClick={()=>void stop()}>Cancel OCR</button>:<button className={s.confirm} disabled={!!blocked} onClick={()=>void submit()}>Create searchable copy</button>}</div>
  </dialog>;
}
