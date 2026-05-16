import { Connection } from "../lib/types";

interface ConnItemProps {
  connection: Connection;
}

function formatDuration(connectedAt: string): string {
  const elapsed = Date.now() - new Date(connectedAt).getTime();
  const minutes = Math.floor(elapsed / 60_000);
  if (minutes < 1) return "< 1m";
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  const rem = minutes % 60;
  return `${hours}h ${rem}m`;
}

function formatBytes(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1_048_576) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1_073_741_824) return `${(bytes / 1_048_576).toFixed(1)} MB`;
  return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
}

function getIndicatorColor(connectedAt: string): string {
  const elapsed = Date.now() - new Date(connectedAt).getTime();
  const minutes = elapsed / 60_000;
  if (minutes > 10) return "bg-green";
  if (minutes > 1) return "bg-orange";
  return "bg-accent";
}

function ConnItem({ connection }: ConnItemProps) {
  const { client_name, client_ip, connected_at, bytes_total } = connection;
  const indicatorColor = getIndicatorColor(connected_at);

  return (
    <div className="flex items-center justify-between bg-card rounded-[8px] p-2">
      <div className="flex items-center gap-2">
        <div className={`w-[3px] h-8 rounded-full ${indicatorColor}`} />
        <div className="flex flex-col">
          <span className="text-xs font-medium text-text">
            {client_name}@{client_ip}
          </span>
          <span className="text-[10px] text-text-muted">
            {formatDuration(connected_at)}
          </span>
        </div>
      </div>
      <div className="flex flex-col items-end">
        <span className="text-[11px] font-medium text-text">
          {formatBytes(bytes_total)}
        </span>
        <span className="text-[10px] text-text-muted">Traffic</span>
      </div>
    </div>
  );
}

export default ConnItem;
