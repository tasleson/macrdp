# MacRDP UI 重设计实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 将 macrdp-ui 从基础 macOS 风格全面升级为 Xcode/Instruments 专业工具风格，包括新配色体系、分组侧边栏、实时图表仪表盘、Console.app 风格日志等。

**Architecture:** 自底向上实施——先建立设计系统基础(CSS 变量/字体)，然后构建可复用组件(StatusBar/MetricCard/ChartPanel 等)，最后逐页重写。每个 Task 产出可独立验证的工作成果。

**Tech Stack:** React 19, TypeScript, Tailwind CSS v4, Radix UI, lucide-react, Tauri 2, 手写 SVG 图表

**Spec:** `docs/superpowers/specs/2026-05-16-ui-redesign-design.md`

---

## File Map

### 新建文件
| 文件 | 职责 |
|------|------|
| `src/components/StatusBar.tsx` | 紧凑服务状态栏(运行状态/端口/运行时长/启停按钮) |
| `src/components/MetricCard.tsx` | 单指标展示卡片(数字+标签+颜色) |
| `src/components/ChartPanel.tsx` | SVG 图表容器(面积图/柱状图) |
| `src/components/AreaChart.tsx` | SVG 面积图(60 点环形缓冲区) |
| `src/components/BarChart.tsx` | SVG 柱状图(流量趋势) |
| `src/components/ConnItem.tsx` | 连接列表项(质量指示条+用户信息+指标) |
| `src/components/LogEntry.tsx` | 日志条目(等宽字体+颜色分级) |
| `src/components/SettingRow.tsx` | 设置行(label+control 布局) |
| `src/components/EmptyState.tsx` | 通用空状态组件(图标+文字) |
| `src/hooks/useFpsHistory.ts` | FPS 环形缓冲区 hook(60 数据点) |

### 重写文件
| 文件 | 行数 | 变更内容 |
|------|------|----------|
| `src/styles/globals.css` | 222→~120 | 全面替换 CSS 变量，移除 oklch，新色彩体系 |
| `src/components/Sidebar.tsx` | 94→~130 | 180px 宽 + 分组标题 + 底部主题切换 |
| `src/pages/Dashboard.tsx` | 177→~180 | 状态栏+FPS图表+指标行+连接列表 |
| `src/pages/Settings.tsx` | 369→~280 | 左右分栏(分类导航+选项面板) |
| `src/pages/Logs.tsx` | 170→~150 | Console.app 风格，等宽字体+颜色分级 |
| `src/pages/Statistics.tsx` | 256→~200 | 时间范围切换+柱状图+历史表 |
| `src/pages/Permissions.tsx` | 104→~110 | 新样式权限卡片 |
| `src/pages/About.tsx` | 154→~120 | 居中信息卡片，精简布局 |

### 修改文件
| 文件 | 变更内容 |
|------|----------|
| `src/App.tsx` | 宽度 class, 替换 macos-* 引用, 移除 PermissionBanner |
| `src/components/ThemeToggle.tsx` | 适配新位置(侧边栏底部)，图标形式 |
| `src/pages/Popover.tsx` | 替换 macos-* class 引用为新变量名 |
| `src-tauri/tauri.conf.json` | 窗口尺寸 960×620, 最小 800×500 |
| `package.json` | 移除 `@fontsource-variable/geist` |

### 可删除文件
| 文件 | 原因 |
|------|------|
| `src/components/MetricsStrip.tsx` | 被 MetricCard 行替代 |
| `src/components/StatusBadge.tsx` | 被 StatusBar 内状态圆点替代 |
| `src/components/ServerInfoTags.tsx` | 信息合并到 StatusBar |
| `src/components/PermissionCard.tsx` | 内联到 Permissions 页面 |
| `src/components/PermissionBanner.tsx` | 功能由 Sidebar 红点 + Permissions 页覆盖 |

---

## Task 1: 设计系统基础 — CSS 变量 + 字体 + 窗口配置

