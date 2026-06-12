# macrdp

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![macOS](https://img.shields.io/badge/macOS-14%2B-black.svg)](https://www.apple.com/macos/)
[![Apple Silicon](https://img.shields.io/badge/Apple%20Silicon-Supported-green.svg)](#)

**[English](README.md)** | 中文

**macOS 远程桌面服务端**

原生 macOS RDP 服务端。从 Windows、Linux、iOS 或 Android 远程连接你的 Mac — 支持任何标准 RDP 客户端，如 Windows 远程桌面 (mstsc)、Microsoft Remote Desktop、FreeRDP。

这个 fork 专注于 macOS RDP 服务本身，并采用最简单实用的使用方式：
通过 `macrdp-server` CLI 运行服务。上游原型中的桌面 GUI 不属于这个
fork 的发布范围。

> **为什么选 macrdp？** macOS 没有内置 RDP 服务端，自带的 VNC 又慢又模糊。macrdp 让你的 Mac 拥有一流的远程桌面体验 — 快速、清晰、开箱即用兼容所有 RDP 客户端。

---

## 功能特性

- **标准 RDP 协议** — 兼容任何 RDP 客户端，客户端无需安装特殊软件
- **硬件加速编码** — 通过 Apple VideoToolbox GPU 加速 H.264，Apple Silicon 上低延迟
- **高保真色彩** — AVC444 模式像素级色彩还原（RDP 10）
- **完整键鼠支持** — 104 键映射、数字键盘、修饰键、滚轮，完整输入注入
- **系统音频** — 通过 RDPSND 通道将远程 Mac 的音频流式传输到客户端；会话期间本地扬声器静音，断开连接时恢复原状态
- **剪贴板共享** — 通过 CLIPRDR 通道双向同步文本、图片和 HTML，并支持文件复制粘贴（可配置大小上限）
- **HiDPI / Retina 支持** — 2x/3x 倍率采集，远程 4K 高清显示
- **动态分辨率** — 自动跟随客户端窗口大小调整；服务端响应 display-control PDU 并实时更新会话分辨率
- **原生光标嵌入** — 真实 macOS 光标样式（缩放手柄、文字光标等）流式传输至视频中；可通过 `show_cursor` 配置
- **睡眠显示器容错** — 即使 Mac 显示器处于睡眠状态，服务端仍可启动并接受连接；客户端首次连接时自动唤醒显示器
- **灵活配置** — 分辨率、帧率、码率、编码器、质量预设，简单 TOML 配置
- **安全连接** — NLA/CredSSP 认证 + 自动生成 TLS 证书
- **锁屏采集** — 锁屏时自动切换 CoreGraphics 回退
- **v1 单客户端** — 支持一个活跃 RDP 会话，暂不支持并发会话

---

## 环境要求

- **macOS 14+**（Sonoma 或更高版本）
- **Rust 1.75+**
- 屏幕录制权限（系统设置 > 隐私与安全性）
- 辅助功能权限（用于键盘鼠标注入）

---

## 快速开始

**方式一 — 预编译二进制（Apple Silicon）**

从 [GitHub Releases](https://github.com/tasleson/macrdp/releases) 下载最新的 `macrdp-server-*-aarch64-apple-darwin.tar.gz`，然后：

```bash
tar -xzf macrdp-server-*-aarch64-apple-darwin.tar.gz
./macrdp-server
```

**方式二 — 从源码编译**

```bash
cargo build --release
cargo run --release --bin macrdp-server
```

从任意 RDP 客户端连接 → `Mac-IP:3389`

macrdp v1 一次只支持一个活跃 RDP 客户端。不支持并发会话；如需从其他
设备连接，请先断开当前客户端。

---

## 配置

复制 `config.example.toml` 为 `config.toml` 并按需修改：

```toml
# 网络
port = 13389
bind_address = "::"  # 双栈 IPv4 + IPv6；如需仅 IPv4 请使用 "0.0.0.0"

# 认证
username = "admin"
password = "123456"
allow_generated_credentials = false

# 显示
width = 0          # 0 = 自动检测
height = 0
frame_rate = 60
hidpi_scale = 2    # Retina 上 2 倍缩放获得 4K
show_cursor = true  # 将 macOS 光标样式嵌入视频流

# 编码
quality = "high_quality"    # low_latency / balanced / high_quality
encoder = "hardware"        # hardware (GPU) / software (CPU)
chroma_mode = "avc420"      # avc420 (兼容) / avc444 (最佳画质)
bitrate_mbps = 50           # 目标码率 (Mbps)

# 日志
log_level = "info"          # trace / debug / info / warn / error
log_path = "/path/to/macrdp.log"

# 音频 (RDPSND) — 默认启用
[audio]
enabled = true
sample_rate = 48000         # Hz
channels = 2                # 1 = 单声道, 2 = 立体声

# 剪贴板 (CLIPRDR) — 默认启用
[clipboard]
enabled = true
file_transfer = true        # 允许文件复制粘贴
max_file_size_mb = 100      # 单个文件传输大小上限
```

所有守护进程文件位于同一基础目录下:

- macOS: `~/Library/Application Support/macrdp/`
- Linux/BSD: `$XDG_CONFIG_HOME/macrdp/` (或 `~/.config/macrdp/`)

默认目录结构:

| 文件                     | 用途                                  |
| ------------------------ | ------------------------------------- |
| `<base>/config.toml`     | 守护进程配置                          |
| `<base>/tls/cert.pem`    | TLS 证书 (缺失时自动生成)             |
| `<base>/tls/key.pem`     | TLS 私钥 (随证书一起生成, 权限 `0600`) |
| `<base>/logs/macrdp.log` | 守护进程日志                          |

每个路径都可以通过对应的配置字段 (`cert_path`、`key_path`、`log_path`) 或 CLI 参数 (`--cert-path`、`--key-path`、`--log-path`) 进行覆盖。

### Keychain 密码存储

除了在 `config.toml` 中明文存储 RDP 密码，你也可以将其存储在 macOS Keychain 中：

```bash
# 存储密码（无回显输入；--username 默认为 $USER）
macrdp-server --keychain-set-password --username alice

# 启动服务端，从 Keychain 读取密码
macrdp-server --password-keychain
```

密码以通用密码条目形式存储，服务名为 `macrdp`，账户名为 RDP 用户名。使用标准 macOS 工具管理：

```bash
# 更新 — 重新执行 set 命令即可覆盖
macrdp-server --keychain-set-password --username alice

# 通过 security CLI 删除
security delete-generic-password -s macrdp -a alice

# 查看条目
security find-generic-password -s macrdp -a alice
```

也可在 **Keychain Access.app** 中搜索 "macrdp" 查看或删除该条目。

---

## 作为 launchd LaunchAgent 运行

仓库中提供了示例 plist: `packaging/launchd/com.macrdp.daemon.plist`。它被设计为按用户运行的 **LaunchAgent** (而非系统级 LaunchDaemon),因为 macOS 的"屏幕录制"和"辅助功能"权限绑定在已登录的图形会话上。

**安装**

```bash
# 1. 编译 release 二进制并放到一个持久路径
cargo build --release
sudo install -m 0755 target/release/macrdp-server /usr/local/bin/macrdp-server

# 2. 准备配置和日志目录
mkdir -p "$HOME/Library/Application Support/macrdp/logs"
cp config.example.toml "$HOME/Library/Application Support/macrdp/config.toml"
# 编辑配置 — 至少设置 username/password (或启用 allow_generated_credentials)

# 3. 将 plist 中的占位符替换为绝对路径 (plist 中不会展开 ~)
mkdir -p "$HOME/Library/LaunchAgents"
sed \
  -e "s|__MACRDP_BIN__|/usr/local/bin/macrdp-server|g" \
  -e "s|__HOME__|$HOME|g" \
  packaging/launchd/com.macrdp.daemon.plist \
  > "$HOME/Library/LaunchAgents/com.macrdp.daemon.plist"

# 4. 加载并启动
launchctl bootstrap "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.macrdp.daemon.plist"
launchctl kickstart -k "gui/$(id -u)/com.macrdp.daemon"
```

launchd 首次启动守护进程时,macOS 会针对 `macrdp-server` 二进制弹出"屏幕录制"和"辅助功能"授权对话框。在"系统设置 > 隐私与安全性"中授予后重启服务即可。

**查看状态、停止、重启**

```bash
launchctl print "gui/$(id -u)/com.macrdp.daemon"          # 状态 + 上次退出码
launchctl kill SIGTERM "gui/$(id -u)/com.macrdp.daemon"   # 优雅停止 (崩溃才会自动重启)
launchctl kickstart -k "gui/$(id -u)/com.macrdp.daemon"   # 强制重启
```

**卸载**

```bash
launchctl bootout "gui/$(id -u)" "$HOME/Library/LaunchAgents/com.macrdp.daemon.plist"
rm "$HOME/Library/LaunchAgents/com.macrdp.daemon.plist"
# 如需清理状态:
# rm -rf "$HOME/Library/Application Support/macrdp"
# sudo rm /usr/local/bin/macrdp-server
```

plist 使用 `KeepAlive = { SuccessfulExit = false; Crashed = true; }`: 干净的 SIGTERM (例如来自 `launchctl bootout`) 不会触发重启,而崩溃则会在 `ThrottleInterval` (10 秒) 后被重新拉起。

---

## 项目结构

```
crates/
├── macrdp-server/       主服务端程序 (CLI 守护进程)
├── macrdp-core/         运行时: 会话、配置、回调、自适应码率
├── macrdp-capture/      屏幕采集
├── macrdp-input/        键鼠注入
├── macrdp-encode/       视频编码
├── macrdp-audio/        系统音频采集 + RDPSND 流式传输
├── macrdp-clipboard/    原生剪贴板桥接 (文本/图片/HTML/文件)
├── macrdp-build/        共享构建脚本辅助
├── ironrdp-server-gfx/  RDP 协议层 (IronRDP fork)
└── ironrdp-acceptor-patched/
                         RDP 连接接受器
```

---

## 致谢

本项目的诞生离不开以下优秀的开源项目，在此致以诚挚的敬意：

- **[IronRDP](https://github.com/Devolutions/IronRDP)** — 纯 Rust RDP 协议实现。macrdp 的协议栈基于 ironrdp-server 的 fork，添加了 GFX/AVC444 扩展。
- **[FreeRDP](https://github.com/FreeRDP/FreeRDP)** — 开源 RDP 参考实现。其 AVC444 双流编码方案和 YUV444 B 区域拆分算法是 macrdp 实现的重要参考。
- **[RustDesk](https://github.com/rustdesk/rustdesk)** — 使用 Rust 编写的开源远程桌面软件。其跨平台屏幕采集和输入注入的架构思路给予了很大启发。

---

## 许可证

本项目采用 **GNU 通用公共许可证 v3.0** 授权 — 详见 [LICENSE](LICENSE)。任何基于本项目的衍生作品必须同样以 GPLv3 开源。

---

<details>
<summary><b>关键词</b></summary>

macOS RDP 服务端, Mac 远程桌面, Mac 远程桌面服务端, 远程桌面协议 macOS, 从 Windows 连接 Mac, 从 Linux 连接 Mac, 从安卓连接 Mac, 远程控制 Mac, Mac 远程访问, Mac 屏幕共享, Apple Silicon 远程桌面, VNC 替代方案, macOS 远程桌面方案, macOS RDP server, Mac remote desktop server, RDP server for Mac, connect to Mac from Windows, remote control Mac, VNC alternative Mac

</details>
