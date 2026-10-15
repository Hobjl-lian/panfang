/**
 * Tauri 后端桥接层（唯一的数据来源开关）。
 *
 * - 在 App（Tauri / Android WebView）里存在 `window.__TAURI__`：`invoke` / `listen` 可用，
 *   上层调用真实命令。
 * - 在浏览器里预览（`vite build` 产物 + 静态 http 服务）没有 `window.__TAURI__`：
 *   `invoke` / `listen` 为 null，上层退回演示数据，界面与动效完全一致。
 *
 * 这里只放「不依赖 React 状态」的纯工具：命令通道、字节/Base64 转换、后端模型的轻量类型、
 * 以及裁剪变换与「照片表盘」判据（与 Rust 侧同一套规则，见 watchface_render.rs / watchface_edit.rs）。
 */

type InvokeFn = (cmd: string, args?: Record<string, unknown>) => Promise<any>;
type ListenFn = (
  event: string,
  handler: (ev: { payload: any }) => void,
) => Promise<() => void>;

interface TauriGlobal {
  core?: { invoke?: InvokeFn };
  event?: { listen?: ListenFn };
}

const g: TauriGlobal | undefined =
  typeof globalThis !== "undefined"
    ? (globalThis as { __TAURI__?: TauriGlobal }).__TAURI__
    : undefined;

/** 当前是否跑在 Tauri（App）里 —— 浏览器预览时为 false */
export const hasTauri: boolean = typeof g?.core?.invoke === "function";

/** Tauri 命令调用（浏览器里为 null，调用方据此退回演示数据） */
export const invoke: InvokeFn | null = hasTauri ? g!.core!.invoke! : null;

/** Tauri 事件监听（浏览器里为 null） */
export const listen: ListenFn | null =
  hasTauri && typeof g?.event?.listen === "function" ? g!.event!.listen! : null;

/** 系统栏图标明暗切换：light=true 深色图标（浅色页），false 白色图标（裁剪页等黑底页）。
 *  浏览器预览 / 桌面端为 no-op；失败只进 console（不弹 UI）。 */
export function setSystemBarsLight(light: boolean): void {
  invoke?.("system_bars_light", { light })?.catch((err: unknown) => {
    console.warn("system_bars_light failed:", err);
  });
}

/** 安装期间锁死返回键：true = 系统返回与侧滑手势全部吞掉（App 退不出去），
 *  装完或出错传 false 放开。浏览器预览 / 桌面端为 no-op；失败只进 console。 */
export function setInstallBackLock(locked: boolean): void {
  invoke?.("install_back_lock", { locked })?.catch((err: unknown) => {
    console.warn("install_back_lock failed:", err);
  });
}

/** 安装期间保持屏幕常亮：息屏后 App 会进入 cached 状态，系统（及各 ROM 省电策略）
 *  可能冻结进程，传输停在半路。与返回锁同一处调用、同一生命周期。
 *  浏览器预览 / 桌面端为 no-op；失败只进 console。 */
export function setKeepScreenOn(on: boolean): void {
  invoke?.("keep_screen_on", { on })?.catch((err: unknown) => {
    console.warn("keep_screen_on failed:", err);
  });
}

/** 打开系统 WebView 的应用详情页（门禁页「去更新」按钮用）。 */
export function openWebViewSettings(): void {
  invoke?.("open_webview_settings")?.catch((err: unknown) => {
    console.warn("open_webview_settings failed:", err);
  });
}

/** 安卓 WebView 的 Chromium 主版本号（界面能否正常显示的下限，见 App.tsx 门禁页）。
 *  取不到时返回 null（桌面 / 浏览器预览 / UA 里没有 Chrome 标记的内核）——一律放行。 */
export function androidWebViewMajor(): number | null {
  if (typeof navigator === "undefined") return null;
  const ua = navigator.userAgent;
  if (!/Android/i.test(ua)) return null;
  const m = /Chrome\/(\d+)/.exec(ua);
  if (!m) return null;
  const major = Number(m[1]);
  return Number.isFinite(major) ? major : null;
}

// ---------------------------------------------------------------- 字节 / Base64

/** 分块转 base64（避免大文件栈溢出，同 legacy 前端） */
export function bytesToB64(u8: Uint8Array): string {
  let binary = "";
  const chunk = 0x8000;
  for (let i = 0; i < u8.length; i += chunk) {
    binary += String.fromCharCode.apply(
      null,
      Array.from(u8.subarray(i, i + chunk)),
    );
  }
  return btoa(binary);
}

export function b64ToBytes(b64: string): Uint8Array {
  const bin = atob(b64);
  const u8 = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) u8[i] = bin.charCodeAt(i);
  return u8;
}

