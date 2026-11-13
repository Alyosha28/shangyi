// 商奕（ShangYi）工作台 —— 浏览器半（手写 bundle，零构建）。
//
// 形态：DSH 内核不变，本插件只做两件事：
//   1) 在 sidebar.panellist 注册一个「商奕」面板入口（id 同时是 main 的 key）；
//   2) 在 main keyed slot 注册该面板：案例 / 阶段 / 支付矩阵 / 均衡 / 轨迹。
// 对话面板仍是 main 的 'conversation' key，点侧栏「新会话」即可切回。
//
// 数据全部来自 dsh-plugin-shangyi 的只读路由（/api/shangyi/cases、/api/shangyi/cards），
// 前端只展示、不重算、不编造数字（商奕铁律 1）。
window.__ModuleLoader__.load({
  id: "dsh-plugin-shangyi-workbench",
  factory: function (require) {
    var React = require("react");
    var h = React.createElement;
