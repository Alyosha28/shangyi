/**
 * 商奕工作台的 Host 半。
 *
 * 这一层不提供任何服务：案例数据由 `dsh-plugin-shangyi` 的只读路由
 * （/api/shangyi/cases、/api/shangyi/cards）提供，工作台只做浏览器半的渲染。
 * 保留空 apply() 是因为 DSH 的 bundle 契约要求每行有 Host 半。
 */
export function apply() {}
