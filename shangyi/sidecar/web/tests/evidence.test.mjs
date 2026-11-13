import test from 'node:test';
import assert from 'node:assert/strict';
import {collectSources,render as renderCase,destroy} from '../js/views/case.js';
import {api} from '../js/api.js';
import {sourceItem} from '../js/ui.js';

test('failed retrievals do not become evidence and call identity is joined',()=>{
 const events=[
  {type:'tool_call_started',call_id:'ok',name:'web_fetch',arguments:JSON.stringify({url:'https://example.com/evidence'})},
  {type:'tool_call_finished',call_id:'ok',outcome:{status:'ok',output:'retrieved content'}},
  {type:'tool_call_started',call_id:'bad',name:'web_fetch',arguments:'{}'},
  {type:'tool_call_finished',call_id:'bad',outcome:{status:'failed',output:'timeout'}}
 ];
 const sources=collectSources({events});
 assert.equal(sources.length,1);assert.equal(sources[0].title,'https://example.com/evidence');
 assert.equal(sources[0].detail,'retrieved content');
});
test('theory is distinguished from observed business facts',()=>{
 const sources=collectSources({events:[
 {type:'tool_call_started',call_id:'kb',name:'kb_retrieve',arguments:'{}'},
 {type:'tool_call_finished',call_id:'kb',outcome:{status:'ok',output:'# Theory\nA theoretical model'}}]});
 assert.match(sources[0].description,/不作为经营事实/);
});
test('retrieved content cannot inject HTML into evidence controls',()=>{
 const html=sourceItem('<img src=x onerror=alert(1)>','<script>bad</script>','</pre><script>bad</script>');
 assert.ok(!html.includes('<script>'));assert.ok(!html.includes('<img src=x'));
 assert.ok(html.includes('&lt;script&gt;'));assert.ok(html.includes('&lt;/pre&gt;'));
});
test('background summaries without detail do not render an empty disclosure',()=>{
 const html=sourceItem('Question','Answer','');
 assert.ok(!html.includes('<details'));assert.ok(html.includes('Answer'));
});
test('archived case keeps structured research and every cost scenario outside retrieval filtering',async()=>{
 const previousDocument=globalThis.document,previousCase=api.case;
 const elements=new Map();
 const node=id=>{
  if(!elements.has(id))elements.set(id,{innerHTML:'',textContent:'',value:''});
  return elements.get(id);
 };
 globalThis.document={getElementById:node};
 const view={innerHTML:'',querySelector:node,querySelectorAll:()=>[]};
 api.case=async()=>({question:'比较两个铺位',header:{playbook_id:'market_entry'},stats:{phases:['archived'],n_events:2},
  artifacts:{
   research:{records:[{kind:'competitor',url:'https://example.com/shop',fields:{name:{value:'保留的竞争店铺',quote:'保留的竞争店铺开业记录'}}}]},
   business_costs:[{scenario:'街边店',listed_upfront_cash:10000},{scenario:'商场店',listed_upfront_cash:20000}],
   gamespec:{params:{rent:{name:'月租参数',value:3000}}}
  },events:[
   {type:'tool_call_started',call_id:'search',name:'web_search',arguments:JSON.stringify({query:'铺位报价'})},
   {type:'tool_call_finished',call_id:'search',outcome:{status:'ok',output:'公开查询结果'}}
  ]});
 try{
  await renderCase(view,{match:['','2026-10-02','case-20261002-test'],isCurrent:()=>true});
  const initial=node('evidence-content').innerHTML;
  assert.match(initial,/保留的竞争店铺/);assert.match(initial,/<h3>街边店<\/h3>/);assert.match(initial,/<h3>商场店<\/h3>/);
  assert.equal((initial.match(/<section class="cost-card">/g)||[]).length,2);
  assert.match(initial,/月租参数/);assert.match(initial,/铺位报价/);
  assert.ok(initial.indexOf('<h3>商场店</h3>')<initial.indexOf('id="source-records"'));
  node('source-search').oninput({target:{value:'完全不匹配'}});
  assert.match(node('source-records').innerHTML,/没匹配上的资料/);
  assert.ok(!node('source-records').innerHTML.includes('cost-card'));
  assert.equal(node('evidence-content').innerHTML,initial);
  node('source-search').oninput({target:{value:'铺位报价'}});
  assert.match(node('source-records').innerHTML,/公开查询结果/);
 }finally{destroy();globalThis.document=previousDocument;api.case=previousCase;}
});
