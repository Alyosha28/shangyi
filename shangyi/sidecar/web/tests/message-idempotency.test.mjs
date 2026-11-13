import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';
import {render,destroy} from '../js/views/chat.js';

const id='chat-000000000000000000000077',draftKey='shangyi-chat-draft:'+id,pendingKey='shangyi-chat-pending:'+id;
const snapshot=(extra={})=>({id,title:'租约咨询',topic:'negotiation',status:'waiting',alive:true,revision:1,messages:[],
 pending:{kind:'question',request_id:'question-1',payload:{questions:[]}},...extra});
const receipt=(body,extra={})=>snapshot({status:'thinking',pending:null,revision:2,
 messages:[{type:'user',message_id:body.message_id,text:body.text,delivery:'received'}],...extra});

async function withSurface(run){
 const names=['document','sessionStorage','setInterval','clearInterval','setTimeout'];
 const originals=Object.fromEntries(names.map(k=>[k,globalThis[k]])),originalApi={...api};
 const storage=new Map(),nodes=new Map();let poll;
 const node=key=>{
  if(!nodes.has(key))nodes.set(key,{value:'',innerHTML:'',textContent:'',disabled:false,events:{},
   scrollHeight:600,clientHeight:600,scrollTop:0,focus(){},setAttribute(){},
   addEventListener(type,callback){this.events[type]=callback;}});
  return nodes.get(key);
 };
 const view={innerHTML:'',querySelector:selector=>node(selector.slice(1)),querySelectorAll(){return [];}};
 globalThis.document={getElementById:node};
 globalThis.sessionStorage={getItem:key=>storage.get(key)??null,setItem:(key,value)=>storage.set(key,value),removeItem:key=>storage.delete(key)};
 globalThis.setInterval=callback=>{poll=callback;return 1;};globalThis.clearInterval=()=>{};globalThis.setTimeout=()=>0;
 const mount=()=>render(view,{match:['',id],isCurrent:()=>true});
 const type=text=>{node('chat-input').value=text;node('chat-input').oninput();};
 try{await run({storage,node,view,mount,type,poll:async()=>{poll();await Promise.resolve();await Promise.resolve();}});}
 finally{destroy();Object.assign(api,originalApi);for(const key of names)globalThis[key]=originals[key];}
}

test('refresh reconciles a delivered message whose response was lost, without erasing a later draft',async()=>{
 for(const laterDraft of [null,'后来编辑的未发送补充'])await withSurface(async({storage,node,mount,type})=>{
  let sent,calls=0;api.chat=async()=>snapshot();
  api.chatMessage=async(_id,body)=>{calls++;sent=body;throw new Error('响应超时');};
  await mount();type('原始回答');await node('chat-send').onclick();
  const saved=JSON.parse(storage.get(pendingKey));assert.equal(saved.id,sent.message_id);
  assert.match(node('composer-host').innerHTML,/送到没，还没确认/);
  if(laterDraft)type(laterDraft);
  api.chat=async()=>receipt(sent);
  destroy();await mount();
  assert.equal(calls,1,'refresh must only reconcile, never automatically resend');
  assert.equal(storage.has(pendingKey),false);
  assert.equal(storage.get(draftKey),laterDraft||undefined);
  assert.match(node('composer-host').innerHTML,/上一条已交给分析/);
  if(laterDraft)assert.ok(node('composer-host').innerHTML.includes(laterDraft));
 });
});

test('same-page and post-refresh retries replay the original id and question snapshot',async()=>{
 await withSurface(async({storage,node,mount,type})=>{
  const sent=[];api.chat=async()=>snapshot();
  api.chatMessage=async(_id,body)=>{sent.push(body);if(sent.length<3)throw new Error('连接中断');return receipt(body);};
  await mount();type('我最多接受每月一万元');await node('chat-send').onclick();
  await node('retry-message').events.click();
  type('另一段尚未发送的补充');
  api.chat=async()=>snapshot({pending:{kind:'question',request_id:'question-2',payload:{questions:[]}}});
  destroy();await mount();
  assert.equal(sent.length,2);
  await node('chat-send').onclick();assert.equal(sent.length,2,'an unresolved send must not become a different new message');
  await node('retry-message').events.click();
  assert.equal(sent.length,3);
  assert.deepEqual(sent[1],sent[0]);assert.deepEqual(sent[2],sent[0]);
  assert.equal(sent[2].request_id,'question-1');
  assert.equal(storage.get(draftKey),'另一段尚未发送的补充');
  assert.equal(storage.has(pendingKey),false);
 });
});

test('receipt found by polling remains confirmed when the original POST later times out',async()=>{
 await withSurface(async({storage,node,mount,type,poll})=>{
  let sent,fail;api.chat=async()=>snapshot();
  api.chatMessage=(_id,body)=>{sent=body;return new Promise((_resolve,reject)=>{fail=reject;});};
  await mount();type('已经交给服务的回答');const request=node('chat-send').onclick();
  type('不要清除这条新草稿');api.chat=async()=>receipt(sent);await poll();
  assert.equal(storage.has(pendingKey),false);
  fail(new Error('迟到的响应超时'));await request;
  assert.equal(storage.get(draftKey),'不要清除这条新草稿');
  assert.equal(node('chat-send-error').textContent,'');
  assert.match(node('composer-host').innerHTML,/已交给分析/);
  assert.ok(!node('composer-host').innerHTML.includes('迟到的响应超时'));
 });
});

test('an explicit server rejection unlocks correction while retaining the unsent text',async()=>{
 await withSurface(async({storage,node,mount,type})=>{
  api.chat=async()=>snapshot();api.chatMessage=async()=>{throw Object.assign(new Error('当前问题已经更新，请查看最新消息后再发送。'),{status:409});};
  await mount();type('需要重新核对的回答');await node('chat-send').onclick();
  assert.equal(storage.has(pendingKey),false);
  assert.equal(storage.get(draftKey),'需要重新核对的回答');
  assert.match(node('chat-send-error').textContent,/当前问题已经更新/);
  assert.ok(!node('composer-host').innerHTML.includes('送到没'));
 });
});

test('confirmation recovery uses the prior request and does not delete an unrelated draft',async()=>{
 await withSurface(async({storage,node,mount})=>{
  const old={id:'confirmation-original-id',payload:{request_id:'checkpoint-1',action:'approve',text:''},draftText:null};
  storage.set(pendingKey,JSON.stringify(old));storage.set(draftKey,'先记下另一个疑问');
  api.chat=async()=>snapshot({pending:{kind:'checkpoint',request_id:'checkpoint-2',payload:{items:[]}}});
  let sent;api.chatMessage=async(_id,body)=>{sent=body;return receipt(body);};
  await mount();await node('retry-message').events.click();
  assert.deepEqual(sent,{...old.payload,message_id:old.id});
  assert.equal(storage.get(draftKey),'先记下另一个疑问');
 });
});
