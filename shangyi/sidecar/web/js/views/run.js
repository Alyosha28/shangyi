import {api} from '../api.js';
import {esc} from '../util.js';
import {TOPICS,icon,composer,evidence,setStatus,bindEnter,toast} from '../ui.js';
import {guideEntry,OPENING_DRAFT_KEY} from '../guide-state.js';
import {staggerList} from '../motion.js';

const STORE=OPENING_DRAFT_KEY;
export const EXAMPLES={
 pricing_response:'旁边一家店最近降价了，我不知道要不要跟。想先看看保持原价和跟着降价各会怎样。',
 negotiation:'我想和房东谈续租，租金可能会涨。我想知道自己最多能接受多少，还能谈哪些条件。',
 market_entry:'我想在家附近开一家店，还没决定。想先看看附近有谁在做、店租和开店大概要花多少钱。',
 competitor_intel:'附近另一家店最近在做活动，可能会影响我。我想先弄清它做了什么，再考虑我要不要回应。'
};
let dispose=()=>{};
export function destroy(){dispose();dispose=()=>{};}
/** 点了某个开场示例之后，示例文字该怎么处理。
 *
 *  UI-010：原实现是「主题总是改，只有空框才填示例文字」，于是用户自己写的内容会和
 *  新主题搭在一起，页面没有任何解释。三种情形分开：
 *   - 框是空的                          -> fill（直接填）
 *   - 框里还是某个示例的原文、用户没动过  -> fill（直接换成新的）
 *     （连着试示例时不该被反复问；用户没写自己的东西，没什么可问的）
 *   - 框里是用户自己写的内容              -> ask（必须问，不覆盖也不静默留着错配）
 */
export function exampleTextDecision({box,exampleTopic}){
 const trimmed=typeof box==='string'?box.trim():'';
 if(!trimmed)return 'fill';
 if(exampleTopic&&trimmed===EXAMPLES[exampleTopic].trim())return 'fill';
 return 'ask';
}

/** 要写进框里的示例原文。 */
export function exampleTextFor(topic){return EXAMPLES[topic]||'';}

