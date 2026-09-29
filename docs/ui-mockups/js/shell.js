/**
 * SessionBridge UI Mockups — 共享壳层渲染
 * 侧栏 / 顶栏 / Inspector / Toast / 主题持久化
 */
(function () {
  const THEME_KEY = "sessionbridge-mock-theme";

  const PAGES = [
    { id: "overview", label: "概览", href: "overview.html", ico: "overview" },
    { id: "work", label: "工作项", href: "work.html", ico: "work" },
    { id: "agents", label: "Agent", href: "agents.html", ico: "agents" },
    { id: "chat", label: "群聊", href: "chat.html", ico: "chat" },
    { id: "activity", label: "活动", href: "activity.html", ico: "activity" },
    { id: "diagnostics", label: "诊断", href: "diagnostics.html", ico: "diagnostics" },
  ];

  function ico(name, cls) {
    return window.SB_ICONS ? window.SB_ICONS.icon(name, cls || "ico") : "";
  }

  function phaseDot(phase) {
    if (phase === "active") return "ok";
    if (phase === "paused") return "warn";
    if (phase === "completed") return "ok";
    if (phase === "cancelled" || phase === "archived") return "err";
    return "";
  }

  function phasePill(phase) {
    if (phase === "active") return "ok";
    if (phase === "paused") return "w";
    if (phase === "completed") return "info";
    if (phase === "cancelled") return "e";
    return "m";
  }

  function shortId(value, size = 8) {
    if (!value) return "—";
    return value.length > size ? value.slice(0, size) : value;
  }

  function relativeMicros(value) {
    if (value == null) return "—";
    const delta = Date.now() - value / 1000;
    if (delta < 0) return "未来";
    if (delta < 60_000) return `${Math.max(1, Math.floor(delta / 1000))} 秒前`;
    if (delta < 3_600_000) return `${Math.floor(delta / 60_000)} 分钟前`;
    if (delta < 86_400_000) return `${Math.floor(delta / 3_600_000)} 小时前`;
    return new Date(value / 1000).toLocaleDateString();
  }

  function escapeHtml(str) {
    return String(str)
      .replace(/&/g, "&amp;")
      .replace(/</g, "&lt;")
      .replace(/>/g, "&gt;")
      .replace(/"/g, "&quot;");
  }

  /* ---------- Theme ---------- */
  function applyTheme(theme) {
    const root = document.documentElement;
    if (theme === "system" || !theme) {
      root.removeAttribute("data-theme");
      localStorage.removeItem(THEME_KEY);
    } else {
      root.setAttribute("data-theme", theme);
      localStorage.setItem(THEME_KEY, theme);
    }
    updateThemeButtons();
  }

  function currentTheme() {
    return localStorage.getItem(THEME_KEY) || "system";
  }

  function cycleTheme() {
    const order = ["system", "light", "dark"];
    const cur = currentTheme();
    const next = order[(order.indexOf(cur) + 1) % order.length];
    applyTheme(next);
    const label = next === "system" ? "跟随系统" : next === "dark" ? "深色" : "浅色";
    toast({ title: "主题已切换", message: label, tone: "ok" });
  }

  function updateThemeButtons() {
    const theme = currentTheme();
    const label =
      theme === "dark" ? "深色" : theme === "light" ? "浅色" : "系统";
    const iconName = theme === "dark" ? "moon" : "sun";
    document.querySelectorAll("[data-theme-toggle]").forEach((btn) => {
      btn.innerHTML = `${ico(iconName)} 主题：${label}`;
      btn.setAttribute("data-tip", "切换主题（系统 / 浅色 / 深色）");
    });
  }

  function initTheme() {
    const saved = localStorage.getItem(THEME_KEY);
    if (saved === "light" || saved === "dark") {
      document.documentElement.setAttribute("data-theme", saved);
    }
  }

  initTheme();

  /* ---------- Toast ---------- */
  function ensureToastHost() {
    let host = document.querySelector(".toast-host");
    if (!host) {
      host = document.createElement("div");
      host.className = "toast-host";
      host.setAttribute("aria-live", "polite");
      document.body.appendChild(host);
    }
    return host;
  }

  function toast({ title, message, tone = "info", duration = 2600 } = {}) {
    const host = ensureToastHost();
    const el = document.createElement("div");
    el.className = `toast ${tone === "ok" ? "ok" : tone === "err" ? "err" : ""}`;
    const icoName = tone === "ok" ? "check" : tone === "err" ? "alert" : "activity";
    el.innerHTML = `
      <span class="toast-ico">${ico(icoName)}</span>
      <span>
        ${title ? `<strong>${escapeHtml(title)}</strong>` : ""}
        ${message ? `<p>${escapeHtml(message)}</p>` : ""}
      </span>`;
    host.appendChild(el);
    const t = setTimeout(() => {
      el.classList.add("out");
      setTimeout(() => el.remove(), 240);
    }, duration);
    el.addEventListener("click", () => {
      clearTimeout(t);
      el.classList.add("out");
      setTimeout(() => el.remove(), 240);
    });
    return el;
  }

  /* ---------- Preview bar ---------- */
  function renderPreviewBar(pageLabel) {
    return `
      <div class="preview-bar">
        <a class="btn pl sm" href="../index.html">${ico("chevronLeft")} 索引</a>
        <div class="crumbs">
          <a href="../index.html">Mockups</a>
          <span>/</span>
          <strong>${escapeHtml(pageLabel || "页面")}</strong>
        </div>
        <span class="hint">静态预览 · 模拟数据</span>
      </div>`;
  }

  /* ---------- Sidebar ---------- */
  function renderSidebar(activePage, activeTaskId, options = {}) {
    const mock = window.SB_MOCK;
    const tasks = options.tasks === null
      ? ""
      : (options.tasks || mock.tasks)
          .map((task) => {
            const on = task.id === activeTaskId ? " on" : "";
            return `<button class="task-link${on}" type="button" title="${escapeHtml(task.title)}" data-task-id="${task.id}">
              <span class="dot ${phaseDot(task.phase)}"></span>
              <span><strong>${escapeHtml(task.title)}</strong><small>${task.phase} · v${task.plan_version}</small></span>
            </button>`;
          })
          .join("") || `<div class="sidebar-empty">还没有任务<br/>点击 ＋ 发布第一个</div>`;

    const chatCount = mock.dashboard?.chat_messages?.length || 0;
    const attention =
      (mock.dashboard?.health?.signals?.open_blockers || 0) +
      (mock.dashboard?.health?.signals?.open_required_gaps || 0);

    const pages = PAGES.map((page) => {
      const on = page.id === activePage ? " on" : "";
      let badge = "";
      if (page.id === "chat" && chatCount) {
        badge = `<span class="nav-badge">${chatCount}</span>`;
      } else if (page.id === "overview" && attention) {
        badge = `<span class="nav-badge warn">${attention}</span>`;
      } else if (page.id === "diagnostics") {
        const w = mock.collaboration?.warning_count || 0;
        if (w) badge = `<span class="nav-badge warn">${w}</span>`;
      }
      return `<a class="page-link${on}" href="${page.href}">
        <span class="nav-ico">${ico(page.ico)}</span>${page.label}${badge}
      </a>`;
    }).join("");

    return `
      <aside class="sidebar">
        <div class="brand-row">
          <div class="brand-mark">SB</div>
          <div><strong>SessionBridge</strong><span class="sub">Operator Console</span></div>
        </div>
        <button class="workspace-switcher" type="button" title="切换 Workspace" data-action="switch-workspace">
          ${ico("folder")}
          <span><strong>${escapeHtml(mock.workspace.name)}</strong><small>${escapeHtml(mock.workspace.root)}</small></span>
          ${ico("chevron")}
        </button>
        <div class="sidebar-section">
          <span>任务</span>
          <a class="btn icon sm" href="launch.html" title="发布任务" data-tip="发布新任务">${ico("plus")}</a>
        </div>
        <nav class="task-nav" aria-label="任务列表">${tasks}</nav>
        <nav class="page-nav" aria-label="页面导航">${pages}</nav>
        <div class="sidebar-footer">
          <button class="page-link" type="button" data-theme-toggle title="切换主题">
            ${ico("sun")} 主题
          </button>
          <div class="connection-state"><span class="dot live"></span> SQLite 实时同步</div>
        </div>
      </aside>`;
  }

  function renderHeader(opts) {
    const { eyebrow, title, phase, actionsHtml, subtitle } = opts;
    return `
      <header class="page-header">
        <div class="title-block">
          <div class="kicker">${escapeHtml(eyebrow || "")}</div>
          <div class="title" title="${escapeHtml(title || "")}">${escapeHtml(title || "Workspace")}</div>
          ${subtitle ? `<div class="sub">${escapeHtml(subtitle)}</div>` : ""}
        </div>
        <div class="ph-actions">
          ${
            phase
              ? `<span class="pill ${phasePill(phase)}"><span class="dot ${phaseDot(phase)}"></span>${phase}</span>`
              : ""
          }
          ${actionsHtml || ""}
        </div>
      </header>`;
  }

  function defaultActions() {
    return `
      <button class="btn icon" type="button" data-action="refresh" data-tip="刷新">${ico("refresh")}</button>
      <a class="btn s" href="prompts.html">${ico("bot")} Agent 提示词</a>
      <a class="btn f" href="launch.html">${ico("plus")} 发布任务</a>`;
  }

  function renderInspector(options = {}) {
    const d = window.SB_MOCK.dashboard;
    const task = options.task || d.details.task;
    const phase = options.phase || task.phase || "active";
    const criteria = (d.details.criteria || [])
      .map(
        (c) =>
          `<div class="criterion"><span>${escapeHtml(c.kind)}</span><p>${escapeHtml(c.content)}</p></div>`
      )
      .join("");

    const gaps = (d.gaps || []).filter((g) => g.status === "open");
    const blockers = (d.blockers || []).filter((b) => b.status === "open");

    const phaseAction =
      phase === "paused"
        ? `<button class="btn f block" type="button" data-action="resume">${ico("play")} 恢复任务</button>`
        : phase === "active"
          ? `<button class="btn s block" type="button" data-action="pause">${ico("pause")} 暂停任务</button>`
          : `<button class="btn s block" type="button" disabled>相位：${escapeHtml(phase)}</button>`;

    return `
      <aside class="inspector">
        <div class="inspector-heading">
          <div>
            <div class="kicker">Task Inspector</div>
            <h2>运行状态</h2>
          </div>
          ${ico("settings")}
        </div>
        <div class="inspector-section">
          <div class="kv copiable" data-copy="${escapeHtml(task.id)}" data-tip="点击复制 Task ID">
            <span>Task ID</span><strong class="mono">${shortId(task.id)}</strong>
          </div>
          <div class="kv"><span>目标分支</span><strong>${escapeHtml(task.target_branch || "当前分支")}</strong></div>
          <div class="kv"><span>自治策略</span><strong>${escapeHtml(task.autonomy_policy || "bounded")}</strong></div>
          <div class="kv"><span>完成 Epoch</span><strong>${task.completion_epoch ?? "—"}</strong></div>
          <div class="kv"><span>Plan Version</span><strong>${task.plan_version ?? "—"}</strong></div>
          <div class="kv"><span>事件游标</span><strong>${d.health?.event_cursor ?? "—"}</strong></div>
        </div>
        <div class="inspector-section">
          <h3>完成条件 <span class="count">${(d.details.criteria || []).length}</span></h3>
          ${criteria || `<div class="muted">无完成条件</div>`}
        </div>
        ${
          gaps.length || blockers.length
            ? `<div class="inspector-section">
                <h3>关注点 <span class="count">${gaps.length + blockers.length}</span></h3>
                ${gaps
                  .map(
                    (g) =>
                      `<div class="criterion"><span>${escapeHtml(g.severity || "gap")}</span><p>${escapeHtml(g.title)}</p></div>`
                  )
                  .join("")}
                ${blockers
                  .map(
                    (b) =>
                      `<div class="criterion"><span>blocker</span><p>${escapeHtml(b.reason)}</p></div>`
                  )
                  .join("")}
              </div>`
            : ""
        }
        <div class="inspector-actions">
          ${phaseAction}
        </div>
        <div class="inspector-section">
          <h3>快速跳转</h3>
          <div class="col-gap">
            <a class="btn s block" href="prompts.html">${ico("bot")} Agent 提示词</a>
            <a class="btn s block" href="launch.html">${ico("plus")} 发布新任务</a>
            <a class="btn s block" href="diagnostics.html">${ico("diagnostics")} 打开诊断</a>
          </div>
        </div>
      </aside>`;
  }

  function bindShellEvents(root) {
    root.querySelectorAll("[data-theme-toggle]").forEach((btn) => {
      btn.addEventListener("click", cycleTheme);
    });

    root.querySelectorAll("[data-action='refresh']").forEach((btn) => {
      btn.addEventListener("click", () => {
        btn.classList.add("is-refreshing");
        toast({ title: "已刷新", message: "模拟同步 Workspace 状态", tone: "ok", duration: 1800 });
        setTimeout(() => btn.classList.remove("is-refreshing"), 800);
      });
    });

    root.querySelectorAll("[data-action='pause']").forEach((btn) => {
      btn.addEventListener("click", () => {
        toast({ title: "任务已暂停", message: "task_transition · pause（模拟）", tone: "ok" });
        btn.outerHTML = `<button class="btn f block" type="button" data-action="resume">${ico("play")} 恢复任务</button>`;
        bindShellEvents(root);
        const pill = root.querySelector(".page-header .pill");
        if (pill) {
          pill.className = "pill w";
          pill.innerHTML = `<span class="dot warn"></span>paused`;
        }
      });
    });

    root.querySelectorAll("[data-action='resume']").forEach((btn) => {
      btn.addEventListener("click", () => {
        toast({ title: "任务已恢复", message: "task_transition · resume（模拟）", tone: "ok" });
        btn.outerHTML = `<button class="btn s block" type="button" data-action="pause">${ico("pause")} 暂停任务</button>`;
        bindShellEvents(root);
        const pill = root.querySelector(".page-header .pill");
        if (pill) {
          pill.className = "pill ok";
          pill.innerHTML = `<span class="dot ok"></span>active`;
        }
      });
    });

    root.querySelectorAll("[data-action='switch-workspace']").forEach((btn) => {
      btn.addEventListener("click", () => {
        toast({ title: "切换 Workspace", message: "将打开系统目录选择器（模拟）", tone: "info" });
      });
    });

    root.querySelectorAll("[data-copy]").forEach((el) => {
      el.addEventListener("click", async () => {
        const value = el.getAttribute("data-copy") || "";
        try {
          await navigator.clipboard.writeText(value);
          toast({ title: "已复制", message: shortId(value, 16), tone: "ok", duration: 1600 });
        } catch {
          toast({ title: "复制失败", message: value, tone: "err" });
        }
      });
    });

    updateThemeButtons();
  }

  function mountShell(options) {
    const {
      activePage,
      activeTaskId = "task_auth_impl",
      title,
      eyebrow,
      phase = "active",
      subtitle,
      actionsHtml,
      contentHtml,
      showInspector = true,
      chromeTitle = "SessionBridge",
      previewLabel,
      tasks,
    } = options;

    const root = document.getElementById("app");
    if (!root) return;

    const shellClass = showInspector ? "app-shell" : "app-shell no-rail";
    const pageLabel =
      previewLabel ||
      PAGES.find((p) => p.id === activePage)?.label ||
      eyebrow ||
      "页面";

    // 若 body 开头没有 preview-bar，由壳层插入到 app 之前
    if (!document.querySelector(".preview-bar")) {
      const bar = document.createElement("div");
      bar.innerHTML = renderPreviewBar(pageLabel);
      const node = bar.firstElementChild;
      if (node) document.body.insertBefore(node, root);
    }

    root.innerHTML = `
      <div class="win">
        <div class="chrome">
          <div class="dots"><i></i><i></i><i></i></div>
          ${escapeHtml(chromeTitle)}
        </div>
        <div class="vp auto">
          <div class="${shellClass}">
            ${renderSidebar(activePage, activeTaskId, { tasks })}
            <main class="main-panel">
              ${renderHeader({
                eyebrow,
                title,
                phase,
                subtitle,
                actionsHtml: actionsHtml !== undefined ? actionsHtml : defaultActions(),
              })}
              <section class="content">${contentHtml}</section>
            </main>
            ${showInspector ? renderInspector({ phase, task: options.task }) : ""}
          </div>
        </div>
      </div>`;

    bindShellEvents(root);
  }

  window.SB_SHELL = {
    PAGES,
    shortId,
    relativeMicros,
    phaseDot,
    phasePill,
    escapeHtml,
    ico,
    toast,
    applyTheme,
    cycleTheme,
    currentTheme,
    defaultActions,
    mountShell,
    renderSidebar,
    renderHeader,
    renderInspector,
    renderPreviewBar,
  };
})();
