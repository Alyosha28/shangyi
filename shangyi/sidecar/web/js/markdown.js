// @ts-check
import {esc} from './util.js';

// Deliberately restricted Markdown, with no HTML passthrough or external resources.
// Supported: ATX headings, paragraphs, lists, pipe tables, fenced/inline code,
// strong/emphasis and HTTP(S) links. Other syntax stays readable text.
function safeLink(value){
 if(!/^https?:\/\//i.test(value)||/[\u0000-\u0020\u007f]/.test(value))return null;
 try{const url=new URL(value);return ['http:','https:'].includes(url.protocol)&&!url.username&&!url.password?url.href:null;}catch{return null;}
}
function bareLink(text,start){
 if(start>0&&/[\p{L}\p{N}_]/u.test(text[start-1])&&!/[\p{Script=Han}]/u.test(text[start-1]))return null;
 const match=text.slice(start).match(/^https?:\/\/[^\s<>"'`，。；！？、【】「」]+/i);
 if(!match)return null;
 let value=match[0].replace(/[.,;:!?]+$/g,'');
 for(const [open,close] of [['(',')'],['[',']'],['{','}']]){
  while(value.endsWith(close)&&value.split(close).length>value.split(open).length)value=value.slice(0,-1);
 }
 const href=safeLink(value);
 return href?{href,value,end:start+value.length}:null;
}
function closing(text,marker,start){
 let at=start;
 while((at=text.indexOf(marker,at))!==-1){
  let backslashes=0;for(let i=at-1;i>=0&&text[i]==='\\';i--)backslashes++;
  if(backslashes%2===0)return at;
  at+=marker.length;
 }
 return -1;
}
function readLink(text,start){
 const labelEnd=closing(text,']',start+1);
 if(labelEnd<0||text[labelEnd+1]!=='(')return null;
 let depth=1,end=labelEnd+2;
 for(;end<text.length;end++){
  if(text[end]==='\\'){end++;continue;}
  if(text[end]==='(')depth++;
  if(text[end]===')'&&--depth===0)break;
 }
 if(depth!==0)return null;
 const destination=text.slice(labelEnd+2,end).replace(/\\([\\()])/g,'$1');
 return {label:text.slice(start+1,labelEnd),url:destination,end:end+1};
}
function inline(text,depth=0,links=true){
 if(depth>6)return esc(text);
 let html='',at=0;
 while(at<text.length){
  const ch=text[at];
  if(ch==='\n'){html+='<br>';at++;continue;}
  if(ch==='\\'&&/[\\`*_[\]{}()#+.!|>~-]/.test(text[at+1]||'')){
   html+=esc(text[at+1]);at+=2;continue;
  }
  if(ch==='`'){
   const marker=text.slice(at).match(/^`+/)[0],end=closing(text,marker,at+marker.length);
   if(end>=0){html+='<code>'+esc(text.slice(at+marker.length,end))+'</code>';at=end+marker.length;continue;}
  }
  if(ch==='<'){
   const end=text.indexOf('>',at+1);
   if(end>=0){html+=esc(text.slice(at,end+1));at=end+1;continue;}
  }
  const image=ch==='!'&&text[at+1]==='[';
  if(image||ch==='['){
   const link=readLink(text,at+(image?1:0));
   if(link){
    const href=!image&&links?safeLink(link.url):null;
    html+=href?'<a href="'+esc(href)+'" target="_blank" rel="noopener noreferrer">'+inline(link.label,depth+1,false)+'</a>':esc(text.slice(at,link.end));
    at=link.end;continue;
   }
  }
  if(links&&(ch==='h'||ch==='H')){
   const link=bareLink(text,at);
   if(link){html+='<a href="'+esc(link.href)+'" target="_blank" rel="noopener noreferrer">'+esc(link.value)+'</a>';at=link.end;continue;}
  }
  if(ch==='*'||ch==='_'){
   const marker=text.startsWith(ch+ch,at)?ch+ch:ch;
   const end=closing(text,marker,at+marker.length);
   const inWord=ch==='_'&&(/[\p{L}\p{N}]/u.test(text[at-1]||'')||/[\p{L}\p{N}]/u.test(text[end+marker.length]||''));
   if(end>at+marker.length&&!inWord){
    const tag=marker.length===2?'strong':'em';
    html+='<'+tag+'>'+inline(text.slice(at+marker.length,end),depth+1,links)+'</'+tag+'>';at=end+marker.length;continue;
   }
  }
  html+=esc(ch);at++;
 }
 return html;
}
function tableRow(line){
 const source=line.trim(),cells=[];let cell='',ticks=0;
 for(let i=0;i<source.length;i++){
  const ch=source[i];
  if(ch==='\\'&&i+1<source.length){cell+=ch+source[++i];continue;}
  if(ch==='`'){
   let n=1;while(source[i+n]==='`')n++;
   if(!ticks)ticks=n;else if(ticks===n)ticks=0;
   cell+='`'.repeat(n);i+=n-1;continue;
  }
  if(ch==='|'&&!ticks){cells.push(cell.trim());cell='';}else cell+=ch;
 }
 cells.push(cell.trim());
 if(source.startsWith('|')&&cells[0]==='')cells.shift();
 if(source.endsWith('|')&&cells.at(-1)==='')cells.pop();
 return cells;
}
function tableHead(lines,at){
 if(!lines[at]?.includes('|')||!lines[at+1])return null;
 const header=tableRow(lines[at]),rules=tableRow(lines[at+1]);
 if(!header.length||header.length!==rules.length||!rules.every(x=>/^:?-{3,}:?$/.test(x)))return null;
 return {header,align:rules.map(x=>x.startsWith(':')&&x.endsWith(':')?'center':x.endsWith(':')?'right':'left')};
}
const fence=line=>line.match(/^ {0,3}(`{3,}|~{3,})(.*)$/);
const heading=line=>line.match(/^ {0,3}(#{1,6})[ \t]+(.+?)(?:[ \t]+#+)?[ \t]*$/);
const listItem=line=>line.match(/^( *)([-+*]|\d{1,9}[.)])\s+(.+)$/);
const rule=line=>/^ {0,3}(?:\*\s*){3,}$/.test(line)||/^ {0,3}(?:-\s*){3,}$/.test(line)||/^ {0,3}(?:_\s*){3,}$/.test(line);
function blockStart(lines,at){return !lines[at].trim()||fence(lines[at])||heading(lines[at])||listItem(lines[at])||rule(lines[at])||tableHead(lines,at);}
function blocks(lines,depth=0){
 if(depth>6)return '<p>'+inline(lines.join('\n'))+'</p>';
 const html=[];let at=0;
 while(at<lines.length){
  const line=lines[at];
  if(!line.trim()){at++;continue;}
  const code=fence(line);
  if(code){
   const body=[],marker=code[1][0],length=code[1].length;at++;
   while(at<lines.length){
    const end=lines[at].trim();
    if(end.length>=length&&[...end].every(c=>c===marker)){at++;break;}
    body.push(lines[at++]);
   }
   html.push('<div class="md-code-block">'+(code[2].trim()?'<div class="md-code-label">'+esc(code[2].trim())+'</div>':'')+'<pre><code>'+esc(body.join('\n'))+'</code></pre></div>');continue;
  }
  const title=heading(line);
  if(title){const level=title[1].length;html.push('<h'+level+'>'+inline(title[2])+'</h'+level+'>');at++;continue;}
  if(rule(line)){html.push('<hr>');at++;continue;}
  const table=tableHead(lines,at);
  if(table){
   const rows=[];at+=2;
   while(at<lines.length&&lines[at].trim()&&lines[at].includes('|')){
    const row=tableRow(lines[at]);if(row.length!==table.header.length)break;
    rows.push(row);at++;
   }
   const cells=(row,tag)=>row.map((cell,i)=>'<'+tag+(tag==='th'?' scope="col"':'')+' class="md-align-'+table.align[i]+'">'+inline(cell)+'</'+tag+'>').join('');
   html.push('<div class="md-table-wrap" role="region" aria-label="表格，可横向滚动" tabindex="0"><table><thead><tr>'+cells(table.header,'th')+'</tr></thead><tbody>'+rows.map(row=>'<tr>'+cells(row,'td')+'</tr>').join('')+'</tbody></table></div>');continue;
  }
  const first=listItem(line);
  if(first){
   const ordered=/^\d/.test(first[2]),tag=ordered?'ol':'ul',indent=first[1].length,items=[];
   while(at<lines.length){
    const item=listItem(lines[at]);
    if(!item||item[1].length!==indent||/^\d/.test(item[2])!==ordered)break;
    const content=[item[3]];at++;
    while(at<lines.length&&lines[at].trim()&&/^ */.exec(lines[at])[0].length>indent){
     content.push(lines[at].slice(Math.min(indent+2,/^ */.exec(lines[at])[0].length)));at++;
    }
    items.push('<li>'+blocks(content,depth+1)+'</li>');
   }
   const start=ordered?Number.parseInt(first[2],10):1;
   html.push('<'+tag+(ordered&&start!==1?' start="'+start+'"':'')+'>'+items.join('')+'</'+tag+'>');continue;
  }
  const paragraph=[line];at++;
  while(at<lines.length&&!blockStart(lines,at))paragraph.push(lines[at++]);
  html.push('<p>'+inline(paragraph.join('\n'))+'</p>');
 }
 return html.join('');
}
export function renderMarkdown(value){return blocks(String(value??'').replace(/\r\n?/g,'\n').split('\n'));}