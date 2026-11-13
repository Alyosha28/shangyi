/**
 * 商奕前端外壳的 Host 半。
 *
 * 这一层不提供任何服务：品牌字标、空态主视觉、配色 token 全在浏览器半
 * （lib/client.js）里通过 slot 注册与 ctx.theme.overrideTokens 完成。
 * 保留空 apply() 是因为 DSH 的 bundle 契约要求每行有 Host 半。
 */
export function apply() {}
