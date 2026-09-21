# KylinPaste 交接文档

> 最后更新:2026-09-21 · 阶段:PoC 完成,待目标机验证

## 1. 项目概述

把开源项目 QuickClipboard(Tauri 2 + React)的**核心剪贴板能力**移植到银河麒麟桌面操作系统 V10 SP1(ARM64 飞腾 D2000/8),通过 GitHub Actions 构建 deb 安装包,开发与预览在 macOS 上进行。

- 仓库:https://github.com/shuglx/kylinpaste
- 参考项目:`ref/QuickClipboard`(Apache 2.0,**仅本地保留,不入库**)
- 技术栈:**Tauri 1.8.3**(不是 v2,原因见下)+ React 18 + Vite 5 + Rust

## 2. 关键环境事实与选型依据

以下决策均经过目标机实测确认,是整个项目的技术地基,**不要轻易更改**:

| 事实 | 结论 |
|---|---|
| 目标机只有 webkit2gtk-4.0(2.28.1-1kylin1k7),无 4.1 | **Tauri 2 不可用**(硬依赖 4.1/libsoup3),锁定 Tauri v1 |
| webkit 2.28 ≈ 2020 年引擎(约 Safari 13) | 前端构建 target 锁 `es2020/safari13`;React 用 18 不用 19 |
| X11 会话(UKUI) | clipboard-rs / enigo(x11rb)可用;粘贴模拟无权限模型 |
| glibc 2.31(Ubuntu 20.04 基座) | CI 在 ubuntu:20.04 arm64 容器内构建,锚定 glibc |
| 疑似银行(ICBC)内网环境 | 砍掉自动更新;deb 依赖必须目标机自带 |
| focal 源 arm64 的 webkit2gtk 恰为 2.28.1-1 | Dockerfile 把 webkit 全家 6 个包 **pin 到 2.28.1-1 + `--allow-downgrades`**,与目标机 ABI 对齐(不 pin 会拉到 2.38,产物可能引用目标机不存在的符号) |
| tauri v1 无 single-instance / autostart 插件 | 手工实现:unix socket 单实例 / XDG `.desktop` 自启 |
| Tauri v1 的 global-shortcut 是核心内置 feature | `Cargo.toml` 的 `features = ["system-tray", "global-shortcut"]`,不是插件 |

## 3. 代码结构

```
kylinpaste/
├── src/                        # React 前端
│   ├── App.jsx                 # 历史列表/搜索/数字键粘贴/明暗主题/错误横幅
│   ├── app.css                 # CSS 变量实现明暗主题(data-theme)
│   └── main.jsx
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json         # v1 格式(注意:identifier 在 tauri.bundle 下)
│   └── src/
│       ├── main.rs             # 入口:Builder/托盘/热键/窗口事件/toggle 三态逻辑
│       ├── state.rs            # 内存历史(VecDeque,500 条上限,哈希去重,抑制窗口)
│       ├── clipboard_service.rs# clipboard-rs 监听 + 多类型捕获(文件>图片>富文本>文本)
│       ├── paste.rs            # 粘贴:隐藏窗口→写剪贴板→enigo 模拟 Ctrl+V
│       ├── tray.rs             # 托盘菜单
│       └── system.rs           # 单实例(unix socket)+ 开机自启(XDG)
├── docker/Dockerfile.build     # focal 构建镜像(webkit 锁 2.28.1-1)
├── scripts/
│   ├── gen-icons.mjs           # 纯 Node 标准库生成应用图标
│   └── package-deb.sh          # 手工 dpkg-deb 打包(不用 tauri bundler,完全控制 Depends)
└── .github/workflows/build-deb.yml
```

## 4. 功能状态

用户选定的正式范围:**A 全部 + B1/B2/B6 + C 全部 + H1/H2/H3 + F1**
(明确排除:Emoji/图库、贴图、OCR、同步/传输、自动更新)

PoC 已实现:

- A1 全类型记录 / A2 哈希去重 / A3 搜索 / A6 内存版上限(500 条)
- B1 点击粘贴回前一应用 / B2 数字键 1-9 / B6 全局热键 Ctrl+Alt+V
- H1 托盘 / H2 开机自启(XDG)/ H3 单实例 / F1 明暗主题
- 图片落盘为 PNG(`~/.../com.kylinpaste.app/clipboard_images/`)

**尚未实现(正式迁移阶段做)**:

- A5 SQLite 持久化(现为内存版,重启丢历史)→ 参考 QC `services/database`
- A4 虚拟列表(现只渲染前 100 条)→ 考虑 react-virtuoso,需验证 webkit 2.28 兼容
- C 收藏与分组 → 参考 QC `favoritesStore`/`groupsStore` 与后端对应表

## 5. 开发与构建

### mac 本地预览

```bash
export PATH="$HOME/.cargo/bin:$PATH"   # rustup 装在 ~/.cargo,未写入 PATH 时需要
npm run tauri:dev                       # vite :5173 + WKWebView
cargo check --manifest-path src-tauri/Cargo.toml
```

### CI 与发布

