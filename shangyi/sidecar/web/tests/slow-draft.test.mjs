import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';
import {render as renderChat,destroy as destroyChat} from '../js/views/chat.js';
import {render as renderStart,destroy as destroyStart} from '../js/views/run.js';

const CHAT_ID='chat-000000000000000000000099';
const CHAT_DRAFT='shangyi-chat-draft:'+CHAT_ID;
const START_DRAFT='shangyi-freeform-draft-v1';
const snapshot=(extra={})=>({id:CHAT_ID,title:'租约咨询',topic:'negotiation',status:'thinking',alive:true,
 revision:1,messages:[],pending:null,...extra});
function deferred(){let resolve,reject;const promise=new Promise((yes,no)=>{resolve=yes;reject=no;});return {promise,resolve,reject};}

async function withSurface(run){
 const names=['document','sessionStorage','window','location','setTimeout','setInterval','clearInterval'];
 const previous=Object.fromEntries(names.map(key=>[key,globalThis[key]]));
 const savedApi={...api},nodes=new Map(),storage=new Map();
 function node(id){
  if(!nodes.has(id))nodes.set(id,{id,value:'',textContent:'',innerHTML:'',disabled:false,hidden:false,
   scrollHeight:600,scrollTop:0,clientHeight:600,events:{},focus(){},
   setAttribute(key,value){this[key]=key==='disabled'?true:value;},
   addEventListener(type,handler){this.events[type]=handler;}});
  return nodes.get(id);
 }
 const view={innerHTML:'',querySelector:selector=>node(selector.slice(1)),querySelectorAll(){return [];}};
 globalThis.document={getElementById:node,querySelector:node};
 globalThis.sessionStorage={getItem:key=>storage.get(key)??null,setItem:(key,value)=>storage.set(key,String(value)),removeItem:key=>storage.delete(key)};
 globalThis.window={dispatchEvent(){}};globalThis.location={hash:'#/run'};
 globalThis.setTimeout=()=>0;globalThis.setInterval=()=>0;globalThis.clearInterval=()=>{};
 api.health=async()=>({frontend_contract:3,interactive_chat:true,services:{search:{status:'available'}}});
 try{await run({node,view,storage});}
 finally{
  destroyChat();destroyStart();Object.assign(api,savedApi);
  for(const key of names)globalThis[key]=previous[key];
 }
}

test('a slow chat send retains later edits and sends only the originally submitted text',async()=>{
 await withSurface(async({node,view,storage})=>{
  const pending=deferred();let sent;
  api.chat=async()=>snapshot();api.chatMessage=async(_id,body)=>{sent=body;return pending.promise;};
  await renderChat(view,{match:['',CHAT_ID],isCurrent:()=>true});
  node('chat-input').value='租金每月一万元';node('chat-input').oninput();
  const sending=node('chat-send').onclick();
  node('chat-input').value='刚才漏了，合同还剩两年';node('chat-input').oninput();
  pending.resolve(snapshot({revision:2}));await sending;
  assert.equal(sent.text,'租金每月一万元');
  assert.equal(storage.get(CHAT_DRAFT),'刚才漏了，合同还剩两年');
  assert.match(node('composer-host').innerHTML,/刚才漏了，合同还剩两年/);
 });
});

test('a chat send clears only an unchanged submitted draft',async()=>{
 await withSurface(async({node,view,storage})=>{
  api.chat=async()=>snapshot();api.chatMessage=async()=>snapshot({revision:2});
  await renderChat(view,{match:['',CHAT_ID],isCurrent:()=>true});
  node('chat-input').value='已确认租约期限';node('chat-input').oninput();
  await node('chat-send').onclick();
  assert.equal(storage.has(CHAT_DRAFT),false);
  assert.ok(!node('composer-host').innerHTML.includes('已确认租约期限'));
 });
});

