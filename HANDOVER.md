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
│   ├── App.jsx                 # 历史列表/搜索/分类筛选/收藏/数字键粘贴/图钉置顶/Esc 收起
│   ├── app.css                 # 亮色极简样式(圆角卡片,无标题栏)
│   └── main.jsx
├── src-tauri/
│   ├── Cargo.toml
│   ├── tauri.conf.json         # v1 格式(注意:identifier 在 tauri.bundle 下;decorations/transparent)
│   └── src/
│       ├── main.rs             # 入口:Builder/托盘/热键/窗口事件/toggle 三态逻辑/置顶·隐藏命令
│       ├── x11.rs              # Linux 专属:自建全局热键(XGrabKey)/焦点归还(EWMH)/XTEST 注入
│       ├── state.rs            # 历史(VecDeque,500 条上限,哈希去重,抑制窗口)+ JSON 落盘
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

用户选定的正式范围:**A 全部 + B1/B2/B6 + C 全部 + H1/H2/H3**
(明确排除:Emoji/图库、贴图、OCR、同步/传输、自动更新)
后续调整:F1 明暗主题**取消**,界面只保留亮色;收藏改为前端 localStorage(见下)。

PoC 已实现:

- A1 全类型记录 / A2 哈希去重 / A3 搜索 / A6 上限 500 条
- B1 点击粘贴回前一应用 / B2 数字键 1-9 / B6 全局热键 Ctrl+Alt+V
- H1 托盘 / H2 开机自启(XDG)/ H3 单实例
- 历史 JSON 落盘(`<app_data_dir>/history.json`,改动后 500ms 防抖写入,启动载入)
- 定长队列上限 500 条(`state.rs::MAX_HISTORY`):超出后**只丢弃最旧的记录**(不再保留它的图片/文件路径信息),
  **不删除磁盘上任何文件**——这是明确的产品决定:本应用只保证自己 JSON 的记录数有上限,不负责清理磁盘
- 图片落盘为 PNG(`<app_data_dir>/clipboard_images/`,文件名 = sha256 前 16 位,内容寻址天然去重)
- 图片缩略图 `<app_data_dir>/clipboard_images/thumbs/<hash 前 16 位>.png`(长边 240px,捕获时顺带生成,
  零额外解码开销);前端用 asset 协议 + `convertFileSrc` 加载,取不到图时回退类型图标。
  该能力依赖三处一致:`tauri.conf.json` 的 `allowlist.protocol.asset` + `assetScope: ["$APPDATA/clipboard_images/**"]`
  与 Cargo feature `protocol-asset`;启动日志会自检并打印 `[资源] 缩略图 ... 在 asset scope 内: true`
- 列表标题一律由后端算好放进 `text`(前端只负责省略号):
  剪贴板图像数据(浏览器/Word 复制的图、截图工具)= `截图「长 × 宽」`;
  复制的图片**文件** = `文件名「所在目录」`;
  复制的文件 = `文件名「所在目录」`(多个文件 = `首个 等 N 个「目录」`);
  文字/富文本 = **只取第一行**
- 副标题固定是 `类型 · 来源应用`(来源拿不到就只显示类型;文字/富文本的类型是 `文字`/`链接`),
  有分组时再跟一个分组小标签(彩点 + 分组名,见 `app.css` 的 `.group-pill` / `.dot`)
- `source_app`(来源应用,统一封装在 `appinfo.rs::active_app_name`):
  - Linux/X11:当前活动窗口的 `WM_CLASS` class 部分(如 `firefox`/`Wps`/`XTerm`)
  - macOS:`NSWorkspace.frontmostApplication.localizedName`(如 `CodeBuddy CN`/`Safari`)
  - 前台/活动窗口就是本应用时**不记**(Linux 比 `_NET_WM_PID`,mac 比 `processIdentifier`,
    mac 用 pid 而不是 bundle id:开发模式直接跑 `target/debug` 可执行文件时 mainBundle 没有 bundle id)
  - 字段 `#[serde(default)]`,老 JSON 照常能读;拿不到就是 None(副标题只显示类型)
- 分组(C,每条记录最多一个):记录里的 `group` 字段,`cmd_set_group(id, group)` 写入
  (空白 = 取消分组,超长按 32 字符截断);**同一份内容重新复制时分组跟着走**
  (去重在 `state.rs::insert_record`,新记录无分组则继承旧记录的分组;收藏按哈希记在前端,天然也有)
- 行尾按钮顺序固定为 **分组(标签图标) → 收藏(星星) → 删除(垃圾桶)**:分组按钮弹小面板,
  可输入新分组或点已有分组标签;删除普通记录直接删,收藏过/分过组的先弹确认框
- 顶部图钉右边的**刷子**= 清理临时记录:只删"没收藏也没分组"的记录,收藏与分组的一律保留
  (保护名单在前端算——收藏在 localStorage、分组在记录里;ids 交给 `cmd_delete_records`)
