interface SettingRowProps {
  label: string;
  description?: string;
  children: React.ReactNode;
}

export function SettingRow({ label, description, children }: SettingRowProps) {
  return (
    <div className="flex items-center justify-between bg-card rounded-[6px] px-3 py-2">
      <div className="flex flex-col gap-0.5 mr-4">
        <span className="text-xs text-text-secondary">{label}</span>
        {description && (
          <span className="text-[10px] text-text-muted">{description}</span>
        )}
      </div>
      <div className="shrink-0">{children}</div>
    </div>
  );
}
