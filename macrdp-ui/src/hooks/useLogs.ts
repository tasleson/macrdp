import { useState, useEffect, useRef, useCallback } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { LogEntry } from "../lib/types";

const POLL_INTERVAL = 1000; // 1 second

export function useLogs() {
  const [logs, setLogs] = useState<LogEntry[]>([]);
  const [autoScroll, setAutoScroll] = useState(true);
  const logsRef = useRef<LogEntry[]>([]);

  useEffect(() => {
    let active = true;

    const poll = async () => {
      if (!active) return;
      try {
        const entries = await invoke<LogEntry[]>("get_logs", { limit: 2000 });
        if (active && entries.length > 0) {
          // get_logs returns newest first, reverse for display
          const ordered = entries.reverse();
          logsRef.current = ordered;
          setLogs(ordered);
        }
      } catch {}
      if (active) setTimeout(poll, POLL_INTERVAL);
    };

    poll();
    return () => { active = false; };
  }, []);

  const clearLogs = useCallback(() => {
    logsRef.current = [];
    setLogs([]);
  }, []);

  return { logs, autoScroll, setAutoScroll, clearLogs };
}