**Files:**
- Rewrite: `macrdp-ui/src/styles/globals.css`
- Modify: `macrdp-ui/src-tauri/tauri.conf.json`
- Modify: `macrdp-ui/package.json`
- Modify: `macrdp-ui/src/main.tsx` (如有 Geist 字体 import 则移除)
- Modify: `macrdp-ui/src/App.tsx` (替换 `bg-macos-bg` 等旧 class)
- Modify: `macrdp-ui/src/pages/Popover.tsx` (替换 `macos-*` 引用为新变量名)

> **迁移策略**: globals.css 重写时，在底部保留 `--color-macos-*` 作为新变量的别名（临时兼容层），确保未重写的文件（Popover 等）在过渡期间仍能构建。Task 11 清理时移除别名。或者，在本 Task 中同步将 App.tsx 和 Popover.tsx 中的 `macos-*` 引用替换为新变量名。推荐后者，一步到位。

- [ ] **Step 1: 重写 globals.css**

替换全部 CSS 变量为新色彩体系。移除 oklch 语义变量和 `--color-macos-*` 前缀。保留 shadcn/ui 组件所需的基础变量映射（`--background`, `--foreground`, `--primary` 等），将它们指向新色值，确保 `src/components/ui/` 下的 shadcn 组件继续正常渲染。

关键内容结构:
```css
@import "tailwindcss";

@theme {
  /* 浅色模式（默认） */
  --color-bg: #f5f5f7;
  --color-sidebar: #ebebed;
  --color-card: #ffffff;
  --color-border: #e8e8e8;
  --color-border-subtle: #f0f0f0;
  --color-text: #1d1d1f;
  --color-text-secondary: #333333;
  --color-text-muted: #86868b;
  --color-accent: #007aff;
  --color-green: #34c759;
  --color-orange: #ff9500;
  --color-red: #ff3b30;
  --color-purple: #af52de;
  --color-yellow: #ffcc00;

  /* 字体 */
  --font-sans: -apple-system, BlinkMacSystemFont, 'SF Pro Text', 'SF Pro Display', system-ui, sans-serif;
  --font-mono: 'SF Mono', SFMono-Regular, ui-monospace, Menlo, monospace;

  /* 圆角 */
  --radius-sm: 5px;
  --radius-md: 8px;
  --radius-lg: 10px;
}

.dark {
  --color-bg: #1a1a1a;
  --color-sidebar: #202020;
  --color-card: #252525;
  --color-border: #2d2d2d;
  --color-border-subtle: #3a3a3a;
  --color-text: #ffffff;
  --color-text-secondary: #cccccc;
  --color-text-muted: #8e8e93;
  --color-accent: #0a84ff;
  --color-green: #30d158;
  --color-orange: #ff9f0a;
  --color-red: #ff453a;
  --color-purple: #bf5af2;
  --color-yellow: #ffd60a;
}

/* 全局基础样式 */
body {
  font-family: var(--font-sans);
  background: var(--color-bg);
  color: var(--color-text);
  -webkit-font-smoothing: antialiased;
}

/* 全局过渡 */
* { transition: color 150ms ease-out, background-color 150ms ease-out, border-color 150ms ease-out; }
```

> 注意: Tailwind v4 的 `@theme` 指令自动生成对应的 utility class (如 `bg-card`, `text-muted`, `border-border`)。

- [ ] **Step 2: 移除 Geist 字体 + 替换旧 class 引用**

Geist 字体 import 在 `globals.css` 中（已在 Step 1 中移除）。检查 `main.tsx` 是否也有，如有则移除。

移除 npm 依赖（如存在）:
```bash
cd macrdp-ui && npm uninstall @fontsource-variable/geist 2>/dev/null || true
```

替换 `App.tsx` 和 `Popover.tsx` 中的旧 `macos-*` class 引用为新变量:
- `bg-macos-bg` → `bg-bg`
- `bg-macos-card` → `bg-card`
- `text-macos-text` → `text-text`
- `text-macos-secondary` → `text-text-secondary`
- `border-macos-border` → `border-border`
- `bg-macos-green` → `bg-green`
- `bg-macos-blue` → `bg-accent`
- `bg-macos-red` → `bg-red`
- `text-macos-green` → `text-green`
- 以此类推，全局搜索 `macos-` 并逐一替换

