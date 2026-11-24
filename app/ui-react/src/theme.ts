/*
 * 主题状态：跟随系统 / 浅色 / 深色
 * ==============================
 * 存在 localStorage 的 `pf.theme`，取值 system | light | dark。
 * 启动时必须在 React 渲染前调用 applyTheme()（main.tsx 里），否则会先闪一帧浅色。
 * 原生状态栏图标明暗走 tauri.ts 的 setSystemBarsLight（桌面/浏览器 no-op）。
 */
import { setSystemBarsLight } from "./tauri";

export type ThemeMode = "system" | "light" | "dark";
export type ResolvedTheme = "light" | "dark";

const KEY = "pf.theme";
const MEDIA = "(prefers-color-scheme: dark)";

/** 安卓 WebView 里 matchMedia 一定存在；桌面预览也支持。 */
function prefersDark(): boolean {
  return typeof window !== "undefined" && typeof window.matchMedia === "function"
    ? window.matchMedia(MEDIA).matches
    : false;
}

export function readThemeMode(): ThemeMode {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw === "light" || raw === "dark") return raw;
  } catch {
    /* 隐私模式读不到就当跟随系统 */
  }
  return "system";
}

export function resolveTheme(mode: ThemeMode): ResolvedTheme {
  if (mode === "system") return prefersDark() ? "dark" : "light";
  return mode;
}

/** 把主题写到 <html> 上，并同步系统栏图标。返回实际生效的主题。 */
export function applyTheme(mode: ThemeMode): ResolvedTheme {
  const resolved = resolveTheme(mode);
  const root = document.documentElement;
  root.dataset.theme = resolved;
  root.style.colorScheme = resolved;
  // 安卓地址栏/状态栏取色（WebView 里基本不显示，桌面调试时有用）
  const meta = document.querySelector('meta[name="theme-color"]');
  if (meta) meta.setAttribute("content", resolved === "dark" ? "#0e1014" : "#f5f6f9");
  // 浅色页 → 深色图标（light=true）；深色页 → 白色图标
  setSystemBarsLight(resolved === "light");
  return resolved;
}

export function saveThemeMode(mode: ThemeMode): void {
  try {
    localStorage.setItem(KEY, mode);
  } catch {
    /* 存不进去只影响下次启动，不影响本次切换 */
  }
}

/**
 * 跟随系统时监听系统切换；返回取消函数。
 * 只负责「系统变了重算一次」，模式仍是 system。
 */
export function watchSystemTheme(onChange: () => void): () => void {
  if (typeof window === "undefined" || typeof window.matchMedia !== "function") {
    return () => {};
  }
  const mql = window.matchMedia(MEDIA);
  const handler = () => onChange();
  if (typeof mql.addEventListener === "function") {
    mql.addEventListener("change", handler);
    return () => mql.removeEventListener("change", handler);
  }
  // 老内核（Chrome < 84 的 WebView）只有 addListener
  mql.addListener(handler);
  return () => mql.removeListener(handler);
}
