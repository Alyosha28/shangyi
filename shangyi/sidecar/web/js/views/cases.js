import {api} from '../api.js';
import {esc,humanTitle,fmtAgo,MODE_LABEL,TASK_LABEL,TASK_FILTERS,taskKindOf} from '../util.js';
import {caseHref,topicName,emptyEvidence,composer,icon} from '../ui.js';
import {staggerList} from '../motion.js';
const CHAT_STATUS={thinking:'正在分析',waiting:'等你回应',completed:'已完成',partial:'已交付，部分信息未取得',blocked:'停下来了',failed:'没启动成',interrupted:'断开了',stopped:'已停止'};
/** 按用户所在时区格式化成 YYYY-MM-DD。
 *
 * 案例目录按 **UTC** 分片（run.rs 里写明了这是刻意的：UTC 无歧义，跨时区协作时
 * 不会出现"同一份日志被归到两个日期"）。副作用是中国用户晚上 8 点（CST）之后
 * 跑的案例落在前一天的目录里。用户按"今天刚问过"去找会找不到——这正是 UI-016。
 * 故展示与搜索一律换算成本地日期，而案例路径本身只当**标识**用，不做任何改动。
 */
function localDate(instant){
 const date=instant instanceof Date?instant:new Date(instant);
 if(Number.isNaN(date.getTime()))return '';
 return [date.getFullYear(),String(date.getMonth()+1).padStart(2,'0'),String(date.getDate()).padStart(2,'0')].join('-');
}
/** case_id 里带 UTC 的日期与时分秒（case-20260919-scenario-041555）。
 *  .header 的 created_at 缺失时，这是唯一还能推出创建时刻的地方。 */
function caseIdInstant(caseId){
 const m=String(caseId||'').match(/^case-(\d{4})(\d{2})(\d{2})-[a-z]+-(\d{2})(\d{2})(\d{2})/);
 if(!m)return null;
 return new Date(Date.UTC(+m[1],+m[2]-1,+m[3],+m[4],+m[5],+m[6]));
}
/** 创建时刻：优先 header 的 created_at（UTC ISO），再退回 case_id 里的时间。 */
function createdInstant(record){
 if(record?.created_at){const d=new Date(record.created_at);if(!Number.isNaN(d.getTime()))return d;}
 return caseIdInstant(record?.case_id);
}
function chatDate(chat){
 // 一致性口径：一律用**创建时刻**换算本地日期；案例路径（UTC 分片）只作标识。
 if(chat.created_at)return localDate(new Date(chat.created_at*1000));
 const instant=caseIdInstant(String(chat.case||'').split('/')[1]);
 if(instant)return localDate(instant);
 return chat.case?chat.case.split('/')[0]:'';
}
/** 兜底标题里的时间戳：一律换算成**用户本地**时间（MM-DD HH:mm）。
 *  case_id 与 created_at 都是 UTC；直接拿字符串切片会显示成 UTC 时刻，
 *  与列表里的本地日期差一天——那正是 UI-016 的一部分。 */
