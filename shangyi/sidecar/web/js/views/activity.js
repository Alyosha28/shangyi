import {api,startPoll,stopPoll} from '../api.js';
import {esc,MODE_LABEL} from '../util.js';
import {icon,emptyEvidence,composer,setStatus,toast} from '../ui.js';
import {enterNew} from '../motion.js';
let poll=null;
export function destroy(){if(poll)stopPoll(poll);poll=null;}
const CHAT_STATUS={thinking:'正在分析',waiting:'等你回应',completed:'已完成',partial:'已交付，部分信息未取得',blocked:'停下来了',failed:'没启动成',interrupted:'断开了',stopped:'已停止'};
export function mergeActivity(chats,runs){
 const claimed=new Set(chats.map(c=>c.case).filter(Boolean));
 return [
  ...chats.map(c=>({kind:'chat',id:c.id,title:c.title||'咨询对话',mode:c.mode,alive:c.alive,
   status:CHAT_STATUS[c.status]||'状态不确定',started_at:c.created_at,case:c.case,
   href:'#/chat/'+encodeURIComponent(c.id),detail:'',waiting:c.status==='waiting',awaitingFollowup:Boolean(c.awaiting_followup)})),
  ...runs.filter(r=>!r.case||!claimed.has(r.case)).map(r=>({kind:'run',id:r.run_id,
   title:r.action||'经营分析',mode:r.mode,alive:r.alive,
   status:r.alive?'正在分析':r.exit_code==null?'程序退了':r.exit_code===0?'正常跑完':'被掐断',
   started_at:r.started_at,case:r.case,href:r.case?'#/case/'+r.case.split('/').map(encodeURIComponent).join('/'):'',detail:r.log_tail||'还没日志…'}))
 ].sort((a,b)=>Number(Boolean(b.alive))-Number(Boolean(a.alive))||(b.started_at||0)-(a.started_at||0));
}
export function renderActivity(items,open=new Set(),stopping=new Set()){
 return items.map(r=>{
  const key=r.kind+':'+r.id,stopPending=stopping.has(key);
  return '<section class="answer-section" data-item="'+esc(key)+'"><div class="analysis-meta">'+icon(r.alive&&!r.awaitingFollowup?'activity':r.status==='已完成'||r.status==='正常跑完'?'check':'clock')+esc(r.status)+' · '+esc(MODE_LABEL[r.mode]||'咨询')+'</div><h3>'+esc(r.title)+'</h3><p class="small-note">'+esc(new Date(r.started_at*1000).toLocaleString('zh-CN'))+'</p><div class="choice-list">'+(r.href?'<a class="secondary-button" href="'+r.href+'">'+icon(r.kind==='chat'?'chat':'file')+(r.kind==='chat'?(r.waiting?'继续回应':'打开对话'):'看记录')+'</a>':'<span class="small-note">还没生成记录。</span>')+(r.alive?'<button class="quiet-button" data-stop="'+esc(r.id)+'" data-kind="'+r.kind+'" '+(stopPending?'disabled':'')+'>'+(stopPending?'正在停…':r.awaitingFollowup?'结束本次对话':'停掉这轮')+'</button>':'')+'</div>'+(r.kind==='run'?'<details class="disclosure" data-run="'+esc(key)+'" '+(open.has(key)?'open':'')+'><summary>看跑的详情</summary><div class="term">'+esc(r.detail)+'</div></details>':'')+'</section>';
 }).join('');
}
export async function render(view,ctx){
 destroy();emptyEvidence('等你的对话：答个话或对下前提。打开接着来；停了，消息和已产出的也还在。');composer('');
 view.innerHTML='<h1 class="page-title">进度</h1><p class="page-caption">看走到哪了，回到等你的对话，也能停没完的。</p><div id="runs"><p class="small-note" role="status">正在读状态…</p></div><div id="run-error" role="status"></div>';
 let busy=false;
 const stopping=new Set();
 async function refresh(){
 if(busy||!ctx.isCurrent())return;busy=true;
 try{
 const results=await Promise.allSettled([api.chats(),api.runs()]);if(!ctx.isCurrent())return;
 if(results.every(r=>r.status==='rejected'))throw new Error('连不上本地服务。');
 const items=mergeActivity(results[0].value?.chats||[],results[1].value?.runs||[]);
 const partial=results.some(r=>r.status==='rejected');
 const open=new Set([...view.querySelectorAll('details[open]')].map(x=>x.dataset.run));
 setStatus(items.some(r=>r.waiting)?'有对话等你回应':items.some(r=>r.alive&&!r.awaitingFollowup)?'分析进行中':items.some(r=>r.awaitingFollowup&&r.alive)?'交了，还能聊':'');
 view.querySelector('#run-error').textContent=partial?'有的状态没读到，列表可能不完整；正在重连。':'';
 view.querySelector('#runs').innerHTML=items.length?renderActivity(items,open,stopping):'<div class="blank-state"><strong>'+(partial?'还没状态可看':'现在没有在跑的')+'</strong><p>'+(partial?'有的没读到；连上后自动更新。':'开始后，这儿看进度和要回的事儿。')+'</p><a href="#/run" class="secondary-button">新咨询</a></div>';
 enterNew(view.querySelector('#runs'),'.answer-section');
 view.querySelectorAll('[data-stop]').forEach(b=>b.onclick=async()=>{
  const key=b.dataset.kind+':'+b.dataset.stop;if(stopping.has(key))return;
  stopping.add(key);b.disabled=true;b.textContent='正在停…';
  try{await (b.dataset.kind==='chat'?api.stopChat(b.dataset.stop):api.stopRun(b.dataset.stop));if(ctx.isCurrent())toast('已叫停');}
  catch(e){if(ctx.isCurrent())toast(e.message,true);}
  finally{stopping.delete(key);if(ctx.isCurrent())await refresh();}
 });
 }catch(e){if(ctx.isCurrent()){setStatus('更不动了');view.querySelector('#run-error').textContent='进度更不动，正在重试。'+e.message;}}finally{busy=false;}
 }
 await refresh();if(ctx.isCurrent())poll=startPoll(refresh,2500);
}