- 图标同源于 ref 项目:**ref 用 Tabler Icons**(`@tabler/icons-webfont`,如 `ti ti-tag`/`ti ti-trash`/
  `ti ti-star`),我们把对应 SVG 路径直接搬进 `App.jsx`,行内/顶栏图标统一 **18×18、stroke 2**,
  避免各画各的导致大小不一。顶栏清理按钮用 **`trash-x`(带 X 的垃圾桶)**:Tabler 里没有 broom/毛刷
  这类图标(`brush` 是长柄笔刷,`broom` 不存在),而 ref 给"清空剪贴板历史"用的就是这个图标,直接沿用
- 分类条互斥:链接记录只在"链接"里出现,不再混进"文字"(`LINK_RE` 判断同时作用于两个分类,
  `kindTone` 里链接也优先于文字/富文本);缩略图色调按类型区分:
  **链接=紫**(独立一类)、**文字/富文本/图片=绿**、文件=橙
- 缩略图的圆角裁切放在内层 `.thumb-clip`,数字角标挂在 `.thumb` 上——角标带负偏移,裁切若放在
  `.thumb` 会把它切掉一角(只有图片类型会出这个问题)
- 分类条最后一个"分组"按钮 `margin-left: auto` 贴到与顶栏图标同一条右边线上,下拉菜单也按同一条线
  右对齐(窗口内边距 16px 处)
- 分组配色:`App.jsx::GROUP_COLORS` 是 mac 标签那 7 种颜色(从设计稿截图直接取样:
  `#ee6f6b`/`#f2a868`/`#f8d86b`/`#83d085`/`#629ef9`/白底灰环/`#a4a5a9`),按**分组名排序后循环取色**
  (≤7 个分组时颜色互不相同,第 8 个开始回到第一个);
  分组筛选下拉里"不筛选"= 取消筛选,菜单固定 `176px` 宽、贴窗口右边界(留 8px),行高 26px
- 删除会同时清掉该记录在 `clipboard_images/` 下的原图与缩略图(`state.rs::remove_record_files`,
  只删我们自己复制的文件,绝不碰用户的原文件);托盘"清空历史"仍是无条件全清
- **捕获优先级:文件 > 文字/富文本 > 图片 > 纯文本**(`clipboard_service::capture_with`)。
  文字优先于图片是因为 Word/WPS/LibreOffice 复制**文字**时会顺带在剪贴板放一份"渲染成位图"的副本
  (image/png、image/bmp),只看"有没有图"会把复制文字误记成截图;只有"没有可见文字"
  (截图工具;或富文本里只有一个 `<img>`——浏览器右键复制图片)才按图片记录。
  判据是 `html_has_visible_text`(去标签、去 `&nbsp;` 后是否还剩字母/数字/CJK),与 ref 项目
  QuickClipboard `capture.rs:is_image_only_html` 的语义一致(它也是文字/富文本在前、图片最后收敛)。
  验证方式:`osascript -l JavaScript` 用 NSPasteboard 一次声明 html+text+png 模拟 Word 剪贴板,
  记录应为 `html` 而不是截图(该判据的两个用例已写成单测)
- 复制的图片文件按图片处理(识别扩展名 → 复制一份 PNG 进 `clipboard_images/` + 缩略图),但记录里保留原始路径,
  **粘贴时同时提供 `image/png` 与 `text/uri-list`**(`clipboard-rs` 的 `set(vec![Image, Files])`,已在容器实测:
  TARGETS 同时含两者),粘到文件管理器还是文件、粘到文档/聊天还是图片
- 可解码的图片格式(png/jpg/gif/bmp/webp/tiff)由 `Cargo.toml` 的 `image` features 决定,新增格式记得同步 `IMAGE_EXTS`
- 界面:无标题栏、圆角、横向 720×470(约 6 条)、搜索 + 分类筛选 + 星级收藏 + 图钉置顶
  + 分组筛选下拉 + 扫帚清理 + 行内删改;弹层(分组面板/筛选菜单/确认框)一律 `position: fixed`
  (列表和分类条都是滚动容器,absolute 会被裁掉)
- 收藏目前只存在前端 localStorage(按内容哈希记),配合 History 落盘后不会丢
- `state.rs` 里有一组 `#[cfg(test)]` 单测覆盖新增的数据逻辑(去重继承分组 / 分组名归一化 /
  选择性删除),`cargo test --manifest-path src-tauri/Cargo.toml` 可直接跑(3 个用例,秒级)

**尚未实现(正式迁移阶段做)**:

