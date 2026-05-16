import { useState, useEffect } from "react";
import { useServerStatus } from "../hooks/useServerStatus";
import { useMetrics } from "../hooks/useMetrics";
import { useConnections } from "../hooks/useConnections";
import { useFpsHistory } from "../hooks/useFpsHistory";
import { api } from "../lib/ipc";
import StatusBar from "../components/StatusBar";
import MetricCard from "../components/MetricCard";
import { ChartPanel } from "../components/ChartPanel";
import { AreaChart } from "../components/AreaChart";
import ConnItem from "../components/ConnItem";
import EmptyState from "../components/EmptyState";
import type { UiConfig } from "../lib/types";

function Dashboard() {
  const status = useServerStatus();
  const { metrics, stale } = useMetrics();
  const connections = useConnections();
  const [loading, setLoading] = useState(false);
  const [config, setConfig] = useState<UiConfig | null>(null);

  const metricsAvailable = status.running && metrics && !stale;
  const fpsHistory = useFpsHistory(metricsAvailable ? metrics.fps : null);
  const port = config?.port ?? 3389;

  // Load config on mount for port display
  useEffect(() => {
    api.getConfig().then(setConfig).catch(console.error);
  }, []);

  const handleStart = async () => {
    setLoading(true);
    try {
      await api.startServer();
    } catch (err: unknown) {
      console.error("Failed to start server:", err);
    } finally {
      setLoading(false);
    }
  };

  const handleStop = async () => {
    setLoading(true);
    try {
      await api.stopServer();
    } catch (err: unknown) {
      console.error("Failed to stop server:", err);
    } finally {
      setLoading(false);
    }
  };

  const avgFps =
    fpsHistory.length > 0
      ? Math.round(fpsHistory.reduce((a, b) => a + b, 0) / fpsHistory.length)
      : null;

  const chartEmpty = !metricsAvailable || fpsHistory.length === 0;

  return (
    <div className="flex flex-col gap-3 p-4 overflow-y-auto">
      {/* 1. Status bar */}
      <StatusBar
        state={status.state}
        port={port}
        uptimeSeconds={status.uptime_secs}
        onStart={handleStart}
        onStop={handleStop}
      />

      {/* 2. FPS chart */}
      <ChartPanel
        title="帧率 (FPS)"
        value={avgFps != null ? `${avgFps} FPS` : undefined}
        valueColor="var(--color-accent)"
        empty={chartEmpty}
        emptyText="等待数据..."
      >
        <AreaChart data={fpsHistory} color="var(--color-accent)" />
      </ChartPanel>

      {/* 3. Metric cards row */}
      <div className="flex gap-2">
        <MetricCard
          value={metricsAvailable ? metrics.fps : null}
          label="FPS"
          color="blue"
        />
        <MetricCard
          value={metricsAvailable ? metrics.bitrate_kbps : null}
          label="Mbps"
          color="green"
          formatter={(v) => (v / 1000).toFixed(1)}
        />
        <MetricCard
          value={metricsAvailable ? metrics.latency_ms : null}
          label="ms 延迟"
          color="orange"
        />
        <MetricCard
          value={metricsAvailable ? connections.length : null}
          label="连接"
          color="purple"
        />
      </div>

      {/* 4. Connection panel */}
      <div className="bg-card rounded-[8px] p-3">
        <div className="flex items-center gap-2 mb-2">
          <span className="text-[11px] text-text-muted font-medium">
            活跃连接
          </span>
          <span className="text-accent bg-accent/10 rounded-full px-2 text-[10px]">
            {connections.length}
          </span>
        </div>
        {connections.length > 0 ? (
          <div className="flex flex-col gap-1">
            {connections.map((c) => (
              <ConnItem
                key={`${c.client_ip}-${c.connected_at}`}
                connection={c}
              />
            ))}
          </div>
        ) : (
          <EmptyState message="暂无活跃连接" />
        )}
      </div>
    </div>
  );
}

export default Dashboard;