/** base64 图片字节 → data URL（可直接喂给 <img src>） */
export function b64ToDataUrl(b64: string, mime = "image/png"): string {
  return `data:${mime};base64,${b64}`;
}

/** data URL → 纯 base64（不是 data URL 时返回 null） */
export function dataUrlToB64(url: string): string | null {
  const i = url.indexOf(",");
  if (!url.startsWith("data:") || i < 0) return null;
  return url.slice(i + 1);
}

/** 按文件头判断图片 MIME（安卓上 file.type 可能为空或 octet-stream） */
export function sniffImageMime(u8: Uint8Array): string {
  if (u8.length > 3 && u8[0] === 0x89 && u8[1] === 0x50 && u8[2] === 0x4e)
    return "image/png";
  if (u8.length > 2 && u8[0] === 0xff && u8[1] === 0xd8) return "image/jpeg";
  if (u8.length > 11 && u8[8] === 0x57 && u8[9] === 0x45 && u8[10] === 0x42)
    return "image/webp";
  if (u8.length > 3 && u8[0] === 0x47 && u8[1] === 0x49 && u8[2] === 0x46)
    return "image/gif";
  if (u8.length > 1 && u8[0] === 0x42 && u8[1] === 0x4d) return "image/bmp";
  return "";
}

// ---------------------------------------------------------------- 后端模型（Rust 侧序化后的形状）

/** cloud.rs `CloudDevice` */
export interface CloudDevice {
  name?: string;
  model?: string;
  mac?: string;
  authkey?: string;
  fwVer?: string;
  identifier?: string;
  status?: number;
}

/** pb `WatchFaceItem`（`watchface_get_list` 的一项） */
export interface WfListItem {
  id: string;
  name?: string;
  isCurrent?: boolean;
  canRemove?: boolean;
  style?: string;
  styleColorIndex?: number;
  supportImageGroup?: boolean;
  slotItemList?: Array<{ slotId?: string; widgetId?: string }>;
}

/** bridge.rs `ArchiveEntry` / `ArchiveData` */
export interface ArchiveEntry {
  id: string;
  ext: string;
  size: number;
}
export interface ArchiveData extends ArchiveEntry {
  dataB64: string;
}

/** bridge.rs `SavedPhoto` */
export interface SavedPhoto {
  name: string;
  b64: string;
  crop?: PhotoCrop | null;
}

/** watchface_bin.rs `StyleSection`（判照片表盘只看这几项） */
export interface BinStyleSection {
  index: number;
  name: string;
  backImageId: number;
  previewOffset: number;
}

/** watchface_edit.rs `ScreenInfo`（屏幕宽高 + **外壳物理圆角** + 形状） */
export interface ScreenInfo {
  width: number;
  height: number;
  /** 外壳物理圆角（px）：336×480 族 R45、432×514 族 R104、跑道屏 ≥半宽（CSS 按邻边比例截成半圆）、圆表 = 宽/2 */
  radius: number;
  /** circle（圆表）| run（跑道屏）| square（方屏） */
  shape: string;
}

/** watchface_render.rs `LayerModelDoc` 里前端用到的部分 */
export interface LayerTheme {
  index: number;
  name: string;
  kind: string;
  /** 该样式实际引用的容器槽位（编辑页按样式过滤容器卡片） */
  slots: number[];
  /** 该样式图片/精灵记录里出现过的配色组（编辑页按样式过滤配色选项） */
  colorGroups: number[];
}
export interface LayerSlot {
  index: number;
  actions: number[];
}
export interface LayerAction {
  index: number;
  name: string;
}
export interface LayerColorGroup {
  index: number;
  name: string;
  swatchPngBase64?: string | null;
}
export interface LayerModelDoc {
  watchfaceId: string;
  name: string;
  width: number;
  height: number;
  themes: LayerTheme[];
  slots: LayerSlot[];
  actions: LayerAction[];
  colorGroups: LayerColorGroup[];
  /** 是否照片表盘（后端判据：亮屏样式 backImageId 高字节 0x82） */
  photoAlbum: boolean;
  /** 表盘自身画布：尺寸 + 圆角 + 形状 */
  screen: ScreenInfo;
  /** 当前**连接设备**的屏幕（型号未知为 null）——与 `screen` 不一致说明表盘不是为它做的 */
  deviceScreen?: ScreenInfo | null;
}

/** watchface_get_list 的返回可能是数组或 `{ list: [...] }` */
export function normalizeWfList(raw: unknown): WfListItem[] {
  if (Array.isArray(raw)) return raw as WfListItem[];
  const l = (raw as { list?: unknown } | null)?.list;
  return Array.isArray(l) ? (l as WfListItem[]) : [];
}

// ---------------------------------------------------------------- 表盘编辑：裁剪变换 / 照片表盘判据

