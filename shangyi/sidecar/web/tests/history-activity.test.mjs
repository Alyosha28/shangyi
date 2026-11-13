import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';
import {mergeConsultations,filterConsultations,render as renderCases} from '../js/views/cases.js';
import {mergeActivity,renderActivity,render as renderActivityPage,destroy} from '../js/views/activity.js';

const chat=(index,extra={})=>({id:'chat-'+String(index).padStart(24,'0'),title:'咨询 '+index,
 initial:'咨询 '+index,topic:'negotiation',mode:'scenario',created_at:100+index,
 status:'waiting',alive:true,case:null,...extra});

test('search includes conversations beyond the sidebar limit and unfinished chats, without duplicating their cases',()=>{
 const chats=Array.from({length:15},(_,i)=>chat(i));
 chats[0]=chat(0,{initial:'背景'.repeat(60)+'寻找后半段关键词',status:'failed',alive:false});
 chats[1].case='2026-10-02/case-linked';
 const cases=[
  {date:'2026-10-02',case_id:'case-linked',question:'关联的分析条件',mtime:600,mode:'scenario'},
  {date:'2026-10-01',case_id:'case-legacy',question:'原有分析记录',mtime:300,mode:'whatif',playbook_id:'market_entry'}
 ];
 const records=mergeConsultations(cases,chats);
 assert.equal(records.length,16);
 assert.equal(records.filter(r=>r.href.includes('case-linked')).length,0);
 assert.equal(filterConsultations(records,'all','关联的分析条件')[0].href,'#/chat/'+chats[1].id);
 assert.equal(filterConsultations(records,'all','后半段关键词')[0].status,'没启动成');
 assert.equal(filterConsultations(records,'consultation','原有分析')[0].href,'#/case/2026-10-01/case-legacy');
 assert.equal(filterConsultations(records,'all','谈条件').length,15);
});

test('activity prioritizes live conversations, links pending questions, and keeps stopped chats readable',()=>{
 const items=mergeActivity([
  chat(1,{status:'stopped',alive:false,created_at:900}),
  chat(2,{case:'2026-10-02/case-linked',title:'<img onerror=bad()>'})
 ],[
  {run_id:'legacy-linked',case:'2026-10-02/case-linked',alive:true,started_at:100},
  {run_id:'legacy',alive:false,exit_code:1,started_at:500,log_tail:'<script>unsafe</script>'}
 ]);
 assert.equal(items.length,3);
 assert.equal(items[0].id,chat(2).id);
 const html=renderActivity(items);
 assert.match(html,/等你回应/);
 assert.match(html,/继续回应/);
 assert.match(html,/data-kind="chat"/);
 assert.match(html,/已停止/);
 assert.ok(!html.includes('data-stop="'+chat(1).id+'"'));
 assert.ok(!html.includes('<img'));
 assert.ok(!html.includes('<script>'));
 assert.match(html,/&lt;script&gt;/);
});

test('a delivered task remains open for followup without an active-analysis badge',()=>{
 const items=mergeActivity([chat(30,{status:'partial',awaiting_followup:true})],[]);
 const html=renderActivity(items);
 assert.match(html,/已交付，部分信息未取得/);
 assert.match(html,/结束本次对话/);
 assert.ok(!html.includes('停止本轮分析'));
 assert.ok(!html.includes('icon-activity'));
 const records=mergeConsultations([], [chat(30,{status:'partial',awaiting_followup:true})]);
 assert.equal(records[0].status,'已交付，部分信息未取得');
});

function testSurface(){
 const elements=new Map();
 const node=id=>{
  if(!elements.has(id))elements.set(id,{id,innerHTML:'',textContent:'',value:'',disabled:false,
   hidden:false,addEventListener(){},setAttribute(){}});
  return elements.get(id);
 };
 const view={innerHTML:'',buttons:[],querySelector:node,querySelectorAll(selector){
  if(selector==='[data-stop]'){
   this.buttons=[...node('#runs').innerHTML.matchAll(/<button[^>]*data-stop="([^"]+)" data-kind="([^"]+)"([^>]*)>/g)]
    .map(m=>({dataset:{stop:m[1],kind:m[2]},disabled:m[3].includes('disabled'),isConnected:true}));
   return this.buttons;
  }
  return [];
 }};
 return {node,view,document:{getElementById:node}};
}

async function withSurface(fn){
 const originals={document:globalThis.document,setTimeout:globalThis.setTimeout,...api};
 const surface=testSurface();
 globalThis.document=surface.document;globalThis.setTimeout=()=>0;
 try{await fn(surface);}finally{
  destroy();globalThis.document=originals.document;globalThis.setTimeout=originals.setTimeout;
  for(const key of Object.keys(api))api[key]=originals[key];
 }
}

test('the activity page opens and stops the same live chat created by the home page',async()=>{
 await withSurface(async({node,view})=>{
  let active=true;const stopped=[];
  api.chats=async()=>({chats:[chat(4,{alive:active,status:active?'waiting':'stopped'})]});
  api.runs=async()=>({runs:[]});
  api.stopChat=async id=>{stopped.push(id);active=false;return {ok:true};};
  api.stopRun=async()=>assert.fail('chat stop must not call the legacy run API');
  await renderActivityPage(view,{isCurrent:()=>true});
  assert.match(node('#runs').innerHTML,new RegExp('#/chat/'+chat(4).id));
  assert.ok(!node('#runs').innerHTML.includes('当前没有运行记录'));
  await view.buttons[0].onclick();
  assert.deepEqual(stopped,[chat(4).id]);
  assert.match(node('#runs').innerHTML,/已停止/);
  assert.equal(view.buttons.length,0);
 });
});

test('a failed stop preserves the live chat and provides another stop attempt',async()=>{
 await withSurface(async({node,view})=>{
  api.chats=async()=>({chats:[chat(5)]});api.runs=async()=>({runs:[]});
  api.stopChat=async()=>{throw new Error('连接暂时中断');};
  await renderActivityPage(view,{isCurrent:()=>true});
  await view.buttons[0].onclick();
  assert.match(node('toast').textContent,/连接暂时中断/);
  assert.equal(view.buttons.length,1);
  assert.equal(view.buttons[0].disabled,false);
  assert.match(node('#runs').innerHTML,/等你回应/);
 });
});

test('history searches the chat source even when case loading fails, and discloses the partial list',async()=>{
 await withSurface(async({node,view})=>{
  api.cases=async()=>{throw new Error('case read failed');};
  api.chats=async()=>({chats:[chat(7,{initial:'开场的一百字之后还有独特背景'})]});
  await renderCases(view,{isCurrent:()=>true});
  assert.match(view.innerHTML,/列表可能不完整/);
  node('#case-search').oninput({target:{value:'独特背景'}});
  assert.match(node('#case-results').innerHTML,new RegExp('#/chat/'+chat(7).id));
  assert.match(node('#result-count').textContent,/共 1 条咨询/);
 });
});
