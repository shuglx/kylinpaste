# KylinPaste

> 麒麟桌面(Linux)剪贴板历史管理工具 —— 常驻后台,一个热键呼出,搜一下、按一下就粘回刚才那个窗口。

Tauri v1 + React 18 实现:后端 Rust 负责剪贴板监听、全局热键、焦点与按键注入,前端只做界面。

## 功能

- **自动记录**:纯文本 / 富文本 / 图片 / 文件,按内容哈希去重,重复复制自动置顶
- **呼出即搜**:全局热键默认 `Ctrl+Shift+V`(macOS `⌘⇧V`),可在设置里改绑;每次呼出光标自动落在搜索框
- **一键粘贴**:点击条目,或 `Ctrl(macOS ⌘)+1~9` 直接把该条粘回"呼出前那个窗口"(按住修饰键时才显示序号角标)
- **搜索与分类**:全部 / 文字 / 图片 / 文件 / 链接 / 收藏(链接独立成类,不混进文字)
- **分组与收藏**:每条可打一个分组(彩色圆点标签),分组可下拉筛选;收藏与分组的记录**不占保留上限、也不会被自动清理**
- **清理**:一键清理"临时记录"(收藏与分组一律保留),或单条删除(受保护的记录会二次确认)
- **设置**:开机自启(Linux)、便捷粘贴开关、快捷键改绑、最大条目数、界面语言(中文 / English)
- **只在本机**:历史与设置是 JSON,图片与缩略图落盘,没有任何网络传输功能

## 安装

| 平台 | 产物 | 步骤 |
|---|---|---|
| Linux (arm64) | `kylinpaste_*.deb` | `sudo dpkg -i kylinpaste_*.deb`,然后从应用菜单启动 |
| macOS (Apple Silicon) | `KylinPaste_*.dmg` | 拖进「应用程序」→ **首次打开请右键 → 打开**(未签名),或先执行 `xattr -cr /Applications/KylinPaste.app`;再到「系统设置 → 隐私与安全性 → 辅助功能」勾选 **KylinPaste**(否则无法模拟粘贴) |

> 开发模式下(macOS)权限是记在**启动它的终端/IDE** 上的,且每次重编译都可能失效;打包安装后权限才记在应用自己身上。

## 使用

| 操作 | 说明 |
|---|---|
| `Ctrl+Shift+V`(macOS `⌘⇧V`) | 呼出 / 隐藏窗口 |
| 点击某条记录 | 粘贴到"上一个活动窗口" |
| `Ctrl`(macOS `⌘`)+ `1`~`9` | 快捷粘贴对应位置(可在设置里关掉) |
| `Esc` | 收起窗口(有弹层时先关弹层) |
| 齿轮按钮 | 打开设置(常规 / 关于) |

## 数据位置

| 平台 | 目录 |
|---|---|
| Linux | `~/.local/share/com.kylinpaste.app/` |
| macOS | `~/Library/Application Support/com.kylinpaste.app/` |

- `history.json` —— 历史记录(含收藏、分组)
- `settings.json` —— 设置
- `clipboard_images/` —— 图片原图与缩略图(`thumbs/`)

**最大条目数只限制"临时记录"**:收藏、分组的记录不计入上限,也不会被自动淘汰(与界面里"清理"按钮的规则一致)。删除记录时会连带删掉我们自己复制/生成的图片与缩略图,**不会碰用户的原文件**。

## 开发

```bash
npm install
npm run tauri:dev                                  # 开发(前端热更新)
npm run tauri:build                                # 打包
npm run build                                      # 只构建前端
cargo test --manifest-path src-tauri/Cargo.toml    # 状态层/捕获判据单测
```

- 环境:Node 20+、Rust stable;Linux 需要 `libwebkit2gtk-4.0-dev libgtk-3-dev libx11-dev` 等,macOS 需要 Xcode Command Line Tools
- **目标机(Linux)构建走容器**:CI 用 `ubuntu-24.04-arm` runner + `docker/Dockerfile.build`(focal 基座)保证 glibc 兼容,再由 `scripts/package-deb.sh` 打 deb。本地预演:

