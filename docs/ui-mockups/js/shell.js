/**
 * SessionBridge UI Mockups — 共享壳层渲染
 * 侧栏 / 顶栏 / Inspector 的静态 HTML 拼装，保证各页面布局一致
 */
(function () {
  const PAGES = [
    { id: "overview", label: "概览", href: "overview.html", ico: "◎" },
    { id: "work", label: "工作项", href: "work.html", ico: "☰" },
    { id: "agents", label: "Agent", href: "agents.html", ico: "⬡" },
    { id: "chat", label: "群聊", href: "chat.html", ico: "💬" },
    { id: "activity", label: "活动", href: "activity.html", ico: "⌁" },
    { id: "diagnostics", label: "诊断", href: "diagnostics.html", ico: "⚙" },
  ];

  function phaseDot(phase) {
    if (phase === "active") return "ok";
    if (phase === "paused") return "warn";
    if (phase === "completed") return "ok";
    return "";
  }

  function shortId(value, size = 8) {
    if (!value) return "—";
    return value.length > size ? value.slice(0, size) : value;
  }

  function relativeMicros(value) {
    const delta = Date.now() - value / 1000;
    if (delta < 0) return "未来";
    if (delta < 60_000) return `${Math.max(1, Math.floor(delta / 1000))} 秒前`;
    if (delta < 3_600_000) return `${Math.floor(delta / 60_000)} 分钟前`;
    if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)} 小时前`;
    return new Date(value / 1000).toLocaleDateString();
  }

  function renderSidebar(activePage, activeTaskId) {
    const mock = window.SB_MOCK;
    const tasks = mock.tasks
      .map((task) => {
        const on = task.id === activeTaskId ? " on" : "";
        return `<button class="task-link${on}" type="button" title="${task.title}">
          <span class="dot ${phaseDot(task.phase)}"></span>
          <span><strong>${task.title}</strong><small>${task.phase} · v${task.plan_version}</small></span>
        </button>`;
      })
      .join("");

    const pages = PAGES.map((page) => {
      const on = page.id === activePage ? " on" : "";
      return `<a class="page-link${on}" href="${page.href}"><span class="nav-ico">${page.ico}</span>${page.label}</a>`;
    }).join("");

    return `
      <aside class="sidebar">
        <div class="brand-row">
          <div class="brand-mark">SB</div>
          <div><strong>SessionBridge</strong><span class="sub">Operator Console</span></div>
        </div>
        <button class="workspace-switcher" type="button" title="切换 Workspace">
          <span>📁</span>
          <span><strong>${mock.workspace.name}</strong><small>${mock.workspace.root}</small></span>
          <span>›</span>
        </button>
        <div class="sidebar-section">
          <span>任务</span>
          <a class="btn icon sm" href="launch.html" title="发布任务">＋</a>
        </div>
        <nav class="task-nav">${tasks}</nav>
        <nav class="page-nav">${pages}</nav>
        <div class="sidebar-footer">
          <button class="page-link" type="button" data-theme-toggle title="切换主题">
            <span class="nav-ico">◐</span> 主题
          </button>
          <div class="connection-state"><span class="dot live"></span> SQLite 实时同步</div>
        </div>
      </aside>`;
  }

  function renderHeader(opts) {
    const { eyebrow, title, phase, actionsHtml } = opts;
    return `
      <header class="page-header">
        <div class="title-block">
          <div class="kicker">${eyebrow || ""}</div>
          <div class="title">${title || "Workspace"}</div>
        </div>
        <div class="ph-actions">
          ${phase ? `<span class="pill ${phase === "active" ? "ok" : phase === "paused" ? "w" : "m"}"><span class="dot ${phaseDot(phase)}"></span>${phase}</span>` : ""}
          ${actionsHtml || ""}
        </div>
      </header>`;
  }

  function renderInspector() {
    const d = window.SB_MOCK.dashboard;
    const task = d.details.task;
    const criteria = d.details.criteria
      .map(
        (c) => `<div class="criterion"><span>${c.kind}</span><p>${c.content}</p></div>`
      )
      .join("");

    return `
      <aside class="inspector">
        <div class="inspector-heading">
          <div>
            <div class="kicker">Task Inspector</div>
            <h2>运行状态</h2>
          </div>
          <span style="color:rgb(var(--ink3))">⚙</span>
        </div>
        <div class="inspector-section">
          <div class="kv"><span>Task ID</span><strong class="mono">${shortId(task.id)}</strong></div>
          <div class="kv"><span>目标分支</span><strong>${task.target_branch || "当前分支"}</strong></div>
          <div class="kv"><span>自治策略</span><strong>${task.autonomy_policy}</strong></div>
          <div class="kv"><span>完成 Epoch</span><strong>${task.completion_epoch}</strong></div>
          <div class="kv"><span>Plan Version</span><strong>${task.plan_version}</strong></div>
          <div class="kv"><span>事件游标</span><strong>${d.health.event_cursor}</strong></div>
        </div>
        <div class="inspector-section">
          <h3>完成条件</h3>
          ${criteria}
        </div>
        <div class="inspector-actions">
          <button class="btn s block" type="button">⏸ 暂停任务</button>
        </div>
        <div class="inspector-section">
          <h3>快速跳转</h3>
          <div class="col-gap">
            <a class="btn s block" href="prompts.html">Agent 提示词</a>
            <a class="btn s block" href="launch.html">发布新任务</a>
          </div>
        </div>
      </aside>`;
  }

  function mountShell(options) {
    const {
      activePage,
      activeTaskId = "task_auth_impl",
      title,
      eyebrow,
      phase = "active",
      actionsHtml,
      contentHtml,
      showInspector = true,
      chromeTitle = "SessionBridge",
    } = options;

    const root = document.getElementById("app");
    if (!root) return;

    const shellClass = showInspector ? "app-shell" : "app-shell no-rail";
    root.innerHTML = `
      <div class="win">
        <div class="chrome">
          <div class="dots"><i></i><i></i><i></i></div>
          ${chromeTitle}
        </div>
        <div class="vp auto">
          <div class="${shellClass}">
            ${renderSidebar(activePage, activeTaskId)}
            <main class="main-panel">
              ${renderHeader({ eyebrow, title, phase, actionsHtml })}
              <section class="content">${contentHtml}</section>
            </main>
            ${showInspector ? renderInspector() : ""}
          </div>
        </div>
      </div>`;

    document.querySelectorAll("[data-theme-toggle]").forEach((btn) => {
      btn.addEventListener("click", () => {
        const rootEl = document.documentElement;
        const next = rootEl.getAttribute("data-theme") === "dark" ? "light" : "dark";
        rootEl.setAttribute("data-theme", next);
      });
    });
  }

  function docHeadLinks() {
    // 供各页面复制参考；实际由 HTML 静态引入
  }

  window.SB_SHELL = {
    PAGES,
    shortId,
    relativeMicros,
    phaseDot,
    mountShell,
    renderSidebar,
    renderHeader,
    renderInspector,
  };
})();
