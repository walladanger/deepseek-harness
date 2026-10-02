# apps/tauri-desktop

[English](README.md) | 中文

## 概述

实验性的轻量级桌面外壳，基于 Tauri 与 Rust 构建，用于承载 `dsh web` 界面。它打开一个普通的原生窗口（标题栏、最小化、最大化、关闭——没有自定义窗口外框），并把正在运行的 `dsh web` 服务器直接加载到该窗口中；不存在独立的浏览器标签页、控制台窗口，也不会出现额外窗口。该包是 Rust/Tauri 代码，不是 `packages/` 下的 TypeScript workspace，因此被排除在 pnpm workspace 和它所包装的 `dsh` 启动接口之外。

它正与既有的、基于 Electron 的 `apps/desktop` 并行评估，目前两者互不取代。它的产品名称被刻意取得与之不同（“DeepSeek Harness (Tauri Preview)”），这样把它和 `apps/desktop` 一起安装时，不会与后者的开始菜单快捷方式和安装程序标识（两者都使用纯粹的 “DeepSeek Harness” 名称）发生冲突或覆盖。

## 启动策略

该外壳从不在 `dsh` profile 之外自行启动 harness：启动时，它会在某个工作目录（可通过 `DSH_WEB_WORKSPACE` 覆盖，否则默认为当前用户的主目录）中派生（spawn）`dsh --profile web --host 127.0.0.1 --port <port> --no-open`（可通过 `DSH_CLI_PATH`、`DSH_WEB_PORT` 覆盖），并在自身整个生命周期内持有该子进程。`127.0.0.1` 不可配置：`dsh web` 自身的配置 schema 只接受该值或 `0.0.0.0`，而 CLI 出于安全考虑本身就拒绝 `0.0.0.0`，因此这并非真正意义上的可配置项。harness 进程本身始终通过 `web` profile 启动，依据
[`docs/architecture.md#application-launch`](../../docs/architecture.zh.md#application-launch)。

窗口在等待期间（最长 30 秒）会显示一个简短的“启动中……”占位界面，等待 `dsh web` 在 stdout 上宣告其带身份验证的启动 URL，随后导航到那个确切的 URL。单纯的 `http://127.0.0.1:<port>` 在这里不可用：索引路由要求携带那条宣告 URL 中所带的进程 token 或会话 cookie，普通请求会被拒绝并返回 401（参见
[`dsh-host-frontend-static`](../../packages/host/frontend-static/README.zh.md)）。如果 `dsh web` 在尚未宣告该 URL 之前就退出，窗口会转而显示它写入 stderr 的内容（其自身可操作的具体原因），若它什么都没写，则回退显示一条通用的超时提示。

关闭窗口会在所有平台上（包括 macOS——在那里，应用原本会在最后一个窗口关闭后仍驻留后台、不再显示任何窗口）终止整个进程——一次启动对应一个 `dsh web` 会话，从开始到结束；不存在重新打开或重新创建窗口的路径。

在窗口关闭或应用退出时，该外壳会停止子进程。在 Unix 上，这是先发出平和的 `SIGTERM` 信号，给予 5 秒退出时间——让 CLI 能走完它自己的正常关闭路径——之后才升级为 `SIGKILL`。在 Windows 上没有平和退出这一步：`dsh` 及其 `cmd.exe` 包装层（wrapper）都是控制台进程，不带 `/F` 的 `taskkill` 无法终止这些进程（本仓库自身的 `packages/subprocess/subprocess-local` 记录了这一事实，并用 Windows 控制台信号机制加以规避，而本外壳并未重新实现该机制），因此它直接采用强制的、整棵进程树的 `taskkill /T /F`。

如果一个关闭请求在 `dsh` 仍处于派生过程中到达，或者在更早一次关闭／超时路径仍处于停止它的中途到达，该请求会阻塞直到那次操作结束，而不是误判为“当下无事可做”便直接返回——否则子进程可能在无人管理的情况下继续存活。这个等待被刻意设计为无界：一个无响应的 `DSH_CLI_PATH`（比如一个卡住的 UNC 路径）理论上可能让应用的关闭过程被阻塞，阻塞时长取决于底层 `Command::spawn()` 调用失败所需的时间，而不是任何固定上限。

在 Unix 上，直接发送给该进程本身的 `SIGINT`（前台 `cargo run` 会话中的 Ctrl-C）或 `SIGTERM` 会执行同样的停止流程，而不是任由操作系统的默认处理方式结束进程、不给清理留任何机会：这两个信号都不会触达 Tauri 自身的窗口关闭处理逻辑，而且[把 `dsh` 移出该进程自身的进程组](#launch-policy)意味着 `dsh` 同样不会收到终端 Ctrl-C 信号，因此如果没有这个处理器，`dsh` 会在该外壳本身已经消失之后，继续占用端口运行下去。

dsh 界面内指向其自身源之外的链接——账号授权链接、聊天引用——会在平台默认浏览器中打开，而不是让本窗口导航离开应用，也不会打开第二个应用内窗口，这与 `apps/desktop` 的 Electron 外壳对此类链接的处理方式一致。只有 `http://`/`https://` 目标会交给系统的打开器；其他任何目标（`file:` 链接、已注册的自定义协议 URL）一律直接拒绝，以免冒险启动本地文件处理程序或任意已安装应用程序。

## 已知限制

这是一个早期评估阶段的外壳，目前接受以下四个尚未修复的缺口：

- 不对正在运行的 `dsh web` 进程做任何存活监控：如果它在窗口已经加载完成之后崩溃，窗口只会停留在显示一个已断开连接的页面，没有重启或失败提示界面。
- 没有 workspace 选择器：`DSH_WEB_WORKSPACE`（或主目录默认值）在整个进程生命周期内固定不变；没有应用内方式可以选择或更改它。
- `Run-DshDesktop.ps1 -Stop` 的 10 秒强制 `taskkill` 回退路径（仅在正常窗口关闭未能及时终止进程时使用）可能与该外壳自身正在阻塞等待的一次进行中 `Command::spawn()` 产生竞态（参见上文关于无界等待的说明）：如果那次 spawn 在 `taskkill /T` 已经枚举完进程树、但尚未杀完之前才完成，新创建的 `dsh` 就可能作为孤儿进程存活下来。这是从外部强制杀死一棵进程树、同时其内部仍有一次 spawn 在途这一情形本身固有的问题，并非该回退路径自身逻辑可以解决的；它需要和上文那个无界等待相同的无响应 `DSH_CLI_PATH` 条件，再叠加不利的时序，才会发生。
- 强制的 Unix 终止只触达 `dsh` 自身的进程组。`dsh` 自己在*独立*进程组中启动的工具或插件子进程（`packages/subprocess/subprocess-local/src/spawn.ts` 中的回退路径）或通过分离的 `systemd-run` scope 启动的子进程（`packages/subprocess/subprocess-local/src/linux-scope.ts`，仅限 Linux）都在这个范围之外，只能依靠 `dsh` 自身的正常关闭流程来停止——这正是本外壳给它 5 秒 `SIGTERM` 宽限期的目的所在。如果那次正常关闭没有在时限内完成，而本外壳随后对 `dsh` 自身的进程组升级为 `SIGKILL`，这样的一个范围就可能比它存活得更久。本外壳没有任何可以据以触达这类范围的 PID 或 scope 标识符：这类记录是 `dsh` 进程内部的事情，不通过任何外部接口暴露，而在本处重新实现它，就意味着在一个实验性外壳里、未经测试地重复那个包自身的归属跟踪逻辑——这和本外壳不为 Windows 重新实现一套真正平和停止所需的控制台信号机制，是同样的考量。

## 使用方式

- 开发（任意平台，只要 `PATH` 中有 `dsh`）：在本目录下执行 `cargo run`。
- Windows，不安装：构建发布版二进制文件（`cargo build --release`），通过
  [`scripts/Run-DshDesktop.ps1`](scripts/Run-DshDesktop.ps1) 运行，或直接运行构建产物的可执行文件。
- Windows，已安装：
  [Tauri Desktop Installer 工作流](../../.github/workflows/tauri-desktop-installer.yml)
  通过 `tauri-action` 生成一个 MSI 和一个 NSIS `.exe` 安装程序；安装完成后，`scripts/Run-DshDesktop.ps1` 会自动在 `%LOCALAPPDATA%\DeepSeek Harness (Tauri Preview)\` 下找到已安装的可执行文件。

## 开发

需要 Rust 工具链；在 Windows 上，还需要 Tauri v2 的前置依赖（WebView2、MSVC 构建工具）。在 Linux 上，Tauri v2 的前置依赖是 GTK 与 WebKitGTK 开发包——在 Debian/Ubuntu 上为
`libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev
libssl-dev libayatana-appindicator3-dev librsvg2-dev`（其他发行版参见
[Tauri v2 Linux 前置依赖](https://v2.tauri.app/start/prerequisites/#linux)）；缺少这些依赖时，`cargo check`/`cargo build` 会在编译 `gdk-sys` 构建脚本时失败。`icons/` 存放打包的窗口／安装程序图标。`tauri.conf.json` 声明了一个空的 `app.windows` 数组，因为唯一的窗口是在 `src/main.rs` 中运行时创建的，此时被包装的 `dsh web` 进程尚未宣告其带身份验证的启动 URL。
