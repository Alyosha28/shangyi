import test from 'node:test';
import assert from 'node:assert/strict';
import {renderFrame} from '../js/frame.js';

const full = {
 question: '在上海南京路32号租间屋开喜茶，值不值？',
 type: '新生意', mode: '看可能', scope: '完整咨询',
 move: {side: '我方', value: '跟着降价10%', via: '你指定的'},
 players: [{name: '我方', side: '我方', moves: ['原价开店', '降价跟进']},
           {name: '商场物业', side: '对方', moves: ['强硬', '让步']}],
 facts: [{label: 'rent', value: '8万/月', source: 'https://example.com'}],
 assumptions: [{label: '月租（现金花费）', value: '15,000', via: '分析假设'}],
 gaps: [{label: '客流', why: '公开资料未取得'}],
 result: {equilibria: 2, completeness: '找全了', has_recommendation: true},
};

test('a complete frame renders every slot in plain language', () => {
 const html = renderFrame(full);
 assert.match(html, /机器这样理解你的问题/);
 assert.match(html, /在上海南京路32号租间屋开喜茶/);
 assert.match(html, /新生意/); assert.match(html, /看可能/);
 assert.match(html, /跟着降价10%<em>你指定的<\/em>/);
 assert.match(html, /我方（原价开店 \/ 降价跟进）/);
 assert.match(html, /商场物业（强硬 \/ 让步）/);
 assert.match(html, /挂的租金 8万\/月/, '字段键要翻成日常词');
 assert.match(html, /月租（现金花费） 15,000<em>分析假设<\/em>/);
 assert.match(html, /data-fill="客流：">客流<\/button><em>公开资料未取得<\/em>/);
 assert.match(html, /2 种稳的做法<em>找全了<\/em><em>已有建议<\/em>/);
});

test('empty slots say so instead of guessing', () => {
 const html = renderFrame({question: '要不要开第二家店', type: '新生意', mode: '看可能',
  move: {side: '我方', value: null, via: null}, players: [], facts: [], assumptions: [], gaps: [], result: null});
 assert.match(html, /还没定（分析里给）/);
 const whatif = renderFrame({mode: '试做法', move: {side: '我方', value: null, via: null}});
 assert.match(whatif, /存档里没单独记录/, '没记录不等于用户没指定');
 assert.ok(!whatif.includes('还没定'));
 assert.match(html, /还没摆局/);
 assert.match(html, /还没有有出处的数字/);
 assert.match(html, /还没算/);
 assert.ok(!html.includes('undefined'));
});

test('missing frame renders nothing rather than an empty shell', () => {
 assert.equal(renderFrame(null), '');
 assert.equal(renderFrame(undefined), '');
});

test('frame text is escaped and long lists are capped', () => {
 const html = renderFrame({question: '<img src=x onerror=bad()>', type: '<b>类型</b>',
  players: ['a','b','c','d','e','f'].map((name) => ({name, moves: ['<script>bad()</script>']}))});
 assert.ok(!html.includes('<img')); assert.ok(!html.includes('<script>'));
 assert.ok(!html.includes('<b>类型</b>'));
 assert.match(html, /等 6 项/);
});

test('partial completeness is never rendered as complete', () => {
 const html = renderFrame({result: {equilibria: 1, completeness: '可能没找全', has_recommendation: false}});
 assert.match(html, /可能没找全/);
 assert.ok(!html.includes('找全了'));
});

test('周边事实：计数、同品牌、未取得项都摆出来', async () => {
  const {renderPoi} = await import('../js/frame.js');
  const html = renderPoi([{address: '上海市黄浦区南京东路', radius: 500,
    source: '高德开放平台 Web 服务（POI 周边搜索）',
    counts: [{label: '茶饮咖啡', count: 144}], brand: {label: '喜茶（同品牌）', count: 3},
    unavailable: ['餐饮：CUQPS_HAS_EXCEEDED_THE_LIMIT']}]);
  assert.match(html, /周边事实（高德 POI）/);
  assert.match(html, /茶饮咖啡：144 家/);
  assert.match(html, /喜茶（同品牌）：3 家/);
  assert.match(html, /半径 500 米/);
  assert.match(html, /这次没取得的（1）/);
  assert.match(html, /不是营业额或客流/);
});

test('没有周边事实时不占位置', async () => {
  const {renderPoi} = await import('../js/frame.js');
  assert.equal(renderPoi([]), '');
  assert.equal(renderPoi(null), '');
  assert.equal(renderPoi([{address: '某地'}]), '');
});

test('周边事实里的文本被转义', async () => {
  const {renderPoi} = await import('../js/frame.js');
  const html = renderPoi([{address: '<img src=x>', radius: 500, counts: [{label: '<script>bad()</script>', count: 1}]}]);
  assert.ok(!html.includes('<img')); assert.ok(!html.includes('<script>'));
});
