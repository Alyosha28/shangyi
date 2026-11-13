import test from 'node:test';
import assert from 'node:assert/strict';
import {buildSpecFromState,explainSolution,PRESETS,FEELINGS,renameChoiceInState,pendingExamplesIn,examplesNoticeIn,freshExampleState,validDraft,parseDraft,stateFromDraft,draftJson,applyDraft,currentState,validateChoiceName,unknownCells,incompleteNotice,stateModified,overwriteScope} from '../js/views/playground.js';

const situation={p1:{name:'我的店',strategies:['维持价','降价']},p2:{name:'对手',strategies:['维持价','降价']},cells:{
 '维持价,维持价':[10,10],'维持价,降价':[2,12],'降价,维持价':[12,2],'降价,降价':[4,4]
}};
test('plain inputs preserve the exact two-player solver contract after renaming',()=>{
 const spec=buildSpecFromState(situation);
 assert.equal(spec.__complete,true);
 assert.deepEqual(spec.players.map(p=>p.id),['我的店','对手']);
 assert.equal(spec.payoffs.values.cells['维持价,降价']['我的店'].value,2);
 assert.equal(spec.payoffs.values.cells['维持价,降价']['对手'].value,12);
 assert.ok(!spec.payoffs.values.cells['维持价,降价']['本企业']);
});
test('unknown or nonfinite scenario scores never become solver input',()=>{
 const state=structuredClone(situation);state.cells['维持价,降价']=['','1e309'];
 const spec=buildSpecFromState(state);
 assert.equal(spec.__complete,false);
 assert.equal(spec.payoffs.values.cells['维持价,降价'],undefined);
});
test('every visible example can be answered with ordinary good-or-bad choices',()=>{
 const levels=new Set(FEELINGS.map(x=>x.value));
 assert.deepEqual(FEELINGS.map(x=>x.label),['很吃亏','有点吃亏','差不多','有点划算','很划算']);
 for(const id of ['price_war','entry']){
  const preset=PRESETS[id];
  assert.equal(buildSpecFromState(preset).__complete,true);
  for(const pair of Object.values(preset.cells))for(const value of pair)assert.ok(levels.has(value),`${id}: ${value}`);
 }
});
test('changing an everyday choice keeps its selected good-or-bad outcomes',()=>{
 const state=structuredClone(PRESETS.price_war);
 const before=state.cells['保持原价,降价'];
 renameChoiceInState(state,'1',0,'先观察');
 assert.deepEqual(state.cells['先观察,降价'],before);
 assert.equal(state.cells['保持原价,降价'],undefined);
 assert.equal(buildSpecFromState(state).__complete,true);
 renameChoiceInState(state,'2',1,'再降价');
 assert.deepEqual(state.cells['先观察,再降价'],before);
 assert.equal(buildSpecFromState(state).__complete,true);
});
test('a fixed result is explained as a conditional combination, not a prediction',()=>{
 const html=explainSolution({equilibria:[{id:'eq1',kind:'Pure',support:[[1],[1]]}],completeness:'Complete'},situation.p1,situation.p2,[]);
 assert.match(html,/我的店选择<strong>降价<\/strong>/);
 assert.match(html,/不知道对方真正会做什么/);
 assert.ok(!html.includes('支撑集'));
});
test('screened-out results are not shown as surviving combinations',()=>{
 const html=explainSolution({equilibria:[
  {id:'eq1',kind:'Pure',support:[[0],[0]]},
  {id:'eq2',kind:'Pure',support:[[1],[1]]}
 ],refinement:{survivors:['eq2']},completeness:'Complete'},situation.p1,situation.p2,[]);
 assert.match(html,/有 1 种做法可以继续比较/);
 assert.match(html,/选择<strong>降价<\/strong>/);
 assert.ok(!html.includes('选择<strong>维持价</strong>'));
});
test('alternating choices are described without implying a real-world forecast',()=>{
 const html=explainSolution({equilibria:[{id:'eqm',kind:'Mixed',support:[[0,1],[0,1]]}]},situation.p1,situation.p2,[]);
 assert.match(html,/有时这样、有时那样/);
 assert.match(html,/不是在说现实中各做法会出现多少次/);
});

// ── UI-009：示例的好坏不得被说成用户自己的判断 ──────────────────────────────
// 复现实况：用户按页面顺序把降价示例改成自己的情况（只改做法名称），
// 示例的 1,1,-1,2,2,-1,0,0 原样参与计算，结论却写「只看你填的好坏」
// 「这些好坏是你自己选的」。修法不是把数值清空（那会毁掉教学入口），
// 而是逐格记住来源：没核对过就不能算作用户的判断。