function localStamp(record){
 const instant=createdInstant(record);
 if(!instant)return String(record?.date||'').slice(5);
 const pad=n=>String(n).padStart(2,'0');
 return pad(instant.getMonth()+1)+'-'+pad(instant.getDate())+' '+pad(instant.getHours())+':'+pad(instant.getMinutes());
}
/** 抽不到问题原文时的兜底标题：话题 + 时间（纯事实，不编内容）。 */
function fallbackTitle(c){
 const topic=topicName(c.playbook_id);
 const time=localStamp(c);
 return time?topic+' · '+time:topic;
}
function modelDate(unix){
 if(!unix)return '';
 const d=new Date(unix*1000);
 return [d.getFullYear(),String(d.getMonth()+1).padStart(2,'0'),String(d.getDate()).padStart(2,'0')].join('-');
}
export function mergeConsultations(cases,chats,models=[]){
 const linked=new Map(cases.map(c=>[c.date+'/'+c.case_id,c]));
 const claimed=new Set(chats.map(c=>c.case).filter(Boolean));
 return [
  ...chats.map(c=>{
   const record=linked.get(c.case);
   return {id:c.id,href:'#/chat/'+encodeURIComponent(c.id),title:humanTitle(c.title)||'经营咨询',
    search:[c.initial,c.title,record?.question,c.id,record?.case_id,
     // 案例目录是 UTC 分片；用户可能照目录日期搜，故两种口径都可搜
     c.case?c.case.split('/')[0]:''].filter(Boolean).join(' '),
    topic:c.topic||record?.playbook_id,mode:c.mode||record?.mode,
    // 列表按「这次做了什么」归类，不按内部分析模式（UI-014）
    kind:taskKindOf(c),date:chatDate(c),
    mtime:Math.max(c.created_at||0,record?.mtime||0),status:CHAT_STATUS[c.status]||'对话记录'};
  }),
  ...cases.filter(c=>!claimed.has(c.date+'/'+c.case_id)).map(c=>({id:c.case_id,href:caseHref(c),
   title:humanTitle(c.question)||fallbackTitle(c),
   search:[c.question,c.case_id,localDate(createdInstant(c))||c.date,c.date].join(' '),
   topic:c.playbook_id,mode:c.mode,kind:taskKindOf(c),
   // 展示用本地日期；c.date 是 UTC 分片名（标识），只留作可搜索的另一口径
   date:localDate(createdInstant(c))||c.date,datePath:c.date,
   mtime:c.mtime||0,status:'分析记录'}))
 ,
  ...models.map(m=>({id:m.id,href:'#/playground/'+encodeURIComponent(m.id),
   title:humanTitle(m.title)||'数字摆局',search:[m.title,m.id].join(' '),
   topic:'数字摆局',mode:'',date:modelDate(m.created_at),mtime:m.created_at||0,
   status:'数字摆局',kind:'model'}))
 ].sort((a,b)=>b.mtime-a.mtime);
}
export function filterConsultations(records,kind,query){
 const needle=query.trim().toLowerCase();
 return records.filter(c=>(kind==='all'||c.kind===kind)&&[c.search,c.title,topicName(c.topic),c.date].join(' ').toLowerCase().includes(needle));
}
// ── 视图状态：从详情返回列表时，搜索、筛选与滚动位置都不该丢（UI-005）──────
// 原来 render() 每次从空 filters 起步，从详情返回列表后用户得重新打字、重新筛、
// 重新滚——尤其是"翻到很下面才点开一条"的场景。这里按仓库已有的草稿范式存进
// sessionStorage（同标签页生效），离开列表时由 app.js 的 destroy() 调用来存滚动位置。
//
// 为什么不给 #view 挂 scroll 监听：#view 是跨路由共用的滚动容器，挂了监听就会把
// 其它页面的滚动也记到这个键里。离开时存一次，足够覆盖"返回列表"这个场景。

const VIEW_KEY = 'shangyi-cases-view-v1';

function readViewState() {
  try {
    const value = JSON.parse(sessionStorage.getItem(VIEW_KEY) || 'null');
    return value && typeof value === 'object' ? value : null;
  } catch {
    return null;
  }
}

function writeViewState(patch) {
  try {
    sessionStorage.setItem(VIEW_KEY, JSON.stringify({ ...(readViewState() || {}), ...patch }));
  } catch {
    // 存不进去不影响本次浏览：搜索与筛选还在页面上，只是返回时不恢复
  }
}

/** 离开列表：记下滚动位置。由 app.js 在切换路由前调用。 */
export function destroy() {
  const scroller = document.getElementById('view');
  if (scroller) writeViewState({ scrollTop: scroller.scrollTop });
}

/** 恢复出来的筛选值要校验：存的是旧 id 时退回"全部"，不硬套。
 *  旧版存的是分析模式 id（scenario/whatif/...），与新口径不通用，
 *  所以**故意不认旧值**——认了就会把「费用整理」筛成「试做法」。 */
export function restoreKind(value) {
  return TASK_FILTERS.some(([id]) => id === value) ? value : 'all';
}

/** 解出本次要用的 mode/query：**显式传入的 filters 优先**，
 *  其次才是上次留在本标签页的状态。显式传空串也算数——
 *  "重新加载"按钮传的就是当前页面的值，不该被旧状态盖掉。 */
