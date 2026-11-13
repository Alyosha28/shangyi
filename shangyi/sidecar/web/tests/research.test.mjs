import test from 'node:test';
import assert from 'node:assert/strict';
import {renderResearch} from '../js/research.js';
function cost(scenario,overrides={}){
 return {scenario,currency:'CNY',listed_upfront_cash:30000,listed_upfront_cash_expense:20000,listed_refundable_deposits:10000,
  listed_monthly_fixed_cost:15000,listed_upfront_opportunity_cost:5000,listed_monthly_opportunity_cost:1000,
  listed_upfront_noncash_expense:0,listed_monthly_noncash_expense:2000,costs_complete:false,cash_classification_complete:true,
  items:[{name:'月租',treatment:'cash_expense',normalized_amount:15000,normalized_period:'month',original:{name:'月租',amount:15000,period:'month',basis:'assumption',source:'待确认报价'}}],...overrides};
}
test('research quotes are escaped and unsafe source schemes are rejected',()=>{
 const html=renderResearch({research:[{kind:'rental',url:'javascript:alert(1)',search_scope:'街区',fields:{rent:{value:'<img src=x>',quote:'<script>x</script>'}}}]});
 assert.ok(!html.includes('<script>'));assert.ok(!html.includes('href="javascript:'));assert.match(html,/发布时间：未知/);
});
test('incomplete cost estimates keep the partial-cost boundary visible',()=>{
 const html=renderResearch({business_cost:{currency:'CNY',listed_upfront_cash:20000,listed_monthly_fixed_cost:5000,costs_complete:false}});
 assert.match(html,/未列费用不视为零/);assert.match(html,/条件计算/);
});
test('older evidence is labeled historical and repeated sources stay visible',()=>{
 const html=renderResearch({research:[{kind:'competitor',url:'https://example.org/first',source_urls:['https://example.org/first','https://example.org/second'],search_scope:'示例街',published_at:'2023-06-12',freshness_review:'older_than_review_window',fields:{name:{value:'甲店',quote:'甲店位于示例街'}}}]});
 assert.match(html,/不能当作当前租金或营业状态/);
 assert.match(html,/同项其他网址/);
 assert.match(html,/example.org\/second/);
});
test('cost details show readable line items instead of a raw data dump',()=>{
 const html=renderResearch({business_cost:cost('A店',{break_even:{monthly_revenue:30000,daily_orders:20,basis:'listed_costs_only_lower_bound'}})});
 assert.match(html,/月租/);
 assert.match(html,/待确认报价/);
 assert.match(html,/实际门槛可能更高/);
 assert.ok(!html.includes('"costs_complete"'));
});
test('distinct scenarios remain visible and same-scenario reruns replace only that card',()=>{
 const html=renderResearch({business_costs:[cost('A店',{listed_upfront_cash:11111}),cost('B店',{listed_upfront_cash:22222}),cost('A店',{listed_upfront_cash:33333})]});
 assert.equal((html.match(/<section class="cost-card">/g)||[]).length,2);
 assert.match(html,/<h3>A店<\/h3>/);assert.match(html,/<h3>B店<\/h3>/);
 assert.match(html,/33,333/);assert.match(html,/22,222/);assert.ok(!html.includes('11,111'));
});
test('cash expenses deposits and opportunity costs are visibly separate',()=>{
 const html=renderResearch({business_cost:cost('<备选店>',{cash_after_listed_upfront:70000})});
 assert.match(html,/&lt;备选店&gt;/);
 assert.match(html,/一次性现金花费/);assert.match(html,/能退的押金（先占钱）/);
 assert.match(html,/一次性让出的机会（不掏现金）/);assert.match(html,/每月让出的机会（不掏现金）/);
 assert.ok(!html.includes('一次性不掏钱的费用'));assert.match(html,/每月不掏钱的费用/);
 assert.match(html,/只扣了先掏的现金和押金，剩：70,000/);
 assert.ok(!html.includes('旧版计算'));
});
test('legacy results retain the record but do not present unreliable cash balance or runway',()=>{
 const html=renderResearch({business_cost:{listed_upfront_cash:98000,listed_monthly_fixed_cost:5000,
  cash_after_listed_upfront:123456,cash_scenario:{monthly_burn:1000,runway_months:123},
  break_even:{monthly_revenue:543210,daily_orders:20}}});
 assert.match(html,/老版没分清现金和让出的机会，要重算/);
 assert.match(html,/老版：先掏的钱合计/);assert.match(html,/98,000/);
 assert.ok(!html.includes('123,456'));assert.ok(!html.includes('543,210'));assert.ok(!html.includes('可支撑约'));
});
test('unclassified costs stay visible and cannot yield a cash sufficiency or runway claim',()=>{
 const html=renderResearch({business_cost:cost('费用待核对',{
  cash_classification_complete:false,cash_after_listed_upfront:70000,cash_scenario:{monthly_burn:1,runway_months:70000},
  items:[{name:'待确认费用',treatment:'unknown',normalized_amount:12000,normalized_period:'once',original:{amount:12000,period:'once'}}]})});
 assert.match(html,/仍有费用性质待确认/);assert.match(html,/不能据此判断资金足够/);
 assert.match(html,/待确认费用 · 性质待确认/);assert.match(html,/12,000/);
 assert.match(html,/未计入性质待确认的费用/);assert.ok(!html.includes('可支撑约'));
 assert.ok(!html.includes('剩余现金约可覆盖'));
 assert.ok(!html.includes('旧版计算'));
});
test('cash gains and flat cash are not described as negative consumption or a zero runway',()=>{
 for(const burn of [-5000,0]){
  const html=renderResearch({business_cost:cost('收入情景',{cash_scenario:{monthly_burn:burn,runway_months:null,cash_after_upfront:70000,status:'no_depletion_in_this_scenario'}})});
  assert.match(html,burn<0?/每月现金净增加：5,000/:/每月现金收支持平/);
  assert.match(html,/没有逐月消耗现金/);assert.match(html,/不代表实际可经营月数/);
  assert.ok(!html.includes('现金消耗：-'));assert.ok(!html.includes('覆盖 0 个月'));
 }
});
test('upfront cash shortfall is explicit and not presented as zero months of operation',()=>{
 const html=renderResearch({business_cost:cost('现金不足',{cash_after_listed_upfront:-8000,
  cash_scenario:{monthly_burn:2000,runway_months:0,cash_after_upfront:-8000,status:'insufficient_upfront_cash'}})});
 assert.match(html,/前期资金不足/);assert.match(html,/超过手头的现金 8,000/);
 assert.match(html,/不能据此给出经营可持续月数/);assert.ok(!html.includes('覆盖 0 个月'));
 const withoutSales=renderResearch({business_cost:cost('未提供收入',{cash_after_listed_upfront:-8000})});
 assert.match(withoutSales,/前期资金不足/);assert.match(withoutSales,/超过手头的现金 8,000/);
 assert.ok(!withoutSales.includes('可覆盖'));
});
test('cash depletion remains a conditional listed-cost calculation and null is never zero',()=>{
 const html=renderResearch({business_cost:cost('部分费用情景',{cash_scenario:{monthly_burn:4000,runway_months:18,cash_after_upfront:72000,status:'cash_depletes'}})});
 assert.match(html,/仅按所列收入和费用，剩余现金约可覆盖 18 个月/);
 assert.match(html,/未列费用不视为零/);assert.match(html,/未包含未列费用、收入变化和收付款时间差/);
 const unavailable=renderResearch({business_cost:cost('缺失续航',{cash_scenario:{monthly_burn:4000,runway_months:null,cash_after_upfront:72000}})});
 assert.match(unavailable,/未取得可计算的现金耗尽月数/);assert.ok(!unavailable.includes('覆盖 0 个月'));
});
test('absent cost categories are not presented as known zero but explicit zero inputs remain visible',()=>{
 const emptyCategories={listed_upfront_cash:0,listed_upfront_cash_expense:0,listed_refundable_deposits:0,
  listed_upfront_opportunity_cost:0,listed_monthly_opportunity_cost:0,listed_upfront_noncash_expense:0,listed_monthly_noncash_expense:0};
 const onlyRent=renderResearch({business_cost:cost('仅列租金',emptyCategories)});
 assert.equal((onlyRent.match(/class="cost-total"/g)||[]).length,2);
 assert.match(onlyRent,/先掏的现金和押金/);assert.match(onlyRent,/每月固定要掏的现金/);
 assert.ok(!onlyRent.includes('一次性让出的机会'));assert.ok(!onlyRent.includes('每月不掏钱的费用'));
 const zeroInput=renderResearch({business_cost:cost('明确不收押金',{...emptyCategories,
  items:[{name:'已确认押金为零',treatment:'refundable_deposit',normalized_amount:0,normalized_period:'once',original:{amount:0,period:'once',basis:'user_provided',source:'用户明确提供零押金'}}]})});
 assert.match(zeroInput,/其中：能退的押金（先占钱）/);
 assert.match(zeroInput,/已确认押金为零/);assert.match(zeroInput,/用户明确提供零押金/);
 assert.equal((zeroInput.match(/class="cost-total"/g)||[]).length,3);
});
test('null cost amounts remain unknown and known treatment does not become unknown treatment',()=>{
 const unknown={name:'装修报价待取得',treatment:'cash_expense',normalized_amount:null,normalized_period:'once',
  missing_amount_reason:'需取得装修方报价 <未取得>',original:{amount:null,period:'once',basis:'user_provided',source:'用户尚未取得报价'}};
 const html=renderResearch({business_cost:cost('装修费用待核对',{
  listed_upfront_cash:0,listed_upfront_cash_expense:0,listed_refundable_deposits:0,
  amounts_complete:false,cash_classification_complete:false,cash_after_listed_upfront:150000,
  items:[unknown],missing_amount_items:[unknown],unclassified_items:[unknown],
  break_even:{monthly_revenue:543210,daily_orders:222},cash_scenario:{monthly_burn:5000,runway_months:30}})});
 assert.match(html,/装修报价待取得 · 现金花费/);assert.match(html,/金额未取得 \/ 一次性/);
 assert.match(html,/先掏的现金和押金（金额未齐）<\/span><strong>金额未取得/);
 assert.match(html,/其中：一次性现金花费（金额未齐）<\/span><strong>金额未取得/);
 assert.match(html,/仍有费用金额未取得/);assert.ok(!html.includes('仍有费用性质待确认'));
 assert.match(html,/未知金额不是零/);assert.match(html,/不能视为完整现金余额/);
 assert.match(html,/金额说明：需取得装修方报价 &lt;未取得&gt;/);
 assert.ok(!html.includes('543,210'));assert.ok(!html.includes('现金约可覆盖'));assert.ok(!html.includes('旧版计算'));
});
test('mixed known and unknown amounts visibly limit subtotals and suppress complete conclusions',()=>{
 const known={name:'已知装修',treatment:'cash_expense',normalized_amount:60000,normalized_period:'once',original:{amount:60000,period:'once'}};
 const unknown={name:'施工追加费待报价',treatment:'cash_expense',normalized_amount:null,normalized_period:'once',original:{amount:null,period:'once'}};
 const unknownType={name:'费用性质待确认',treatment:'unknown',normalized_amount:3000,normalized_period:'once',original:{amount:3000,period:'once'}};
 const html=renderResearch({business_cost:cost('费用未齐',{
  listed_upfront_cash:60000,listed_upfront_cash_expense:60000,items:[known,unknown,unknownType],
  amounts_complete:false,cash_classification_complete:true,costs_complete:true,
  break_even:{monthly_revenue:543210},cash_scenario:{monthly_burn:1000,runway_months:60}})});
 assert.match(html,/先掏的现金和押金（仅已知部分）<\/span><strong>60,000/);
 assert.match(html,/其中：一次性现金花费（仅已知部分）<\/span><strong>60,000/);
 assert.match(html,/仍有费用金额未取得/);assert.match(html,/仍有费用性质待确认/);
 assert.ok(!html.includes('按输入声明的完整固定成本'));assert.ok(!html.includes('543,210'));assert.ok(!html.includes('现金约可覆盖'));
});

