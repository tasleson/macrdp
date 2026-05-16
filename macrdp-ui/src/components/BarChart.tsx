interface BarChartProps {
  data: { label: string; value: number }[];
  color: string;
  height?: number;
}

export function BarChart({ data, color, height = 70 }: BarChartProps) {
  if (data.length === 0) return null;

  const max = Math.max(...data.map(d => d.value), 1);
  const gap = 2;
  const count = data.length;
  // Each bar: (totalWidth - gaps) / count
  // Use viewBox width = count * barWidth + (count - 1) * gap
  const barWidth = 20;
  const totalWidth = count * barWidth + (count - 1) * gap;

  return (
    <svg viewBox={`0 0 ${totalWidth} ${height}`} width="100%" height={height} className="block">
      {data.map((d, i) => {
        const barHeight = (d.value / max) * (height - 4); // leave 4px top padding
        const x = i * (barWidth + gap);
        const y = height - barHeight;
        return (
          <rect
            key={d.label}
            x={x}
            y={y}
            width={barWidth}
            height={barHeight}
            rx={2}
            fill={color}
            opacity={0.7}
            className="transition-opacity duration-150 hover:opacity-100"
          />
        );
      })}
    </svg>
  );
}
