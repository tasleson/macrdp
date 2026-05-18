import { useState, useEffect } from "react";
import { AlertTriangle, X } from "lucide-react";
import { api } from "../lib/ipc";
import type { UiConfig } from "../lib/types";
import { Input } from "../components/ui/input";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "../components/ui/select";
import { Switch } from "../components/ui/switch";
import { Alert, AlertDescription } from "../components/ui/alert";
import { Button } from "../components/ui/button";
import { SettingRow } from "../components/SettingRow";
import { useTheme } from "../contexts/ThemeContext";

const categories = [
  { id: "rdp", label: "RDP 服务" },
  { id: "network", label: "网络" },
  { id: "security", label: "安全" },
  { id: "display", label: "显示" },
  { id: "audio", label: "音频" },
  { id: "general", label: "通用" },
] as const;

type CategoryId = (typeof categories)[number]["id"];

function Settings() {
  const [config, setConfig] = useState<UiConfig | null>(null);
  const [restartRequired, setRestartRequired] = useState(false);
  const [activeCategory, setActiveCategory] = useState<CategoryId>("rdp");
  const { theme, setTheme } = useTheme();

  useEffect(() => {
    api.getConfig().then(setConfig).catch(console.error);
  }, []);

  const updateConfig = async (key: keyof UiConfig, value: unknown) => {
    try {
      const result = await api.setConfig(key, value);
      if (result.restart_required) {
        setRestartRequired(true);
      }
      setConfig((prev) => (prev ? { ...prev, [key]: value } : prev));
    } catch (err) {
      console.error("Failed to update config:", err);
    }
  };

  const handleAutostart = async (enabled: boolean) => {
    try {
      await api.setAutostart(enabled);
      await updateConfig("autostart", enabled);
    } catch (err) {
      console.error("Failed to set autostart:", err);
    }
  };

  if (!config) {
    return (
      <div className="flex items-center justify-center py-20 text-sm text-text-muted">
        加载配置中...
      </div>
    );
  }

  return (
    <div className="flex h-full">
      {/* Left nav */}
      <nav className="w-[140px] shrink-0 border-r border-border py-3 px-2 space-y-0.5">
        {categories.map((cat) => (
          <button
            key={cat.id}
            onClick={() => setActiveCategory(cat.id)}
            className={`w-full text-left text-xs px-2.5 py-1.5 rounded-md transition-colors ${
              activeCategory === cat.id
                ? "bg-accent/12 text-accent font-medium"
                : "text-text-secondary hover:text-text hover:bg-card"
            }`}
          >
            {cat.label}
          </button>
        ))}
      </nav>

      {/* Right panel */}
      <div className="flex-1 overflow-y-auto p-4">
        {/* Restart required banner */}
        {restartRequired && (
          <Alert
            variant="default"
            className="mb-4 border-yellow-400/60 bg-yellow-50 dark:bg-yellow-950/30"
          >
            <AlertTriangle className="h-4 w-4 text-yellow-600 dark:text-yellow-400" />
            <AlertDescription className="flex items-center justify-between">
              <span className="text-yellow-800 dark:text-yellow-300 text-xs">
                部分配置需重启服务后生效
              </span>
              <Button
                variant="ghost"
                size="sm"
                className="h-6 w-6 p-0 text-yellow-700 hover:text-yellow-900 dark:text-yellow-400"
                onClick={() => setRestartRequired(false)}
              >
                <X className="h-3.5 w-3.5" />
              </Button>
            </AlertDescription>
          </Alert>
        )}

        {activeCategory === "rdp" && (
          <CategorySection title="RDP 服务">
            <SettingRow label="编码器">
              <Select
                value={config.encoder}
                onValueChange={(v) => updateConfig("encoder", v)}
              >
                <SelectTrigger className="h-7 w-[120px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="software">软件编码</SelectItem>
                  <SelectItem value="hardware">硬件加速</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>

            <SettingRow label="比特率">
              <Select
                value={String(config.bitrate_mbps)}
                onValueChange={(v) => updateConfig("bitrate_mbps", Number(v))}
              >
                <SelectTrigger className="h-7 w-[120px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="2">2 Mbps</SelectItem>
                  <SelectItem value="4">4 Mbps</SelectItem>
                  <SelectItem value="8">8 Mbps</SelectItem>
                  <SelectItem value="16">16 Mbps</SelectItem>
                  <SelectItem value="32">32 Mbps</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>

            <SettingRow label="帧率">
              <Select
                value={String(config.frame_rate)}
                onValueChange={(v) => updateConfig("frame_rate", Number(v))}
              >
                <SelectTrigger className="h-7 w-[120px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="30">30 fps</SelectItem>
                  <SelectItem value="60">60 fps</SelectItem>
                  <SelectItem value="120">120 fps</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>

            <SettingRow label="色度模式">
              <Select
                value={config.chroma_mode}
                onValueChange={(v) => updateConfig("chroma_mode", v)}
              >
                <SelectTrigger className="h-7 w-[120px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="avc420">avc420</SelectItem>
                  <SelectItem value="avc444">avc444</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>
          </CategorySection>
        )}

        {activeCategory === "network" && (
          <CategorySection title="网络">
            <SettingRow label="端口" description="重启服务生效">
              <Input
                type="number"
                value={config.port}
                onChange={(e) => updateConfig("port", parseInt(e.target.value, 10))}
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>

            <SettingRow label="绑定地址" description="重启服务生效">
              <Input
                type="text"
                value={config.bind_address}
                placeholder="0.0.0.0"
                onChange={(e) =>
                  setConfig((prev) =>
                    prev ? { ...prev, bind_address: e.target.value } : prev
                  )
                }
                onBlur={(e) => updateConfig("bind_address", e.target.value)}
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>

            <SettingRow label="最大连接数" description="重启服务生效">
              <Input
                type="number"
                value={config.max_connections}
                onChange={(e) =>
                  updateConfig("max_connections", parseInt(e.target.value, 10))
                }
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>

            <SettingRow label="空闲超时 (秒)" description="重启服务生效">
              <Input
                type="number"
                value={config.idle_timeout_secs}
                onChange={(e) =>
                  updateConfig("idle_timeout_secs", parseInt(e.target.value, 10))
                }
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>
          </CategorySection>
        )}

        {activeCategory === "security" && (
          <CategorySection title="安全">
            <SettingRow label="用户名">
              <Input
                type="text"
                value={config.username}
                onChange={(e) =>
                  setConfig((prev) =>
                    prev ? { ...prev, username: e.target.value } : prev
                  )
                }
                onBlur={(e) => updateConfig("username", e.target.value)}
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>

            <SettingRow label="密码">
              <Input
                type="password"
                value={config.password}
                onChange={(e) =>
                  setConfig((prev) =>
                    prev ? { ...prev, password: e.target.value } : prev
                  )
                }
                onBlur={(e) => updateConfig("password", e.target.value)}
                className="h-7 w-[120px] text-xs"
              />
            </SettingRow>
          </CategorySection>
        )}

        {activeCategory === "display" && (
          <CategorySection title="显示">
            <SettingRow label="分辨率" description="新连接生效，Auto 跟随屏幕">
              <Select
                value={config.resolution || "auto"}
                onValueChange={(v) => updateConfig("resolution", v)}
              >
                <SelectTrigger className="h-7 w-[180px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="auto">Auto</SelectItem>
                  <SelectItem value="7680x4320">7680 × 4320</SelectItem>
                  <SelectItem value="5120x2880">5120 × 2880</SelectItem>
                  <SelectItem value="3840x2160">3840 × 2160</SelectItem>
                  <SelectItem value="2560x1600">2560 × 1600</SelectItem>
                  <SelectItem value="2560x1440">2560 × 1440</SelectItem>
                  <SelectItem value="2048x1536">2048 × 1536</SelectItem>
                  <SelectItem value="1920x1440">1920 × 1440</SelectItem>
                  <SelectItem value="1920x1200">1920 × 1200</SelectItem>
                  <SelectItem value="1920x1080">1920 × 1080</SelectItem>
                  <SelectItem value="1680x1050">1680 × 1050</SelectItem>
                  <SelectItem value="1600x1200">1600 × 1200</SelectItem>
                  <SelectItem value="1600x1024">1600 × 1024</SelectItem>
                  <SelectItem value="1600x900">1600 × 900</SelectItem>
                  <SelectItem value="1440x1080">1440 × 1080</SelectItem>
                  <SelectItem value="1366x768">1366 × 768</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>

            <SettingRow label="显示光标" description="在采集画面中渲染系统光标">
              <Switch
                checked={config.show_cursor}
                onCheckedChange={(checked) => updateConfig("show_cursor", checked)}
              />
            </SettingRow>
          </CategorySection>
        )}

        {activeCategory === "audio" && (
          <CategorySection title="音频">
            <div className="flex items-center justify-center py-10 text-xs text-text-muted">
              音频配置尚未开放
            </div>
          </CategorySection>
        )}

        {activeCategory === "general" && (
          <CategorySection title="通用">
            <SettingRow label="日志级别">
              <Select
                value={config.log_level}
                onValueChange={(v) => updateConfig("log_level", v)}
              >
                <SelectTrigger className="h-7 w-[120px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="trace">trace</SelectItem>
                  <SelectItem value="debug">debug</SelectItem>
                  <SelectItem value="info">info</SelectItem>
                  <SelectItem value="warn">warn</SelectItem>
                  <SelectItem value="error">error</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>

            <SettingRow label="开机自启" description="登录时自动启动服务">
              <Switch
                checked={config.autostart}
                onCheckedChange={handleAutostart}
              />
            </SettingRow>

            <SettingRow label="主题">
              <Select
                value={theme}
                onValueChange={(v) => {
                  if (v) setTheme(v as "system" | "light" | "dark");
                }}
              >
                <SelectTrigger className="h-7 w-[120px] text-xs">
                  <SelectValue />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem value="system">跟随系统</SelectItem>
                  <SelectItem value="light">浅色</SelectItem>
                  <SelectItem value="dark">深色</SelectItem>
                </SelectContent>
              </Select>
            </SettingRow>
          </CategorySection>
        )}
      </div>
    </div>
  );
}

function CategorySection({
  title,
  children,
}: {
  title: string;
  children: React.ReactNode;
}) {
  return (
    <div>
      <h2 className="text-sm font-medium text-text mb-3">{title}</h2>
      <div className="space-y-1.5">{children}</div>
    </div>
  );
}

export default Settings;
