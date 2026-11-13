    // ──────────────────────────────────────────────────────────────
    // 注册：侧栏入口 = sidebar.panellist 的一条；主面板 = main 的 keyed 注册。
    // 侧栏的点击由 DSH 的 layout.selectPanel(id) 处理，id 必须等于 main 的 key。
    // ──────────────────────────────────────────────────────────────
    var PANEL_ID = "shangyi-workbench";

    /** 侧栏入口图标：props 由 DSH 传入 { size, active }。 */
    function WorkbenchEntryIcon(props) {
      var size = (props && props.size) || 16;
      return h("span", {
        style: { display: "inline-flex", alignItems: "center", justifyContent: "center", width: size, height: size },
        "aria-hidden": true
      }, h(Icon, { name: "弈", size: size }));
    }

    function apply(ctx) {
      if (typeof window !== "undefined") {
        window.__SHANGYI_WORKBENCH_READY__ = true;
      }
      var Panel = createWorkbenchPanel(ctx);

      ctx.slots.inject("sidebar.panellist", function () {
        return ctx.slots.register({
          name: "sidebar.panellist",
          id: PANEL_ID,
          order: 10,
          label: "商奕"
        }, WorkbenchEntryIcon);
      });

      ctx.slots.inject("main", function () {
        return ctx.slots.register({
          name: "main",
          key: PANEL_ID
        }, Panel);
      });
    }

    return {
      inject: ["slots", "layout"],
      apply: apply,
      // 给 scripts/verify.mjs 用的内部引用（DSH 只读 apply/inject，多余导出会被忽略）。
      // 这不是运行时 API，不要在其他插件里依赖它。
      __internals: { SYData: SYData, SYView: SYView, SYIcon: SYIcon, Icon: Icon, PANEL_ID: PANEL_ID }
    };
  }
});
