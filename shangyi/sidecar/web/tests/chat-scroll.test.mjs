import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';
import {render,destroy} from '../js/views/chat.js';

const id='chat-000000000000000000000088';
const snapshot=(extra={})=>({id,title:'长对话',topic:'negotiation',status:'thinking',alive:true,
 revision:1,messages:[{type:'message',text:'已有分析内容'}],pending:null,...extra});

async function withLayout(run){
 const names=['document','sessionStorage','setInterval','clearInterval','setTimeout'];
 const originals=Object.fromEntries(names.map(k=>[k,globalThis[k]])),savedApi={...api};
 const elements=new Map(),layout={contentHeight:2731,composerViewport:628};
 let top=0,height=774,viewport=774,poll;
 const scroller={get scrollHeight(){return height;},get clientHeight(){return viewport;},
  get scrollTop(){return top;},set scrollTop(value){top=Math.max(0,Math.min(value,height-viewport));}};
 function node(key){
  if(key==='view')return scroller;
  if(!elements.has(key)){
   let html='';
   elements.set(key,{id:key,value:'',textContent:'',disabled:false,events:{},
    get innerHTML(){return html;},set innerHTML(value){html=value;if(key==='composer-host')viewport=layout.composerViewport;},
    focus(){},setAttribute(){},addEventListener(type,callback){this.events[type]=callback;}});
  }
  return elements.get(key);
 }
 let viewHtml='';
 const view={get innerHTML(){return viewHtml;},set innerHTML(value){viewHtml=value;height=layout.contentHeight;},
  querySelector:selector=>node(selector.slice(1)),querySelectorAll(){return [];}};
 globalThis.document={getElementById:node};
 globalThis.sessionStorage={getItem(){return null;},setItem(){},removeItem(){}};
 globalThis.setInterval=callback=>{poll=callback;return 1;};globalThis.clearInterval=()=>{};globalThis.setTimeout=()=>0;
 try{await run({view,node,scroller,layout,poll:async()=>{poll();await Promise.resolve();await Promise.resolve();}});}
 finally{destroy();Object.assign(api,savedApi);for(const key of names)globalThis[key]=originals[key];}
}

test('opening a long chat lands at the latest question after the composer reduces the viewport',async()=>{
 await withLayout(async({view,scroller})=>{
  api.chat=async()=>snapshot({status:'waiting',pending:{kind:'question',request_id:'question-1',payload:{questions:[]}}});
  await render(view,{match:['',id],isCurrent:()=>true});
  assert.equal(scroller.scrollHeight,2731);
  assert.equal(scroller.clientHeight,628);
  assert.equal(scroller.scrollTop,2103);
  assert.equal(scroller.scrollHeight-scroller.clientHeight-scroller.scrollTop,0);
 });
});

test('new messages follow the bottom after layout changes while reading older messages keeps the position',async()=>{
 await withLayout(async({view,scroller,layout,poll})=>{
  let data=snapshot();api.chat=async()=>data;
  await render(view,{match:['',id],isCurrent:()=>true});
  scroller.scrollTop=900;
  layout.contentHeight=3100;data=snapshot({revision:2});await poll();
  assert.equal(scroller.scrollTop,900);
  scroller.scrollTop=scroller.scrollHeight-scroller.clientHeight-40;
  layout.contentHeight=3400;layout.composerViewport=540;
  data=snapshot({revision:3,status:'waiting',pending:{kind:'question',request_id:'question-2',payload:{questions:[]}}});await poll();
  assert.equal(scroller.scrollTop,2860);
 });
});

test('unchanged polling does not pull a reader back down',async()=>{
 await withLayout(async({view,scroller,poll})=>{
  api.chat=async()=>snapshot();await render(view,{match:['',id],isCurrent:()=>true});
  scroller.scrollTop=scroller.scrollHeight-scroller.clientHeight-80;
  const readingPosition=scroller.scrollTop;await poll();
  assert.equal(scroller.scrollTop,readingPosition);
});
});

test('a delivered partial task keeps a composer without pretending analysis is still running',async()=>{
 await withLayout(async({view,node,poll})=>{
  let data=snapshot({status:'partial',task_result:{scope:'cost',status:'partial',missing:['押金金额未取得 <script>unsafe</script>']}});
  api.chat=async()=>data;
  await render(view,{match:['',id],isCurrent:()=>true});
  assert.match(view.innerHTML,/交了能交的/);
  assert.match(view.innerHTML,/押金金额未取得 &lt;script&gt;/);
  assert.ok(!view.innerHTML.includes('<script>'));
  assert.ok(!view.innerHTML.includes('conversation-progress'));
  assert.ok(!node('composer-host').innerHTML.includes('正在分析'));
  assert.match(node('composer-host').innerHTML,/结果已交付/);
  assert.match(node('composer-host').innerHTML,/id="chat-input"/);
  data=snapshot({revision:2,status:'thinking',task_result:null});await poll();
  assert.match(view.innerHTML,/conversation-progress/);
  assert.match(node('composer-host').innerHTML,/正在分析/);
  assert.ok(!view.innerHTML.includes('已交付取得的部分'));
 });
});

test('a successful user reply moves to the latest message even when sent while reading above',async()=>{
 await withLayout(async({view,node,scroller,layout})=>{
  api.chat=async()=>snapshot();api.chatMessage=async()=>snapshot({revision:2});
  await render(view,{match:['',id],isCurrent:()=>true});
  scroller.scrollTop=300;
  node('chat-input').value='补充一条新信息';node('chat-input').oninput();
  layout.contentHeight=3300;layout.composerViewport=600;
  await node('chat-send').onclick();
  assert.equal(scroller.scrollTop,2700);
  assert.equal(scroller.scrollHeight-scroller.clientHeight-scroller.scrollTop,0);
 });
});

test('a gap in the decision frame can be filled straight into the composer',async()=>{
 await withLayout(async({view,node})=>{
  const data=snapshot({frame:{question:'要不要在附近开店',type:'新生意',mode:'看可能',
   move:{side:'我方',value:null,via:null},players:[],facts:[],assumptions:[],
   gaps:[{label:'rent',why:'公开资料未取得'}],result:null}});
  api.chat=async()=>data;
  await render(view,{match:['',id],isCurrent:()=>true});
  const panel=node('evidence-content');
  assert.equal(typeof panel.onclick,'function','帧的缺口要挂上点击');
  assert.match(node('evidence-content').innerHTML,/data-fill="挂的租金："/,'缺口用日常词当填充词');
  panel.onclick({target:{closest:()=>({dataset:{fill:'挂的租金：'}})}});
  assert.equal(node('chat-input').value,'挂的租金：');
  panel.onclick({target:{closest:()=>({dataset:{fill:'我的客单价：'}})}});
  assert.equal(node('chat-input').value,'挂的租金：\n我的客单价：','再点一个缺口是追加，不覆盖');
  assert.equal(panel.onclick({target:{closest:()=>null}}),undefined,'点别处不该有反应');
 });
});
