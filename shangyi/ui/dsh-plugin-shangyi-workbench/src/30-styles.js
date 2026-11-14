    // ──────────────────────────────────────────────────────────────
    // 样式：只取 --dsw-alias-* token，不写死颜色；明暗主题自动跟随。
    // 以 <style> 元素随组件树渲染，卸载即移除（不往 document.head 里留东西）。
    // 约束：无渐变、圆角 8、间距 8 的倍数、数字 tabular-nums、阴影 0 层。
    // ──────────────────────────────────────────────────────────────
    var SY_FONT = '-apple-system, BlinkMacSystemFont, "Segoe UI", "PingFang SC", "Hiragino Sans GB", "Microsoft YaHei", sans-serif';

    var SY_CSS = [
        ".sy-wb{position:relative;display:flex;flex-direction:column;height:100%;flex:1 1 auto;min-height:0;background:var(--dsw-alias-bg-base);color:var(--dsw-alias-label-primary);font-family:" + SY_FONT + ";font-size:13px;line-height:1.5;-webkit-font-smoothing:antialiased}",
        ".sy-wb *{box-sizing:border-box}",
        ".sy-wb ::-webkit-scrollbar{width:8px;height:8px}",
        ".sy-wb ::-webkit-scrollbar-thumb{background:var(--dsw-alias-scrollbar-bg-l1);border-radius:999px}",
        ".sy-wb ::-webkit-scrollbar-thumb:hover{background:var(--dsw-alias-scrollbar-hover-l1)}",
        ".sy-wb ::-webkit-scrollbar-track{background:transparent}",

        // 顶栏
        ".sy-wb-head{flex:none;display:flex;align-items:center;gap:8px;padding:12px 16px;border-bottom:1px solid var(--dsw-alias-border-l1)}",
        ".sy-wb-head .sy-logo{display:inline-flex;color:var(--dsw-alias-brand-primary)}",
        ".sy-wb-head h1{margin:0;font-size:14px;font-weight:600;letter-spacing:.02em;display:flex;align-items:baseline;gap:8px}",
        ".sy-wb-head .sy-sub{color:var(--dsw-alias-label-tertiary);font-size:12px;font-weight:400}",
        ".sy-spacer{flex:1 1 auto}",

        // 三栏
        ".sy-wb-body{flex:1 1 auto;display:grid;grid-template-columns:264px minmax(0,1fr) 288px;min-height:0}",

        // 左栏：案例列表
        ".sy-wb-left{display:flex;flex-direction:column;min-height:0;border-right:1px solid var(--dsw-alias-border-l1);background:var(--dsw-alias-bg-layer-1)}",
        ".sy-search{flex:none;padding:8px}",
        ".sy-search input{width:100%;height:32px;padding:0 8px;border-radius:8px;border:1px solid var(--dsw-alias-border-l2);background:var(--dsw-alias-bg-base);color:inherit;font:inherit;font-size:12px}",
        ".sy-search input:focus{outline:2px solid var(--dsw-alias-brand-primary);outline-offset:-1px;border-color:transparent}",
        ".sy-list{flex:1 1 auto;overflow:auto;padding:0 8px 8px;display:flex;flex-direction:column;gap:4px}",
        ".sy-case{appearance:none;display:block;width:100%;text-align:left;padding:8px;border-radius:8px;border:1px solid transparent;background:transparent;color:inherit;font:inherit;cursor:pointer}",
        ".sy-case:hover{background:var(--dsw-alias-interactive-bg-hover)}",
        ".sy-case[aria-pressed=true]{background:var(--dsw-alias-bg-layer-2);border-color:var(--dsw-alias-border-l2)}",
        ".sy-case-id{font-size:12px;font-weight:500;font-variant-numeric:tabular-nums;word-break:break-all}",
        ".sy-case-meta{margin-top:4px;display:flex;align-items:center;gap:6px;color:var(--dsw-alias-label-tertiary);font-size:11px;flex-wrap:wrap}",

        // 中栏
        ".sy-wb-center{display:flex;flex-direction:column;min-width:0;min-height:0}",
        ".sy-casehead{flex:none;padding:16px 24px 8px}",
        ".sy-casename{margin:0 0 4px;font-size:15px;font-weight:600;font-variant-numeric:tabular-nums;word-break:break-all}",
        ".sy-kv{display:flex;flex-wrap:wrap;gap:4px 16px;color:var(--dsw-alias-label-secondary);font-size:12px}",
        ".sy-kv b{font-weight:500;color:var(--dsw-alias-label-primary);font-variant-numeric:tabular-nums}",
        ".sy-tabs{flex:none;display:flex;gap:4px;padding:8px 24px 0}",
        ".sy-tab{appearance:none;border:0;background:transparent;color:var(--dsw-alias-label-secondary);font:inherit;font-size:12px;padding:6px 8px;border-radius:8px;cursor:pointer;display:inline-flex;align-items:center;gap:6px}",
        ".sy-tab:hover{background:var(--dsw-alias-interactive-bg-hover)}",
        ".sy-tab[aria-selected=true]{color:var(--dsw-alias-label-primary);background:var(--dsw-alias-bg-layer-2);font-weight:500}",
        ".sy-content{flex:1 1 auto;overflow:auto;padding:8px 24px 24px;min-height:0}",

        // 统计块
        ".sy-stats{display:grid;grid-template-columns:repeat(auto-fit,minmax(104px,1fr));gap:8px;margin-top:8px}",
        ".sy-stat{border:1px solid var(--dsw-alias-border-l1);border-radius:8px;padding:8px 12px;background:var(--dsw-alias-bg-layer-1)}",
        ".sy-stat .sy-k{color:var(--dsw-alias-label-tertiary);font-size:11px}",
        ".sy-stat .sy-v{margin-top:2px;font-size:18px;font-weight:600;font-variant-numeric:tabular-nums}",

        // 区块
        ".sy-sec{margin-top:16px}",
        ".sy-sec-h{display:flex;align-items:center;gap:8px;margin:0 0 8px;font-size:12px;font-weight:600;color:var(--dsw-alias-label-secondary)}",
        ".sy-note{color:var(--dsw-alias-label-tertiary);font-size:11px;line-height:1.6}",

        // 阶段链
        ".sy-chain{display:flex;flex-wrap:wrap;align-items:center;gap:4px}",
        ".sy-phase{padding:2px 8px;border-radius:8px;border:1px solid var(--dsw-alias-border-l2);font-size:12px;color:var(--dsw-alias-label-secondary);white-space:nowrap}",
        ".sy-phase.is-done{color:var(--dsw-alias-label-primary);border-color:var(--dsw-alias-border-l1);background:var(--dsw-alias-bg-layer-1)}",
        ".sy-phase.is-current{color:var(--dsw-alias-label-primary-foreground);background:var(--dsw-alias-brand-primary);border-color:transparent;font-weight:500}",
        ".sy-sep{color:var(--dsw-alias-label-caption);font-size:11px}",

        // 支付矩阵
        ".sy-matrix-wrap{border:1px solid var(--dsw-alias-border-l1);border-radius:8px;overflow:auto;background:var(--dsw-alias-bg-layer-1)}",
        ".sy-matrix{border-collapse:separate;border-spacing:0;width:100%;font-variant-numeric:tabular-nums}",
        ".sy-matrix th,.sy-matrix td{border-bottom:1px solid var(--dsw-alias-border-l1);border-right:1px solid var(--dsw-alias-border-l1);padding:8px;text-align:left;vertical-align:top;font-size:12px;font-weight:400}",
        ".sy-matrix thead th{position:sticky;top:0;z-index:1;background:var(--dsw-alias-bg-layer-2);font-weight:500;color:var(--dsw-alias-label-secondary)}",
        ".sy-matrix tbody th{background:var(--dsw-alias-bg-layer-2);font-weight:500;color:var(--dsw-alias-label-secondary)}",
        ".sy-matrix tr:last-child td,.sy-matrix tr:last-child th{border-bottom:0}",
        ".sy-matrix th:last-child,.sy-matrix td:last-child{border-right:0}",
        ".sy-cell-line{display:flex;gap:6px;align-items:baseline;font-size:12px;white-space:nowrap}",
        ".sy-cell-line .sy-p{color:var(--dsw-alias-label-tertiary);font-size:11px}",
        ".sy-expr{font-variant-numeric:tabular-nums}",

        // 均衡
        ".sy-eq{border:1px solid var(--dsw-alias-border-l1);border-radius:8px;padding:8px 12px;background:var(--dsw-alias-bg-layer-1);margin-bottom:8px}",
        ".sy-eq-top{display:flex;align-items:baseline;gap:8px;flex-wrap:wrap}",
        ".sy-eq-id{font-weight:600;font-size:12px;font-variant-numeric:tabular-nums}",
        ".sy-eq-kind{color:var(--dsw-alias-label-tertiary);font-size:11px}",
        ".sy-eq-pay{color:var(--dsw-alias-label-secondary);font-size:11px;font-variant-numeric:tabular-nums}",
        ".sy-eq-prob{margin-left:auto;font-variant-numeric:tabular-nums;font-size:12px;font-weight:500}",
        ".sy-bar{height:6px;border-radius:999px;background:var(--dsw-alias-bg-layer-3);overflow:hidden;margin-top:6px}",
        ".sy-bar > i{display:block;height:100%;background:var(--dsw-alias-brand-primary)}",

        // 轨迹
        ".sy-trail-item{display:flex;gap:8px;padding:8px 0;border-bottom:1px solid var(--dsw-alias-border-l1)}",
        ".sy-trail-item:last-child{border-bottom:0}",
        ".sy-trail-when{min-width:96px;color:var(--dsw-alias-label-tertiary);font-size:11px;font-variant-numeric:tabular-nums}",
        ".sy-trail-body{flex:1 1 auto;min-width:0}",
        ".sy-trail-tool{font-weight:500;font-size:12px}",
        ".sy-trail-detail{color:var(--dsw-alias-label-secondary);font-size:11px;word-break:break-word;margin-top:2px}",

        // 右检查器
        ".sy-wb-right{min-height:0;overflow:auto;padding:16px;border-left:1px solid var(--dsw-alias-border-l1);background:var(--dsw-alias-bg-layer-1)}",
        ".sy-insp-sec{margin-bottom:16px}",
        ".sy-insp-sec:last-child{margin-bottom:0}",
        ".sy-insp-sec h3{margin:0 0 8px;font-size:11px;font-weight:600;color:var(--dsw-alias-label-tertiary);letter-spacing:.06em}",
        ".sy-insp-row{display:flex;justify-content:space-between;gap:8px;padding:2px 0;font-size:12px}",
        ".sy-insp-row .sy-k{color:var(--dsw-alias-label-tertiary)}",
        ".sy-insp-row .sy-v{text-align:right;font-variant-numeric:tabular-nums;word-break:break-all}",

        // 空态 / 加载 / 错误
        ".sy-empty{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:8px;padding:48px 16px;text-align:center;color:var(--dsw-alias-label-secondary)}",
        ".sy-empty .sy-empty-icon{color:var(--dsw-alias-brand-primary)}",
        ".sy-empty h2{margin:0;font-size:14px;font-weight:600;color:var(--dsw-alias-label-primary)}",
        ".sy-empty p{margin:0;font-size:12px;line-height:1.6;max-width:34em}",
        ".sy-empty-actions{display:flex;gap:8px;margin-top:8px}",
        ".sy-btn{appearance:none;font:inherit;font-size:12px;height:28px;padding:0 12px;border-radius:8px;border:1px solid var(--dsw-alias-border-l2);background:var(--dsw-alias-bg-base);color:var(--dsw-alias-label-primary);cursor:pointer;display:inline-flex;align-items:center;gap:6px}",
        ".sy-btn:hover{background:var(--dsw-alias-interactive-bg-hover)}",
        ".sy-btn[disabled]{opacity:.55;cursor:not-allowed}",
        ".sy-btn.is-primary{background:var(--dsw-alias-brand-primary);border-color:transparent;color:var(--dsw-alias-label-primary-foreground)}",
        ".sy-btn.is-primary:hover{opacity:.92}",
        ".sy-wb button:focus-visible,.sy-wb textarea:focus-visible{outline:2px solid var(--dsw-alias-brand-primary);outline-offset:2px}",
        ".sy-loading{display:flex;align-items:center;gap:8px;color:var(--dsw-alias-label-tertiary);font-size:12px;padding:16px 0}",
        ".sy-spin{width:12px;height:12px;border-radius:999px;border:1.5px solid var(--dsw-alias-border-l2);border-top-color:var(--dsw-alias-brand-primary);animation:sy-wb-spin .8s linear infinite}",
        "@keyframes sy-wb-spin{to{transform:rotate(360deg)}}",
        "@media (prefers-reduced-motion:reduce){.sy-spin{animation:none}}",
        ".sy-error{display:flex;gap:8px;align-items:flex-start;border:1px solid var(--dsw-alias-state-error-primary);border-radius:8px;padding:8px 12px;color:var(--dsw-alias-state-error-primary);font-size:12px;background:var(--dsw-alias-bg-layer-1)}",
        ".sy-status{display:flex;gap:8px;align-items:center;border:1px solid var(--dsw-alias-state-success-primary);border-radius:8px;padding:8px 12px;color:var(--dsw-alias-state-success-primary);font-size:12px;background:var(--dsw-alias-bg-layer-1);margin-bottom:12px}",
        ".sy-skel{height:44px;border-radius:8px;background:var(--dsw-alias-bg-layer-2)}",
        ".sy-skel+.sy-skel{margin-top:8px}",

        // 待决关口确认卡（铁律 2：真人确认）
        ".sy-cp-card{border:1px solid var(--dsw-alias-state-warn-primary);border-radius:8px;padding:16px;background:var(--dsw-alias-bg-layer-1);margin-top:8px;margin-bottom:16px}",
        ".sy-cp-head{display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin-bottom:8px}",
        ".sy-cp-title{margin:0;font-size:13px;font-weight:600;color:var(--dsw-alias-label-primary)}",
        ".sy-cp-items{margin:8px 0 12px;padding-left:20px;font-size:12px;line-height:1.6;color:var(--dsw-alias-label-primary)}",
        ".sy-cp-form{display:flex;flex-direction:column;gap:8px}",
        ".sy-cp-label{font-size:12px;font-weight:500;color:var(--dsw-alias-label-secondary)}",
        ".sy-cp-textarea{width:100%;min-height:64px;padding:8px;border-radius:8px;border:1px solid var(--dsw-alias-border-l2);background:var(--dsw-alias-bg-base);color:inherit;font:inherit;font-size:12px;line-height:1.5;resize:vertical}",
        ".sy-cp-actions{display:flex;align-items:center;gap:8px;flex-wrap:wrap}",

        // 知识库条目按钮与全文阅读抽屉
        ".sy-kb-btn{appearance:none;display:block;width:100%;text-align:left;padding:4px 8px;margin:2px -8px;border:0;border-radius:8px;background:transparent;color:var(--dsw-alias-label-secondary);font:inherit;font-size:11px;line-height:1.5;cursor:pointer}",
        ".sy-kb-btn:hover{background:var(--dsw-alias-interactive-bg-hover);color:var(--dsw-alias-label-primary)}",
        ".sy-kb-drawer{position:absolute;top:0;right:0;bottom:0;width:min(600px,94%);z-index:4;display:flex;flex-direction:column;background:var(--dsw-alias-bg-base);border-left:1px solid var(--dsw-alias-border-l1)}",
        ".sy-kb-drawer-head{flex:none;display:flex;align-items:center;gap:8px;padding:12px 16px;border-bottom:1px solid var(--dsw-alias-border-l1);background:var(--dsw-alias-bg-layer-1)}",
        ".sy-kb-drawer-title{margin:0;font-size:13px;font-weight:600;flex:1 1 auto;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}",
        ".sy-kb-drawer-meta{flex:none;display:flex;flex-wrap:wrap;gap:6px;padding:8px 24px;border-bottom:1px solid var(--dsw-alias-border-l1);background:var(--dsw-alias-bg-layer-1)}",
        ".sy-kb-drawer-body{flex:1 1 auto;overflow:auto;padding:16px 24px;word-break:break-word;font-size:13px;line-height:1.7;color:var(--dsw-alias-label-primary)}",

        // 健康状态警报条（缺二进制/MCP未连接时强提示）
        ".sy-health-alert{margin:12px 24px 0;padding:12px 16px;border-radius:8px;border:1px solid var(--dsw-alias-state-error-primary);background:var(--dsw-alias-bg-layer-2);color:var(--dsw-alias-label-primary)}",
        ".sy-health-head{display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin-bottom:6px}",
        ".sy-health-title{font-size:13px;font-weight:600;color:var(--dsw-alias-state-error-primary)}",
        ".sy-health-cmd{display:inline-block;margin-top:4px;padding:3px 8px;border-radius:6px;background:var(--dsw-alias-bg-base);border:1px solid var(--dsw-alias-border-l1);font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:11px;color:var(--dsw-alias-label-primary)}",

        // 新手引导与示例任务卡
        ".sy-guide{padding:24px;max-width:840px;margin:0 auto}",
        ".sy-guide-hero{border:1px solid var(--dsw-alias-border-l1);border-radius:8px;padding:16px 20px;background:var(--dsw-alias-bg-layer-1);margin-bottom:16px}",
        ".sy-guide-hero h2{margin:0 0 8px;font-size:16px;font-weight:600}",
        ".sy-guide-hero p{margin:0;font-size:13px;line-height:1.6;color:var(--dsw-alias-label-secondary)}",
        ".sy-steps{display:grid;grid-template-columns:repeat(auto-fit,minmax(200px,1fr));gap:12px;margin:16px 0}",
        ".sy-step-card{border:1px solid var(--dsw-alias-border-l1);border-radius:8px;padding:12px 14px;background:var(--dsw-alias-bg-base)}",
        ".sy-step-num{display:inline-flex;align-items:center;justify-content:center;width:20px;height:20px;border-radius:999px;background:var(--dsw-alias-brand-primary);color:var(--dsw-alias-label-primary-foreground);font-size:11px;font-weight:600;margin-right:8px}",
        ".sy-examples{display:grid;grid-template-columns:repeat(auto-fit,minmax(260px,1fr));gap:12px;margin-top:8px}",
        ".sy-ex-card{appearance:none;text-align:left;border:1px solid var(--dsw-alias-border-l1);border-radius:8px;padding:12px 14px;background:var(--dsw-alias-bg-layer-1);color:inherit;font:inherit;cursor:pointer;display:flex;flex-direction:column;gap:6px}",
        ".sy-ex-card:hover{border-color:var(--dsw-alias-brand-primary);background:var(--dsw-alias-interactive-bg-hover)}",
        ".sy-ex-title{font-size:13px;font-weight:600;color:var(--dsw-alias-label-primary)}",
        ".sy-ex-hint{font-size:12px;color:var(--dsw-alias-label-secondary);line-height:1.5}",

        // 白话结论卡与导出工具条
        ".sy-conclusion{border:1px solid var(--dsw-alias-brand-primary);border-radius:8px;padding:16px;background:var(--dsw-alias-bg-layer-1);margin-bottom:16px}",
        ".sy-conclusion-hl{margin:0 0 10px;font-size:15px;font-weight:600;line-height:1.5;color:var(--dsw-alias-label-primary)}",
        ".sy-act-list{margin:8px 0;padding-left:20px;font-size:13px;line-height:1.65}",
        ".sy-act-why{color:var(--dsw-alias-label-secondary);font-size:12px;margin-top:2px}",
        ".sy-toolbar-row{display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin-top:12px}",

        // 成本精算与内联知识库网格
        ".sy-cost-card{border:1px solid var(--dsw-alias-border-l1);border-radius:8px;padding:16px;background:var(--dsw-alias-bg-layer-1);margin-bottom:16px}",
        ".sy-missing-val{color:var(--dsw-alias-state-warn-primary);font-weight:600}",
        ".sy-kb-toolbar{display:flex;align-items:center;gap:8px;flex-wrap:wrap;margin-bottom:16px}",
        ".sy-kb-search{flex:1 1 220px;min-width:180px;height:32px;padding:0 10px;border:1px solid var(--dsw-alias-border-l2);border-radius:8px;background:var(--dsw-alias-bg-base);color:inherit;font:inherit;font-size:12px}",
        ".sy-kb-grid{display:grid;grid-template-columns:repeat(auto-fill,minmax(240px,1fr));gap:12px}",
        ".sy-kb-card{appearance:none;text-align:left;border:1px solid var(--dsw-alias-border-l1);border-radius:8px;padding:12px 14px;background:var(--dsw-alias-bg-layer-1);color:inherit;font:inherit;cursor:pointer;display:flex;flex-direction:column;gap:6px}",
        ".sy-kb-card:hover{border-color:var(--dsw-alias-brand-primary);background:var(--dsw-alias-interactive-bg-hover)}",

        // 安全 Markdown 排版（纯 React 节点渲染）
        ".sy-md h1,.sy-md h2,.sy-md h3,.sy-md h4{margin:16px 0 8px;font-weight:600;line-height:1.4;color:var(--dsw-alias-label-primary)}",
        ".sy-md h1{font-size:17px;border-bottom:1px solid var(--dsw-alias-border-l1);padding-bottom:6px}",
        ".sy-md h2{font-size:15px}",
        ".sy-md h3{font-size:13px}",
        ".sy-md p{margin:8px 0;font-size:13px;line-height:1.7}",
        ".sy-md ul,.sy-md ol{margin:8px 0;padding-left:22px;font-size:13px;line-height:1.7}",
        ".sy-md blockquote{margin:10px 0;padding:8px 12px;border-left:3px solid var(--dsw-alias-brand-primary);background:var(--dsw-alias-bg-layer-1);color:var(--dsw-alias-label-secondary);border-radius:0 8px 8px 0}",
        ".sy-md pre{margin:10px 0;padding:10px 12px;border-radius:8px;border:1px solid var(--dsw-alias-border-l1);background:var(--dsw-alias-bg-layer-2);overflow:auto;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:11px;line-height:1.55}",
        ".sy-md code{padding:1px 5px;border-radius:4px;background:var(--dsw-alias-bg-layer-2);font-family:ui-monospace,SFMono-Regular,Menlo,monospace;font-size:11px}",
        ".sy-md pre code{padding:0;background:transparent}",
        ".sy-md hr{border:0;border-top:1px solid var(--dsw-alias-border-l1);margin:14px 0}",
        ".sy-md a{color:var(--dsw-alias-brand-primary);text-decoration:underline}",
        ".sy-md-wiki{appearance:none;border:0;border-bottom:1px dashed var(--dsw-alias-brand-primary);padding:0 2px;background:transparent;color:var(--dsw-alias-brand-primary);font:inherit;cursor:pointer}",

        // 标签
        ".sy-tag{display:inline-flex;align-items:center;height:20px;padding:0 8px;border-radius:8px;font-size:11px;border:1px solid var(--dsw-alias-border-l2);color:var(--dsw-alias-label-secondary);font-variant-numeric:tabular-nums;white-space:nowrap}",
        ".sy-tag.is-ok{color:var(--dsw-alias-state-success-primary);border-color:var(--dsw-alias-state-success-primary)}",
        ".sy-tag.is-warn{color:var(--dsw-alias-state-warn-primary);border-color:var(--dsw-alias-state-warn-primary)}",
        ".sy-tag.is-err{color:var(--dsw-alias-state-error-primary);border-color:var(--dsw-alias-state-error-primary)}",
        ".sy-tag.is-mute{color:var(--dsw-alias-label-tertiary)}",

        // 窄屏：右栏折叠为抽屉，左栏收窄
        ".sy-insp-toggle{display:none}",
        "@media (max-width:1100px){",
        ".sy-wb-body{grid-template-columns:224px minmax(0,1fr)}",
        ".sy-wb-right{display:none}",
        ".sy-insp-toggle{display:inline-flex}",
        ".sy-wb-right.is-open{display:block;position:absolute;top:0;right:0;bottom:0;width:272px;z-index:3;box-shadow:0 0 0 1px var(--dsw-alias-border-l1)}",
        "}",

        // 打印 / 另存 PDF 排版
        "@media print{",
        ".sy-wb{height:auto;overflow:visible}",
        ".sy-wb-head,.sy-wb-left,.sy-wb-right,.sy-tabs,.sy-btn,.sy-kb-drawer{display:none!important}",
        ".sy-wb-body{display:block!important;overflow:visible}",
        ".sy-wb-center,.sy-content{overflow:visible!important;height:auto!important;padding:0!important}",
        "}"
    ].join("\n");
