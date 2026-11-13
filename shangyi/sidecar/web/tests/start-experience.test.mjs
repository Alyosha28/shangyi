import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';
import {render,destroy} from '../js/views/run.js';

test('the opening page and editable draft appear before service health responds',async()=>{
 const original={document:globalThis.document,sessionStorage:globalThis.sessionStorage,health:api.health};
 const elements=new Map();
 function node(id){
  if(!elements.has(id))elements.set(id,{id,value:'',textContent:'',innerHTML:'',disabled:true,hidden:false,addEventListener(){},focus(){}});
  return elements.get(id);
 }
 const view={innerHTML:'',querySelectorAll(){return[];}};
 let finishHealth;
 try{
  globalThis.document={getElementById:node,querySelector(){return node('composer-hint');}};
  globalThis.sessionStorage={getItem(){return null;},setItem(){},removeItem(){}};
  api.health=()=>new Promise(resolve=>{finishHealth=resolve;});
  await render(view,{match:[],isCurrent:()=>true});
  assert.match(view.innerHTML,/说说你正在考虑的事/);
  assert.match(node('composer-host').innerHTML,/正在检查。/);
  assert.equal(node('start-send').disabled,true);
  finishHealth({frontend_contract:3,interactive_chat:true,services:{search:{status:'available'}}});
  await Promise.resolve();
  assert.equal(node('start-send').disabled,false);
  assert.match(node('composer-hint').textContent,/Enter 发送/);
 }finally{
  destroy();api.health=original.health;
  globalThis.document=original.document;globalThis.sessionStorage=original.sessionStorage;
 }
});

test('failed health check keeps the draft and offers an in-page retry',async()=>{
 const original={document:globalThis.document,sessionStorage:globalThis.sessionStorage,health:api.health};
 const elements=new Map();let checks=0;
 function node(id){if(!elements.has(id))elements.set(id,{id,value:'',textContent:'',innerHTML:'',disabled:true,addEventListener(){},focus(){}});return elements.get(id);}
 try{
  globalThis.document={getElementById:node,querySelector(){return node('composer-hint');}};
  globalThis.sessionStorage={getItem(){return JSON.stringify({text:'已保存的问题'});},setItem(){},removeItem(){}};
  api.health=()=>++checks===1?Promise.reject(new Error('offline')):Promise.resolve({frontend_contract:3,interactive_chat:true,services:{search:{status:'available'}}});
  await render({innerHTML:'',querySelectorAll(){return[];}},{match:[],isCurrent:()=>true});
  await Promise.resolve();
  assert.match(node('composer-host').innerHTML,/已保存的问题/);
  assert.match(node('composer-hint').innerHTML,/重新连接/);
  assert.equal(node('start-send').disabled,true);
  node('retry-health').onclick();
  await Promise.resolve();
  assert.equal(checks,2);
  assert.equal(node('start-send').disabled,false);
 }finally{destroy();api.health=original.health;globalThis.document=original.document;globalThis.sessionStorage=original.sessionStorage;}
});

// ── UI-010：换示例时不能让主题与问题文字错配 ──────────────────────────────
import {exampleTextDecision,exampleTextFor,EXAMPLES} from '../js/views/run.js';

test('空框或还是示例原文时直接换，不反复问', () => {
  assert.equal(exampleTextDecision({ box: '', exampleTopic: null }), 'fill');
  assert.equal(exampleTextDecision({ box: '   ', exampleTopic: 'pricing_response' }), 'fill');
  // 框里还是上一个示例的原文（用户没动过）：连着试示例不该被反复问
  assert.equal(exampleTextDecision({ box: EXAMPLES.pricing_response, exampleTopic: 'pricing_response' }), 'fill');
});

test('框里是用户自己写的内容时必须问，不能静默覆盖', () => {
  assert.equal(exampleTextDecision({ box: '我想续租，但房东要涨租', exampleTopic: null }), 'ask');
  // 示例原文被改过一个字，也算用户自己的内容
  assert.equal(exampleTextDecision({ box: EXAMPLES.pricing_response + '。', exampleTopic: 'pricing_response' }), 'ask');
});

test('要写进框里的就是该示例的原文', () => {
  assert.equal(exampleTextFor('market_entry'), EXAMPLES.market_entry);
  assert.equal(exampleTextFor('no-such-topic'), '');
});
