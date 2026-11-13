import {esc} from '../util.js';
import {assistant,userMessage,composer,evidence,setStatus,bindEnter} from '../ui.js';
import {readGuide,saveGuide,guideIsSaved,guideAction,guidePremises,prepareOpening} from '../guide-state.js';

const PREMISE_LABEL={goal:'练习目标：',rent:'你刚才的练习回答：',landlord:'房东愿不愿意让价：'};
const PREMISE_NOTE={fixed:'练习设定，不用你改。',kept:'之后没有修改，这就是要确认的说法。',revised:'原文保留；你之后发的修改列在下面，前后说法冲突时才以最后一条为准。'};
// 合并后的「当前有效前提」清单：原回答与修改并排全列，谁都不删谁，
// 用户不用自己把旧清单和修改消息拼起来；拿不到的内容直接说明，不留空行冒充回答。
function premiseRows(premises){
 return '<div class="review-item">'+premises.rows.map(row=>{
  if(row.status==='revision')return '<p>你之后发送的修改（第 '+row.order+' 条'+(row.latest?'，最新说法':'')+'）：'+esc(row.text)+'</p>';
  const note=PREMISE_NOTE[row.status];
  return '<p>'+esc((PREMISE_LABEL[row.key]??'')+(row.text||'（这条没有取到内容）'))+(note?'<span>'+esc(note)+'</span>':'')+'</p>';
 }).join('')+'</div>';
}
// 说明这份清单就是确认按钮对应的内容，界面显示的与提交的保持一致。
function premiseNote(premises,step){
 if(step!==2)return '你确认的就是上面这份清单：原回答加上之后发送的全部修改，哪一条都没有被删掉。';
 return premises.revised?'确认按钮对应上面整份清单：原回答加上全部修改，越晚发送的说法越新；前后说法冲突时，以最后一条修改为准。':'确认按钮对应上面这份清单；有问题可以直接修改，也可以先问清楚。确认前提不代表事情一定会这样发生。';
}

