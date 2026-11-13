// @ts-check
import {esc} from './util.js';

export const FIELD_LABEL={address:'地址',name:'店名',category:'做哪行',property_type:'房子类型',transaction_type:'交易类型',rent:'挂的租金',rent_unit:'按什么算',area:'面积',area_sqm:'面积',floor:'楼层',status:'营业状态',transfer_fee:'转让费'};
const freshness={
 recently_published_not_verified_current:'近期发布；当前有效性仍待核验',
 older_than_review_window:'较早资料；不能当作当前租金或营业状态',
 undated:'发布时间未知；不能当作当前租金或营业状态',
 clock_unavailable:'本机日期不可用；无法判断资料时效'
};
const basis={user_provided:'你提供的',public_quote:'公开报价',assumption:'分析假设'};
const period={once:'一次性',month:'每月',year:'每年',day_per_sqm:'每天每平方米'};
const treatment={cash_expense:'现金花费',refundable_deposit:'能退的押金',opportunity_cost:'让出的机会',noncash_expense:'不掏钱的费用',unknown:'性质待确认'};
function link(url,label='查看来源'){
 if(typeof url!=='string'||/[\u0000-\u0020\u007f]/.test(url))return '';
 try{const u=new URL(url);return ['http:','https:'].includes(u.protocol)&&!u.username&&!u.password?'<a target="_blank" rel="noopener noreferrer" href="'+esc(u.href)+'">'+esc(label)+'</a>':'';}catch{return '';}
}
function amount(value){return typeof value==='number'&&Number.isFinite(value)?value.toLocaleString('zh-CN',{maximumFractionDigits:2}):'未取得';}
function recordCard(r){
 const fields=r.fields||{},entries=Object.entries(fields),title=r.kind==='rental'?(fields.address?.value||'租的挂牌'):(fields.name?.value||'可能的对手店');
 const detail=entries.map(([key,item])=>'<div><dt>'+esc(FIELD_LABEL[key]||key)+'</dt><dd>'+esc(item?.value||'未取得')+'</dd></div>').join('');
 const quotes=entries.filter(([,item])=>item?.quote).map(([key,item])=>'<p><strong>'+esc(FIELD_LABEL[key]||key)+'</strong><br>'+esc(item.quote)+'</p>').join('');
 const more=(r.source_urls||[]).filter(url=>url!==r.url).map((url,i)=>link(url,'同项其他网址 '+(i+1))).filter(Boolean).join(' ');
 return '<article class="research-card"><h3>'+esc(title)+'</h3><p class="research-scope">'+esc(r.search_scope||'没写范围')+'</p>'+
  '<dl class="research-fields">'+detail+'</dl><p class="research-caution">发布时间：'+esc(r.published_at||'未知')+'。'+esc(freshness[r.freshness_review]||'没分时效')+'</p>'+
  '<div class="research-links">'+link(r.url,'看原文')+more+'</div>'+
  (quotes?'<details class="research-quotes"><summary>看原文摘录</summary>'+quotes+'</details>':'')+
  '<p class="research-caution">引文都出自同一段网页；挂牌、状态和解读还要再核。</p></article>';
}
function costCard(c){
 const currency=esc(c.currency||'币种未注明'),items=Array.isArray(c.items)?c.items:[];
 const scenario=typeof c.scenario==='string'&&c.scenario.trim()?c.scenario.trim():'没起名字的情况';
 const classified=typeof c.cash_classification_complete==='boolean'&&items.length>0&&items.every(item=>Object.hasOwn(treatment,item.treatment||item.original?.treatment));
 const itemTreatment=item=>item.treatment||item.original?.treatment;
 const isOnce=item=>item.normalized_period==='once'||(!item.normalized_period&&item.original?.period==='once');
 const knownAmount=item=>typeof item.normalized_amount==='number'&&Number.isFinite(item.normalized_amount);
 const missingAmounts=c.amounts_complete===false||items.some(item=>!knownAmount(item)),unknownTypes=items.some(item=>itemTreatment(item)==='unknown');
 const ready=classified&&c.cash_classification_complete&&!missingAmounts&&!unknownTypes;
 const total=(label,value,group=[])=>{
  const missing=group.some(item=>!knownAmount(item)),anyKnown=group.some(knownAmount);
  return '<div class="cost-total"><span>'+label+(missing?'（'+(anyKnown?'仅已知部分':'金额未齐')+'）':'')+'</span><strong>'+(missing&&!anyKnown?'金额未取得':amount(value))+'</strong></div>';
 };
 const categoryTotal=(label,value,kind,once)=>{
  const group=items.filter(item=>itemTreatment(item)===kind&&isOnce(item)===once);
  return group.length||(typeof value==='number'&&Number.isFinite(value)&&value!==0)?total(label,value,group):'';
 };
 const rows=items.map(item=>{
  const original=item.original||{};
  return '<li><strong>'+esc(item.name||original.name||'费用')+' · '+esc(treatment[itemTreatment(item)]||'旧版未分类')+'</strong><span>'+(knownAmount(item)?amount(item.normalized_amount)+' '+currency:'金额未取得')+' / '+(isOnce(item)?'一次性':'月')+'</span><small>'+esc(basis[original.basis]||'来源未注明')+'：'+esc(original.source||item.source||'未注明')+'；原值 '+(typeof original.amount==='number'?amount(original.amount):'金额未取得')+' / '+esc(period[original.period]||original.period||'未注明')+'</small>'+
   (!knownAmount(item)&&item.missing_amount_reason?'<small>金额说明：'+esc(item.missing_amount_reason)+'</small>':'')+'</li>';
 }).join('');
 let base='<div class="evidence-group">要花的钱 · '+currency+'</div><section class="cost-card"><h3>'+esc(scenario)+'</h3>';
 if(c.distinct_reason)base+='<p class="research-scope">为什么另列：'+esc(c.distinct_reason)+'</p>';
 if(classified){
  base+=total('先掏的现金和押金',c.listed_upfront_cash,items.filter(item=>isOnce(item)&&['cash_expense','refundable_deposit'].includes(itemTreatment(item))))+
   categoryTotal('其中：一次性现金花费',c.listed_upfront_cash_expense,'cash_expense',true)+
   categoryTotal('其中：能退的押金（先占钱）',c.listed_refundable_deposits,'refundable_deposit',true)+
   total('每月固定要掏的现金',c.listed_monthly_fixed_cost,items.filter(item=>!isOnce(item)&&itemTreatment(item)==='cash_expense'))+
   categoryTotal('一次性让出的机会（不掏现金）',c.listed_upfront_opportunity_cost,'opportunity_cost',true)+
   categoryTotal('每月让出的机会（不掏现金）',c.listed_monthly_opportunity_cost,'opportunity_cost',false)+
   categoryTotal('一次性不掏钱的费用',c.listed_upfront_noncash_expense,'noncash_expense',true)+
   categoryTotal('每月不掏钱的费用',c.listed_monthly_noncash_expense,'noncash_expense',false);
  if(missingAmounts)base+='<p class="research-caution"><strong>仍有费用金额未取得。</strong>未知金额不是零；这些项目保留在明细中，尚未计入已知金额小计，不能据此判断资金足够或计算完整现金门槛。</p>';
  if(unknownTypes)base+='<p class="research-caution"><strong>仍有费用性质待确认。</strong>这些项目保留在明细中，尚未计入现金、押金或其他分类小计；不能据此判断资金足够。</p>';
  if(!c.cash_classification_complete&&!missingAmounts&&!unknownTypes)base+='<p class="research-caution">费用信息尚未完整，当前金额仅为已取得部分，不能据此判断资金足够。</p>';
 }else{
  base+='<p class="research-caution"><strong>老版没分清现金和让出的机会，要重算。</strong>以下仅保留当时记录，不作为现金支出或资金余额的可靠结论。</p>'+
   total('老版：先掏的钱合计',c.listed_upfront_cash)+total('老版：每月固定合计',c.listed_monthly_fixed_cost);
 }
 base+=
  '<p class="research-caution">'+(c.costs_complete&&ready?'按输入声明的完整固定成本测算。':'成本尚未列全，未列费用不视为零。')+'</p>';
 let calculations='';
 if(classified&&c.horizon_totals){
  const h=c.horizon_totals,partial=!h.complete||!ready;
  calculations+='<div class="cost-result"><strong>'+amount(h.months)+' 个月·列出来的钱累计</strong>'+
   total('现金花费',h.listed_cash_expense,items.filter(item=>itemTreatment(item)==='cash_expense'))+
   total('连押金一起掏的',h.listed_cash_outflow_before_deposit_return,items.filter(item=>['cash_expense','refundable_deposit'].includes(itemTreatment(item))))+
   (h.listed_opportunity_cost?'<span>让出的机会另算：'+amount(h.listed_opportunity_cost)+'</span>':'')+
   (h.listed_noncash_expense?'<span>不掏钱的费用另算：'+amount(h.listed_noncash_expense)+'</span>':'')+
   '<small>这是整个期间的累计，不是开始时一次付清。月费按所填金额固定计算，未推定押金返还时间。'+(partial?'仅为已知、已分类费用的部分合计；未知项未按零计算。':'仍以输入范围与条件为限。')+'</small></div>';
 }
 if(ready&&c.break_even)calculations+='<div class="cost-result"><span>每月要卖多少才够付固定现金</span><strong>'+amount(c.break_even.monthly_revenue)+'</strong><span>折合每天几单：'+amount(c.break_even.daily_orders)+'</span><small>'+esc(c.break_even.basis==='listed_costs_only_lower_bound'?'仅基于已列且已分类的固定现金费用；实际门槛可能更高。':'按所列固定现金费用和销售假设计算。')+' 让出的机会和不掏钱的费用另算，没进这个门槛。</small></div>';
 if(classified&&typeof c.cash_after_listed_upfront==='number')calculations+='<p class="cost-extra">'+
  (c.cash_after_listed_upfront<0?'<strong>前期资金不足</strong>：先掏的现金和押金超过手头的现金 '+amount(-c.cash_after_listed_upfront):(missingAmounts?'只扣了知道多少钱的先掏项，剩：':'只扣了先掏的现金和押金，剩：')+amount(c.cash_after_listed_upfront))+
  (missingAmounts?'；未计入金额未取得的费用，不能视为完整现金余额':'')+(unknownTypes?'；未计入性质待确认的费用':'')+'。</p>';
 if(ready&&c.cash_scenario){
  const cash=c.cash_scenario,burn=cash.monthly_burn,balance=cash.cash_after_upfront??c.cash_after_listed_upfront;
  const knownBurn=typeof burn==='number'&&Number.isFinite(burn);
  let cashText=knownBurn?(burn<0?'收入情景下每月现金净增加：'+amount(-burn):burn===0?'收入情景下每月现金收支持平':'收入情景下每月现金消耗：'+amount(burn)):'本情景的每月现金变化未取得';
  if(cash.status==='insufficient_upfront_cash'||(typeof balance==='number'&&balance<0)){
   if(!(typeof c.cash_after_listed_upfront==='number'&&c.cash_after_listed_upfront<0))cashText+='。<strong>前期资金不足</strong>：已列一次性现金费用及押金超过可用现金'+(typeof balance==='number'&&Number.isFinite(balance)&&balance<0?' '+amount(-balance):'');
   cashText+='。尚有前期资金缺口，不能据此给出经营可持续月数。';
  }else if(knownBurn&&burn<=0){
   cashText+='；按这一收入和列出的花费，没有逐月消耗现金。';
  }else if(knownBurn&&burn>0&&typeof cash.runway_months==='number'&&Number.isFinite(cash.runway_months)){
   cashText+='；仅按所列收入和费用，剩余现金约可覆盖 '+amount(cash.runway_months)+' 个月。';
  }else{
   cashText+='；本情景未取得可计算的现金耗尽月数。';
  }
  calculations+='<p class="cost-extra">'+cashText+'</p><p class="research-caution">以上是输入条件保持不变时的现金测算，未包含未列费用、收入变化和收付款时间差，不代表实际可经营月数。</p>';
 }
 const detail='<details class="cost-details"><summary>钱从哪来、怎么算的</summary>'+
  (rows?'<ul>'+rows+'</ul>':'<p>还没有逐项的钱可看。</p>')+
  (ready&&c.break_even?'<p>每月固定现金 ÷ 毛利率 = 每月要卖的；再按客单价和营业天数换成每天几单，往上取整。</p>':'')+
  '<p>一年的钱除以 12；按天、按面积报的，用你填的面积和每月收费天数算。一次性的单独列。</p>'+
  (classified?'<p>押金先占着现金；让出的机会和不掏钱的费用，不从现金里扣。</p>':'')+
  (c.note?'<p>'+esc(c.note)+'</p>':'')+'</details>';
 return base+calculations+detail+'<p class="research-caution">条件计算，不是客流或收益预测；公开报价也不等于成交价。</p></section>';
}
function webSources(sources){
 const valid=(Array.isArray(sources)?sources:[]).filter(source=>source&&link(source.url));
 if(!valid.length)return '';
 const read=valid.filter(s=>s.status==='body_retrieved'),unread=valid.filter(s=>s.status!=='body_retrieved');
 const card=s=>{
  const read=s.status==='body_retrieved';
  let timestamp='没记读取时间';
  if(Number.isFinite(s.retrieved_at_unix)){
   const date=new Date(s.retrieved_at_unix*1000);
   if(!Number.isNaN(date.getTime()))timestamp='读取于 '+date.toLocaleString('zh-CN');
  }
  let replay='';
  if(Number.isFinite(s.last_replayed_snapshot_at_unix)){
   const date=new Date(s.last_replayed_snapshot_at_unix*1000);
   if(!Number.isNaN(date.getTime()))replay='<p class="small-note">最近一次调用复用了较早的页面快照（读取于 '+esc(date.toLocaleString('zh-CN'))+'）；上方保留较新的已读记录。</p>';
  }
  const host=new URL(s.url).hostname;
  return '<article class="web-source-card">'+link(s.url,s.title||host)+'<small>'+esc(host)+' · '+(read?'读到正文了':s.status==='body_unavailable'?'正文还没读到':'只有搜索摘要')+'</small>'+
   (read?'<small>'+esc(timestamp)+(s.cached?' · 复用本次会话快照':'')+'</small>':'')+
   (read&&s.last_attempt_failed?'<p class="small-note">最近一次重新读取未成功，先前已取得的页面仍保留。</p>':'')+(read?replay:'')+'</article>';
 };
 return '<div class="evidence-group">查阅来源 · 已读 '+read.length+' 个页面</div>'+
  '<p class="small-note">读取成功不等于页面证明了所有结论。点击标题可核对原文；读取时间不是文章发布日期。</p>'+read.map(card).join('')+
  (unread.length?'<details class="web-source-leads"><summary>尚未取得正文的线索（'+unread.length+'）</summary><p class="small-note">只是线索，不算核对过的证据。</p>'+unread.map(card).join('')+'</details>':'');
}

export function renderResearch(data){
 let html='';
 for(const r of data.research||[])html+='<div class="evidence-group">'+(r.kind==='rental'?'租的挂牌':'可能的对手店')+'</div>'+recordCard(r);
 const costs=Array.isArray(data.business_costs)&&data.business_costs.length?data.business_costs:(data.business_cost?[data.business_cost]:[]);
 const scenarios=new Map();
 for(const cost of costs){if(cost&&typeof cost==='object')scenarios.set(cost.scenario_id?'id:'+cost.scenario_id:'legacy:'+(typeof cost.scenario==='string'?cost.scenario.trim():''),cost);}
 for(const cost of scenarios.values())html+=costCard(cost);
 html+=webSources(data.web_sources);
 return html;
}