import test from 'node:test';
import assert from 'node:assert/strict';
import {motionEnabled,holdView,enterView,staggerList,enterNew,revealOnScroll} from '../js/motion.js';

// 动效封装是**降级优先**的：引擎没加载、系统要求减弱动效、测试环境没有
// document——任何一种情况下都必须安静地什么都不做，且绝不能把页面留在
// 不可见状态。下面这些用例锁的就是这条底线。

function fakeEngine(){
 const state={animate:[],inView:[]};
 return {state,animate:(...a)=>{state.animate.push(a);},stagger:d=>({__stagger:d}),inView:(el,cb)=>{state.inView.push(el);cb();}};
}
function withEnv(motion,reduced,fn){
 const saved={motion:globalThis.Motion,mq:globalThis.matchMedia,doc:globalThis.document};
 if(motion===undefined)delete globalThis.Motion;else globalThis.Motion=motion;
 globalThis.matchMedia=q=>({matches:reduced,q});
 globalThis.document={};
 try{return fn();}finally{
  if(saved.motion===undefined)delete globalThis.Motion;else globalThis.Motion=saved.motion;
  if(saved.mq===undefined)delete globalThis.matchMedia;else globalThis.matchMedia=saved.mq;
  if(saved.doc===undefined)delete globalThis.document;else globalThis.document=saved.doc;
 }
}
function el(attrs={}){
 return {style:{},getAttribute:k=>(k in attrs?attrs[k]:null),setAttribute(){}};
}
const H=list=>({querySelectorAll:()=>list});
/** 模拟 `#runs`：容器本身跨轮询存活，只有子节点被 innerHTML 换掉。 */
function host(){
 const box={list:[]};
 box.root={querySelectorAll:()=>box.list};
 box.set=list=>{box.list=list;};
 return box;
}

test('没有加载引擎时全部入口静默降级',()=>{
 withEnv(undefined,false,()=>{
  assert.equal(motionEnabled(),false);
  assert.equal(holdView(el()),false);
  enterView(el());staggerList(H([el(),el()]),'x');revealOnScroll(H([el()]),'x');
 });
});

test('引擎缺失时 holdView 不写 opacity，避免整页停在不可见状态',()=>{
 withEnv(undefined,false,()=>{
  const node=el();
  assert.equal(holdView(node),false);
  assert.equal(node.style.opacity,undefined,'降级路径不得留下 opacity:0');
 });
});

test('系统要求减弱动效时不动画（补上 CSS 兜底管不到的一半）',()=>{
 const engine=fakeEngine();
 withEnv(engine,true,()=>{
  assert.equal(motionEnabled(),false);
  const node=el();
  assert.equal(holdView(node),false);
  assert.equal(node.style.opacity,undefined);
  enterView(node);
  assert.equal(engine.state.animate.length,0,'减弱动效时不应调用 animate');
 });
});

test('缺 matchMedia 的环境按减弱动效处理，宁可不动',()=>{
 const saved=globalThis.matchMedia;
 delete globalThis.matchMedia;
 try{
  assert.equal(motionEnabled(),false);
 }finally{
  if(saved===undefined)delete globalThis.matchMedia;else globalThis.matchMedia=saved;
 }
});

test('引擎可用时视图入场被按住并在放开时归位',()=>{
 const engine=fakeEngine();
 withEnv(engine,false,()=>{
  assert.equal(motionEnabled(),true);
  const node=el();
  assert.equal(holdView(node),true);
  assert.equal(node.style.opacity,'0');
  enterView(node);
  assert.equal(engine.state.animate.length,1);
  assert.deepEqual(engine.state.animate[0][1].opacity,[0,1],'必须从 0 起，否则放开前会闪一下');
 });
});

test('enterNew 只为新条目入场，轮询重绘不会整屏反复闪',()=>{
 const engine=fakeEngine();
 withEnv(engine,false,()=>{
  // 容器跨轮询存活，子节点每轮都是新对象——正是 activity.js 的情形。
  const runs=host();
  runs.set([el({'data-item':'run:1'}),el({'data-item':'run:2'})]);
  enterNew(runs.root,'.answer-section');
  assert.equal(engine.state.animate.length,1,'首次两条都是新的');
  // 第二轮轮询：同样的两条，但 innerHTML 已经换成了新节点
  runs.set([el({'data-item':'run:1'}),el({'data-item':'run:2'})]);
  enterNew(runs.root,'.answer-section');
  assert.equal(engine.state.animate.length,1,'同样的条目不应再次入场');
  // 第三轮：新来了一条
  runs.set([el({'data-item':'run:1'}),el({'data-item':'run:2'}),el({'data-item':'run:3'})]);
  enterNew(runs.root,'.answer-section');
  assert.equal(engine.state.animate.length,2,'只有新条目入场');
 });
});

test('条目离场后再回来不重复入场，长期轮询也保持稳定',()=>{
 const engine=fakeEngine();
 withEnv(engine,false,()=>{
  const runs=host();
  runs.set([el({'data-item':'run:1'}),el({'data-item':'run:2'})]);enterNew(runs.root,'.x');
  runs.set([el({'data-item':'run:1'})]);enterNew(runs.root,'.x');
  runs.set([el({'data-item':'run:1'}),el({'data-item':'run:2'})]);enterNew(runs.root,'.x');
  assert.equal(engine.state.animate.length,1,'回来的老条目不应再次入场');
  runs.set([el({'data-item':'run:1'}),el({'data-item':'run:2'}),el({'data-item':'run:3'})]);enterNew(runs.root,'.x');
  assert.equal(engine.state.animate.length,2,'新条目仍要入场');
 });
});

test('单条列表不做错峰，避免为了动画增加一次无谓调用',()=>{
 const engine=fakeEngine();
 withEnv(engine,false,()=>{
  staggerList(H([el()]),'.x');
  assert.equal(engine.state.animate.length,0);
 });
});

test('滚动揭示只对尚未揭示过的节点注册一次',()=>{
 const engine=fakeEngine();
 withEnv(engine,false,()=>{
  const node=el();
  const box=host();box.set([node]);
  revealOnScroll(box.root,'.answer-section');
  assert.equal(engine.state.inView.length,1);
  revealOnScroll(box.root,'.answer-section');
  assert.equal(engine.state.inView.length,1,'同一节点不应重复注册 inView');
 });
});

test('引擎抛错不影响调用方（内容必须照常显示）',()=>{
 const broken={animate(){throw new Error('boom');},stagger:()=>0,inView(){throw new Error('boom');}};
 withEnv(broken,false,()=>{
  const node=el();
  assert.equal(holdView(node),true);
  assert.doesNotThrow(()=>{enterView(node);staggerList(H([el(),el()]),'x');revealOnScroll(H([el()]),'x');});
 });
});