- A5 SQLite 持久化(现为单文件 JSON,够用;量大再换)→ 参考 QC `services/database`
- A4 虚拟列表(现渲染前 200 条)→ 考虑 react-virtuoso,需验证 webkit 2.28 兼容
- 分组与收藏落库(`group` 已在记录里,收藏仍在 localStorage;两者规则不同源是个已知的割裂点)
- 齿轮(配置)按钮已占位,功能未做

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
4. Ctrl+Alt+V 全局热键唤起/隐藏(UKUI 下)。成功日志:`[热键] 已抓取 Ctrl+Alt+V (keycode=.., 组合 n/8)`(出现"部分锁定键组合注册失败"说明该组合被别的程序占用)
5. **点击条目/数字键 → 粘贴回前一应用**(X11 焦点交还 + XTEST)。成功日志:`[粘贴] 剪贴板已写入` → `主窗口已隐藏: true` → `归还焦点到 0x…,结果=true` → `Ctrl+V 已发送`;任一步失败都会以 `[粘贴] 失败: …` 打到终端
6. 托盘右键菜单全项
7. 单实例(二次启动退出并唤起旧实例)
8. 开机自启(检查 `~/.config/autostart/kylinpaste.desktop`)
9. 无标题栏窗口:圆角是否正常(见 §7 "透明圆角"一条)、顶部空白处拖动、图钉置顶(`[窗口] 置顶=true`)、Esc 收起(Alt+F4 等关闭请求仍应只是隐藏到托盘)
10. 历史持久化:复制几条 → 退出应用 → 重开,列表里历史仍在。成功日志:`[持久化] 载入 N 条历史`(文件在 `~/.local/share/com.kylinpaste.app/history.json`)
11. **图片缩略图**:复制一张截图/图片 → 列表那一行显示缩略图(不是类型图标);启动日志应打印 `[资源] 缩略图 ... 在 asset scope 内: true`,缩略图文件在 `clipboard_images/thumbs/<hash 前16位>.png`
12. **图片标题区分**:截图(剪贴板图像数据)→ `截图（1920 × 1080）`;在文件管理器里复制一个图片文件 → 标题是文件名、副标题「图片文件」,并同样有缩略图。粘贴这种记录时,文件管理器里应粘出文件、文档里应粘出图片

## 7. 已知问题与遗留风险

