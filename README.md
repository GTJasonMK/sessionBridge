# SessionBridge

本地工作区协作桌面应用。一个项目目录对应一个工作区；每个工作群有一位老板、一位协调者和若干执行者。支持 Claude Code / Codex 混合编组。

## 使用

1. 在 CC Switch 中配置供应商，安装所需的 `claude`、`codex` CLI。
2. 打开 SessionBridge，选择项目目录，创建工作群。
3. 在成员面板分别选择 CLI、供应商和模型，保存配置，然后初始化。
4. 向协调者发送任务；通过收件人选择器选择单播、多播或所有人。
5. 在群里查看分工、协作、权限请求和验证结果。验收通过后自动合入本地目标分支。

普通目录可以创建群和聊天。初始化隔离执行环境前，项目必须是有初始提交、工作目录洁净的 Git 仓库。每位执行者和协调者使用独立 worktree，不覆盖用户未提交的改动。

关闭窗口保留后台任务；暂停群会停止成员。项目面板中的“停止后台并退出”停止所有成员并暂停活跃工作群。

## 构建与启动

支持 Linux 本地文件系统，需要 Rust 1.95、Node.js 20.19+、Git、Bubblewrap（bwrap）、WebKitGTK 4.1 和 GTK 3。

```bash
cargo build --bin sessionbridge
cd gui
npm ci
npm run tauri dev
```

CLI 必须与桌面程序位于同一目录或位于 PATH。也可以用 `SESSIONBRIDGE_CLI` 指定绝对路径。

```bash
cargo build --release --bin sessionbridge
cd gui
npm run tauri build -- --no-bundle
```

安装后运行 `sessionbridge gui`。桌面程序通过 Tauri 调用本地服务，不执行拼接的 shell 命令。

## CLI 与 MCP

```bash
sessionbridge call --workspace /path/to/project room.create --input '{"title":"修复登录"}'
sessionbridge call --workspace /path/to/project room.list
sessionbridge call --workspace /path/to/project room.get --input '{"room":"ROOM_ID"}'
sessionbridge call --workspace /path/to/project history --input '{"room":"ROOM_ID"}'
```

较长 JSON 用 `--input -` 从 stdin 读取。CLI 为没有 request_id 的直接调用生成 UUID；显式重试时应复用同一 ID。

Agent 使用随会话装配的 `sessionbridge` MCP 工具，工具负责身份认证、群隔离和协议校验。详见 [协议](docs/protocol.md) 和 [协作 Skill](skills/sessionbridge-collaboration/SKILL.md)。

## 状态与配置

新版只使用：

```text
$XDG_STATE_HOME/sessionbridge-v2/
  bridge.sock
  operator.token
  workspaces/<canonical-path-hash>/
    state.db
    workspace.json
    members/<member-id>/runtime/
    worktrees/<room-id>/
```

可用 `SESSIONBRIDGE_V2_STATE` 指定独立测试目录。旧版状态目录不会被打开；v2 状态目录中的 user_version=1 过渡 schema 会原子迁移到当前 schema。工作区按规范化项目路径识别。

CC Switch 数据库只读访问，默认位置 `~/.cc-switch/cc-switch.db`，可用 `SESSIONBRIDGE_CCSWITCH_DB` 指定。每个成员持有独立配置快照，不调用全局供应商切换，也不默认使用其他供应商或全局登录。变更配置需要先停止成员。

权限边界来自原生 CLI；worktree 不是安全沙箱，同一系统用户下的进程也不是互不信任的租户。内部协作 MCP 工具已授权，其余原生权限请求在群里明确处理。宿主验证命令通过 Bubblewrap 执行，仅群整合目录可写，并隔离网络；缺少沙箱时不会退回无隔离执行。

## 验证

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --no-run
timeout 60s cargo test --workspace
cargo build --bin sessionbridge
python3 tests/bridge_runtime.py target/debug/sessionbridge
cd gui
npm test
npm run build
```

浏览器测试使用实际 Rust 服务，仅替换 Tauri 的桌面传输和目录选择：

```bash
# 先启动 gui 的 Vite 开发服务，再运行：
python3 tests/bridge_ui.py target/debug/sessionbridge
```

需要 Python Playwright 和 Chromium。截图写入 `/tmp/sessionbridge-ui-review`。

可选原生 CLI 检查：

- `python3 tests/bridge_claude_native.py target/debug/sessionbridge`：真实 Claude CLI，使用本地确定性 Messages API，无模型费用。
- `python3 tests/bridge_live.py target/debug/sessionbridge`：使用 CC Switch 当前 Claude/Codex 配置调用真实模型，会产生供应商费用。
- 后者支持追加 `claude` 或 `codex`，仅检查指定 CLI 初始化。供应商不可用、模型未部署或认证失败均明确失败，不切换配置。

历史方案和静态 UI 草图保留在 docs 中；当前合同以 docs/protocol.md 为准。