- [ ] **Step 3: 更新窗口尺寸**

修改 `macrdp-ui/src-tauri/tauri.conf.json` 中 main 窗口配置:
```json
{
  "width": 960,
  "height": 620,
  "minWidth": 800,
  "minHeight": 500
}
```

- [ ] **Step 4: 验证基础构建**

```bash
cd macrdp-ui && npm run build
```
Expected: 构建成功，无错误。页面可正常加载（配色变化但布局未变）。

- [ ] **Step 5: Commit**

```bash
git add macrdp-ui/src/styles/globals.css macrdp-ui/src-tauri/tauri.conf.json macrdp-ui/package.json macrdp-ui/package-lock.json macrdp-ui/src/main.tsx
git commit -m "refactor(ui): replace design system — new color tokens, system fonts, window sizing"
```

---

## Task 2: 侧边栏重写 — 分组导航 + 底部主题切换

**Files:**
- Rewrite: `macrdp-ui/src/components/Sidebar.tsx`
- Modify: `macrdp-ui/src/components/ThemeToggle.tsx`
- Modify: `macrdp-ui/src/App.tsx` (sidebar 宽度 class)

- [ ] **Step 1: 重写 Sidebar.tsx**

新侧边栏结构:
- 宽度 180px (`w-[180px]`)
- 顶部 28px drag region
- 三个分组: 服务(控制台) / 配置(设置, 权限) / 诊断(日志, 统计)
- 分组标题: `text-[10px] font-semibold uppercase tracking-wider text-muted`
- 导航项: lucide-react 图标(16px) + 文字标签(12px)
- 选中态: `bg-accent/15 text-accent`
- Hover: `hover:bg-accent/8`
- 底部固定: 分隔线 + "关于"链接 + ThemeToggle

```tsx
// 导航项数据结构
const navGroups = [
  {
    label: "服务",
    items: [{ path: "/", icon: Monitor, label: "控制台" }],
  },
  {
    label: "配置",
    items: [
      { path: "/settings", icon: Settings, label: "设置" },
      { path: "/permissions", icon: Shield, label: "权限" },
    ],
  },
  {
    label: "诊断",
    items: [
      { path: "/logs", icon: FileText, label: "日志" },
      { path: "/statistics", icon: Activity, label: "统计" },
    ],
  },
];
```

ARIA: `<nav role="navigation">`, 当前项 `aria-current="page"`

- [ ] **Step 2: 改造 ThemeToggle.tsx**

从独立悬浮按钮改为内联侧边栏底部元素:
- 移除 fixed 定位和 z-50
- 三态图标按钮: Sun / Moon / Monitor (lucide-react)
- 点击循环: light → dark → system → light
- 小尺寸: 28×28px 圆角按钮
- 显示当前模式 tooltip

- [ ] **Step 3: 更新 App.tsx 布局**

修改主布局中侧边栏宽度 class:
```diff
- <div className="w-52 ...">
+ <div className="w-[180px] flex-shrink-0 ...">
```

确保内容区 `flex-1 overflow-y-auto` 正确填充剩余空间。

- [ ] **Step 4: 验证导航功能**

```bash
cd macrdp-ui && npm run dev
```

手动验证:
- 侧边栏 180px 宽，分组标题正确显示
- 所有 6 个路由点击可切换
- 选中态蓝色高亮
- 主题切换在底部可用，三态循环正常
- 权限红点指示器保留

- [ ] **Step 5: Commit**

```bash
git add macrdp-ui/src/components/Sidebar.tsx macrdp-ui/src/components/ThemeToggle.tsx macrdp-ui/src/App.tsx
git commit -m "refactor(ui): rewrite sidebar — grouped navigation, 180px width, bottom theme toggle"
```

---

## Task 3: 共享组件 — StatusBar + MetricCard + EmptyState

**Files:**
- Create: `macrdp-ui/src/components/StatusBar.tsx`
- Create: `macrdp-ui/src/components/MetricCard.tsx`
- Create: `macrdp-ui/src/components/EmptyState.tsx`

- [ ] **Step 1: 实现 StatusBar**

