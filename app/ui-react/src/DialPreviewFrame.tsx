import {
  useId,
  type CSSProperties,
  type MouseEventHandler,
  type PointerEventHandler,
  type ReactNode,
  type Ref,
} from "react";

/**
 * 表盘预览外壳：统一的「屏面裁剪 + Squircle 圆角 + 钢化玻璃反光」质感。
 *
 * 集市列表、集市详情、表盘编辑画布、视频表盘制作/作品缩略图全都走这一套，
 * 保证同一张预览图在不同页面上的圆角、边缘高光和反光方向完全一致。
 */

/**
 * 预览屏面的裁剪圆角：**按设备的真实物理圆角**裁，而不是一刀切某个比例。
 *
 * 与 Rust 的 `screen_info_for_size` 同一套数值（那是权威来源）：
 *   212×520 → 106、192×490 → 280、336×480 → 52、432×514 → 112。
 * 半径按**画布像素**算再等比缩到预览尺寸，所以同一张图在列表和详情页圆角一致。
 *
 * 为什么不用固定比例：作者导出时烤的圆角深浅不一（实测有 13.5% / 24.3% / 47% 三档），
 * 固定比例要么盖不住、要么把作者做好的大圆角裁成小角。按物理圆角裁才是设备真实的形状。
 */
export function previewCornerRadius(screen: [number, number], displayW: number): number {
  const [w, h] = screen;
  const r = (() => {
    if (w === h) return w / 2;
    switch (`${w}x${h}`) {
      case "212x520": return 106;
      case "192x490": return 280;
      case "336x480": return 52;
      case "432x514": return 112;
      case "390x450": return 90;
      case "320x385": return 82;
      case "410x502": return 116;
      case "280x456": return 36;
      default: return Math.round(Math.min(w, h) * 0.13);
    }
  })();
  // 半径不能超过宽的一半；再按「画布宽 → 预览宽」等比缩放
  return Math.max(1, Math.round(Math.min(r, w / 2) * (displayW / w)));
}

/**
 * 胶囊屏（212×520 / 192×490）上下两端已是完整半圆，半径无法再加大；
 * 解包预览图在上下半圆弧边会带一圈细黑边，等比微放大 3% 裁掉黑边且不变形。
 */
export function previewImageScale(screen: [number, number]): number {
  const key = `${screen[0]}x${screen[1]}`;
  return key === "212x520" || key === "192x490" ? 1.03 : 1;
}

/** 用连续的二次/三次曲线拼出 Squircle（平滑圆角）路径，避免大圆角看起来「塌」下去 */
export function squirclePath(w: number, h: number, r: number, smoothing = 0.6): string {
  const maxR = Math.min(w, h) / 2;
  const rr = Math.max(0, Math.min(r, maxR));
  const f = (n: number) => Number(n.toFixed(2));

  if (rr <= 0.5) {
    return `M 0 0 H ${f(w)} V ${f(h)} H 0 Z`;
  }

  const s = Math.min(smoothing, maxR / rr - 1);

  if (s <= 0.02) {
    return [
      `M ${f(rr)} 0`,
      `H ${f(w - rr)}`,
      `A ${f(rr)} ${f(rr)} 0 0 1 ${f(w)} ${f(rr)}`,
      `V ${f(h - rr)}`,
      `A ${f(rr)} ${f(rr)} 0 0 1 ${f(w - rr)} ${f(h)}`,
      `H ${f(rr)}`,
      `A ${f(rr)} ${f(rr)} 0 0 1 0 ${f(h - rr)}`,
      `V ${f(rr)}`,
      `A ${f(rr)} ${f(rr)} 0 0 1 ${f(rr)} 0`,
      "Z",
    ].join(" ");
  }

  const p = (1 + s) * rr;
  const theta = (Math.PI / 4) * s;
  const x3 = rr * (1 - Math.sin(theta));
  const y3 = rr * (1 - Math.cos(theta));
  const xInt = rr * (1 - Math.tan(theta / 2));
  const c1 = p - ((p - xInt) * 2) / 3;
  const c2 = xInt;

  return [
    `M ${f(p)} 0`,
    `H ${f(w - p)}`,
    `C ${f(w - c1)} 0 ${f(w - c2)} 0 ${f(w - x3)} ${f(y3)}`,
    `A ${f(rr)} ${f(rr)} 0 0 1 ${f(w - y3)} ${f(x3)}`,
    `C ${f(w)} ${f(c2)} ${f(w)} ${f(c1)} ${f(w)} ${f(p)}`,
    `V ${f(h - p)}`,
    `C ${f(w)} ${f(h - c1)} ${f(w)} ${f(h - c2)} ${f(w - y3)} ${f(h - x3)}`,
    `A ${f(rr)} ${f(rr)} 0 0 1 ${f(w - x3)} ${f(h - y3)}`,
    `C ${f(w - c2)} ${f(h)} ${f(w - c1)} ${f(h)} ${f(w - p)} ${f(h)}`,
    `H ${f(p)}`,
    `C ${f(c1)} ${f(h)} ${f(c2)} ${f(h)} ${f(x3)} ${f(h - y3)}`,
    `A ${f(rr)} ${f(rr)} 0 0 1 ${f(y3)} ${f(h - x3)}`,
    `C 0 ${f(h - c2)} 0 ${f(h - c1)} 0 ${f(h - p)}`,
    `V ${f(p)}`,
    `C 0 ${f(c1)} 0 ${f(c2)} ${f(y3)} ${f(x3)}`,
    `A ${f(rr)} ${f(rr)} 0 0 1 ${f(x3)} ${f(y3)}`,
    `C ${f(c2)} 0 ${f(c1)} 0 ${f(p)} 0`,
    "Z",
  ].join(" ");
}

