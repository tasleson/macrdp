import type { LogEntry } from "../lib/types";

interface LogEntryRowProps {
  entry: LogEntry;
}

const levelColors: Record<LogEntry["level"], string> = {
  info: "text-accent",
  warn: "text-orange",
  error: "text-red",
  debug: "text-text-muted",
  trace: "text-text-muted",
};

function formatTimestamp(ts: string): string {
  try {
    const d = new Date(ts);
    return d.toLocaleTimeString("zh-CN", {
      hour12: false,
      hour: "2-digit",
      minute: "2-digit",
      second: "2-digit",
    });
  } catch {
    return ts;
  }
}

function LogEntryRow({ entry }: LogEntryRowProps) {
  return (
    <div className="flex gap-2 px-2.5 py-1 border-b border-border font-mono text-[11px]">
      <span className="min-w-[70px] text-text-muted">
        {formatTimestamp(entry.timestamp)}
      </span>
      <span className={`min-w-[40px] font-semibold ${levelColors[entry.level]}`}>
        {entry.level.toUpperCase()}
      </span>
      <span className="flex-1 text-text-secondary truncate">
        {entry.message}
      </span>
    </div>
  );
}

export default LogEntryRow;