Props:
```tsx
interface StatusBarProps {
  state: "running" | "starting" | "stopped" | "error";
  port?: number;
  uptimeSeconds?: number;
  errorMessage?: string;
  onStart: () => void;
  onStop: () => void;
}
```

布局: 单行 flex, 高 36px, `bg-card rounded-[8px] px-3`
- 左侧: 状态圆点 (8px, glow via box-shadow) + 文字 + 辅助信息
- 右侧: 启动/停止按钮
- 状态色: running=green, starting=yellow, stopped=gray (`text-muted`), error=red
- Error 态: `bg-red/10 border border-red/20` + 错误消息
- `role="status" aria-live="polite"`

运行时间格式: `formatUptime(seconds)` → "2h 34m" / "5m" / "< 1m"

- [ ] **Step 2: 实现 MetricCard**

Props:
```tsx
interface MetricCardProps {
  value: number | null;
  label: string;
  color: "blue" | "green" | "orange" | "purple";
  unit?: string;
  formatter?: (v: number) => string;
}
```

布局: `flex-1 bg-card rounded-[8px] p-2 text-center`
- 数字: `text-xl font-bold` + 颜色映射
- 无数据: `--` + `text-muted`
- 标签: `text-[10px] text-muted mt-0.5`

颜色映射 (Tailwind class):
```tsx
const colorMap = {
  blue: "text-accent",
  green: "text-green",
  orange: "text-orange",
  purple: "text-purple",
};
```

- [ ] **Step 3: 实现 EmptyState**

Props:
```tsx
interface EmptyStateProps {
  icon?: LucideIcon;
  message: string;
}
```

布局: `flex flex-col items-center justify-center py-8 text-muted`
- 图标 32px + 文字 12px

- [ ] **Step 4: 验证组件渲染**

临时在 Dashboard 中 import 并渲染 StatusBar 和 MetricCard，确认样式正确。

- [ ] **Step 5: Commit**

```bash
git add macrdp-ui/src/components/StatusBar.tsx macrdp-ui/src/components/MetricCard.tsx macrdp-ui/src/components/EmptyState.tsx
git commit -m "feat(ui): add StatusBar, MetricCard, EmptyState components"
```

---

## Task 4: 图表组件 — AreaChart + BarChart + ChartPanel + useFpsHistory

**Files:**
- Create: `macrdp-ui/src/components/AreaChart.tsx`
- Create: `macrdp-ui/src/components/BarChart.tsx`
- Create: `macrdp-ui/src/components/ChartPanel.tsx`
- Create: `macrdp-ui/src/hooks/useFpsHistory.ts`

- [ ] **Step 1: 实现 useFpsHistory hook**

60 元素环形缓冲区，每秒从 metrics 推入新数据点。使用 `useState` 触发重渲染:
```tsx
export function useFpsHistory(currentFps: number | null, maxPoints = 60): number[] {
  const bufferRef = useRef<number[]>([]);
  const [, setTick] = useState(0);

  useEffect(() => {
    if (currentFps === null) return;
    const buf = bufferRef.current;
    buf.push(currentFps);
    if (buf.length > maxPoints) buf.shift();
    setTick(t => t + 1); // 触发重渲染使 AreaChart 更新
  }, [currentFps, maxPoints]);

  return bufferRef.current;
}
```

> 注意: 仅用 `useRef` 不会触发 React 重渲染。`setTick` 是轻量的 forceUpdate 机制。

- [ ] **Step 2: 实现 AreaChart**

纯 SVG 面积图:
```tsx
interface AreaChartProps {
  data: number[];
  color: string;       // CSS color value
  height?: number;     // default 60
  maxValue?: number;   // auto-scale if omitted
}
```

- SVG `viewBox="0 0 {width} {height}"`, `preserveAspectRatio="none"`
- 数据点 → SVG path (polyline smoothed with simple interpolation)
- 渐变填充: `<linearGradient>` 从 `{color}/30%` 到 `transparent`
- 线条: `stroke={color} stroke-width="1.5" fill="none"`
- 空数据: 不渲染 (ChartPanel 负责空态)