test('stable scenario identity replaces renamed results without merging distinct same-name options',()=>{
 const html=renderResearch({business_costs:[
  {scenario_id:'move',scenario:'旧搬店名称',currency:'CNY'},
  {scenario_id:'stay',scenario:'新名称',currency:'CNY'},
  {scenario_id:'move',scenario:'新名称',currency:'CNY'}
 ]});
 assert.ok(!html.includes('旧搬店名称'));
 assert.equal((html.match(/<h3>新名称<\/h3>/g)||[]).length,2);
});

test('duration totals are separate from upfront cash and retain incomplete cost boundaries',()=>{
 const html=renderResearch({business_cost:{scenario_id:'move',scenario:'搬店',currency:'CNY',cash_classification_complete:true,costs_complete:false,
  items:[{treatment:'cash_expense',normalized_amount:60000,normalized_period:'once',original:{amount:60000,period:'once'}}],
  listed_upfront_cash:78000,listed_upfront_cash_expense:60000,listed_monthly_fixed_cost:9000,cash_after_listed_upfront:72000,
  horizon_totals:{months:24,listed_cash_expense:276000,listed_cash_outflow_before_deposit_return:294000,listed_opportunity_cost:20000,complete:false}}});
 for(const value of ['78,000','72,000','276,000','294,000','20,000'])assert.ok(html.includes(value));
 assert.match(html,/24 个月·列出来的钱累计/);
 assert.match(html,/不是开始时一次付清/);
 assert.match(html,/未知项未按零计算/);
});