test('刚点开的示例，每一格好坏都还是示例判断',()=>{
 const s=freshExampleState(PRESETS.price_war);
 const pending=pendingExamplesIn(s);
 assert.equal(pending.length,4);
 assert.deepEqual(pending.map(x=>x.label),['保持原价／保持原价','保持原价／降价','跟着降价／保持原价','跟着降价／降价']);
});

test('把示例改成自己的情况后，示例判断仍然被标为未核对',()=>{
 const s=freshExampleState(PRESETS.price_war);
 renameChoiceInState(s,'1',0,'继续租店');
 renameChoiceInState(s,'2',0,'维持租金');
 const pending=pendingExamplesIn(s);
 assert.equal(pending.length,4,'继承数值可以，但继承来的判断必须仍算示例的');
 assert.deepEqual(pending.map(x=>x.label),['继续租店／维持租金','继续租店／降价','跟着降价／维持租金','跟着降价／降价']);
 // 数值确实跟着搬了过去（这正是用户看到的「结果变了」），来源也必须跟着搬
 assert.deepEqual(s.cells['继续租店,维持租金'],[1,1]);
 assert.deepEqual(s.origin['继续租店,维持租金'],[true,true]);
});

test('来源在改名后不会被洗掉：数值搬家，来源一起搬家',()=>{
 const s=freshExampleState(PRESETS.price_war);
 s.origin['保持原价,保持原价']=[false,false]; // 用户已核对过这一格
 renameChoiceInState(s,'1',0,'继续租店');
 renameChoiceInState(s,'2',0,'维持租金');
 assert.deepEqual(s.origin['继续租店,维持租金'],[false,false],'核对过的不能改名后变回示例');
 assert.deepEqual(s.origin['跟着降价,降价'],[true,true]);
});

test('提示条逐个核对递减，全部核对后消失',()=>{
 const s=freshExampleState(PRESETS.price_war);
 assert.match(examplesNoticeIn(s),/还有 <strong>4<\/strong> 项好坏是例子里的/);
 s.origin['保持原价,保持原价']=[false,false];
 assert.match(examplesNoticeIn(s),/还有 <strong>3<\/strong> 项好坏是例子里的/);
 for(const k of Object.keys(s.origin))s.origin[k]=[false,false];
 assert.equal(examplesNoticeIn(s),'');
});

test('结论在核对前不把示例判断说成用户自己的判断',()=>{
 const s=freshExampleState(PRESETS.price_war);
 const html=explainSolution({equilibria:[{id:'e',kind:'Pure',support:[[1],[1]]}],completeness:'Complete'},s.p1,s.p2,pendingExamplesIn(s));
 assert.match(html,/按现在表里的好坏/);
 assert.match(html,/还有 4 项好坏是例子里的，你没有核对过/);
 assert.match(html,/不要把这当成你自己条件下的比较/);
 assert.ok(!html.includes('这些好坏是你自己'),'未核对之前不得声称判断来自用户');
 const done=explainSolution({equilibria:[{id:'e',kind:'Pure',support:[[1],[1]]}],completeness:'Complete'},s.p1,s.p2,[]);
 assert.match(done,/只看你核对过的好坏/);
 assert.match(done,/这些好坏是你自己核对过的/);
 assert.ok(!done.includes('例子里的'));
});

test('拿不出来历时不声称判断属于用户，也不编一个数目',()=>{
 const s=freshExampleState(PRESETS.price_war);
 const html=explainSolution({equilibria:[{id:'e',kind:'Pure',support:[[1],[1]]}],completeness:'Complete'},s.p1,s.p2);
 assert.match(html,/这些好坏的来源没能确定/);
 assert.match(html,/不要把这当成你自己条件下的比较/);
 assert.ok(!html.includes('这些好坏是你自己'));
 assert.ok(!/\d+ 项好坏是例子里的/.test(html),'没取到来源就不能报一个具体条数');
});


// ── UI-002：刷新不该让用户重填一遍 ─────────────────────────────────────────
// 原形态：state 只在模块内存里，一刷新回默认示例，改过的名称/做法/好坏全丢，
// 且没有任何提示。验收三条：刷新后恢复全部输入；明确告知结果要重算；
// 存不进去时给出可读提示并保留当前编辑。

