import {api,startPoll,stopPoll} from '../api.js';
import {esc,humanTitle,MODE_LABEL,CONF_LABEL,fmtTime,costScopeNote} from '../util.js';
import {payoffMatrix,equilibriaCards,distributionCard,consequenceCard,discoveryCard,reportCard,timeline,eqCellKeys} from '../components.js';
import {icon,assistant,userMessage,evidence,sourceItem,composer,setStatus,topicName,toast,downloadText} from '../ui.js';
import {renderResearch} from '../research.js';
import {renderFrame,renderPoi} from '../frame.js';
import {renderMarkdown} from '../markdown.js';
import {revealOnScroll} from '../motion.js';
let poll=null,dispose=()=>{};
export function destroy(){dispose();dispose=()=>{};if(poll)stopPoll(poll);poll=null;}

export function collectSources(data){
 const sources=[],started=new Map();
 for(const e of data.events||[]){
  if(e.type==='tool_call_started')started.set(e.call_id,e);
  if(e.type!=='tool_call_finished'||e.outcome?.status!=='ok')continue;
  const start=started.get(e.call_id),name=e.name||start?.name;
  if(!['web_search','web_fetch','kb_retrieve'].includes(name))continue;
  let args={};try{args=JSON.parse(start?.arguments||'{}');}catch{}
  const output=e.outcome.output||'';
  const title=name==='kb_retrieve'?(output.match(/^#\s+(.+)/m)?.[1]||args.page||'理论资料'):args.query||args.url||'公开资料检索';
  sources.push({title,description:name==='kb_retrieve'?'理论资料 · 不作为经营事实':name==='web_fetch'?'网页读取记录':'公开检索记录',detail:output,kind:name==='kb_retrieve'?'file':'globe'});
 }
 const unique=new Map();for(const s of sources)unique.set(s.description+s.title,s);
 return [...unique.values()];
}
function sourceRecords(sourceList,filter){
 const matches=sourceList.filter(s=>(s.title+' '+s.description+' '+s.detail).toLowerCase().includes(filter.toLowerCase()));
 return '<div class="evidence-group">查过的资料 · '+matches.length+'</div>'+
  (matches.map(s=>sourceItem(s.title,s.description,s.detail,s.kind)).join('')||'<p class="small-note">'+(filter?'没匹配上的资料。':'这次没留下检索资料。')+'</p>');
}
const TASK_SCOPE={research:'资料整理',cost:'费用整理',research_and_cost:'资料和费用',consultation:'咨询'};
const taskLabel=result=>TASK_SCOPE[result?.scope]||'这次做的事';
const taskStatus=result=>result?.status==='partial'?'有结果，还缺信息':result?.status==='complete'?'本次整理已完成':'没写完成没完成';
function scopedResults(data){return Array.isArray(data.artifacts?.task_results)?data.artifacts.task_results:[];}
export function renderTaskResults(data){
 const results=scopedResults(data),pending=data.artifacts?.latest_task?.status==='processing';
 let html=pending?'<h2>新要求还没出结果</h2><p class="small-note">下面是之前已完成的整理；还在不在跑，回对话看。</p>':'';
 if(results.length)html+='<p class="small-note">按每次要求存档；不代表已完成方案比较。</p>';
 return html+results.map((result,index)=>({result,index})).reverse().map(({result,index})=>{
  const missing=Array.isArray(result.missing)?result.missing:[];
  const summary=typeof result.summary==='string'&&result.summary.trim()?renderMarkdown(result.summary):'<p>这条没留下结果正文。</p>';
  const costNote=result.scope==='cost'||result.scope==='research_and_cost'?costScopeNote(result):'';
  return '<section class="answer-section"><h2>'+esc(taskLabel(result))+(results.length>1?' · 第 '+(index+1)+' 次':'')+'</h2><p class="small-note">'+esc(taskStatus(result))+(result.at?' · '+esc(fmtTime(String(result.at))):'')+'</p><div class="markdown-prose">'+summary+'</div>'+
   (result.status==='partial'||missing.length?'<div class="notice"><strong>仍缺少的信息</strong>'+(missing.length?'<ul>'+missing.map(item=>'<li>'+esc(item)+'</li>').join('')+'</ul>':'<p>本次记录标记为部分完成，但未列出具体缺口；请结合原对话核对。</p>')+'</div>':'')+
   (costNote?'<p class="small-note">'+esc(costNote)+'</p>':'')+'</section>';
 }).join('');
}
export function caseCopyText(data){
 const a=data.artifacts||{},rec=a.recommendation,results=scopedResults(data),parts=[];
 const question=typeof data.question==='string'?data.question.trim():'';if(question)parts.push(question);
 if(results.length){
  if(a.latest_task?.status==='processing')parts.push('新要求还没出结果；以下是之前保存的整理。');
  for(const [index,result] of results.entries()){
   const content=typeof result.summary==='string'?result.summary.trim():'';
   const missing=Array.isArray(result.missing)?result.missing:[];
   if(!content&&!missing.length)continue;
   parts.push(taskLabel(result)+' · 第 '+(index+1)+' 次\n'+taskStatus(result)+'\n\n'+content+
    (result.status==='partial'||missing.length?'\n\n仍缺少的信息\n'+(missing.length?missing.map(item=>'- '+item).join('\n'):'没写具体缺啥，对着原对话看。'):''));
  }
 }
 if(rec?.headline||rec?.actions?.length){
  parts.push([rec.headline,...(rec.actions||[]).map((x,i)=>(i+1)+'. '+x.action+'\n'+(x.rationale||'')+'\n当时把握：'+(CONF_LABEL[x.confidence]||x.confidence||'未注明')),
   rec.do_not?.length?'需要避开的选择\n'+rec.do_not.join('\n'):'',
   rec.caveats?.length?'适用条件与限制\n'+rec.caveats.map(x=>typeof x==='string'?x:JSON.stringify(x)).join('\n'):''].filter(Boolean).join('\n\n'));
 }
 return parts.length>(question?1:0)?parts.join('\n\n'):'';
}
/** 给人看的导出：Markdown。结果、表格、适用范围、来源都在一份文件里。
 *
 *  UI-015：原来「保存记录」只有原始 JSON（86 KB，header/events/artifacts 套着结构），
 *  用户想之后阅读、打印或分享时根本用不了。这里先出一份能直接读的，
 *  原始 JSON 降级成第二个、标注清楚用途的按钮——**两者都保留**，
 *  因为审计与复查仍然需要原始依据。
 */
export function caseExportMarkdown(data){
 const a=data.artifacts||{},header=data.header||{},rec=a.recommendation,spec=a.gamespec?.spec||a.gamespec;
 const taskResults=scopedResults(data);
 const lines=[];
 const question=typeof data.question==='string'?data.question.trim():'';
 lines.push('# '+(question||(header.playbook_id||'咨询')+'记录'));
 const meta=[];
 if(header.created_at)meta.push('时间：'+String(header.created_at).slice(0,10));
 if(header.mode)meta.push('分析路径：'+(MODE_LABEL[header.mode]||header.mode));
 if(header.playbook_id)meta.push('主题：'+topicName(header.playbook_id));
 if(meta.length)lines.push(meta.join(' · '));
 if(question)lines.push('## 我当时的问法','',question);

 // 每次要求的结果
 if(taskResults.length){
  lines.push('## 结果','');
  if(a.latest_task?.status==='processing')lines.push('> 追加的要求还没出结果；下面是之前保存的整理。','');
  for(const [index,result] of taskResults.entries()){
   const content=typeof result.summary==='string'?result.summary.trim():'';
   lines.push('### '+taskLabel(result)+(taskResults.length>1?' · 第 '+(index+1)+' 次':''),
    '','状态：'+taskStatus(result)+(result.at?'（'+String(result.at)+'）':''),'');
   if(content)lines.push(content,'');
   const missing=Array.isArray(result.missing)?result.missing:[];
   if(result.status==='partial'||missing.length){
    lines.push('**仍缺少的信息**','');
    if(missing.length)lines.push(...missing.map(item=>'- '+item));
    else lines.push('- 本次记录标记为部分完成，但没写具体缺什么；请对着原对话核对。');
    lines.push('');
   }
  }
 }

 // 建议
 if(rec?.headline||rec?.actions?.length){
  lines.push('## 建议','');
  if(rec.headline)lines.push(rec.headline,'');
  for(const [index,action] of (rec.actions||[]).entries()){
   lines.push('**'+(index+1)+'. '+action.action+'**',
    '当时把握：'+(CONF_LABEL[action.confidence]||action.confidence||'未注明'));
   if(action.rationale)lines.push('',action.rationale);
   lines.push('');
  }
  if(rec.do_not?.length)lines.push('### 需要避开的选择','',...rec.do_not.map(x=>'- '+x),'');
  if(rec.caveats?.length)lines.push('### 适用条件与限制','',...rec.caveats.map(x=>'- '+(typeof x==='string'?x:JSON.stringify(x))),'');
 }

 // 表格：收益矩阵 / 均衡 / 分布 / 条件后果 / 达标做法
 const tableSections=[];
 if(spec?.players&&spec?.strategies&&spec?.payoffs?.values?.cells)tableSections.push(['好坏对照',payoffMarkdown(spec)]);
 if(a.equilibria)tableSections.push(['稳的做法',equilibriaMarkdown(a.equilibria)]);
 if(a.distribution)tableSections.push(['结果分布',distributionMarkdown(a.distribution)]);
 if(a.consequence)tableSections.push(['做了会怎样',consequenceMarkdown(a.consequence)]);
 if(a.discovery)tableSections.push(['达标做法',discoveryMarkdown(a.discovery)]);
 for(const [title,body] of tableSections){
  if(!body)continue;
  lines.push('## '+title,'',body,'');
 }

 // 来源与依据
 const sourceList=collectSources(data);
 const records=(a.research?.records||[]).map(r=>r&&typeof r==='object'?r:{}).filter(Boolean);
 const webSources=(a.web_sources||[]).filter(Boolean);
 const params=Object.values(spec?.params||{});
 const ledger=a.report?.report?.ledger||a.report?.ledger;
 const hasSources=sourceList.length||records.length||webSources.length||params.length||ledger;
 if(hasSources){
  lines.push('## 来源与怎么算的','');
  if(sourceList.length){
   lines.push('**用过的资料**','');
   for(const item of sourceList)lines.push('- '+item.title+(item.detail?'（'+item.detail+'）':''));
   lines.push('');
  }
  if(records.length||webSources.length){
   lines.push('**登记过的原文字段**','');
   for(const r of records){
    const fields=Object.entries(r).filter(([,v])=>v!=null&&v!=='').map(([k,v])=>k+'：'+v).join('；');
    if(fields)lines.push('- '+fields);
   }
   for(const item of webSources){
    if(typeof item==='string')lines.push('- '+item);
    else if(item?.url||item?.title)lines.push('- '+(item.title||item.url)+(item.url&&item.title?' <'+item.url+'>':''));
   }
   lines.push('');
  }
  if(params.length){
   lines.push('**关键数字**','');
   for(const p of params)lines.push('- '+(p.name||'参数')+'：'+String(p.value?.value??p.value??'未提供')+(p.unit?' '+p.unit:''));
   lines.push('');
  }
  if(ledger)lines.push('**计算记录**：用了什么工具、哪个版本见原始 JSON 的 `artifacts.report.ledger`。','');
 }

 lines.push('---','','这份文件由商奕在导出时生成；其中的判断与数字来自本次咨询的记录，',
  '适用范围与限制见上。原始事件与结构数据可用同一页的「保存原始数据」导出。','');
 return lines.join('\n');
}

/** 收益矩阵转 Markdown 表：行列按 players 数组顺序，不按 JSON 键排序。 */
function payoffMarkdown(spec){
 const players=spec.players||[],strats=spec.strategies||{},cells=spec.payoffs?.values?.cells||{};
 const me=players[0]?.id,them=players[1]?.id;
 if(!me||!them||!strats[me]||!strats[them])return '';
 const nameOf=id=>players.find(p=>p.id===id)?.name||id;
 const rows=[];
 rows.push('| '+nameOf(me)+' \\ '+nameOf(them)+' | '+strats[them].map(x=>'**'+x+'**').join(' | ')+' |');
 rows.push('| --- |'+' --- |'.repeat(strats[them].length));
 for(const a of strats[me]){
  const cellsRow=strats[them].map(b=>{
   const cell=cells[a+','+b]||cells[b+','+a];
   if(!cell)return '未取得';
   const mine=cell[me],theirs=cell[them];
   const fmt=v=>v&&typeof v==='object'&&'value'in v?v.value:v;
   return fmt(mine)+' / '+fmt(theirs);
  });
  rows.push('| **'+a+'** | '+cellsRow.join(' | ')+' |');
 }
 return rows.join('\n');
}

function equilibriaMarkdown(equilibria){
 const list=Array.isArray(equilibria)?equilibria:(equilibria?.equilibria||[]);
 if(!list.length)return '';
 const kept=Array.isArray(equilibria?.refinement?.survivors)?equilibria.refinement.survivors:null;
 return list.map(eq=>'- '+(eq.kind||'')+(kept?('（'+(kept.includes(eq.id)?'通过筛选':'未通过筛选')+'）'):'')+
  (eq.support?.length?'：'+eq.support.map((s,side)=>Array.isArray(s)?('['+side+'] '+s.join('/')):s).join(' × '):'')).join('\n');
}

function distributionMarkdown(distribution){
 const dist=distribution?.distribution||distribution;
 if(!Array.isArray(dist)||!dist.length)return '';
 return dist.map(d=>'- '+(d.label||d.id||JSON.stringify(d))+(d.probability!=null?'：'+d.probability:'')).join('\n');
}

function consequenceMarkdown(consequence){
 const c=consequence?.consequence||consequence;
 if(!c||typeof c!=='object')return '';
 const out=[];
 for(const [key,value] of Object.entries(c)){
  if(value==null)continue;
  out.push('- '+key+'：'+(typeof value==='object'?JSON.stringify(value):String(value)));
 }
 return out.join('\n');
}

function discoveryMarkdown(discovery){
 const d=discovery?.discovery||discovery;
 const list=d?.satisficing||d?.strategies||(Array.isArray(d)?d:[]);
 if(!Array.isArray(list)||!list.length)return '';
 return list.map(x=>'- '+(typeof x==='string'?x:JSON.stringify(x))).join('\n');
}

export async function render(view,ctx){
 destroy();let latest=null,lastSignature='',filter='',busy=false,linkedChat=null,chatLookupFailed=false;
 // 滚动揭示只做本次访问的第一次：draw 在数据变化时会重绘，进行中的咨询
 // 每次轮询都换新节点，重复揭示会让整页反复闪。放在 render 的局部作用域，
 // 因为 render() 自己会先调 destroy()，模块级标志会被每次进入重置。
 let revealed=false;
 let active=true;dispose=()=>active=false;const isCurrent=()=>active&&ctx.isCurrent();
 const date=decodeURIComponent(ctx.match[1]),id=decodeURIComponent(ctx.match[2]);
 function renderFooter(data){
  const topic=data.header?.playbook_id,newHref=topic?'#/run/'+encodeURIComponent(topic):'#/run';
  composer('<div class="composer"><div class="composer-static"><span>'+ (linkedChat?'这是存档。可回到对话看消息和现状。':chatLookupFailed?'对话列表没读到；记录还能看、能存。':'只能看。可从左边开对话，或开新的。')+'</span>'+
   (linkedChat?'<a class="quiet-button selected" href="#/chat/'+encodeURIComponent(linkedChat.id)+'">'+icon('chat')+'回到对话</a>':'')+
   '<a class="quiet-button" href="'+newHref+'">'+icon('plus')+'再问类似的</a></div></div>');
 }
 function renderSources(data){
  const sourceList=collectSources(data),spec=data.artifacts?.gamespec?.spec||data.artifacts?.gamespec||{};
  const params=Object.values(spec.params||{}),ledger=data.artifacts?.report?.report?.ledger||data.artifacts?.report?.ledger;
  const structured=renderResearch({research:data.artifacts?.research?.records||[],business_costs:data.artifacts?.business_costs,business_cost:data.artifacts?.business_cost,web_sources:data.artifacts?.web_sources});
  evidence(renderFrame(data.frame)+renderPoi((data.artifacts||{}).poi_surveys)+'<div id="case-structured-evidence">'+structured+'</div>'+
   '<label class="visually-hidden" for="source-search">筛选检索资料来源</label><input class="search-field" id="source-search" placeholder="筛选检索记录…" value="'+esc(filter)+'"><div id="source-records">'+sourceRecords(sourceList,filter)+'</div>'+
   '<div class="evidence-group">关键数字 · '+params.length+'</div>'+params.map(p=>sourceItem(p.name||'参数',String(p.value?.value??p.value??'未提供')+(p.unit?' '+p.unit:''),p)).join('')+
   (ledger?'<div class="evidence-group">怎么算的</div>'+sourceItem('计算记录','用了什么工具、哪个版本',ledger):''));
  const search=document.getElementById('source-search');
  search.oninput=e=>{
   filter=e.target.value;
   // Keep the focused search node and caret while replacing only the result list.
   document.getElementById('source-records').innerHTML=sourceRecords(sourceList,filter);
  };
 }
 function draw(data){
  latest=data;
  const a=data.artifacts||{},header=data.header||{},rec=a.recommendation,spec=a.gamespec?.spec||a.gamespec;
  const done=data.stats?.phases?.includes('archived'),tasks=scopedResults(data),hasTasks=tasks.length>0||a.latest_task?.status==='processing';
  const lastTask=tasks.at(-1),question=humanTitle(data.question)||(hasTasks?taskLabel(lastTask)+'记录':topicName(header.playbook_id)+'咨询');
  document.getElementById('crumb').textContent=question;document.title=question+' · 商奕';
  setStatus(done?'以前的咨询':a.latest_task?.status==='processing'?'追加结果待完成':lastTask?taskStatus(lastTask):'未完成');
  const names={},strats={};
  for(const p of spec?.players||[]){names[p.id]=p.name||p.id;if((p.relation_to_game||'player')==='player'&&spec.strategies?.[p.id])strats[p.id]=spec.strategies[p.id];}
  const blocks=[];
  if(hasTasks)blocks.push(renderTaskResults(data));
  if(rec?.headline)blocks.push('<h2>'+esc(rec.headline)+'</h2>');
  else if(!hasTasks)blocks.push('<h2>'+(done?'本次没有生成建议':'分析尚未完成')+'</h2><p class="small-note">下面是已产出的部分；没完成不代表还在跑，去“进度”看。</p>');
  if(rec?.actions?.length)blocks.push(rec.actions.map((action,i)=>'<section class="answer-section"><h3>'+(i+1)+'. '+esc(action.action)+'<span class="confidence">当时把握：'+esc(CONF_LABEL[action.confidence]||action.confidence||'没写')+'</span></h3><p>'+esc(action.rationale||'')+'</p></section>').join(''));
  if(rec?.do_not?.length)blocks.push('<h3>避开这些</h3><ul>'+rec.do_not.map(x=>'<li>'+esc(x)+'</li>').join('')+'</ul>');
  if(rec?.caveats?.length)blocks.push('<h3>适用范围</h3><ul>'+rec.caveats.map(x=>'<li>'+esc(typeof x==='string'?x:JSON.stringify(x))+'</li>').join('')+'</ul>');
  const sections=[];
  if(spec?.players&&spec?.strategies)sections.push(['payoffs','好坏对照',payoffMatrix(spec,{equilibriumCells:a.equilibria?eqCellKeys(a.equilibria,strats):null})]);
  if(a.equilibria)sections.push(['equilibria','稳的做法',equilibriaCards(a.equilibria,strats,names)]);
  if(a.distribution)sections.push(['distribution','结果分布',distributionCard(a.distribution)]);
  if(a.consequence)sections.push(['consequence','做了会怎样',consequenceCard(a.consequence)]);
  if(a.discovery)sections.push(['discovery','达标做法',discoveryCard(a.discovery)]);
  if(a.report)sections.push(['report','完整报告',reportCard(a.report)]);
  const open=new Set([...view.querySelectorAll('details[open]')].map(x=>x.dataset.section));
  const detailHtml=sections.map(([key,title,body])=>'<details class="disclosure" data-section="'+key+'" '+(open.has(key)?'open':'')+'><summary>'+esc(title)+'</summary><div>'+body+'</div></details>').join('');
  const copyText=caseCopyText(data);
  const originalQuestion=typeof data.question==='string'?data.question.trim():'';
  view.innerHTML=userMessage(originalQuestion||question)+(data.question_source==='web_conversation_state'?'<p class="small-note">以上为这次对话的最初输入。</p>':'')+assistant('<div class="analysis-meta">'+icon('file')+esc(hasTasks?taskLabel(lastTask):MODE_LABEL[header.mode]||'咨询')+(hasTasks?'':'<span>·</span>'+esc(topicName(header.playbook_id)))+'<span>·</span>'+esc((header.created_at||'').slice(0,10))+'</div>'+blocks.join('')+
   '<div class="message-tools"><button class="quiet-button" id="copy-analysis" '+(copyText?'':'disabled title="尚未有可复制的结果"')+'>'+icon('copy')+(hasTasks?'复制结果':'复制建议')+'</button><button class="quiet-button" id="export-readable">'+icon('download')+'保存可读记录</button><button class="quiet-button" id="export-raw">'+icon('download')+'保存原始数据</button></div>'+
   '<p class="small-note">「保存可读记录」下载 Markdown 文件：结果、表格、适用范围和来源都能直接读、能打印。「保存原始数据」下载完整 JSON（事件与结构数据），给需要核对过程的人用。</p>'+
   '<p class="small-note">'+(hasTasks?'按每次要求记录；来源和怎么算的看右侧依据。':'按当时记录呈现；假设和来源看右侧依据。')+'</p>'+detailHtml+
   '<details class="disclosure" data-section="timeline" '+(open.has('timeline')?'open':'')+'><summary>查看分析过程（'+(data.stats?.n_events||0)+' 条记录）</summary><div>'+timeline(data.events,{showToken:false})+'</div></details>');
  renderSources(data);
  if(!revealed){revealed=true;revealOnScroll(view,'.answer-section,details.disclosure');}
  renderFooter(data);
  view.querySelector('#copy-analysis').onclick=async()=>{if(!copyText)return;try{await navigator.clipboard.writeText(copyText);toast(hasTasks?'结果已复制':'建议已复制');}catch{toast('浏览器不让复制；可以用“保存记录”。',true);}};
  view.querySelector('#export-readable').onclick=()=>{
   const text=caseExportMarkdown(latest);
   if(!text.trim()){toast('还没有可导出的内容。',true);return;}
   downloadText(id+'.md',text);toast('已导出可读记录（Markdown）；原始数据在另一个按钮。');
  };
  view.querySelector('#export-raw').onclick=()=>{
   downloadText(id+'.json',JSON.stringify(latest,null,2));toast('已导出原始数据（JSON，含全部事件与结构）');
  };
 }
 async function refresh(){
  if(busy||!isCurrent())return;busy=true;
  try{
   const data=await api.case(date,id);if(!isCurrent())return;
   const sig=String(data.stats?.n_events)+'|'+String(data.stats?.last_at);
   if(sig!==lastSignature){draw(data);lastSignature=sig;}
   if(data.stats?.phases?.includes('archived')&&poll){stopPoll(poll);poll=null;}
  }finally{busy=false;}
 }
 await refresh();
 if(!isCurrent())return;
 api.chats().then(data=>{
  if(!isCurrent())return;
  const matches=(data.chats||[]).filter(c=>c.case===date+'/'+id&&/^chat-[a-f0-9]{24}$/.test(c.id));
  if(matches.length===1){linkedChat=matches[0];if(latest)renderFooter(latest);}
 }).catch(()=>{
  if(!isCurrent())return;
  chatLookupFailed=true;if(latest)renderFooter(latest);
 });
 if(isCurrent()&&!latest?.stats?.phases?.includes('archived'))poll=startPoll(refresh,4000);
}