export async function render(view,ctx){
 let alive=true;dispose=()=>alive=false;
 let health={frontend_contract:3,interactive_chat:false,services:null};
 let draft={};try{draft=JSON.parse(sessionStorage.getItem(STORE)||'{}');}catch{}
 let topic=TOPICS[ctx.match?.[1]]?ctx.match[1]:(draft.topic||'');
 let input=typeof draft.text==='string'?draft.text:'';
 let mode=draft.mode||'scenario',action=draft.action||'',sending=false;
 let attempt=typeof draft.attempt==='object'?draft.attempt:null;
 // UI-010：点示例时主题总会变，但只有空框才填示例文字，于是用户自己写的内容
 // 会和新主题搭在一起，而页面什么都不说。这里记住「框里这段是不是某个示例的原文」，
 // 由此决定能不能直接换文字；是用户自己写的内容时必须问，不能静默覆盖。
 let exampleTopic=TOPICS[topic]&&input===EXAMPLES[topic]?topic:null;
 const save=()=>{try{sessionStorage.setItem(STORE,JSON.stringify({topic,text:input,mode,action,attempt}));}catch{}};
 const intro=h=>'<div class="evidence-empty">'+icon('chat')+'直接说发生了什么、在犹豫什么。可以一次说完，也可以边聊边补。<br><br>只查资料或算钱也行，结果直接列出。公开资料由商奕核对；要比做法时，再请你对目标、底线和关键假设。'+(h?.services?.search?.providers?.bocha?.status==='configured'?'<br><br>搜索配好了。怕按次收费，开始前不试；真查时再验证并给出来源。':h?.services?.search?.status==='empty_probe'?'<br><br>搜索没试出结果；查不到的资料会标“没查到”。':h?.services?.search?.status==='available'?'<br><br>搜索能用了；具体来源仍逐条核对。':'')+'</div>';
 setStatus('检查中');evidence(intro(health),'这次怎么聊');
 view.innerHTML='<section class="welcome"><div class="wordmark"><span class="assistant-mark">弈</span> 商奕 · 经营咨询</div><h1>说说你正在考虑的事。</h1><p>不用先把问题整理完整。把背景、顾虑和初步想法一起告诉我，我们从这里往下梳理。</p>'+guideEntry()+'<div class="topic-options">'+Object.entries(TOPICS).map(([id,t])=>'<button class="topic-option" data-example="'+id+'">'+icon(t.icon)+'<strong>'+esc(t.label)+'</strong><span>'+esc(t.hint)+'</span></button>').join('')+'</div><p class="small-note start-note">这些只是开场示例，你也可以直接输入自己的问题。换示例时会先问你要不要连问题一起换。</p></section>';
 // 首屏默认路由：四个开场示例错峰入场。本页不轮询、无滚动锚定，安全。
 staggerList(view,'.topic-option');
 composer('<form class="composer freeform-composer" id="start-conversation"><label class="visually-hidden" for="brief-input">描述你想讨论的问题</label><textarea id="brief-input" rows="3" placeholder="例如：我想续租现在的门店，但房东提出涨租。我还没确定自己的底线，想先聊聊有哪些谈法。">'+esc(input)+'</textarea><details class="conversation-settings"><summary>设置 <span id="topic-hint">'+esc(TOPICS[topic]?.label||'按描述选')+'</span></summary><div class="settings-grid"><label>聊哪方面<select id="chat-topic"><option value="">按描述选（可改）</option>'+Object.entries(TOPICS).map(([id,t])=>'<option value="'+id+'">'+esc(t.label)+'</option>').join('')+'</select></label><label>最想怎样<select id="chat-mode"><option value="scenario">先看看有哪些可能</option><option value="whatif">看看某个做法会怎样</option><option value="discover">找找适合我的做法</option></select></label></div><label id="chat-action-wrap" '+(mode==='whatif'?'':'hidden')+'>你的做法<input class="search-field" id="chat-action" value="'+esc(action)+'" placeholder="例如：接受小幅涨租，换取更长免租期"></label></details><div id="start-error" role="alert"></div><div id="start-example-conflict" role="status" hidden></div><div class="composer-toolbar"><span class="small-note">可以补充、纠正，也可以说“不确定”</span><button class="send-button" id="start-send" type="submit" disabled>'+icon('arrow')+'<span>开始咨询</span></button></div></form><div class="composer-hint" role="status" aria-live="polite">正在检查。可以先写问题，草稿会保留。</div>');
 const ta=document.getElementById('brief-input');
 const pb=document.getElementById('chat-topic'),mo=document.getElementById('chat-mode');
 pb.value=topic;mo.value=mode;
 ta.oninput=()=>{input=ta.value;if(input.trim()!==(exampleTopic?EXAMPLES[exampleTopic].trim():''))exampleTopic=null;save();};
 pb.onchange=()=>{topic=pb.value;document.getElementById('topic-hint').textContent=TOPICS[topic]?.label||'按描述选';save();};
 mo.onchange=()=>{mode=mo.value;document.getElementById('chat-action-wrap').hidden=mode!=='whatif';save();};
 document.getElementById('chat-action').oninput=e=>{action=e.target.value;save();};
 view.querySelectorAll('[data-example]').forEach(b=>b.onclick=()=>{
  const next=b.dataset.example;
  topic=next;pb.value=topic;document.getElementById('topic-hint').textContent=TOPICS[next].label;
  if(exampleTextDecision({box:ta.value,exampleTopic})==='fill'){
   ta.value=EXAMPLES[next];input=ta.value;exampleTopic=next;hideExampleConflict();
  }else{
   // 用户自己写的内容：主题已经按他点的改了，但**问题文字不动**，并明确问他要哪一样。
   // 两种答案各自自洽，不会留下没说明的错配。
   exampleTopic=null;showExampleConflict(next);
  }
  save();ta.focus();
 });
 function showExampleConflict(next){
  const host=document.getElementById('start-example-conflict');
  if(!host)return;
  host.hidden=false;
  host.innerHTML='<p class="inline-error">主题已经换成「'+esc(TOPICS[next].label)+'」，但框里还是你自己写的那段。要换成这个示例的问题，还是把主题改回「按描述选」？</p>'
   +'<div class="choice-list"><button class="choice-button" id="use-example-text">换成示例的问题</button>'
   +'<button class="choice-button" id="keep-my-text">保留我的问题，主题按描述选</button></div>';
  document.getElementById('use-example-text').onclick=()=>{
   ta.value=EXAMPLES[next];input=ta.value;exampleTopic=next;hideExampleConflict();save();ta.focus();};
  document.getElementById('keep-my-text').onclick=()=>{
   topic='';pb.value='';document.getElementById('topic-hint').textContent='按描述选';hideExampleConflict();save();ta.focus();};
 }
 function hideExampleConflict(){
  const host=document.getElementById('start-example-conflict');
  if(host){host.hidden=true;host.innerHTML='';}
 }
 async function send(){
  if(sending||!alive||health.frontend_contract<3||!health.interactive_chat)return;
  input=ta.value;const err=document.getElementById('start-error');
  if(!input.trim()){ta.focus();return;}
  if(mode==='whatif'&&!action.trim()){err.innerHTML='<p class="inline-error">在“设置”里写下你打算怎么做。</p>';document.querySelector('.conversation-settings').open=true;document.getElementById('chat-action').focus();return;}
  const signature=JSON.stringify([input.trim(),topic,mode,action]);
  if(!attempt||attempt.signature!==signature)attempt={signature,id:crypto.randomUUID()};
  save();sending=true;document.getElementById('start-send').disabled=true;document.querySelector('#start-send span').textContent='正在启动…';setStatus('正在启动');err.innerHTML='';
  const submittedDraft=JSON.stringify({topic,text:input,mode,action,attempt});
  try{
   const chat=await api.startChat({text:input,playbook:topic||undefined,mode,action:mode==='whatif'?action:undefined,message_id:attempt.id});
   let laterDraft=false;
   try{
    const saved=sessionStorage.getItem(STORE);
    if(saved===submittedDraft)sessionStorage.removeItem(STORE);else laterDraft=Boolean(saved);
   }catch{}
   window.dispatchEvent(new Event('shangyi:history'));
   if(ctx.isCurrent()){
    if(laterDraft)toast('已开始。刚改的内容还在草稿里，没发送。');
    location.hash='#/chat/'+chat.id;
   }
  }catch(e){if(ctx.isCurrent()){err.innerHTML='<p class="inline-error">'+esc(e.message)+' 草稿已保留。</p>';setStatus('可以再试');}}
  finally{sending=false;if(ctx.isCurrent()){document.getElementById('start-send').disabled=false;document.querySelector('#start-send span').textContent='开始咨询';}}
 }
 bindEnter(ta,send);document.getElementById('start-conversation').onsubmit=e=>{e.preventDefault();return send();};
 function checkHealth(){
  document.querySelector('#composer-host .composer-hint').textContent='正在检查。可以先写问题，草稿会保留。';
  api.health().then(next=>{
  if(!alive||!ctx.isCurrent())return;
  health=next;
  const ready=health.frontend_contract>=3&&health.interactive_chat;
  document.getElementById('start-send').disabled=!ready||sending;
  document.querySelector('#composer-host .composer-hint').textContent=ready?'Enter 发送 · Shift + Enter 换行 · 可以随时补充或纠正':'还没准备好；写的问题会保留。';
  if(health.frontend_contract<3)document.getElementById('start-error').textContent='服务要更新，现在还不能开始。';
  setStatus(ready?'可以开始':'还没准备好');evidence(intro(health),'这次怎么聊');
  }).catch(()=>{
  if(!alive||!ctx.isCurrent())return;
  setStatus('连不上服务');
   const hint=document.querySelector('#composer-host .composer-hint');
   hint.innerHTML='连不上服务，写的问题会保留。<button class="quiet-button" id="retry-health" type="button">重新连接</button>';
   document.getElementById('retry-health').onclick=checkHealth;
  });
 }
 checkHealth();
}