test('duration totals never present entirely unknown cash expenses as a known zero',()=>{
 const html=renderResearch({business_cost:{scenario:'租金待确认',currency:'CNY',cash_classification_complete:false,amounts_complete:false,costs_complete:false,
  items:[{treatment:'cash_expense',normalized_amount:null,normalized_period:'month',original:{amount:null,period:'month'}}],
  horizon_totals:{months:24,listed_cash_expense:0,listed_cash_outflow_before_deposit_return:0,complete:false}}});
 assert.match(html,/现金花费（金额未齐）<\/span><strong>金额未取得/);
 assert.match(html,/连押金一起掏的（金额未齐）<\/span><strong>金额未取得/);
});

test('web sources distinguish read snapshots from search leads and later fetch failures',()=>{
 const html=renderResearch({web_sources:[
  {url:'https://example.com/official',title:'官方页面',status:'body_retrieved',retrieved_at_unix:100,cached:true,last_attempt_failed:true},
  {url:'https://example.com/lead',title:'尚未核验',status:'search_only'},
  {url:'javascript:bad()',title:'不安全',status:'body_retrieved'}
 ]});
 assert.match(html,/已读 1 个页面/);assert.match(html,/复用本次会话快照/);
 assert.match(html,/最近一次重新读取未成功/);assert.match(html,/读取时间不是文章发布日期/);
 assert.match(html,/尚未取得正文的线索（1）/);assert.match(html,/只有搜索摘要/);
 assert.ok(!html.includes('javascript:'));assert.ok(html.includes('href="https://example.com/official"'));
});
test('source dates describe retrieval and older cache use without inventing a publication date',()=>{
 const html=renderResearch({web_sources:[
  {url:'https://example.com/official',title:'官方页面',status:'body_retrieved',retrieved_at_unix:200,last_replayed_snapshot_at_unix:100,last_attempt_failed:true},
  {url:'https://user:secret@example.com/private',status:'body_retrieved'},
  {url:' https://example.com/space',status:'body_retrieved'}
 ]});
 assert.match(html,/已读 1 个页面/);assert.match(html,/读取于/);
 assert.match(html,/最近一次调用复用了较早的页面快照/);assert.match(html,/上方保留较新的已读记录/);
 assert.match(html,/最近一次重新读取未成功/);assert.match(html,/读取时间不是文章发布日期/);
 assert.ok(!html.includes('secret'));assert.ok(!html.includes('space'));
 assert.ok(!html.includes('发布时间：'));
});