- [ ] **Step 3: 实现 BarChart**

纯 SVG 柱状图:
```tsx
interface BarChartProps {
  data: { label: string; value: number }[];
  color: string;
  height?: number;    // default 70
}
```

- 等宽柱子, 间距 2px, 圆角顶部 (`rx="2"`)
- 柱子高度按 maxValue 归一化
- hover 时不透明度增加到 1.0 (默认 0.7)

- [ ] **Step 4: 实现 ChartPanel**

包装容器:
```tsx
interface ChartPanelProps {
  title: string;
  value?: string;         // 右侧显示的当前值
  valueColor?: string;    // 当前值颜色
  empty?: boolean;        // 是否无数据
  emptyText?: string;     // 无数据提示 (default "等待数据...")
  children: React.ReactNode;
}
```

布局: `bg-card rounded-[8px] p-3`
- Header: title(左, `text-[11px] text-muted`) + value(右, `text-[11px] font-medium`)
- Body: children (图表)
- 空态: 灰色虚线 + 居中文字

- [ ] **Step 5: 验证图表渲染**

临时在 Dashboard 中用静态数据渲染 AreaChart 和 BarChart，确认 SVG 正确绘制。

- [ ] **Step 6: Commit**

```bash
git add macrdp-ui/src/components/AreaChart.tsx macrdp-ui/src/components/BarChart.tsx macrdp-ui/src/components/ChartPanel.tsx macrdp-ui/src/hooks/useFpsHistory.ts
git commit -m "feat(ui): add SVG chart components (AreaChart, BarChart, ChartPanel) and useFpsHistory hook"
```

---

## Task 5: 连接组件 — ConnItem

**Files:**
- Create: `macrdp-ui/src/components/ConnItem.tsx`

- [ ] **Step 1: 实现 ConnItem**

Props (复用现有 `Connection` 类型):
```tsx
interface ConnItemProps {
  connection: Connection;
}
```

布局: `flex items-center justify-between bg-card rounded-[8px] p-2`
- 左侧 3px 彩色指示条 (基于连接时长/流量的简单指示):
  - 连接时间 > 10min → green (稳定连接)
  - 连接时间 > 1min → orange (新连接)
  - 否则 → accent (刚连上)
- 主区: `client_name`@`client_ip` (`text-xs font-medium text-text`) + 连接时长 (`text-[10px] text-muted`)
- 右侧: 流量 (`text-[11px] font-medium`, 格式化为 MB/GB) (`text-[10px] text-muted`)

> 注意: `Connection` 类型只有 `client_ip`, `client_name`, `connected_at`, `bytes_total`。没有 per-connection FPS 数据。质量指示条基于连接时长而非 FPS。

连接时长: 用 `connected_at` 与 `Date.now()` 差值计算，复用/新建 `formatDuration()` helper

- [ ] **Step 2: Commit**

```bash
git add macrdp-ui/src/components/ConnItem.tsx
git commit -m "feat(ui): add ConnItem component with quality indicator"
```

---

## Task 6: Dashboard 页面重写

**Files:**
- Rewrite: `macrdp-ui/src/pages/Dashboard.tsx`
- Delete: `macrdp-ui/src/components/MetricsStrip.tsx`
- Delete: `macrdp-ui/src/components/StatusBadge.tsx`
- Delete: `macrdp-ui/src/components/ServerInfoTags.tsx`

- [ ] **Step 1: 重写 Dashboard.tsx**

Import 新组件: StatusBar, MetricCard, ChartPanel, AreaChart, ConnItem, EmptyState
Import hooks: useServerStatus, useMetrics, useConnections, useFpsHistory

布局 (从上到下, `flex flex-col gap-3 p-4 overflow-y-auto`):
```
1. StatusBar — state/port/uptime/onStart/onStop
2. ChartPanel("帧率 (FPS)") → AreaChart(fpsHistory, accent color)
3. div.flex.gap-2 →
   MetricCard(fps, "FPS", "blue")
   MetricCard(bitrate, "Mbps", "green", v => (v/1000).toFixed(1))
   MetricCard(latency, "ms 延迟", "orange")
   MetricCard(connectionCount, "连接", "purple")
4. ConnPanel (bg-card rounded-[8px] p-3):
   Header: "活跃连接" + count badge
   connections.map(c => <ConnItem key={c.id} connection={c} />)
   或 EmptyState("暂无活跃连接")
```

