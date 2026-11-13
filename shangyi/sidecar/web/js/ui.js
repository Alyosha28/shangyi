// @ts-check
import { esc } from './util.js';
import { fadeIn, fadeOut, statusIn, enterNew, resetItems } from './motion.js';

const paths = {
 plus:'<path d="M12 5v14M5 12h14"/>',
 search:'<circle cx="10.5" cy="10.5" r="6.5"/><path d="m16 16 4 4"/>',
 grid:'<rect x="4" y="4" width="16" height="16" rx="2"/><path d="M4 12h16M12 4v16"/>',
 chat:'<path d="M5 4h14a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H9l-5 3V6a2 2 0 0 1 1-2Z"/>',
 panel:'<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M9 4v16"/>',
 close:'<path d="m6 6 12 12M6 18 18 6"/>',
 arrow:'<path d="M12 19V5m-6 6 6-6 6 6"/>',
 file:'<path d="M14 3H6a2 2 0 0 0-2 2v14a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V9zM14 3v6h6M8 13h8M8 17h6"/>',
 refresh:'<path d="M20 7v5h-5M4 17v-5h5"/><path d="M6 7a7 7 0 0 1 12-1l2 6M4 12l2 6a7 7 0 0 0 12-1"/>',
 activity:'<path d="M3 12h4l3-7 4 14 3-7h4"/>',
 copy:'<rect x="8" y="8" width="12" height="13" rx="2"/><path d="M16 8V3H3v13h5"/>',
 download:'<path d="M12 3v12m-5-5 5 5 5-5M4 17v4h16v-4"/>',
 check:'<path d="m5 12 4 4L19 6"/>',
 globe:'<circle cx="12" cy="12" r="9"/><path d="M3 12h18M12 3a17 17 0 0 1 0 18 17 17 0 0 1 0-18"/>',
 clock:'<circle cx="12" cy="12" r="9"/><path d="M12 7v5l3 2"/>',
 edit:'<path d="m15 4 5 5-10 10-6 1 1-6zM13 6l5 5"/>'
};
export function icon(name){return '<svg class="icon" aria-hidden="true" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-linecap="round" stroke-linejoin="round">'+(paths[name]||paths.file)+'</svg>';}
export const TOPICS = {
 pricing_response:{label:'定价',hint:'别人降价，我跟不跟',icon:'activity'},
 negotiation:{label:'谈条件',hint:'想清底线再开口',icon:'chat'},
 market_entry:{label:'新生意',hint:'开店前先看这几样',icon:'globe'},
 competitor_intel:{label:'看对手',hint:'对手最近做了什么',icon:'search'}
};
export function topicName(id){return TOPICS[id]?.label||id||'经营';}
export function caseHref(c){return '#/case/'+encodeURIComponent(c.date)+'/'+encodeURIComponent(c.case_id);}
export function evidence(html,title='看依据'){
 document.getElementById('evidence-title').textContent=title;
 const host=document.getElementById('evidence-content');
 // 只在内容真的变了时入场：证据面板每次轮询都会被重设，无条件动画会一直闪。
 const changed=host.innerHTML!==html;
 host.innerHTML=html;
 if(changed)enterNew(host,'.evidence-item');
}
export function emptyEvidence(message='这次用到的资料和依据，都在这儿。'){
 evidence('<div class="evidence-empty">'+icon('file')+esc(message)+'</div>');
 // 换上下文：清掉判重记录，否则新页面里同名条目会被当成「早就出现过了」。
 resetItems(document.getElementById('evidence-content'));
}
export function composer(html){document.getElementById('composer-host').innerHTML=html;}
export function setStatus(text){
 const el=document.getElementById('page-status');
 // 轮询会反复设回同样的文字；不是真变化就不动画，否则状态条一直闪。
 if(el.textContent===text)return;
 el.textContent=text;
 statusIn(el);
}
export function assistant(html){return '<div class="thread-message assistant-message"><div class="assistant-mark" aria-hidden="true">弈</div><div class="assistant-body">'+html+'</div></div>';}
export function userMessage(text){return '<div class="user-message"><div class="user-bubble">'+esc(text)+'</div></div>';}
let toastTimer,toastSeq=0;
export function toast(message,error=false){
 const t=document.getElementById('toast');
 // seq 防止「新的弹出来了，旧的退场定时器到了把新的一起收掉」。
 const seq=++toastSeq;
 t.textContent=message;t.className=error?'err':'';t.hidden=false;
 fadeIn(t);
 clearTimeout(toastTimer);
 toastTimer=setTimeout(()=>{
  if(seq!==toastSeq)return;
  fadeOut(t,()=>{if(seq===toastSeq)t.hidden=true;});
 },4500);
}
export function bindEnter(textarea,submit){
 textarea.addEventListener('keydown',e=>{if(e.key==='Enter'&&!e.shiftKey&&!e.isComposing&&e.keyCode!==229){e.preventDefault();submit();}});
}
export function plainBlock(value){return typeof value==='string'?value:JSON.stringify(value,null,2);}
export function sourceItem(title,description,detail,kind='file'){
 // data-item 供 motion.js 的 enterNew 判重（同类条目只入场一次）。
 // 用标题当 key：同面板内标题重复时后一条不再入场，纯外观影响，无正确性风险。
 // esc 已把引号转成实体，因此放进属性里是安全的。
 const key=' data-item="'+esc(title)+'"';
 if(!detail)return '<div class="evidence-item"'+key+'><div class="source-head"><span class="source-icon">'+icon(kind)+'</span><span class="source-summary"><span class="source-title">'+esc(title)+'</span><span class="source-meta">'+esc(description)+'</span></span></div></div>';
 return '<details class="evidence-item"'+key+'><summary><span class="source-icon">'+icon(kind)+'</span><span class="source-summary"><span class="source-title">'+esc(title)+'</span><span class="source-meta">'+esc(description)+'</span></span></summary>'+(detail?'<pre>'+esc(plainBlock(detail))+'</pre>':'')+'</details>';
}
export function downloadText(name,text){
 const blob=new Blob([text],{type:'text/plain;charset=utf-8'}),url=URL.createObjectURL(blob),a=document.createElement('a');
 a.href=url;a.download=name;a.click();setTimeout(()=>URL.revokeObjectURL(url),1000);
}