```bash
docker build -t kylinpaste-build -f docker/Dockerfile.build docker/
docker run --rm -v "$PWD:/work" kylinpaste-build cargo build --release --manifest-path src-tauri/Cargo.toml
```

## 代码结构

```
src/                        前端(React)
  App.jsx                   列表页:搜索/筛选/分组/收藏/清理/删除/各类弹层
  Settings.jsx              设置页(常规、关于)
  icons.jsx                 图标(与参考项目同源的 Tabler 图标)
  i18n.js                   中英文文案
  app.css                   样式
src-tauri/src/              后端(Rust)
  main.rs                   启动流程、窗口显示/隐藏、单实例、命令注册
  clipboard_service.rs      剪贴板监听与捕获(类型优先级、去重、图片落盘)
  paste.rs                  粘贴链路:写剪贴板 → 隐藏窗口 → 归还焦点 → 注入快捷键
  state.rs                  历史记录(内存 + JSON 落盘、收藏/分组/上限/删除)
  settings.rs               设置(JSON,改动即存)
  hotkey.rs                 全局热键注册与运行时改绑
  x11.rs                    Linux 底层:X11 全局热键、焦点归还、XTEST 注入按键
  appinfo.rs                来源应用识别(macOS NSWorkspace / Linux WM_CLASS)
  system.rs                 单实例 socket、Linux 开机自启(XDG)
```

## 实现要点(踩过的坑)

1. **不用 Tauri v1 内置的全局热键**:tao 在 Linux 上按"按键**释放**瞬间的修饰键状态"判定命中,用户先松 Ctrl 再松 V 就匹配不上(表现为"按好几下才出来"),而且只覆盖部分锁定键组合。Linux 改为自建 `XGrabKey` + `KeyPress` 判定,抓键失败才回退内置实现。
2. **粘贴链路要按顺序**:写剪贴板 → 隐藏窗口 → 归还焦点(记录"呼出剪贴板之前的活动窗口")→ 注入 `Ctrl+V`;粘贴命令必须是 `#[tauri::command(async)]`,否则窗口请求会排在命令之后处理,结果是"窗口消失了但什么都没粘上"。
3. **剪贴板上下文复用**:clipboard-rs 在 X11 下每次 `new()` 都会新建 2 条 X 连接和一个常驻线程,监听线程每次变化都读一次剪贴板,复制几百次就会耗尽 X 客户端名额(默认 256)。
4. **捕获优先级**:文件 > 文字/富文本 > 图片 > 纯文本。Word/WPS/LibreOffice 复制**文字**时会顺带放一张位图副本,只按"有图就算图"判断会把复制文字误记成截图。
5. **macOS 的两个坑**:粘贴是 `⌘V` 不是 `Ctrl+V`;tao 0.16 把 ⌘ 的 keyUp 转发给 `[NSApp keyWindow]`,窗口已隐藏时该值为 nil 会直接 abort —— 所以凡是"让窗口消失"的动作都等 ⌘ 松开再执行。
6. **保留上限只作用于临时记录**:收藏/分组不占上限、也不被自动淘汰;为此收藏从 localStorage 挪进了记录(启动时自动迁移一次)。

## CI

`.github/workflows/build-deb.yml` 一次构建两个平台:

| Job | Runner | 产物 |
|---|---|---|
| deb (arm64) | `ubuntu-24.04-arm` + focal 容器 | `dist-deb/*.deb` |
| dmg (macOS arm64) | `macos-14`(Apple Silicon) | `src-tauri/target/release/bundle/dmg/*.dmg` |

推送 `main` 触发构建并上传产物;推送 `v*` tag 时把 deb 与 dmg 一起附到 GitHub Release。

## 已知限制

- 开机自启目前只实现了 Linux(XDG autostart),macOS 上该开关暂不生效
- 图片只向剪贴板 owner 索取 `image/png`:只提供 bmp/tiff 的少数程序(Wine、部分 Java 程序)可能漏抓
- 富文本只保存 HTML 原文,内嵌图片不做落盘
- 界面渲染依赖系统 webview(WebKitGTK / WKWebView),低版本 WebKitGTK(如 2.28)未实测

## 许可

[MIT](LICENSE)