test('合法草稿完整恢复：名称、做法、好坏、来源、计算设置都在',()=>{
 // 按真实路径改：先改名（好坏跟着一起搬），再标一格已核对
 const draft=freshExampleState(PRESETS.price_war);
 draft.p1.name='我方';
 renameChoiceInState(draft,'1',0,'暂时维持售价');
 draft.origin['暂时维持售价,降价']=[false,true];
 draft.refineDen=12;
 const restored=stateFromDraft(draft);
 assert.ok(restored,'合法草稿必须能恢复');
 assert.equal(restored.p1.name,'我方');
 assert.deepEqual(restored.p1.strategies,['暂时维持售价','跟着降价']);
 assert.deepEqual(restored.cells['暂时维持售价,降价'],[-1,2]);
 assert.deepEqual(restored.origin['暂时维持售价,降价'],[false,true]);
 assert.equal(restored.refineDen,12);
 // 恢复出来仍然能直接参与比较（不会因为换个名字就少一格）
 assert.equal(buildSpecFromState(restored).__complete,true);
});

test('存进去的再读回来还对得上（round-trip）',()=>{
 const draft=structuredClone(freshExampleState(PRESETS.entry));
 draft.p2.strategies[1]='照常开门';
 const again=parseDraft(draftJson(draft));
 assert.deepEqual(stateFromDraft(again),stateFromDraft(draft));
});

test('形状不对的草稿一律拒绝，不猜不补',()=>{
 const base=()=>({p1:{name:'我',strategies:['a','b']},p2:{name:'对手',strategies:['a','b']},cells:{}});
 for(const bad of [
  null, undefined, 42, 'x', [], {},
  {...base(),p1:undefined},
  {...base(),p1:{name:'',strategies:['a','b']}},
  {...base(),p1:{name:'我',strategies:['a']}},
  {...base(),p1:{name:'我',strategies:['a','']}},
  {...base(),cells:null},
  {...base(),origin:'nope'},
  {...base(),method:'no_such_method'},
  {...base(),refine:'no_such_refine'},
 ]){
  assert.equal(validDraft(bad),false,JSON.stringify(bad));
  assert.equal(stateFromDraft(bad),null,JSON.stringify(bad));
 }
});

test('坏 JSON、空串、非字符串都当没有草稿',()=>{
 for(const raw of [null,undefined,'','{oops','[1,2', 42]){
  assert.equal(parseDraft(raw),null,String(raw));
 }
 assert.deepEqual(parseDraft('{"a":1}'),{a:1});
});

test('恢复后的计算档位被夹在合法区间，免得把怪值传给求解器',()=>{
 const base=()=>({p1:{name:'我',strategies:['a','b']},p2:{name:'对手',strategies:['a','b']},cells:{}});
 assert.equal(stateFromDraft({...base(),refineDen:9999}).refineDen,64,'超界夹到上限');
 assert.equal(stateFromDraft({...base(),refineDen:0}).refineDen,2,'低于下限夹到 2');
 assert.equal(stateFromDraft({...base(),refineDen:'abc'}).refineDen,6,'非数字回落默认档');
 assert.equal(stateFromDraft(base()).method,'support_enumeration');
});

test('applyDraft 形状不对时不碰现有状态',()=>{
 assert.equal(applyDraft(structuredClone(freshExampleState(PRESETS.price_war))),true);
 const before=buildSpecFromState(currentState()).__complete;
 assert.equal(applyDraft({}),false,'坏草稿不得清掉用户已经填好的东西');
 assert.equal(buildSpecFromState(currentState()).__complete,before);
 assert.equal(currentState().p1.strategies[0],'保持原价');
});

// ── UI-007：逗号与重名分开判定、分开提示 ──────────────────────────────────
// 原来合成一个分支：写「保持原价,送优惠券」被回复「同一边的做法不能重名」，
// 用户照着"避免重复"去改还是被拒——真正的原因是英文逗号（矩阵 cell 的键分隔符）。
test('英文逗号有它自己的提示，并告诉用户怎么改', () => {
  const verdict = validateChoiceName({ name: '保持原价,送优惠券', others: ['跟着降价'] });
  assert.equal(verdict.ok, false);
  assert.equal(verdict.reason, 'comma');
  assert.match(verdict.message, /英文逗号/);
  assert.match(verdict.message, /中文逗号/, '要给出能照着做的改法');
});

test('重名有它自己的提示，与标点无关', () => {
  const verdict = validateChoiceName({ name: '跟着降价', others: ['跟着降价'] });
  assert.equal(verdict.ok, false);
  assert.equal(verdict.reason, 'duplicate');
  assert.match(verdict.message, /不能写成一样/);
  assert.ok(!verdict.message.includes('逗号'), '重名提示不该提逗号');
});

test('含中文逗号不算违规（用户被引导去用的就是中文逗号）', () => {
  const verdict = validateChoiceName({ name: '保持原价，送优惠券', others: ['跟着降价'] });
  assert.equal(verdict.ok, true);
  assert.equal(verdict.value, '保持原价，送优惠券');
});

