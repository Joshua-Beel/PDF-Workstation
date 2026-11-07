import { act, create, type ReactTestRenderer } from 'react-test-renderer';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import App from './App';
import Viewer from './Viewer';
import { documentAnnotations, editPages, openDocument, redactDocument, saveCopy } from './bridge';
import type { DocumentInfo } from './model';

vi.mock('./bridge', () => ({ native:false, openDocument:vi.fn(), reopenDocument:vi.fn(), closeDocument:vi.fn(), editPages:vi.fn(), saveCopy:vi.fn(), splitDocument:vi.fn(), cropPages:vi.fn(), resetCrops:vi.fn(), combineDocuments:vi.fn(), insertPagesCopy:vi.fn(), replacePagesCopy:vi.fn(), documentFormFields:vi.fn(), fillFormCopy:vi.fn(), documentAnnotations:vi.fn(), documentPageLabels:vi.fn(), createPdfFromImage:vi.fn(), exportPageImage:vi.fn(), redactDocument:vi.fn(), createComment:vi.fn(), updateComment:vi.fn(), deleteComment:vi.fn(), createHighlight:vi.fn(), createTextHighlight:vi.fn(), updateHighlight:vi.fn(), deleteHighlight:vi.fn() }));
vi.mock('./Viewer', () => ({ default:(props:{redactionPage:number|null;redactions:unknown[];onRedactionCreate:(page:number,rect:{x:number;y:number;width:number;height:number})=>void;onTextSelection:(selection:{id:number;page:number;revision:number;start:number;end:number},source:{id:number;page:number;revision:number})=>void}) => <div data-redaction-page={props.redactionPage} data-redaction-count={props.redactions.length}><button onClick={() => props.onRedactionCreate(props.redactionPage ?? 0,{x:61.2,y:79.2,width:122.4,height:158.4})}>Draw valid redaction</button><button onClick={() => props.onRedactionCreate(props.redactionPage ?? 0,{x:-1,y:0,width:1,height:1})}>Draw invalid redaction</button><button onClick={() => props.onTextSelection({id:7,page:0,revision:4,start:1,end:2},{id:7,page:0,revision:4})}>Select source text</button></div> }));

const source:DocumentInfo={id:7,name:'source.pdf',path:'C:/source.pdf',pages:[{width:612,height:792},{width:792,height:612}],revision:4,dirty:false,can_undo:false,can_redo:false};
const output:DocumentInfo={id:8,name:'redacted.pdf',path:'C:/redacted.pdf',pages:source.pages,revision:0,dirty:false,can_undo:false,can_redo:false};
let keydown:((event:KeyboardEvent)=>void)|undefined;
beforeEach(() => { vi.clearAllMocks(); vi.mocked(documentAnnotations).mockResolvedValue({documentId:7,revision:4,status:'supported',reason:null,annotations:[],viewOnly:[],viewOnlyTruncated:0}); const storage=new Map(); keydown=undefined; vi.stubGlobal('window',{localStorage:{getItem:(key:string)=>storage.get(key)??null,setItem:(key:string,value:string)=>storage.set(key,value)},addEventListener:(type:string,listener:(event:KeyboardEvent)=>void)=>{if(type==='keydown')keydown=listener;},removeEventListener:vi.fn()}); });
afterEach(() => vi.unstubAllGlobals());
const namedButton=(ui:ReactTestRenderer,name:string)=>ui.root.findAllByType('button').find(button=>button.children.join('')===name)!;
async function open(ui:ReactTestRenderer){vi.mocked(openDocument).mockResolvedValue({status:'opened',document:source});await act(async()=>namedButton(ui,'Open a file').props.onClick());}
async function launch(ui:ReactTestRenderer){act(()=>ui.root.findByProps({'aria-label':'Home'}).props.onClick());act(()=>ui.root.findAllByType('button').find(button=>button.children.includes('All tools'))!.props.onClick());act(()=>ui.root.findAllByType('button').find(button=>button.props.title==='Permanently blank marked areas in a rasterized copy')!.props.onClick());}

