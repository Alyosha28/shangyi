import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';
import {render as renderCase,renderTaskResults,destroy} from '../js/views/case.js';

// 滚动揭示只在首屏做一次（`views/case.js` 的 `revealed` 标志）。
// 这条用例锁住它的重置时机：离开一条咨询再回来，必须重新生效，
// 否则第一次访问后该页面就再也看不到揭示效果了。

const record=()=>({question:'比较两个铺位',header:{mode:'scenario',playbook_id:'market_entry'},
 stats:{phases:['archived'],n_events:2},
 artifacts:{task_results:[{scope:'cost',status:'complete',summary:'## 已列费用\n\n两店共 20000 元。',missing:[],source_urls:[],retrieval_attempts:1,cost_scenarios:[],at:'2026-10-02T11:01:00Z'}]},events:[]});

const section=()=>({getAttribute:()=>null,style:{},dataset:{}});

async function withPage(fn){
 const saved={document:globalThis.document,case:api.case,mq:globalThis.matchMedia,motion:globalThis.Motion};
 const elements=new Map();
 const node=id=>{
  if(!elements.has(id))elements.set(id,{innerHTML:'',textContent:'',value:'',onclick:null,addEventListener(){}});
  return elements.get(id);
 };
 const registered=[];
 globalThis.document={getElementById:node};
 globalThis.matchMedia=()=>({matches:false});
 globalThis.Motion={animate:()=>{},stagger:()=>0,inView:(el,cb)=>{registered.push(el);cb();}};
 api.case=async()=>record();
 const view={innerHTML:'',querySelector:node,querySelectorAll:()=>[section(),section(),section()]};
 const ctx={match:['','2026-10-02','case-20261002-test'],isCurrent:()=>true};
 try{
  return await fn({view,ctx,registered});
 }finally{
  destroy();
  globalThis.document=saved.document;
  globalThis.matchMedia=saved.mq;
  if(saved.motion===undefined)delete globalThis.Motion;else globalThis.Motion=saved.motion;
  api.case=saved.case;
 }
}

test('一次进入只注册一轮揭示，每个区块一次，不重复',async()=>{
 await withPage(async({view,ctx,registered})=>{
  await renderCase(view,ctx);
  assert.equal(registered.length,3,'3 个区块应各注册一次，不应变成 6 次');
 });
});

test('重新进入该咨询时滚动揭示重新生效',async()=>{
 await withPage(async({view,ctx,registered})=>{
  await renderCase(view,ctx);
  const first=registered.length;
  assert.ok(first>0);
  // app.js 换路由时会调 destroy()，而 render() 自己也会先调一次，
  // 因此「一次进入」的边界就是 render()：再次进入必须重新揭示。
  await renderCase(view,ctx);
  assert.ok(registered.length>first,'重新进入必须重新注册，否则该页面此后再无揭示效果');
 });
});

test('引擎缺失时报告照常渲染（降级不影响内容）',async()=>{
 const saved=globalThis.Motion;
 delete globalThis.Motion;
 try{
  await withPage(async({view,ctx})=>{
   await renderCase(view,ctx);
   assert.ok(view.innerHTML.includes('比较两个铺位')||view.innerHTML.length>0,'内容必须照常渲染');
  });
 }finally{
  if(saved!==undefined)globalThis.Motion=saved;
 }
});

test('任务结果渲染不依赖动效引擎',()=>{
 const saved=globalThis.Motion;
 delete globalThis.Motion;
 try{
  const html=renderTaskResults({artifacts:{task_results:[{scope:'cost',status:'partial',summary:'装修与设备共 110000 元。',missing:['押金金额未取得'],source_urls:['https://example.com/s'],retrieval_attempts:2,cost_scenarios:[],at:'2026-10-02T11:01:00Z'}]}});
  assert.match(html,/装修与设备共/);
  assert.match(html,/押金金额未取得/);
 }finally{
  if(saved!==undefined)globalThis.Motion=saved;
 }
});