export function resolveViewState(filters = {}, saved = null) {
  return {
    kind: filters.kind || restoreKind(saved?.kind),
    query:
      typeof filters.query === 'string'
        ? filters.query
        : typeof saved?.query === 'string'
          ? saved.query
          : '',
  };
}

export async function render(view,ctx,filters={}){
 const results=await Promise.allSettled([api.cases(),api.chats(),api.models()]);if(!ctx.isCurrent())return;
 if(results.every(r=>r.status==='rejected'))throw new Error('记录没读到，稍后再试。');
 const saved=readViewState();
 const cases=mergeConsultations(results[0].value?.cases||[],results[1].value?.chats||[],results[2].value?.models||[]);
 const view_state=resolveViewState(filters,saved);
 let kind=view_state.kind,query=view_state.query;
 const partial=results.some(r=>r.status==='rejected');
 emptyEvidence('打开一条：没聊完的能接着聊，聊完的看分析和依据。');
 composer('');
 view.innerHTML='<h1 class="page-title">查找咨询</h1><p class="page-caption">在聊的、聊完的、没聊完的，都能在这儿找回。</p>'+(partial?'<p class="inline-error" role="status">有的记录没读到，列表可能不完整。<button class="quiet-button" id="retry-records">重新加载</button></p>':'')+'<label class="visually-hidden" for="case-search">搜索咨询</label><input class="search-field" id="case-search" placeholder="搜索问题、主题或日期…" value="'+esc(query)+'"><div class="filter-bar" aria-label="按想怎样筛选">'+TASK_FILTERS.map(([k,l])=>'<button data-mode="'+k+'" aria-pressed="'+(k===kind)+'">'+esc(l)+'</button>').join('')+'</div><div id="case-results"></div><p class="small-note" id="result-count" role="status"></p>';
 let firstRows=true;
 const renderRows=()=>{
 const selected=filterConsultations(cases,kind,query);
 view.querySelector('#case-results').innerHTML=selected.length?selected.map(c=>'<a class="case-list-row" href="'+c.href+'"><div class="case-title">'+esc(c.title)+'</div><div class="case-meta"><span>'+esc(c.status)+'</span><span>'+esc(topicName(c.topic))+'</span><span>'+esc(TASK_LABEL[c.kind]||MODE_LABEL[c.mode]||'咨询')+'</span><span>'+esc(c.date)+'</span><span>'+fmtAgo(c.mtime)+'</span></div></a>').join(''):'<div class="blank-state"><strong>'+(cases.length?'没有找到匹配的咨询':partial?'暂时没有能显示的':'还没有咨询记录')+'</strong><p>'+(cases.length?'换个短词，或选“全部”试试。':partial?'有的没读到；点上面“重新加载”。':'从一个你正烦的事儿开始。')+'</p><a class="secondary-button" href="#/run">'+icon('plus')+'新咨询</a></div>';
 view.querySelector('#result-count').textContent=selected.length?'共 '+selected.length+' 条咨询':'';
  // 只在首屏错峰入场：搜索与筛选会反复重绘整张列表，重复入场会一直闪。
  if(firstRows){firstRows=false;staggerList(view,'.case-list-row');}
 };
 view.querySelector('#case-search').oninput=e=>{query=e.target.value;writeViewState({query});renderRows();};
 view.querySelectorAll('[data-mode]').forEach(b=>b.onclick=()=>{kind=b.dataset.mode;writeViewState({kind});view.querySelectorAll('[data-mode]').forEach(x=>x.setAttribute('aria-pressed',String(x===b)));renderRows();});
 view.querySelector('#retry-records')?.addEventListener('click',async e=>{
  const button=e.currentTarget;button.disabled=true;
  try{await render(view,ctx,{kind,query});}catch{if(ctx.isCurrent()){button.disabled=false;button.textContent='连接未恢复，再试一次';}}
 });
 renderRows();
  // 滚回上次看到的位置（从详情返回时）；没有记录就留在顶部
 const top=Number(saved?.scrollTop);
 const scroller=document.getElementById('view');
  if(scroller&&Number.isFinite(top))scroller.scrollTop=Math.max(0,Math.min(top,scroller.scrollHeight));
}
