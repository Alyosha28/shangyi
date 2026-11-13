import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';
import {caseCopyText,renderTaskResults,render,destroy} from '../js/views/case.js';

const result=(extra={})=>({scope:'cost',status:'partial',summary:'## 已列费用\n\n装修与设备共 **110000 元**。',
 missing:['押金金额未取得'],source_urls:['https://example.com/source'],retrieval_attempts:2,cost_scenarios:[],at:'2026-10-02T11:01:00Z',...extra});
const record=(results=[result()],latest=results.at(-1))=>({question:'只整理已知费用',header:{mode:'scenario',playbook_id:'market_entry'},
 stats:{phases:['intake'],n_events:3,last_at:'2026-10-02T11:01:00Z'},events:[],artifacts:{task_results:results,latest_task:latest}});

async function withPage(data,chatList,run){
 const keys=['document','setInterval','clearInterval','setTimeout'];
 const previous=Object.fromEntries(keys.map(k=>[k,globalThis[k]]));
 const navigatorDescriptor=Object.getOwnPropertyDescriptor(globalThis,'navigator');
 const saved={case:api.case,chats:api.chats,create:URL.createObjectURL,revoke:URL.revokeObjectURL};
 const nodes=new Map(),copied=[],downloads=[];
 const node=id=>{if(!nodes.has(id))nodes.set(id,{innerHTML:'',textContent:'',value:''});return nodes.get(id);};
 const view={innerHTML:'',querySelector:node,querySelectorAll:()=>[]};
 globalThis.document={getElementById:node,createElement:()=>({click(){}})};
 Object.defineProperty(globalThis,'navigator',{configurable:true,value:{clipboard:{writeText:async text=>copied.push(text)}}});
 globalThis.setInterval=()=>1;globalThis.clearInterval=()=>{};globalThis.setTimeout=()=>0;
 URL.createObjectURL=blob=>{downloads.push(blob);return 'blob:test';};URL.revokeObjectURL=()=>{};
 api.case=async()=>data;api.chats=async()=>{if(chatList instanceof Error)throw chatList;if(chatList?.then)return chatList;return {chats:chatList||[]};};
 try{await render(view,{match:['','2026-10-02','case-20261002-brief'],isCurrent:()=>true});await Promise.resolve();await run({node,view,copied,downloads});}
 finally{
  destroy();api.case=saved.case;api.chats=saved.chats;URL.createObjectURL=saved.create;URL.revokeObjectURL=saved.revoke;
  for(const key of keys)globalThis[key]=previous[key];
  if(navigatorDescriptor)Object.defineProperty(globalThis,'navigator',navigatorDescriptor);else delete globalThis.navigator;
 }
}