export interface DialPreviewFrameProps {
  /** 设备画布尺寸（物理像素），用来算真实圆角与胶囊屏微放大 */
  screen: [number, number];
  /** 预览框显示宽高（px） */
  width: number;
  height: number;
  className?: string;
  /** 外层容器的额外样式（宽高与圆角已由组件计算，可在这里覆盖） */
  style?: CSSProperties;
  /** 内层裁剪容器的额外类名；默认深色媒体底，显式传 "" 可去掉 */
  innerClassName?: string;
  innerStyle?: CSSProperties;
  children?: ReactNode;
  /** 外层容器 ref：视频表盘制作页的裁剪拖拽需要拿它算坐标 */
  frameRef?: Ref<HTMLDivElement>;
  onPointerDown?: PointerEventHandler<HTMLDivElement>;
  onPointerMove?: PointerEventHandler<HTMLDivElement>;
  onPointerUp?: PointerEventHandler<HTMLDivElement>;
  onPointerCancel?: PointerEventHandler<HTMLDivElement>;
  onPointerLeave?: PointerEventHandler<HTMLDivElement>;
  onClick?: MouseEventHandler<HTMLDivElement>;
}

export default function DialPreviewFrame({
  screen,
  width,
  height,
  className,
  style,
  innerClassName = "bg-[var(--pf-media)]",
  innerStyle,
  children,
  frameRef,
  onPointerDown,
  onPointerMove,
  onPointerUp,
  onPointerCancel,
  onPointerLeave,
  onClick,
}: DialPreviewFrameProps) {
  // 同一页面可能同时有几十个预览框：渐变 ID 必须唯一，否则 SVG 会互相串色
  const uid = useId().replace(/[^a-zA-Z0-9_-]/g, "");
  const gradId = `pf-dial-sheen-${uid}`;
  const cornerR = previewCornerRadius(screen, width);
  const clipPathD = squirclePath(width, height, cornerR, 0.62);

  return (
    <div
      ref={frameRef}
      className={`pf-market-dial ${className ?? ""}`}
      style={{ width, height, borderRadius: cornerR, ...style }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
      onPointerCancel={onPointerCancel}
      onPointerLeave={onPointerLeave}
      onClick={onClick}
    >
      <div
        className={`relative w-full h-full overflow-hidden ${innerClassName}`}
        style={{
          borderRadius: cornerR,
          clipPath: `path("${clipPathD}")`,
          ...innerStyle,
        }}
      >
        {children}
        {/* 盖板玻璃：斜向反光 */}
        <span
          aria-hidden="true"
          className="pf-market-dial-sheen pointer-events-none absolute inset-0"
        />
        {/* 边缘高光 + 细边框都沿 Squircle 路径画，才能真正贴合屏面 */}
        <svg
          aria-hidden="true"
          className="pointer-events-none absolute inset-0 w-full h-full"
          viewBox={`0 0 ${width} ${height}`}
          preserveAspectRatio="none"
        >
          <defs>
            <linearGradient id={gradId} x1="0" y1="0" x2="0.2" y2="1">
              <stop offset="0%" stopColor="var(--pf-dial-sheen-top)" />
              <stop offset="45%" stopColor="var(--pf-dial-sheen-mid)" />
              <stop offset="100%" stopColor="var(--pf-dial-sheen-bot)" />
            </linearGradient>
          </defs>
          <path
            d={clipPathD}
            fill="none"
            stroke={`url(#${gradId})`}
            strokeWidth="2.2"
          />
          <path d={clipPathD} fill="none" stroke="var(--pf-dial-rim)" strokeWidth="1" />
        </svg>
      </div>
    </div>
  );
}