/** 演示态（浏览器预览 / 未连接设备）默认屏幕：小米手环 10 族 212×520 跑道屏 */
export const DEMO_SCREEN: ScreenInfo = { width: 212, height: 520, radius: 104, shape: "run" };

export interface FrameSize {
  /** 显示宽 / 高（px） */
  w: number;
  h: number;
  /** 显示圆角（px）——超过短边一半时 CSS 自动截成半圆/正圆 */
  radius: number;
  /** 显示像素 / 设备像素 */
  k: number;
  screen: ScreenInfo;
}

/**
 * 屏幕 → 显示框：**等比缩放 + 圆角同比例**（legacy `frameSize()` 同一套口径，
 * 见 docs/设计稿v2-对照与UI实施要点.md §5.2/§5.3）。
 *
 *   k      = min(maxW / W, maxH / H)
 *   w, h   = round(W × k), round(H × k)
 *   radius = round(min(R × k, min(w, h) / 2))   ← 跑道屏/圆表靠这一条截成半圆/正圆
 *
 * 验收值（同文档 §8）：212×520→106×260 R52｜336×480→182×260 R24｜432×514→200×238 R48｜
 * 464×464→200×200 R100（正圆）｜192×490→102×260 R51。
 */
export function frameSizeFor(screen: ScreenInfo, maxW = 200, maxH = 260): FrameSize {
  const k = Math.min(maxW / screen.width, maxH / screen.height);
  const w = Math.max(1, Math.round(screen.width * k));
  const h = Math.max(1, Math.round(screen.height * k));
  const radius = Math.round(Math.min(screen.radius * k, Math.min(w, h) / 2));
  return { w, h, radius, k, screen };
}

/** 后端 `PhotoTransform`（背景层像素） */
export interface PhotoCrop {
  scale: number;
  dx: number;
  dy: number;
  rotation: number;
}

/**
 * 裁剪视图上的拖拽/缩放/旋转 → 后端需要的变换（画布像素）。
 *
 * 换算**必须用表盘画布宽度**（后端 `draw_photo` 是在 `model.width×height` 空间里做变换的，
 * 视图与画布规格不一致时只有按画布宽度换算才对得上）——与 legacy `ed-crop-apply` 一致。
 */
export function cropToTransform(
  view: { x: number; y: number; scale: number; rotation: number },
  canvasW: number,
  viewW: number,
): PhotoCrop {
  const k = viewW > 0 ? canvasW / viewW : 1;
  return {
    scale: view.scale,
    dx: Math.round(view.x * k),
    dy: Math.round(view.y * k),
    rotation: view.rotation,
  };
}

/**
 * 照片表盘判据（与 Rust `derive_model` 完全一致）：
 * 任意**亮屏**样式（previewOffset ≠ 0）的 backImageId 高字节 == 0x82。
 */
export function isPhotoDialBinSummary(
  summary: { styles?: BinStyleSection[] } | null | undefined,
): boolean {
  for (const s of summary?.styles ?? []) {
    if (s.previewOffset === 0) continue; // 息屏样式
    if (s.backImageId >>> 24 === 0x82) return true;
  }
  return false;
}

// ---------------------------------------------------------------- App 自更新（安卓）

/** 检查更新的结果（后端 `app_update_check`）。`no_release` = 服务端还没发布过任何版本。 */
export interface AppUpdateCheck {
  current: string;
  current_code: number;
  can_install: boolean;
  available: boolean;
  mandatory: boolean;
  no_release?: boolean;
  version_name?: string;
  version_code?: number;
  notes?: string;
  size?: number;
  sha256?: string;
}

/** 问服务端有没有新版本（不下载）。浏览器预览 / 桌面不可用时抛错。 */
export async function appUpdateCheck(): Promise<AppUpdateCheck> {
  if (!invoke) throw new Error("检查更新仅支持安卓版");
  return (await invoke("app_update_check")) as AppUpdateCheck;
}

/** 下载新版 APK（进度走 `wf-app-update-progress` 事件），返回可交给安装器的本地路径。 */
export async function appUpdateDownload(): Promise<{
  path: string;
  version_name?: string;
  size?: number;
}> {
  if (!invoke) throw new Error("更新仅支持安卓版");
  return (await invoke("app_update_download")) as {
    path: string;
    version_name?: string;
    size?: number;
  };
}

/** 调起系统安装器。未授权「安装未知来源应用」时后端会打开设置页并回 `needs_permission`。 */
export async function appUpdateInstall(
  path: string,
): Promise<{ needs_permission?: boolean; launched?: boolean }> {
  if (!invoke) throw new Error("更新仅支持安卓版");
  return (await invoke("app_update_install", { path })) as {
    needs_permission?: boolean;
    launched?: boolean;
  };
}
