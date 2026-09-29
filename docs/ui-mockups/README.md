# SessionBridge UI Mockups

参照 [A4N `docs/ui-mockups`](../../../A4N/docs/ui-mockups) 的 iOS / 简约 / 朴素 / 丝滑风格，为 SessionBridge Operator Console 产出的**可打开预览**。仅覆盖前端布局与样式；数据字段对齐现有 `gui/src/types.ts` 与 Tauri 命令。

## 快速查看

在仓库根目录：

```bash
# 任意静态服务器均可
python3 -m http.server 8765 --directory docs/ui-mockups
# 浏览器打开 http://127.0.0.1:8765/
```

或直接用浏览器打开 `docs/ui-mockups/index.html`（部分浏览器对本地 `file://` 的模块限制较少，本目录为纯静态 HTML/CSS/JS，一般可直接打开）。

## 风格要点

| 维度 | 选择 |
|------|------|
| 气质 | iOS 纸面、玻璃顶栏、轻阴影、大圆角 |
| 主色 | 静谧靛蓝 `rgb(72 98 176)`（相对 A4N 暖棕的微调） |
| 动效 | hover 抬升、主按钮扫光、卡片 rise-in、live 呼吸、Toast / 骨架；尊重 `prefers-reduced-motion` |
| 壳层 | 侧栏 · 主区 · Inspector 三栏（窄屏收 Inspector） |
| 主题 | 系统 / 浅色 / 深色，`localStorage` 键 `sessionbridge-mock-theme` |

## 细节清单（本轮已补）

- 统一 SVG 图标（`js/icons.js`），避免 emoji 跨平台差异
- 预览顶栏面包屑、导航角标（群聊数 / 关注数 / 诊断警告）
- 概览：缺口与阻塞列表、流水线进度条
- 工作项：即时筛选、SVG 依赖箭头图
- 群聊：回复条、字数统计、发送 Toast、空内容校验
- 发布：同侧栏内编辑 ↔ Preflight 切换、脏工作区拒绝态
- 提示词：复制成功态 + Toast
- 边界页 `states.html`：空任务 / 骨架加载 / 错误重试 / 暂停条幅
- Inspector：可复制 Task ID、关注点摘要、暂停/恢复模拟
- Toast、工具提示、主题三态切换

## 与后端能力的对应

| 预览页 | 后端 / Tauri |
|--------|----------------|
| `pages/workspace-gate.html` | `workspace_restore` / `open` / `initialize` |
| `pages/overview.html` | `task_snapshot`（pipeline / deliverables / health / events） |
| `pages/work.html` | work_items / relations / speculative_work |
| `pages/agents.html` | participants / invitations / waits |
| `pages/chat.html` | `chat_send` + `chat_messages` |
| `pages/activity.html` | `recent_events` |
| `pages/diagnostics.html` | `doctor_run` / `collaboration_diagnostics` / `logs_tail` |
| `pages/launch.html` | `launch_preflight` / `launch_execute` |
| `pages/prompts.html` | `prompt_list` / `prompt_show` |

## 目录职责

```
docs/ui-mockups/
├── index.html           # 设计说明 + 导航
├── README.md            # 本文件
├── css/                 # 令牌 / 原子 / 布局 / 页面 / 抛光
├── js/                  # 模拟数据 + 壳层拼装
├── components/          # 组件展柜（非业务页）
└── pages/               # 各业务页独立 HTML
```

**不要**把所有页面塞进单个 HTML：共享样式在 `css/`，共享壳在 `js/shell.js`，页面只写内容区。

## 落地到 `gui/` 的建议

1. 将 `css/tokens.css` 变量迁入 `gui/src/styles.css` 的 `:root`。
2. 按本 mockup 的 IA 拆分 `App.tsx`（pages + components），**不改** API 与 Query 契约。
3. 交互与 class 名可逐步对齐 mockup；动效层对齐 `polish.css`。

## 本地验证

```bash
# 文件齐全
find docs/ui-mockups -type f | sort

# 链接与引用无 404（简易检查）
grep -rhoE 'href="[^"]+"|src="[^"]+"' docs/ui-mockups --include='*.html' | sort -u
```
