import test from 'node:test';
import assert from 'node:assert/strict';
import {render} from '../js/views/guide.js';
import {newGuide,guideAction,saveGuide,readGuide,OPENING_DRAFT_KEY} from '../js/guide-state.js';

const decode=text=>text.replace(/&quot;/g,'"').replace(/&#39;/g,"'").replace(/&lt;/g,'<').replace(/&gt;/g,'>').replace(/&amp;/g,'&');
function surface(){
 const nodes=new Map(),owners=new Map();
 function element(id){return {id,value:'',textContent:'',hidden:false,disabled:false,open:false,dataset:{},events:{},
  addEventListener(type,fn){this.events[type]=fn;},removeAttribute(name){if(name==='disabled')this.disabled=false;},
  focus(){},scrollIntoView(){},fire(type){return this.events[type]?.({preventDefault(){},target:this,currentTarget:this});}};}
 function parse(html,owner){
  for(const old of owners.get(owner)||[])if(old.id)nodes.delete(old.id);
  const children=[];
  for(const match of html.matchAll(/<([a-z][\w-]*)([^>]*)>/gi)){
   const attrs=match[2],id=attrs.match(/\bid="([^"]*)"/)?.[1];
   const child=element(id);child.className=attrs.match(/\bclass="([^"]*)"/)?.[1]||'';
   child.hidden=/\bhidden(?:\s|$)/.test(attrs);child.disabled=/\bdisabled(?:\s|$)/.test(attrs);
   child.open=/\bopen(?:\s|$)/.test(attrs);
   const fill=attrs.match(/\bdata-fill="([^"]*)"/);if(fill)child.dataset.fill=decode(fill[1]);
   if(match[1]==='textarea')child.value=decode(html.slice(match.index+match[0].length).split('</textarea>')[0]);
   if(id)nodes.set(id,child);children.push(child);
  }
  owners.set(owner,children);
 }
 function root(id){const el=element(id);let html='';Object.defineProperty(el,'innerHTML',{get:()=>html,set:value=>{html=value;parse(value,id);}});nodes.set(id,el);return el;}
 const view=root('view');view.scrollTop=0;view.scrollHeight=1000;
 view.querySelector=selector=>(owners.get('view')||[]).find(n=>selector.startsWith('.')?n.className.split(' ').includes(selector.slice(1)):n.id===selector.slice(1))||null;
 root('composer-host');for(const id of ['page-status','evidence-title','evidence-content'])root(id);
 return {view,node:id=>nodes.get(id)||null,document:{getElementById:id=>nodes.get(id)||null,
  querySelectorAll:selector=>selector==='[data-fill]'?(owners.get('composer-host')||[]).filter(n=>n.dataset.fill):[]}};
}
async function setup(run){
 const old={document:globalThis.document,sessionStorage:globalThis.sessionStorage,location:globalThis.location,fetch:globalThis.fetch};
 const ui=surface(),values=new Map();let deny=null,requests=0;
 const store={getItem:key=>values.get(key)??null,setItem(key,value){if(deny?.(key))throw new Error('write denied');values.set(key,value);}};
 globalThis.document=ui.document;globalThis.sessionStorage=store;globalThis.location={hash:'#/guide'};
 globalThis.fetch=()=>{requests++;throw new Error('guide must not make API requests');};
 const mount=(returnTo='#/run')=>render(ui.view,{isCurrent:()=>true,returnTo});
 const type=(id,text)=>{const input=ui.node(id);assert.ok(input,id+' must be visible');input.value=text;input.oninput();};
 try{await run({...ui,store,values,mount,type,denyWrites:fn=>deny=fn,requests:()=>requests});}
 finally{Object.assign(globalThis,old);}
}
function finishedGuide(){
 let state=guideAction(newGuide(),'submit','练习开场');state=guideAction(state,'submit','最多1.1万元（练习）');
 state=guideAction(state,'confirm');state=guideAction(state,'sources');return guideAction(state,'finish');
}
// 取出第一段 review-item（核对清单本体）：它内部只有 p/span，第一个 </div> 就是它的结尾。
function checklist(html){const start=html.indexOf('<div class="review-item">');assert.ok(start>=0,'核对清单要渲染成 review-item');return html.slice(start,html.indexOf('</div>',start)+6);}