| 问题 | 状态 | 说明 |
|---|---|---|
| mac 上粘贴模拟不生效 | 已搁置 | 三步日志全通过(权限 OK、事件已发),目标应用无输入。仅影响 mac 预览便利性,**不影响 Linux 目标**(X11/XTEST 无权限模型)。后续可试 `Settings` 的 `independent_of_keyboard_state: false` |
| webkit 2.28 前端兼容 | 待目标机实测 | React 18 + es2020/safari13 target,大概率 OK;若渲染异常,降级方向:React 17 / Preact |
| gtk/soup 构建版本略新 | 低风险 | 容器内 gtk 走 focal-updates(3.24.20),目标机 3.24.18 基座;soname 稳定,理论上兼容 |
| 全局热键要按 2 下以上/失效 | 已修 | 两个原因:Tauri v1(tao)在 Linux 上用"按键释放瞬间的修饰键状态"判定命中,先松 Ctrl/Alt 再松 V 就匹配不上,且只覆盖 NumLock/CapsLock 组合;另外 `show()` 后紧接 `set_focus()` 时窗口尚未映射,tao 会丢弃聚焦请求(窗口映射出来却没焦点)。现改为自建 XGrabKey + KeyPress 判定,并等窗口可见后再补一次聚焦(见 `src/x11.rs`、`main.rs::show_main`) |
| 粘贴无反应(窗口消失但没内容) | 已修 | 粘贴命令原先是不带 async 的同步命令,跑在 GTK 主线程上,`window.hide()` 请求被排在命令之后处理,Ctrl+V 发出时焦点还在自己窗口上。现 `#[tauri::command(async)]` + 等待窗口真正隐藏 + 显式归还焦点后再注入 |
| 剪贴板上下文泄漏 | 已修 | clipboard-rs 在 X11 下每次 `new()` 都新建 2 条 X 连接 + 1 个常驻线程;监听线程每次变化都读一次剪贴板,复制几百次后会耗尽 X 客户端名额(默认 256)。现读/写各复用一份上下文(见 `clipboard_service.rs`) |
| X11 焦点交还 | 已加固 | 仍优先依赖窗口管理器归还焦点,但会记录"唤起剪贴板前的活动窗口"(`_NET_ACTIVE_WINDOW`),必要时用 EWMH `_NET_ACTIVE_WINDOW`(source=2)显式激活,失败再退 `XSetInputFocus` |
| 透明圆角窗口 | 待目标机实测 | 圆角靠 `transparent: true` + CSS `border-radius` 实现,需要窗口合成器(UKUI 默认有)。若目标机上四角发黑/整窗异常,退路:把 `transparent` 改回 false 并用直角(或重新打开 `decorations`) |
| 界面无"清空历史"入口 | 有意为之 | 按极简设计移除了顶部工具行;`cmd_clear_history` 仍在(以后放进齿轮菜单) |
| 存储选型:JSON vs SQLite | 暂用 JSON | 参考实现(QC)用 SQLite 5 张表 + 图片落盘 + `clipboard_data` 存原始格式 BLOB + 按条数裁剪并级联删图片。本 PoC 用单文件 JSON:文本/HTML 内联(各上限 1 万字),图片走磁盘只存路径,文件只存路径;500 条规模下够用。**换成 SQLite 的触发条件**:文件涨到几 MB 以上(500 条长文最坏可达几十 MB,且每次复制整文件重写)、收藏/分组要落库、要做跨设备同步(需 tombstones)、或要用 SQL 做搜索/分页 |
| clipboard_images 只增不减 | 有意为之 | 按产品要求:队列裁剪只丢记录,不动磁盘文件。因此 `clipboard_images/`(含 `thumbs/`)会随使用持续增长,旧的无引用 PNG 也不会自动清。若哪天要控体积,方案是"启动时清理 10 分钟前的无引用缩略图/原图"或加体积上限 LRU(需产品确认) |
| 只有非 PNG 图像格式的剪贴板 | 已知,未处理 | 我们只向 owner 要 `image/png`(clipboard-rs 的 `get_image` 写死)。容器实测:GTK(`Gtk.Clipboard.set_image`)提供 `image/png image/bmp image/x-bmp … image/jpeg image/tiff`,Qt(`QClipboard.setPixmap`)提供 `application/x-qt-image image/png image/bmp …`——**两类截图工具都给 png**,所以系统截图/常见截图软件不会漏。可能只给 bmp/tiff 的是 Wine 兼容层程序(CF_DIB→image/bmp)、Java/AWT 程序、个别专有软件。要兜底的话:PNG 失败后依次 `get_buffer("image/bmp"/"image/tiff"/"image/jpeg")` 再 `RustImageData::from_bytes`(解码器已随 image features 打开) |
| 富文本里的内嵌图片 | 未处理 | 只存 HTML 原文,不抓 `<img src>` 落盘(QC 会下载并改写 HTML 注入 `data-image-id`)。粘回富文本时内嵌图可能丢 |
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
- Linux 下不用 Tauri v1 的 global-shortcut(tao 实现按"按键释放"判定且修饰键匹配脆弱),改为 `src/x11.rs` 自建 XGrabKey + KeyPress;注入 Ctrl+V 也由 enigo 换成直接 XTEST(enigo 每次注入前要用 XInput 枚举设备,多一条依赖链)。抓键失败时会自动回退到 Tauri 内置实现。迁移到 Tauri v2 时其 global-shortcut 基于 global-hotkey(KeyPress 语义),这段自建代码可重新评估

## 10. 运维小抄

数据目录(应用数据目录,即 `app_data_dir`):

| 平台 | 路径 |
|---|---|
| 麒麟 / Linux | `~/.local/share/com.kylinpaste.app/` |
| mac(预览) | `~/Library/Application Support/com.kylinpaste.app/` |

- `history.json`:历史(文本/HTML 内联,图片与文件只存路径);损坏或删掉只是丢历史,应用照常启动
- `clipboard_images/<sha256前16位>.png`:图片本体;历史淘汰时会自动删除对应文件
- 单实例 socket:`$XDG_RUNTIME_DIR/kylinpaste.sock`(无 XDG_RUNTIME_DIR 时退回缓存目录)

排查用的关键日志(终端里跑 `kylinpaste` 即可看到):

```
[热键] 已抓取 Ctrl+Alt+V (keycode=.., 组合 n/8)      # 有组合失败会另打一行
[窗口] 切换: 可见=.. 有焦点=.. → 显示并聚焦 / 隐藏
[窗口] 置顶=true|false
[粘贴] 剪贴板已写入 → 主窗口已隐藏: true → 归还焦点到 0x..,结果=true → Ctrl+V 已发送
[粘贴] 失败: <原因>                                  # 出错必打,窗口已隐藏时前端横幅看不到
[持久化] 载入 N 条历史 / 清理图片 <路径>
```

手工清理无引用图片(默认**不会**自动清,理由见 §7;这条只在你想要回收磁盘时才用):

```bash
cd ~/.local/share/com.kylinpaste.app   # mac 换成上面那行路径
node -e 'const fs=require("fs"),j=require("./history.json");const u=new Set(j.items.map(i=>i.image_path).filter(Boolean).map(p=>p.split("/").pop()));fs.readdirSync("clipboard_images").filter(f=>!u.has(f)).forEach(f=>{fs.unlinkSync("clipboard_images/"+f);console.log("删除",f)})'
```