test('using the unsure shortcut does not erase a separately typed draft',async()=>{
 await withSurface(async({node,view,storage})=>{
  api.chat=async()=>snapshot({status:'waiting',pending:{kind:'question',request_id:'request-1',payload:{questions:[]}}});
  api.chatMessage=async()=>snapshot({revision:2});
  await renderChat(view,{match:['',CHAT_ID],isCurrent:()=>true});
  node('chat-input').value='这句补充还没有写完';node('chat-input').oninput();
  await node('not-sure').events.click();
  assert.equal(storage.get(CHAT_DRAFT),'这句补充还没有写完');
  assert.match(node('composer-host').innerHTML,/这句补充还没有写完/);
 });
});

test('a late chat failure does not place its error into a different conversation',async()=>{
 await withSurface(async({node,view})=>{
  const pending=deferred();let current=true;
  api.chat=async()=>snapshot();api.chatMessage=()=>pending.promise;
  await renderChat(view,{match:['',CHAT_ID],isCurrent:()=>current});
  node('chat-input').value='原会话的消息';node('chat-input').oninput();
  const sending=node('chat-send').onclick();
  current=false;node('chat-send-error').textContent='新会话自己的状态';
  pending.reject(new Error('原会话发送失败'));await sending;
  assert.equal(node('chat-send-error').textContent,'新会话自己的状态');
 });
});

test('a slow consultation start preserves edits made after submission as an unsent opening draft',async()=>{
 await withSurface(async({node,view,storage})=>{
  const pending=deferred();let sent;
  api.startChat=body=>{sent=body;return pending.promise;};
  await renderStart(view,{match:[],isCurrent:()=>true});await Promise.resolve();
  node('brief-input').value='准备和房东谈续租';node('brief-input').oninput();
  const sending=node('start-conversation').onsubmit({preventDefault(){}});
  node('brief-input').value='改写后的问题，准备谈三年续租';node('brief-input').oninput();
  pending.resolve({id:CHAT_ID});await sending;
  assert.equal(sent.text,'准备和房东谈续租');
  assert.equal(JSON.parse(storage.get(START_DRAFT)).text,'改写后的问题，准备谈三年续租');
  assert.equal(location.hash,'#/chat/'+CHAT_ID);
  assert.match(node('toast').textContent,/没发送/);
 });
});

test('a late consultation start cannot erase a draft written on a newer page',async()=>{
 await withSurface(async({node,view,storage})=>{
  const pending=deferred();let current=true;
  api.startChat=()=>pending.promise;
  await renderStart(view,{match:[],isCurrent:()=>current});await Promise.resolve();
  node('brief-input').value='旧页面的问题';node('brief-input').oninput();
  const sending=node('start-conversation').onsubmit({preventDefault(){}});
  current=false;
  storage.set(START_DRAFT,JSON.stringify({text:'新页面正在写的新问题',topic:'market_entry',mode:'scenario',action:''}));
  pending.resolve({id:CHAT_ID});await sending;
  assert.equal(JSON.parse(storage.get(START_DRAFT)).text,'新页面正在写的新问题');
  assert.equal(location.hash,'#/run');
 });
});

test('a successful consultation start removes its unchanged opening draft',async()=>{
 await withSurface(async({node,view,storage})=>{
  api.startChat=async()=>({id:CHAT_ID});
  await renderStart(view,{match:[],isCurrent:()=>true});await Promise.resolve();
  node('brief-input').value='已经提交的问题';node('brief-input').oninput();
  await node('start-conversation').onsubmit({preventDefault(){}});
  assert.equal(storage.has(START_DRAFT),false);
 });
});

test('a failed stop shows an inline reason, retains the conversation, and enables retry',async()=>{
 await withSurface(async({node,view})=>{
  const pending=deferred();let calls=0;
  api.chat=async()=>snapshot();api.stopChat=()=>{calls++;return pending.promise;};
  await renderChat(view,{match:['',CHAT_ID],isCurrent:()=>true});
  const stopping=node('chat-stop').events.click();
  assert.equal(node('chat-stop').disabled,true);
  await node('chat-stop').events.click();assert.equal(calls,1);
  pending.reject(new Error('连接暂时中断'));await stopping;
  assert.equal(node('chat-stop').disabled,false);
  assert.match(node('chat-stop-error').textContent,/连接暂时中断/);
  assert.match(node('chat-stop-error').textContent,/再试一次/);
  assert.match(node('composer-host').innerHTML,/chat-input/);
 });
});
