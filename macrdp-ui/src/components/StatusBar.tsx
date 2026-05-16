import { Play, Square } from "lucide-react";

interface StatusBarProps {
  state: "running" | "starting" | "stopped" | "error";
  port?: number;
  uptimeSeconds?: number;
  errorMessage?: string;
  onStart: () => void;
  onStop: () => void;
}

function formatUptime(seconds: number): string {
  if (seconds < 60) return "< 1m";
  const h = Math.floor(seconds / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  if (h > 0) return `${h}h ${m}m`;
  return `${m}m`;
}

const statusConfig: Record<
  StatusBarProps["state"],
  { dotClass: string; glowColor: string; label: string }
> = {
  running: {
    dotClass: "bg-green",
    glowColor: "0 0 6px var(--color-green)",
    label: "运行中",
  },
  starting: {
    dotClass: "bg-yellow",
    glowColor: "0 0 6px var(--color-yellow)",
    label: "启动中",
  },
  stopped: {
    dotClass: "bg-text-muted",
    glowColor: "none",
    label: "已停止",
  },
  error: {
    dotClass: "bg-red",
    glowColor: "0 0 6px var(--color-red)",
    label: "错误",
  },
};

function StatusBar({
  state,
  port,
  uptimeSeconds,
  errorMessage,
  onStart,
  onStop,
}: StatusBarProps) {
  const config = statusConfig[state];
  const isError = state === "error";
  const showStop = state === "running" || state === "starting";

  const wrapperClass = isError
    ? "flex items-center h-9 bg-red/10 border border-red/20 rounded-[8px] px-3"
    : "flex items-center h-9 bg-card rounded-[8px] px-3";

  return (
    <div className={wrapperClass} role="status" aria-live="polite">
      {/* Left: status dot + text + aux */}
      <div className="flex items-center gap-2 flex-1 min-w-0">
        <span
          className={`inline-block h-2 w-2 rounded-full shrink-0 ${config.dotClass}`}
          style={{ boxShadow: config.glowColor }}
        />
        <span className="text-sm font-medium truncate">
          {isError && errorMessage ? errorMessage : config.label}
        </span>
        {!isError && (
          <div className="flex items-center gap-2 text-xs text-text-muted">
            {port != null && <span>:{port}</span>}
            {state === "running" && uptimeSeconds != null && (
              <span>{formatUptime(uptimeSeconds)}</span>
            )}
          </div>
        )}
      </div>

      {/* Right: start/stop button */}
      <div className="shrink-0 ml-2">
        {showStop ? (
          <button
            onClick={onStop}
            disabled={state === "starting"}
            className="inline-flex items-center gap-1.5 rounded-md bg-red/10 px-2.5 py-1 text-xs font-medium text-red hover:bg-red/20 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
          >
            <Square className="h-3 w-3" />
            停止
          </button>
        ) : (
          <button
            onClick={onStart}
            className="inline-flex items-center gap-1.5 rounded-md bg-green/10 px-2.5 py-1 text-xs font-medium text-green hover:bg-green/20 transition-colors"
          >
            <Play className="h-3 w-3" />
            启动
          </button>
        )}
      </div>
    </div>
  );
}

export default StatusBar;
