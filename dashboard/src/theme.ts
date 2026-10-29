import { useEffect, useState } from "react";

export type Theme = "dark" | "light";

export function useTheme() {
  const [theme, setTheme] = useState<Theme>(() =>
    document.documentElement.dataset.theme === "light" ? "light" : "dark",
  );

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.querySelector('meta[name="theme-color"]')?.setAttribute(
      "content",
      theme === "light" ? "#f8fafc" : "#06090f",
    );
    try {
      localStorage.setItem("wms-theme", theme);
    } catch {
      // Theme switching remains available when browser storage is disabled.
    }
  }, [theme]);

  return { theme, toggleTheme: () => setTheme(theme === "dark" ? "light" : "dark") };
}