test('intake-only completed work displays and copies its actual result rather than an empty recommendation',async()=>{
 const data=record([result({status:'complete',missing:[]})]);
 data.question='只整理已知费用。\n不要预测利润；押金按资金占用单列。';data.question_source='web_conversation_state';
 data.context={source:'web_conversation_state',chat_id:'chat-000000000000000000000055',initial:data.question,case:'2026-10-02/case-20261002-brief'};
 await withPage(data,[],async({view,node,copied,downloads})=>{
  assert.match(view.innerHTML,/<h2>已列费用<\/h2>/);assert.match(view.innerHTML,/<strong>110000 元<\/strong>/);
  assert.ok(!view.innerHTML.includes('分析尚未完成'));assert.ok(!view.innerHTML.includes('未生成建议'));
  assert.match(view.innerHTML,/复制结果/);assert.match(node('page-status').textContent,/本次整理已完成/);
  assert.ok(!view.innerHTML.includes('完整咨询报告'));
  assert.match(view.innerHTML,/不要预测利润；押金按资金占用单列/);assert.match(view.innerHTML,/这次对话的最初输入/);
  await node('#copy-analysis').onclick();assert.equal(copied.length,1);assert.match(copied[0],/装修与设备共 \*\*110000 元\*\*/);
  assert.ok(copied[0].startsWith(data.question));
  // 两个导出按钮：先可读 Markdown，再原始 JSON
  assert.match(view.innerHTML,/保存可读记录/);
  assert.match(view.innerHTML,/保存原始数据/);
  assert.match(view.innerHTML,/给需要核对过程的人用/,'两个按钮的用途必须写明');
  node('#export-readable').onclick();node('#export-raw').onclick();
  assert.equal(downloads.length,2);
  const markdown=await downloads[0].text();
  assert.match(markdown,/^# 只整理已知费用/);
  assert.match(markdown,/## 我当时的问法/);
  assert.match(markdown,/装修与设备共 \*\*110000 元\*\*/,'结果正文要进可读文件');
  assert.ok(!markdown.includes('仍缺少的信息'),'本次没有缺口就别列出缺口一节');
  const exported=JSON.parse(await downloads[1].text());
  assert.deepEqual(exported.artifacts.task_results,data.artifacts.task_results);
  assert.deepEqual(exported.artifacts.latest_task,data.artifacts.latest_task);
  assert.deepEqual(exported.context,data.context);assert.equal(exported.question,data.question);
 });
});

test('every saved summary remains visible and partial missing items are explicit and escaped',()=>{
 const first=result({status:'complete',summary:'第一份资料结果',missing:[]});
 const second=result({summary:'**第二份结果**\n\n<script>bad()</script>',missing:['税费 <img src=x onerror=bad()>']});
 const html=renderTaskResults(record([first,second]));
 assert.match(html,/第一份资料结果/);assert.match(html,/<strong>第二份结果<\/strong>/);
 assert.match(html,/有结果，还缺信息/);assert.match(html,/仍缺少的信息/);
 assert.match(html,/税费 &lt;img/);assert.ok(!/<(?:img|script)\b/.test(html));
 assert.ok(html.indexOf('第二份结果')<html.indexOf('第一份资料结果'));
 const copied=caseCopyText(record([first,second]));assert.match(copied,/第一份资料结果/);assert.match(copied,/税费/);
});

test('a started followup does not present the prior completed result as the new answer',async()=>{
 const data=record([result({status:'complete',missing:[]})],{status:'processing',at:'2026-10-02T11:02:00Z'});
 await withPage(data,[],async({view,node,copied})=>{
  assert.match(view.innerHTML,/新要求还没出结果/);
  assert.match(view.innerHTML,/下面是之前已完成的整理/);
  assert.equal(node('page-status').textContent,'追加结果待完成');
  await node('#copy-analysis').onclick();assert.match(copied[0],/新要求还没出结果；以下是之前保存的整理/);
 });
});

test('an exact case match provides a return link without blocking saved results',async()=>{
 const id='chat-000000000000000000000055';
 await withPage(record(),[{id,case:'2026-10-02/case-20261002-brief'}],async({node})=>{
  assert.ok(node('composer-host').innerHTML.includes('href="#/chat/'+id+'"'));
  assert.match(node('composer-host').innerHTML,/回到对话/);
 });
 await withPage(record(),[{id,case:'2026-10-01/case-20261002-brief'}],async({node})=>{
  assert.ok(!node('composer-host').innerHTML.includes('#/chat/'));
 });
 await withPage(record(),new Error('chat list unavailable'),async({view,node})=>{
  assert.match(view.innerHTML,/110000 元/);assert.ok(!node('composer-host').innerHTML.includes('#/chat/'));
  assert.match(node('composer-host').innerHTML,/对话列表没读到/);
 });
});

test('records without a result cannot copy a placeholder or empty recommendation',async()=>{
 const data=record([],null);
 assert.equal(caseCopyText(data),'');
 await withPage(data,[],async({view,node,copied})=>{
  assert.match(view.innerHTML,/id="copy-analysis" disabled/);
  await node('#copy-analysis').onclick();assert.deepEqual(copied,[]);
 });
});

test('destroyed case views ignore both late chat matches and late lookup failures',async()=>{
 for(const fails of [false,true]){
  let finish,reject;const lookup=new Promise((resolve,fail)=>{finish=resolve;reject=fail;});
  await withPage(record(),lookup,async({node})=>{
   const before=node('composer-host').innerHTML;destroy();globalThis.document=undefined;
   if(fails)reject(new Error('late lookup failure'));
   else finish({chats:[{id:'chat-000000000000000000000055',case:'2026-10-02/case-20261002-brief'}]});
   await new Promise(resolve=>setImmediate(resolve));
   assert.equal(node('composer-host').innerHTML,before);
  });
 }
});

test('an archived record can still receive its exact conversation link after polling stops',async()=>{
 const data=record();data.stats.phases=['archived'];
 const id='chat-000000000000000000000055';
 await withPage(data,[{id,case:'2026-10-02/case-20261002-brief'}],async({node})=>{
  assert.ok(node('composer-host').innerHTML.includes('href="#/chat/'+id+'"'));
 });
});
