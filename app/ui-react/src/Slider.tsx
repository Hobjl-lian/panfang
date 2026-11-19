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
        className={`relative w-full h-1.5 rounded-full ${className || "bg-black/10"}`}
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
            className={`w-5 h-5 rounded-full bg-white border-2 shadow-[0_1px_4px_rgba(0,0,0,0.2)] transition-colors ${
              active ? "border-[#4361EE]" : "border-black/15"
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
      <div className="flex items-center gap-1 w-[52px] shrink-0 text-black/45 whitespace-nowrap">
        {icon}
        <span className="text-[12px] font-medium">{label}</span>
      </div>
      <DragSlider
        value={value}
        min={ctl.min}
        max={ctl.max}
        step={ctl.step}
        onChange={onChange}
        disabled={disabled}
      />
      <span className="text-[12px] font-mono font-medium text-black/55 w-[44px] text-right shrink-0">
        {text}
      </span>
    </div>
  );
}
