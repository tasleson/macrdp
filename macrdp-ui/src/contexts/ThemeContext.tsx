import { createContext, useContext, useEffect, useState, type ReactNode } from "react";
import { api } from "../lib/ipc";

type Theme = "system" | "light" | "dark";

interface ThemeContextValue {
  theme: Theme;
  setTheme: (theme: Theme) => void;
}

const ThemeContext = createContext<ThemeContextValue>({
  theme: "system",
  setTheme: () => {},
});

export function ThemeProvider({ children }: { children: ReactNode }) {
  const [theme, setThemeState] = useState<Theme>("system");

  useEffect(() => {
    api.getConfig().then((config) => {
      const t = config.theme as Theme;
      if (t && ["system", "light", "dark"].includes(t)) {
        setThemeState(t);
        applyTheme(t);
      }
    }).catch(() => {});
  }, []);

  const setTheme = (t: Theme) => {
    setThemeState(t);
    applyTheme(t);
    api.setConfig("theme", t).catch(console.error);
  };

  return (
    <ThemeContext.Provider value={{ theme, setTheme }}>
      {children}
    </ThemeContext.Provider>
  );
}

export function useTheme() {
  return useContext(ThemeContext);
}

function applyTheme(theme: Theme) {
  const root = document.documentElement;
  root.removeAttribute("data-theme");
  root.classList.remove("dark");

  if (theme === "dark") {
    root.setAttribute("data-theme", "dark");
    root.classList.add("dark");
  } else if (theme === "light") {
    root.setAttribute("data-theme", "light");
  } else {
    // "system" — sync .dark class with OS preference
    if (window.matchMedia("(prefers-color-scheme: dark)").matches) {
      root.classList.add("dark");
    }
  }
}

// Listen for OS theme changes and re-apply when in "system" mode
if (typeof window !== "undefined") {
  window.matchMedia("(prefers-color-scheme: dark)").addEventListener("change", () => {
    const root = document.documentElement;
    if (!root.hasAttribute("data-theme")) {
      root.classList.toggle("dark", window.matchMedia("(prefers-color-scheme: dark)").matches);
    }
  });
}
