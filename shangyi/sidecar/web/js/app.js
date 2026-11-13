import {api,stopAllPolls} from './api.js';
import {esc,humanTitle} from './util.js';
import {icon,composer,emptyEvidence,setStatus} from './ui.js';
import {enterView,holdView} from './motion.js';
import * as run from './views/run.js';
import * as cases from './views/cases.js';
import * as detail from './views/case.js';
import * as playground from './views/playground.js';
import * as activity from './views/activity.js';
import * as chat from './views/chat.js';
import * as guide from './views/guide.js';

const host=document.getElementById('view');
document.querySelector('.skip-link').onclick=e=>{e.preventDefault();host.focus();};
let current=null,generation=0,lastHash=null,guideReturn='#/run';
const routes=[
 {re:/^#\/guide$/,nav:'guide',title:'使用引导',mod:guide},
 {re:/^#\/chat\/(chat-[a-f0-9]{24})$/,nav:'chat',title:'对话',mod:chat},
 {re:/^#\/(?:run|overview)(?:\/([a-z_]+))?$/,nav:'run',title:'新咨询',mod:run},
 {re:/^#\/cases$/,nav:'cases',title:'查找咨询',mod:cases},
 {re:/^#\/case\/([^/]+)\/([^/]+)$/,nav:'cases',title:'咨询记录',mod:detail},
 {re:/^#\/playground(?:\/(model-[a-f0-9]{12}))?$/,nav:'playground',title:'比比看',mod:playground},
 {re:/^#\/activity$/,nav:'activity',title:'进度',mod:activity}
];
async function route(){
 const hash=location.hash||'#/run',hit=routes.find(r=>r.re.test(hash));
 if(!hit){location.hash='#/run';return;}
 if(hit.nav==='guide'){
  try{if(lastHash&&lastHash!=='#/guide'){guideReturn=lastHash;sessionStorage.setItem('shangyi-guide-return',guideReturn);}else guideReturn=sessionStorage.getItem('shangyi-guide-return')||'#/run';}catch{guideReturn=lastHash||'#/run';}
  if(!routes.some(r=>r.nav!=='guide'&&r.re.test(guideReturn)))guideReturn='#/run';
 }
 lastHash=hash;
 const id=++generation;stopAllPolls();current?.destroy?.();current=hit.mod;
 composer('');emptyEvidence();setStatus('');host.innerHTML='';host.scrollTop=0;
 document.getElementById('crumb').textContent=hit.title;
 document.title=hit.title+' · 商奕';
 document.querySelectorAll('[data-nav]').forEach(a=>{const selected=a.dataset.nav===hit.nav;a.classList.toggle('active',selected);if(selected)a.setAttribute('aria-current','page');else a.removeAttribute('aria-current');});
 document.querySelectorAll('#recent-cases a').forEach(a=>a.classList.toggle('active',a.getAttribute('href')===hash));
 closeMobileNav();
 // Each mount gets its own detached-on-navigation node, so late requests cannot replace the next route.
 const mount=document.createElement('div');mount.className=hit.nav==='playground'?'':'reading-column';host.replaceChildren(mount);
 const held=holdView(mount);
 try{await hit.mod.render(mount,{match:hash.match(hit.re),isCurrent:()=>id===generation,returnTo:guideReturn});}
 catch(e){if(id===generation){mount.innerHTML='<div class="blank-state"><strong>暂时无法打开</strong><p>'+esc(e.message)+'</p><button class="secondary-button" id="retry-page">重新加载</button></div>';mount.querySelector('#retry-page').onclick=route;}}
 if(id!==generation)return;
 if(held)enterView(mount);
 if(hit.nav==='playground'){emptyEvidence('先想两边可能怎么做，再为每种情况选“吃亏、差不多、划算”。这里是让你自己试着比较，不会自动查真实利润，也不知道对方下一步会做什么。');setStatus('自己试试');}
}
for(const a of document.querySelectorAll('[data-icon]'))a.insertAdjacentHTML('afterbegin',icon(a.dataset.icon));
document.getElementById('header-icon').innerHTML=icon('chat');
for(const [id,name] of Object.entries({'sidebar-close':'panel','sidebar-open':'panel','evidence-close':'panel','refresh-history':'refresh','activity-link':'activity'}))document.getElementById(id).innerHTML=icon(name);
function closeMobileNav(){document.body.classList.remove('mobile-nav');document.getElementById('sidebar-scrim').hidden=true;}
document.getElementById('sidebar-close').onclick=()=>{if(innerWidth<=680)closeMobileNav();else document.body.classList.add('sidebar-hidden');};
document.getElementById('sidebar-open').onclick=()=>{document.body.classList.remove('sidebar-hidden');if(innerWidth<=680){document.body.classList.add('mobile-nav');document.getElementById('sidebar-scrim').hidden=false;}};
document.getElementById('sidebar-scrim').onclick=closeMobileNav;
// UI-008：点**当前**页面的导航入口时 hash 不变 ⇒ 不触发 hashchange ⇒ route() 不跑 ⇒
// 导航和遮罩一直开着。所以每个入口都自己关，不依赖路由切换。
for(const a of document.querySelectorAll('[data-nav]')){
 a.addEventListener('click',()=>{if(innerWidth<=680)closeMobileNav();});
}
function toggleEvidence(force){
 const mobile=innerWidth<=950;
 const open=force??(mobile?!document.body.classList.contains('evidence-open'):document.body.classList.contains('evidence-hidden'));
 document.body.classList.toggle('evidence-hidden',!open);document.body.classList.toggle('evidence-open',open);
 document.getElementById('evidence-toggle').setAttribute('aria-expanded',String(open));
 // UI-006：窄屏上面板是覆盖层，开着的时候要把背景排除出键盘范围、把焦点引进面板，
 // 否则 Tab 会走到被面板挡住的背景按钮上（看得见的点不到，点得到的看不见）。
 if(mobile&&open)enterEvidencePanel();else leaveEvidencePanel();
}

/** 面板里可聚焦的控件（关不掉的面板等于陷阱）。*/
function evidenceFocusables(){
 const panel=document.getElementById('evidence-panel');
 return panel?[...panel.querySelectorAll('button,[href],input,select,textarea,summary,[tabindex]:not([tabindex="-1"])')].filter(el=>!el.disabled&&el.offsetParent!==null):[];
}

function enterEvidencePanel(){
 const panel=document.getElementById('evidence-panel');
 // 优先用 inert：把面板以外的可交互元素整体排除，Tab/Shift+Tab 都出不去
 // 只把**面板以外**的部分设为 inert：面板本身在 .workspace-body 里，
 // 要是把整个 .workspace 一起 inert，面板里的关闭按钮也点不动、聚焦不了。
 const others=[document.querySelector('.app-sidebar'),document.querySelector('.workspace-header'),
  document.querySelector('.center-pane')].filter(Boolean);
 if(panel&&typeof panel.inert==='boolean'&&others.every(el=>typeof el.inert==='boolean')){
  others.forEach(el=>{el.inert=true;});
  evidenceInertOthers=others;
 }
 // 焦点先落到面板里的关闭按钮上（永远可见、永远可操作）
 const close=document.getElementById('evidence-close');
 if(close)close.focus();
 evidenceTrap=(e)=>{
  if(e.key!=='Tab')return;
  const items=evidenceFocusables();
  if(!items.length)return;
  const first=items[0],last=items[items.length-1];
  const active=document.activeElement;
  if(!panel?.contains(active)){e.preventDefault();first.focus();return;}
  if(e.shiftKey&&active===first){e.preventDefault();last.focus();}
  else if(!e.shiftKey&&active===last){e.preventDefault();first.focus();}
 };
 document.addEventListener('keydown',evidenceTrap,true);
}

function leaveEvidencePanel(){
 if(evidenceTrap){document.removeEventListener('keydown',evidenceTrap,true);evidenceTrap=null;}
 if(evidenceInertOthers){evidenceInertOthers.forEach(el=>{el.inert=false;});evidenceInertOthers=null;}
}

let evidenceTrap=null,evidenceInertOthers=null;
document.getElementById('evidence-toggle').onclick=()=>toggleEvidence();
document.getElementById('evidence-close').onclick=()=>{toggleEvidence(false);document.getElementById('evidence-toggle').focus();};
// Escape 关面板：焦点回到触发按钮，原页面输入不动
document.addEventListener('keydown',e=>{if(e.key==='Escape'){
 closeMobileNav();
 if(innerWidth<=950&&document.body.classList.contains('evidence-open')){toggleEvidence(false);document.getElementById('evidence-toggle').focus();}
}});
const breakpoint=matchMedia('(max-width:950px)');breakpoint.addEventListener('change',()=>{toggleEvidence(!breakpoint.matches);if(breakpoint.matches)leaveEvidencePanel();});toggleEvidence(!breakpoint.matches);
async function history(){
 const root=document.getElementById('recent-cases');
 try{const results=await Promise.allSettled([api.cases(),api.chats()]);
 if(results.every(r=>r.status==='rejected'))throw new Error('列表没读到');
 const list=cases.mergeConsultations(results[0].value?.cases||[],results[1].value?.chats||[]);
 root.innerHTML=(list.slice(0,18).map(c=>'<a href="'+c.href+'" title="'+esc(c.title)+'">'+icon(c.href.startsWith('#/chat/')?'chat':'file')+'<span>'+esc(c.title)+'</span></a>').join('')||'<p class="sidebar-note">咨询记录存在这儿。</p>')+(results.some(r=>r.status==='rejected')?'<p class="sidebar-note">有的没读到；点上面刷新。</p>':'');
 root.querySelectorAll('a').forEach(a=>a.classList.toggle('active',a.getAttribute('href')===location.hash));
 }catch{root.innerHTML='<p class="sidebar-note">列表没读到。点上面刷新。</p>';}
}
document.getElementById('refresh-history').onclick=history;
window.addEventListener('shangyi:history',history);
api.health().then(h=>{
 const el=document.getElementById('engine-status');
 const search=h.services?.search,bocha=search?.providers?.bocha?.status==='configured';
 el.textContent=h.frontend_contract<3?'服务要重启':!h.interactive_chat?'只能看记录':bocha?'能开聊 · 搜索配好了':search?.status==='unconfigured'?'能开聊 · 搜索没配':search?.status==='unknown'?'能开聊 · 搜索凭据读不到':'能开聊 · 搜索用不了';
 el.title=[h.services?.search?.detail,h.services?.model?.detail,h.services?.fetch?.detail].filter(Boolean).join('；');
}).catch(()=>document.getElementById('engine-status').textContent='本地服务没连上');
window.addEventListener('hashchange',route);history();route();
