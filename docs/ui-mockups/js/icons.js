/**
 * SessionBridge UI Mockups — 内联 SVG 图标
 * 统一 24 视口、1.75 描边，避免 emoji 跨平台不一致
 */
(function () {
  const paths = {
    overview: '<circle cx="12" cy="12" r="9"/><circle cx="12" cy="12" r="3"/>',
    work: '<path d="M4 7h16M4 12h16M4 17h10"/>',
    agents: '<path d="M8 14a4 4 0 1 1 8 0"/><circle cx="12" cy="8" r="3"/><path d="M4 20a6 6 0 0 1 4-3.5M20 20a6 6 0 0 0-4-3.5"/>',
    chat: '<path d="M5 6h14a2 2 0 0 1 2 2v7a2 2 0 0 1-2 2H10l-4 3v-3H5a2 2 0 0 1-2-2V8a2 2 0 0 1 2-2z"/>',
    activity: '<path d="M4 14h3l2-6 3 10 2-6h4"/>',
    diagnostics: '<circle cx="12" cy="12" r="3"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4"/>',
    plus: '<path d="M12 5v14M5 12h14"/>',
    refresh: '<path d="M20 12a8 8 0 1 1-2.3-5.6"/><path d="M20 4v5h-5"/>',
    folder: '<path d="M3 7a2 2 0 0 1 2-2h4l2 2h8a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z"/>',
    chevron: '<path d="M9 6l6 6-6 6"/>',
    chevronLeft: '<path d="M15 6l-6 6 6 6"/>',
    send: '<path d="M4 12 20 4l-6 16-2-7-8-1z"/>',
    reply: '<path d="M9 14 4 9l5-5"/><path d="M4 9h10a6 6 0 0 1 6 6v2"/>',
    copy: '<rect x="8" y="8" width="12" height="12" rx="2"/><path d="M4 16V6a2 2 0 0 1 2-2h10"/>',
    check: '<path d="M5 12.5 9.5 17 19 7"/>',
    pause: '<path d="M8 6v12M16 6v12"/>',
    play: '<path d="M8 6v12l10-6z" class="fill-path"/>',
    alert: '<path d="M12 4 3 19h18L12 4z"/><path d="M12 10v4M12 16.5v.5"/>',
    x: '<path d="M7 7l10 10M17 7 7 17"/>',
    search: '<circle cx="11" cy="11" r="6"/><path d="m20 20-3.5-3.5"/>',
    bot: '<rect x="5" y="8" width="14" height="11" rx="3"/><path d="M9 8V6a3 3 0 0 1 6 0v2M9 13h.01M15 13h.01M10 16h4"/>',
    sun: '<circle cx="12" cy="12" r="4"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4"/>',
    moon: '<path d="M20 14.5A7.5 7.5 0 1 1 9.5 4 6 6 0 0 0 20 14.5z"/>',
    shield: '<path d="M12 3 5 6v5c0 4.5 3 7.5 7 9 4-1.5 7-4.5 7-9V6l-7-3z"/>',
    terminal: '<rect x="3" y="5" width="18" height="14" rx="2"/><path d="m7 10 3 2-3 2M12 14h5"/>',
    list: '<path d="M8 7h12M8 12h12M8 17h12M4 7h.01M4 12h.01M4 17h.01"/>',
    graph: '<circle cx="6" cy="7" r="2.5"/><circle cx="18" cy="7" r="2.5"/><circle cx="12" cy="17" r="2.5"/><path d="M8 8.5 10.5 15M16 8.5 13.5 15"/>',
    settings: '<circle cx="12" cy="12" r="3"/><path d="M12 3v2M12 19v2M3 12h2M19 12h2M5.6 5.6l1.4 1.4M17 17l1.4 1.4M18.4 5.6 17 7M7 17l-1.4 1.4"/>',
  };

  function icon(name, className = "ico") {
    const body = paths[name] || paths.overview;
    // play 等填充路径
    const html = body.replace(
      'class="fill-path"',
      'fill="currentColor" stroke="none"'
    );
    return `<svg class="${className}" viewBox="0 0 24 24" aria-hidden="true">${html}</svg>`;
  }

  window.SB_ICONS = { paths, icon };
})();