数据流:
- `const { status, start, stop } = useServerStatus()`
- `const metrics = useMetrics()`
- `const connections = useConnections()`
- `const fpsHistory = useFpsHistory(metrics?.fps ?? null)`

- [ ] **Step 2: 删除废弃组件**

删除 `MetricsStrip.tsx`、`StatusBadge.tsx`、`ServerInfoTags.tsx`。在项目中 grep 确认无其他引用。

- [ ] **Step 3: 验证 Dashboard**

```bash
cd macrdp-ui && npm run dev
```

手动验证:
- 服务停止时: 灰色状态栏 + 图表空态 + 连接空态
- 服务启动后: 绿色状态栏 + FPS 曲线逐步填充 + 指标卡片实时更新
- 有连接时: 连接列表显示，彩色指示条正确

- [ ] **Step 4: Commit**

```bash
git add macrdp-ui/src/pages/Dashboard.tsx
git rm macrdp-ui/src/components/MetricsStrip.tsx macrdp-ui/src/components/StatusBadge.tsx macrdp-ui/src/components/ServerInfoTags.tsx
git commit -m "feat(ui): rewrite Dashboard — status bar, FPS chart, metrics cards, connection list"
```

---

## Task 7: 设置页重写 — 左右分栏

**Files:**
- Create: `macrdp-ui/src/components/SettingRow.tsx`
- Rewrite: `macrdp-ui/src/pages/Settings.tsx`

- [ ] **Step 1: 实现 SettingRow**

Props:
```tsx
interface SettingRowProps {
  label: string;
  description?: string;
  children: React.ReactNode;  // control (Switch, Select, Input)
}
```

布局: `flex items-center justify-between bg-card rounded-[6px] px-3 py-2`
- 左侧: label (`text-xs text-text-secondary`) + optional description (`text-[10px] text-muted`)
- 右侧: children (control slot)

- [ ] **Step 2: 重写 Settings.tsx**

左右分栏结构:
```tsx
const categories = [
  { id: "rdp", label: "RDP 服务" },
  { id: "network", label: "网络" },
  { id: "security", label: "安全" },
  { id: "display", label: "显示" },
  { id: "audio", label: "音频" },
  { id: "general", label: "通用" },
];
```

布局: `flex h-full`
- 左侧导航 (140px, `border-r border-border`): 分类列表，选中态 `bg-accent/12 text-accent`
- 右侧面板 (`flex-1 overflow-y-auto p-4`): 分组标题 + SettingRow 列表

每个分类的字段直接映射 UiConfig:
- RDP 服务: encoder (Select), bitrate_mbps (Select: 2/4/8/16/32), frame_rate (Select: 30/60/120), chroma_mode (Select)
- 网络: port (Input), bind_address (Input), max_connections (Input), idle_timeout_secs (Input)
- 安全: username (Input), password (Input type=password)
- 显示: hidpi_scale (Select: 1/2/3/4), show_cursor (Switch)
- 音频: 显示"音频配置尚未开放"提示
- 通用: log_level (Select), auto_start (Switch), 主题 (三态 Select)

保持现有 `setConfig(key, value)` 调用模式和 `restart_required` 提示。

- [ ] **Step 3: 验证设置页**

手动验证:
- 左侧分类点击切换
- 右侧选项正确显示
- 修改设置值能正确调用 `setConfig`
- restart_required 提示正常弹出

- [ ] **Step 4: Commit**

```bash
git add macrdp-ui/src/components/SettingRow.tsx macrdp-ui/src/pages/Settings.tsx
git commit -m "feat(ui): rewrite Settings — split panel layout with category navigation"
```

---

## Task 8: 日志页重写 — Console.app 风格

**Files:**
- Create: `macrdp-ui/src/components/LogEntry.tsx`
- Rewrite: `macrdp-ui/src/pages/Logs.tsx`

