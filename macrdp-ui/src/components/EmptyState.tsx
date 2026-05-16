import type { LucideIcon } from "lucide-react";

interface EmptyStateProps {
  icon?: LucideIcon;
  message: string;
}

function EmptyState({ icon: Icon, message }: EmptyStateProps) {
  return (
    <div className="flex flex-col items-center justify-center py-8 text-text-muted">
      {Icon && <Icon className="h-8 w-8 mb-2" />}
      <span className="text-xs">{message}</span>
    </div>
  );
}

export default EmptyState;