export async function render(view,ctx){
 let state=readGuide(),editing=false,error='',conflict='',saved=guideIsSaved();
 editing=state.step===2&&Boolean(state.draft);
 const store=()=>{saved=saveGuide(state);const note=document.getElementById('guide-storage-note');if(note)note.hidden=saved;};
 function act(action,text){
  try{state=guideAction(state,action,text);error='';editing=false;store();draw(true);}
  catch(e){error=e.message;draw(false);}
 }
 function handoff(replace=false){
  state.realDraft=document.getElementById('guide-real-input')?.value??state.realDraft;store();
  try{
   const result=prepareOpening(state.realDraft,undefined,replace);
   if(result.conflict){conflict=result.previous;error='';draw(false);return;}
   location.hash='#/run';
  }catch(e){error=e.message;draw(false);}
 }
 function draw(moveToEnd=false){
  if(!ctx.isCurrent())return;
  setStatus(state.step===4?'练习完成':'引导练习 '+(state.step+1)+'/4');
  const steps=['说说问题','回应追问','核对与修改','看看依据'];
  const backTo=ctx.returnTo||'#/run',backLabel=backTo.startsWith('#/chat/')?'回到咨询':backTo==='#/run'?'聊我的事':'回上一页';
  let html='<div class="guide-heading"><div><span class="guide-tag">使用引导 · 只是练习</span><h1 class="page-title">跟我聊一小段</h1></div><a class="quiet-button" href="'+esc(backTo)+'">'+backLabel+'</a></div>'+
   '<p class="page-caption guide-caption">只练操作，不真分析。随时走，随时回来接着练。</p>'+
   '<ol class="guide-steps" aria-label="引导进度">'+steps.map((label,i)=>'<li'+(i===state.step?' aria-current="step"':'')+' class="'+(i<state.step?'done':'')+'"><span>'+(i+1)+'</span>'+label+'</li>').join('')+'</ol>';
  html+=assistant('<h2>先用一个小店例子</h2><p>假设房东想涨租，你还没想好怎么谈。开场只要说清正在考虑什么、最担心什么，不用先准备完整资料。</p><p class="small-note">例如：“房东想涨租，我想看看有哪些谈法。”</p><p class="small-note">这次练习演示需要核对假设、比较做法的问题。简单查资料或列费用可能直接给出结果，不一定需要逐步确认。</p>');
  if(state.step>0){
   html+=userMessage(state.opening)+assistant('<p>这样就开始了。真实咨询会围绕你的问题继续追问；这次练习仍用涨租的例子。</p><h3>你每月最多愿意付多少租金？</h3><p>知道就直接说。不知道也没关系，可以说“还没想好”，后面再补充。</p>');
  }
  if(state.step>1){
   const premises=guidePremises(state);
   html+=userMessage(state.answer)+assistant('<h3>'+(state.step===2?'现在核对这份练习前提':'你已经确认的练习前提')+'</h3>'+premiseRows(premises)+'<p>'+premiseNote(premises,state.step)+'</p>');
   if(state.asked)html+=userMessage('这些信息是从哪里来的？')+assistant('<p>练习中的回答来自你刚才的输入；房东会怎么做仍是未知。真实咨询里，你也可以问“这个数字哪来的？”“哪个是事实，哪个是假设？”</p>');
   for(const correction of state.corrections)html+=userMessage(correction)+assistant('<p>修改意见已记下，并入了上面的待确认清单：</p><div class="review-item">'+esc(correction)+'</div><p class="small-note">真实咨询会按补充重新整理前提，再请你核对。这里保留你的修改原文，不把它当作实际经营信息。</p>');
  }
  if(state.step>2){
   html+=userMessage('确认这份练习前提')+assistant('<h3>读建议时，也看看它靠什么成立</h3><p>练习建议：先想清自己能接受的范围，再和房东谈；对方是否同意仍未知。</p><p>真实咨询中，右上角的“看依据”能看资料、已知费用和假设。你也可以直接问：“如果这条假设不成立，结果会怎样？”</p><details class="guide-source" '+(state.sourcesSeen?'open':'')+'><summary id="guide-sources">查看这条练习建议的依据</summary><ul><li>原始练习回答：'+esc(state.answer)+'</li>'+state.corrections.map(text=>'<li>随后发出的练习修改：'+esc(text)+'</li>').join('')+'<li>房东愿不愿意让价：没有信息，保留未知。</li><li>这只是操作示例，没有查询真实报价，也没有预测谈判结果。</li></ul></details>');
  }
  if(state.step===4){
   html+='<section class="guide-finish"><h2>现在可以聊你自己的事了</h2><p>记住这几句就够用：</p><ul><li>“我正在考虑……，最担心……”</li><li>“这点不知道，先保留未知。”</li><li>“这里不对，请改成……”</li><li>“这个结论是根据什么得出的？”</li></ul><p>下面只填写你的真实问题。练习里的回答、数字和修改都不会带过去。</p><button class="quiet-button" id="guide-restart">再练一次</button></section>';
  }
  view.innerHTML=html;
  const storageNote='<p id="guide-storage-note" class="send-error" role="status" '+(saved?'hidden':'')+'>现在保存不了进度；本页还能用，刷新可能丢。</p>';
  let controls='';
  if(state.step<2){
   const opening=state.step===0;
   controls='<form id="guide-form"><label for="guide-input">'+(opening?'练习开场：写一句就好':'练习回答：知道多少说多少')+'</label><textarea id="guide-input" rows="2" maxlength="2000" placeholder="'+(opening?'比如：房东想涨租，我不知道怎么谈。':'比如：我还没想清楚能接受多少。')+'">'+esc(state.draft)+'</textarea><div class="choice-list guide-examples">'+(opening?'<button type="button" class="choice-button" data-fill="房东想涨租，我想看看有哪些谈法。">填入练习开场</button>':'<button type="button" class="choice-button" data-fill="最多每月1.1万元（练习数字）。">填入一个示例回答</button><button type="button" class="choice-button" data-fill="我还没想清楚，先保留未知。">我还不确定</button>')+'</div><div class="guide-actions"><span class="small-note">这些按钮只填入文字，点击发送后继续。</span><button class="primary-button" type="submit">发送</button></div></form>';
  }else if(state.step===2){
   controls=editing?'<div class="guide-actions"><span class="small-note">先发修改，再看更新后的内容。</span><button class="quiet-button" id="guide-cancel-edit">取消修改</button></div>':'<div class="guide-actions"><button class="primary-button" id="guide-confirm">确认</button><button class="secondary-button" id="guide-edit">要改</button><button class="quiet-button" id="guide-ask">先问清楚</button></div>';
   if(editing)controls+='<form id="guide-correction"><label for="guide-input">写下哪里需要修改</label><textarea id="guide-input" rows="2" maxlength="2000" placeholder="比如：我刚才的上限还没定，请先保留未知。">'+esc(state.draft)+'</textarea><button class="primary-button" type="submit">发送修改</button></form>';
  }else if(state.step===3){
   controls='<p class="small-note" id="guide-source-hint">'+(state.sourcesSeen?'依据已展开。真咨询里也能这样核对。':'展开依据，看看哪些知道、哪些不知道。')+'</p><button class="primary-button" id="guide-finish" '+(state.sourcesSeen?'':'disabled')+'>聊我的事</button>';
  }else{
   controls='<form id="guide-handoff"><label for="guide-real-input">现在写下你自己的问题</label><textarea id="guide-real-input" rows="2" maxlength="24000" placeholder="我正在考虑……，最担心的是……">'+esc(state.realDraft)+'</textarea><div class="guide-actions"><span class="small-note">先给你检查，点“开始咨询”才发送。</span><button class="primary-button" type="submit">带过去</button></div></form>';
   if(conflict)controls+='<div class="guide-conflict" role="status"><strong>已经有一份草稿了</strong><p>'+esc(conflict)+'</p><a class="secondary-button" href="#/run">用原来的</a><button class="quiet-button" id="guide-replace">用刚写的</button></div>';
  }
  composer('<div class="composer guide-composer">'+controls+'<p class="send-error" role="alert">'+esc(error)+'</p>'+storageNote+'</div>');
  evidence('<div class="evidence-empty"><strong>这次只练习怎样交流</strong><p>练习不跑真分析，也不混进真实咨询记录。</p><p>真咨询里：公开资料商奕查；目标、底线和内部情况你来补；不确定的可以留着。</p><p>可以随时说“这里不对”“先解释一下”；也能停没完的分析。停了不删已存的消息。</p></div>','使用提示');
  const input=document.getElementById('guide-input');
  if(input){input.oninput=()=>{state.draft=input.value;store();};bindEnter(input,()=>act(editing?'correct':'submit',input.value));}
  document.getElementById('guide-form')?.addEventListener('submit',e=>{e.preventDefault();act('submit',input.value);});
  document.getElementById('guide-correction')?.addEventListener('submit',e=>{e.preventDefault();act('correct',input.value);});
  document.querySelectorAll('[data-fill]').forEach(button=>button.onclick=()=>{input.value=button.dataset.fill;state.draft=input.value;store();input.focus();});
  document.getElementById('guide-confirm')?.addEventListener('click',()=>act('confirm'));
  document.getElementById('guide-edit')?.addEventListener('click',()=>{editing=true;draw(false);document.getElementById('guide-input').focus();});
  document.getElementById('guide-cancel-edit')?.addEventListener('click',()=>{editing=false;state.draft='';store();draw(false);});
  document.getElementById('guide-ask')?.addEventListener('click',()=>act('ask'));
  view.querySelector('.guide-source')?.addEventListener('toggle',e=>{
   if(!ctx.isCurrent())return;
   if(e.target.open&&!state.sourcesSeen){state=guideAction(state,'sources');store();document.getElementById('guide-finish')?.removeAttribute('disabled');const hint=document.getElementById('guide-source-hint');if(hint)hint.textContent='依据已展开。真实咨询中也可以这样核对，不用只看结论。';e.target.scrollIntoView({block:'nearest'});}
  });
  document.getElementById('guide-finish')?.addEventListener('click',()=>act('finish'));
  document.getElementById('guide-restart')?.addEventListener('click',()=>act('restart'));
  const real=document.getElementById('guide-real-input');if(real)real.oninput=()=>{state.realDraft=real.value;store();};
  document.getElementById('guide-handoff')?.addEventListener('submit',e=>{e.preventDefault();handoff();});
  document.getElementById('guide-replace')?.addEventListener('click',()=>handoff(true));
  if(moveToEnd){const scroller=document.getElementById('view');scroller.scrollTop=scroller.scrollHeight;}
 }
 draw();
}
