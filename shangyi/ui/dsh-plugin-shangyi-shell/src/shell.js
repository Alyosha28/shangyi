// 商奕（ShangYi）前端外壳 —— DSH Web GUI 的浏览器半（手写 bundle，零构建）。
//
// 形态：DSH 内核 + 一层客户端插件。不改 app root、不动官方组件，只做三件事：
//   1) 换品牌位：sidebar.brand.mark / sidebar.brand.name / conversation.hero.brand.mark；
//   2) 换配色：ctx.theme.overrideTokens 叠加 alias token 层（107 个，可整体撤掉）；
//   3) 加一层商奕自己的纸面（shell.overlay，pointer-events:none）。
// 卸载本插件（或 bundle 补丁写 disabled:true）即完全回滚到 DSH 官方外观。
//
// 品牌图形的颜色一律读 --dsw-alias-* / --dsw-specific-*，不写死：
// 用户在「设置 → 通用」里切明暗时，印章与字标跟着走，不需要重渲染。
//
// 视觉方向用 URL 预览：?shangyi=paper|lacquer|ledger（默认 paper）。
// 方向一旦定稿，把 readVariant() 的默认值改掉即可，预览参数保留作对比工具。
window.__ModuleLoader__.load({
  id: "dsh-plugin-shangyi-shell",
  factory: function (require) {
    var React = require("react");
    var h = React.createElement;

    // ──────────────────────────────────────────────────────────────
    // 一、方向
    //    token 表由 scripts/gen-tokens.py 从 DSH 默认主题的实测快照推导，
    //    见 lib/tokens.gen.js（107 个 --dsw-alias-* × light/dark）。
    // ──────────────────────────────────────────────────────────────
    var VARIANTS = tokenLayers();
    var ORDER = ["paper", "lacquer", "ledger"];

    var FONT_SERIF = '"Songti SC", "Source Han Serif SC", "Noto Serif CJK SC", "STSong", serif';
    var FONT_SANS = '-apple-system, BlinkMacSystemFont, "PingFang SC", "Hiragino Sans GB", "Segoe UI", sans-serif';

    // 品牌图形只读 token：主色 → 印面，纸色 → 印文。
    var C_SEAL = "var(--dsw-alias-brand-primary)";
    var C_SEAL_INK = "var(--dsw-alias-bg-base)";
    var C_LINE = "var(--dsw-alias-brand-primary)";
    var C_LABEL = "var(--dsw-alias-label-primary)";
    var C_MUTED = "var(--dsw-alias-label-secondary)";

    // ──────────────────────────────────────────────────────────────
    // 二、品牌图形
    // ──────────────────────────────────────────────────────────────

    // 印章：方章 + 阴文「奕」。
    function Seal(props) {
      var size = props.size || 26;
      return h("svg", {
        width: size, height: size, viewBox: "0 0 32 32",
        "aria-hidden": true, style: { display: "block", flex: "none" }
      },
        h("rect", { x: 1, y: 1, width: 30, height: 30, rx: 2.5, fill: C_SEAL }),
        h("rect", { x: 3.5, y: 3.5, width: 25, height: 25, rx: 1.2, fill: "none", stroke: C_SEAL_INK, strokeOpacity: 0.4, strokeWidth: 0.75 }),
        h("text", {
          x: 16, y: 16.4, textAnchor: "middle", dominantBaseline: "central",
          fill: C_SEAL_INK, fontFamily: FONT_SERIF, fontSize: 17.5, fontWeight: 600
        }, "奕")
      );
    }

    // 空态主视觉：一枚放大的印 + 四分之一枰线（9 路的暗示，不画满棋盘）。
    function HeroMark(props) {
      var size = props.size || 34;
      return h("svg", {
        width: size, height: size, viewBox: "0 0 40 40",
        "aria-hidden": true, style: { display: "block", flex: "none" }
      },
        h("rect", { x: 0.5, y: 0.5, width: 39, height: 39, rx: 3, fill: "none", stroke: C_LINE, strokeOpacity: 0.28, strokeWidth: 0.75 }),
        h("path", {
          d: "M8.5 28.5H20M8.5 24.5H16M24.5 8.5V20M28.5 8.5V16",
          stroke: C_LINE, strokeOpacity: 0.34, strokeWidth: 0.75, strokeLinecap: "square"
        }),
        h("rect", { x: 9.5, y: 9.5, width: 21, height: 21, rx: 1.5, fill: C_SEAL }),
        h("rect", { x: 11.3, y: 11.3, width: 17.4, height: 17.4, rx: 0.8, fill: "none", stroke: C_SEAL_INK, strokeOpacity: 0.45, strokeWidth: 0.6 }),
        h("text", {
          x: 20, y: 20.4, textAnchor: "middle", dominantBaseline: "central",
          fill: C_SEAL_INK, fontFamily: FONT_SERIF, fontSize: 12.5, fontWeight: 600
        }, "奕")
      );
    }

    // 字标：中文两字宋体立骨，下面一行 8px 拉丁小字拉开字距，再压一道细线收口。
    function BrandName() {
      return h("span", {
        style: {
          display: "flex", flexDirection: "column", alignItems: "flex-start",
          lineHeight: 1, paddingTop: 1, userSelect: "none"
        }
      },
        h("span", { style: { fontFamily: FONT_SERIF, fontSize: 17, fontWeight: 600, letterSpacing: "0.16em", color: C_LABEL } }, "商奕"),
        h("span", {
          style: {
            marginTop: 4, fontFamily: FONT_SANS, fontSize: 8, fontWeight: 500,
            letterSpacing: "0.34em", textTransform: "uppercase", color: C_MUTED
          }
        }, "SHANGYI"),
        h("span", { style: { marginTop: 3.5, height: 1, width: 22, background: C_LINE, opacity: 0.55 } })
      );
    }

    // ──────────────────────────────────────────────────────────────
    // 三、外面那一层「材料」
    //    方向之间的差别主要在这里：宣纸的纤维、漆面的金线、账册的横格。
    // ──────────────────────────────────────────────────────────────
    var NOISE = "url(\"data:image/svg+xml;utf8,<svg xmlns='http://www.w3.org/2000/svg' width='140' height='140'><filter id='n'><feTurbulence type='fractalNoise' baseFrequency='0.85' numOctaves='2' stitchTiles='stitch'/><feColorMatrix type='saturate' values='0'/></filter><rect width='140' height='140' filter='url(%23n)' opacity='0.030'/></svg>\")";

    function layerStyle(variant) {
      var base = { position: "fixed", inset: 0, pointerEvents: "none", zIndex: 0 };
      if (variant === "ledger") {
        // 账册：每 32px 一道横格，纸面略偏米黄；格线用主色兑纸色，不抢字。
        return Object.assign(base, {
          backgroundImage:
            "repeating-linear-gradient(to bottom, transparent 0 31px, color-mix(in srgb, var(--dsw-alias-brand-primary) 7%, transparent) 31px 32px)," +
            "radial-gradient(120% 55% at 50% -8%, color-mix(in srgb, var(--dsw-alias-brand-primary) 5%, transparent), transparent 62%)"
        });
      }
      if (variant === "lacquer") {
        // 黑漆：顶部一道暖光 + 金线内框（离边 14px），其余留黑。
        return Object.assign(base, {
          backgroundImage:
            "radial-gradient(110% 55% at 50% -14%, color-mix(in srgb, var(--dsw-alias-brand-primary) 10%, transparent), transparent 58%)," +
            "linear-gradient(to right, transparent 13px, color-mix(in srgb, var(--dsw-alias-brand-primary) 16%, transparent) 14px, transparent 15px)," +
            "linear-gradient(to left, transparent 13px, color-mix(in srgb, var(--dsw-alias-brand-primary) 16%, transparent) 14px, transparent 15px)," +
            "linear-gradient(to bottom, transparent 13px, color-mix(in srgb, var(--dsw-alias-brand-primary) 12%, transparent) 14px, transparent 15px)"
        });
      }
      // 宣纸：纤维噪点 + 顶部一层极淡的青。
      return Object.assign(base, {
        backgroundImage:
          "radial-gradient(120% 60% at 50% -12%, color-mix(in srgb, var(--dsw-alias-brand-primary) 6%, transparent), transparent 64%)," + NOISE
      });
    }

    function MaterialLayer(props) {
      return h("div", { "aria-hidden": true, style: layerStyle(props.variant) });
    }

    // ──────────────────────────────────────────────────────────────
    // 四、装配
    // ──────────────────────────────────────────────────────────────
    function readVariant() {
      try {
        var q = new URLSearchParams(window.location.search).get("shangyi");
        if (q && VARIANTS[q]) return q;
      } catch (e) { /* 无 window 的宿主里忽略 */ }
      return "paper";
    }

    function HealthBanner() {
      var st = React.useState({ health: null, loading: false, dismissed: false });
      var state = st[0], setState = st[1];

      var refresh = React.useCallback(function () {
        setState(function (prev) { return { health: prev.health, loading: true, dismissed: false }; });
        fetch("/api/shangyi/health", { cache: "no-store" })
          .then(function (r) { return r.json(); })
          .then(function (data) {
            setState({ health: data, loading: false, dismissed: false });
          })
          .catch(function (err) {
            setState({
              health: {
                ok: false,
                problems: [{
                  code: "health_unreachable",
                  severity: "blocking",
                  title: "无法连接商奕宿主健康检查接口",
                  detail: String(err && err.message ? err.message : err),
                  fixCommands: ["shangyi start"]
                }]
              },
              loading: false,
              dismissed: false
            });
          });
      }, []);

      React.useEffect(function () {
        refresh();
        var timer = setInterval(refresh, 15000);
        return function () { clearInterval(timer); };
      }, [refresh]);

      var health = state.health;
      if (!health || health.ok || state.dismissed) return null;

      var problems = Array.isArray(health.problems) ? health.problems : [];
      var primary = problems[0] || {
        title: "商奕确定性求解内核未就绪",
        detail: "缺少可执行 shangyi 内核或 MCP 工具未连接，当前若继续对话将退化为无工具自由发挥，严禁用于正式商业决策。",
        fixCommands: ["shangyi start"]
      };
      var cmd = Array.isArray(primary.fixCommands) && primary.fixCommands.length ? primary.fixCommands[0] : "shangyi start";

      return h("div", {
        role: "alert",
        "data-shangyi-health-banner": "degraded",
        style: {
          display: "flex",
          alignItems: "center",
          justifyContent: "space-between",
          gap: "12px",
          flexWrap: "wrap",
          padding: "8px 16px",
          background: "var(--dsw-alias-bg-layer-2)",
          borderTop: "2px solid var(--dsw-alias-state-error-primary)",
          color: "var(--dsw-alias-label-primary)",
          fontFamily: FONT_SANS,
          fontSize: "12px",
          lineHeight: "18px"
        }
      },
        h("div", { style: { display: "flex", alignItems: "center", gap: "8px", flexWrap: "wrap", flex: "1 1 320px" } },
          h("span", {
            style: {
              padding: "2px 8px",
              borderRadius: "4px",
              background: "var(--dsw-alias-state-error-primary)",
              color: "var(--dsw-alias-label-primary-foreground)",
              fontWeight: 600,
              whiteSpace: "nowrap"
            }
          }, "内核未就绪 · 禁止盲聊"),
          h("strong", { style: { color: "var(--dsw-alias-state-error-primary)" } }, primary.title),
          h("span", { style: { color: "var(--dsw-alias-label-secondary)" } }, primary.detail),
          h("code", {
            style: {
              padding: "2px 8px",
              borderRadius: "4px",
              background: "var(--dsw-alias-bg-layer-3)",
              border: "1px solid var(--dsw-alias-border-l1)",
              color: "var(--dsw-alias-label-primary)",
              fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace"
            }
          }, cmd)
        ),
        h("div", { style: { display: "flex", alignItems: "center", gap: "8px" } },
          h("button", {
            type: "button",
            disabled: state.loading,
            onClick: refresh,
            style: {
              padding: "4px 10px",
              borderRadius: "6px",
              border: "1px solid var(--dsw-alias-border-l2)",
              background: "var(--dsw-alias-bg-base)",
              color: "var(--dsw-alias-label-primary)",
              cursor: "pointer",
              fontSize: "12px"
            }
          }, state.loading ? "检测中…" : "重新检测"),
          h("button", {
            type: "button",
            onClick: function () { setState(function (prev) { return { health: prev.health, loading: prev.loading, dismissed: true }; }); },
            style: {
              padding: "4px 8px",
              borderRadius: "6px",
              border: "1px solid var(--dsw-alias-border-l1)",
              background: "transparent",
              color: "var(--dsw-alias-label-secondary)",
              cursor: "pointer",
              fontSize: "12px"
            }
          }, "暂收")
        )
      );
    }

    function apply(ctx) {
      var variant = readVariant();

      // token 层：source 固定，重复调用即替换整层；ctx.effect 的清理函数让卸载自动还原。
      ctx.effect(function () {
        return ctx.theme.overrideTokens("dsh-plugin-shangyi-shell", VARIANTS[variant]);
      }, "shangyi: color tokens");

      // 单槽品牌位：官方品牌插件已在 bundle 补丁里关掉，这三个座位由本插件独占。
      var mark = function () { return h(Seal, { size: 26 }); };
      var hero = function (p) { return h(HeroMark, { size: (p && p.size) || 34 }); };
      ctx.slots.inject("sidebar.brand.mark", function () {
        return ctx.slots.register({ name: "sidebar.brand.mark" }, mark);
      });
      ctx.slots.inject("sidebar.brand.name", function () {
        return ctx.slots.register({ name: "sidebar.brand.name" }, BrandName);
      });
      ctx.slots.inject("conversation.hero.brand.mark", function () {
        return ctx.slots.register({ name: "conversation.hero.brand.mark" }, hero);
      });
      ctx.slots.inject("shell.overlay", function () {
        return ctx.slots.register({ name: "shell.overlay", id: "shangyi-material", order: -10 }, function () {
          return h(MaterialLayer, { variant: variant });
        });
      });
      ctx.slots.inject("shell.bottom", function () {
        return ctx.slots.register({ name: "shell.bottom", id: "shangyi-health-banner", order: 10 }, HealthBanner);
      });
    }

    return {
      inject: ["slots", "theme"],
      apply: apply
    };
  }
});