test('空名称与前后空格各自处理', () => {
  assert.equal(validateChoiceName({ name: '   ', others: ['a', 'b'] }).reason, 'empty');
  assert.equal(validateChoiceName({ name: '  先观察  ', others: ['a', 'b'] }).ok, true);
  assert.equal(validateChoiceName({ name: '  先观察  ', others: ['a', 'b'] }).value, '先观察');
});

// ── UI-011：选「还不知道」后要点名是哪几种组合、哪一方，并能跳回去 ──────────
test('未填齐时列清楚是哪几种组合、哪一方还不知道', () => {
  const state = freshExampleState(PRESETS.price_war);
  state.cells['保持原价,降价'] = ['', 2];
  const unknown = unknownCells(state);
  assert.deepEqual(unknown.map(x => x.label), ['保持原价／降价']);
  assert.deepEqual(unknown[0].missing, [0]);
  assert.deepEqual(unknown[0].sides, ['我']);
  const notice = incompleteNotice(state);
  assert.match(notice, /还有 1 种情况没选完/);
  assert.match(notice, /保持原价／降价/);
  assert.match(notice, /我还不知道/);
  assert.match(notice, /#pg-cell-0-1/, '每条都要有跳回字段的锚点');
  assert.match(notice, /不替你猜/);
});

test('两侧都未知时把双方都列出来，已填过的不提', () => {
  const state = freshExampleState(PRESETS.price_war);
  state.cells['保持原价,降价'] = ['', ''];
  state.cells['跟着降价,降价'] = ['', ''];
  const notice = incompleteNotice(state);
  assert.match(notice, /还有 2 种情况没选完/);
  assert.match(notice, /我、另一家店还不知道/);
  assert.ok(!notice.includes('保持原价／保持原价'), '已填齐的格子不该被点名');
});

test('全部填齐时没有提示', () => {
  const state = freshExampleState(PRESETS.price_war);
  assert.deepEqual(unknownCells(state), []);
  assert.equal(incompleteNotice(state), '');
});

test('摆局（数字）格子里空的一侧也算未知', () => {
  const state = freshExampleState(PRESETS.entry);
  state.cells['开店,打折揽客'] = [null, 0];
  const unknown = unknownCells(state);
  assert.deepEqual(unknown.map(x => x.label), ['开店／打折揽客']);
  assert.deepEqual(unknown[0].sides, ['我']);
});

// ── UI-003：切换示例前先问，且点当前示例不静默清空 ────────────────────────
test('刚点开的示例算「未修改」，切换不该被问', () => {
  const state = freshExampleState(PRESETS.price_war);
  assert.equal(stateModified(state, PRESETS.price_war), false);
  // 判「改过没有」跟**当前**示例比：没动过就没什么可覆盖的，问也是白问
  assert.equal(stateModified(state, PRESETS.price_war), false);
});

test('改过名称/做法/好坏/计算设置都算「已修改」，切换前必须问', () => {
  const renamed = freshExampleState(PRESETS.price_war);
  renamed.p1.name = '我方';
  assert.equal(stateModified(renamed, PRESETS.price_war), true);

  const moved = freshExampleState(PRESETS.price_war);
  renameChoiceInState(moved, '1', 0, '赠送饮品');
  assert.equal(stateModified(moved, PRESETS.price_war), true);

  const scored = freshExampleState(PRESETS.price_war);
  scored.cells['保持原价,保持原价'] = [2, 2];
  assert.equal(stateModified(scored, PRESETS.price_war), true);

  const tuned = freshExampleState(PRESETS.price_war);
  tuned.refineDen = 12;
  assert.equal(stateModified(tuned, PRESETS.price_war), true);

  const dropped = freshExampleState(PRESETS.price_war);
  delete dropped.cells['保持原价,降价'];
  assert.equal(stateModified(dropped, PRESETS.price_war), true, '删掉一格也算改过');
});

test('覆盖前说清楚会丢哪几项', () => {
  const state = freshExampleState(PRESETS.price_war);
  assert.deepEqual(overwriteScope(state, PRESETS.price_war), [], '没改过就没什么可覆盖');
  state.p1.name = '我方';
  renameChoiceInState(state, '1', 0, '赠送饮品');
  state.cells['赠送饮品,降价'] = [1, 1];
  const scope = overwriteScope(state, PRESETS.price_war);
  assert.ok(scope.includes('双方名称'), scope.join('、'));
  assert.ok(scope.includes('做法'));
  assert.ok(scope.includes('好坏选择'));
});
