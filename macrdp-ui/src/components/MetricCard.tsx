interface MetricCardProps {
  value: number | null;
  label: string;
  color: "blue" | "green" | "orange" | "purple";
  formatter?: (v: number) => string;
}

const colorMap: Record<MetricCardProps["color"], string> = {
  blue: "text-accent",
  green: "text-green",
  orange: "text-orange",
  purple: "text-purple",
};

function MetricCard({ value, label, color, formatter }: MetricCardProps) {
  const hasValue = value != null;
  const display = hasValue
    ? formatter
      ? formatter(value)
      : String(value)
    : "--";
  const valueClass = hasValue ? colorMap[color] : "text-text-muted";

  return (
    <div className="flex-1 bg-card rounded-[8px] p-2 text-center">
      <div className={`text-xl font-bold ${valueClass}`}>{display}</div>
      <div className="text-[10px] text-text-muted mt-0.5">{label}</div>
    </div>
  );
}

export default MetricCard;
