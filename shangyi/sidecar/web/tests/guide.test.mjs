import test from 'node:test';
import assert from 'node:assert/strict';
import {newGuide,readGuide,saveGuide,guideIsSaved,guideAction,guidePremises,prepareOpening,OPENING_DRAFT_KEY} from '../js/guide-state.js';

function storage(){const values=new Map();return {getItem:k=>values.get(k)??null,setItem:(k,v)=>values.set(k,v)};}
function answered(){return guideAction(guideAction(newGuide(),'submit','房东想涨租，我想看看有哪些谈法。'),'submit','我还没想清楚，先保留未知。');}

test('guide practices uncertainty, questions, corrections and source reading before real handoff',()=>{
 let s=newGuide();
 s=guideAction(s,'submit','房东想涨租，我不知道怎么谈。');
 s=guideAction(s,'submit','上限还没想好，先保留未知。');
 s=guideAction(s,'ask');assert.equal(s.step,2);
 s=guideAction(s,'correct','我想先比较不搬店的办法。');assert.equal(s.step,2);
 assert.equal(s.corrections.length,1);assert.equal(s.asked,true);
 s=guideAction(s,'confirm');assert.equal(s.step,3);
 assert.throws(()=>guideAction(s,'finish'),/先展开/);
 s=guideAction(s,'sources');s=guideAction(s,'finish');assert.equal(s.step,4);
 assert.equal(s.realDraft,'','fictional answers must never become the real opening');
 const store=storage();prepareOpening('我真正想讨论的是供应商交货延期。',store);
 const actual=JSON.parse(store.getItem(OPENING_DRAFT_KEY));
 assert.equal(actual.text,'我真正想讨论的是供应商交货延期。');
 assert.equal(actual.attempt,null);assert.ok(!JSON.stringify(actual).includes('房东'));
});

test('current premises merge the practice answer with the sent corrections and keep every original',()=>{
  const before=guidePremises(answered());
  assert.equal(before.revised,false);
  assert.equal(before.rows.find(r=>r.key==='rent').status,'kept');
  assert.equal(before.rows.find(r=>r.key==='rent').text,'我还没想清楚，先保留未知。','没有修改时清单里就是用户原话');
  assert.equal(before.rows.find(r=>r.key==='landlord').status,'unknown');
  assert.equal(before.rows.find(r=>r.key==='landlord').text,'没有信息，保留未知。','没有信息的项要写明未知，不能填默认值');
  assert.deepEqual(before.rows.filter(r=>r.status==='revision'),[]);

  const s=guideAction(guideAction(answered(),'correct','我的租金上限现在明确为每月9000元。'),'correct','另外我想先比较不搬店的办法。');
  const after=guidePremises(s);
  assert.equal(after.revised,true);
  const rent=after.rows.find(r=>r.key==='rent');
  assert.equal(rent.status,'revised','有修改时原回答要标出已被后续修改覆盖');
  assert.equal(rent.text,'我还没想清楚，先保留未知。','覆盖标记不能抹掉原回答原文');
  const revisions=after.rows.filter(r=>r.status==='revision');
  assert.deepEqual(revisions.map(r=>r.order),[1,2],'修改要按发送顺序列出');
  assert.equal(revisions[0].latest,false);assert.equal(revisions[1].latest,true,'最后一条才是最新说法');
  assert.equal(revisions[1].text,'另外我想先比较不搬店的办法。');
  assert.equal(after.rows.find(r=>r.key==='rent').text,'我还没想清楚，先保留未知。','练习回答不能冒充成用户的修改判断');

  assert.deepEqual(guidePremises(guideAction(s,'confirm')).rows,after.rows,'确认前后的清单必须一致，确认的内容就是展示的内容');
  s.draft='还没发送的修改';
  assert.deepEqual(guidePremises(s).rows,after.rows,'未发送的修改不进清单');
  assert.deepEqual(guidePremises(guideAction(s,'restart')).rows.filter(r=>r.status==='revision'),[],'再练一次要清空修改历史');
});

test('current premises never invent content from a missing or damaged practice state',()=>{
  const damaged=[{},{answer:42},{corrections:'不是数组'},{corrections:[null,'一句真的修改']},null];
  for(const state of damaged){
   const premises=guidePremises(state);
   assert.ok(premises.rows.length>=3,'固定三项不能因为状态损坏就消失');
   assert.equal(premises.rows.find(r=>r.key==='landlord').status,'unknown','状态损坏时未知项仍然是未知');
   assert.ok(premises.rows.every(r=>typeof r.text==='string'||r.status==='unknown'),'不能出现非字符串内容');
  }
  const mixed=guidePremises({answer:'一句回答',corrections:[null,'真的修改',7]}).rows.filter(r=>r.status==='revision');
  assert.deepEqual(mixed.map(r=>r.text),['真的修改'],'坏掉的修改条目要被忽略，不能当成用户说过的话');
});

