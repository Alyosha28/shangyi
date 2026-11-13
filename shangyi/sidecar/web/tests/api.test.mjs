import test from 'node:test';
import assert from 'node:assert/strict';
import {api} from '../js/api.js';

test('requests fail clearly for malformed success responses and network failures', async () => {
 const original=globalThis.fetch;
 try {
  globalThis.fetch=async()=>({ok:true,json:async()=>{throw new SyntaxError('html');}});
  await assert.rejects(api.health(),/内容无法读取/);
  globalThis.fetch=async()=>({ok:true,json:async()=>null});
  await assert.rejects(api.health(),/内容不完整/);
  globalThis.fetch=async()=>{throw new TypeError('Failed to fetch');};
  await assert.rejects(api.health(),/无法连接本地服务/);
 } finally {globalThis.fetch=original;}
});

test('a hanging submission times out without automatically sending it again', async () => {
 const original={fetch:globalThis.fetch,setTimeout:globalThis.setTimeout,clearTimeout:globalThis.clearTimeout};
 let expire,cleared=false,calls=0;
 try {
  globalThis.setTimeout=(fn,ms)=>{assert.equal(ms,15000);expire=fn;return 42;};
  globalThis.clearTimeout=id=>{assert.equal(id,42);cleared=true;};
  globalThis.fetch=async(path,opts)=>{calls++;return new Promise((resolve,reject)=>opts.signal.addEventListener('abort',()=>reject(new DOMException('Aborted','AbortError'))));};
  const request=api.startChat({text:'测试',message_id:'same-message-id'});
  expire();
  await assert.rejects(request,/响应超时.*保留/);
  assert.equal(calls,1);assert.equal(cleared,true);
 } finally {Object.assign(globalThis,original);}
});

test('solver allows the server to finish its 60 second computation deadline', async () => {
 const original={fetch:globalThis.fetch,setTimeout:globalThis.setTimeout,clearTimeout:globalThis.clearTimeout};
 try {
  globalThis.setTimeout=(fn,ms)=>{assert.equal(ms,70000);return 42;};
  globalThis.clearTimeout=()=>{};
  globalThis.fetch=async()=>({ok:true,json:async()=>({ok:true,result:{equilibria:[]}})});
  assert.deepEqual(await api.solve({spec:{}}),{ok:true,result:{equilibria:[]}});
 } finally {Object.assign(globalThis,original);}
});
