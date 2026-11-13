import test from 'node:test';
import assert from 'node:assert/strict';
import {renderRequest,mergeChoiceDraft} from '../js/views/chat.js';

test('renders the actual model questions without a fixed questionnaire counter',()=>{
 const html=renderRequest({kind:'question',payload:{questions:[{id:'dynamic-goal',text:'现在最想保住的是什么？'}]}});
 assert.ok(html.includes('现在最想保住的是什么？'));
 assert.ok(!html.includes('1 / 5'));
 assert.ok(html.includes('可以一起答'));
});
test('checkpoint facts and sources are escaped, never interpreted as page instructions',()=>{
 const html=renderRequest({kind:'checkpoint',payload:{items:[{content:'<script>bad()</script>',source:'<img onerror=bad()>',proposed_value:'0'}]}});
 assert.ok(!html.includes('<script>'));assert.ok(!html.includes('<img onerror'));
 assert.ok(html.includes('等你定：0'));
 assert.ok(html.includes('&lt;script&gt;'));
});
test('rewind proposals explain their effect before approval',()=>{
 const html=renderRequest({kind:'rewind',payload:{target:'intel',reason:'用户补充了新的竞争者'}});
 assert.ok(html.includes('用户补充了新的竞争者'));
 assert.ok(html.includes('后面的结果要重新生成'));
});
test('answer chips keep freeform text and replace only their own question',()=>{
 const q1={text:'想开什么店？'},q2={text:'预算是多少？'};
 let draft=mergeChoiceDraft('我已经看过两处铺位\n\n还没定位置',q1,'咖啡店');
 draft=mergeChoiceDraft(draft,q2,'20万元');
 draft=mergeChoiceDraft(draft,q1,'面包店');
 assert.match(draft,/我已经看过两处铺位/);
 assert.match(draft,/铺位\n\n还没定位置/);
 assert.match(draft,/想开什么店？：面包店/);
 assert.match(draft,/预算是多少？：20万元/);
 assert.ok(!draft.includes('咖啡店'));
});