- [ ] **Step 1: 实现 LogEntry**

Props (复用现有 `LogEntry` 类型):
```tsx
interface LogEntryRowProps {
  entry: LogEntry;
}
```

布局: `flex gap-2 px-2.5 py-1 border-b border-border font-mono text-[11px]`
- 时间戳: `min-w-[70px] text-muted` (format: "HH:mm:ss")
- 级别: `min-w-[40px] font-semibold` + 颜色映射
  - "info" → `text-accent`, "warn" → `text-orange`, "error" → `text-red`, "debug" → `text-muted`, "trace" → `text-muted`
- 消息: `flex-1 text-text-secondary truncate`

- [ ] **Step 2: 重写 Logs.tsx**

布局 (`flex flex-col gap-3 p-4 h-full`):
```
1. Header: 标题 "日志" + 实时指示 (● 绿色 "实时" / ⏸ 灰色 "已暂停")
2. Toolbar: 级别筛选标签 + 搜索框 (flex gap-1.5)
   - 筛选: "全部" | "Error" | "Warn" | "Info" | "Debug"
   - 选中态: bg-accent/15 border-accent text-accent
   - 默认态: bg-card border-border-subtle text-muted
   - 搜索: flex-1, bg-card, border, rounded-[5px], placeholder "搜索日志..."
3. LogList: flex-1 overflow-y-auto, bg-[#1a1a1a] dark:bg-[#111] rounded-[6px] border
   - logs.filter(levelFilter).filter(searchFilter).map(log => <LogEntry />)
   - 空态: EmptyState("暂无日志记录") 或 "未找到匹配的日志"
```

保持现有 useLogs hook 的 polling 机制和 auto-scroll 逻辑。

- [ ] **Step 3: 验证日志页**

手动验证:
- 等宽字体正确显示
- 级别颜色编码正确 (INFO=蓝, WARN=橙, ERROR=红)
- 筛选和搜索功能正常
- 自动滚动/暂停切换正常

- [ ] **Step 4: Commit**

```bash
git add macrdp-ui/src/components/LogEntry.tsx macrdp-ui/src/pages/Logs.tsx
git commit -m "feat(ui): rewrite Logs — Console.app style with monospace font and color-coded levels"
```

---

## Task 9: 统计页重写

**Files:**
- Rewrite: `macrdp-ui/src/pages/Statistics.tsx`

- [ ] **Step 1: 重写 Statistics.tsx**

布局 (`flex flex-col gap-3 p-4 overflow-y-auto`):
```
1. Header: "统计" + 时间范围切换 (7天 | 30天, pill 按钮组)
2. 汇总卡片行: 3× MetricCard
   - 总连接数 (blue)
   - 总流量 (green, 格式化 GB/MB)
   - 平均会话时长 (orange, 格式化 h m)
3. ChartPanel("流量趋势") → BarChart(trafficStats)
4. 连接历史表:
   bg-card rounded-[8px] p-3
   Header: 用户 | 时间 | 时长 | 流量 (text-[10px] text-muted font-medium)
   Rows: text-[11px], border-b border-border
   分页保持现有逻辑 (PAGE_SIZE=20)
```

数据: 复用现有 `getTrafficStats(days)` 和 `getConnectionHistory(limit, offset)`。
`days` 根据选中的时间范围切换 (7 or 30)。

空态: MetricCard 显示 `--`，ChartPanel 显示"暂无统计数据"，表格显示 EmptyState。

- [ ] **Step 2: 验证统计页**

手动验证:
- 7天/30天切换正确请求数据
- 柱状图正确渲染
- 分页功能正常
- 无数据时空态显示

- [ ] **Step 3: Commit**

```bash
git add macrdp-ui/src/pages/Statistics.tsx
git commit -m "feat(ui): rewrite Statistics — time range selector, bar chart, summary cards"
```

---

## Task 10: 权限页 + 关于页重写