- push `main` → 构建验证,deb 存为 artifact(`kylinpaste-arm64-deb`,Actions 页底部下载,需登录、会过期)
- push tag `v*` → **自动创建 GitHub Release 并附上 deb**(免登录可下载,给目标机用这个)
- 首次构建约 10-15 分钟,后续有 cargo 缓存(`actions/cache`,key 为 Cargo.lock 哈希)
- 修改 Rust 代码后别依赖 tauri dev 自动重编译,**建议 Ctrl+C 重跑**(mac 调试时踩过旧代码坑)

```bash
# 发一个版本:
git tag v0.1.0 && git push origin v0.1.0
```

### 本地 Docker 预演(可选,Apple Silicon 原生 arm64)

```bash
docker build -t kylinpaste-build -f docker/Dockerfile.build docker/
docker run --rm -v "$PWD:/work" \
  -v "$PWD/.ci-cache/cargo/registry:/root/.cargo/registry" \
  -v "$PWD/.ci-cache/cargo/git:/root/.cargo/git" \
  kylinpaste-build cargo build --release --manifest-path src-tauri/Cargo.toml
```

### 目标机安装

```bash
sudo dpkg -i kylinpaste_0.1.0_arm64.deb
kylinpaste    # 或应用菜单启动;出问题用终端跑看报错
```

## 6. 目标机验证清单(当前待办)

1. deb 安装依赖解析通过(依赖均为麒麟自带:libwebkit2gtk-4.0-37 / libgtk-3-0 / libappindicator3-1 / libxkbcommon0)
2. **窗口渲染**(webkit 2.28 × React 18,本 PoC 最大验证目标)
3. 复制文本/图片/文件 → 列表实时出现
4. Ctrl+Alt+V 全局热键唤起/隐藏(UKUI 下)
5. **点击条目/数字键 → 粘贴回前一应用**(X11 焦点交还 + XTEST)
6. 托盘右键菜单全项
7. 单实例(二次启动退出并唤起旧实例)
8. 开机自启(检查 `~/.config/autostart/kylinpaste.desktop`)
9. 关闭按钮 → 隐藏到托盘不退出

## 7. 已知问题与遗留风险

| 问题 | 状态 | 说明 |
|---|---|---|
| mac 上粘贴模拟不生效 | 已搁置 | 三步日志全通过(权限 OK、事件已发),目标应用无输入。仅影响 mac 预览便利性,**不影响 Linux 目标**(X11/XTEST 无权限模型)。后续可试 `Settings` 的 `independent_of_keyboard_state: false` |
| webkit 2.28 前端兼容 | 待目标机实测 | React 18 + es2020/safari13 target,大概率 OK;若渲染异常,降级方向:React 17 / Preact |
| gtk/soup 构建版本略新 | 低风险 | 容器内 gtk 走 focal-updates(3.24.20),目标机 3.24.18 基座;soname 稳定,理论上兼容 |
| X11 焦点交还 | 待目标机实测 | 依赖 UKUI 窗口管理器在 unmap 后归还焦点;不生效则需改为显式激活前一窗口(active-win-pos-rs / x11rb) |
| vite 扫描 ref/ | 已解决 | `vite.config.js` 显式 `rollupOptions.input`,否则 dev 扫描器爬 ref/QuickClipboard 的多页面源码报错 |

## 8. QuickClipboard 参考映射(正式迁移用)

| 要迁移的能力 | QC 源码位置(ref/QuickClipboard/src-tauri/src/) |
|---|---|
| SQLite 持久化 | `services/database/` |
| 剪贴板捕获完整版(重试/多格式) | `services/clipboard/capture.rs` |
| 收藏/分组 | `services/database/` 相关表 + 前端 `src/shared/store/favoritesStore、groupsStore` |
| 粘贴选项(纯文本/格式) | `services/paste/` |
| 主题/样式 | `src/shared/styles/` |

QC 是 Tauri 2 代码,移植时注意 API 差异:窗口/托盘/事件 API、插件版本、配置格式(见 §2)。

## 9. 决策备忘

- 范围谈判时的排除项:Emoji/符号/图库、贴图到屏幕、OCR、同步/传输、自动更新(内网)
- mac 调试要点:粘贴模拟需辅助功能权限且归属到实际启动终端;`window.hide()` 在 mac 会让应用保持激活态且无键窗口,必须 `app.hide()`(代码已按平台 cfg 分支处理)
- 首次搭建时 vite 需显式入口、tauri v1 配置字段位置、enigo 的 `Key::Unicode` 中文输入法查表风险(mac 已用 `Key::Other(9)` 规避)
- **Rust std 不支持前导 NUL 的抽象 unix socket 路径**(bind 返回 InvalidInput "paths must not contain interior null bytes"),`SocketAddr::from_abstract_name` 也未进 stable——v0.1.0 因此在麒麟上"每次启动都判定已有实例直接退出"。v0.1.1 起改用文件 socket(XDG_RUNTIME_DIR 优先)+ 陈旧 socket 探活清理,且单实例任何异常都只降级、绝不退出应用
- 目标机的 AT-SPI/dbind-WARNING(GtkKit 无 a11y 总线)无害,已在 main 里 `NO_AT_BRIDGE=1` 消除
