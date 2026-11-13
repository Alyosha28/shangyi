import {renderResearch} from '../research.js';
import {renderFrame,renderPoi} from '../frame.js';
import {renderMarkdown} from '../markdown.js';
import {api,startPoll,stopPoll} from '../api.js';
import {esc,PHASE_LABEL,costScopeNote} from '../util.js';
import {icon,assistant,userMessage,composer,evidence,sourceItem,setStatus,topicName,bindEnter} from '../ui.js';
let timer;
export function destroy(){if(timer)stopPoll(timer);timer=null;}
const STATUS={thinking:'正在分析',waiting:'等你回应',completed:'已完成',partial:'已交付，部分信息未取得',blocked:'停下来了',failed:'没启动成',interrupted:'断开了',stopped:'已停止'};
const TASK_LABEL={research:'资料整理',cost:'费用整理',research_and_cost:'资料和费用',consultation:'经营分析'};
export function renderTaskResult(data){
 const result=data.task_result;if(!result)return '';
 const partial=result.status==='partial',label=TASK_LABEL[result.scope]||'这次做啥';
 return '<div class="conversation-end"><strong>'+esc(partial?'交了能交的':label+'已完成')+'</strong>'+
  (partial&&Array.isArray(result.missing)&&result.missing.length?'<p>这些还没问到：</p><ul>'+result.missing.map(item=>'<li>'+esc(item)+'</li>').join('')+'</ul>':'')+
  (costScopeNote(result)?'<p class="small-note">'+esc(costScopeNote(result))+'</p>':'')+
  '<p>'+esc(data.alive?'还能接着补、接着改，或另问一个。':'这次的记录和依据还在。')+'</p>'+
  (data.case?'<a href="#/case/'+data.case.split('/').map(encodeURIComponent).join('/')+'" class="secondary-button">看资料和怎么算的</a>':'')+'</div>';
}
const progressText=text=>/^(阶段推进到|等待用户确认|阶段 .+ turn 完成|模型建议回退到阶段)/.test(text);
export function mergeChoiceDraft(current,question,option){
 const prefix=(question?.text||'当前问题')+'：';
 const rows=current?String(current).split(/\r?\n/):[];
 const at=rows.findIndex(row=>row.startsWith(prefix));
 if(at<0)rows.push(prefix+option);else rows[at]=prefix+option;
 return rows.join('\n');
}
export function renderRequest(e){
 const p=e.payload||{};
 if(e.kind==='question'){
  return assistant((p.questions||[]).map(q=>'<p>'+esc(q.text)+'</p>').join('')+
    '<span class="small-note">可以一起答，也可先聊更要紧的。</span>');
 }
 if(e.kind==='checkpoint'){
  return assistant('<h3>先核对这些前提</h3><p class="small-note">认下≠证实；来源和还不知道的都会留着。</p>'+ (p.items||[]).map(x=>'<div class="review-item"><p>'+esc(x.content)+'</p>'+(x.proposed_value?'<span>等你定：'+esc(x.proposed_value)+'</span>':'')+(x.source?'<small>来源：'+esc(x.source)+'</small>':'')+'</div>').join(''));
 }
 return assistant('<h3>前面要重做</h3><p>'+esc(p.reason||'')+'</p><p class="small-note">同意就回“'+esc(PHASE_LABEL[p.target]||p.target)+'”重来；后面的结果要重新生成。</p>');
}
export async function render(view,ctx){
 destroy();const id=ctx.match[1],draftKey='shangyi-chat-draft:'+id,attemptKey='shangyi-chat-pending:'+id;
 let input='';try{input=sessionStorage.getItem(draftKey)||'';}catch{}
 let current=null,signature='',composerState='',polling=false,sending=false,attempt=null,sendError='',deliveryNotice='',stopping=false,stopError='';
 try{
  const saved=JSON.parse(sessionStorage.getItem(attemptKey)||'null');
  if(saved&&typeof saved.id==='string'&&typeof saved.payload?.text==='string'&&typeof saved.payload?.action==='string')attempt=saved;
 }catch{}
 const save=()=>{try{sessionStorage.setItem(draftKey,input);}catch{}};
 function preserveScroll(forceLatest=false){
  const scroller=document.getElementById('view'),oldTop=scroller.scrollTop;
  const followLatest=forceLatest||!signature||scroller.scrollHeight-oldTop-scroller.clientHeight<100;
  return ()=>{
   if(!ctx.isCurrent())return;
   // Read the final layout after both the message list and composer have changed.
   scroller.scrollTop=followLatest?Math.max(0,scroller.scrollHeight-scroller.clientHeight):oldTop;
  };
 }
 function forgetAttempt(sent){
  try{if(JSON.parse(sessionStorage.getItem(attemptKey)||'null')?.id===sent.id)sessionStorage.removeItem(attemptKey);}catch{}
  if(attempt?.id===sent.id)attempt=null;
 }
 function acknowledge(sent,receipt){
  if(typeof sent.draftText==='string'){
   if(input===sent.draftText)input='';
   try{if(sessionStorage.getItem(draftKey)===sent.draftText)sessionStorage.removeItem(draftKey);}catch{}
  }
  forgetAttempt(sent);sendError='';composerState='';
  deliveryNotice=receipt?.delivery==='received'?'上一条已交给分析。':receipt?.delivery==='unprocessed'?'上一条存了，但这轮结束前没用上。':'上一条存了；用没用上看消息下面的状态。';
 }
 async function submitAttempt(sent){
  if(sending||!sent)return;
  sending=true;sendError='';deliveryNotice='';composerState='';
  if(ctx.isCurrent()){const restoreScroll=preserveScroll();drawComposer(current);restoreScroll();}
  view.querySelectorAll('[data-respond]').forEach(b=>b.disabled=true);
  try{
   // Always replay the original payload and id, even if a newer question is now visible.
   const next=await api.chatMessage(id,{...sent.payload,message_id:sent.id});
   acknowledge(sent,(next.messages||[]).find(e=>e.type==='user'&&e.message_id===sent.id));
   current=next;draw(next,true);
  }catch(e){
   // Polling may already have confirmed receipt while the POST response was lost.
   if(attempt?.id===sent.id){
    if([400,404,409,413,422].includes(e.status))forgetAttempt(sent);
    sendError=e.message;
   }
  }finally{
   sending=false;
   if(ctx.isCurrent()){
    const restoreScroll=preserveScroll();composerState='';if(current)drawComposer(current);
    view.querySelectorAll('[data-respond]').forEach(b=>b.disabled=Boolean(attempt));restoreScroll();
   }
  }
 }
 async function send(action='message',preset){
  if(sending||!current?.alive)return;
  if(attempt){sendError='上一条送到没还没确认；先核对或重发。新写的草稿还在。';composerState='';drawComposer(current);return;}
  const pending=current.pending,text=preset??document.getElementById('chat-input')?.value??input;
  if(!['approve','reject'].includes(action)&&!text.trim()){document.getElementById('chat-input')?.focus();return;}
  if(pending?.kind==='checkpoint'&&action==='message')action=document.getElementById('review-action')?.value||'revise';
  const draftText=preset===undefined&&!['approve','reject'].includes(action)?text:null;
  if(draftText!==null){input=text;save();}
  const sent={id:crypto.randomUUID(),payload:{request_id:pending?.request_id||null,action,text},draftText};
  try{sessionStorage.setItem(attemptKey,JSON.stringify(sent));}
  catch{sendError='存不了发送记录，消息没发出去。请恢复本页存储权限再试。';composerState='';drawComposer(current);return;}
  attempt=sent;await submitAttempt(sent);
 }
 function drawStopState(){
  if(!ctx.isCurrent())return;
  const button=document.getElementById('chat-stop');
  if(button){button.disabled=stopping;button.textContent=stopping?'正在停…':current?.task_result?'结束本次对话':'停掉这轮';}
  const error=document.getElementById('chat-stop-error');if(error)error.textContent=stopError;
 }
 async function stop(){
  if(stopping)return;
  stopping=true;stopError='';drawStopState();
  try{await api.stopChat(id);await refresh();}
  catch(e){stopError='没停成：'+e.message+'。再试一次。';}
  finally{stopping=false;drawStopState();}
 }
 function drawComposer(data){
  const state=data.status+'|'+data.alive+'|'+Boolean(data.task_result)+'|'+(data.pending?.request_id||'')+'|'+(attempt?.id||'')+'|'+sending+'|'+sendError+'|'+deliveryNotice;
  if(state===composerState)return;
  composerState=state;
  const active=document.activeElement,selection=active?.id==='chat-input'?[active.selectionStart,active.selectionEnd]:null;
  if(!data.alive){
   composer('<div class="composer">'+deliveryPanel()+'<div class="composer-static"><span>'+esc(STATUS[data.status]||'本轮已结束')+'。消息与已生成的结果已保留。</span><a class="quiet-button selected" href="#/run">'+icon('plus')+'新对话</a></div></div>');bindDelivery();
   return;
  }
  if(data.pending?.kind==='rewind'){
   composer('<div class="composer">'+deliveryPanel()+'<p class="small-note">请先在上方选择是否重新分析。当前输入草稿会保留。</p></div>');bindDelivery();
   return;
  }
  const review=data.pending?.kind==='checkpoint';
  const hint=data.task_result?'结果已交付。还能问，也能指哪儿要改。':data.pending?.kind==='rewind'?'先在上面选要不要重做。':review?'确认只管上面列的；哪儿不对直接说。':data.pending?'不用按题号，直接说。':'正在分析；现在发的，下一轮算进去。';
  composer('<div class="composer">'+deliveryPanel()+'<label class="visually-hidden" for="chat-input">补充、回答或纠正信息</label><textarea id="chat-input" rows="2" placeholder="'+(review?'哪里需要修改，或还有什么想先问清楚？':'接着说，也可以改前面的…')+'">'+esc(input)+'</textarea><div class="composer-toolbar"><div class="composer-controls">'+(review?'<select id="review-action" aria-label="回应方式"><option value="revise">补充或修改</option><option value="followup">先问清楚</option></select>':'<span class="small-note">随便说</span>')+'</div><button class="send-button" id="chat-send" aria-label="发送消息" '+(sending||attempt?'disabled':'')+'>'+icon('arrow')+'</button></div></div><div class="composer-hint">'+esc(hint)+' <a href="#/guide">看看怎么聊</a></div>');
  bindDelivery();
  document.getElementById('chat-send-error').textContent=sendError;
  const ta=document.getElementById('chat-input');ta.oninput=()=>{input=ta.value;save();};
  bindEnter(ta,()=>send());document.getElementById('chat-send').onclick=()=>send();
  if(selection){ta.focus({preventScroll:true});ta.setSelectionRange(...selection);}
 }
 /** 帧里的缺口可点：把「挂的租金：」补进输入框。只在对话还能说话时可用。 */
 function bindFrameGaps(){
  const panel=document.getElementById('evidence-content');
  if(!panel)return;
  panel.onclick=(event)=>{
   const button=event.target?.closest?.('.frame-gap');
   if(!button||!ctx.isCurrent())return;
   const field=document.getElementById('chat-input');
   if(!field){setStatus('对话已结束，开新对话再补');return;}
   const line=String(button.dataset.fill||'');
   field.value=(field.value.trim()?field.value.replace(/\s+$/,'')+'\n':'')+line;
   input=field.value;save();field.focus();
   field.setSelectionRange?.(field.value.length,field.value.length);
  };
 }

 function deliveryPanel(){
  const error='<p id="chat-send-error" class="send-error" role="alert">'+esc(sendError)+'</p>';
  if(!attempt)return (deliveryNotice?'<p class="small-note" role="status">'+esc(deliveryNotice)+'</p>':'')+error;
  if(sending)return '<p class="small-note" role="status">正在发，稍等。</p>'+error;
  const text=attempt.payload.text||({approve:'确认前面列的',reject:'保留现在结果'}[attempt.payload.action]||'刚才的操作');
  return '<div class="small-note" role="status"><p>上一条送到没，还没确认；不会自动重发。</p><details><summary>待确认的消息</summary><div class="conversation-prose">'+esc(text)+'</div></details><div class="choice-list"><button class="quiet-button" id="retry-message">重发</button><button class="quiet-button" id="check-message">再看状态</button></div></div>'+error;
 }
 function bindDelivery(){
  document.getElementById('retry-message')?.addEventListener('click',()=>submitAttempt(attempt));
  document.getElementById('check-message')?.addEventListener('click',()=>refresh());
 }
 function draw(data,forceLatest=false){
  if(!ctx.isCurrent())return;current=data;
  const restoreScroll=preserveScroll(forceLatest),previousComposer=composerState;
  if(attempt){const receipt=(data.messages||[]).find(e=>e.type==='user'&&e.message_id===attempt.id);if(receipt)acknowledge(attempt,receipt);}
  document.getElementById('crumb').textContent=data.title;
  setStatus(STATUS[data.status]||'');
  const sig=JSON.stringify([data.research,data.business_cost,data.business_costs,data.web_sources,data.progress])+'|'+data.revision+'|'+data.status+'|'+data.alive+'|'+(data.pending?.request_id||'');
  const changed=sig!==signature;
  if(changed){
   signature=sig;
   let html='',latestProgress=data.progress||'正在理解你的问题…';
   for(const e of data.messages||[]){
    if(e.type==='user'){
     html+=userMessage(e.text)+(e.delivery==='queued'?'<p class="delivery-note">存好了，等交给分析</p>':e.delivery==='unprocessed'?'<p class="delivery-note">这条还没过分析</p>':e.delivery==='received'?'<p class="delivery-note">已交给分析</p>':'');
    }else if(e.type==='request')html+=renderRequest(e);
    else if(e.type==='message'){
     if(progressText(e.text)){if(!data.progress)latestProgress=e.text;continue;}
     html+=assistant('<div class="markdown-prose">'+renderMarkdown(e.text)+'</div>');
    }else if(e.type==='error')html+='<p class="inline-error">'+esc(e.text)+'</p>';
   }
   const pending=data.pending;
   let quickChoices=[];
   if(pending?.kind==='question'){
    const groups=(pending.payload.questions||[]).filter(q=>(q.options||[]).length);
    html+='<div class="chat-quick-actions"><div class="choice-groups">'+groups.map(q=>{
     const buttons=q.options.map(s=>{const i=quickChoices.length;quickChoices.push({question:q,option:s});return '<button class="choice-button" data-option="'+i+'">'+esc(s)+'</button>';}).join('');
     return '<div class="choice-group"><p>'+esc(q.text)+'</p><div class="choice-list">'+buttons+'</div></div>';
    }).join('')+'</div><button class="quiet-button" id="not-sure">还不清楚，先按已知的看</button></div>';
   }else if(pending?.kind==='checkpoint')html+='<div class="chat-quick-actions"><button class="primary-button" data-respond="approve">对的，就这样</button><span class="small-note">哪儿不对，在下面直接改。</span></div>';
   else if(pending?.kind==='rewind')html+='<div class="chat-quick-actions"><button class="secondary-button" data-respond="approve">好，重来</button><button class="quiet-button" data-respond="reject">先不重做</button></div>';
   else if(data.alive&&!data.task_result)html+='<div class="analysis-meta conversation-progress" role="status">'+icon('activity')+esc(latestProgress)+'</div>';
   html+=renderTaskResult(data);
   if(!data.alive&&!data.task_result)html+='<div class="conversation-end"><strong>'+esc(STATUS[data.status]||'本轮已结束')+'</strong><p>'+esc(data.status==='completed'?'能看完整报告，对结果和依据。':data.status==='blocked'?'这轮没完成。看最后的说明；已生成的不等于最终结论。':'消息都留着。关了输入后，不会替你确认任何假设。')+'</p>'+(data.case?'<a href="#/case/'+data.case.split('/').map(encodeURIComponent).join('/')+'" class="secondary-button">看生成的分析</a>':'')+(data.error_detail?'<details class="disclosure"><summary>查看具体原因</summary><pre class="term">'+esc(data.error_detail)+'</pre></details>':'')+'</div>';
   view.innerHTML=html;
   view.querySelectorAll('[data-respond]').forEach(b=>{b.disabled=sending||Boolean(attempt);b.onclick=()=>send(b.dataset.respond,'');});
   view.querySelectorAll('[data-option]').forEach(b=>b.onclick=()=>{const choice=quickChoices[Number(b.dataset.option)];if(!choice)return;const ta=document.getElementById('chat-input');input=mergeChoiceDraft(ta?.value??input,choice.question,choice.option);save();if(ta){ta.value=input;ta.focus();}});
   view.querySelector('#not-sure')?.addEventListener('click',()=>send('message','这些信息我目前不清楚。请先按已知信息分析，缺失的内容保留为未取得，不要自行估算。'));
   const users=(data.messages||[]).filter(e=>e.type==='user');
   evidence(renderFrame(data.frame)+renderPoi(data.poi_surveys)+renderResearch(data)+'<div class="evidence-group">'+(data.task_scope?'这次要做啥':'这次聊啥')+'</div>'+sourceItem(TASK_LABEL[data.task_scope]||topicName(data.topic),data.task_scope?'按这次要求整理的；还能补、还能改。':'开新对话时能换主题。','')+
     '<div class="evidence-group">你给的背景和补充</div>'+users.map((u,i)=>sourceItem(i?'补充 '+i:'最开始的问题',u.text,'')).join('')+
     (data.case?'<div class="evidence-group">产出的东西</div><a class="quiet-button" href="#/case/'+data.case.split('/').map(encodeURIComponent).join('/')+'">'+icon('file')+'查看分析记录</a>':'')+
     (data.alive?'<div class="evidence-group">对话控制</div><button class="quiet-button" id="chat-stop">停掉这轮</button><p id="chat-stop-error" class="send-error" role="alert"></p>':''),'看依据');
   document.getElementById('chat-stop')?.addEventListener('click',stop);drawStopState();
   bindFrameGaps();
  }
  drawComposer(data);
  if(changed||previousComposer!==composerState||forceLatest)restoreScroll();
 }
 async function refresh(){
  if(polling||!ctx.isCurrent())return;polling=true;
  try{const data=await api.chat(id);draw(data);if(!data.alive)destroy();}
  catch(e){if(!current)throw e;setStatus('暂时断开，正在重连');}
  finally{polling=false;}
 }
 await refresh();if(ctx.isCurrent()&&current?.alive)timer=startPoll(refresh,1200);
}
