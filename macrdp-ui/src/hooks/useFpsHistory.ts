import { useRef, useState, useEffect } from "react";

export function useFpsHistory(currentFps: number | null, maxPoints = 60): number[] {
  const bufferRef = useRef<number[]>([]);
  const [, setTick] = useState(0);

  useEffect(() => {
    if (currentFps === null) return;
    const buf = bufferRef.current;
    buf.push(currentFps);
    if (buf.length > maxPoints) buf.shift();
    setTick(t => t + 1);
  }, [currentFps, maxPoints]);

  return bufferRef.current;
}