test('after a correction the checklist itself shows the whole current premises, and confirming keeps them',async()=>{
  await setup(async({view,node,mount,type,store})=>{
   const corrected=guideAction(guideAction(guideAction(newGuide(),'submit','房东想涨租，我想看看有哪些谈法。'),'submit','我还没想清楚，先保留未知。'),'correct','练习修改：我的租金上限现在明确为每月9000元。');
   saveGuide(corrected,store);await mount();
   assert.match(checklist(view.innerHTML),/你刚才的练习回答：我还没想清楚，先保留未知。/);
   assert.match(checklist(view.innerHTML),/你之后发送的修改（第 1 条，最新说法）：练习修改：我的租金上限现在明确为每月9000元。/,'修改必须进入清单，不能只追加在下面');
   assert.match(checklist(view.innerHTML),/房东愿不愿意让价：没有信息，保留未知。/,'没有信息的项要写明未知');
   assert.match(checklist(view.innerHTML),/原文保留；你之后发的修改列在下面/,`原回答要标出被后续修改覆盖`);
   assert.match(view.innerHTML,/确认按钮对应上面整份清单：原回答加上全部修改/);
   node('guide-confirm').fire('click');
   assert.equal(readGuide(store).step,3);
   assert.match(view.innerHTML,/你已经确认的练习前提/);
   assert.match(checklist(view.innerHTML),/我还没想清楚，先保留未知。/,'确认后原回答仍然可查');
   assert.match(checklist(view.innerHTML),/练习修改：我的租金上限现在明确为每月9000元。/,'确认后修改仍然可查');
   assert.match(view.innerHTML,/你确认的就是上面这份清单：原回答加上之后发送的全部修改/);
   const source=view.querySelector('.guide-source');source.open=true;source.fire('toggle');
   node('guide-finish').fire('click');await mount();
   assert.equal(readGuide(store).step,4);
   assert.match(checklist(view.innerHTML),/我的租金上限现在明确为每月9000元。/,'确认的清单在练习结束后仍可查');
   assert.equal(node('guide-real-input').value,'','练习回答和修改不能自动带进真实咨询');
  });
});

test('an uncorrected checklist states that the answer stands and keeps the confirm scope unchanged',async()=>{
  await setup(async({view,node,mount,type,store})=>{
   const plain=guideAction(guideAction(newGuide(),'submit','练习开场'),'submit','我还没想清楚，能接受多少还不知道。');
   saveGuide(plain,store);await mount();
   assert.match(checklist(view.innerHTML),/之后没有修改，这就是要确认的说法。/);
   assert.equal(/最新说法/.test(checklist(view.innerHTML)),false,'没有修改就不该出现最新说法');
   assert.match(view.innerHTML,/确认按钮对应上面这份清单/);
   node('guide-confirm').fire('click');assert.equal(readGuide(store).step,3);
   assert.equal(/最新说法/.test(checklist(view.innerHTML)),false);
  });
});

