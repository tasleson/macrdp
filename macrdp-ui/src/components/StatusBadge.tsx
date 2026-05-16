interface StatusBadgeProps {
  status: "running" | "stopped" | "starting" | "error";
  text?: string;
}

const statusConfig: Record<
  StatusBadgeProps["status"],
  { color: string; label: string }
> = {
  running: { color: "bg-green", label: "运行中" },
  stopped: { color: "bg-text-muted", label: "已停止" },
  starting: { color: "bg-yellow", label: "启动中" },
  error: { color: "bg-red", label: "错误" },
};

function StatusBadge({ status, text }: StatusBadgeProps) {
  const config = statusConfig[status];
  return (
    <div className="flex items-center gap-2">
      <div className={`h-2 w-2 rounded-full ${config.color}`} />
      <span className="text-sm text-text-muted">
        {text ?? config.label}
      </span>
    </div>
  );
}

export default StatusBadge;
