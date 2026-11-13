// @ts-check
// API 客户端：全部走同源 sidecar（web_console.py）。
// 轮询由视图自己管理（stopPoll 由路由切换时调用），API 层只提供单次请求。

async function req(path, opts, timeout = 15000) {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeout);
  try {
    const r = await fetch(path, {...opts, signal:controller.signal});
    let body;
    try { body = await r.json(); }
    catch { throw new Error('服务返回的内容无法读取，请重新尝试。'); }
    if (!r.ok || body?.ok === false) {
      const error = /** @type {Error & {status?:number}} */ (new Error(body?.error || `请求失败（HTTP ${r.status}）`));
      error.status = r.status;
      throw error;
    }
    if (!body || typeof body !== 'object' || Array.isArray(body)) {
      throw new Error('服务返回的内容不完整，请重新尝试。');
    }
    return body;
  } catch (error) {
    if (controller.signal.aborted) throw new Error('服务响应超时。内容会保留，请检查连接后重试。');
    if (error instanceof TypeError) throw new Error('暂时无法连接本地服务。内容会保留，服务恢复后可以重试。');
    throw error;
  } finally {
    clearTimeout(timer);
  }
}

export const api = {
  chats: () => req('/api/chats'),
  chat: (id) => req('/api/chats/' + encodeURIComponent(id)),
  startChat: (body) => req('/api/chats', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)}),
  chatMessage: (id, body) => req('/api/chats/' + encodeURIComponent(id) + '/message', {method:'POST',headers:{'Content-Type':'application/json'},body:JSON.stringify(body)}),
  stopChat: (id) => req('/api/chats/' + encodeURIComponent(id) + '/stop', {method:'POST'}),
  health: () => req("/api/health"),
  cases: () => req("/api/cases"),
  case: (date, id) => req(`/api/case/${date}/${encodeURIComponent(id)}`),
  library: () => req("/api/library"),
  playbooks: () => req("/api/playbooks"),
  poi: (payload) =>
    req("/api/poi", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    }, 60000),
  models: () => req("/api/models"),
  model: (id) => req("/api/models/" + encodeURIComponent(id)),
  saveModel: (payload) =>
    req("/api/models", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    }, 70000),
  robustness: (payload) =>
    req("/api/robustness", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    }, 70000),
  model: (payload) =>
    req("/api/model", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    }),
  solve: (payload) =>
    req("/api/solve", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    }, 70000),
  run: (payload) =>
    req("/api/run", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify(payload),
    }),
  runs: () => req("/api/runs"),
  stopRun: (id) => req(`/api/runs/${encodeURIComponent(id)}/stop`, { method: "POST" }),
};

// ── 轮询助手 ────────────────────────────────────────────────────────────────
// 视图 mount 时 start(fn, ms) 拿到句柄；路由切换统一 stop，防泄漏。
const timers = new Set();

export function startPoll(fn, ms = 2000) {
  const t = setInterval(() => {
    fn().catch(() => {}); // 网络抖动不打断界面；下一轮自然重试
  }, ms);
  timers.add(t);
  return t;
}

export function stopPoll(t) {
  clearInterval(t);
  timers.delete(t);
}

export function stopAllPolls() {
  for (const t of timers) clearInterval(t);
  timers.clear();
}
