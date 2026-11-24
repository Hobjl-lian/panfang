/*
 * 字号档位：小 / 标准 / 大
 * =========================
 * 存在 localStorage 的 `pf.fontSize`，实际生效的是 <html> 上的 `--pf-fs-scale`，
 * 字号刻度全部在 type.css 里按这个因子换算（组件只引用 pf-fs-* 类）。
 * 启动时必须在 React 渲染前调用 applyFontSize()（main.tsx 里），否则会先闪一帧标准字号。
 */

export type FontSizeMode = "small" | "standard" | "large";

const KEY = "pf.fontSize";

/** 三档的缩放因子。改这里等于改全站字号，不要在其他地方再写一套换算。 */
const SCALE: Record<FontSizeMode, number> = {
  small: 0.9,
  standard: 1,
  large: 1.1,
};

export function readFontSizeMode(): FontSizeMode {
  try {
    const raw = localStorage.getItem(KEY);
    if (raw === "small" || raw === "large") return raw;
  } catch {
    /* 隐私模式读不到就按标准 */
  }
  return "standard";
}

/** 把档位写到 <html> 的内联样式上（内联样式优先级高于 type.css 里的默认值 1）。 */
export function applyFontSize(mode: FontSizeMode): void {
  if (typeof document === "undefined") return;
  document.documentElement.style.setProperty("--pf-fs-scale", String(SCALE[mode]));
}

export function saveFontSizeMode(mode: FontSizeMode): void {
  try {
    localStorage.setItem(KEY, mode);
  } catch {
    /* 存不进去只影响下次启动，不影响本次切换 */
  }
}

/** 设置页摘要用：跟随系统，标准字号 */
export function fontSizeLabel(mode: FontSizeMode): string {
  if (mode === "small") return "小";
  if (mode === "large") return "大";
  return "标准";
}
