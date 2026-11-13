// @ts-check
export const OPENING_DRAFT_KEY='shangyi-freeform-draft-v1';
const GUIDE_KEY='shangyi-guide-v1';
let memory=null;
const unavailableStorage=Symbol('guide-storage-unavailable');
export function newGuide(){return {step:0,draft:'',opening:'',answer:'',corrections:[],asked:false,sourcesSeen:false,realDraft:''};}
export function readGuide(storage){
 let source=storage??unavailableStorage;
 try{
  source=storage??globalThis.sessionStorage;
  // A readable old value must not overwrite newer in-memory progress after a failed write.
  if(memory?.source===source&&!memory.saved)return structuredClone(memory.state);
  const value=JSON.parse(source.getItem(GUIDE_KEY)||'null');
  if(value&&Number.isInteger(value.step)&&value.step>=0&&value.step<=4){
   const next=newGuide();next.step=value.step;
   for(const key of ['draft','opening','answer','realDraft'])if(typeof value[key]==='string')next[key]=value[key];
   next.corrections=Array.isArray(value.corrections)?value.corrections.filter(x=>typeof x==='string'):[];
   next.asked=value.asked===true;next.sourcesSeen=value.sourcesSeen===true;
   return next;
  }
  return newGuide();
 }catch{
  if(memory?.source===source){memory.saved=false;return structuredClone(memory.state);}
  memory={source,state:newGuide(),saved:false};
  return newGuide();
 }
}
export function saveGuide(state,storage){
 memory={source:storage??unavailableStorage,state:structuredClone(state),saved:false};
 try{
  memory.source=storage??globalThis.sessionStorage;
  memory.source.setItem(GUIDE_KEY,JSON.stringify(state));memory.saved=true;return true;
 }catch{return false;}
}
export function guideIsSaved(storage){
 try{const source=storage??globalThis.sessionStorage;return !(memory?.source===source&&!memory.saved);}catch{return false;}
}
export function guideAction(state,action,text=''){
 const next=structuredClone(state),value=String(text).trim();
 if(action==='restart')return {...newGuide(),realDraft:state.realDraft};
 if(action==='submit'&&state.step<2){
  if(!value)throw new Error(state.step===0?'先写一句练习开场，也可以点击示例填入。':'写下你的练习回答；不清楚就说“还没想好”。');
  next[state.step===0?'opening':'answer']=value;next.draft='';next.step++;
 }else if(action==='correct'&&state.step===2){
  if(!value)throw new Error('写下想改的地方，也可以直接说“这点不确定，先保留未知”。');
  next.corrections.push(value);next.draft='';
 }else if(action==='ask'&&state.step===2){next.asked=true;
 }else if(action==='confirm'&&state.step===2){
  if(state.draft.trim())throw new Error('还有未发送的修改，请先发送或取消这次修改。');
  next.step=3;next.draft='';
 }else if(action==='sources'&&state.step===3){next.sourcesSeen=true;
 }else if(action==='finish'&&state.step===3){
  if(!state.sourcesSeen)throw new Error('先展开一次示例依据，看看哪些信息来自哪里。');
  next.step=4;
 }else throw new Error('练习步骤已变化，请按当前页面继续。');
 return next;
}
// 引导第三步的「当前有效前提」合并视图：练习回答 + 之后发送的修改并成一份清单，
// 确认按钮旁边能看到全部待确认内容，而不是只挂着一开始的回答（UI-012）。
// 只呈现用户真正说过的话，不替用户判断一条修改改的是哪一句：修改按发送顺序列在原回答之后并标出最新说法，
// 原文一律保留备查；拿不到信息的项直接写成未知，不用默认值填空，也不把练习回答冒充成用户判断。
export function guidePremises(state){
 const answer=typeof state?.answer==='string'?state.answer:'';
 const corrections=(Array.isArray(state?.corrections)?state.corrections:[]).filter(text=>typeof text==='string');
 // order/latest 只有修改行用得到，做成可选字段，其余行保持同一形状。
 /** @type {{key:string,status:string,text:string,order?:number,latest?:boolean}[]} */
 const rows=[
  {key:'goal',status:'fixed',text:'想清楚怎么谈续租。'},
  {key:'rent',status:corrections.length?'revised':'kept',text:answer},
  {key:'landlord',status:'unknown',text:'没有信息，保留未知。'},
 ];
 for(const [index,text] of corrections.entries())rows.push({key:'revision',status:'revision',text,order:index+1,latest:index===corrections.length-1});
 return {rows,revised:corrections.length>0};
}
export function prepareOpening(text,storage,replace=false){
 const value=String(text).trim();if(!value)throw new Error('写一句你真正想讨论的事情，再带到新咨询。');
 let raw,old=null;
 try{storage=storage??globalThis.sessionStorage;raw=storage.getItem(OPENING_DRAFT_KEY);}catch{throw new Error('浏览器暂时无法读取草稿。当前问题仍在输入框中，请稍后再试。');}
 if(raw){try{
  old=JSON.parse(raw);
  if(old!==null&&(typeof old!=='object'||Array.isArray(old)||old.text!==undefined&&typeof old.text!=='string'))throw new Error('invalid draft');
 }catch{throw new Error('原草稿暂时无法读取。可以返回新咨询查看，当前内容会保留在这里。');}}
 if(old?.text?.trim()===value)return {conflict:false};
 if(old?.text?.trim()&&!replace)return {conflict:true,previous:old.text};
 try{
  if(raw&&replace)storage.setItem(OPENING_DRAFT_KEY+':before-guide',raw);
  storage.setItem(OPENING_DRAFT_KEY,JSON.stringify({text:value,topic:'',mode:'scenario',action:'',attempt:null}));
 }catch{throw new Error('浏览器暂时无法保存草稿。当前问题仍在输入框中，没有发送，请稍后再试。');}
 return {conflict:false};
}
export function guideEntry(){
 const s=readGuide();
 const label=s.step===4?'看引导':s.step>0?'继续练习':'开始引导';
 return '<aside class="guide-entry"><div><strong>第一次用？先跟我聊一小段</strong><p>练习怎么开场、回答和核对，再聊你自己的事。</p></div><a class="secondary-button" href="#/guide">'+label+'</a></aside>';
}