**Files:**
- Rewrite: `macrdp-ui/src/pages/Permissions.tsx`
- Rewrite: `macrdp-ui/src/pages/About.tsx`
- Modify: `macrdp-ui/src/App.tsx` (移除 PermissionBanner import 和渲染)
- Delete: `macrdp-ui/src/components/PermissionCard.tsx` (内联到页面)
- Delete: `macrdp-ui/src/components/PermissionBanner.tsx` (替换为 inline 提示)

- [ ] **Step 1: 重写 Permissions.tsx**

布局 (`flex flex-col gap-3 p-4`):
```
1. Header: "权限"
2. 全部已授权时: 绿色提示条 (bg-green/10 border-green/20 text-green)
3. 权限卡片列表 (gap-2):
   每个卡片: bg-card rounded-[8px] p-3
   - 左侧: 图标 (Monitor/Hand/Mic) + 名称 + 状态文字
   - 右侧: 已授权 → 绿色 ✓ / 未授权 → "前往系统设置" 按钮 (橙色)
```

权限: screen_capture (屏幕录制), accessibility (辅助功能), microphone (麦克风)

- [ ] **Step 2: 重写 About.tsx**

布局 (`flex flex-col items-center justify-center h-full p-4`):
```
居中卡片 (bg-card rounded-[10px] p-6 max-w-sm text-center):
1. App 图标 (64px)
2. "MacRDP" (text-lg font-semibold)
3. 版本号 (text-xs text-muted)
4. 分隔线
5. 技术栈: IronRDP · OpenH264 · ScreenCaptureKit (text-[11px] text-muted)
6. 链接行: GitHub | License | 检查更新 (text-xs text-accent)
```

- [ ] **Step 3: 删除废弃组件 + 更新 App.tsx**

删除 `PermissionCard.tsx` 和 `PermissionBanner.tsx`。
同步修改 `App.tsx`: 移除 `import PermissionBanner` 和对应的 `<PermissionBanner />` 渲染（当前在 Sidebar 和 main content 之间）。权限警告功能已由新 Sidebar 的 permission 红点和 Permissions 页面覆盖。

Grep 确认无其他引用:
```bash
cd macrdp-ui && grep -r "PermissionBanner\|PermissionCard" src/ --include="*.tsx"
```

- [ ] **Step 4: 验证两个页面**

手动验证权限页和关于页显示正确。

- [ ] **Step 5: Commit**

```bash
git add macrdp-ui/src/pages/Permissions.tsx macrdp-ui/src/pages/About.tsx macrdp-ui/src/App.tsx
git rm macrdp-ui/src/components/PermissionCard.tsx macrdp-ui/src/components/PermissionBanner.tsx
git commit -m "feat(ui): rewrite Permissions and About pages, remove PermissionBanner"
```

---

## Task 11: 清理 + 全面验证

**Files:**
- Modify: Various (cleanup unused imports, dead code)

- [ ] **Step 1: 清理未使用的 shadcn 变量**

检查 `globals.css` 是否还有残留的旧变量引用。Grep 整个 `src/` 目录:
```bash
cd macrdp-ui && grep -r "macos-" src/ --include="*.tsx" --include="*.ts" --include="*.css"
```

清除所有对旧 `macos-*` 变量的引用，替换为新变量名。

- [ ] **Step 2: 检查未使用的 imports**

```bash
cd macrdp-ui && npx tsc --noEmit
```

修复所有 TypeScript 编译错误和未使用的 imports。

- [ ] **Step 3: 构建验证**

```bash
cd macrdp-ui && npm run build
```

Expected: 构建成功，无错误，无警告。

- [ ] **Step 4: 全面功能验证**

逐页手动测试:
- [ ] Dashboard: 启停服务、图表渲染、连接列表
- [ ] Settings: 所有分类、所有控件、配置保存
- [ ] Permissions: 权限状态检测、跳转系统设置
- [ ] Logs: 筛选、搜索、自动滚动
- [ ] Statistics: 时间范围切换、分页
- [ ] About: 版本信息、链接
- [ ] 主题切换: 浅色/深色/跟随系统 三态正常
- [ ] Popover: 不受影响，仍然可用

- [ ] **Step 5: Final Commit**

```bash
git add -A macrdp-ui/
git commit -m "chore(ui): cleanup dead code, fix remaining references to old design system"
```