test('empty inputs and stale guide actions cannot advance a lesson',()=>{
 const s=newGuide();assert.throws(()=>guideAction(s,'submit','  '),/写一句/);
 assert.equal(s.step,0);assert.throws(()=>guideAction(s,'confirm'),/步骤已变化/);
 assert.throws(()=>prepareOpening(' ',storage()),/真实|真正/);
});

test('refresh and navigation retain practice and real draft separately',()=>{
 const store=storage();let s=guideAction(newGuide(),'submit','练习开场');
 s.draft='尚未发出的练习回答';s.realDraft='尚未发出的真实问题';
 assert.equal(saveGuide(s,store),true);assert.deepEqual(readGuide(store),s);
 const restarted=guideAction(s,'restart');
 assert.equal(restarted.step,0);assert.equal(restarted.opening,'');
 assert.equal(restarted.realDraft,s.realDraft);
 assert.equal(store.getItem(OPENING_DRAFT_KEY),null);
});

test('real handoff does not overwrite an existing consultation draft without an explicit choice',()=>{
 const store=storage(),old={text:'原来的问题',topic:'pricing_response',mode:'whatif',action:'原动作',attempt:{id:'pending'}};
 store.setItem(OPENING_DRAFT_KEY,JSON.stringify(old));
 const result=prepareOpening('新问题',store);
 assert.deepEqual(result,{conflict:true,previous:'原来的问题'});
 assert.deepEqual(JSON.parse(store.getItem(OPENING_DRAFT_KEY)),old);
 prepareOpening('新问题',store,true);
 assert.equal(JSON.parse(store.getItem(OPENING_DRAFT_KEY)).text,'新问题');
 assert.deepEqual(JSON.parse(store.getItem(OPENING_DRAFT_KEY+':before-guide')),old);
});

test('handoff of an unchanged draft preserves its existing analysis settings',()=>{
 const store=storage(),old={text:'相同问题',mode:'whatif',action:'我的动作'};
 store.setItem(OPENING_DRAFT_KEY,JSON.stringify(old));
 assert.equal(prepareOpening('相同问题',store).conflict,false);
 assert.deepEqual(JSON.parse(store.getItem(OPENING_DRAFT_KEY)),old);
});

test('storage failure is detectable and never silently starts a real consultation',()=>{
 const denied={getItem(){throw new Error('storage denied');},setItem(){throw new Error('storage denied');}};
 const s=guideAction(newGuide(),'submit','保留当前练习');
 assert.equal(saveGuide(s,denied),false);
 assert.equal(readGuide(denied).opening,s.opening);
 assert.throws(()=>prepareOpening('真实问题',denied),/无法读取草稿/);
});

test('confirmation cannot silently discard a correction that has not been sent',()=>{
 let s=guideAction(guideAction(newGuide(),'submit','练习开场'),'submit','还不知道');
 s.draft='修改写到一半';
 assert.throws(()=>guideAction(s,'confirm'),/未发送的修改/);
 assert.equal(s.draft,'修改写到一半');assert.equal(s.step,2);
 s=guideAction(s,'correct',s.draft);assert.equal(guideAction(s,'confirm').step,3);
});

test('write failure cannot restore an older readable practice over newer in-memory progress',()=>{
 const values=new Map();let denyWrite=false;
 const store={getItem:k=>values.get(k)??null,setItem(k,v){if(denyWrite)throw new Error('quota');values.set(k,v);}};
 const initial=newGuide();initial.draft='旧草稿';assert.equal(saveGuide(initial,store),true);
 const next=guideAction(initial,'submit','新的练习开场');next.draft='后来填写的回答';
 denyWrite=true;assert.equal(saveGuide(next,store),false);
 assert.deepEqual(readGuide(store),next);
 assert.equal(guideIsSaved(store),false);
 denyWrite=false;assert.equal(saveGuide(next,store),true);
 assert.deepEqual(readGuide(store),next);assert.equal(guideIsSaved(store),true);
});

test('invalid existing consultation draft is retained with a readable error',()=>{
 for(const raw of ['{broken',JSON.stringify({text:42}),JSON.stringify(['原草稿']),JSON.stringify(3)]){
  const store=storage();store.setItem(OPENING_DRAFT_KEY,raw);
  assert.throws(()=>prepareOpening('真实问题',store,true),/原草稿暂时无法读取/);
  assert.equal(store.getItem(OPENING_DRAFT_KEY),raw);
 }
});
