import test from 'node:test';
import assert from 'node:assert/strict';
import {toast,setStatus,evidence,emptyEvidence,sourceItem} from '../js/ui.js';

// ui.js 这三个组件都接在**会被轮询反复重设**的全局节点上
// （#toast / #page-status / #evidence-content）。所以真正的考点不是
// 「动画好看不好看」，而是**只在真变化时才动**——否则每 2.5s 轮询一次
// 就闪一次，比没有动画更糟。下面锁的就是这条。

const motionEngine=()=>{
 const state={animate:[]};
 return {state,animate:(el,kf,o)=>{state.animate.push({el,kf,o});return {finished:Promise.resolve()};},
  stagger:d=>({d}),inView:(el,cb)=>cb()};
};

function withDom(engine,fn){
 const saved={document:globalThis.document,motion:globalThis.Motion,mq:globalThis.matchMedia,
  st:globalThis.setTimeout,ct:globalThis.clearTimeout};
 const els=new Map(),timers=[];
 const node=id=>{
  if(!els.has(id))els.set(id,{id,innerHTML:'',textContent:'',className:'',hidden:true,style:{}});
  return els.get(id);
 };
 globalThis.document={getElementById:node,createElement:()=>({click(){},style:{},setAttribute(){}})};
 globalThis.matchMedia=()=>({matches:false});
 if(engine===undefined)delete globalThis.Motion;else globalThis.Motion=engine;
 // 桩掉定时器：既能断言「4.5s 后收掉」，也避免测试进程被挂起的真实定时器拖住。
 globalThis.setTimeout=(fn,ms)=>{timers.push({fn,ms});return timers.length;};
 globalThis.clearTimeout=()=>{};
 try{
  return fn({node,timers});
 }finally{
  globalThis.document=saved.document;globalThis.matchMedia=saved.mq;
  globalThis.setTimeout=saved.st;globalThis.clearTimeout=saved.ct;
  if(saved.motion===undefined)delete globalThis.Motion;else globalThis.Motion=saved.motion;
 }
}

const item=key=>({getAttribute:()=>key,style:{}});

/** 让证据面板的 querySelectorAll 忠实反映**当前已渲染的 html**。
 *  若固定返回同一批条目，就测不出「新增条目才入场」——判重会正确地
 *  把第二批当成老条目挡掉，测试会给出假结论。 */
const wireEvidence=host=>{
 host.querySelectorAll=sel=>sel!=='.evidence-item'?[]:
  [...String(host.innerHTML).matchAll(/data-item="([^"]*)"/g)].map(m=>item(m[1]));
};

test('setStatus 重复设同样的字不动画（轮询每 2.5s 会重设一次）',()=>{
 const e=motionEngine();
 withDom(e,({node})=>{
  setStatus('分析进行中');
  const after1=e.state.animate.length;
  assert.equal(after1,1,'首次应入场');
  setStatus('分析进行中');
  setStatus('分析进行中');
  assert.equal(e.state.animate.length,after1,'同样的字不应再动，否则状态条一直闪');
  assert.equal(node('page-status').textContent,'分析进行中');
 });
});

test('setStatus 文字真变化时才再动画',()=>{
 const e=motionEngine();
 withDom(e,()=>{
  setStatus('分析进行中');
  setStatus('已交付结果，可继续对话');
  assert.equal(e.state.animate.length,2,'换了内容应再入场');
 });
});

test('evidence 内容没变就不动（证据面板每轮询都会被重设）',()=>{
 const e=motionEngine();
 withDom(e,({node})=>{
  wireEvidence(node('evidence-content'));
  const html='<div class="evidence-item" data-item="a"></div><div class="evidence-item" data-item="b"></div>';
  evidence(html);
  assert.equal(e.state.animate.length,1,'首次两条一起入场，算一次调用');
  evidence(html);
  evidence(html);
  assert.equal(e.state.animate.length,1,'内容没变不应再动');
 });
});

test('evidence 内容变化时新条目入场',()=>{
 const e=motionEngine();
 withDom(e,({node})=>{
  const host=node('evidence-content');
  wireEvidence(host);
  evidence('<div class="evidence-item" data-item="a"></div>');
  const before=e.state.animate.length;
  evidence('<div class="evidence-item" data-item="a"></div><div class="evidence-item" data-item="b"></div>');
  assert.ok(e.state.animate.length>before,'新增的 b 应有入场');
  // 而且只应给新来的 b 入场，老条目 a 不重复入场
  const last=e.state.animate[e.state.animate.length-1];
  assert.equal(last.el.length,1,'只对新条目入场');
  assert.equal(last.el[0].getAttribute(),'b');
  assert.ok(host.innerHTML.includes('data-item="a"'),'enterNew 必须在 innerHTML 写入之后解析');
 });
});

test('emptyEvidence 重置判重，换上下文后同名条目会重新入场',()=>{
 const e=motionEngine();
 withDom(e,({node})=>{
  wireEvidence(node('evidence-content'));
  evidence('<div class="evidence-item" data-item="x"></div>');
  const first=e.state.animate.length;
  emptyEvidence();                       // app.js 每次换路由都会调
  evidence('<div class="evidence-item" data-item="x"></div>');
  assert.ok(e.state.animate.length>first,'换上下文后应重新入场');
 });
});

test('sourceItem 带 data-item 且标题含引号时属性不被截断',()=>{
 const html=sourceItem('他说"这个价不能做"','公开报价','原文','file');
 assert.match(html,/data-item="/);
 assert.ok(!/data-item="[^"]*他说"这个价/.test(html),'裸引号会提前闭合属性，必须转义');
 assert.match(html,/&quot;/);
});

test('toast 到时收起，且走的是出场回调而不是硬隐藏',async()=>{
 const e=motionEngine();
 await withDom(e,async({node,timers})=>{
  const t=node('toast');
  toast('已请求停止分析');
  assert.equal(t.hidden,false,'应当先显示');
  assert.equal(timers.length,1);
  timers[0].fn();                        // 手动触发 4.5s 定时器
  await Promise.resolve();await Promise.resolve();
  assert.equal(t.hidden,true,'出场回调必须把 hidden 置回，否则永远留在屏幕上');
 });
});

test('新的 toast 不会被旧定时器收掉',async()=>{
 const e=motionEngine();
 await withDom(e,async({node,timers})=>{
  const t=node('toast');
  toast('第一条');timers[0].fn();
  toast('第二条');                        // 新的弹出来了
  await Promise.resolve();await Promise.resolve();
  assert.equal(t.hidden,false,'旧定时器不得收起新 toast');
  assert.equal(t.textContent,'第二条');
  timers[timers.length-1].fn();
  await Promise.resolve();await Promise.resolve();
  assert.equal(t.hidden,true);
 });
});

test('引擎缺失时 toast 仍会按时收起（fadeOut 必须同步回调，不能吞 done）',()=>{
 withDom(undefined,({node,timers})=>{
  const t=node('toast');
  toast('内容已复制');
  assert.equal(t.hidden,false);
  timers[0].fn();                        // 无引擎：出场回调应同步执行
  assert.equal(t.hidden,true,'降级路径必须仍然收起 toast');
 });
});

test('引擎缺失时 setStatus 与 evidence 照常写入内容',()=>{
 withDom(undefined,({node})=>{
  setStatus('分析进行中');
  assert.equal(node('page-status').textContent,'分析进行中');
  const host=node('evidence-content');
  host.querySelectorAll=()=>[];
  evidence('<p>依据</p>');
  assert.equal(host.innerHTML,'<p>依据</p>');
 });
});