it('sends captured page-point rectangles and retains the draft on picker cancel',async()=>{
  vi.mocked(redactDocument).mockResolvedValueOnce(null).mockResolvedValueOnce({path:output.path,document:output});
  let ui!:ReactTestRenderer;act(()=>{ui=create(<App/>);});await open(ui);act(()=>namedButton(ui,'Select source text').props.onClick());await act(async()=>{await Promise.resolve();await Promise.resolve();});await launch(ui);
  expect(ui.root.findByType(Viewer).props.redactionPage).toBe(0);
  act(()=>namedButton(ui,'Draw invalid redaction').props.onClick());expect(ui.root.findByType(Viewer).props.redactions).toHaveLength(0);
  act(()=>namedButton(ui,'Draw valid redaction').props.onClick());expect(ui.root.findByType(Viewer).props.redactions).toHaveLength(1);
  expect(ui.root.findByProps({'aria-label':'Highlight selected text'}).props.disabled).toBe(true);
  expect(ui.root.findByProps({'aria-label':'Fill existing fields'}).props.disabled).toBe(true);
  act(()=>keydown!({key:'s',ctrlKey:true,preventDefault:vi.fn(),target:{matches:()=>false}} as unknown as KeyboardEvent));
  act(()=>keydown!({key:'z',ctrlKey:true,preventDefault:vi.fn(),target:{matches:()=>false}} as unknown as KeyboardEvent));
  expect(saveCopy).not.toHaveBeenCalled();expect(editPages).not.toHaveBeenCalled();expect(ui.root.findByType(Viewer).props.redactions).toHaveLength(1);
  await act(async()=>namedButton(ui,'Create redacted copy…').props.onClick());
  expect(redactDocument).toHaveBeenLastCalledWith({id:7,revision:4,page:0,rectangles:[{x:61.2,y:79.2,width:122.4,height:158.4}]});
  expect(ui.root.findAllByProps({'aria-label':'Redact a PDF'})).toHaveLength(1);
  await act(async()=>namedButton(ui,'Create redacted copy…').props.onClick());
  expect(ui.root.findAllByProps({'aria-label':'Redact a PDF'})).toHaveLength(0);
  expect(JSON.stringify(ui.toJSON())).toContain('redacted.pdf');
  act(()=>ui.unmount());
});

it('closes an unsubmitted draft with Escape without invoking native code',async()=>{
  let ui!:ReactTestRenderer;act(()=>{ui=create(<App/>);});await open(ui);await launch(ui);act(()=>namedButton(ui,'Draw valid redaction').props.onClick());
  act(()=>keydown!({key:'Escape',ctrlKey:false,target:{matches:()=>false}} as unknown as KeyboardEvent));
  expect(ui.root.findAllByProps({'aria-label':'Redact a PDF'})).toHaveLength(0);expect(redactDocument).not.toHaveBeenCalled();act(()=>ui.unmount());
});

it('retains marked areas after a native refusal',async()=>{
  vi.mocked(redactDocument).mockRejectedValue(new Error('Unsupported source'));
  let ui!:ReactTestRenderer;act(()=>{ui=create(<App/>);});await open(ui);await launch(ui);act(()=>namedButton(ui,'Draw valid redaction').props.onClick());
  await act(async()=>namedButton(ui,'Create redacted copy…').props.onClick());
  expect(ui.root.findByType(Viewer).props.redactions).toHaveLength(1);expect(JSON.stringify(ui.toJSON())).toContain('Unsupported source');act(()=>ui.unmount());
});

it('blocks visible document switching and closing while areas are marked',async()=>{
  const other={...source,id:9,name:'other.pdf',path:'C:/other.pdf'};
  vi.mocked(openDocument).mockResolvedValueOnce({status:'opened',document:source}).mockResolvedValueOnce({status:'opened',document:other});
  let ui!:ReactTestRenderer;act(()=>{ui=create(<App/>);});
  const openButton=()=>ui.root.findAllByType('button').find(button=>button.children.includes('Open a file'))!;
  await act(async()=>openButton().props.onClick());await act(async()=>openButton().props.onClick());await launch(ui);act(()=>namedButton(ui,'Draw valid redaction').props.onClick());
  const sourceTab=ui.root.findAllByType('button').find(button=>button.findAllByType('span').some(span=>span.children.includes('source.pdf')))!;
  const otherTab=ui.root.findAllByType('button').find(button=>button.findAllByType('span').some(span=>span.children.includes('other.pdf')))!;
  expect(sourceTab.props.disabled).toBe(true);expect(otherTab.props.disabled).toBe(true);expect(ui.root.findByProps({'aria-label':'Close other.pdf'}).props.disabled).toBe(true);expect(ui.root.findByProps({'aria-label':'Home'}).props.disabled).toBe(true);
  act(()=>sourceTab.props.onClick());expect(ui.root.findByType(Viewer).props.document.id).toBe(9);expect(ui.root.findByType(Viewer).props.redactions).toHaveLength(1);act(()=>ui.unmount());
});
