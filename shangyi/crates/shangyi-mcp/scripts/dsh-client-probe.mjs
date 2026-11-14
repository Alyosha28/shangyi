#!/usr/bin/env node
// 用 **dsh 自己的 MCP 客户端 SDK** 驱动 `shangyi mcp-serve`（手工验证脚本，不进 cargo test）。
//
// 为什么值得有这样的脚本
// ----------------------
// dsh 的 `@deepseek-ai/dsh-mcp-client` 用的是 `@modelcontextprotocol/client`（本机实测
// 版本 2.2.0）。用同一个 SDK、同一个 stdio transport 连我们的服务端，可以在**不调模型**
// 的前提下验证客户端侧的整条链路：协议协商 → tools/list → 工具 schema 被 SDK 的
// zod 校验接受 → tools/call（含工具级 isError 与多段 content）→ 阶段推进 → 真实求解。
//
// 它**不能**替代的东西：dsh harness 层（profile 装载、工具注册进 ctx.tools、模型选择工具、
// 会话事件）仍然只有跑步进模型才能验；DeepSeek 余额为负时那条路无法验收。
//
// 用法
// ----
//   DSH_NODE_MODULES=<dsh>/node_modules \
//   SHANGYI_BIN=/abs/path/to/shangyi \
//   node scripts/dsh-client-probe.mjs [--home <dir>] [--auto-approve]
//
// 说明：Node 的 ESM 解析不看 NODE_PATH，故本脚本要求**运行目录下**能解析到
// `@modelcontextprotocol/client`。最省事的做法是在 /tmp 建一个探测目录：
//   mkdir -p /tmp/mcp-client-probe && ln -s "$DSH_NODE_MODULES" /tmp/mcp-client-probe/node_modules
//   cp <本文件> /tmp/mcp-client-probe/probe.mjs
//   cd /tmp/mcp-client-probe && node probe.mjs
//
// 断言（任一失败即非零退出）：
// 1. initialize 成功，protocolVersion 为 2025-11-25
// 2. tools/list 返回 39 个工具，且 solve_equilibria 的 inputSchema 被 SDK 接受
// 3. 未选范围时调 solve_equilibria → 工具级 isError=true（RPC 层不抛异常）
// 4. 完整链路（auto-approve）走到 solve，solve_equilibria 返回可解析的均衡 JSON
// 5. 阶段闸门的拒绝理由真的出现在工具结果里

import { Client } from "@modelcontextprotocol/client";
import { StdioClientTransport } from "@modelcontextprotocol/client/stdio";

const argv = process.argv.slice(2);
const flag = (name, fallback) => {
  const i = argv.indexOf(name);
  return i >= 0 && argv[i + 1] ? argv[i + 1] : fallback;
};
const BIN = process.env.SHANGYI_BIN ?? "shangyi";
const HOME = flag("--home", "/tmp/shangyi-mcp-probe-home");
const autoApprove = argv.includes("--auto-approve");

const failures = [];
const check = (ok, label, detail) => {
  console.log(`${ok ? "PASS" : "FAIL"}  ${label}${detail ? ` — ${detail}` : ""}`);
  if (!ok) failures.push(label);
};

const transport = new StdioClientTransport({
  command: BIN,
  args: ["mcp-serve", "--home", HOME, ...(autoApprove ? ["--auto-approve-checkpoints"] : [])],
  stderr: "inherit",
});
const client = new Client({ name: "dsh-mcp-client-probe", version: "0.0.0" });
await client.connect(transport);

const version = client.getNegotiatedProtocolVersion?.() ?? "(n/a)";
check(version === "2025-11-25", "协议协商", `protocolVersion=${version}`);
const caps = client.getServerCapabilities?.() ?? {};
check(Boolean(caps.tools), "capabilities.tools", JSON.stringify(caps));

const { tools } = await client.listTools();
// 39 = `ToolRegistry::standard()` 的 spec 数（含 `begin_followup_task`）
check(tools.length === 39, "tools/list 数量", `${tools.length}`);

const solveTool = tools.find((t) => t.name === "solve_equilibria");
check(Boolean(solveTool), "solve_equilibria 在清单里");
check(
  Boolean(solveTool?.inputSchema?.properties?.method?.enum),
  "inputSchema 结构被 SDK 保留（method.enum 可见）",
);

const rejected = await client.callTool({ name: "solve_equilibria", arguments: {} });
check(rejected.isError === true, "错阶段调用是工具级失败（不是 RPC 异常）");

const step = async (name, args) => {
  const r = await client.callTool({ name, arguments: args });
  const statusLine =
    (r.content ?? [])
      .map((c) => c.text)
      .join("\n")
      .split("\n")
      .filter((l) => l.startsWith("[session] 阶段推进") || l.startsWith("[session] 阶段未推进"))
      .pop() ?? "";
  console.log(`      ${name}: isError=${r.isError} ${statusLine.slice(0, 110)}`);
  return r;
};

await step("select_task_scope", { scope: "consultation", reason: "客户端 SDK 探测：完整咨询" });
await step("vn_map", {});
const profile = await step("participant_profile_update", {
  participant_id: "rival",
  value_net_role: "substitutor",
  relation_to_game: "player",
  common: {},
});
const profileText = (profile.content ?? []).map((c) => c.text).join("\n");
check(
  profileText.includes("cp1") || profileText.includes("checkpoint"),
  "闸门拒绝理由带进工具结果",
  profileText.match(/阶段未推进[^\n]{0,80}/)?.[0] ?? "",
);

await step("checkpoint", { items: [{ id: "i1", content: "直接竞对只有一家（rival）" }] });
const spec = {
  game_id: "sdk-probe",
  players: [
    { id: "us", name: "我方", vn_role: "focal", relation_to_game: "player" },
    { id: "rival", name: "对手", vn_role: "substitutor", relation_to_game: "player" },
  ],
  strategies: { us: ["维持", "降价"], rival: ["维持", "降价"] },
  payoffs: {
    form: "normal",
    values: {
      cells: {
        "维持,维持": { us: 3, rival: 3 },
        "维持,降价": { us: 0, rival: 5 },
        "降价,维持": { us: 5, rival: 0 },
        "降价,降价": { us: 1, rival: 1 },
      },
    },
  },
  params: {
    margin: { name: "margin", value: 3, source: { kind: "user_confirmed", checkpoint: "cp2" } },
  },
};
await step("gamespec_build", spec);
await step("checkpoint", {
  items: [{ id: "p1", content: "margin = 3", source: "用户口述毛利率区间", proposed_value: "3" }],
});

const solved = await step("solve_equilibria", {});
if (autoApprove) {
  check(solved.isError === false, "solve_equilibria 成功");
  let body = null;
  try {
    body = JSON.parse((solved.content ?? [])[0]?.text ?? "{}");
  } catch {
    /* 下面会断言 */
  }
  check(Array.isArray(body?.equilibria) && body.equilibria.length > 0, "解集 JSON 可解析", JSON.stringify(body?.equilibria?.[0] ?? null));
} else {
  // 没有 auto-approve 时应当停在 checkpoint（本服务端的默认语义：确认必须由真人做）
  check(solved.isError === true, "未启用自动结算时 solver 仍被拦截（阶段停在 model_build）");
}

await client.close();

if (failures.length > 0) {
  console.error(`\n${failures.length} 条断言失败：${failures.join(" / ")}`);
  process.exit(1);
}
console.log("\n全部断言通过：dsh 所用 MCP 客户端 SDK 成功驱动商奕服务端。");
process.exit(0);
