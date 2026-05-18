import { useRef, useState, useEffect } from "react";

export function useMetricHistory(currentValue: number | null, maxPoints = 60): number[] {
  const bufferRef = useRef<number[]>([]);
  const [, setTick] = useState(0);

  useEffect(() => {
    if (currentValue === null) {
      if (bufferRef.current.length > 0) {
        bufferRef.current = [];
        setTick(t => t + 1);
      }
      return;
    }
    const buf = bufferRef.current;
    buf.push(currentValue);
    if (buf.length > maxPoints) buf.shift();
    setTick(t => t + 1);
  }, [currentValue, maxPoints]);

  return bufferRef.current;
}
