# macrdp

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](https://www.gnu.org/licenses/gpl-3.0)
[![Rust](https://img.shields.io/badge/Rust-1.75%2B-orange.svg)](https://www.rust-lang.org/)
[![macOS](https://img.shields.io/badge/macOS-14%2B-black.svg)](https://www.apple.com/macos/)
[![Apple Silicon](https://img.shields.io/badge/Apple%20Silicon-Supported-green.svg)](#)

**[English](README_EN.md)** | 中文

**macOS 远程桌面服务端**

原生 macOS RDP 服务端。从 Windows、Linux、iOS 或 Android 远程连接你的 Mac — 支持任何标准 RDP 客户端，如 Windows 远程桌面 (mstsc)、Microsoft Remote Desktop、FreeRDP。

> **为什么选 macrdp？** macOS 没有内置 RDP 服务端，自带的 VNC 又慢又模糊。macrdp 让你的 Mac 拥有一流的远程桌面体验 — 快速、清晰、开箱即用兼容所有 RDP 客户端。

---

## 功能特性

- **标准 RDP 协议** — 兼容任何 RDP 客户端，客户端无需安装特殊软件
- **硬件加速编码** — 通过 Apple VideoToolbox GPU 加速 H.264，Apple Silicon 上低延迟
- **高保真色彩** — AVC444 模式像素级色彩还原（RDP 10）
- **完整键鼠支持** — 104 键映射、数字键盘、修饰键、滚轮，完整输入注入
- **HiDPI / Retina 支持** — 2x/3x 倍率采集，远程 4K 高清显示
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

```bash
# 编译
cargo build --release

# 运行
cargo run --release --bin macrdp-server

# 从任意 RDP 客户端连接 → Mac-IP:3389
```

macrdp v1 一次只支持一个活跃 RDP 客户端。不支持并发会话；如需从其他
设备连接，请先断开当前客户端。

---

## 配置

复制 `config.example.toml` 为 `config.toml` 并按需修改：

```toml
# 网络
port = 13389
bind_address = "0.0.0.0"

# 认证
username = "admin"
password = "123456"
allow_generated_credentials = false

# 显示
width = 0          # 0 = 自动检测
height = 0
frame_rate = 60
hidpi_scale = 2    # Retina 上 2 倍缩放获得 4K

# 编码
quality = "high_quality"    # low_latency / balanced / high_quality
encoder = "hardware"        # hardware (GPU) / software (CPU)
chroma_mode = "avc420"      # avc420 (兼容) / avc444 (最佳画质)
bitrate_mbps = 50           # 目标码率 (Mbps)

# 日志
log_level = "info"          # trace / debug / info / warn / error
log_path = "/path/to/macrdp.log"
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
├── macrdp-server/       主服务端程序
├── macrdp-capture/      屏幕采集
├── macrdp-input/        键鼠注入
├── macrdp-encode/       视频编码
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
