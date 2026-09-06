# ZooPeek

**A modern, fast and lightweight desktop GUI client for Apache ZooKeeper** — cross-platform (Windows / macOS / Linux), built with Tauri 2 + Rust + Vue 3.

> 一个现代化、轻量的 ZooKeeper 桌面可视化管理工具，跨平台，秒级启动，内存占用仅为传统 JavaFX 客户端的零头。

English | [中文](#中文介绍)

---

## Why ZooPeek?

Existing ZooKeeper GUI tools are either outdated, sluggish, or locked behind paywalls. ZooPeek is built to fix that:

- 🚀 **Lightweight & fast** — ~10 MB installer, native-speed startup, low memory footprint (no JVM, no bundled Chromium)
- 🖥️ **Truly cross-platform** — one codebase, native installers for Windows, macOS and Linux
- 🎨 **Modern UI** — clean dark-theme interface inspired by database clients like DataGrip
- 🔴 **Live state visibility** — connection state machine (Connecting / SyncConnected / Disconnected / Expired), session ID, and a real-time watcher event stream, so you always know what's happening
- 🔌 **Multi-connection tabs** — manage multiple ZooKeeper clusters side by side, like a database IDE

## Features

- **Znode browser** — lazy-loading tree with watch-driven live refresh (no manual reload)
- **Data editor** — view/edit znode data with JSON formatting; binary data detected and protected from accidental corruption
- **Full CRUD** — create / delete nodes, recursive delete with confirmation
- **ACL management** — view and edit node ACLs (world / auth / digest / ip schemes)
- **Watcher event stream** — every node change (created / deleted / data changed / children changed) shows up in a live event log with zxid
- **zkCli shell** — built-in terminal that runs familiar zkCli commands (`ls` / `get` / `set` / `create` / `delete` / `deleteall` / `stat` / `getAcl` / `setAcl` / `addauth` / `whoami`) against the connected cluster, with command history and zkCli-style output
- **Session monitoring** — connection state changes pushed in real time
- **Connection manager** — save, organize and reconnect to clusters instantly

## Download

Pre-built installers are available on the [Releases](https://github.com/PangJayoo/ZooPeek/releases) page:

| Platform | Package |
|---|---|
| Windows | `.msi` / `.exe` / `portable.zip`（免安装，需系统自带 WebView2） |
| macOS | `.dmg` (Apple Silicon & Intel) |
| Linux | `.AppImage` / `.deb` |

## Tech Stack

| Layer | Tech |
|---|---|
| Desktop shell | [Tauri 2](https://tauri.app/) (system WebView, Rust core) |
| ZooKeeper client | [zookeeper-client](https://github.com/kezhuw/zookeeper-client-rust) (async Rust) |
| UI | Vue 3 + TypeScript + [Naive UI](https://www.naiveui.com/) |

## Build from Source

Prerequisites: Node.js 24+, pnpm, Rust 1.77+.

```bash
git clone https://github.com/PangJayoo/ZooPeek.git
cd ZooPeek
pnpm install
pnpm tauri dev      # dev mode with hot reload
pnpm tauri build    # produce platform installer
```

Run the smoke test against a local ZooKeeper (e.g. `docker run -p 2181:2181 zookeeper:3.9`):

```bash
cd src-tauri && cargo test --test zk_smoke
```

## Roadmap

- [x] Multi-connection tabs, znode tree browsing, data editing
- [x] Watcher event stream & session state monitoring
- [x] Node CRUD, recursive delete, ACL management
- [x] Authentication (digest / SASL) connections
- [x] Node search (index-based, Spotlight-style)
- [x] zkCli shell
- [ ] Import/export

## Contributing

Issues and PRs are welcome. Development happens on the `develop` branch; `main` is protected and used for tagged releases.

## License

MIT

---

## 中文介绍

**ZooPeek 是一个现代化的 ZooKeeper 桌面客户端**

### 核心特性

- **轻量快速**：安装包约 10MB，秒级启动，低内存占用
- **跨平台**：Windows / macOS / Linux 原生安装包
- **中间状态透明**：连接状态机实时可视（连接中/已连接/断开重连/会话过期），Session ID 可见，watcher 事件流实时滚动——永远知道客户端当前处于什么状态
- **多连接管理**：tab 式多集群并行管理，体验对标 DataGrip
- **完整 CRUD**：节点增删改查、递归删除（带确认）、ACL 权限管理
- **数据编辑**：JSON 一键格式化；二进制数据自动检测并锁定只读，防止误保存损坏
- **实时刷新**：基于 ZK watcher 的事件驱动刷新，节点变化无需手动刷新
- **zkCli Shell**：内置命令行终端，直接对已连接集群执行熟悉的 zkCli 命令（ls/get/set/create/delete/deleteall/stat/getAcl/setAcl/addauth/whoami），支持命令历史（↑/↓），输出格式对齐 zkCli

### 下载安装

前往 [Releases](https://github.com/PangJayoo/ZooPeek/releases) 下载对应平台安装包。

### 本地开发

```bash
git clone https://github.com/PangJayoo/ZooPeek.git
cd ZooPeek
pnpm install
pnpm tauri dev
```
### License

MIT