test('visible guide controls practice, resume a correction and hand off only the real question',async()=>{
 await setup(async({view,node,store,mount,type,document,requests})=>{
  const chat='#/chat/chat-000000000000000000000011';await mount(chat);
  assert.match(view.innerHTML,/只是练习/);assert.ok(view.innerHTML.includes('href="'+chat+'"'));
  assert.match(view.innerHTML,/简单查资料或列费用可能直接给出结果/);
  assert.match(view.innerHTML,/回到咨询/);
  document.querySelectorAll('[data-fill]')[0].onclick();
  assert.equal(readGuide(store).step,0,'filling an example must not send or advance');
  node('guide-form').fire('submit');assert.equal(readGuide(store).step,1);
  type('guide-input','我还不确定，先保留未知。');node('guide-form').fire('submit');
  node('guide-edit').fire('click');type('guide-input','刚才的上限未定，请先别填数字。');
  assert.equal(node('guide-confirm'),null,'confirmation is unavailable while editing');
  await mount(chat);assert.equal(node('guide-input').value,'刚才的上限未定，请先别填数字。');
  assert.equal(node('guide-confirm'),null);
  node('guide-correction').fire('submit');node('guide-confirm').fire('click');
  assert.equal(readGuide(store).step,3);assert.equal(node('guide-finish').disabled,true);
  assert.match(view.innerHTML,/随后发出的练习修改：刚才的上限未定/);
  const source=view.querySelector('.guide-source');source.open=true;source.fire('toggle');
  assert.equal(node('guide-finish').disabled,false);node('guide-finish').fire('click');
  assert.equal(node('guide-real-input').value,'');assert.equal(store.getItem(OPENING_DRAFT_KEY),null);
  type('guide-real-input','我真正想讨论的是供应商延迟交货。');node('guide-handoff').fire('submit');
  const opening=JSON.parse(store.getItem(OPENING_DRAFT_KEY));
  assert.equal(opening.text,'我真正想讨论的是供应商延迟交货。');
  assert.equal(opening.attempt,null);assert.ok(!JSON.stringify(opening).includes('练习'));
  assert.equal(location.hash,'#/run');assert.equal(requests(),0);
 });
});

test('visible handoff conflict preserves the existing draft until the replacement is chosen',async()=>{
 await setup(async({node,store,mount,type,requests})=>{
  saveGuide(finishedGuide(),store);
  const old={text:'原来的定价问题',mode:'whatif',action:'保持价格',attempt:{id:'existing-attempt'}};
  store.setItem(OPENING_DRAFT_KEY,JSON.stringify(old));await mount();
  type('guide-real-input','新写的真实问题');node('guide-handoff').fire('submit');
  assert.match(node('composer-host').innerHTML,/已经有一份草稿了/);
  assert.deepEqual(JSON.parse(store.getItem(OPENING_DRAFT_KEY)),old);
  assert.equal(location.hash,'#/guide');
  type('guide-real-input','冲突出现后再次改写的问题');node('guide-replace').fire('click');
  assert.equal(JSON.parse(store.getItem(OPENING_DRAFT_KEY)).text,'冲突出现后再次改写的问题');
  assert.deepEqual(JSON.parse(store.getItem(OPENING_DRAFT_KEY+':before-guide')),old);
  assert.equal(location.hash,'#/run');assert.equal(requests(),0);
 });
});

test('write-only storage failure keeps newer practice on navigation and keeps its warning visible',async()=>{
 await setup(async({node,store,mount,type,denyWrites})=>{
  const original=newGuide();original.draft='旧的已保存草稿';saveGuide(original,store);await mount();
  denyWrites(()=>true);type('guide-input','这份是写入失败后的新草稿');
  assert.equal(node('guide-storage-note').hidden,false);
  await mount();assert.equal(node('guide-input').value,'这份是写入失败后的新草稿');
  assert.equal(node('guide-storage-note').hidden,false);
  node('guide-form').fire('submit');await mount();assert.equal(readGuide(store).step,1);
  denyWrites(null);type('guide-input','恢复存储后的回答');
  assert.equal(node('guide-storage-note').hidden,true);
  await mount();assert.equal(node('guide-input').value,'恢复存储后的回答');
 });
});

test('a handoff write failure leaves the real question visible and never claims it was sent',async()=>{
 await setup(async({node,store,mount,type,denyWrites,requests})=>{
  saveGuide(finishedGuide(),store);await mount();type('guide-real-input','遇到存储错误也不能丢的真实问题');
  denyWrites(key=>key===OPENING_DRAFT_KEY);node('guide-handoff').fire('submit');
  assert.equal(location.hash,'#/guide');assert.equal(store.getItem(OPENING_DRAFT_KEY),null);
  assert.equal(node('guide-real-input').value,'遇到存储错误也不能丢的真实问题');
  assert.match(node('composer-host').innerHTML,/没有发送/);
  denyWrites(null);node('guide-handoff').fire('submit');
  assert.equal(location.hash,'#/run');assert.equal(requests(),0);
 });
});
