/**
 * 共用拖拽滑杆（表盘编辑 / 壁纸生成 / 视频表盘同一套）。
 *
 * **只有白点周围 44px 的圆形热区能起拖，轨道本身完全不响应触摸**（用户 2026-11-13）：
 * 原来用原生 `<input type=range>`，手指按在轨道任意位置都会立刻跳值并开始拖，
 * 一页里两层照片各有四根滑杆，上下滑动列表时几乎每次都误触。
 * 现在轨道只是画出来的，不接事件；改动参数必须按住白点。
 *
 * 白点热区用 `touch-action:none` 吃掉垂直手势，避免拖动时连带滚动整页。
 */
import { useRef, useState } from "react";
import type { CSSProperties, ReactNode } from "react";
import type { GenNumCtl } from "./tauri";

export function DragSlider(props: {
  value: number;
  min: number;
  max: number;
  step: number;
  onChange: (v: number) => void;
  /** 轨道外观（纯色色板用渐变），默认灰底 */
  trackStyle?: CSSProperties;
  className?: string;
  disabled?: boolean;
}) {
  const { value, min, max, step, onChange, trackStyle, className, disabled } = props;
  const trackRef = useRef<HTMLDivElement | null>(null);
  const dragging = useRef(false);
  const [active, setActive] = useState(false);

  const lo = Math.min(min, max);
  const hi = Math.max(min, max);
  const st = step > 0 ? step : 0.01;
  const cur = Math.min(hi, Math.max(lo, value));
  const pct = hi > lo ? (cur - lo) / (hi - lo) : 0;

  const setFromClientX = (clientX: number) => {
    const el = trackRef.current;
    if (!el) return;
    const r = el.getBoundingClientRect();
    if (r.width <= 0) return;
    const k = Math.min(1, Math.max(0, (clientX - r.left) / r.width));
    const snapped = Math.round((lo + (hi - lo) * k) / st) * st;
    onChange(Math.min(hi, Math.max(lo, Number(snapped.toFixed(6)))));
  };

  return (
    <div className={`flex-1 min-w-0 h-10 flex items-center ${disabled ? "opacity-45" : ""}`}>
      <div
        ref={trackRef}
        className={`relative w-full h-1.5 rounded-full ${className || "bg-[var(--pf-bg-10)]"}`}
        style={trackStyle}
      >
        <div
          role="slider"
          aria-valuemin={lo}
          aria-valuemax={hi}
          aria-valuenow={cur}
          aria-disabled={disabled || undefined}
          className={`absolute top-1/2 w-11 h-11 -translate-x-1/2 -translate-y-1/2 flex items-center justify-center ${
            disabled ? "cursor-not-allowed" : "cursor-grab active:cursor-grabbing"
          }`}
          style={{ left: `${pct * 100}%`, touchAction: "none" }}
          onPointerDown={e => {
            if (disabled) return;
            dragging.current = true;
            setActive(true);
            try {
              e.currentTarget.setPointerCapture(e.pointerId);
            } catch {
              /* 老内核拿不到 capture 也能靠后续 move 事件 */
            }
          }}
          onPointerMove={e => {
            if (disabled) return;
            if (dragging.current) setFromClientX(e.clientX);
          }}
          onPointerUp={e => {
            if (!dragging.current) return;
            dragging.current = false;
            setActive(false);
            try {
              e.currentTarget.releasePointerCapture(e.pointerId);
            } catch {
              /* 已经释放 */
            }
          }}
          onPointerCancel={() => {
            dragging.current = false;
            setActive(false);
          }}
        >
          <div
            className={`w-5 h-5 rounded-full bg-[var(--pf-card)] border-2 shadow-[0_1px_4px_rgba(0,0,0,0.2)] transition-colors ${
              active ? "border-[var(--pf-primary)]" : "border-[var(--pf-bd-15)]"
            }`}
          />
        </div>
      </div>
    </div>
  );
}

/**
 * 一行滑杆（图标 + 标签 + 轨道 + 数值）。
 * 三列宽度固定（标签 52 / 轨道自适应 / 数值 44），行与行上下对齐；
 * 标签列 `whitespace-nowrap`，否则「缩放」会被拆成两行、行高跟着跳。
 */
export function GenSlider(props: {
  icon: ReactNode;
  label: string;
  value: number;
  ctl: GenNumCtl;
  text: string;
  onChange: (v: number) => void;
  disabled?: boolean;
}) {
  const { icon, label, value, ctl, text, onChange, disabled } = props;
  return (
    <div className="flex items-center gap-2.5 h-10">
      <div className="flex items-center gap-1 w-[52px] shrink-0 text-[var(--pf-i-45)] whitespace-nowrap">
        {icon}
        <span className="pf-fs-12 font-medium">{label}</span>
      </div>
      <DragSlider
        value={value}
        min={ctl.min}
        max={ctl.max}
        step={ctl.step}
        onChange={onChange}
        disabled={disabled}
      />
      <span className="pf-fs-12 font-mono font-medium text-[var(--pf-i-55)] w-[44px] text-right shrink-0">
        {text}
      </span>
    </div>
  );
}

/** #RRGGBB → 色相（0~360）+ 深浅（0~1，用 HSL 亮度近似；滑杆位置只是起点，颜色以预览为准） */
export function hexToHueShade(hex: string): { hue: number; shade: number } {
  const h = hex.replace("#", "");
  if (!/^[0-9a-fA-F]{6}$/.test(h)) return { hue: 0, shade: 0.5 };
  const r = parseInt(h.slice(0, 2), 16) / 255;
  const g = parseInt(h.slice(2, 4), 16) / 255;
  const b = parseInt(h.slice(4, 6), 16) / 255;
  const max = Math.max(r, g, b);
  const min = Math.min(r, g, b);
  const l = (max + min) / 2;
  const d = max - min;
  let hue = 0;
  if (d > 1e-6) {
    if (max === r) hue = ((g - b) / d + 6) % 6;
    else if (max === g) hue = (b - r) / d + 2;
    else hue = (r - g) / d + 4;
    hue *= 60;
  }
  return { hue, shade: l };
}

function hslToRgb(h: number, s: number, l: number): [number, number, number] {
  const c = (1 - Math.abs(2 * l - 1)) * s;
  const hp = (((h % 360) + 360) % 360) / 60;
  const x = c * (1 - Math.abs((hp % 2) - 1));
  let r = 0;
  let g = 0;
  let b = 0;
  if (hp < 1) [r, g, b] = [c, x, 0];
  else if (hp < 2) [r, g, b] = [x, c, 0];
  else if (hp < 3) [r, g, b] = [0, c, x];
  else if (hp < 4) [r, g, b] = [0, x, c];
  else if (hp < 5) [r, g, b] = [x, 0, c];
  else [r, g, b] = [c, 0, x];
  const m = l - c / 2;
  return [(r + m) * 255, (g + m) * 255, (b + m) * 255];
}

/**
 * 色相 + 深浅 → #RRGGBB。
 * 深浅 0 = 黑，0.5 = 当前色相的标准色，1 = 白；中间线性过渡。
 */
export function hueShadeToHex(hue: number, shade: number): string {
  const [r, g, b] = hslToRgb(hue, 1, 0.5);
  const t = Math.max(0, Math.min(1, shade));
  let rr: number;
  let gg: number;
  let bb: number;
  if (t <= 0.5) {
    const k = t * 2;
    rr = r * k;
    gg = g * k;
    bb = b * k;
  } else {
    const k = (t - 0.5) * 2;
    rr = r + (255 - r) * k;
    gg = g + (255 - g) * k;
    bb = b + (255 - b) * k;
  }
  const to2 = (v: number) => Math.round(v).toString(16).padStart(2, "0");
  return `#${to2(rr)}${to2(gg)}${to2(bb)}`.toUpperCase();
}

/**
 * 纯色自定义面板：只有两根滑杆（颜色 / 深浅），不做二维色板、HEX、RGB。
 * 支持空值状态（placeholder）：未选颜色前展示提示框，避免误以为已默认选中。
 */
export function GenCustomColorPanel(props: {
  value: string;
  onChange: (hex: string) => void;
  placeholder?: string;
}) {
  const { value, onChange, placeholder } = props;
  const hasVal = /^#[0-9a-fA-F]{6}$/.test(value);
  const init = useRef(hexToHueShade(hasVal ? value : "#3B82F6"));
  const [hue, setHue] = useState(init.current.hue);
  const [shade, setShade] = useState(init.current.shade);

  const setHueAndKeepVisible = (v: number) => {
    const nextShade = shade <= 0.02 || shade >= 0.98 ? 0.5 : shade;
    setHue(v);
    setShade(nextShade);
    onChange(hueShadeToHex(v, nextShade));
  };
  const setShadeAndApply = (v: number) => {
    setShade(v);
    onChange(hueShadeToHex(hue, v));
  };

  const hueTrack =
    "linear-gradient(to right, #ff0000 0%, #ffff00 16.66%, #00ff00 33.33%, #00ffff 50%, #0000ff 66.66%, #ff00ff 83.33%, #ff0000 100%)";
  const shadeTrack = `linear-gradient(to right, #000000, hsl(${Math.round(hue)}, 100%, 50%), #ffffff)`;

  return (
    <div>
      {hasVal ? (
        <div className="h-12 rounded-2xl border border-[var(--pf-bd-5)] mb-3" style={{ background: value }} />
      ) : (
        <div className="h-12 rounded-2xl border border-dashed border-[var(--pf-bd-15)] mb-3 flex items-center justify-center pf-fs-12 font-medium text-[var(--pf-i-40)]">
          {placeholder || "拖动下方滑杆选择颜色"}
        </div>
      )}

      <div className="flex items-center gap-2.5 h-10">
        <span className="pf-fs-12 font-medium text-[var(--pf-i-45)] w-[52px] shrink-0 whitespace-nowrap">
          颜色
        </span>
        <DragSlider
          value={hue}
          min={0}
          max={360}
          step={1}
          onChange={setHueAndKeepVisible}
          trackStyle={{ background: hueTrack }}
        />
      </div>

      <div className="flex items-center gap-2.5 h-10">
        <span className="pf-fs-12 font-medium text-[var(--pf-i-45)] w-[52px] shrink-0 whitespace-nowrap">
          深浅
        </span>
        <DragSlider
          value={Math.round(shade * 100)}
          min={0}
          max={100}
          step={1}
          onChange={v => setShadeAndApply(v / 100)}
          trackStyle={{ background: shadeTrack }}
        />
      </div>
      <div className="flex justify-between pf-fs-10 font-medium text-[var(--pf-i-35)] px-1 -mt-1">
        <span className="w-[52px]">黑</span>
        <span>白</span>
      </div>
    </div>
  );
}
