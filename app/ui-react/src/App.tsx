import React, { useState, useRef, useEffect } from "react";
import developerAvatar from "./assets/developer-avatar.png";
import appIcon from "./assets/app-icon.png";
import afdianIcon from "./assets/afdian-icon.svg";
import wp01 from "./assets/wallpapers/wp-01.svg";
import wp02 from "./assets/wallpapers/wp-02.svg";
import wp03 from "./assets/wallpapers/wp-03.svg";
import wp04 from "./assets/wallpapers/wp-04.svg";
import wp05 from "./assets/wallpapers/wp-05.svg";
import wp06 from "./assets/wallpapers/wp-06.svg";
import wp07 from "./assets/wallpapers/wp-07.svg";
import wp08 from "./assets/wallpapers/wp-08.svg";
import wp09 from "./assets/wallpapers/wp-09.svg";
import wp10 from "./assets/wallpapers/wp-10.svg";
import wp11 from "./assets/wallpapers/wp-11.svg";
import wp12 from "./assets/wallpapers/wp-12.svg";
import {
  invoke,
  listen,
  bytesToB64,
  b64ToBytes,
  b64ToDataUrl,
  dataUrlToB64,
  sniffImageMime,
  cropToTransform,
  isPhotoDialBinSummary,
  normalizeWfList,
  frameSizeFor,
  DEMO_SCREEN,
  type CloudDevice,
  type WfListItem,
  type ArchiveEntry,
  type ArchiveData,
  type SavedPhoto,
  type BinStyleSection,
  type LayerModelDoc,
  type ScreenInfo,
  type PhotoCrop,
  setSystemBarsLight,
  setInstallBackLock,
  setKeepScreenOn,
  openWebViewSettings,
  androidWebViewMajor,
  appUpdateCheck,
  appUpdateDownload,
  appUpdateInstall,
  type AppUpdateCheck
} from "./tauri";
import {
  Lightning, CaretRight, FolderOpen, Check, House, SlidersHorizontal, Watch,
  CircleNotch, CaretLeft, X, SignOut, ArrowsClockwise, Flame, Heart,
  Footprints, CaretDown, Plus, GearSix, ArrowsDownUp, ArrowSquareOut,
  Info, ShieldCheck, FileText, Scroll, Code, DotsThree, ArrowClockwise,
  ArrowCounterClockwise, MagnifyingGlassPlus, IconContext
} from "@phosphor-icons/react";

/** 隐私政策 / 用户协议所在站点（已备案域名上的静态页；App 内、登录页、购买页都指向这里） */
const LEGAL_BASE = "https://api.hobjl-shop.top";

type FlowStep = "login" | "device_select" | "main" | "paid_bind" | "paid_dials" | "paid_account" | "redeem_installing" | "redeem_success" | "connect_help" | "dial_edit" | "about_app" | "about_developer" | "open_source_licenses" | "crop_image";

/** 付费资源（服务端 /v1/me 的返回，只含有包体的设备行） */
interface PaidVariant {
  variant_id: string;
  name: string;
  device_models: string[];
  screen: [number | null, number | null];
  latest_version: string | null;
  /** 最新版本的**内容指纹**（服务端算的）；本机装过的话用它判有没有新版本 */
  latest_sha?: string | null;
  /** 下面三个由 App 侧（paid_list 命令）按本机安装记录补上 */
  installed?: boolean;
  installed_version?: string | null;
  has_update?: boolean;
}
interface PaidProduct {
  product_id: string;
  name: string;
  variants: PaidVariant[];
}

/** 一个版本里的包体（`paid_latest_file` 的 files 项；勾选安装只用到 file_id） */
interface PaidFile {
  file_id: string;
  size?: number;
}

/** 本机安装记录（`paid_installs_get`）：某个设备行上次装的是哪个版本 + 装出来的表盘 id */
interface PaidInstallRecord {
  product_id: string;
  variant_id: string;
  version: string;
  sha: string;
  dials: string[];
  at: number;
}

interface DeviceItem {
  id: string;
  name: string;
  model: string;
  /** 电量：未连接设备时拿不到真值 → null（界面显示 --%） */
  battery: number | null;
}

interface InstalledDial {
  id: string;
  name: string;
  isCurrent: boolean;
  isPhotoDial?: boolean;
}

const MOCK_DEVICES: DeviceItem[] = [
  { id: "mb10", name: "Xiaomi 手环 10", model: "主力设备", battery: 78 },
  { id: "mb9p", name: "Xiaomi 手环 9 Pro", model: "备用", battery: 45 },
  { id: "rw5", name: "Redmi Watch 5", model: "家庭", battery: 92 }
];

const LARGE_STYLES = [
  "亮屏-玻璃拟物", "亮屏-极简线条", "亮屏-像素怀旧", "亮屏-数字排版",
  "亮屏-赛博霓虹", "亮屏-钛金质感", "息屏常显-极暗", "息屏常显-空心字",
  "息屏常显-单色红", "息屏常显-单色绿", "节电夜览-微光"
];

// 选项文案统一为「配色 1 / 配色 2 ...」，不使用色名与色板色块
const LARGE_COLORS = [
  { name: "配色 1", hex: "#4361EE" },
  { name: "配色 2", hex: "#22252A" },
  { name: "配色 3", hex: "#D4973B" },
  { name: "配色 4", hex: "#4D8B6F" },
  { name: "配色 5", hex: "#9EABB8" },
  { name: "配色 6", hex: "#E85D04" },
  { name: "配色 7", hex: "#7209B7" },
  { name: "配色 8", hex: "#E63946" },
  { name: "配色 9", hex: "#2D6A4F" },
  { name: "配色 10", hex: "#DDBEA9" },
  { name: "配色 11", hex: "#1D3557" },
  { name: "配色 12", hex: "#70C1B3" }
];

const LARGE_CONTAINERS_TOP = [
  "玻璃-细", "玻璃-粗", "普通-中", "无边框",
  "胶囊微光", "金属磨砂", "线条镂空", "双层重叠",
  "圆角矩阵", "极简单行", "色块底板"
];

const LARGE_CONTAINERS_BOTTOM = [
  "玻璃-细", "玻璃-粗", "普通-中", "无边框",
  "三段切分", "大圆角卡", "双环仪表", "独立卡片",
  "心率优先", "能量环形", "步数进度"
];

// 多达 10+ 张正方形高精壁纸库预设（离线：本地渐变占位素材，不联网）
const INITIAL_WALLPAPERS = [
  wp01, wp02, wp03, wp04, wp05, wp06,
  wp07, wp08, wp09, wp10, wp11, wp12
];

/**
 * 编辑页的一张「容器 N · 槽位名」卡片。
 * 卡片数量**跟着模型里的可编辑槽位走**（有几个槽位就几张，不写死两个、不限两个）。
 */
interface ContainerCard {
  /** 模型里的槽位序号（演示态没有真实槽位 → null，只做界面预览） */
  slot: number | null;
  /** 槽位名（顶部槽位 / 底部数据槽 / 槽位 N） */
  label: string;
  /** 候选文案（界面显示这一列；重名时会带「(2)」这种后缀） */
  options: string[];
  /** 候选对应的真实动作序号（与 options 一一对应，下发用；演示态为空） */
  ids: number[];
  /**
   * 当前选中的候选**动作序号**——容器的身份只认它，不认名字：
   * .bin 里两个不同动作可以同名（内容不同），按名字会「一起选中」且选中的那个下发不出去。
   */
  selectedId: number | null;
  /** 当前选中的文案（只用于显示） */
  selected: string;
  /**
   * 用户是否**点过**这张卡片。下发判据是它，而不是「选中的是不是默认项」——
   * 列表第一项就是该槽位的默认项，用「等于默认项＝没改动」会让**第一项永远发不出去**
   * （2026-10-07 用户报的「第一个怎么点都应用不上」就是这么来的）。
   */
  touched: boolean;
}

/**
 * 候选名字去重：同一个槽位里出现重复名字时，给第 2、3… 个加「(2)」「(3)」后缀。
 * 为什么需要：文件里两个候选同名时界面完全分不清谁是谁（身份仍走动作序号，这里只管看得见）。
 */
function dedupeNames(names: string[]): string[] {
  const used = new Set<string>();
  return names.map((n) => {
    let label = n;
    let k = 1;
    while (used.has(label)) {
      k += 1;
      label = `${n} (${k})`;
    }
    used.add(label);
    return label;
  });
}

/**
 * 槽位名：模型里的槽位（SlotModel）只有序号没有名字 → 前两个沿用设计稿的
 * 「顶部槽位 / 底部数据槽」，其余按序号给通用名（槽位 3、槽位 4 …）。
 */
function slotLabelFor(order: number): string {
  if (order === 0) return "顶部槽位";
  if (order === 1) return "底部数据槽";
  return `槽位 ${order + 1}`;
}

/** 演示态（未连接设备 / 浏览器预览）的两张容器卡片：与设计稿的默认选择完全一致 */
const DEMO_CONTAINERS: ContainerCard[] = [
  { slot: null, label: "顶部槽位", options: LARGE_CONTAINERS_TOP, ids: [], selectedId: null, selected: LARGE_CONTAINERS_TOP[0], touched: false },
  { slot: null, label: "底部数据槽", options: LARGE_CONTAINERS_BOTTOM, ids: [], selectedId: null, selected: LARGE_CONTAINERS_BOTTOM[1], touched: false }
];

/** 容器卡片的展开态：一个卡片一个（`container-<下标>`） */
type AccordionSection = "style" | "color" | `container-${number}` | null;

/** 读取本地持久化开关（无 localStorage / 无值时退回默认值） */
function readStoredFlag(key: string, fallback: boolean): boolean {
  try {
    const v = localStorage.getItem(key);
    return v === null ? fallback : v === "1";
  } catch {
    return fallback;
  }
}

/** 写入本地持久化开关 */
function storeFlag(key: string, value: boolean) {
  try {
    localStorage.setItem(key, value ? "1" : "0");
  } catch {
    /* 忽略：无 localStorage 时只在内存生效 */
  }
}

/** 读取本地持久化的字符串列表（无 localStorage / 坏数据一律当空） */
function readStoredList(key: string): string[] {
  try {
    const raw = localStorage.getItem(key);
    const arr = raw ? (JSON.parse(raw) as unknown) : null;
    return Array.isArray(arr) ? arr.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}

/** 写入本地持久化的字符串列表（失败静默：无 localStorage 时只在内存生效） */
function storeList(key: string, list: string[]) {
  try {
    localStorage.setItem(key, JSON.stringify(list));
  } catch {
    /* 忽略 */
  }
}

// ================= 页面栈 / 页面恢复（逐级返回 + 退出 App 后回到上次页面） =================

/** 主界面 Tab（与 activeTab 同口径） */
type TabKey = "home" | "manage" | "settings";

/** 界面态持久化的键（登录凭证由 Rust 侧本地文件持久化，这里只存界面） */
const UI_STORE_KEY = "panfang.ui";

/**
 * 已从设备列表移除的设备 id（mac）。
 * 小米账号里的设备删不掉也不该删 —— 这里只记住「列表里别再出现它」；
 * 「重新导入设备列表」会清空这个名单，把账号下的设备重新拉全（用户 2026-10-08 要求）。
 */
const HIDDEN_DEVICES_KEY = "panfang.hiddenDevices";

/**
 * 页面路由口径（唯一一套字符串）：login / devices / main/<tab> / redeem / install / dial-edit /
 * crop / about / about/developer / about/licenses。兑换流程三个阶段算同一页（"redeem"）、
 * 本地安装进度的全过程算同一页（"install"）：返回一次即回到进来之前，进度页/成功页不再单独入栈。
 */

/** 路由字符串 → 页面状态（恢复与返回时用） */
function parseRoute(route: string): { step: FlowStep; tab: TabKey } {
  if (route === "login") return { step: "login", tab: "home" };
  if (route === "devices") return { step: "device_select", tab: "home" };
  if (route === "paid") return { step: "paid_bind", tab: "home" };
  if (route === "paid/dials") return { step: "paid_dials", tab: "home" };
  // 爱发电账号管理：二级页面，从设置进来（账号的日常管理只在这里，不在绑定流程里）
  if (route === "paid/account") return { step: "paid_account", tab: "settings" };
  // 连接失败帮助页：三级页面（从设备抽屉进来），返回按层级回抽屉
  if (route === "connect-help") return { step: "connect_help", tab: "home" };
  // 本地 .bin 安装：进度/成功页与兑换流程共用同一套页面（"install" 不参与持久化恢复，
  // 见 KNOWN_ROUTES：安装是瞬时过程，重启后回到进来之前那一页才是对的）
  if (route === "install") return { step: "redeem_installing", tab: "home" };
  if (route === "dial-edit") return { step: "dial_edit", tab: "manage" };
  if (route === "crop") return { step: "crop_image", tab: "manage" };
  if (route === "about") return { step: "about_app", tab: "settings" };
  if (route === "about/developer") return { step: "about_developer", tab: "settings" };
  if (route === "about/licenses") return { step: "open_source_licenses", tab: "settings" };
  if (route.startsWith("main/")) {
    const t = route.slice(5);
    return { step: "main", tab: t === "manage" || t === "settings" ? t : "home" };
  }
  return { step: "main", tab: "home" };
}

/** 已知路由集合（持久化恢复时的白名单） */
const KNOWN_ROUTES = [
  "login",
  "devices",
  "paid",
  "paid/dials",
  "paid/account",
  "connect-help",
  "dial-edit",
  "crop",
  "about",
  "about/developer",
  "about/licenses",
  "main/home",
  "main/manage",
  "main/settings"
];

/**
 * 退出 App 时持久化的界面态 —— **只允许页面级状态**，列表就是全部白名单：
 *   页面栈 / 账号资料 / 当前设备 / 表盘列表 / 编辑中的表盘。
 * **绝不写入任何瞬时状态**：连接中（connectingId / connectToken / connectSettled）、
 * 加载中（isApplying / isLoggingIn / installProgress）、浮层（抽屉 / 菜单 / 各种确认框）、
 * 错误文案、deviceBusy / 心跳计数 —— 它们跨进程恢复只会把界面「卡」在上一次的状态里。
 * （`writePersistedUi` 与 `readPersistedUi` 都只认白名单字段，旧版本多写进去的键会在首次写入时被清掉。）
 */
interface PersistedUi {
  /** 页面栈（栈底 = 根页面：登录页或一级界面） */
  stack: string[];
  profile: { nickname: string; phoneTail: string; avatarInitial: string } | null;
  device: DeviceItem | null;
  dials: InstalledDial[];
  editingDial: InstalledDial | null;
}

/** 读取上次退出时的界面态（无 localStorage / 坏数据一律当没有） */
function readPersistedUi(): PersistedUi | null {
  try {
    const raw = localStorage.getItem(UI_STORE_KEY);
    if (!raw) return null;
    const p = JSON.parse(raw) as Partial<PersistedUi> | null;
    if (!p || typeof p !== "object") return null;
    return {
      stack: Array.isArray(p.stack) ? (p.stack as string[]).filter(r => typeof r === "string") : [],
      profile: p.profile && typeof p.profile === "object" ? p.profile : null,
      device: p.device && typeof p.device === "object" ? p.device : null,
      dials: Array.isArray(p.dials) ? p.dials : [],
      editingDial: p.editingDial && typeof p.editingDial === "object" ? p.editingDial : null
    };
  } catch {
    return null;
  }
}

/** 写入界面态（失败静默：无 localStorage 时只在本次会话内生效） */
function writePersistedUi(v: PersistedUi) {
  try {
    localStorage.setItem(UI_STORE_KEY, JSON.stringify(v));
  } catch {
    /* 忽略 */
  }
}

/**
 * 恢复出来的页面栈要体检一遍：
 * - 一级界面（main/*）只保留最后一次的那条，并放到栈底（第三轮 4c：一级界面之间切换不产生返回记录）；
 * - 栈底必须是根页面（登录页 / 一级界面）——只有根页面才允许退出 App；
 * - 裁剪页的现场（待裁剪照片）在内存里，重启恢复不了 → 退到编辑页；
 * - 编辑页要有「编辑中的表盘」对象，没有就退到管理页。
 */
function sanitizeBootStack(p: PersistedUi | null): string[] {
  const raw = (p?.stack || []).filter(r => KNOWN_ROUTES.includes(r));
  const lastMain = [...raw].reverse().find(r => r.startsWith("main/"));
  const rest = raw.filter(r => !r.startsWith("main/"));
  const stack = (lastMain ? [lastMain, ...rest] : rest).slice(-20);
  if (!stack.length) return ["login"];
  // 只有「从来没登录过」才回登录页：账号资料（profile）就是「已登录」这个事实的持久化
  // （第三轮 2）——登录页/选择设备页之外的路由都需要已登录。
  const loggedIn = !!p?.profile?.nickname;
  if (!loggedIn && stack.some(r => r !== "login" && r !== "devices")) return ["login"];
  if (stack[0] !== "login" && !stack[0].startsWith("main/")) return ["login"];
  // 编辑页/裁剪页的现场（留档字节、图层模型、待裁照片）都在内存里，重启恢复不了：
  // 一律退回去 —— 否则编辑页会拿设计稿的演示数据充数（真机上出现过「假表盘」就是这个）。
  while (stack.length > 1 && (stack[stack.length - 1] === "crop" || stack[stack.length - 1] === "dial-edit")) {
    stack.pop();
  }
  return stack;
}

/**
 * 登录账号 → 界面上的账号资料（掩码形态，不编造：拿不到就留空）。
 * 手机号 → `138****8888`；邮箱 → `abc***@xx.com`（@ 前保留首 3 位 + `***`）；
 * 头像字母 = 账号首字符。`phoneTail` 只给「小米账号 · 尾号 xxxx」这一句用。
 */
function profileFromAccount(acc: string): { nickname: string; phoneTail: string; avatarInitial: string } {
  const t = acc.trim();
  if (!t) return { nickname: "", phoneTail: "", avatarInitial: "" };
  if (t.includes("@")) {
    const [local, domain = ""] = t.split("@");
    return {
      nickname: `${local.slice(0, 3)}***@${domain}`,
      phoneTail: "",
      avatarInitial: (local[0] || "").toUpperCase()
    };
  }
  const digits = t.replace(/\D/g, "");
  if (digits.length >= 8) {
    return {
      nickname: `${digits.slice(0, 3)}****${digits.slice(-4)}`,
      phoneTail: digits.slice(-4),
      avatarInitial: digits[0]
    };
  }
  return { nickname: t, phoneTail: "", avatarInitial: t[0].toUpperCase() };
}

/** 电量显示：拿不到真值时显示 --%（设计稿的 `{N}` 位） */
function fmtBattery(battery: number | null | undefined): string {
  return battery === null || battery === undefined ? "--" : String(battery);
}

/** 界面能正常显示所需的最低 WebView 主版本（Tailwind v4 产物的 color-mix / dvh 门槛） */
const WEBVIEW_MIN_MAJOR = 111;

/**
 * 系统 WebView 过旧时的说明页（安卓专有；桌面与浏览器预览走不到这里）。
 * 为什么要有：低版本 WebView 上界面是「半透明面整片消失 + 滚动区失效」，用户只会以为
 * App 坏了。这里给一句人话 + 一个「去更新」的直达入口（原生打开系统 WebView 的应用详情页），
 * 同时允许强行继续（有些内核版本号低但显示没问题）。
 */
function WebViewGateScreen({ major, onContinue }: { major: number; onContinue: () => void }) {
  return (
    <IconContext.Provider value={{ weight: "bold" }}>
      <div className="min-h-screen bg-[#0C0D0E] flex items-center justify-center p-0 md:p-6 select-none font-sans">
        <div className="w-full max-w-[400px] h-[100dvh] md:h-[844px] bg-[#F5F6F9] md:rounded-[44px] relative flex flex-col overflow-hidden">
          <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-12">
            <div />
            <div className="flex flex-col items-center text-center">
              <div className="w-14 h-14 rounded-2xl bg-black/5 flex items-center justify-center">
                <ArrowsClockwise size={26} className="text-black/40" />
              </div>
              <h2 className="text-[18px] font-bold text-black mt-4">系统组件版本过旧</h2>
              <p className="text-[14px] font-medium text-black/45 leading-relaxed mt-2 max-w-[280px]">
                这台手机的系统 WebView 版本较低（Chrome {major}），盘坊的界面可能显示不全。
                更新后重新打开盘坊即可。
              </p>
              <p className="text-[12px] font-medium text-black/30 leading-relaxed mt-3 max-w-[280px]">
                在应用商店里搜索「Android System WebView」更新，或点下面的按钮直达它的应用信息页。
              </p>
            </div>
            <div className="flex flex-col gap-3">
              <button
                type="button"
                onClick={() => openWebViewSettings()}
                className="w-full h-[44px] rounded-full bg-[#4361EE] text-white font-bold text-[15px] active:scale-[0.98] transition-all cursor-pointer"
              >
                去更新系统 WebView
              </button>
              <button
                type="button"
                onClick={onContinue}
                className="w-full h-[44px] rounded-full bg-black/5 text-black/60 font-bold text-[15px] active:scale-[0.98] transition-all cursor-pointer"
              >
                仍然继续（界面可能异常）
              </button>
            </div>
          </div>
        </div>
      </div>
    </IconContext.Provider>
  );
}

export default function App() {
  // 启动时读一次上次退出时的界面态：页面栈 + 账号资料 / 设备 / 表盘列表 / 编辑中的表盘
  const [boot] = useState(() => {
    const saved = readPersistedUi();
    const stack = sanitizeBootStack(saved);
    const top = parseRoute(stack[stack.length - 1]);
    return {
      saved,
      stack,
      step: top.step,
      tab: top.tab,
      profile: saved?.profile ?? null,
      device: saved?.device ?? null,
      dials: saved?.dials ?? [],
      editingDial: saved?.editingDial ?? null
    };
  });

  // 当前页面：从恢复出来的页面栈顶开始（恢复不了就是登录页）
  const [currentStep, setCurrentStep] = useState<FlowStep>(boot.step);

  // 状态栏/导航栏图标颜色随页面底色走：浅色页深色图标；裁剪页黑底用白色图标。
  // 其余页全是浅底（登录/兑换/关于/编辑页），只有 crop_image 是深色。
  useEffect(() => {
    setSystemBarsLight(currentStep !== "crop_image");
  }, [currentStep]);

  /**
   * 系统 WebView 版本门禁（安卓）：界面用了 Tailwind v4 产物的 color-mix() 与 dvh，
   * 需要 Chromium 111+ 才正常显示。低版本不是「丑一点」——半透明面会整片变透明、
   * 根容器拿不到高度（100dvh 整条被丢弃）导致滚动区失效，看起来就是"坏了"。
   * 取不到版本号一律放行（桌面端 / 浏览器预览 / UA 里没有 Chrome 标记的内核）；
   * 更低的一档（< 88，前端 JS 可能根本解析不了）无法在 JS 里提示，由 MainActivity 弹原生对话框。
   */
  const [webViewTooOld, setWebViewTooOld] = useState<number | null>(null);
  useEffect(() => {
    // 预览门禁页用：dev server 下加 ?pf_gate=1 强制显示（生产包不含这段）
    if (import.meta.env.DEV && new URLSearchParams(window.location.search).has("pf_gate")) {
      setWebViewTooOld(105);
      return;
    }
    const major = androidWebViewMajor();
    if (major !== null && major < WEBVIEW_MIN_MAJOR) setWebViewTooOld(major);
  }, []);

  // 账号资料：登录时按输入的小米账号推导（手机号取尾号 4 位 / 邮箱取 @ 前一段）
  const [userProfile, setUserProfile] = useState(() => boot.profile ?? profileFromAccount(""));

  // 登录输入一律留空：只显示 placeholder 提示（不预填演示值，真机与浏览器演示同样留空）
  const [account, setAccount] = useState("");
  const [password, setPassword] = useState("");
  const [isLoggingIn, setIsLoggingIn] = useState(false);

  // 设备连接态
  const [connectingId, setConnectingId] = useState<string | null>(null);
  const [connectedId, setConnectedId] = useState<string | null>(null);
  const [selectedDevice, setSelectedDevice] = useState<DeviceItem>(boot.device ?? MOCK_DEVICES[0]);
  const [isDeviceDrawerOpen, setIsDeviceDrawerOpen] = useState(false);
  const [isDeviceSwitchingAnim, setIsDeviceSwitchingAnim] = useState(false);
  /**
   * 设备**真实**连接状态（第三轮 3）：杀后台/走远后连接其实断了，界面不能继续显示「已连接」。
   * 启动一律从 false 开始（不继承上次会话），之后由心跳 + 回到前台时的探测维持。
   */
  const [isDeviceConnected, setIsDeviceConnected] = useState(false);
  /** 心跳：连续失败次数（连续两次才判死，避免单次抖动误报掉线） */
  const heartbeatFailures = useRef(0);
  /** 心跳：同一时间只允许一个探测在飞 */
  const heartbeatInFlight = useRef(false);
  /** 正在安装 / 正在应用配置期间不探连接状态（把「忙」当成掉线会误报红点） */
  const deviceBusy = useRef(false);
  /**
   * 心跳兜底间隔与单次探测超时。
   * 主心跳已经搬到 Rust 连接总管（15s 一次，后台也跑，掉线会主动推 wf-link-* 事件）；
   * 前端的 60 秒兜底只防「总管万一没跑起来」，以及给界面留一条自己也能探活的路。
   */
  const HEARTBEAT_MS = 60000;
  const STATUS_PROBE_TIMEOUT_MS = 8000;
  /** 掉线后正在自动重连（Rust 接管）：设备行显示「重连中…」而不是「未连接设备」 */
  const [linkRetrying, setLinkRetrying] = useState(false);
  /** 连接状态的镜像（事件回调里读它，不用可能陈旧的 state —— 铁律③：用真实状态判断） */
  const connectedStateRef = useRef(false);
  /** 掉线信号：安装/应用期间掉线 → 立刻失败，不等停滞看门狗（25 秒） */
  const linkLostRejects = useRef<Array<(err: Error) => void>>([]);
  /** 「后台保持连接」引导只提示一次 */
  const LINK_HELP_FLAG = "panfang.linkHelpShown";

  // 连接竞态控制：每次点设备生成递增令牌，只有最新一次尝试能改界面（旧请求的成功/失败/超时一律忽略）
  const connectToken = useRef(0);
  /** 最新一次尝试是否已结算（成功 / 失败 / 超时已发生） */
  const connectSettled = useRef(true);
  /** 单次连接的超时定时器（换目标或结算时必须清掉，绝不留转圈） */
  const connectTimer = useRef<number | null>(null);
  /** 单次连接超时上限（毫秒）：到点即作废这次尝试，屏幕只留一句短提示 */
  /** 转圈与「给引导」的上限：到点先告诉用户怎么做（手环慢的话后台还会继续试） */
  const CONNECT_TIMEOUT_MS = 15000;
  /** 宽限上限：超过它才真正判失败（15~45s 之间连上了照样算成功） */
  const CONNECT_GRACE_MS = 45000;
  /**
   * 连接/扫描失败时**唯一的屏幕提示**（技术细节只进 console，长文与 MAC 不上屏）。
   * 手环不在「连接新手机」模式时后端各种失败原因都会归结到这一句可操作的话上。
   */
  const CONNECT_HINT = "请在手环设置-系统管理中点击连接新手机，进入该模式后，才可扫描到手环";

  /**
   * 失败打点：把「哪一步失败了、后端原话是什么」送给开发者后台（管理页「客户端诊断」）。
   * 屏幕上的文案完全不变（仍是 humanize 那句），这里只是让卖家第一次能看见失败——
   * 以前这类信号只进这台手机的 console，卖家那边什么都收不到。
   * 只发排障字段（步骤/错误码/原始错误/机型），不含账号、不含表盘内容；发不出去就算了。
   */
  const diagReport = (step: string, code: string, message: string, model?: string | null) => {
    try {
      void invoke?.("diag_report", {
        step,
        code,
        message: String(message).slice(0, 400),
        deviceModel: model ?? connected.current.model ?? null,
      })?.catch(() => {});
    } catch {
      /* 上报绝不能影响主流程 */
    }
  };

  /**
   * 装到设备这类长操作的**停滞看门狗**：只要还有进度事件就不算超时；
   * 超过 INSTALL_STALL_MS 一点动静都没有，就判这次操作失败。
   *
   * 为什么必须有：手环被别的 App（如「小米运动健康」）占用时，传输会挂在半路——
   * **既不报错也不返回**。实测表现就是卡在进度页出不来，而且因为安装期间不探连接状态，
   * 连「已掉线」都不会告诉你（2026-10-08 真机踩到）。
   */
  const INSTALL_STALL_MS = 25000;
  /** 最近一次收到安装/传输进度事件的时刻（看门狗据此判断「还在动」） */
  const installBeat = useRef(0);
  const withStallGuard = <T,>(p: Promise<T>): Promise<T> =>
    new Promise<T>((resolve, reject) => {
      const startedAt = Date.now();
      const timer = window.setInterval(() => {
        const beat = Math.max(installBeat.current, startedAt);
        if (Date.now() - beat > INSTALL_STALL_MS) {
          window.clearInterval(timer);
          reject(new Error("设备长时间无响应（安装停滞）"));
        }
      }, 1000);
      p.then(
        v => {
          window.clearInterval(timer);
          resolve(v);
        },
        e => {
          window.clearInterval(timer);
          reject(e);
        }
      );
    });

  /**
   * 错误上屏一律只给「一句人话」——后端原始错误（含 Rust 文件路径 / MAC / 内部术语）只进 console。
   * （用户定的铁律：报错只给一句操作指引，技术细节只进 console。细节同时进开发者后台。）
   * 第二参数 `step` 只用于失败打点，不影响屏幕文案。
   */
  const humanize = (err: unknown, step?: string): string => {
    const raw = String(err);
    console.error("[err]", raw);
    let code = "other";
    let msg = "操作失败，请重试";
    if (raw.includes("NOT_BOUND")) {
      code = "not_bound";
      msg = "请先绑定爱发电";
    } else if (raw.includes("device_limit")) {
      code = "device_limit";
      msg = "已达到设备数量上限，请联系开发者解绑";
    } else if (raw.includes("ExceedQuantityLimit")) {
      // 手环侧表盘数量上限（prepare 状态码 7）：重试没用，得先腾位置
      code = "dial_limit";
      msg = "手环里表盘已装满，请先卸载几个表盘，再回来安装";
    } else if (raw.includes("LowStorage")) {
      code = "low_storage";
      msg = "手环存储空间不足，请先删掉一些表盘或文件再安装";
    } else if (raw.includes("LowBattery")) {
      code = "low_battery";
      msg = "手环电量过低，请先充电再安装";
    } else if (raw.includes("Downgrade")) {
      code = "downgrade";
      msg = "手环拒绝了旧版本安装，请先在管理页卸载旧表盘再装";
    } else if (raw.includes("Duplicated")) {
      code = "duplicated";
      msg = "手环上已有同名表盘，请先卸载再装";
    } else if (raw.includes("OpNotSupport")) {
      code = "not_support";
      msg = "手环不支持安装这份文件";
    } else if (raw.includes("prepare failed with status: Busy")) {
      code = "busy";
      msg = "手环正忙，请稍等片刻再试";
    } else if (raw.includes("登录已过期") || raw.includes("token")) {
      code = "token";
      msg = "登录已过期，请重新绑定";
    } else if (raw.includes("停滞")) {
      code = "stalled";
      msg = "手环没有响应，请让手环进入「连接新手机」后重新连接，再试一次";
    } else if (raw.includes("连接已断开")) {
      // 掉线自动重连（连接总管）期间的操作失败：正在重连，稍后重试即可
      code = "link_lost";
      msg = "手环连接已断开，正在自动重连，请稍后重试";
    } else if (raw.includes("Disconnected") || raw.includes("未连接") || raw.includes("timeout")) {
      code = "disconnected";
      msg = "手环连接已断开，请重连后再试";
    } else if (raw.includes("安装包校验失败")) {
      code = "update_checksum";
      msg = "安装包下载不完整，请重试";
    } else if (raw.includes("连不上更新服务器")) {
      // 检查更新阶段的网络失败：不能说「下载失败」（用户点的是检查）
      code = "update_check_net";
      msg = "连不上更新服务器，请检查网络后重试";
    } else if (raw.includes("下载安装包失败")) {
      code = "update_download";
      msg = "下载失败，请检查网络后重试";
    } else if (raw.includes("NEED_INSTALL_PERMISSION")) {
      code = "update_permission";
      msg = "请先允许盘坊安装应用，然后回来再点一次更新";
    }
    // 拆包安装失败时保留「第 N/M 个表盘」上下文（一句话说清是哪一包失败）
    const pkg = raw.match(/第 \d+\/\d+ 个表盘/);
    if (step) diagReport(step, code, raw);
    return pkg ? `${pkg[0]}安装失败：${msg}` : msg;
  };
  /** 每次点击至少让转圈显示这么久：否则「点不动」和「秒失败」在屏幕上看不出区别（排查困难） */
  const MIN_SPIN_MS = 400;
  /** 浮窗与底栏（Tab 栏/编辑页应用栏/裁剪页面板）的固定间距（px） */
  const TOAST_GAP = 16;

  const [activeTab, setActiveTab] = useState<TabKey>(boot.tab);
  /** 全局浮窗：success=绿勾（操作完成）；error=红叉（失败/拒绝/需要先做某事的提示） */
  const [toast, setToast] = useState<{ msg: string; kind: "success" | "error" } | null>(null);
  /** 当前页面底栏高度（px）：浮窗据此定位，与底栏保持固定间距 */
  const [bottomBarH, setBottomBarH] = useState(0);

  // 页面切换后测量底栏（data-pf-bottombar）高度：底栏低浮窗就低、底栏高浮窗跟着抬，
  // 两者间距恒定（TOAST_GAP）。无底栏页面按 80px 的基础位置显示。
  useEffect(() => {
    const measure = () => {
      const el = document.querySelector("[data-pf-bottombar]");
      setBottomBarH(el ? Math.round(el.getBoundingClientRect().height) : 0);
    };
    measure();
    const t = window.setTimeout(measure, 120); // 等页面进场动画/布局稳定后复测一次
    return () => window.clearTimeout(t);
  }, [currentStep, activeTab]);

  // 设置项状态（本地持久化：localStorage）
  // 「本地表盘安装」入口默认关闭（用户要求：首页默认只显示兑换付费表盘）
  const [enableLocalInstall, setEnableLocalInstall] = useState(() => readStoredFlag("panfang.enableLocalInstall", false));
  const [isPrimaryLocal, setIsPrimaryLocal] = useState(() => readStoredFlag("panfang.primaryLocal", false)); // false: 付费表盘为主；true: 本地表盘为主

  // 付费表盘：绑定态 / 已购列表 / 本机设备屏幕（判断"能不能装"）
  const [paidBound, setPaidBound] = useState(false);
  const [paidDisplay, setPaidDisplay] = useState<string | null>(null);
  const [paidProducts, setPaidProducts] = useState<PaidProduct[]>([]);
  /**
   * 有新版本的已购表盘数（首页卡片角标 + 文案用）：
   * 单位是**资源**（一个购买链接一个），行内有任一设备行可更新就算一个。
   */
  const paidUpdateCount = paidProducts.reduce(
    (n, p) => n + (p.variants.some(v => v.has_update) ? 1 : 0),
    0
  );
  const [paidShopUrl, setPaidShopUrl] = useState<string | null>(null);
  /** 绑定方式：oauth（爱发电授权页，正式） / dev（联调） / none（未开放） */
  const [paidMode, setPaidMode] = useState<"oauth" | "dev" | "none" | null>(null);
  /** 正在等浏览器里完成授权时的会话号（null = 没在等） */
  const paidOauthState = useRef<string | null>(null);
  const paidOauthTimer = useRef<number | null>(null);
  const [paidScreen, setPaidScreen] = useState<[number, number] | null>(null);
  /** 本机安装记录（纯本地读）：管理页把「设备上的表盘」对上资源、标「可更新」用 */
  const [paidInstalls, setPaidInstalls] = useState<PaidInstallRecord[]>([]);
  const [paidLoaded, setPaidLoaded] = useState(false);
  const [paidBusy, setPaidBusy] = useState(false);
  /** 付费安装失败后「重试安装」用：重试装的就是上次勾的那几个包体（fileIds=null = 整组装） */
  const paidTarget = useRef<{
    product: PaidProduct;
    variant: PaidVariant;
    fileIds: string[] | null;
  } | null>(null);
  /** 多包体安装前的「勾选包体」浮层：files = 该版本全部包体（顺序即表盘1..N），picked = 勾选状态 */
  const [installPicker, setInstallPicker] = useState<{
    product: PaidProduct;
    variant: PaidVariant;
    files: PaidFile[];
    picked: boolean[];
  } | null>(null);
  /** 点了安装、正在读包体列表的资源（按钮转「准备中…」；与 installBusyRef 一起挡连点） */
  const [installPreparing, setInstallPreparing] = useState<string | null>(null);
  const installBusyRef = useRef(false);
  const [installProgress, setInstallProgress] = useState(0);
  const [progressPhase, setProgressPhase] = useState("正在验证兑换凭证...");
  const [dialName, setDialName] = useState("雾屿蓝·动态");
  /** 这次安装来自哪里：兑换流程（演示）还是本地 .bin 文件 → 决定失败后「重试安装」怎么重试 */
  const [installSource, setInstallSource] = useState<"paid" | "local">("paid");
  /** 本地安装的待装文件（失败重试要用同一份字节，不再让用户重选文件） */
  const localInstall = useRef<{ addr: string; b64: string; ext: string } | null>(null);

  const [dials, setDials] = useState<InstalledDial[]>(() => boot.dials);
  const [editingDial, setEditingDial] = useState<InstalledDial | null>(boot.editingDial);

  // 动作表（Action Sheet）状态
  const [activeActionSheetDial, setActiveActionSheetDial] = useState<InstalledDial | null>(null);
  const [uninstallConfirmDial, setUninstallConfirmDial] = useState<InstalledDial | null>(null);
  const [showLogoutConfirm, setShowLogoutConfirm] = useState(false);
  /** 爱发电账号的二次确认框：退出登录 / 换账号（换账号 = 退出后直接去授权页） */
  const [paidAccountConfirm, setPaidAccountConfirm] = useState<null | "logout" | "switch">(null);
  /** 从设备列表移除的二次确认（只移除列表项，小米账号不动） */
  const [deviceRemoveConfirm, setDeviceRemoveConfirm] = useState<DeviceItem | null>(null);

  // App 自更新（安卓）：检查结果 / 「发现新版本」对话框 / 下载进度浮层
  const [appUpdate, setAppUpdate] = useState<AppUpdateCheck | null>(null);
  const [appUpdatePrompt, setAppUpdatePrompt] = useState(false);
  const [appUpdatePhase, setAppUpdatePhase] = useState<{ step: "downloading" | "installing"; percent: number } | null>(null);
  /** 上次检查到的新版本号（localStorage 记住）：关于页在没有实时结果时也能显示「有新版本」 */
  const [appUpdateKnown, setAppUpdateKnown] = useState<string | null>(() => {
    try {
      return localStorage.getItem("panfang.updateKnown") || null;
    } catch {
      return null;
    }
  });

  /** 检查更新。silent=true（启动后台查）时只弹一条浮窗，不打断当前操作。 */
  const runAppUpdateCheck = async (silent: boolean) => {
    if (!invoke) {
      if (!silent) showToast("浏览器预览里没有更新检查", "error");
      return;
    }
    try {
      const r = await appUpdateCheck();
      setAppUpdate(r);
      try {
        localStorage.setItem("panfang.updateCheckedAt", String(Date.now()));
      } catch {
        /* 存不下就算了，只是节流无效 */
      }
      if (!r.available) {
        setAppUpdateKnown(null);
        try {
          localStorage.removeItem("panfang.updateKnown");
        } catch {
          /* 同上 */
        }
        if (!silent) showToast(`已是最新版本 v${r.current}`);
        return;
      }
      const vn = r.version_name || "";
      setAppUpdateKnown(vn || null);
      try {
        localStorage.setItem("panfang.updateKnown", vn);
      } catch {
        /* 同上 */
      }
      if (silent) {
        // 同一个版本只打扰一次（不然每次冷启动都弹）
        let notified = false;
        try {
          notified = localStorage.getItem("panfang.updateNotified") === vn;
        } catch {
          /* 同上 */
        }
        if (!notified && vn) {
          try {
            localStorage.setItem("panfang.updateNotified", vn);
          } catch {
            /* 同上 */
          }
          showToast(`发现新版本 v${vn}，在「设置 → 关于应用」里更新`);
        }
        return;
      }
      setAppUpdatePrompt(true);
    } catch (err) {
      if (!silent) showToast(humanize(err, "update_check"), "error");
    }
  };

  /** 点「立即更新」：下载（进度浮层 + 保持屏幕常亮）→ 校验 → 调系统安装器。 */
  const startAppUpdate = async () => {
    setAppUpdatePrompt(false);
    setAppUpdatePhase({ step: "downloading", percent: 0 });
    // 下载期间不熄屏：息屏后进程可能被 ROM 冻结，下载就断在半路（与装表盘同一条经验）
    setKeepScreenOn(true);
    let off: (() => void) | null = null;
    try {
      if (listen) {
        off = await listen("wf-app-update-progress", ev => {
          const p = (ev.payload || {}) as { percent?: number };
          setAppUpdatePhase(prev =>
            prev ? { step: "downloading", percent: typeof p.percent === "number" ? p.percent : prev.percent } : prev
          );
        });
      }
      const dl = await appUpdateDownload();
      setAppUpdatePhase({ step: "installing", percent: 100 });
      const r = await appUpdateInstall(dl.path);
      if (r?.needs_permission) {
        showToast("请先允许盘坊安装应用，然后回来再点一次更新", "error");
      } else {
        showToast("安装器已打开，按提示点「安装」即可");
      }
    } catch (err) {
      showToast(humanize(err, "update"), "error");
    } finally {
      setKeepScreenOn(false);
      setAppUpdatePhase(null);
      off?.();
    }
  };

  // 启动后静默检查一次更新（24 小时节流）：只为在「关于应用」上挂个「有新版本」提示，
  // 不打断任何操作。判断依据是**服务端发布的版本**，不是本机版本。
  useEffect(() => {
    if (!invoke) return;
    let last = 0;
    try {
      last = Number(localStorage.getItem("panfang.updateCheckedAt") || 0) || 0;
    } catch {
      /* 读不到就当没查过 */
    }
    if (Date.now() - last < 24 * 3600 * 1000) return;
    const t = window.setTimeout(() => {
      void runAppUpdateCheck(true);
    }, 5000);
    return () => window.clearTimeout(t);
    // 只在启动时挂一次：检查本身有 24h 节流，重复挂没有意义
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 登录页状态规范化（按单独交付版文档）
  const [loginButtonText, setLoginButtonText] = useState("登录");
  const [showLoginFallback, setShowLoginFallback] = useState(false); // 状态 B · 异常兜底
  const [loginError, setLoginError] = useState<string | null>(null); // 状态 C · 错误行内提示

  // 错误态样例状态（行内红色提示）
  const [paidError, setPaidError] = useState<string | null>(null);
  const [connectError, setConnectError] = useState<string | null>(null);
  /** 最近一次连不上的设备（失败引导卡与帮助页的「重新连接」都用它，避免用户回去重找） */
  const [failedDevice, setFailedDevice] = useState<DeviceItem | null>(null);
  /** 连续失败次数（连上即清零）：到 3 次主动问一句「要不要看看怎么连」 */
  const connectFailStreak = useRef(0);
  const [showConnectHelpPrompt, setShowConnectHelpPrompt] = useState(false);
  const [installError, setInstallError] = useState<string | null>(null);

  // 空态样例触发与展示状态
  const [mockDevicesList, setMockDevicesList] = useState<DeviceItem[]>(MOCK_DEVICES);
  /** 已从设备列表移除的设备 id（mac）：小米账号里不动，只在本机记住「别再列出来」 */
  const [hiddenDeviceIds, setHiddenDeviceIds] = useState<string[]>(() => readStoredList(HIDDEN_DEVICES_KEY));
  /** 移除名单的最新值（applyCloudDevices 会被启动/登录/2FA 等多处旧闭包调用，闭包里读 state 会拿到旧值） */
  const hiddenDeviceIdsRef = useRef(hiddenDeviceIds);
  hiddenDeviceIdsRef.current = hiddenDeviceIds;

  // ================= 页面栈：按「层级」返回（不是操作历史） =================
  // 单一事实来源：navStack（栈顶 = 当前页面）。规则（第三轮 4c）：
  //   · 一级界面（首页/管理/设置）之间切换**不产生返回记录** —— 栈里永远只有一条 `main/<tab>`，
  //     只把栈顶那条改写成当前 tab（replaceState），所以在任何一级界面按返回都直接退出 App；
  //   · 二级/三级页面（选择设备/编辑页/裁剪/关于/许可/开发者/兑换/安装）才 push 一层，返回按层级退。
  // 返回键（popstate）只做一件事：栈深 > 1 就退一格；栈里只剩根页面时不拦截（由系统退出 App）。

  /** 页面栈（栈底 = 根页面，从恢复出来的栈或登录页开始） */
  const navStack = useRef<string[]>([]);
  if (!navStack.current.length) navStack.current = boot.stack;
  /** 已压入浏览器历史的条目数（= 栈深 - 1） */
  const navDepth = useRef(0);
  /** 应用内返回按钮自己同步历史时，要忽略的那几次 popstate */
  const ignorePop = useRef(0);
  /** 重置页面栈时挂起的根路由：popstate 到达后把落点条目改写成它 */
  const pendingReset = useRef<string | null>(null);
  /** 启动时把页面栈镜像进浏览器历史只做一次 */
  const bootNavDone = useRef(false);

  /** 栈顶路由（当前页面） */
  const currentRoute = () => navStack.current[navStack.current.length - 1] || "login";

  /** 路由 → 界面状态（所有换页的唯一出口） */
  const applyRoute = (route: string) => {
    const { step, tab } = parseRoute(route);
    setActiveTab(tab);
    setCurrentStep(step);
    // 换页时顺手收掉页面上的临时浮层：返回键切页后不该把「⋯」菜单/弹层留在新页面上
    setActiveActionSheetDial(null);
    setUninstallConfirmDial(null);
    setShowLogoutConfirm(false);
    // 从「怎么连上手环」返回 → 把设备浮窗还回去（它才是上一级界面，用户 2026-10-07 要求）
    if (helpFromDrawer.current && step !== "connect_help") {
      helpFromDrawer.current = false;
      setIsDeviceDrawerOpen(true);
      topIsOverlay.current = true;
    } else {
      setIsDeviceDrawerOpen(false);
    }
  };

  /**
   * 一级界面之间切换（首页 / 管理 / 设置）：**不入栈**，只把栈顶那条改写成目标 tab。
   * 这样切来切去历史里始终只有一条 main 记录，返回键在一级界面直接退出 App。
   */
  const switchTab = (tab: TabKey) => {
    const route = `main/${tab}`;
    const stack = navStack.current;
    if (route === currentRoute()) {
      setActiveTab(tab);
      return;
    }
    navStack.current = [...stack.slice(0, -1), route];
    try {
      window.history.replaceState({ panfang: true, route }, "", `#${route}`);
    } catch {
      /* 忽略 */
    }
    applyRoute(route);
  };

  /** 进入二级/三级页面：压一层栈 + 压一条浏览器历史（按层级返回） */
  const pushRoute = (route: string) => {
    if (route === currentRoute()) return;
    navStack.current = [...navStack.current, route];
    navDepth.current += 1;
    topIsOverlay.current = false; // 新压的是一层真页面
    try {
      window.history.pushState({ panfang: true, route }, "", `#${route}`);
    } catch {
      /* 无 history（极少数环境）：返回键退化成系统默认行为 */
    }
    applyRoute(route);
  };

  /** 当前历史栈顶是不是「设备浮窗」那一层（返回键据此只关浮窗、不跳页） */
  const topIsOverlay = useRef(false);
  /** 「怎么连上手环」是不是从浮窗进来的（返回时要把浮窗还回去） */
  const helpFromDrawer = useRef(false);

  /**
   * 打开设备浮窗 —— **当成一层覆盖层**：压一条历史条目。
   * 这样返回键/侧滑只会关掉浮窗、页面停在本页（用户 2026-10-07 要求）。
   */
  const openDeviceDrawer = () => {
    setIsDeviceDrawerOpen(true);
    navDepth.current += 1;
    topIsOverlay.current = true;
    try {
      window.history.pushState(
        { panfang: true, route: currentRoute(), overlay: "device-drawer" },
        "",
        `#${currentRoute()}`
      );
    } catch {
      /* 无 history（极少数环境）：退化成原来的行为 */
    }
  };

  /** 点 X / 点遮罩关闭浮窗：顺手把那条历史消费掉，避免影响后面的返回 */
  const closeDeviceDrawer = () => {
    setIsDeviceDrawerOpen(false);
    if (navDepth.current > 0) {
      navDepth.current -= 1;
      ignorePop.current += 1;
      try {
        window.history.back();
      } catch {
        ignorePop.current -= 1;
      }
    }
    topIsOverlay.current = false;
  };

  /** 返回上一页（返回到栈里上一层；栈里没有上一层时退回 fallback 并把它当根页面） */
  const goBackOr = (fallback: string) => {
    if (navStack.current.length <= 1) {
      resetStackTo(fallback);
      return;
    }
    navStack.current = navStack.current.slice(0, -1);
    applyRoute(currentRoute());
    if (navDepth.current > 0) {
      // 历史里确实还有一层：同步退回一格，并忽略它带来的这次 popstate
      navDepth.current -= 1;
      ignorePop.current += 1;
      try {
        window.history.back();
      } catch {
        ignorePop.current -= 1;
      }
    }
  };

  /**
   * 重置页面栈（登录成功 → 首页 / 退出登录 → 登录页）：目标页面成为新的根页面。
   * 栈底以上的浏览器历史条目一次性回退掉，再把落点条目改写成新根（popstate 里收尾）。
   */
  const resetStackTo = (route: string) => {
    navStack.current = [route];
    const depth = navDepth.current;
    pendingReset.current = route;
    navDepth.current = 0;
    if (depth > 0) {
      try {
        window.history.go(-depth);
      } catch {
        pendingReset.current = null;
      }
    } else {
      pendingReset.current = null;
      try {
        window.history.replaceState({ panfang: true, route }, "", `#${route}`);
      } catch {
        /* 忽略 */
      }
    }
    applyRoute(route);
  };

  // —— 真实链路（Tauri）会话态：浏览器里没有 window.__TAURI__ 时全程退回演示数据 ——
  /** 账号下云端设备（含 mac / authkey / model）→ 设备列表与连接用 */
  const accountDevices = useRef<CloudDevice[]>([]);
  /** 当前连接：addr = 设备 mac，model = 型号（喂给渲染/裁剪），name = 显示名 */
  const connected = useRef<{ addr: string | null; model: string | null; name: string | null }>({
    addr: null,
    model: null,
    name: null
  });
  /** 各设备最近一次读到的电量（按 mac 缓存，刷新设备列表时不丢） */
  const batteryOf = useRef<Record<string, number>>({});
  /** 最近一次 watchface_get_list 的原始项（设备当前样式 / 配色 / 槽位） */
  const wfItems = useRef<WfListItem[]>([]);
  /** 留档 .bin → 是否照片表盘（只算一次，避免重复读留档） */
  const photoDialCache = useRef<Record<string, boolean>>({});
  /** 设备上有本地留档（= 本 APP 装过、可编辑）的表盘 id：没有留档的不让进编辑页 */
  const archivedIds = useRef<Record<string, boolean>>({});
  /** 编辑页当前会话：留档 .bin 字节 + 图层模型 */
  const binSession = useRef<{ faceId: string; dataB64: string; model: LayerModelDoc | null } | null>(null);
  const dialPreviewSeq = useRef(0); // 预览渲染序号：只让最新一次请求的结果上画布
  /** 编辑页选项与真实 id 的对应关系（界面只显示文案，下发要用真实序号） */
  const dialOptions = useRef<{
    /** 亮屏样式的主题序号（与 styleLabels 一一对应，按下标取，避免同名样式串位） */
    styleIds: number[];
    /** 界面上的样式标签（重名样式会带「(2)」后缀） */
    styleLabels: string[];
    /** 给设备发的原始样式名（手环的 style 字段认名字，不能带后缀） */
    styleRawNames: string[];
    colorNames: string[];
    colorIds: number[];
    /** 可编辑槽位：槽位序号 + 每个候选的真实动作序号 + 默认动作序号（names 仅用于显示） */
    slots: Array<{ slot: number; names: string[]; ids: number[]; defaultId: number }>;
    startStyleColorIndex: number | null;
  } | null>(null);
  /** 待裁剪照片（选图后暂存，裁剪完成才进壁纸列表） */
  const pendingPhoto = useRef<{ b64: string; mime: string; name: string } | null>(null);
  /** 壁纸列表对应的原始字节与裁剪变换（下标与 wallpapers 一一对应） */
  const wallpaperMeta = useRef<Array<{ b64: string; crop: PhotoCrop | null; name: string }>>([]);
  /** 真实渲染出的表盘预览图（PNG data URL） */
  const [renderPreviewUrl, setRenderPreviewUrl] = useState<string | null>(null);
  /** 表盘自身屏幕 / 当前连接设备的屏幕（模型里带；演示态为 null → 用 DEMO_SCREEN） */
  const [dialScreens, setDialScreens] = useState<{ dial: ScreenInfo | null; device: ScreenInfo | null }>({
    dial: null,
    device: null
  });
  /** 真实 App 版本（app_version 命令；浏览器演示保持设计稿文案） */
  const [appVersion, setAppVersion] = useState<string | null>(null);

  // 编辑页选项（沿用设计稿的组件与排版，只换数组内容；初始 = 原演示数组）
  const [styleOptions, setStyleOptions] = useState<string[]>(LARGE_STYLES);
  const [colorOptions, setColorOptions] = useState<Array<{ name: string; hex: string }>>(LARGE_COLORS);

  const [selectedStyle, setSelectedStyle] = useState(LARGE_STYLES[0]);
  const [selectedColorName, setSelectedColorName] = useState(LARGE_COLORS[0].name);
  /** 容器卡片：数量由模型的槽位数决定（演示态 = 设计稿那两张） */
  const [containers, setContainers] = useState<ContainerCard[]>(DEMO_CONTAINERS);

  /**
   * 编辑页的真实数据状态：真机上只有 ready 才渲染编辑内容。
   * 其余状态一律显示「载入中 / 没有可编辑的留档」——**不许拿设计稿的演示数据充数**
   * （2026-10-07 真机上出现过一次「假表盘」：重启恢复页面栈时跳过了留档载入）。
   */
  const [dialSessionState, setDialSessionState] = useState<"idle" | "loading" | "ready" | "failed">("idle");

  const [openSection, setOpenSection] = useState<AccordionSection>(null);

  // 多图壁纸状态（支持 10+ 张正方形预览与上传添加）
  const [wallpapers, setWallpapers] = useState<string[]>(INITIAL_WALLPAPERS);
  const [currentWallpaperIndex, setCurrentWallpaperIndex] = useState(0);

  // 裁剪图片页面状态
  const [pendingCropImage, setPendingCropImage] = useState<string>("");
  const [cropOffset, setCropOffset] = useState({ x: 0, y: 0 });
  const [cropRotation, setCropRotation] = useState(0);
  const [cropScale, setCropScale] = useState(1);
  const isCropDragging = useRef(false);
  const cropDragStart = useRef({ x: 0, y: 0 });
  // 裁剪预览用 canvas 2D 画（安卓/vivo WebView 上 <img>+CSS transform 会黑屏）：
  // 位图缓存 + 「位图被回收后重解码」那套防护见 rebuildCropBitmap / ensureCropVisible
  const cropCanvasRef = useRef<HTMLCanvasElement | null>(null);
  const cropViewRef = useRef<HTMLDivElement | null>(null);
  const cropBitmap = useRef<CanvasImageSource | null>(null);
  const cropBitmapCache = useRef<Map<string, CanvasImageSource>>(new Map());
  const cropDecoding = useRef(false);
  /** 供滚动/可见性等异步回调读取的最新变换与视口尺寸（避免闭包读到旧值） */
  const cropStateRef = useRef({
    x: 0,
    y: 0,
    scale: 1,
    rotation: 0,
    frame: { w: 208, h: 248, radius: 30 }
  });

  const [isApplying, setIsApplying] = useState(false);
  const [appliedSuccess, setAppliedSuccess] = useState(false);

  const currentColorHex = colorOptions.find(c => c.name === selectedColorName)?.hex || "#4361EE";
  /** 演示画布上那两处容器装饰，跟着第 1/2 张容器卡片的选择变（与设计稿 mock 一致） */
  const mockTopContainer = containers[0]?.selected ?? "";
  const mockBottomContainer = containers[1]?.selected ?? "";

  // —— 画布 / 裁剪遮罩的尺寸参数化（docs/设计稿v2-对照与UI实施要点.md §5.2）——
  // 屏幕来源：优先当前连接设备，其次表盘自身画布，最后演示默认（212×520 跑道屏）。
  const canvasScreen: ScreenInfo = dialScreens.device ?? dialScreens.dial ?? DEMO_SCREEN;
  /** 编辑页画布框：等比最大 200×260 的 2/3（收紧版，给下方操作区留更多空间）+ 圆角同比例 */
  const canvasFrame = frameSizeFor(canvasScreen, 134, 174);
  /** 裁剪视图/蒙版：同一套比例（设计稿的裁剪框 208×248 作为尺寸上限） */
  const cropFrame = frameSizeFor(canvasScreen, 208, 248);
  /** 表盘与当前设备规格不一致时给出告警（画布标识行变黄） */
  const canvasSpecMismatch =
    dialScreens.dial && dialScreens.device &&
    (dialScreens.dial.width !== dialScreens.device.width || dialScreens.dial.height !== dialScreens.device.height)
      ? { dial: dialScreens.dial, device: dialScreens.device }
      : null;
  /** 设备标识行：`{设备名} · {宽}×{高}`（设备名未知时只留尺寸） */
  const canvasLabel = [connected.current.name, `${canvasScreen.width}×${canvasScreen.height}`]
    .filter(Boolean)
    .join(" · ");
  /** 演示/无渲染图时，设计稿那层 mock 内容按 208×248 基准等比缩放进设备框 */
  const mockScale = Math.min(canvasFrame.w / 208, canvasFrame.h / 248);
  // 裁剪预览的绘制参数（异步回调里读它，始终是最新值）
  cropStateRef.current = {
    x: cropOffset.x,
    y: cropOffset.y,
    scale: cropScale,
    rotation: cropRotation,
    frame: { w: cropFrame.w, h: cropFrame.h, radius: cropFrame.radius }
  };

  const showToast = (msg: string, kind: "success" | "error" = "success") => {
    setToast({ msg, kind });
    setTimeout(() => setToast(null), 2500);
  };

  // ================= 真实链路（Tauri）数据源 =================
  // 以下函数只在 App（window.__TAURI__ 存在）里真正调用后端命令；
  // 浏览器预览（invoke === null）时它们不产生任何效果，界面继续走演示数据。

  /** 登录 / 2FA 完成 → 用云端设备填充「选择设备」列表（保留已选设备与已读到的电量） */
  const applyCloudDevices = (list: CloudDevice[] | null | undefined) => {
    const devs = Array.isArray(list) ? list : [];
    accountDevices.current = devs;
    /** 账号下的设备（id 与原始下标绑定，先映射再过滤，避免下标错位） */
    const all: DeviceItem[] = devs.map((d, i) => {
      const id = d.mac || `cloud-${i}`;
      return {
        id,
        name: d.name || d.model || d.mac || `设备 ${i + 1}`,
        model: d.model || "",
        // 未连接时拿不到电量 → null（界面显示 --%）；连上后由 device_get_data(status) 补真值
        battery: batteryOf.current[id] ?? null
      };
    });
    // 用户手动移除过的设备不再出现在列表里（重新导入设备列表会清掉这个名单）
    const hidden = new Set(hiddenDeviceIdsRef.current);
    const mapped = all.filter(d => !hidden.has(d.id));
    setMockDevicesList(mapped);
    if (!mapped.length) return;
    setSelectedDevice(prev => mapped.find(d => d.id === prev.id) ?? mapped[0]);
  };

  /**
   * 从设备列表移除一台设备：只在本机记住「别再列出来」——
   * 小米账号下的设备列表不归我们管，也不该动（用户 2026-10-08 要求）。
   * 移除的正好是当前连接的那台 → 一并断开（不能留「已连接到一台不在列表里的设备」）。
   */
  const removeDeviceFromList = (device: DeviceItem) => {
    const next = hiddenDeviceIdsRef.current.includes(device.id)
      ? hiddenDeviceIdsRef.current
      : [...hiddenDeviceIdsRef.current, device.id];
    hiddenDeviceIdsRef.current = next;
    setHiddenDeviceIds(next);
    storeList(HIDDEN_DEVICES_KEY, next);
    setMockDevicesList(prev => prev.filter(d => d.id !== device.id));
    if (connected.current.addr === device.id) {
      releaseStaleConnection(device.id);
      connected.current = { addr: null, model: null, name: null };
      setIsDeviceConnected(false);
      connectedStateRef.current = false;
      setLinkRetrying(false);
      reportedLoss.current = false;
      setDials([]);
    }
    showToast(`已移除「${device.name}」`);
  };

  /** 重新导入：先清掉「已移除」名单，再把账号下的设备全拉回来（用户要求的找回方式） */
  const clearHiddenDevices = () => {
    hiddenDeviceIdsRef.current = [];
    setHiddenDeviceIds([]);
    storeList(HIDDEN_DEVICES_KEY, []);
  };

  /** 电量写回设备列表与当前选中设备（null = 掉线/拿不到真值 → 界面显示 --%） */
  const patchBattery = (addr: string, capacity: number | null) => {
    if (capacity === null) delete batteryOf.current[addr];
    else batteryOf.current[addr] = capacity;
    setMockDevicesList(prev => prev.map(d => (d.id === addr ? { ...d, battery: capacity } : d)));
    setSelectedDevice(prev => (prev.id === addr ? { ...prev, battery: capacity } : prev));
  };

  /** 连接状态的镜像 effect：事件回调读 ref，判断依据永远是当前的真心话 */
  useEffect(() => {
    connectedStateRef.current = isDeviceConnected;
  }, [isDeviceConnected]);

  /**
   * 统一的「链路可用」入口：连接成功 / 前端探测成功 / 总管报 wf-link-restored 都走这里。
   * 返回 true = 之前界面显示的是未连接（= 刚发生一次「重连恢复」，调用方据此决定提示与刷新）。
   * 不是当前这台设备的信号一律忽略（切换设备期间的迟到事件）。
   */
  const applyLinkConnected = (addr: string | null | undefined, battery?: number | null): boolean => {
    if (addr && connected.current.addr && addr !== connected.current.addr) return false;
    const wasRed = !connectedStateRef.current;
    setIsDeviceConnected(true);
    connectedStateRef.current = true;
    heartbeatFailures.current = 0;
    reportedLoss.current = false;
    setLinkRetrying(false);
    if (addr && typeof battery === "number") patchBattery(addr, Math.round(battery));
    return wasRed;
  };

  /** 掉线信号发给所有在等的长任务（安装/应用）：立刻失败，不再干等停滞看门狗 */
  const fireLinkLost = () => {
    const list = linkLostRejects.current;
    linkLostRejects.current = [];
    list.forEach(f => f(new Error("手环连接已断开，安装已中断")));
  };

  /**
   * 让「等待设备操作」与「掉线」赛跑：链路一断就不再干等（原来要等 25 秒停滞看门狗）。
   * 用法：withStallGuard(linkLostRace(inv("device_install_payload", {...})))
   */
  const linkLostRace = <T,>(p: Promise<T>): Promise<T> =>
    new Promise<T>((resolve, reject) => {
      const onLost = (err: Error) => reject(err);
      linkLostRejects.current.push(onLost);
      const drop = () => {
        const i = linkLostRejects.current.indexOf(onLost);
        if (i >= 0) linkLostRejects.current.splice(i, 1);
      };
      p.then(
        v => {
          drop();
          resolve(v);
        },
        e => {
          drop();
          reject(e);
        }
      );
    });

  /** 后台保活引导（一次性）：国产 ROM 会把后台进程冻死/杀掉，给一句能照做的话 */
  const maybeShowLinkHelp = () => {
    if (readStoredFlag(LINK_HELP_FLAG, false)) return;
    storeFlag(LINK_HELP_FLAG, true);
    showToast("可在「设置 → 后台保持连接」里允许盘坊后台运行", "error");
  };

  /** 「照片」角标判据：读本地留档 .bin，亮屏样式 backImageId 高字节 0x82（与后端 derive_model 同源） */
  const markPhotoDials = async (addr: string, faceIds: string[]) => {
    if (!invoke) return;
    for (const faceId of faceIds) {
      if (photoDialCache.current[faceId] !== undefined) continue;
      try {
        const a = (await invoke("watchface_archive_get", { addr, faceId })) as ArchiveData | null;
        if (!a?.dataB64) {
          photoDialCache.current[faceId] = false;
          continue;
        }
        const summary = (await invoke("watchface_bin_inspect", { dataB64: a.dataB64 })) as
          | { styles?: BinStyleSection[] }
          | null;
        const isPhoto = isPhotoDialBinSummary(summary);
        photoDialCache.current[faceId] = isPhoto;
        if (isPhoto) {
          setDials(prev => prev.map(d => (d.id === faceId ? { ...d, isPhotoDial: true } : d)));
        }
      } catch {
        photoDialCache.current[faceId] = false;
      }
    }
  };

  /** 管理页表盘列表：watchface_get_list（使用中 = isCurrent）+ 本地留档（能否编辑 / 照片角标） */
  const refreshDials = async (addr: string) => {
    if (!invoke) return;
    try {
      const items = normalizeWfList(await invoke("watchface_get_list", { addr }));
      wfItems.current = items;
      const archived: Record<string, boolean> = {};
      try {
        const arr = (await invoke("watchface_archive_list", { addr })) as ArchiveEntry[] | null;
        for (const a of arr || []) if (a?.id) archived[a.id] = true;
      } catch {
        /* 无留档目录 = 全部没有留档 */
      }
      const mapped: InstalledDial[] = items.map(wf => ({
        id: wf.id,
        name: wf.name || wf.id,
        isCurrent: !!wf.isCurrent,
        isPhotoDial: photoDialCache.current[wf.id] ?? false
      }));
      // 记下哪些表盘有留档（= 可编辑）：轻触无留档的表盘不进编辑页（F 项）
      archivedIds.current = archived;
      setDials(mapped);
      // 角标判定不阻塞列表显示：仅对本设备有留档的表盘读 .bin（每个只算一次）
      void markPhotoDials(addr, mapped.map(d => d.id).filter(id => archived[id]));
    } catch (err) {
      // 后台自动刷新失败不上屏（技术细节进 console）：连接状态胶囊会如实反映问题
      console.error("[refreshDials]", err);
    }
  };

  /** 连接成功后拉一次真值：电量 + 表盘列表（握手需要时间，与 legacy 同样延迟 3s） */
  /**
   * 连接后的**会话验证**：蓝牙连上 ≠ 握手成功。
   * 手环没进「连接新手机」时，device_connect 可能返回成功但会话是死的——
   * 这里连查几次设备状态，拿到电量才算真的连上；否则按失败处理（红点 + 未连接）。
   */
  const verifySession = async (addr: string, tries = 4): Promise<number | null> => {
    const inv = invoke;
    if (!inv) return null;
    for (let i = 0; i < tries; i++) {
      try {
        const st = (await Promise.race([
          inv("device_get_data", { addr, dataType: "status" }) as Promise<{
            battery?: { capacity?: number };
          } | null>,
          new Promise<null>(res => window.setTimeout(() => res(null), 2500))
        ])) as { battery?: { capacity?: number } } | null;
        const cap = Number(st?.battery?.capacity);
        if (st && Number.isFinite(cap)) return Math.round(cap);
      } catch {
        /* 这次没通，继续下一次 */
      }
      await new Promise(res => window.setTimeout(res, 900));
    }
    return null;
  };

  const refreshDeviceData = async (addr: string) => {    if (!invoke) return;
    try {
      const st = (await invoke("device_get_data", { addr, dataType: "status" })) as
        | { battery?: { capacity?: number } }
        | null;
      const cap = Number(st?.battery?.capacity);
      if (Number.isFinite(cap)) patchBattery(addr, Math.round(cap));
    } catch {
      /* 认证未完成 / 设备未上报 → 保持原值 */
    }
    await refreshDials(addr);
  };

  /** 掉线只提示一次（心跳每 12~15 秒一次，别刷屏）；重新探测成功即复位 */
  const reportedLoss = useRef(false);

  /**
   * 探测一次设备连接状态（心跳 / 回到前台时调）：
   * - 成功 → 已连接 + 刷新电量；
   * - 失败或超时 → 计数 +1，**连续两次**才判掉线（红点 + 未连接 + 电量 --%），避免一次抖动就误报。
   * 后端没有断线事件（只有 wf-2fa / wf-edit-progress / wf-install-progress），所以以前端心跳为准。
   */
  const probeDeviceStatus = async () => {
    const inv = invoke;
    const addr = connected.current.addr;
    if (!inv || !addr || heartbeatInFlight.current || deviceBusy.current) return;
    heartbeatInFlight.current = true;
    let timer: number | null = null;
    try {
      const status = (await Promise.race([
        inv("device_get_data", { addr, dataType: "status" }),
        new Promise<never>((_, reject) => {
          timer = window.setTimeout(() => reject(new Error("status 探测超时")), STATUS_PROBE_TIMEOUT_MS);
        })
      ])) as { battery?: { capacity?: number } } | null;
      const cap = Number(status?.battery?.capacity);
      // 只有真的拿回设备状态（含电量）才算「已连接」：RPC 没抛错但没带回电量时，
      // 说明这条链路并没有真的通 —— 手环被「小米运动健康」占用时实测就是这个形态
      // （先是假的绿点 + 电量 --%，几十秒后才变回未连接）。
      if (!status || !Number.isFinite(cap)) throw new Error("设备无有效状态回复");
      const wasRed = applyLinkConnected(addr, cap);
      if (wasRed) {
        // 界面本来是红的、探测却成功了 → 让 Rust 连接总管也复核一次（它才是连接状态的持有者）
        void inv("device_relink");
        void refreshDials(addr);
      }
    } catch {
      // 探测期间可能已经切走/断开 → 只统计当前这台
      if (connected.current.addr !== addr) return;
      const wasRed = !connectedStateRef.current;
      heartbeatFailures.current += 1;
      if (heartbeatFailures.current >= 2) {
        const firstNotice = !reportedLoss.current;
        reportedLoss.current = true;
        setIsDeviceConnected(false);
        connectedStateRef.current = false;
        setLinkRetrying(true);
        patchBattery(addr, null); // 电量回 --%
        fireLinkLost(); // 长任务（安装/应用）立刻失败，不等停滞看门狗
        // 掉线不吭声是最难受的：给一句能照做的提示，同时记进开发者后台
        if (firstNotice) {
          diagReport("connect", "lost", "心跳连续两次探测失败，判定掉线");
          showToast("手环连接已断开，正在自动重连…", "error");
        }
      }
      if (heartbeatFailures.current >= 2 || wasRed) {
        // 请连接总管复核 →（真断了就）自动重连；已经红着还探失败 → 立刻再武装一轮，
        // 这样「蓝牙重新打开」不必等下一次 60 秒兜底就能接上。
        // 总管回 armed=false = 用户主动断开过（手里没有「要保住的连接」）→ 收掉「重连中…」，
        // 也不再自动重连（用户 2026-10-10 明确要求）。
        void inv("device_relink")
          .then((r: { armed?: boolean } | null) => {
            if (r && r.armed === false) {
              setLinkRetrying(false);
              reportedLoss.current = false;
              connected.current = { addr: null, model: null, name: null };
            }
          })
          .catch(() => {});
      }
    } finally {
      if (timer !== null) clearTimeout(timer);
      heartbeatInFlight.current = false;
    }
  };

  /**
   * 安装停滞（看门狗超时）= 这条链路已经不可用了：立刻把连接状态改回真实样子
   * （红点 + 电量 --%）。不然用户会对着一个假的「已连接」等心跳——那要 30 秒才判掉线。
   * 屏幕提示已经由安装页的行内错误给了（humanize 那句），这里只修状态，不重复打扰。
   */
  const noteLinkDeadIfStalled = (err: unknown, addr: string | null) => {
    if (!addr || !String(err).includes("停滞")) return;
    heartbeatFailures.current = 2;
    reportedLoss.current = true;
    setIsDeviceConnected(false);
    patchBattery(addr, null);
  };

  /** 照片表盘的本地相册存档 → 壁纸列表 */
  const loadDialAlbum = async (faceId: string) => {
    if (!invoke) return;
    try {
      const saved = (await invoke("watchface_photos_load", { faceId })) as SavedPhoto[] | null;
      const list = Array.isArray(saved) ? saved : [];
      const urls: string[] = [];
      wallpaperMeta.current = [];
      for (const p of list) {
        const mime = sniffImageMime(b64ToBytes(p.b64)) || "image/png";
        urls.push(b64ToDataUrl(p.b64, mime));
        wallpaperMeta.current.push({
          b64: p.b64,
          crop: (p.crop as PhotoCrop) || null,
          name: p.name || "照片"
        });
      }
      setWallpapers(urls);
      setCurrentWallpaperIndex(0);
    } catch {
      /* 没有存档 → 空列表（设计稿空态） */
    }
  };

  /** 用系统浏览器打开外部链接（App 内；浏览器演示让 <a> 自己开新标签） */
  const openLink = (e: React.MouseEvent, url: string) => {
    const inv = invoke;
    if (!inv) return;
    e.preventDefault();
    inv("open_external", { url }).catch(() => {
      /* 打不开就算了：不打断用户 */
    });
  };

  /** 诊断信息正文（版本 + 机型 + 最近几条问题）：买家找开发者时不用自己描述现象 */
  const [diagText, setDiagText] = useState<string | null>(null);
  const copyDiag = async () => {
    const lines: string[] = [];
    try {
      const d = (await invoke?.("diag_recent")) as
        | {
            app_version?: string;
            recent?: { ts?: number; step?: string; code?: string; message?: string }[];
          }
        | null;
      lines.push(`盘坊 v${d?.app_version ?? appVersion ?? "?"}`);
      const name = connected.current.name || connected.current.model;
      lines.push(
        `设备：${name || "未连接"}${connected.current.name && connected.current.model ? `（${connected.current.model}）` : ""}`
      );
      const rec = (d?.recent || []).slice(0, 6);
      if (rec.length) {
        lines.push("最近问题：");
        for (const r of rec) {
          const t = r.ts ? new Date(r.ts * 1000) : null;
          const stamp = t
            ? `${String(t.getMonth() + 1).padStart(2, "0")}-${String(t.getDate()).padStart(2, "0")} ` +
              `${String(t.getHours()).padStart(2, "0")}:${String(t.getMinutes()).padStart(2, "0")}`
            : "";
          lines.push(`· ${stamp} ${r.step || ""} ${r.code || ""} ${r.message || ""}`.replace(/\s+/g, " ").trim());
        }
      } else {
        lines.push("最近没有问题记录");
      }
    } catch (err) {
      console.error("[diag_recent] 读取失败：", err);
      lines.push("（本机诊断信息读取失败）");
    }
    const text = lines.join("\n");
    setDiagText(text);
    try {
      await navigator.clipboard.writeText(text);
      showToast("已复制，粘贴给开发者即可", "success");
    } catch {
      // WebView 不给剪贴板时不要装作成功：让用户长按下面的文字自己复制
      showToast("请长按下方文字复制", "error");
    }
  };

  /**
   * 连接失败引导卡（**设备抽屉与「选择设备」整页共用**）。
   * 放抽屉里时必须位于列表上方：列表长，卡片在列表下方会被挤出可视区（2026-10-07 踩过）。
   */
  const connectGuideCard = connectError ? (
    <div className="bg-[#F5F6F9] rounded-2xl p-4 mb-3">
      <div className="flex items-start gap-3">
        <div className="w-8 h-8 rounded-full bg-[#FF3B30]/10 text-[#FF3B30] flex items-center justify-center shrink-0">
          <svg
            viewBox="0 0 24 24"
            className="w-4 h-4"
            fill="none"
            stroke="currentColor"
            strokeWidth="2.2"
            strokeLinecap="round"
          >
            <line x1="12" y1="8" x2="12" y2="13" />
            <circle cx="12" cy="16.5" r="0.6" fill="currentColor" />
          </svg>
        </div>
        <div className="flex-1 min-w-0">
          <p className="text-[15px] font-bold text-black leading-tight">
            没连上{failedDevice?.name ? `「${failedDevice.name}」` : ""}
          </p>
          <p className="text-[13px] font-medium text-black/45 mt-1 leading-snug">
            先让手环进入「连接新手机」模式，再重新连接
          </p>
        </div>
      </div>
      <div className="flex gap-2 mt-3">
        <button
          type="button"
          onClick={() => {
            helpFromDrawer.current = true; // 返回时把浮窗还回去
            pushRoute("connect-help");
          }}
          className="flex-1 h-[40px] rounded-full bg-[#4361EE] text-white font-bold text-[14px] active:scale-[0.98] transition-all cursor-pointer"
        >
          看怎么连
        </button>
        <button
          type="button"
          onClick={() => failedDevice && handleSelectDevice(failedDevice)}
          className="flex-1 h-[40px] rounded-full bg-black/5 hover:bg-black/10 text-black/75 font-bold text-[14px] active:scale-95 transition-all cursor-pointer"
        >
          重试
        </button>
      </div>
    </div>
  ) : null;

  /** 用户改动过的容器选择（槽位序号 → 动作序号）；没改动不下发，避免误改设备上的容器 */
  const slotChoicesFor = (cards: ContainerCard[]) => {
    const opt = dialOptions.current;
    if (!opt) return null;
    const out: Record<number, number> = {};
    for (const card of cards) {
      if (card.slot == null || card.selectedId == null) continue;
      // 判据是「用户点过这张卡」，不是「选中的是不是默认项」：
      // 第一项就是默认项，用默认项判「没改动」会让第一项永远发不出去（用户实测踩到）。
      if (!card.touched) continue;
      const s = opt.slots.find(x => x.slot === card.slot);
      const aid = card.selectedId;
      if (s && s.ids.includes(aid)) out[card.slot] = aid;
    }
    return Object.keys(out).length ? out : null;
  };

  /** 全部槽位的默认动作序号（下发时把设备没上报的槽位也补上，避免元素错位） */
  const slotDefaultsOf = () => {
    const opt = dialOptions.current;
    if (!opt?.slots.length) return null;
    const out: Record<number, number> = {};
    for (const s of opt.slots) out[s.slot] = s.defaultId;
    return out;
  };

  /** 界面上的样式标签 → 后端主题序号（按**位置**解析；同名样式也不会串到第一个） */
  const themeIndexOf = (label: string) => {
    const opt = dialOptions.current;
    const i = opt?.styleLabels.indexOf(label) ?? -1;
    if (opt && i >= 0) return opt.styleIds[i];
    const normals = (binSession.current?.model?.themes || []).filter(t => t.kind === "normal");
    return normals[0]?.index ?? 0;
  };

  /** 界面样式标签 → 发给设备的原始样式名；标签不认识时返回 null（不下发样式，避免改坏设备） */
  const rawStyleNameOf = (label: string) => {
    const opt = dialOptions.current;
    const i = opt?.styleLabels.indexOf(label) ?? -1;
    if (opt && i >= 0) return opt.styleRawNames[i] ?? null;
    return null;
  };

  /** 当前样式在留档模型里的主题：容器/配色编辑项都按它过滤（每个样式有自己的容器和配色） */
  const currentEditTheme = (() => {
    const themes = binSession.current?.model?.themes;
    if (!themes) return null;
    const idx = themeIndexOf(selectedStyle);
    return (
      themes.find(t => t.kind === "normal" && t.index === idx) ||
      themes.find(t => t.kind === "normal") ||
      null
    );
  })();

  /** 当前样式可用的配色（模型没给数据/演示态时全显示） */
  const visibleColors = (() => {
    const cgs = currentEditTheme?.colorGroups;
    const opt = dialOptions.current;
    if (!cgs || !cgs.length || !opt) return colorOptions;
    return colorOptions.filter(col => {
      const ci = opt.colorNames.indexOf(col.name);
      if (ci < 0) return false;
      const cid = opt.colorIds[ci];
      return cid != null && cgs.includes(cid);
    });
  })();

  /** 当前样式实际用到的容器卡片（样式没引用的槽位不显示；演示槽位 null 恒显示） */
  const visibleContainers = (() => {
    const slots = currentEditTheme?.slots;
    if (!slots || !slots.length) return containers;
    return containers.filter(c => c.slot == null || slots.includes(c.slot));
  })();

  /** 真实预览图：留档 .bin + 样式/配色/容器/照片 → PNG（填进编辑页画布背景） */
  const renderDialPreview = async (over?: { styleName?: string; colorName?: string; wallIdx?: number }) => {
    const session = binSession.current;
    const opt = dialOptions.current;
    if (!invoke || !session?.model || !opt) return;
    const m = session.model;
    const styleName = over?.styleName ?? selectedStyle;
    const colorName = over?.colorName ?? selectedColorName;
    const wallIdx = over?.wallIdx ?? currentWallpaperIndex;
    const ci = opt.colorNames.indexOf(colorName);
    const colorGroup = ci >= 0 ? opt.colorIds[ci] : null;
    const meta = wallpaperMeta.current[wallIdx] || null;
    const isPhoto = !!m.photoAlbum;
    const mySeq = ++dialPreviewSeq.current;
    try {
      const png = (await invoke("watchface_render_preview", {
        dataB64: session.dataB64,
        themeIndex: themeIndexOf(styleName),
        photoB64: isPhoto && meta?.b64 ? meta.b64 : null,
        deviceModel: connected.current.model || null,
        slotChoices: slotChoicesFor(containers),
        photoTransform: isPhoto && meta?.crop ? meta.crop : null,
        colorGroup: (m.colorGroups || []).length > 1 ? colorGroup : null
      })) as string | null;
      // 渲染期间用户又改了别的选项 → 已有更新的请求在途，这次旧结果直接丢弃（防旧图覆盖新图）
      if (mySeq !== dialPreviewSeq.current) return;
      if (typeof png === "string" && png) setRenderPreviewUrl(b64ToDataUrl(png, "image/png"));
    } catch {
      /* 渲染失败：保持上一张预览 */
    }
  };

  // 预览画布实时更新：样式 / 配色 / 壁纸（切换与增删）/ 容器任一变化都重新渲染
  useEffect(() => {
    if (currentStep !== "dial_edit") return;
    void renderDialPreview();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [currentStep, selectedStyle, selectedColorName, currentWallpaperIndex, containers, wallpapers]);

  /** 编辑页入口：留档 .bin → 图层模型 → 选项数组 / 相册 / 预览图
   *  （**真机上不许拿设计稿的演示数据充数**：没连设备/拿不到留档时留成 failed，界面会明确提示） */
  const loadDialSession = async (dial: InstalledDial) => {
    const addr = connected.current.addr;
    if (!invoke) return; // 浏览器预览：保持演示数据（设计稿那套）
    if (!addr) {
      setDialSessionState("failed");
      showToast("请先在管理页连接手环", "error");
      return;
    }
    setDialSessionState("loading");
    const deviceModel = connected.current.model;
    try {
      const a = (await invoke("watchface_archive_get", { addr, faceId: dial.id })) as ArchiveData | null;
      if (!a?.dataB64) throw new Error("留档为空");
      const model = (await invoke("watchface_layer_model", {
        dataB64: a.dataB64,
        deviceModel: deviceModel || null
      })) as LayerModelDoc | null;
      binSession.current = { faceId: dial.id, dataB64: a.dataB64, model };

      const dev = wfItems.current.find(x => x.id === dial.id);
      // 1) 表盘样式：只列亮屏样式（kind === 'normal'）；设备当前样式优先选中，避免一应用就换样式
      const normals = ((model?.themes) || []).filter(t => t.kind === "normal");
      const styleNames = normals.map((t, i) => t.name || `样式 ${i + 1}`);
      // 亮屏样式重名时也给后缀：身份走 styleIds 的下标，显示才用这个标签
      const styleLabels = dedupeNames(styleNames);
      const devStyle = (dev?.style || "").trim();
      const devStylePos = devStyle ? styleNames.indexOf(devStyle) : -1;
      const startStyle = styleLabels[devStylePos >= 0 ? devStylePos : 0];
      // 2) 主题配色：数量用真实值，文案统一「配色 N」（与设计稿一致）
      const groups = model?.colorGroups || [];
      const colorNames = groups.map((_, i) => `配色 ${i + 1}`);
      const devColorIdx = Number.isFinite(dev?.styleColorIndex) ? (dev?.styleColorIndex as number) : null;
      const startColorPos = devColorIdx != null ? groups.findIndex(g => g.index === devColorIdx) : -1;
      // 3) 可编辑容器：**有几个槽位就往下排几张卡片**（不写死、不限两个），选项 = 候选动作名
      const slots = (model?.slots || []).filter(s => (s.actions || []).length > 1);
      const actionName = (aid: number) => model?.actions.find(x => x.index === aid)?.name || `候选 #${aid}`;
      dialOptions.current = {
        styleIds: normals.map(t => t.index),
        styleLabels,
        styleRawNames: styleNames,
        colorNames,
        colorIds: groups.map(g => g.index),
        slots: slots.map(s => ({
          slot: s.index,
          // 显示用名字（同槽位重名时加「(2)」后缀）；下发一律用 ids，不拿名字反查
          names: dedupeNames(s.actions.map(actionName)),
          ids: s.actions,
          defaultId: s.actions[0]
        })),
        startStyleColorIndex: devColorIdx
      };

      if (styleLabels.length) {
        setStyleOptions(styleLabels);
        setSelectedStyle(startStyle || styleLabels[0]);
      }
      if (colorNames.length) {
        setColorOptions(
          colorNames.map((name, i) => ({
            name,
            // .bin 里没有配色色值：氛围渐变的色样沿用设计稿色板
            hex: LARGE_COLORS[i % LARGE_COLORS.length].hex
          }))
        );
        setSelectedColorName(colorNames[startColorPos >= 0 ? startColorPos : 0]);
      }
      // 每个槽位一张卡片；初始选中沿用原口径（第 N 张卡取第 N 个候选，不够就取最后一个）
      if (slots.length) {
        setContainers(
          slots.map((s, order) => {
            const names = dedupeNames(s.actions.map(actionName));
            const startPos = Math.min(order, names.length - 1);
            return {
              slot: s.index,
              label: slotLabelFor(order),
              options: names,
              ids: s.actions,
              selectedId: s.actions[startPos] ?? null,
              selected: names[startPos],
              touched: false
            };
          })
        );
      }

      // 照片表盘以模型为准（回写列表角标与编辑页状态）
      const isPhoto = !!model?.photoAlbum;
      photoDialCache.current[dial.id] = isPhoto;
      // 画布/裁剪按设备屏幕参数化（连接设备优先，型号未知则退回表盘自身画布）
      setDialScreens({ dial: model?.screen ?? null, device: model?.deviceScreen ?? null });
      setDials(prev => prev.map(d => (d.id === dial.id ? { ...d, isPhotoDial: isPhoto } : d)));
      setEditingDial(prev => (prev && prev.id === dial.id ? { ...prev, isPhotoDial: isPhoto } : prev));

      if (isPhoto) await loadDialAlbum(dial.id);
      await renderDialPreview({ styleName: startStyle, colorName: colorNames[startColorPos >= 0 ? startColorPos : 0], wallIdx: 0 });
      setDialSessionState("ready");
    } catch (err) {
      setDialSessionState("failed");
      showToast(humanize(err, "edit_load"), "error");
    }
  };

  // ---------------------- 页面栈：浏览器历史镜像 + 返回键 ----------------------

  // 启动时把恢复出来的页面栈镜像进浏览器历史：栈底写在当前条目上、栈底以上逐条压入，
  // 这样安卓 WebView 的 canGoBack()/goBack() 与页面栈严格一一对应（返回键才能逐级返回）。
  useEffect(() => {
    if (bootNavDone.current) return;
    bootNavDone.current = true;
    const stack = navStack.current;
    try {
      window.history.replaceState({ panfang: true, route: stack[0] }, "", `#${stack[0]}`);
      for (let i = 1; i < stack.length; i++) {
        window.history.pushState({ panfang: true, route: stack[i] }, "", `#${stack[i]}`);
      }
      navDepth.current = stack.length - 1;
    } catch {
      /* 无 history：返回键退化成系统默认行为（安卓上仍是 goBack / 退出） */
    }
  }, []);

  // 返回键（安卓系统返回 / 侧滑 / 浏览器后退）：栈深 > 1 就退一格；
  // 已在根页面时不拦，这次返回继续交给系统 → 只有根页面才允许退出 App。
  /** 安装进行中（进度页且没出错）：返回键与界面都锁住，装完/出错才放开 */
  const installLockRef = useRef(false);
  useEffect(() => {
    const locked = currentStep === "redeem_installing" && !installError;
    installLockRef.current = locked;
    // 原生侧同时锁：系统返回与侧滑手势在 Android 里不走 popstate，必须让原生吞掉
    setInstallBackLock(locked);
    // 同时保持屏幕常亮：用户放着不动等安装时屏幕会按系统超时熄掉，
    // 息屏后 App 进入 cached 状态、进程可能被冻结，传输停在半路（同一生命周期，一起开一起关）
    setKeepScreenOn(locked);
  }, [currentStep, installError]);
  /** 安装中每次拦下返回都弹一条「安装中，请勿退出」：
   *  原生吞掉返回时会回调这个全局函数（插件/MainActivity 侧 evaluateJavascript），
   *  用户每划一下就看见一条，才知道为什么退不出去。 */
  useEffect(() => {
    const w = window as unknown as { __pfBackBlocked?: () => void };
    w.__pfBackBlocked = () => {
      if (installLockRef.current) showToast("安装中，请勿退出", "error");
    };
    return () => {
      delete w.__pfBackBlocked;
    };
    // showToast 只 setState + 定时器，闭包不依赖任何会变的状态
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);
  /** 管理页每次进入都重拉设备表盘列表（用户可能在手环上删了/换了表盘，旧清单是错的） */
  useEffect(() => {
    if (currentStep === "main" && activeTab === "manage" && isDeviceConnected && !deviceBusy.current) {
      const addr = connected.current.addr;
      if (addr) void refreshDials(addr);
    }
  }, [currentStep, activeTab, isDeviceConnected]);
  useEffect(() => {
    const onPop = () => {
      // 安装中锁死返回键：把历史退回本页（一步也走不了），并告诉用户原因
      if (installLockRef.current) {
        showToast("安装中，请勿退出", "error");
        try {
          window.history.pushState({ panfang: true, route: "install" }, "", "#install");
        } catch {
          /* 无 history 环境：忽略 */
        }
        navDepth.current += 1;
        return;
      }
      if (ignorePop.current > 0) {
        ignorePop.current -= 1; // 应用内返回按钮自己同步历史的那一次
        return;
      }
      // 设备浮窗那一层：**只关浮窗，页面保持不变**（停在本页）
      if (topIsOverlay.current) {
        topIsOverlay.current = false;
        setIsDeviceDrawerOpen(false);
        navDepth.current = Math.max(0, navDepth.current - 1);
        return;
      }
      if (pendingReset.current) {
        const r = pendingReset.current;
        pendingReset.current = null;
        // 重置页面栈的落点条目 = 新的根页面
        try {
          window.history.replaceState({ panfang: true, route: r }, "", `#${r}`);
        } catch {
          /* 忽略 */
        }
        return;
      }
      if (navStack.current.length > 1) {
        navStack.current = navStack.current.slice(0, -1);
        navDepth.current = Math.max(0, navDepth.current - 1);
        applyRoute(currentRoute());
      }
    };
    window.addEventListener("popstate", onPop);
    return () => window.removeEventListener("popstate", onPop);
  }, []);

  /**
   * 启动复位（本轮修复「连接卡死」的关键一条）：**所有瞬时状态一律清零，绝不跨会话继承**。
   * 这些字段本来就不在持久化白名单里（见 PersistedUi），这里再显式复位一遍——哪怕以后有人
   * 往持久化里加了字段、或 WebView 复用了上一次的 JS 内存，界面也不会卡在「正在连接 / 转圈 /
   * 弹层开着」这种状态里（那种状态下点击会被误判成「没反应」）。
   */
  useEffect(() => {
    // 连接相关：令牌、结算标志、超时定时器、忙标志、心跳计数
    connectToken.current = 0;
    connectSettled.current = true;
    if (connectTimer.current !== null) {
      window.clearTimeout(connectTimer.current);
      connectTimer.current = null;
    }
    deviceBusy.current = false;
    heartbeatFailures.current = 0;
    heartbeatInFlight.current = false;
    // 界面态：连接中/已连接、加载中、行内错误、各种浮层
    setConnectingId(null);
    setConnectedId(null);
    setIsDeviceConnected(false);
    setIsDeviceSwitchingAnim(false);
    setConnectError(null);
    setIsDeviceDrawerOpen(false);
    setActiveActionSheetDial(null);
    setUninstallConfirmDial(null);
    setShowLogoutConfirm(false);
    setIsApplying(false);
    setAppliedSuccess(false);
    setIsLoggingIn(false);
    setShowLoginFallback(false);
    setLoginError(null);
    setInstallError(null);
  }, []);

  // 界面态持久化：页面栈 + 恢复界面所需的必要状态（退出 App 再打开回到上次所在页面）
  useEffect(() => {
    writePersistedUi({
      stack: navStack.current,
      profile: userProfile,
      device: selectedDevice,
      dials,
      editingDial
    });
  }, [currentStep, activeTab, userProfile, selectedDevice, dials, editingDial]);

  // 连接心跳：主心跳已经搬到 Rust 连接总管（15s 一次、后台也跑、掉线主动推 wf-link-* 事件）；
  // 这里只留两件事：① 回到前台立刻探一次（用户马上看到真实状态）② 60 秒兜底探一次。
  // 回前台若界面显示未连接 → 请总管复核/重连一次（结果仍走 wf-link-* 事件，界面不自己拍板）。
  useEffect(() => {
    const inv = invoke;
    if (!inv) return;
    const tick = () => {
      if (document.visibilityState === "hidden") return;
      void probeDeviceStatus();
    };
    const timer = window.setInterval(tick, HEARTBEAT_MS);
    const onVisible = () => {
      if (document.visibilityState !== "visible") return;
      tick();
      if (!connectedStateRef.current && connected.current.addr) {
        void inv("device_relink").catch(() => {});
      }
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => {
      window.clearInterval(timer);
      document.removeEventListener("visibilitychange", onVisible);
    };
  }, []);

  // 事件订阅 + 启动时本地凭证免登录（只在 Tauri 里注册；浏览器预览不订阅）
  useEffect(() => {
    const inv = invoke;
    const lis = listen;
    if (!inv || !lis) return;
    let alive = true;
    const unsubs: Array<() => void> = [];
    const track = (u: () => void) => {
      if (alive) unsubs.push(u);
      else u();
    };
    void (async () => {
      // 登录二次验证：verifying / done（带设备列表）/ error
      track(
        await lis("wf-2fa", ev => {
          const p = (ev.payload || {}) as { phase?: string; message?: string; devices?: CloudDevice[] };
          if (p.phase === "verifying") {
            setIsLoggingIn(true);
            setLoginButtonText("正在完成安全验证…");
            return;
          }
          if (p.phase === "done") {
            applyCloudDevices(p.devices);
            setIsLoggingIn(false);
            setLoginButtonText("登录");
            setShowLoginFallback(false);
            setLoginError(null);
            pushRoute("devices");
            return;
          }
          if (p.phase === "error") {
            setIsLoggingIn(false);
            setLoginButtonText("登录");
            setShowLoginFallback(true);
            diagReport("login", "2fa_failed", p.message || "二次验证失败");
            setLoginError(p.message || "验证未完成，请重新验证");
          }
        })
      );
      // 表盘编辑进度：映射到设计稿的进度文案 / 进度条
      track(
        await lis("wf-edit-progress", ev => {
          const p = (ev.payload || {}) as {
            phase?: string;
            detail?: string;
            sent?: number | null;
            total?: number | null;
          };
          let text = p.detail || p.phase || "";
          if (p.total && p.total > 0) {
            const pct = Math.min(100, Math.round(((p.sent || 0) / p.total) * 100));
            setInstallProgress(pct);
            text = `${text} ${pct}%`;
          }
          if (text) setProgressPhase(text);
        })
      );
      // 本地表盘安装进度：Rust 侧 install_file_bytes 推的 SendMassCallbackData
      // （字段是 snake_case：progress 0~1、total_parts / current_part_num 分片号、
      //  actual_data_payload_len 本次分片字节数；语义与兑换流程的假进度完全不同）
      track(
        await lis("wf-install-progress", ev => {
          installBeat.current = Date.now(); // 还在动 → 看门狗别判停滞
          const p = (ev.payload || {}) as {
            progress?: number;
            total_parts?: number;
            current_part_num?: number;
            actual_data_payload_len?: number;
          };
          const ratio = Number(p.progress);
          if (!Number.isFinite(ratio)) return;
          const pct = Math.max(0, Math.min(100, Math.round(ratio * 100)));
          if (pct >= 100) {
            setInstallProgress(100);
            setProgressPhase("安装完成，正在激活...");
            return;
          }
          setInstallProgress(pct);
          setProgressPhase(`正在传输至手环 ${pct}%...`);
        })
      );
      // 付费表盘进度：验证授权 → 下载（按字节）→ 解密 → 传输（交给 wf-install-progress 报百分比）
      track(
        await lis("wf-paid-progress", ev => {
          installBeat.current = Date.now(); // 还在动 → 看门狗别判停滞
          const p = (ev.payload || {}) as {
            phase?: string;
            detail?: string;
            percent?: number | null;
          };
          switch (p.phase) {
            case "auth":
              setProgressPhase(p.detail || "正在验证授权…");
              break;
            case "download": {
              const pct = typeof p.percent === "number" ? p.percent : 0;
              setInstallProgress(pct);
              // detail 里带「第 N/M 个表盘」（拆包版本依次安装），优先用它
              setProgressPhase(p.detail ? `${p.detail} ${pct}%` : `正在下载表盘 ${pct}%`);
              break;
            }
            case "decrypt":
              setProgressPhase(p.detail || "正在准备…");
              break;
            case "transfer":
              setProgressPhase(p.detail || "正在传输至手环…");
              break;
            case "done":
              setInstallProgress(100);
              setProgressPhase(p.detail || "安装完成，正在激活…");
              break;
            case "error":
              setInstallError(p.detail || "安装失败");
              break;
            default:
              if (p.detail) setProgressPhase(p.detail);
          }
        })
      );
      // 掉线（Rust 连接总管判定：通知通道断开 / 发送失败 / 心跳连续失败）：
      // 界面立刻如实（红点 + 电量 --%），长任务立刻失败；重连由总管按退避表自己跑。
      track(
        await lis("wf-link-lost", ev => {
          const p = (ev.payload || {}) as {
            addr?: string;
            reason?: string;
            willRetry?: boolean;
          };
          const addr = p.addr || connected.current.addr;
          if (addr && connected.current.addr && addr !== connected.current.addr) return;
          const firstNotice = !reportedLoss.current;
          reportedLoss.current = true;
          heartbeatFailures.current = 2;
          setIsDeviceConnected(false);
          connectedStateRef.current = false;
          setLinkRetrying(!!p.willRetry);
          if (addr) patchBattery(addr, null);
          fireLinkLost(); // 安装/应用立刻失败，不再干等 25 秒
          if (p.reason) diagReport("connect", "lost_auto", String(p.reason));
          if (firstNotice) {
            showToast(p.willRetry ? "手环连接已断开，正在自动重连…" : "手环连接已断开", "error");
          }
          // 后台里掉的（App 在后台时被系统冻结/断链）→ 提示一次后台保活开关
          if (document.visibilityState === "hidden") maybeShowLinkHelp();
        })
      );
      // 总管正在退避重试（第 N 次）：界面维持「重连中…」
      track(
        await lis("wf-link-retry", () => {
          setLinkRetrying(true);
        })
      );
      // 自动重连成功（或复核发现链路其实还在）→ 恢复如实状态 + 刷新表盘列表
      track(
        await lis("wf-link-restored", ev => {
          const p = (ev.payload || {}) as { addr?: string; name?: string };
          const wasRed = applyLinkConnected(p.addr, null);
          if (wasRed) {
            showToast("已重新连接到手环");
            const a = p.addr || connected.current.addr;
            if (a) void refreshDials(a);
          }
        })
      );
      // 退避跑完仍连不上：给一句能照做的指引（少数情况要手环进「连接新手机」）
      track(
        await lis("wf-link-give-up", ev => {
          const p = (ev.payload || {}) as { addr?: string; reason?: string };
          setLinkRetrying(false);
          connectFailStreak.current = 0;
          if (p.reason) diagReport("connect", "relink_give_up", String(p.reason));
          showToast("无法自动重连，请让手环进入「连接新手机」后重新连接", "error");
        })
      );
      // 保活心跳（Rust 侧每 15s 一次）：刷新电量，顺带确认链路仍真实可用
      track(
        await lis("wf-device-status", ev => {
          const p = (ev.payload || {}) as { addr?: string; battery?: number };
          if (!p.addr || (connected.current.addr && p.addr !== connected.current.addr)) return;
          const wasRed = applyLinkConnected(p.addr, typeof p.battery === "number" ? p.battery : null);
          if (wasRed && p.addr) void refreshDials(p.addr);
        })
      );
      // 启动时用本地凭证后台续期（第三轮 2）：
      // · 成功 → 填设备列表，**保持当前页面不动**；
      // · 失败 → **留在应用内，绝不自动跳登录页**：设备列表走设计稿空态（「账号下暂无设备…」+
      //   「重新导入」），胶囊显示「未连接设备」，再提示一句去设置里重新登录。
      //   登录只做一次：只有用户主动点「重新登录小米账号 / 重新导入设备」才需要再登录。
      try {
        const devices = (await inv("xiaomi_restore")) as CloudDevice[] | null;
        applyCloudDevices(devices);
      } catch (err) {
        console.error("[xiaomi_restore] 本地登录凭证续期失败：", err);
        setMockDevicesList([]); // 空态：不把演示设备当成账号下的设备
        setIsDeviceConnected(false);
        connected.current = { addr: null, model: null, name: null };
        // 提示只说「请登录您的小米账号」：不再指引去「设置」（没登录时根本进不去设置）
        // 已经停在登录页时就不弹了——登录页本身就是那句提示。
        if (currentRoute() !== "login") showToast("请登录您的小米账号", "error");
      }
      // WebView 重建（进程还活着、前台服务保活的连接也还活着）：用后端的**真实状态**重新挂载，
      // 而不是一律当未连接 —— 否则用户会遇到「其实连着，界面却说未连接」。
      try {
        const st = (await inv("device_link_state")) as {
          addr?: string | null;
          name?: string | null;
          connected?: boolean;
          retrying?: boolean;
        } | null;
        if (st?.addr) {
          // 设备行按地址找回（云端设备列表里 mac 匹配；账号列表可能还没回来 → 用后端给的 name 兜底）
          const dev = accountDevices.current.find(d => d.mac === st.addr);
          setSelectedDevice({
            id: st.addr,
            name: st.name || dev?.name || dev?.model || "手环",
            model: dev?.model || "",
            battery: batteryOf.current[st.addr] ?? null
          });
          if (st.connected) {
            connected.current = {
              addr: st.addr,
              model: dev?.model || null,
              name: st.name || dev?.name || null
            };
            setIsDeviceConnected(true);
            connectedStateRef.current = true;
            heartbeatFailures.current = 0;
            void refreshDials(st.addr);
          } else if (st.retrying) {
            // 上次的连接还惦记着但还没连上：界面进「重连中…」，总管继续自己按退避表重试
            connected.current = {
              addr: st.addr,
              model: dev?.model || null,
              name: st.name || dev?.name || null
            };
            setLinkRetrying(true);
          }
        }
      } catch {
        /* 拿不到就当未连接（宁红勿假绿） */
      }
      // 真实版本号（关于页 / 设置页页脚）
      try {
        const v = (await inv("app_version")) as string | null;
        if (v) setAppVersion(String(v));
      } catch {
        /* 拿不到就保持设计稿文案 */
      }
      // 本机安装记录（本地文件，不联网）：管理页把表盘对上资源、标「可更新」用
      void refreshPaidInstalls();
      // 有爱发电登录态时后台问一次「买过的表盘有没有新版本」：首页卡片的角标就靠它
      // （这是本 App 里唯一的「主动提醒」—— 不做系统通知，见 docs/付费资源-进度与待办.md）
      void (async () => {
        try {
          const st = (await inv("paid_status")) as { bound?: boolean; display?: string } | null;
          if (!st?.bound) return;
          setPaidBound(true);
          paidBoundRef.current = true;
          setPaidDisplay(st?.display ?? null);
          await refreshPaidProductsBg();
        } catch {
          /* 静默：没绑/网络差都不该在启动时打扰用户 */
        }
      })();
    })();
    return () => {
      alive = false;
      unsubs.forEach(u => u());
    };
  }, []);

  const handleLogin = (e: React.FormEvent) => {
    e.preventDefault();
    if (!account.trim() || !password.trim() || isLoggingIn) return;
    
    setLoginError(null);
    setShowLoginFallback(false);
    setIsLoggingIn(true);
    setLoginButtonText("正在登录…");
    // 账号资料按输入推导（不编造）：手机号 → 尾号 4 位；邮箱 → @ 前一段
    setUserProfile(profileFromAccount(account.trim()));

    // —— 真实登录（App 内）：xiaomi_login；requires2fa → 自动开验证窗，由 wf-2fa 事件收尾 ——
    if (invoke) {
      const username = account.trim();
      void (async () => {
        try {
          const resp = (await invoke("xiaomi_login", { username, password })) as
            | { requires2fa?: boolean; devices?: CloudDevice[] }
            | null;
          if (resp?.requires2fa) {
            setLoginButtonText("正在完成安全验证…");
            await invoke("xiaomi_2fa_open");
            return; // 收尾交给 wf-2fa（verifying / done / error）
          }
          applyCloudDevices(resp?.devices);
          setIsLoggingIn(false);
          setLoginButtonText("登录");
          pushRoute("devices");
        } catch (err) {
          setIsLoggingIn(false);
          setLoginButtonText("登录");
          diagReport("login", "failed", String(err));
          setLoginError(String(err));
        }
      })();
      return;
    }

    // —— 浏览器演示：以下魔法值与延时保持原样（离线预览用） ——

    // 错误输入演示
    if (account.trim() === "error") {
      setTimeout(() => {
        setIsLoggingIn(false);
        setLoginButtonText("登录");
        setLoginError("账号或密码错误，请重新输入");
      }, 700);
      return;
    }

    if (account.trim() === "network") {
      setTimeout(() => {
        setIsLoggingIn(false);
        setLoginButtonText("登录");
        setLoginError("网络异常，请检查网络后重试");
      }, 700);
      return;
    }

    // 异常兜底演示（输入 fallback 时触发）
    if (account.trim() === "fallback") {
      setTimeout(() => {
        setLoginButtonText("正在完成安全验证…");
        setTimeout(() => {
          setIsLoggingIn(false);
          setLoginButtonText("登录");
          setShowLoginFallback(true);
          setLoginError("验证未完成，请重新验证");
        }, 1200);
      }, 800);
      return;
    }

    // 正常主路径：自动进入「正在完成安全验证…」，然后自动完成跳「选择设备」
    setTimeout(() => {
      setLoginButtonText("正在完成安全验证…");
      setTimeout(() => {
        setIsLoggingIn(false);
        setLoginButtonText("登录");
        pushRoute("devices");
      }, 1000);
    }, 800);
  };

  /**
   * 尽力而为地清掉某个地址上的蓝牙会话（连接前 / 连接失败后各调一次）。
   * 后端 `device_disconnect` → 插件 `BTSpp.disconnect()` 会真的关掉 socket，
   * 这样上一次失败可能留在系统层的半开 SPP 连接不会把下一次连接拖住。
   * 没连过 / 已断开时它会报错，忽略即可。
   */
  const releaseStaleConnection = (addr: string) => {
    const inv = invoke;
    if (!inv || !addr) return;
    inv("device_disconnect", { addr }).catch(() => {
      /* 本来就没连上 → 忽略 */
    });
  };

  /**
   * 主动断开当前设备（用户在设备列表里点「已连接」的那台）。
   * 语义是「我真的不要这条连接了」：
   *  - 后端 `device_disconnect` → 总管 disarm：撤销期望连接、停前台服务、作废在飞的重连
   *    → **不会**再自动重连（用户 2026-10-10 明确要求）；
   *  - 界面立刻回到未连接，只给一句确认，**不报任何「没连上」的错误**（那是给连接失败用的）。
   */
  const handleDisconnectDevice = (device: DeviceItem) => {
    const addr = connected.current.addr || device.id;
    // 作废可能还在飞的那次连接尝试（点了连接又想断开时，别让它回头改界面）
    connectToken.current += 1;
    connectSettled.current = true;
    if (connectTimer.current !== null) {
      clearTimeout(connectTimer.current);
      connectTimer.current = null;
    }
    releaseStaleConnection(addr);
    connected.current = { addr: null, model: null, name: null };
    setIsDeviceConnected(false);
    connectedStateRef.current = false;
    setLinkRetrying(false);
    reportedLoss.current = false;
    heartbeatFailures.current = 0;
    setConnectError(null);
    setFailedDevice(null);
    setDials([]);
    patchBattery(addr, null);
    console.info("[disconnect] 用户主动断开", addr);
    showToast(`已断开「${device.name}」`);
  };

  const handleSelectDevice = (device: DeviceItem) => {
    // —— 连接竞态：这次点击领一个递增令牌，只有令牌还是最新的一次尝试有权改界面 ——
    const token = ++connectToken.current;
    connectSettled.current = false;
    if (connectTimer.current !== null) {
      clearTimeout(connectTimer.current);
      connectTimer.current = null;
    }
    const startedAt = Date.now();
    // 新点击立刻打断旧请求的 UI 状态：转圈换到新目标、清掉旧错误
    setConnectingId(device.id);
    setConnectError(null);
    // 这次要连的地址（真实模式用云端 mac，演示态用 id）
    const cloud = invoke ? accountDevices.current.find(d => d.mac === device.id) : undefined;
    const addr = cloud?.mac || device.id;
    const authkey = cloud?.authkey || "";
    // 连接前先尽力清掉同一个地址上的残留会话：上一次失败可能留下系统层半开 SPP 连接，
    // 会让新连接「不发给手环、直接失败/挂住」。失败无所谓，清理本身不会报错。
    if (invoke) {
      void releaseStaleConnection(addr);
    }

    /** 这次尝试是否仍是最新且未结算 */
    const isLive = () => connectToken.current === token && !connectSettled.current;
    /** 结算这次尝试（成功/失败/超时各只算一次）：过期尝试返回 false，调用方据此整段忽略 */
    const settle = (): boolean => {
      if (!isLive()) return false;
      connectSettled.current = true;
      if (connectTimer.current !== null) {
        clearTimeout(connectTimer.current);
        connectTimer.current = null;
      }
      return true;
    };
    /**
     * 收掉转圈：至少让本次尝试被看见 MIN_SPIN_MS，否则「点不动」和「秒失败」在屏幕上长得一样。
     * 只在这次尝试仍是最新时收（新一轮自己会收，绝不会留死转圈）。
     */
    const hideSpinner = () => {
      const rest = Math.max(0, MIN_SPIN_MS - (Date.now() - startedAt));
      if (rest === 0) {
        setConnectingId(null);
        return;
      }
      window.setTimeout(() => {
        if (connectToken.current === token) setConnectingId(null);
      }, rest);
    };

    // 连接成功后的界面过渡（与演示完全一致：收起抽屉 → 主界面 → 连接高亮 → 提示）
    const finishSwitch = () => {
      if (!settle()) return; // 已被更晚的点击取代 / 已超时 → 这次成功一律忽略
      setConnectingId(null);
      setConnectedId(device.id);
      // 电量要带上**刚测到的真值**：`device` 是点连接前的列表快照（battery 可能还是 null），
      // 直接 setSelectedDevice(device) 会把 patchBattery 刚写进去的电量又盖回去 ——
      // 真机实测就是「显示已连接、电量却是 --%」（2026-10-08）。
      setSelectedDevice({ ...device, battery: batteryOf.current[addr] ?? device.battery ?? null });
      setFailedDevice(null);
      setConnectError(null);
      connectFailStreak.current = 0; // 连上了：连续失败清零
      // 真连上了：绿点 + 已连接（心跳从这里开始维持真实状态）
      setIsDeviceConnected(true);
      connectedStateRef.current = true;
      setLinkRetrying(false);
      heartbeatFailures.current = 0;
      reportedLoss.current = false;

      // 连上后再验两次（2.5s / 5s）：手环被别的 App 占用时握手能过、链路几秒后就死，
      // 这样能把「假绿点」的窗口从 ~30 秒压到几秒；两次都失败才判掉线，
      // 就算误判，下一次心跳探测成功也会自动变回已连接（自愈）。
      window.setTimeout(() => void probeDeviceStatus(), 2500);
      window.setTimeout(() => void probeDeviceStatus(), 5000);

      setTimeout(() => {
        setIsDeviceDrawerOpen(false);
        setIsDeviceSwitchingAnim(true);
        if (currentStep !== "main") {
          // 从「选择设备」连上 → 首页成为新的根页面（返回键从这里就退出 App）
          resetStackTo("main/home");
        }
        showToast(`已连接并同步 ${device.name}`);

        setTimeout(() => {
          setIsDeviceSwitchingAnim(false);
          setConnectedId(null);
        }, 600);
      }, 500);
    };

    // 15 秒没回音 → **先给引导**（不放弃尝试）：手环慢的时候 20 秒也能连上，
    // 所以后台继续试到宽限期（CONNECT_GRACE_MS）才真正判失败。
    connectTimer.current = window.setTimeout(() => {
      if (!isLive()) return;
      hideSpinner();
      console.warn("[device_connect] 15 秒未返回：先给引导，后台继续尝试");
      diagReport("connect", "timeout15s", "连接 15 秒未返回（后台继续试到 45 秒）");
      setConnectError(CONNECT_HINT);
      setFailedDevice(device);
      showToast("连接未完成，请在手环上进入「连接新手机」后重试", "error");
      connectFailStreak.current += 1;
      if (connectFailStreak.current >= 3) {
        connectFailStreak.current = 0;
        setShowConnectHelpPrompt(true);
      }
      connectTimer.current = window.setTimeout(() => {
        if (!settle()) return;
        console.warn("[device_connect] 宽限期结束，判定失败");
        void releaseStaleConnection(addr);
        setIsDeviceConnected(false);
        setLinkRetrying(false); // 这次连接已经判失败：别留着「重连中…」（那个是掉线自动重连的态）
        reportedLoss.current = false;
        // 同上：该地址的会话在尝试开头就被清掉了，失败 = 真没了 → 心跳别再去探它
        if (connected.current.addr === addr) {
          connected.current = { addr: null, model: null, name: null };
        }
      }, CONNECT_GRACE_MS - CONNECT_TIMEOUT_MS);
    }, CONNECT_TIMEOUT_MS);

    // —— 真实连接（App 内）：device_connect（云端设备自带 mac / authkey）——
    if (invoke) {
      void (async () => {
        try {
          const info = (await invoke("device_connect", {
            name: cloud?.name || device.name,
            addr,
            authkey
          })) as { name?: string } | null;
          if (!isLive()) return; // 令牌已过期（又点了别的设备 / 已超时）→ 成功结果也忽略
          // 关键：蓝牙连上不代表握手成功（手环没进「连接新手机」时最常见）→ 用状态查询验证会话
          const cap = await verifySession(addr);
          if (!isLive()) return;
          if (cap === null) {
            throw new Error("session-not-ready");
          }
          connected.current = { addr, model: cloud?.model || null, name: info?.name || device.name };
          patchBattery(addr, cap);
          finishSwitch();
          // 表盘列表随后台刷新（失败只进 console）
          setTimeout(() => {
            if (connectToken.current !== token) return; // 期间又切了设备 → 别拿旧设备的数据刷界面
            void refreshDials(addr);
          }, 1500);
        } catch (err) {
          if (!settle()) return; // 过期尝试的失败一律忽略（不打断当前目标）
          // 技术细节（真实错误对象 / 后端那串 ①②③ 与 MAC）只进 console，不上屏
          console.error("[device_connect] 连接失败：", err);
          diagReport("connect", "failed", String(err));
          hideSpinner();
          setConnectError(CONNECT_HINT);
          setFailedDevice(device);
          // 抽屉可能是被自动收起的（假成功路径），这里再明确提示一次
          showToast("连接未完成，请在手环上进入「连接新手机」后重试", "error");
          connectFailStreak.current += 1;
          if (connectFailStreak.current >= 3) {
            connectFailStreak.current = 0;
            setShowConnectHelpPrompt(true);
          }
          // 失败后再清一次这条地址上的残留会话，避免下一次连接被系统层半开连接拖住
          void releaseStaleConnection(addr);
          setIsDeviceConnected(false);
          setLinkRetrying(false); // 连接失败 → 不显示「重连中…」（那是掉线自动重连的态）
          reportedLoss.current = false;
          // 这次尝试开头就把该地址上的会话清掉了：连接失败 = 它真的没了。
          // 必须把 addr 也清掉 —— 不然心跳会一直探一台已经断开的设备，
          // 反复判「掉线」并要总管重连，而总管手里没有「要保住的连接」→ 界面卡在「重连中…」。
          if (connected.current.addr === addr) {
            connected.current = { addr: null, model: null, name: null };
          }
        }
      })();
      return;
    }

    // —— 浏览器演示：延时后视为连接成功（同样受令牌保护：连点两台以最后一次为准）——
    setTimeout(() => {
      finishSwitch();
    }, 700);
  };

  const handleOpenDialEdit = (dial: InstalledDial) => {
    // F：只支持编辑本 APP 装过的表盘（设备上的表盘无法回读，没有留档就没有可编辑的图层模型）
    if (invoke && !archivedIds.current[dial.id]) {
      showToast("仅支持编辑通过本 APP 安装的表盘", "error");
      return;
    }
    setEditingDial(dial);
    // 选项先回到设计稿的演示数组；连了设备就从留档 .bin 的图层模型换成真实候选
    setDialSessionState("idle");
    setStyleOptions(LARGE_STYLES);
    setColorOptions(LARGE_COLORS);
    setSelectedStyle(LARGE_STYLES[0]);
    setSelectedColorName(LARGE_COLORS[0].name);
    setContainers(DEMO_CONTAINERS);
    setOpenSection(null);
    setCurrentWallpaperIndex(0);
    setIsApplying(false);
    setAppliedSuccess(false);
    setRenderPreviewUrl(null);
    setDialScreens({ dial: null, device: null });
    binSession.current = null;
    dialOptions.current = null;
    if (invoke) {
      // 真实模式：壁纸库按表盘从本地相册存档载入（先清空，避免带入上一个表盘的照片）
      setWallpapers([]);
      wallpaperMeta.current = [];
    }
    pushRoute("dial-edit"); // 进编辑页 = 压一层页面栈
    void loadDialSession(dial);
  };

  const toggleSection = (section: AccordionSection) => {
    setOpenSection(prev => {
      const next = prev === section ? null : section;
      if (next) {
        // 展开后把这张卡滚到滚动区顶部：靠底部的卡片展开后选项会被固定的「应用」栏
        // 盖住（点样式2/3 没反应的元凶），先滚上来再点
        window.setTimeout(() => {
          document.getElementById(`acc-${next}`)?.scrollIntoView({ behavior: "smooth", block: "start" });
        }, 60);
      }
      return next;
    });
  };

  /** 选中某张容器卡片的第 i 个候选：**身份记动作序号**（下载到设备就靠它），文案只用于显示。
   *  卡片按「槽位 id」（演示卡按槽位名）定位而不是可见列表下标——样式过滤后可见下标 ≠ 全量容器下标，
   *  按下标写会改到别的（被过滤隐藏的）卡片，看起来就是「点了没反应」。 */
  const selectContainerAt = (slot: number | null, label: string, optionIndex: number) => {
    setContainers(prev =>
      prev.map(c => {
        const hit = slot == null ? c.slot == null && c.label === label : c.slot === slot;
        if (!hit) return c;
        return {
          ...c,
          selected: c.options[optionIndex] ?? c.selected,
          selectedId: c.ids[optionIndex] ?? null,
          touched: true
        };
      })
    );
  };

  // 添加新照片：App 内走系统选图（读成 data URL），浏览器演示用本地素材
  /** 删除第 idx 张壁纸：列表与元数据同步移除，选中索引收到合法范围，相册存档即时更新 */
  const handleRemoveWallpaper = (idx: number) => {
    const nextList = wallpapers.filter((_, i) => i !== idx);
    wallpaperMeta.current = wallpaperMeta.current.filter((_, i) => i !== idx);
    setWallpapers(nextList);
    setCurrentWallpaperIndex(prev => {
      let next = prev > idx ? prev - 1 : prev;
      return Math.max(0, Math.min(next, nextList.length - 1));
    });
    // 相册存档立即重写（下次进编辑页按这份载入）
    const session = binSession.current;
    if (invoke && session) {
      void invoke("watchface_photos_save", {
        faceId: session.faceId,
        photos: wallpaperMeta.current.map(p => ({ name: p.name, b64: p.b64, crop: p.crop }))
      }).catch(() => {});
    }
    showToast("已删除壁纸");
  };

  const handleAddWallpaper = () => {
    if (invoke) {
      openPhotoPicker();
      return;
    }
    const newPhotos = [INITIAL_WALLPAPERS[0], INITIAL_WALLPAPERS[3], INITIAL_WALLPAPERS[1], INITIAL_WALLPAPERS[2]];
    const picked = newPhotos[wallpapers.length % newPhotos.length];
    setPendingCropImage(picked);
    setCropOffset({ x: 0, y: 0 });
    setCropRotation(0);
    setCropScale(1);
    pushRoute("crop"); // 进裁剪页 = 压一层页面栈（返回/取消回编辑页）
  };

  /**
   * 系统选图（安卓 content:// 与桌面通用）：文件字节 → base64 缓存 → data URL 进裁剪页。
   * 用临时 input 触发，不改动任何界面结构。
   */
  const openPhotoPicker = () => {
    const input = document.createElement("input");
    input.type = "file";
    input.accept = "image/*";
    input.style.display = "none";
    document.body.appendChild(input);
    input.addEventListener("change", () => {
      const file = input.files && input.files[0];
      input.remove();
      if (!file) return;
      void (async () => {
        try {
          const u8 = new Uint8Array(await file.arrayBuffer());
          const b64 = bytesToB64(u8);
          const mime = sniffImageMime(u8) || file.type || "image/png";
          pendingPhoto.current = { b64, mime, name: file.name || "照片" };
          setPendingCropImage(b64ToDataUrl(b64, mime));
          setCropOffset({ x: 0, y: 0 });
          setCropRotation(0);
          setCropScale(1);
          pushRoute("crop"); // 进裁剪页 = 压一层页面栈
        } catch (err) {
          showToast(humanize(err, "crop_open"), "error");
        }
      })();
    });
    input.click();
  };

  // ================= 付费表盘：绑定 → 我的表盘 → 安装 =================

  /** 进列表页（从绑定页过来时把栈顶改写成列表页，返回键直接回首页） */
  const goToList = () => {
    if (currentRoute() === "paid") {
      navStack.current = [...navStack.current.slice(0, -1), "paid/dials"];
      applyRoute("paid/dials");
    } else {
      pushRoute("paid/dials");
    }
    void loadPaidList();
  };

  /** 绑定方式与店铺链接（绑定页主按钮 + 空态「去爱发电看看」用） */
  const loadPaidMode = async () => {
    const inv = invoke;
    if (!inv) return;
    try {
      const m = (await inv("paid_bind_mode")) as { mode?: string; shop_url?: string } | null;
      setPaidShopUrl(m?.shop_url ?? null);
      const mode = m?.mode === "oauth" || m?.mode === "dev" ? m.mode : "none";
      setPaidMode(mode);
    } catch {
      /* 拿不到就不显示空态跳转 */
    }
  };

  /** 首页主卡入口：已绑定 → 我的表盘；未绑定 → 绑定页 */
  const openPaidFlow = () => {
    const inv = invoke;
    if (!inv) {
      showToast("安装已购表盘：真机可用");
      return;
    }
    setPaidError(null);
    void (async () => {
      // 跳转判据用**这次刚读到的值**：setPaidBound 是异步的，紧接着读 paidBoundRef
      // 拿到的是上一帧的旧值 —— 已绑定的用户会被送进「绑定爱发电」页（用户 2026-10-08 报的现象）。
      // 要求是：有爱发电登录态就直接进「我的表盘」，那张绑定页只在第一次绑定时出现。
      let bound = paidBoundRef.current;
      try {
        const st = (await inv("paid_status")) as { bound?: boolean; display?: string } | null;
        bound = !!st?.bound;
        setPaidBound(bound);
        setPaidDisplay(st?.display ?? null);
      } catch {
        /* 状态读不到：按当前界面态走，别把已绑定的人挡在门外 */
      }
      void loadPaidMode();
      if (bound) {
        pushRoute("paid/dials");
        void loadPaidList();
      } else {
        pushRoute("paid");
      }
    })();
  };
  /** 绑定态的最新值（openPaidFlow 里要读，避免闭包拿到旧 state） */
  const paidBoundRef = useRef(false);
  paidBoundRef.current = paidBound;

  /** 停掉等待授权的轮询（离开绑定页 / 成功 / 超时都走这里） */
  const stopPaidOauthPoll = () => {
    paidOauthState.current = null;
    if (paidOauthTimer.current !== null) {
      window.clearInterval(paidOauthTimer.current);
      paidOauthTimer.current = null;
    }
  };

  /**
   * 正式绑定：打开爱发电授权页 → 用户授权 → 服务端收回调 → 这里轮询取结果。
   * 用户整个过程中在本机浏览器里，App 只是等着，所以每 2 秒问一次，最长 3 分钟。
   */
  const handlePaidOauth = () => {
    const inv = invoke;
    if (!inv) return;
    setPaidBusy(true);
    setPaidError(null);
    void (async () => {
      try {
        const r = (await inv("paid_bind_oauth_start", {
          deviceName: connected.current.name || null
        })) as { state?: string; url?: string } | null;
        const st = r?.state ?? "";
        const url = r?.url ?? "";
        if (!st || !url) throw new Error("暂时无法发起爱发电授权，请稍后再试");
        paidOauthState.current = st;
        await inv("open_external", { url });
        showToast("请在浏览器里完成授权");
        const startedAt = Date.now();
        stopPaidOauthPoll();
        paidOauthState.current = st;
        paidOauthTimer.current = window.setInterval(() => {
          void (async () => {
            const cur = paidOauthState.current;
            if (!cur) return;
            if (Date.now() - startedAt > 3 * 60 * 1000) {
              stopPaidOauthPoll();
              setPaidBusy(false);
              setPaidError("授权超时，请重新点一次");
              return;
            }
            try {
              const p = (await inv("paid_bind_oauth_poll", { stateKey: cur })) as {
                status?: string;
                display?: string;
                message?: string;
              } | null;
              if (p?.status === "ok") {
                stopPaidOauthPoll();
                setPaidBound(true);
                paidBoundRef.current = true;
                setPaidDisplay(p?.display ?? null);
                setPaidBusy(false);
                showToast("已绑定");
                goToList();
              } else if (p?.status === "error") {
                stopPaidOauthPoll();
                setPaidBusy(false);
                setPaidError("授权没成功，请重试");
              }
            } catch {
              /* 网络抖动：下一次轮询再说，别打断用户 */
            }
          })();
        }, 2000);
      } catch (err) {
        stopPaidOauthPoll();
        setPaidBusy(false);
        setPaidError(humanize(err, "bind"));
      }
    })();
  };

  /** 绑定爱发电（联调模式：直接用已登录的小米账号，不跳浏览器） */
  const handlePaidBind = () => {
    const inv = invoke;
    if (!inv) return;
    setPaidBusy(true);
    setPaidError(null);
    void (async () => {
      try {
        const r = (await inv("paid_bind", {
          miUserId: "",
          deviceName: connected.current.name || null
        })) as { bound?: boolean; display?: string } | null;
        setPaidBound(!!r?.bound);
        paidBoundRef.current = !!r?.bound;
        setPaidDisplay(r?.display ?? null);
        showToast("已绑定");
        goToList();
      } catch (err) {
        setPaidError(humanize(err, "paid_list"));
      } finally {
        setPaidBusy(false);
      }
    })();
  };

  /** 绑定页主按钮：正式（oauth）走授权页，联调（dev）直接绑 */
  const handlePaidPrimary = () => {
    if (paidMode === "oauth") {
      handlePaidOauth();
      return;
    }
    handlePaidBind();
  };

  /**
   * 进绑定页统一走这里：先把绑定方式读回来，否则按钮会停在「暂不可用」。
   * replace = 就地替换当前页 —— 从「爱发电账号」页去换号时用，这样返回键直接回设置页
   *（否则会退回刚才那张已经不成立的账号页）。
   */
  const goPaidBindPage = (replace = false) => {
    setPaidError(null);
    void loadPaidMode();
    if (replace && currentRoute() === "paid/account") {
      navStack.current = [...navStack.current.slice(0, -1), "paid"];
      applyRoute("paid");
      return;
    }
    pushRoute("paid");
  };

  /**
   * 退出爱发电绑定（设置 → 爱发电账号）：
   * · logout —— 只退出（回设置页，账号管理项会显示「未绑定」）；
   * · switch —— 换账号：退完直接去授权页，让用户用新的爱发电账号登录。
   */
  const doPaidUnbind = (mode: "logout" | "switch") => {
    setPaidAccountConfirm(null);
    const inv = invoke;
    void (async () => {
      if (inv) {
        try {
          await inv("paid_unbind");
        } catch (err) {
          showToast(humanize(err, "bind"), "error");
          return;
        }
      }
      // 本机界面态一并清掉：已购列表与绑定态都不再属于这个账号
      setPaidBound(false);
      paidBoundRef.current = false;
      setPaidDisplay(null);
      setPaidProducts([]);
      setPaidLoaded(false);
      setPaidError(null);
      if (mode === "switch") {
        goPaidBindPage(true);
        showToast("请用新的爱发电账号登录");
        return;
      }
      showToast("已退出爱发电账号");
      goBackOr("main/settings");
    })();
  };

  // 离开绑定页就别再轮询了（成功/超时各自会停）
  useEffect(() => {
    if (currentStep !== "paid_bind") stopPaidOauthPoll();
  }, [currentStep]);

  /**
   * 已有爱发电登录态时**不进「绑定爱发电」页**（用户要求：这张页只在第一次绑定完成后出现，
   * 之后只要 App 里还登录着就直接进「我的表盘」；账号管理去设置里做）。
   * 兜住「上次退出时停在绑定页」的重启恢复这类边角入口。
   */
  useEffect(() => {
    if (currentStep !== "paid_bind" || !invoke) return;
    let alive = true;
    void (async () => {
      try {
        const st = (await invoke("paid_status")) as { bound?: boolean; display?: string } | null;
        if (!alive || !st?.bound) return;
        setPaidBound(true);
        paidBoundRef.current = true;
        setPaidDisplay(st?.display ?? null);
        showToast("已绑定");
        goToList();
      } catch {
        /* 读不到状态就照常显示绑定页（用户还能自己点一次绑定） */
      }
    })();
    return () => {
      alive = false;
    };
  }, [currentStep]);

  /** 设置页要显示「爱发电账号」这一项：切到设置时同步一次绑定态（本机状态，秒回） */
  useEffect(() => {
    if (!invoke || currentStep !== "main" || activeTab !== "settings") return;
    void (async () => {
      try {
        const st = (await invoke("paid_status")) as { bound?: boolean; display?: string } | null;
        setPaidBound(!!st?.bound);
        paidBoundRef.current = !!st?.bound;
        setPaidDisplay(st?.display ?? null);
      } catch {
        /* 读不到就保持现有显示 */
      }
    })();
  }, [currentStep, activeTab]);

  /** 我的表盘：拉已购资源 + 判断本机设备屏幕 */
  const loadPaidList = async () => {
    const inv = invoke;
    if (!inv) return;
    setPaidError(null);
    setPaidLoaded(false);
    try {
      if (!paidScreen) {
        try {
          const s = (await inv("paid_device_screen", {
            model: connected.current.model || ""
          })) as { screen?: [number, number] | null } | null;
          if (s?.screen) setPaidScreen(s.screen);
        } catch {
          /* 型号认不出：列表按「可安装」展示，卡片上写明适配机型 */
        }
      }
      const d = (await inv("paid_list")) as { products?: PaidProduct[] } | null;
      setPaidProducts(d?.products ?? []);
      setPaidLoaded(true);
    } catch (err) {
      const msg = String(err);
      if (msg.includes("NOT_BOUND")) {
        setPaidBound(false);
        paidBoundRef.current = false;
        setPaidProducts([]);
        setPaidLoaded(true);
        return;
      }
      setPaidError(msg);
      setPaidLoaded(true);
    }
  };

  /**
   * 后台刷一次已购列表（首页角标要它）：失败静默 —— 这里只是提前量，
   * 真正打开「我的表盘」时 loadPaidList 会把错误如实显示出来。
   */
  const refreshPaidProductsBg = async () => {
    const inv = invoke;
    if (!inv) return;
    try {
      const d = (await inv("paid_list")) as { products?: PaidProduct[] } | null;
      setPaidProducts(d?.products ?? []);
    } catch {
      /* 静默 */
    }
  };

  /** 读本机安装记录（本地文件，每次安装后都会变）—— 管理页标「可更新」靠它 */
  const refreshPaidInstalls = async () => {
    const inv = invoke;
    if (!inv) return;
    try {
      const r = (await inv("paid_installs_get")) as PaidInstallRecord[] | null;
      setPaidInstalls(Array.isArray(r) ? r : []);
    } catch {
      /* 静默：读不到就当作「都没装过」，宁可不标也不乱标 */
    }
  };

  /**
   * 「设备上的表盘」里属于**有新版本的已购资源**的那些：表盘 id → 该资源与设备行。
   * 数据来源 = 本机安装记录（表盘 id）+ 已购列表的 has_update（服务端指纹与本机记录比出来的）。
   * 点这个标记会直接更新（复用安装通道，连接/型号的防呆都在 runPaidInstall 里）。
   */
  const paidUpdateDials = (() => {
    const map = new Map<string, { product: PaidProduct; variant: PaidVariant }>();
    for (const p of paidProducts) {
      for (const v of p.variants) {
        if (!v.has_update) continue;
        for (const r of paidInstalls) {
          if (r.product_id !== p.product_id || r.variant_id !== v.variant_id) continue;
          for (const id of r.dials) map.set(id, { product: p, variant: v });
        }
      }
    }
    return map;
  })();

  /** 这个资源里能装哪个设备行（按本机屏幕匹配；认不出屏幕时取第一行） */
  const matchPaidVariant = (p: PaidProduct): { variant: PaidVariant | null; screenKnown: boolean } => {
    if (paidScreen) {
      const hit = p.variants.find(
        v => v.screen?.[0] === paidScreen[0] && v.screen?.[1] === paidScreen[1]
      );
      return { variant: hit ?? null, screenKnown: true };
    }
    return { variant: p.variants[0] ?? null, screenKnown: false };
  };

  /** 点安装：先读该版本的包体列表 —— 多包体先弹「选择要安装的表盘」勾选（表盘1..N，默认全选），单包体直接装 */
  const runPaidInstall = (product: PaidProduct, variant: PaidVariant) => {
    const inv = invoke;
    if (!inv) return;
    const addr = connected.current.addr;
    if (!isDeviceConnected || !addr) {
      showToast("请先连接手环设备", "error");
      return;
    }
    // 型号防呆（防御性兜底）：设备行屏幕 ≠ 本机屏幕就拦下
    // （列表页本来就按本机屏幕过滤，这里是防止误点了别的型号的行）
    const vs = variant.screen;
    if (vs?.[0] && vs[1] && paidScreen && (vs[0] !== paidScreen[0] || vs[1] !== paidScreen[1])) {
      showToast("该表盘不适配你的设备，请选择正确型号的表盘后再试。", "error");
      return;
    }
    if (installBusyRef.current) return;
    installBusyRef.current = true;
    setInstallPreparing(product.product_id);
    void (async () => {
      try {
        const info = (await inv("paid_latest_file", {
          productId: product.product_id,
          variantId: variant.variant_id
        })) as { files?: PaidFile[] } | null;
        const files = (info?.files ?? []).filter((f): f is PaidFile => !!f && !!f.file_id);
        if (files.length === 0) throw new Error("该设备暂无可用包体");
        if (files.length === 1) {
          startPaidInstall(product, variant, null);
          return;
        }
        // 多包体：弹勾选列表（默认全选），确认后才进安装页（installBusyRef 到确认/取消才放开）
        setInstallPicker({ product, variant, files, picked: files.map(() => true) });
      } catch (err) {
        installBusyRef.current = false;
        showToast(humanize(err, "install_paid"), "error");
      } finally {
        setInstallPreparing(null);
      }
    })();
  };

  /** 勾选浮层：点一行切换勾选；「全选」= 全勾↔全不勾（部分勾选时显示横杠） */
  const toggleInstallPick = (idx: number) =>
    setInstallPicker(p => (p ? { ...p, picked: p.picked.map((on, i) => (i === idx ? !on : on)) } : p));
  const toggleInstallPickAll = () =>
    setInstallPicker(p => {
      if (!p) return p;
      const allOn = p.picked.every(Boolean);
      return { ...p, picked: p.picked.map(() => !allOn) };
    });
  const cancelInstallPicker = () => {
    setInstallPicker(null);
    installBusyRef.current = false;
  };
  const confirmInstallPicker = () => {
    const p = installPicker;
    if (!p) return;
    const ids = p.files.filter((_, i) => p.picked[i]).map(f => f.file_id);
    if (ids.length === 0) return; // 按钮置灰兜底：一个都不选不给装
    setInstallPicker(null);
    startPaidInstall(p.product, p.variant, ids);
  };

  /** 安装：下载 → 解密（内存）→ 传输 → 自动设为当前表盘（fileIds=null = 整组装；重试沿用上次勾的包体） */
  const startPaidInstall = (product: PaidProduct, variant: PaidVariant, fileIds: string[] | null) => {
    const inv = invoke;
    if (!inv) return;
    const addr = connected.current.addr;
    if (!isDeviceConnected || !addr) {
      installBusyRef.current = false;
      showToast("请先连接手环设备", "error");
      return;
    }
    installBusyRef.current = true;
    paidTarget.current = { product, variant, fileIds };
    setInstallSource("paid");
    setDialName(product.name);
    setInstallError(null);
    setInstallProgress(2);
    setProgressPhase("正在验证授权…");
    if (currentRoute() === "install") setCurrentStep("redeem_installing");
    else pushRoute("install");
    deviceBusy.current = true;
    installBeat.current = Date.now();
    void (async () => {
      try {
        const installedId = (await withStallGuard(
          linkLostRace(
            inv("paid_install", {
              addr,
              productId: product.product_id,
              variantId: variant.variant_id,
              deviceName: connected.current.name || null,
              fileIds
            })
          )
        )) as string | null;
        setInstallProgress(100);
        setProgressPhase("安装完成，正在激活...");
        // 装完自动设为当前表盘（失败不打断：表盘已经装上了）
        if (installedId) {
          try {
            await inv("watchface_set_current", { addr, watchfaceId: installedId });
          } catch {
            /* 忽略：用户也可以在管理页手动设为当前 */
          }
        }
        await refreshDials(addr);
        // 刚才装的就是这个设备行的最新版 → 就地清掉「可更新」标记（角标立刻消失），
        // 再后台向服务端核对一次（App 侧记的指纹与服务端 latest_sha 必须一致）
        setPaidProducts(prev => prev.map(pp => pp.product_id !== product.product_id ? pp : {
          ...pp,
          variants: pp.variants.map(vv => vv.variant_id !== variant.variant_id ? vv : {
            ...vv,
            installed: true,
            installed_version: variant.latest_version ?? vv.installed_version ?? null,
            has_update: false
          })
        }));
        void refreshPaidProductsBg();
        void refreshPaidInstalls();
        setCurrentStep("redeem_success");
        setIsDeviceConnected(true);
        heartbeatFailures.current = 0;
      } catch (err) {
        setInstallError(humanize(err, "install_paid"));
        noteLinkDeadIfStalled(err, addr);
      } finally {
        deviceBusy.current = false;
        installBusyRef.current = false;
      }
    })();
  };

  /**
   * 本地 .bin / .mwz / .rpk 安装：整段进度 UI 复用兑换流程那套页面
   * （进度条 + 阶段文案 + 成功态，见 currentStep === "redeem_installing" / "redeem_success"），
   * 所以只切页面状态、不另造 UI。失败留在同一页给行内错误 + 「重试安装」（用同一份字节重试）。
   */
  const runLocalInstall = (payload: { addr: string; b64: string; ext: string; name: string }) => {
    const inv = invoke;
    if (!inv) return;
    localInstall.current = { addr: payload.addr, b64: payload.b64, ext: payload.ext };
    setInstallSource("local");
    setDialName(payload.name);
    setInstallError(null);
    setInstallProgress(6);
    setProgressPhase("正在校验表盘文件...");
    // 进安装进度页（与兑换流程同一套页面；返回键可退回进来之前那一页）
    if (currentRoute() === "install") setCurrentStep("redeem_installing");
    else pushRoute("install");
    deviceBusy.current = true; // 传输期间不探连接状态（忙 ≠ 掉线）
    installBeat.current = Date.now();
    void (async () => {
      try {
        // 型号防呆：文件画布 ≠ 本机屏幕 → 直接阻断
        // （例：小米手环10 Pro（336×480）不能装手环10（212×520）或红米手表5（432×514）的表盘）
        // 只识别 .bin；.mwz/.rpk 是容器格式，这里不解析（交给安装流程）。
        if (payload.ext === "bin") {
          try {
            const info = (await inv("bin_inspect_info", { dataB64: payload.b64 })) as {
              canvas?: [number, number] | null;
            } | null;
            let dev: [number, number] | null = paidScreen;
            if (!dev) {
              const s = (await inv("paid_device_screen", {
                model: connected.current.model || ""
              })) as { screen?: [number, number] | null } | null;
              dev = s?.screen ?? null;
            }
            const c = info?.canvas ?? null;
            if (c && dev && (c[0] !== dev[0] || c[1] !== dev[1])) {
              // 屏幕上一句话，尺寸细节只进 console（铁律：报错只给操作指引）
              console.error(
                `[model] 画布 ${c[0]}×${c[1]} ≠ 设备「${connected.current.name || "手环"}」${dev[0]}×${dev[1]}`
              );
              setInstallError("该表盘不适配你的设备，请选择正确型号的表盘后再试。");
              deviceBusy.current = false;
              return;
            }
          } catch {
            /* 画布认不出来就不拦，交给安装流程报错 */
          }
        }
        const msg = (await withStallGuard(
          linkLostRace(
            inv("device_install_payload", {
              addr: payload.addr,
              dataB64: payload.b64,
              ext: payload.ext
            })
          )
        )) as string | null;
        setInstallProgress(100);
        setProgressPhase("安装完成，正在激活...");
        // 装完刷新表盘列表（首页/管理页跟着更新；新装的表盘也会有留档 → 可编辑）
        await refreshDials(payload.addr);
        setCurrentStep("redeem_success");
        setIsDeviceConnected(true);
        heartbeatFailures.current = 0;
        if (msg) showToast(msg);
      } catch (err) {
        setInstallError(humanize(err, "install_local"));
        noteLinkDeadIfStalled(err, payload.addr);
      } finally {
        deviceBusy.current = false;
      }
    })();
  };

  /**
   * 首页「本地表盘安装」：选 .bin / .mwz / .rpk → device_install_payload（字节直传，安卓通用）。
   * 真机走上面的安装进度页；浏览器演示没有真实链路，保持原来的提示文案。
   */
  const handleLocalInstall = () => {
    const inv = invoke;
    if (!inv) {
      showToast("已打开 .bin 表盘文件选择器");
      return;
    }
    const addr = connected.current.addr;
    // 未连接（用心跳维持的真实状态判断，不认残留的旧地址）→ 直接浮窗提示，不进安装流程
    if (!isDeviceConnected || !addr) {
      showToast("请先连接手环设备", "error");
      return;
    }
    const input = document.createElement("input");
    input.type = "file";
    input.accept = ".bin,.mwz,.rpk,application/octet-stream";
    input.style.display = "none";
    document.body.appendChild(input);
    input.addEventListener("change", () => {
      const file = input.files && input.files[0];
      input.remove();
      if (!file) return;
      void (async () => {
        try {
          const u8 = new Uint8Array(await file.arrayBuffer());
          const ext = (file.name.split(".").pop() || "").toLowerCase();
          // 进度页标题用文件名（去掉扩展名），与兑换流程显示表盘名同一个位置
          const name = (file.name || "").replace(/\.[^.]+$/, "") || "本地表盘";
          runLocalInstall({ addr, b64: bytesToB64(u8), ext, name });
        } catch (err) {
          showToast(humanize(err, "pick_file"), "error");
        }
      })();
    });
    input.click();
  };

  /** 裁剪页「完成」：新照片进壁纸列表（真实模式同时记录裁剪变换，应用时合成用） */
  const handleCropConfirm = () => {
    if (pendingCropImage) {
      const session = binSession.current;
      const photo = pendingPhoto.current;
      const applied =
        invoke && session?.model
          ? cropToTransform(
              { x: cropOffset.x, y: cropOffset.y, scale: cropScale, rotation: cropRotation },
              session.model.width,
              cropFrame.w
            )
          : null;
      const b64 = photo?.b64 || dataUrlToB64(pendingCropImage) || "";
      setWallpapers(prev => [pendingCropImage, ...prev]);
      wallpaperMeta.current = [
        { b64, crop: applied, name: photo?.name || "照片" },
        ...wallpaperMeta.current
      ];
      setCurrentWallpaperIndex(0);
      if (invoke && session) void renderDialPreview({ wallIdx: 0 });
    }
    pendingPhoto.current = null;
    goBackOr("main/manage"); // 裁剪页是编辑页压出来的 → 返回编辑页
    showToast("已保存裁剪壁纸");
  };

  // ---------------------- 裁剪预览：canvas 2D 绘制 + 真机防护 ----------------------
  // 背景：安卓（尤其 vivo）WebView 上 `<img>` + CSS transform 会黑屏、位图滚出视野会被回收，
  // 所以照搬 legacy `app/ui/index.legacy2.html` 的 canvas 2D 画法，并带上三条防护：
  //   ① cropSourceDead → rebuildCropBitmap（清缓存后重新解码）
  //   ② cropCanvasLooksEmpty → ensureCropVisible（滚动/回到前台/重新进入视野时检查）
  //   ③ drawImage 包 try/catch（回收时不报错也不出图，只能按结果兜底）

  /** 位图尺寸（ImageBitmap 用 width，<img> 用 naturalWidth） */
  const bitmapSize = (src: CanvasImageSource | null) => {
    if (!src) return { w: 0, h: 0 };
    const s = src as { width?: number; naturalWidth?: number; height?: number; naturalHeight?: number };
    return { w: Number(s.naturalWidth) || Number(s.width) || 0, h: Number(s.naturalHeight) || Number(s.height) || 0 };
  };

  /** 位图是否已被 WebView 回收（被 close 的 ImageBitmap / 解码丢失的 <img> → 尺寸为 0） */
  const cropSourceDead = (src: CanvasImageSource | null): boolean => {
    const { w, h } = bitmapSize(src);
    return !w || !h;
  };

  /** 待裁剪图片 → Blob（App 内用选定文件的字节；浏览器演示用本地素材 URL） */
  const cropPhotoBlob = async (): Promise<Blob | null> => {
    const photo = pendingPhoto.current;
    if (photo?.b64) {
      try {
        const u8 = b64ToBytes(photo.b64);
        // 复制成独立的 ArrayBuffer，避开 TS 的 SharedArrayBuffer 宽化
        const buf = new ArrayBuffer(u8.byteLength);
        new Uint8Array(buf).set(u8);
        return new Blob([buf], { type: photo.mime || "image/png" });
      } catch {
        /* 落到 URL 分支 */
      }
    }
    if (pendingCropImage) {
      try {
        return await (await fetch(pendingCropImage)).blob();
      } catch {
        return null;
      }
    }
    return null;
  };

  /** 位图解码：优先 createImageBitmap，回退 <img> 解码 */
  const decodeBitmapFromBlob = async (blob: Blob): Promise<CanvasImageSource | null> => {
    try {
      return await createImageBitmap(blob);
    } catch {
      /* 回退到 <img> */
    }
    return await new Promise((resolve) => {
      const im = new Image();
      const url = URL.createObjectURL(blob);
      im.onload = () => {
        URL.revokeObjectURL(url);
        resolve(im);
      };
      im.onerror = () => {
        URL.revokeObjectURL(url);
        resolve(null);
      };
      im.src = url;
    });
  };

  /** 载入当前待裁剪图片的位图（命中缓存直接复用；缓存里的已死则丢弃重解码），完成后重绘 */
  const loadCropBitmap = async () => {
    if (cropDecoding.current) return;
    cropDecoding.current = true;
    try {
      const key = pendingCropImage;
      let src = (key ? cropBitmapCache.current.get(key) : undefined) ?? null;
      if (src && cropSourceDead(src)) {
        cropBitmapCache.current.delete(key);
        src = null;
      }
      if (!src) {
        const blob = await cropPhotoBlob();
        if (blob) src = await decodeBitmapFromBlob(blob);
        if (src && key) cropBitmapCache.current.set(key, src);
      }
      cropBitmap.current = src;
      drawCrop();
    } finally {
      cropDecoding.current = false;
    }
  };

  /** 丢掉缓存的位图并重新解码（画布空了 / 位图被回收时调） */
  const rebuildCropBitmap = async (_reason: string) => {
    const key = pendingCropImage;
    if (key) cropBitmapCache.current.delete(key);
    cropBitmap.current = null;
    await loadCropBitmap();
  };

  /** 裁剪预览：canvas 2D 绘制（与后端 draw_photo 同一套变换：cover 基准 × 缩放 + 平移 + 旋转） */
  const drawCrop = () => {
    const cvs = cropCanvasRef.current;
    if (!cvs) return;
    const src = cropBitmap.current;
    if (!src) {
      // 位图还没解码出来 → 触发一次解码（解完会自己重绘）
      if (pendingCropImage || pendingPhoto.current) void loadCropBitmap();
      return;
    }
    if (cropSourceDead(src)) {
      void rebuildCropBitmap("位图已回收");
      return;
    }
    const st = cropStateRef.current;
    const { w, h } = st.frame;
    const dpr = window.devicePixelRatio || 1;
    cvs.width = Math.round(w * dpr);
    cvs.height = Math.round(h * dpr);
    cvs.style.width = `${w}px`;
    cvs.style.height = `${h}px`;
    const ctx = cvs.getContext("2d");
    if (!ctx) return;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, w, h);
    const { w: nw, h: nh } = bitmapSize(src);
    // 基础缩放 = 画布→视图 的等比显示比（视图框是画布的缩小版，与 cropToTransform 用同一个比例）：
    // scale=1 表示 1 图片像素 = 1 画布像素 —— 按表盘尺寸出的图（如 212×520）在视图里恰好铺满框；
    // 不是画布规格的图则按原生尺寸居中摆放。再乘用户缩放。
    const canvasW = binSession.current?.model?.width || st.frame.w;
    const k = (st.frame.w / canvasW) * Math.max(st.scale, 0.05);
    ctx.save();
    try {
      ctx.translate(w / 2 + st.x, h / 2 + st.y);
      ctx.rotate((st.rotation * Math.PI) / 180);
      ctx.scale(k, k);
      ctx.drawImage(src, -nw / 2, -nh / 2, nw, nh);
    } catch {
      // 位图被回收时 drawImage 会抛：清缓存重解码，别再画个黑屏
      ctx.restore();
      void rebuildCropBitmap("drawImage 失败");
      return;
    }
    ctx.restore();
  };

  /** 画布「看起来是空的」探测：中心区 3×3 采样点全黑即认定没画上（回收后可能既不报错也不出图） */
  const cropCanvasLooksEmpty = (cvs: HTMLCanvasElement): boolean => {
    try {
      const ctx = cvs.getContext("2d");
      if (!ctx || !cvs.width) return false;
      for (const fx of [0.3, 0.5, 0.7]) {
        for (const fy of [0.3, 0.5, 0.7]) {
          const d = ctx.getImageData(Math.round(cvs.width * fx), Math.round(cvs.height * fy), 1, 1).data;
          if (d[0] > 8 || d[1] > 8 || d[2] > 8) return false; // 有内容
        }
      }
      return true; // 9 个采样点全黑 → 当没画上
    } catch {
      return false; // 读不到就当作正常，交给 try/catch 那条路
    }
  };

  /** 确认裁剪预览还在（滚回来 / 切回前台时调）：画布空了就重建位图，否则只重绘 */
  const ensureCropVisible = (reason: string) => {
    if (!pendingCropImage || currentStep !== "crop_image") return;
    const cvs = cropCanvasRef.current;
    if (!cvs) return;
    if (cropCanvasLooksEmpty(cvs)) {
      void rebuildCropBitmap(reason);
      return;
    }
    drawCrop();
  };

  // 进裁剪页：解码首绘 + 滚动/可见性/视野三条检查（vivo 上「滚出屏幕再回来永久黑屏」的修法）
  useEffect(() => {
    if (currentStep !== "crop_image") return;
    void loadCropBitmap();
    // 滚动（捕获阶段）中读像素很贵，节流到 300ms 一次
    let lastCheck = 0;
    const onScroll = () => {
      const now = Date.now();
      if (now - lastCheck < 300) return;
      lastCheck = now;
      ensureCropVisible("滚动后画布为空");
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") ensureCropVisible("回到前台");
    };
    document.addEventListener("scroll", onScroll, true);
    document.addEventListener("visibilitychange", onVisibility);
    // 滚动事件在个别 WebView 上不一定到（滚动发生在内层容器）→ 再挂一个 IntersectionObserver 兜底
    let wasOffscreen = false;
    const ob =
      typeof IntersectionObserver === "function"
        ? new IntersectionObserver(
            entries => {
              for (const en of entries) {
                if (!en.isIntersecting) {
                  wasOffscreen = true;
                  continue;
                }
                if (wasOffscreen) {
                  wasOffscreen = false;
                  ensureCropVisible("重新进入视野");
                }
              }
            },
            { threshold: 0.01 }
          )
        : null;
    const view = cropViewRef.current;
    if (ob && view) ob.observe(view);
    return () => {
      document.removeEventListener("scroll", onScroll, true);
      document.removeEventListener("visibilitychange", onVisibility);
      if (ob) ob.disconnect();
    };
  }, [currentStep, pendingCropImage]);

  // 拖拽/缩放/旋转变化 → 重绘（canvas 2D 画，不用 CSS transform）
  useEffect(() => {
    if (currentStep !== "crop_image") return;
    drawCrop();
  }, [currentStep, pendingCropImage, cropOffset, cropRotation, cropScale]);

  const handleApplyDialConfig = () => {
    if (!editingDial) return;

    // —— 真实应用（App 内）：合成壁纸 → watchface_apply_edit（两阶段 EditRequest + 图片上传）——
    if (invoke) {
      const addr = connected.current.addr;
      const session = binSession.current;
      if (!addr || !session?.model) {
        showToast("请先连接设备并载入该表盘的留档", "error");
        return;
      }
      const opt = dialOptions.current;
      const styleName = selectedStyle;
      setIsApplying(true);
      deviceBusy.current = true; // 合成/上传期间不探连接状态（忙 ≠ 掉线）
      void (async () => {
        try {
          const m = session.model as LayerModelDoc;
          // 1) 逐张合成待下发壁纸（照片 + 该张自己的裁剪变换，画布尺寸；表盘元素由设备绘制）
          const photosPngB64: string[] = [];
          if (m.photoAlbum) {
            for (let i = 0; i < wallpapers.length; i++) {
              const meta = wallpaperMeta.current[i];
              if (!meta?.b64) continue;
              const png = (await invoke("watchface_compose_photo", {
                dataB64: session.dataB64,
                themeIndex: themeIndexOf(styleName),
                photoB64: meta.b64,
                photoTransform: meta.crop,
                deviceModel: connected.current.model || null
              })) as string | null;
              if (png) photosPngB64.push(png);
            }
          }
          // 2) 应用：样式 / 配色 / 容器 / 壁纸（配色与容器只在用户改过时才下发）
          const colorPos = (opt?.colorNames || []).indexOf(selectedColorName);
          const colorId = colorPos >= 0 ? opt!.colorIds[colorPos] : null;
          const colorChanged =
            colorId != null && opt!.startStyleColorIndex != null && colorId !== opt!.startStyleColorIndex;
          const report = (await withStallGuard(
            linkLostRace(
              invoke("watchface_apply_edit", {
                addr,
                faceId: session.faceId,
                // 给设备发原始样式名（手环的 style 字段认名字，不能带「(2)」后缀）
                style: rawStyleNameOf(styleName),
                styleColorIndex: colorChanged ? colorId : null,
                slotChoices: slotChoicesFor(containers),
                slotDefaults: slotDefaultsOf(),
                photosPngB64: photosPngB64.length ? photosPngB64 : null,
                albumMode: "append",
                setCurrent: true
              })
            )
          )) as { uploaded?: number; finalCode?: number; imageCode?: number | null } | null;

          // 3) 本地相册存档（跨重启保留；下次进编辑页自动载入）
          if (photosPngB64.length) {
            try {
              await invoke("watchface_photos_save", {
                faceId: session.faceId,
                photos: wallpaperMeta.current.map(p => ({ name: p.name, b64: p.b64, crop: p.crop }))
              });
            } catch {
              /* 存不上不影响本次应用 */
            }
          }

          setIsApplying(false);
          deviceBusy.current = false;
          setIsDeviceConnected(true); // 能下发说明还连着
          heartbeatFailures.current = 0;
          setAppliedSuccess(true);
          setDials(prev =>
            prev.map(d => ({
              ...d,
              isCurrent: d.id === editingDial.id
            }))
          );
          // 壁纸被设备拒绝时要说清楚，别把失败报成成功（其余情况沿用设计稿文案）
          const imgOk = report?.imageCode === null || report?.imageCode === undefined || report.imageCode === 0;
          if (imgOk) {
            showToast(`已应用「${editingDial.name}」配置`);
          } else {
            showToast(`已应用样式/容器；壁纸被设备拒绝（imageCode=${report?.imageCode}）`, "error");
          }
          // 按钮绿一下、停留给用户看清反馈，再变回蓝色「应用」—— 留在编辑页继续调整
          setTimeout(() => {
            setAppliedSuccess(false);
          }, 1600);
        } catch (err) {
          setIsApplying(false);
          deviceBusy.current = false;
          showToast(`应用失败：${String(err)}`, "error");
        }
      })();
      return;
    }

    // —— 浏览器演示：延时进度 ——
    setIsApplying(true);

    setTimeout(() => {
      setIsApplying(false);
      setAppliedSuccess(true);

      setDials(prev =>
        prev.map(d => ({
          ...d,
          isCurrent: d.id === editingDial.id
        }))
      );

      setTimeout(() => {
        setAppliedSuccess(false); // 绿一下回蓝，留在编辑页（演示态同样不退出）
        showToast(`已应用「${editingDial.name}」配置`);
      }, 1600);
    }, 1200);
  };

  // 系统 WebView 过旧：整棵界面不渲染，只给说明页（否则用户看到的是"坏了"的界面）
  if (webViewTooOld !== null) {
    return <WebViewGateScreen major={webViewTooOld} onContinue={() => setWebViewTooOld(null)} />;
  }

  return (
    <IconContext.Provider value={{ weight: "bold" }}>
    <div className="min-h-screen bg-[#0C0D0E] flex items-center justify-center p-0 md:p-6 select-none font-sans">
      {/* 手机容器 */}
      <div className="w-full max-w-[400px] h-[100dvh] md:h-[844px] bg-[#F5F6F9] md:rounded-[44px] relative flex flex-col overflow-hidden shadow-[0_20px_50px_rgba(0,0,0,0.3)]">
        
        {/* 核心内容视口容器（普通滚动，不再有全局拖拽形变；顶部假 iOS 状态栏已删） */}
        <div className="flex-1 flex flex-col overflow-hidden relative">

          {/* 1. 登录页 */}
          {currentStep === "login" && (
            <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-12 page-enter-anim">
              <div className="pt-6">
                <h1 className="text-[36px] font-bold text-black tracking-tight leading-tight">
                  盘坊
                </h1>
                <p className="text-[17px] font-medium text-black/45 mt-2">
                  为你的手环装上喜欢的表盘
                </p>
              </div>

              <form onSubmit={handleLogin} className="flex flex-col my-auto">
                <div className="bg-white rounded-3xl p-2 flex flex-col divide-y divide-black/5 shadow-[0_1px_3px_rgba(0,0,0,0.02)]">
                  <div className="px-5 py-4">
                    <input
                      type="text"
                      value={account}
                      onChange={(e) => {
                        setAccount(e.target.value);
                        if (loginError) setLoginError(null);
                      }}
                      placeholder="小米账号 / 手机号 / 邮箱"
                      className="w-full text-[17px] font-medium text-black placeholder:text-black/25 outline-none bg-transparent"
                    />
                  </div>
                  <div className="px-5 py-4">
                    <input
                      type="password"
                      value={password}
                      onChange={(e) => {
                        setPassword(e.target.value);
                        if (loginError) setLoginError(null);
                      }}
                      placeholder="密码"
                      className="w-full text-[17px] font-medium text-black placeholder:text-black/25 outline-none bg-transparent"
                    />
                  </div>
                </div>

                {/* 登录按钮：高度 52，全圆角，白字 17/700，禁用 = black/10 底 + black/25 字 */}
                <button
                  type="submit"
                  disabled={!account.trim() || !password.trim() || isLoggingIn}
                  className="w-full h-[52px] rounded-full bg-[#4361EE] disabled:bg-black/10 disabled:text-black/25 text-white font-bold text-[17px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center gap-2 mt-5 tracking-tight"
                >
                  <span>{loginButtonText}</span>
                </button>

                {/* 状态 C · 错误（行内提示，不弹窗，13px，#FF3B30） */}
                {loginError && (
                  <p className="text-[13px] text-[#FF3B30] text-center font-bold mt-3 animate-in fade-in">
                    {loginError}
                  </p>
                )}

                {/* 状态 B · 异常兜底（平时不可见，只有异常才出现，一行行内次级动作） */}
                {showLoginFallback && (
                  <div className="flex items-center justify-center gap-3 mt-3.5 text-[14px] font-bold text-black/80 animate-in fade-in">
                    <button
                      type="button"
                      onClick={() => {
                        setShowLoginFallback(false);
                        setLoginError(null);
                        setLoginButtonText("正在登录…");
                        setIsLoggingIn(true);
                        // App 内：重新拉起受控验证窗口，收尾交给 wf-2fa 事件
                        if (invoke) {
                          void (async () => {
                            setLoginButtonText("正在完成安全验证…");
                            try {
                              await invoke("xiaomi_2fa_open");
                            } catch (err) {
                              setIsLoggingIn(false);
                              setLoginButtonText("登录");
                              setShowLoginFallback(true);
                              setLoginError(String(err));
                            }
                          })();
                          return;
                        }
                        setTimeout(() => {
                          setLoginButtonText("正在完成安全验证…");
                          setTimeout(() => {
                            setIsLoggingIn(false);
                            setLoginButtonText("登录");
                            pushRoute("devices");
                          }, 1000);
                        }, 800);
                      }}
                      className="text-[#4361EE] font-bold hover:underline cursor-pointer active:scale-95 transition-all"
                    >
                      重新验证
                    </button>
                  </div>
                )}
              </form>

              <div className="text-center space-y-1.5">
                {/* 取得同意的落点：登录前必须能看到协议与政策（也放进「关于应用」页） */}
                <p className="text-[12px] font-medium text-black/35 leading-relaxed">
                  登录即表示同意
                  <button
                    type="button"
                    onClick={(e) => { openLink(e, `${LEGAL_BASE}/terms`); }}
                    className="text-[#4361EE] font-bold cursor-pointer"
                  >
                    《用户协议》
                  </button>
                  与
                  <button
                    type="button"
                    onClick={(e) => { openLink(e, `${LEGAL_BASE}/privacy`); }}
                    className="text-[#4361EE] font-bold cursor-pointer"
                  >
                    《隐私政策》
                  </button>
                </p>
                <span className="text-[13px] font-medium text-black/40">
                  小米账号安全登录
                </span>
              </div>
            </div>
          )}

          {/* 2. 设备连接页 */}
          {currentStep === "device_select" && (
            <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-12 page-enter-anim">
              <div>
                <h1 className="text-[32px] font-bold text-black tracking-tight leading-tight">
                  选择设备
                </h1>
                <p className="text-[17px] font-medium text-black/45 mt-1.5">
                  点按连接腕上手环
                </p>
              </div>

              <div className="flex flex-col gap-3 my-auto">
                {mockDevicesList.length === 0 ? (
                  /* C5. 空态样例 1：无设备 */
                  <div className="bg-white rounded-3xl p-5 flex flex-col items-center text-center shadow-[0_1px_3px_rgba(0,0,0,0.02)]">
                    <p className="text-[14px] font-medium text-black/40 leading-relaxed mb-4">
                      账号下暂无设备，可在手环端与小米账号绑定后重新导入
                    </p>
                    <button
                      type="button"
                      onClick={() => {
                        // 重新导入 = 把账号下的设备全拉回来：先清掉「已移除」名单
                        clearHiddenDevices();
                        // App 内：重新拉一次账号下的设备（本地凭证续期）
                        if (invoke) {
                          void (async () => {
                            try {
                              const devices = (await invoke("xiaomi_restore")) as CloudDevice[] | null;
                              applyCloudDevices(devices);
                              showToast("已重新拉取手环设备");
                            } catch (err) {
                              console.error("[xiaomi_restore] 重新导入失败：", err);
                              showToast("重新导入失败，请稍后重试", "error");
                            }
                          })();
                          return;
                        }
                        setMockDevicesList(MOCK_DEVICES);
                        showToast("已重新拉取手环设备");
                      }}
                      className="h-[44px] px-5 rounded-full bg-black/5 hover:bg-black/10 text-black/75 font-bold text-[14px] cursor-pointer active:scale-95 transition-all"
                    >
                      重新导入设备
                    </button>
                  </div>
                ) : (
                  mockDevicesList.map((device) => {
                    const isConnecting = connectingId === device.id;
                    const isConnected = connectedId === device.id;

                    return (
                      <div
                        key={device.id}
                        className="w-full bg-white rounded-3xl p-5 transition-all duration-200 flex items-center justify-between shadow-[0_1px_3px_rgba(0,0,0,0.02)]"
                      >
                        <button
                          type="button"
                          onClick={() => handleSelectDevice(device)}
                          className="flex-1 min-w-0 text-left flex items-center gap-4 cursor-pointer active:scale-[0.98] transition-transform group"
                        >
                          <div className="w-11 h-11 shrink-0 rounded-xl bg-[#F5F6F9] flex items-center justify-center text-black/80">
                            <Watch className="w-5 h-5 stroke-[2]" />
                          </div>
                          <h3 className="text-[18px] font-bold text-black leading-snug tracking-tight min-w-0">
                            {device.name}
                          </h3>
                        </button>

                        <div className="flex items-center gap-1.5 pl-2 shrink-0">
                          {isConnecting && (
                            <div className="flex items-center gap-2 text-[#4361EE] font-bold text-[14px]">
                              <CircleNotch className="w-5 h-5 animate-spin" />
                            </div>
                          )}

                          {isConnected && (
                            <div className="w-8 h-8 rounded-full bg-[#34C759]/15 text-[#34C759] flex items-center justify-center animate-in zoom-in-75 duration-200">
                              <Check className="w-5 h-5 stroke-[2]" />
                            </div>
                          )}

                          {!isConnecting && !isConnected && (
                            <CaretRight className="w-5 h-5 text-black/25 stroke-[2] group-hover:text-black/45 group-hover:translate-x-0.5 transition-all" />
                          )}

                          {/* 移除：只从列表里去掉（小米账号里不动），「重新导入设备」可找回 */}
                          <button
                            type="button"
                            aria-label={`移除 ${device.name}`}
                            onClick={() => setDeviceRemoveConfirm(device)}
                            className="w-8 h-8 rounded-full flex items-center justify-center text-black/25 hover:text-[#FF3B30] hover:bg-black/5 active:scale-95 transition-all cursor-pointer"
                          >
                            <X className="w-4 h-4 stroke-[2.5]" />
                          </button>
                        </div>
                      </div>
                    );
                  })
                )}

                {connectGuideCard}
              </div>

              <div className="text-center flex flex-col items-center">
                <button
                  type="button"
                  onClick={() => resetStackTo("login")}
                  className="text-[14px] font-semibold text-[#4361EE] p-1 cursor-pointer active:scale-95 transition-all"
                >
                  切换账号
                </button>
              </div>
            </div>
          )}

          {/* 3. 主界面 (首页 + 管理 + 设置 Tab) */}
          {currentStep === "main" && (
            <div className="flex-1 flex flex-col overflow-hidden relative">
              
              {/* Tab 1: 首页 */}
              {activeTab === "home" && (
                <div key="tab-home" className="flex-1 flex flex-col justify-between overflow-y-auto no-scrollbar px-6 safe-top-tab pb-24 page-enter-anim">
                  {/* 顶栏空条（fixed 覆盖层）+ 胶囊在内容流 56px 起点（设计稿口径） */}
                  <div>
                    <div className="top-bar" aria-hidden="true" />
                    <div className="flex justify-center">
                    <button
                      type="button"
                      onClick={openDeviceDrawer}
                      className={`inline-flex items-center gap-2 px-3.5 h-[40px] bg-white rounded-full cursor-pointer active:scale-95 transition-all shadow-[0_1px_3px_rgba(0,0,0,0.02)] border border-black/5 ${
                        isDeviceSwitchingAnim ? "ring-2 ring-[#4361EE] scale-105" : ""
                      }`}
                    >
                      {/* 连接状态：绿点=已连接，红点=未连接；掉线自动重连中显示「重连中…」 */}
                      <span className={`w-2 h-2 rounded-full ${isDeviceConnected ? "bg-[#34C759]" : linkRetrying ? "bg-[#D4973B]" : "bg-[#FF3B30]"}`} />
                      <span className="text-[13px] font-bold text-black tracking-tight">
                        {isDeviceConnected ? selectedDevice.name : linkRetrying ? "重连中…" : "未连接设备"}
                      </span>
                      <div className="flex items-center gap-1 pl-0.5 text-[12px] font-bold text-black/40">
                        <Lightning className="w-3.5 h-3.5 text-[#D4973B] fill-[#D4973B]" />
                        <span>{isDeviceConnected ? `${fmtBattery(selectedDevice.battery)}%` : "--%"}</span>
                      </div>
                      <CaretRight className="w-3.5 h-3.5 text-black/25 stroke-[2] ml-0.5" />
                    </button>
                    </div>
                  </div>

                  <div className="my-auto py-4">
                    {/* 根据主次排序渲染卡片 */}
                    {isPrimaryLocal && enableLocalInstall ? (
                      /* 本地表盘为主卡片，兑换表盘为次卡片 */
                      <>
                        <button
                          type="button"
                          onClick={handleLocalInstall}
                          className="w-full text-left bg-white rounded-3xl p-6 shadow-[0_1px_3px_rgba(0,0,0,0.02)] active:scale-[0.97] transition-all duration-200 cursor-pointer group flex flex-col"
                        >
                          <div className="w-14 h-14 rounded-2xl bg-[#0C0D0E] text-white flex items-center justify-center mb-6">
                            <FolderOpen className="w-6 h-6 stroke-[2]" />
                          </div>

                          <div className="flex items-baseline justify-between w-full">
                            <h2 className="text-[22px] font-bold text-black tracking-tight leading-tight">
                              本地表盘安装
                            </h2>
                            <CaretRight className="w-6 h-6 text-black/25 group-hover:text-black/45 group-hover:translate-x-0.5 transition-all" />
                          </div>

                          <p className="text-[14px] font-medium text-black/45 mt-1.5">
                            选择 .bin 文件直接安装
                          </p>
                        </button>

                        <div className="mt-3">
                          <button
                            type="button"
                            onClick={openPaidFlow}
                            className="w-full flex items-center justify-between px-5 h-[52px] rounded-2xl bg-white active:scale-[0.98] transition-all text-black/80 cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.02)]"
                          >
                            <div className="flex items-center gap-3">
                              {/* 极简纯粹的卡券线性图标 */}
                              <svg
                                viewBox="0 0 24 24"
                                className="w-5 h-5 text-[#4361EE]"
                                fill="none"
                                stroke="currentColor"
                                strokeWidth="2"
                                strokeLinecap="round"
                                strokeLinejoin="round"
                              >
                                <rect x="3" y="6" width="18" height="12" rx="3" />
                                <line x1="9" y1="6" x2="9" y2="18" strokeDasharray="2 2" />
                              </svg>
                              <span className="text-[16px] font-bold text-black/80">
                                安装已购表盘
                              </span>
                            </div>
                            {paidUpdateCount > 0 ? (
                              <span className="shrink-0 text-[13px] font-bold text-[#D4973B]">
                                {paidUpdateCount} 个可更新
                              </span>
                            ) : (
                              <span className="text-[13px] font-medium text-black/40">
                                爱发电
                              </span>
                            )}
                          </button>
                        </div>
                      </>
                    ) : (
                      /* 付费表盘为主卡片 */
                      <>
                        <button
                          type="button"
                          onClick={openPaidFlow}
                          className="w-full text-left bg-white rounded-3xl p-6 shadow-[0_1px_3px_rgba(0,0,0,0.02)] active:scale-[0.97] transition-all duration-200 cursor-pointer group flex flex-col"
                        >
                          <div className="relative w-14 h-14 rounded-2xl bg-[#4361EE] text-white flex items-center justify-center mb-6">
                            {/* 极简大方、克制的卡券线性图标 */}
                            <svg
                              viewBox="0 0 24 24"
                              className="w-6 h-6 text-white"
                              fill="none"
                              stroke="currentColor"
                              strokeWidth="2"
                              strokeLinecap="round"
                              strokeLinejoin="round"
                            >
                              <rect x="3" y="6" width="18" height="12" rx="3" />
                              <line x1="9" y1="6" x2="9" y2="18" strokeDasharray="2 2" />
                            </svg>
                            {/* 有可更新的已购表盘 → 角标（点进去就能看到是哪几个） */}
                            {paidUpdateCount > 0 && (
                              <span className="absolute -top-1.5 -right-1.5 min-w-[20px] h-5 px-1 rounded-full bg-[#D4973B] text-white text-[11px] font-bold flex items-center justify-center ring-2 ring-white">
                                {paidUpdateCount}
                              </span>
                            )}
                          </div>

                          <div className="flex items-baseline justify-between w-full">
                            <h2 className="text-[22px] font-bold text-black tracking-tight leading-tight">
                              安装已购表盘
                            </h2>
                            <CaretRight className="w-6 h-6 text-black/25 group-hover:text-black/45 group-hover:translate-x-0.5 transition-all" />
                          </div>

                          <p className="text-[14px] font-medium text-black/45 mt-1.5">
                            {paidUpdateCount > 0
                              ? `有 ${paidUpdateCount} 个表盘可更新`
                              : "绑定爱发电，同步你买过的表盘"}
                          </p>
                        </button>

                        {/* 本地表盘安装次级入口（受开关控制） */}
                        {enableLocalInstall && (
                          <div className="mt-3">
                            <button
                              type="button"
                              onClick={handleLocalInstall}
                              className="w-full flex items-center justify-between px-5 h-[52px] rounded-2xl bg-white active:scale-[0.98] transition-all text-black/80 cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.02)]"
                            >
                              <div className="flex items-center gap-3">
                                <FolderOpen className="w-5 h-5 text-black/40 stroke-[2]" />
                                <span className="text-[16px] font-bold text-black/80">
                                  本地表盘安装
                                </span>
                              </div>
                              <span className="text-[13px] font-medium text-black/40">
                                .bin 文件
                              </span>
                            </button>
                          </div>
                        )}
                      </>
                    )}
                  </div>
                </div>
              )}

              {/* Tab 2: 管理 */}
              {activeTab === "manage" && (
                <div key="tab-manage" className="flex-1 flex flex-col overflow-y-auto no-scrollbar px-6 safe-top-tab pb-24 page-enter-anim">
                  {/* 顶栏空条（fixed 覆盖层）；标题在内容流 56px 起点，条下间隙 12px、标题到卡片 28px（设计稿 56/116 口径） */}
                  <div className="px-1 pb-7">
                    <div className="top-bar" aria-hidden="true" />
                    <h1 className="text-[32px] font-bold text-black tracking-tight leading-none">
                      管理
                    </h1>
                  </div>

                  {/* 长方形设备连接状态卡片：堆在下方表盘卡片上方 */}
                  <div className="mb-4">
                    <button
                      type="button"
                      onClick={openDeviceDrawer}
                      className={`w-full text-left bg-white rounded-3xl p-5 cursor-pointer active:scale-[0.98] transition-all flex items-center justify-between shadow-[0_1px_3px_rgba(0,0,0,0.02)] border border-black/5 ${
                        isDeviceSwitchingAnim ? "ring-2 ring-[#4361EE] scale-[1.01]" : ""
                      }`}
                    >
                      <div className="flex items-center gap-3.5">
                        <div className="w-11 h-11 rounded-xl bg-[#F5F6F9] flex items-center justify-center text-black/80 shrink-0">
                          <Watch className="w-5 h-5 stroke-[2]" />
                        </div>
                        <div>
                          {/* 两行同构：图标列统一 8px（状态点/闪电居中），文字列统一从 16px 起 —— 上下左对齐 */}
                          <div className="flex items-center gap-2">
                            <span className={`w-2 h-2 rounded-full shrink-0 ${isDeviceConnected ? "bg-[#34C759]" : linkRetrying ? "bg-[#D4973B]" : "bg-[#FF3B30]"}`} />
                            <h3 className="text-[17px] font-bold text-black leading-snug tracking-tight">
                              {isDeviceConnected ? selectedDevice.name : linkRetrying ? "重连中…" : "未连接设备"}
                            </h3>
                          </div>
                          <div className="flex items-center gap-2 text-[13px] font-medium text-black/40 mt-0.5">
                            <span className="w-2 shrink-0 flex justify-center">
                              <Lightning className="w-3 h-3 text-[#D4973B] fill-[#D4973B]" />
                            </span>
                            <span>电量 {isDeviceConnected ? fmtBattery(selectedDevice.battery) : "--"}%</span>
                            <span className="text-black/25">·</span>
                            <span>{isDeviceConnected ? "已连接" : linkRetrying ? "自动重连中" : "未连接"}</span>
                          </div>
                        </div>
                      </div>

                      <div className="flex items-center text-black/25">
                        <CaretRight className="w-5 h-5 text-black/25 stroke-[2]" />
                      </div>
                    </button>
                  </div>

                  <div className={`grid grid-cols-2 gap-3 pb-3 transition-opacity duration-300 ${
                    isDeviceSwitchingAnim ? "opacity-75" : "opacity-100"
                  }`}>
                    {!isDeviceConnected ? (
                      /* 未连接设备：不显示任何表盘卡片（也没有预设假数据），只留一行提示 */
                      <div className="col-span-2 py-10 text-center text-[14px] font-medium text-black/40">
                        请连接设备后再查看表盘列表
                      </div>
                    ) : dials.length === 0 ? (
                      /* C5. 空态样例 2：无表盘 */
                      <div className="col-span-2 bg-white rounded-3xl p-5 flex flex-col items-center text-center shadow-[0_1px_3px_rgba(0,0,0,0.02)] my-8">
                        <p className="text-[14px] font-medium text-black/40 mb-4">
                          手环上还没有表盘
                        </p>
                        <button
                          type="button"
                          onClick={() => {
                            // 重新拉一次设备表盘列表（空态可能只是还没同步）
                            if (invoke && connected.current.addr) {
                              void refreshDials(connected.current.addr);
                            }
                            showToast("已重新拉取手环表盘");
                          }}
                          className="h-[44px] px-5 rounded-full bg-black/5 hover:bg-black/10 text-black/75 font-bold text-[14px] cursor-pointer active:scale-95 transition-all"
                        >
                          重新拉取表盘列表
                        </button>
                      </div>
                    ) : (
                      // 展示顺序：可编辑（本 APP 安装、有留档）的排最前，其余保持原顺序排后面
                      dials
                        .slice()
                        .sort(
                          (a, b) =>
                            Number(!!archivedIds.current[b.id]) - Number(!!archivedIds.current[a.id])
                        )
                        .map((dial) => {
                        const isMenuOpen = activeActionSheetDial?.id === dial.id;
                        return (
                          <div
                            key={dial.id}
                            onClick={() => handleOpenDialEdit(dial)}
                            className={`rounded-3xl p-4 cursor-pointer active:scale-[0.96] transition-all flex flex-col justify-between min-h-[142px] bg-white border-2 shadow-[0_1px_3px_rgba(0,0,0,0.02)] relative group ${
                              isMenuOpen ? "z-40" : "z-0"
                            } ${
                              dial.isCurrent
                                ? "border-[#4361EE]"
                                : "border-transparent"
                            }`}
                          >
                            <div className="h-6 flex items-center justify-between">
                              <div className="flex items-center gap-1.5 min-w-0">
                                {/* 有更新的表盘这一格只放「可更新」：卡片是两列布局、格子只有 ~119px 宽，
                                    再并排一个「使用中/照片」会把文字挤成竖排（真机 2026-10-08 见到）；
                                    「使用中」本来就由卡片蓝框表示，不丢信息。 */}
                                {paidUpdateDials.has(dial.id) ? (
                                  <button
                                    type="button"
                                    onClick={(e) => {
                                      e.stopPropagation();
                                      const hit = paidUpdateDials.get(dial.id);
                                      if (hit) runPaidInstall(hit.product, hit.variant);
                                    }}
                                    className="shrink-0 whitespace-nowrap text-[12px] font-bold text-[#D4973B] bg-[#D4973B]/15 rounded-full px-2 py-0.5 cursor-pointer active:scale-95 transition-all"
                                  >
                                    可更新
                                  </button>
                                ) : dial.isCurrent ? (
                                  <span className="inline-flex items-center gap-1.5 whitespace-nowrap text-[12px] font-bold text-[#4361EE]">
                                    <span className="w-1.5 h-1.5 rounded-full bg-[#4361EE]" />
                                    使用中
                                  </span>
                                ) : dial.isPhotoDial ? (
                                  <span className="whitespace-nowrap text-[12px] font-bold px-2 py-0.5 bg-black/5 text-black/40 rounded-full">
                                    照片
                                  </span>
                                ) : null}
                              </div>

                              {/* 表盘卡右上角「⋯」（更多）按钮及就地弹出选项列表 */}
                              <div className="relative">
                                <button
                                  type="button"
                                  onClick={(e) => {
                                    e.stopPropagation();
                                    setActiveActionSheetDial(
                                      isMenuOpen ? null : dial
                                    );
                                  }}
                                  aria-label="更多操作"
                                  className="w-11 h-11 -mr-2 -mt-2 rounded-full flex items-center justify-center text-black/25 hover:text-black/80 hover:bg-black/5 active:scale-95 transition-all cursor-pointer relative z-40"
                                >
                                  <DotsThree className="w-6 h-6 stroke-[2]" />
                                </button>

                                {/* 从 3 个点位置就地弹出的紧凑选择列表（Popover Menu）
                                    关闭靠手机容器那层全屏透明遮罩（点空白处即关） */}
                                {isMenuOpen && (
                                  <div
                                      onClick={(e) => e.stopPropagation()}
                                      className="absolute right-0 top-9 w-36 bg-white/95 backdrop-blur-2xl rounded-2xl shadow-[0_8px_24px_rgba(0,0,0,0.10)] border border-black/10 p-1.5 z-50 flex flex-col gap-0.5 animate-in fade-in zoom-in-95 origin-top-right duration-150"
                                    >
                                      {!dial.isCurrent && (
                                        <button
                                          type="button"
                                          onClick={() => {
                                            setDials((prev) =>
                                              prev.map((d) => ({
                                                ...d,
                                                isCurrent: d.id === dial.id,
                                              }))
                                            );
                                            setActiveActionSheetDial(null);
                                            // App 内：真实切换设备当前表盘（失败则回读列表纠正显示）
                                            if (invoke && connected.current.addr) {
                                              const addr = connected.current.addr;
                                              invoke("watchface_set_current", {
                                                addr,
                                                watchfaceId: dial.id
                                              })
                                                .then(() => showToast(`已将「${dial.name}」设为当前手环表盘`))
                                                .catch((err) => {
                                                  showToast(humanize(err, "set_current"), "error");
                                                  void refreshDials(addr);
                                                });
                                              return;
                                            }
                                            showToast(`已将「${dial.name}」设为当前手环表盘`);
                                          }}
                                          className="w-full px-3 py-2 text-left text-[14px] font-bold text-black hover:bg-black/5 rounded-xl transition-colors cursor-pointer"
                                        >
                                          设为当前
                                        </button>
                                      )}

                                      <button
                                        type="button"
                                        onClick={() => {
                                          setActiveActionSheetDial(null);
                                          handleOpenDialEdit(dial);
                                        }}
                                        className="w-full px-3 py-2 text-left text-[14px] font-bold text-black hover:bg-black/5 rounded-xl transition-colors cursor-pointer"
                                      >
                                        编辑表盘
                                      </button>

                                      <button
                                        type="button"
                                        onClick={() => {
                                          setActiveActionSheetDial(null);
                                          setUninstallConfirmDial(dial);
                                        }}
                                        className="w-full px-3 py-2 text-left text-[14px] font-bold text-[#FF3B30] hover:bg-[#FF3B30]/10 rounded-xl transition-colors cursor-pointer"
                                      >
                                        卸载表盘
                                      </button>
                                  </div>
                                )}
                              </div>
                            </div>

                            <div className="my-auto py-1">
                              <h4 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                                {dial.name}
                              </h4>
                            </div>

                            <div className="flex items-center justify-between pt-2 border-t border-black/5 text-[12px] font-medium text-black/25">
                              <span>轻触编辑</span>
                              <CaretRight className="w-3.5 h-3.5 text-black/25 stroke-[2]" />
                            </div>
                          </div>
                        );
                      })
                    )}
                  </div>
                </div>
              )}

              {/* Tab 3: 设置（极简纯列表页） */}
              {activeTab === "settings" && (
                <div key="tab-settings" className="flex-1 flex flex-col justify-between overflow-y-auto no-scrollbar px-6 safe-top-tab pb-24 page-enter-anim">
                  <div>
                    {/* 顶栏空条（fixed 覆盖层）；标题口径与管理页一致（56/116） */}
                    <div className="top-bar" aria-hidden="true" />
                    <div className="px-1 pb-7">
                      <h1 className="text-[32px] font-bold text-black tracking-tight leading-none">
                        设置
                      </h1>
                    </div>

                    {/* 功能设置组 */}
                    <div className="bg-white rounded-3xl overflow-hidden shadow-[0_1px_3px_rgba(0,0,0,0.02)] mb-4 divide-y divide-black/5">
                      {/* 1. 本地表盘安装开关：52×30 · 开 #34C759 · 关 black/10 */}
                      <div className="py-[18px] px-5 flex items-center justify-between">
                        <div className="pr-3">
                          <h4 className="text-[16px] font-bold text-black leading-tight">
                            本地表盘安装
                          </h4>
                          <p className="text-[13px] font-medium text-black/40 mt-1">
                            开启后首页显示 .bin 文件安装入口
                          </p>
                        </div>

                        <button
                          type="button"
                          onClick={() => {
                            const next = !enableLocalInstall;
                            setEnableLocalInstall(next);
                            storeFlag("panfang.enableLocalInstall", next);
                            if (!next && isPrimaryLocal) {
                              setIsPrimaryLocal(false);
                              storeFlag("panfang.primaryLocal", false);
                            }
                            showToast(next ? "已开启本地表盘安装入口" : "已隐藏本地表盘安装入口");
                          }}
                          className={`w-[52px] h-[30px] rounded-full p-1 transition-colors duration-200 cursor-pointer flex items-center shrink-0 ${
                            enableLocalInstall ? "bg-[#34C759]" : "bg-black/10"
                          }`}
                        >
                          <div
                            className={`w-[22px] h-[22px] rounded-full bg-white shadow-xs transition-transform duration-200 ${
                              enableLocalInstall ? "translate-x-[22px]" : "translate-x-0"
                            }`}
                          />
                        </button>
                      </div>

                      {/* 2. 切换主次卡片按钮（在开启本地表盘后可用） */}
                      {enableLocalInstall && (
                        <div className="py-[18px] px-5 flex items-center justify-between">
                          <div className="pr-3">
                            <h4 className="text-[16px] font-bold text-black leading-tight">
                              首页卡片主次切换
                            </h4>
                            <p className="text-[13px] font-medium text-black/40 mt-1">
                              当前主卡片：{isPrimaryLocal ? "本地表盘安装" : "安装已购表盘"}
                            </p>
                          </div>

                          <button
                            type="button"
                            onClick={() => {
                              setIsPrimaryLocal(!isPrimaryLocal);
                              storeFlag("panfang.primaryLocal", !isPrimaryLocal);
                              showToast(`已设「${!isPrimaryLocal ? "本地表盘安装" : "安装已购表盘"}」为主卡片`);
                            }}
                            className="flex items-center gap-1.5 px-3.5 h-[32px] rounded-full bg-black/5 hover:bg-black/10 active:bg-black/15 text-black font-bold text-[13px] cursor-pointer transition-all active:scale-95 shrink-0"
                          >
                            <ArrowsDownUp className="w-3.5 h-3.5 text-black/40 stroke-[2]" />
                            <span>切换</span>
                          </button>
                        </div>
                      )}
                    </div>

                    {/* 账号与设备组 */}
                    <div className="bg-white rounded-3xl overflow-hidden shadow-[0_1px_3px_rgba(0,0,0,0.02)] mb-4 divide-y divide-black/5">
                      {/* 重新登录小米账号 */}
                      <button
                        type="button"
                        onClick={() => {
                          resetStackTo("login"); // 重新登录：登录页成为根页面
                          showToast("请登录您的小米账号", "error");
                        }}
                        className="w-full py-[18px] px-5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer"
                      >
                        <div>
                          <h4 className="text-[16px] font-bold text-black leading-tight">
                            重新登录小米账号
                          </h4>
                          <p className="text-[13px] font-medium text-black/40 mt-1">
                            已登录：{userProfile.nickname}
                          </p>
                        </div>
                        <CaretRight className="w-5 h-5 text-black/25 stroke-[2]" />
                      </button>

                      {/* 重新导入设备 */}
                      <button
                        type="button"
                        onClick={() => {
                          // 与设备抽屉里的「重新导入设备列表」同一口径：清掉「已移除」名单 + 重新拉全
                          clearHiddenDevices();
                          if (invoke) {
                            void (async () => {
                              try {
                                const devices = (await invoke("xiaomi_restore")) as CloudDevice[] | null;
                                applyCloudDevices(devices);
                              } catch (err) {
                                console.error("[xiaomi_restore] 重新导入失败：", err);
                                showToast("重新导入失败，请稍后重试", "error");
                              }
                            })();
                          } else {
                            setMockDevicesList(MOCK_DEVICES);
                          }
                          pushRoute("devices");
                          showToast("已刷新手环列表");
                        }}
                        className="w-full py-[18px] px-5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer"
                      >
                        <div>
                          <h4 className="text-[16px] font-bold text-black leading-tight">
                            重新导入手环设备
                          </h4>
                          <p className="text-[13px] font-medium text-black/40 mt-1">
                            当前连接：{selectedDevice.name}
                          </p>
                        </div>
                        <CaretRight className="w-5 h-5 text-black/25 stroke-[2]" />
                      </button>
                    </div>

                    {/* 后台保活：连接期间有前台服务（常驻一条静默通知）；被 ROM 省电策略冻死会断连 */}
                    <div className="bg-white rounded-3xl overflow-hidden shadow-[0_1px_3px_rgba(0,0,0,0.02)] mb-4">
                      <button
                        type="button"
                        onClick={() => {
                          if (!invoke) {
                            showToast("浏览器预览里没有系统设置", "error");
                            return;
                          }
                          // 后端按「最能帮到用户的一步」走并回报做了什么（already/dialog/settings/failed）——
                          // 每次点都必须有可见反馈，否则「点了没反应」会被当成坏了（2026-10-10 用户反馈）
                          void invoke("device_open_battery_settings")
                            .then((r: { result?: string } | null) => {
                              switch (r?.result) {
                                case "already":
                                  showToast("盘坊已被系统允许后台运行");
                                  break;
                                case "dialog":
                                  showToast("请在系统弹窗里选择「允许」");
                                  break;
                                case "settings":
                                  showToast("请在「电池」里把盘坊设为不受限制");
                                  break;
                                default:
                                  showToast("请到 系统设置 → 应用 → 盘坊 → 电池 里允许后台运行", "error");
                              }
                            })
                            .catch(() => {
                              showToast("请在系统设置里允许盘坊后台运行", "error");
                            });
                        }}
                        className="w-full py-[18px] px-5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer"
                      >
                        <div className="pr-3">
                          <h4 className="text-[16px] font-bold text-black leading-tight">
                            后台保持连接
                          </h4>
                          <p className="text-[13px] font-medium text-black/40 mt-1">
                            打开系统的电池设置，允许盘坊在后台运行
                          </p>
                        </div>
                        <CaretRight className="w-5 h-5 text-black/25 stroke-[2]" />
                      </button>
                    </div>

                    {/* 爱发电账号（已购资源）：账号管理只在这里，不在绑定流程里 */}
                    <div className="bg-white rounded-3xl overflow-hidden shadow-[0_1px_3px_rgba(0,0,0,0.02)] mb-4">
                      <button
                        type="button"
                        onClick={() => {
                          if (paidBound) {
                            pushRoute("paid/account");
                            return;
                          }
                          goPaidBindPage();
                        }}
                        className="w-full py-[18px] px-5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer group"
                      >
                        <div className="flex items-center gap-3.5 min-w-0">
                          {/* 爱发电 brand 图标（用户在 Figma 里画的：紫底 + 白色 logo）；
                              圆角裁在资源里（clipPath rx=300/1000 = 30%，与 rounded-xl 在 40px 上的比例一致） */}
                          <img
                            src={afdianIcon}
                            alt=""
                            className="w-10 h-10 shrink-0 rounded-xl"
                          />
                          <div className="min-w-0">
                            <h4 className="text-[16px] font-bold text-black leading-tight">
                              爱发电账号
                            </h4>
                            <p className="text-[13px] font-medium text-black/40 mt-0.5 truncate">
                              {paidBound
                                ? `已绑定${paidDisplay ? ` · 尾号 ${paidDisplay}` : ""}`
                                : "未绑定 · 绑定后同步已购表盘"}
                            </p>
                          </div>
                        </div>
                        <CaretRight className="w-5 h-5 shrink-0 text-black/25 stroke-[2] group-hover:text-black/45 transition-colors" />
                      </button>
                    </div>

                    {/* 关于应用 */}
                    <div className="bg-white rounded-3xl overflow-hidden shadow-[0_1px_3px_rgba(0,0,0,0.02)] mb-4">
                      <button
                        type="button"
                        onClick={() => pushRoute("about")}
                        className="w-full py-[18px] px-5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer group"
                      >
                        <div className="flex items-center gap-3.5 min-w-0">
                          <div className="w-10 h-10 rounded-xl bg-[#4361EE]/10 text-[#4361EE] flex items-center justify-center shrink-0">
                            <Info className="w-5 h-5 stroke-[2]" />
                          </div>
                          <div className="min-w-0">
                            <h4 className="text-[16px] font-bold text-black leading-tight">
                              关于应用
                            </h4>
                            <p className="text-[13px] font-medium text-black/40 mt-0.5">
                              盘坊 v{appVersion ?? "0.3.20"} · 开源许可与开发者
                            </p>
                          </div>
                        </div>
                        <CaretRight className="w-5 h-5 shrink-0 text-black/25 stroke-[2] group-hover:text-black/45 transition-colors" />
                      </button>
                    </div>

                    {/* 危险操作组 */}
                    <div className="bg-white rounded-3xl overflow-hidden shadow-[0_1px_3px_rgba(0,0,0,0.02)] mb-4">
                      <button
                        type="button"
                        onClick={() => {
                          setShowLogoutConfirm(true);
                        }}
                        className="w-full py-[18px] px-5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer"
                      >
                        <span className="text-[16px] font-bold text-[#FF3B30]">
                          退出登录
                        </span>
                        <SignOut className="w-5 h-5 text-[#FF3B30] stroke-[2]" />
                      </button>
                    </div>
                  </div>

                  {/* 最底下一行版本号小字 */}
                  <div className="text-center pt-6 pb-2">
                    <p className="text-[12px] font-medium text-black/40 tracking-wide">
                      盘坊 · 版本 {appVersion ?? "0.3.20"}
                    </p>
                  </div>
                </div>
              )}
            </div>
          )}

          {/* 3.5 怎么连上手环（连接失败后的自助帮助页） */}
          {currentStep === "connect_help" && (() => {
            const devName = failedDevice?.name || "手环";
            const isWatch = /watch|手表/i.test(devName);
            /** 文案里的设备称呼跟机型走（手表/手环），别一会儿手表一会儿手环 */
            const devWord = isWatch ? "手表" : "手环";
            // 菜单路径按机型给（不同系统版本可能略有差异 → 底部附一句兜底说明）
            const step2Path = isWatch
              ? "设置 → 系统 → 连接新手机"
              : "设置 → 系统操作 → 连接新手机";
            const connecting = connectingId !== null;
            const steps: { icon: React.ReactNode; title: string; desc: string }[] = [
              {
                icon: (
                  <svg viewBox="0 0 24 24" className="w-5 h-5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <circle cx="12" cy="12" r="6" />
                    <path d="m16.13 7.66-.81-4.05a2 2 0 0 0-2-1.61h-2.68a2 2 0 0 0-2 1.61l-.78 4.05" />
                    <path d="m7.88 16.36.8 4a2 2 0 0 0 2 1.61h2.72a2 2 0 0 0 2-1.61l.81-4.05" />
                  </svg>
                ),
                title: `在${isWatch ? "手表" : "手环"}上打开「连接新手机」`,
                desc: step2Path
              },
              {
                icon: (
                  <svg viewBox="0 0 24 24" className="w-5 h-5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d="M5 12.55a11 11 0 0 1 14.08 0" />
                    <path d="M8.53 16.11a6 6 0 0 1 6.95 0" />
                    <circle cx="12" cy="19.5" r="0.8" fill="currentColor" />
                  </svg>
                ),
                title: `${devWord}放在手机旁边`,
                desc: "保持 1 米内，电量充足"
              },
              {
                icon: (
                  <svg viewBox="0 0 24 24" className="w-5 h-5" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <rect x="7" y="3" width="10" height="18" rx="2.5" />
                    <line x1="4" y1="8" x2="6.5" y2="8" />
                    <line x1="17.5" y1="8" x2="20" y2="8" />
                  </svg>
                ),
                title: `确认${devWord}没连着别的手机`,
                desc: "如果连过其他手机，先在那里断开连接"
              }
            ];
            return (
              <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-12 page-enter-anim overflow-y-auto no-scrollbar">
                <div>
                  <button
                    type="button"
                    onClick={() => goBackOr("main/home")}
                    className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 cursor-pointer active:scale-95 transition-all"
                  >
                    <CaretLeft className="w-6 h-6 stroke-[2]" />
                  </button>

                  <div className="pt-6 pb-5">
                    <h1 className="text-[32px] font-bold text-black tracking-tight leading-tight">
                      怎么连上手环
                    </h1>
                    <p className="text-[14px] font-medium text-black/45 mt-2">
                      连不上多半是因为{devWord}还没进入「连接新手机」模式
                    </p>
                  </div>

                  <div className="space-y-3">
                    {steps.map((s, i) => (
                      <div
                        key={i}
                        className="bg-white rounded-3xl p-5 shadow-[0_1px_3px_rgba(0,0,0,0.02)] flex items-start gap-4"
                      >
                        <div className="w-9 h-9 rounded-full bg-[#4361EE]/10 text-[#4361EE] flex items-center justify-center shrink-0">
                          {s.icon}
                        </div>
                        <div className="flex-1 min-w-0">
                          <h3 className="text-[16px] font-bold text-black leading-tight">
                            {i + 1}. {s.title}
                          </h3>
                          <p className="text-[13px] font-medium text-black/45 mt-1">{s.desc}</p>
                        </div>
                      </div>
                    ))}
                  </div>

                  <p className="text-[13px] font-medium text-black/40 mt-4 leading-snug">
                    不同系统版本的菜单名称可能略有差异，找到「连接新手机」即可。
                  </p>

                  {connectError && (
                    <p className="text-[13px] font-bold text-[#FF3B30] mt-4">
                      还是没连上，请确认{devWord}正处于「连接新手机」界面
                    </p>
                  )}
                </div>

                <div className="pt-6">
                  <button
                    type="button"
                    disabled={connecting || !failedDevice}
                    onClick={() => failedDevice && handleSelectDevice(failedDevice)}
                    className="w-full h-[52px] rounded-full bg-[#4361EE] disabled:bg-black/10 disabled:text-black/25 text-white font-bold text-[17px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center tracking-tight"
                  >
                    {connecting ? "正在连接…" : "我已设置好，重新连接"}
                  </button>
                  <div className="flex justify-center mt-4">
                    <button
                      type="button"
                      onClick={(e) => {
                        openLink(e, paidShopUrl || "https://afdian.com/a/Hobjl");
                        showToast("正在前往爱发电…");
                      }}
                      className="text-[13px] font-bold text-black/45 hover:text-black/70 transition-colors cursor-pointer"
                    >
                      还是连不上？联系开发者 ›
                    </button>
                  </div>
                  {/* 找开发者之前先把「版本 + 机型 + 最近问题」复制好：省掉来回问现象 */}
                  <div className="flex justify-center mt-3">
                    <button
                      type="button"
                      onClick={() => void copyDiag()}
                      className="text-[13px] font-bold text-[#4361EE]/80 hover:text-[#4361EE] transition-colors cursor-pointer"
                    >
                      复制诊断信息
                    </button>
                  </div>
                  {diagText && (
                    <textarea
                      readOnly
                      value={diagText}
                      rows={Math.min(8, diagText.split("\n").length + 1)}
                      className="w-full mt-3 text-[12px] font-medium text-black/55 bg-black/[0.03] rounded-2xl p-3 outline-none resize-none select-text leading-relaxed"
                    />
                  )}
                </div>
              </div>
            );
          })()}

          {/* 4. 绑定爱发电（付费表盘入口） */}
          {currentStep === "paid_bind" && (
            <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-12 page-enter-anim">
              <div>
                <button
                  type="button"
                  onClick={() => goBackOr("main/home")}
                  className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 cursor-pointer active:scale-95 transition-all"
                >
                  <CaretLeft className="w-6 h-6 stroke-[2]" />
                </button>

                <div className="pt-6">
                  <h1 className="text-[32px] font-bold text-black tracking-tight leading-tight">
                    绑定爱发电
                  </h1>
                </div>
              </div>

              <div className="my-auto flex flex-col items-center text-center">
                {/* 爱发电 brand 图标（Figma 里画的真商标：紫底 + 白色 logo）；
                    圆角按本页标准由 rounded-2xl 给（56px → 16px），资源里也预裁了 28.5% 兜底 */}
                <img
                  src={afdianIcon}
                  alt=""
                  className="w-14 h-14 shrink-0 rounded-2xl mb-6"
                />

                {paidBound ? (
                  <>
                    <h2 className="text-[22px] font-bold text-black tracking-tight">
                      已绑定
                    </h2>
                    <p className="text-[14px] font-medium text-black/45 mt-1.5">
                      爱发电账号{paidDisplay ? `（尾号 ${paidDisplay}）` : ""}
                    </p>
                  </>
                ) : (
                  <>
                    <h2 className="text-[22px] font-bold text-black tracking-tight">
                      同步已购表盘
                    </h2>
                    <p className="text-[14px] font-medium text-black/45 mt-1.5 max-w-[260px]">
                      绑定你在爱发电买过的表盘，之后每次购买也会自动到账
                    </p>
                  </>
                )}

                {paidError && (
                  <p className="text-[13px] text-[#FF3B30] font-bold mt-4">{paidError}</p>
                )}
              </div>

              <div>
                <button
                  type="button"
                  disabled={paidBusy || (!paidBound && paidMode === "none")}
                  onClick={paidBound ? goToList : handlePaidPrimary}
                  className="w-full h-[52px] rounded-full bg-[#4361EE] disabled:bg-black/10 disabled:text-black/25 text-white font-bold text-[17px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center tracking-tight"
                >
                  {paidBusy
                    ? paidMode === "oauth"
                      ? "等待授权…"
                      : "正在绑定…"
                    : paidBound
                      ? "查看我的表盘"
                      : paidMode === "none"
                        ? "暂不可用"
                        : paidMode === "oauth"
                          ? "用爱发电账号登录"
                          : "去绑定爱发电"}
                </button>
                {!paidBound && paidMode === "oauth" && (
                  <p className="text-[12px] font-medium text-black/35 text-center mt-3 leading-snug">
                    会打开爱发电的授权页，登录自己的账号即可
                  </p>
                )}
                {/* 取得同意的落点（付费页）：购买与使用即表示接受用户协议 */}
                <p className="text-[12px] font-medium text-black/35 text-center mt-2 leading-snug">
                  购买与使用即表示同意
                  <button
                    type="button"
                    onClick={(e) => { openLink(e, `${LEGAL_BASE}/terms`); }}
                    className="text-[#4361EE] font-bold cursor-pointer"
                  >
                    《用户协议》
                  </button>
                </p>
              </div>
            </div>
          )}

          {/* 4.5 我的表盘（已购资源列表） */}
          {currentStep === "paid_dials" && (
            <div className="flex-1 flex flex-col overflow-y-auto no-scrollbar px-7 safe-top pb-12 page-enter-anim">
              <div>
                <button
                  type="button"
                  onClick={() => goBackOr("main/home")}
                  className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 cursor-pointer active:scale-95 transition-all"
                >
                  <CaretLeft className="w-6 h-6 stroke-[2]" />
                </button>

                <div className="pt-6 pb-5">
                  <h1 className="text-[32px] font-bold text-black tracking-tight leading-tight">
                    我的表盘
                  </h1>
                </div>
              </div>

              {paidError && (
                <div className="bg-white rounded-3xl p-5 shadow-[0_1px_3px_rgba(0,0,0,0.02)]">
                  <p className="text-[13px] text-[#FF3B30] font-bold">{paidError}</p>
                  <button
                    type="button"
                    onClick={() => void loadPaidList()}
                    className="mt-3 h-[40px] px-4 rounded-full bg-black/5 hover:bg-black/10 text-black/75 font-bold text-[14px] active:scale-95 transition-all"
                  >
                    重试
                  </button>
                </div>
              )}

              {!paidError && !paidLoaded && (
                <p className="text-[13px] font-medium text-black/40 text-center mt-10">正在同步…</p>
              )}

              {!paidError && paidLoaded && paidProducts.length === 0 && (
                <div className="my-auto flex flex-col items-center text-center pb-16">
                  <div className="w-14 h-14 rounded-2xl bg-black/5 text-black/25 flex items-center justify-center mb-6">
                    {/* 空盒子（线性） */}
                    <svg
                      viewBox="0 0 24 24"
                      className="w-6 h-6"
                      fill="none"
                      stroke="currentColor"
                      strokeWidth="2"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    >
                      <path d="M3 8h18v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" />
                      <path d="M3 8l2-4h14l2 4" />
                      <path d="M10 12h4" />
                    </svg>
                  </div>
                  <h2 className="text-[22px] font-bold text-black tracking-tight">
                    还没有可安装的表盘
                  </h2>
                  <p className="text-[14px] font-medium text-black/45 mt-1.5">
                    去爱发电看看有没有喜欢的
                  </p>
                  {paidShopUrl && (
                    <a
                      href={paidShopUrl}
                      target="_blank"
                      rel="noreferrer"
                      className="mt-6 w-full h-[52px] rounded-full bg-[#4361EE] text-white font-bold text-[17px] active:scale-[0.98] transition-all flex items-center justify-center tracking-tight"
                    >
                      去爱发电看看
                    </a>
                  )}
                </div>
              )}

              {!paidError && paidLoaded && paidProducts.length > 0 && (
                <div className="pb-4">
                  {paidProducts.map(p => {
                    const m = matchPaidVariant(p);
                    const v = m.variant;
                    return (
                      <div
                        key={p.product_id}
                        className="bg-white rounded-3xl p-5 shadow-[0_1px_3px_rgba(0,0,0,0.02)] mb-3"
                      >
                        <div className="flex items-baseline justify-between">
                          <div className="flex items-center gap-2 min-w-0">
                            <h3 className={`text-[17px] font-bold tracking-tight ${v ? "text-black" : "text-black/40"}`}>
                              {p.name}
                            </h3>
                            {/* 有新版本：装过这行才可能标出来（没装过就是「安装」） */}
                            {v?.has_update && (
                              <span className="shrink-0 text-[11px] font-bold text-[#D4973B] bg-[#D4973B]/15 rounded-full px-2 py-[3px] leading-none">
                                可更新
                              </span>
                            )}
                          </div>
                          <span className="shrink-0 text-[13px] font-medium text-black/40">
                            {v?.latest_version ?? ""}
                          </span>
                        </div>
                        <p className="text-[13px] font-medium text-black/40 mt-1">
                          {v
                            ? `适配 ${v.name}${v.installed && v.installed_version ? ` · 已装 ${v.installed_version}` : ""}`
                            : m.screenKnown
                              ? "暂不支持你的设备"
                              : "连接手环后安装"}
                        </p>
                        {v ? (
                          <button
                            type="button"
                            disabled={installPreparing === p.product_id}
                            onClick={() => runPaidInstall(p, v)}
                            className={`mt-4 w-full h-[44px] rounded-full text-white font-bold text-[15px] active:scale-[0.98] transition-all cursor-pointer ${
                              installPreparing === p.product_id ? "bg-[#4361EE]/60" : "bg-[#4361EE]"
                            }`}
                          >
                            {installPreparing === p.product_id
                              ? "准备中…"
                              : v.has_update
                                ? "更新到最新版"
                                : "安装"}
                          </button>
                        ) : (
                          <div className="mt-4 h-[44px] rounded-full bg-black/5 text-black/25 font-bold text-[15px] flex items-center justify-center">
                            暂不可安装
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          )}

          {/* 4.6 爱发电账号管理（设置 → 爱发电账号）：看当前账号 / 退出 / 换账号 */}
          {currentStep === "paid_account" && (
            <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-12 page-enter-anim">
              <div>
                <button
                  type="button"
                  onClick={() => goBackOr("main/settings")}
                  className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 cursor-pointer active:scale-95 transition-all"
                >
                  <CaretLeft className="w-6 h-6 stroke-[2]" />
                </button>

                <div className="pt-6">
                  <h1 className="text-[32px] font-bold text-black tracking-tight leading-tight">
                    爱发电账号
                  </h1>
                </div>
              </div>

              <div className="my-auto flex flex-col items-center text-center">
                {/* 爱发电 brand 图标（Figma 里画的真商标：紫底 + 白色 logo）；
                    圆角按本页标准由 rounded-2xl 给（56px → 16px），资源里也预裁了 28.5% 兜底 */}
                <img
                  src={afdianIcon}
                  alt=""
                  className="w-14 h-14 shrink-0 rounded-2xl mb-6"
                />

                {paidBound ? (
                  <>
                    <h2 className="text-[22px] font-bold text-black tracking-tight">
                      已绑定
                    </h2>
                    <p className="text-[14px] font-medium text-black/45 mt-1.5">
                      爱发电账号{paidDisplay ? `（尾号 ${paidDisplay}）` : ""}
                    </p>
                  </>
                ) : (
                  <>
                    <h2 className="text-[22px] font-bold text-black tracking-tight">
                      未绑定
                    </h2>
                    <p className="text-[14px] font-medium text-black/45 mt-1.5 max-w-[260px]">
                      绑定后即可查看你在爱发电买过的表盘
                    </p>
                  </>
                )}
              </div>

              <div className="flex flex-col gap-3">
                {paidBound ? (
                  <>
                    <button
                      type="button"
                      onClick={() => setPaidAccountConfirm("switch")}
                      className="w-full h-[52px] rounded-full bg-[#4361EE] text-white font-bold text-[17px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center tracking-tight"
                    >
                      登录新的爱发电账号
                    </button>
                    <button
                      type="button"
                      onClick={() => setPaidAccountConfirm("logout")}
                      className="w-full h-[52px] rounded-full bg-black/5 hover:bg-black/10 text-[#FF3B30] font-bold text-[17px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center tracking-tight"
                    >
                      退出登录
                    </button>
                  </>
                ) : (
                  <button
                    type="button"
                    onClick={() => goPaidBindPage()}
                    className="w-full h-[52px] rounded-full bg-[#4361EE] text-white font-bold text-[17px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center tracking-tight"
                  >
                    去绑定爱发电
                  </button>
                )}
              </div>
            </div>
          )}

          {/* 5. 连续安装进度页 */}
          {currentStep === "redeem_installing" && (
            <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-12 page-enter-anim">
              <div />
              <div className="flex flex-col items-center text-center">
                <div className="w-20 h-20 rounded-full bg-[#4361EE]/10 text-[#4361EE] flex items-center justify-center mb-8 relative">
                  <span className="absolute inset-0 rounded-full bg-[#4361EE]/20 animate-ping duration-1000 opacity-60" />
                  <Watch className="w-10 h-10 stroke-[2.2] relative z-10" />
                </div>

                <h2 className="text-[30px] font-bold text-black tracking-tight">
                  {dialName}
                </h2>
                <p className="text-[17px] font-medium text-black/45 mt-2">
                  {progressPhase}
                </p>

                {/* 规范进度条：高 6 · 全圆角 · 轨道 black/10 · 填充 #4361EE */}
                <div className="w-full max-w-[280px] h-1.5 bg-black/10 rounded-full mt-10 overflow-hidden">
                  <div
                    className="h-full bg-[#4361EE] rounded-full transition-all duration-500 ease-out"
                    style={{ width: `${installProgress}%` }}
                  />
                </div>

                {/* C4. 错误态样例 3：安装失败行内错误提示 */}
                {installError ? (
                  <div className="flex flex-col items-center mt-6">
                    <p className="text-[13px] text-[#FF3B30] font-bold text-center">
                      {installError}
                    </p>
                    <button
                      type="button"
                      onClick={() => {
                        // 本地安装失败：用同一份文件字节重跑
                        const pending = localInstall.current;
                        if (installSource === "local" && pending) {
                          runLocalInstall({ ...pending, name: dialName });
                          return;
                        }
                        // 付费安装失败：重新走「认证 → 下载 → 传输」（装上次勾的那几个包体）
                        const paid = paidTarget.current;
                        if (installSource === "paid" && paid) {
                          startPaidInstall(paid.product, paid.variant, paid.fileIds);
                          return;
                        }
                        setInstallError(null);
                      }}
                      className="mt-3 h-[44px] px-5 rounded-full bg-black/5 hover:bg-black/10 text-black/75 font-bold text-[14px] active:scale-95 transition-all"
                    >
                      重试安装
                    </button>
                  </div>
                ) : (
                  <div className="flex flex-col items-center gap-1 mt-3">
                    <span className="text-[13px] font-medium text-black/40">
                      正在安装，请勿退出应用
                    </span>
                    <span className="text-[13px] font-medium text-black/40">
                      保持手环在手机附近
                    </span>
                  </div>
                )}
              </div>
              <div />
            </div>
          )}

          {/* 安装中锁死整块界面：吞掉一切点击/滑动（返回键另在 popstate 里拦），
              装完或出错才放开 —— 避免安装被打断（用户 2026-10-08 要求） */}
          {currentStep === "redeem_installing" && !installError && (
            <div
              className="fixed inset-0 z-[120] touch-none"
              style={{ background: "transparent" }}
              onClick={(e) => {
                e.preventDefault();
                e.stopPropagation();
              }}
              onTouchStart={(e) => {
                e.preventDefault();
                e.stopPropagation();
              }}
              onTouchMove={(e) => e.preventDefault()}
              onPointerDown={(e) => e.preventDefault()}
            />
          )}

          {/* 6. 成功页 */}
          {currentStep === "redeem_success" && (
            <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-10 page-enter-anim">
              <div />
              <div className="flex flex-col items-center text-center">
                <div className="w-20 h-20 rounded-full bg-[#34C759]/15 text-[#34C759] flex items-center justify-center mb-6">
                  <Check className="w-10 h-10 stroke-[2.2]" />
                </div>

                <h2 className="text-[30px] font-bold text-black tracking-tight">
                  {dialName}
                </h2>
                <p className="text-[17px] font-medium text-black/45 mt-2">
                  已应用到你的手环
                </p>
              </div>

              <div>
                {/* 深色按钮（完成）：底 black · 白字 17/700 · 高 52 */}
                <button
                  type="button"
                  onClick={() => {
                    goBackOr("main/home"); // 回进兑换页之前那一页
                    showToast(`「${dialName}」已生效`);
                  }}
                  className="w-full h-[52px] rounded-full bg-black text-white font-bold text-[17px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center tracking-tight"
                >
                  完成
                </button>
              </div>
            </div>
          )}

          {/* 7. 表盘编辑页（全功能壁纸预览与快捷切换）。真机上必须先拿到真实留档：
              没有就明确说「不能编辑」，绝不渲染设计稿的演示数据（假表盘）。 */}
          {currentStep === "dial_edit" && editingDial && invoke && dialSessionState !== "ready" && (
            <div className="flex-1 flex flex-col bg-[#F5F6F9] overflow-hidden page-enter-anim relative">
              <div className="px-6 safe-top pb-2 flex items-center justify-between shrink-0 z-20">
                <button
                  type="button"
                  onClick={() => goBackOr("main/manage")}
                  className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 active:scale-95 transition-all cursor-pointer"
                >
                  <CaretLeft className="w-6 h-6 stroke-[2]" />
                </button>
                <span className="text-[17px] font-bold text-black tracking-tight">{editingDial.name}</span>
                <div className="w-11 h-11" />
              </div>
              <div className="flex-1 flex items-start justify-center px-6 pt-10">
                <div className="w-full max-w-[420px] bg-white rounded-2xl shadow-[0_1px_3px_rgba(0,0,0,0.02)] p-5">
                  {dialSessionState === "loading" || dialSessionState === "idle" ? (
                    <>
                      <div className="text-[15px] font-bold text-black">正在载入表盘…</div>
                      <div className="text-[13px] font-medium text-black/45 mt-1.5 leading-relaxed">
                        正在读取本机留档，请稍候。
                      </div>
                    </>
                  ) : (
                    <>
                      <div className="text-[15px] font-bold text-black">这个表盘不能编辑</div>
                      <div className="text-[13px] font-medium text-black/45 mt-1.5 leading-relaxed">
                        本地没有它的留档（不是通过本 App 安装的，或应用数据被清理过）。
                        到「管理」页用本 App 重新安装一次这个表盘，之后就能编辑了。
                      </div>
                      <button
                        type="button"
                        onClick={() => goBackOr("main/manage")}
                        className="w-full mt-4 h-[44px] rounded-full bg-[#4361EE] text-white font-bold text-[15px] active:scale-[0.98] transition-all cursor-pointer"
                      >
                        返回管理页
                      </button>
                    </>
                  )}
                </div>
              </div>
            </div>
          )}
          {currentStep === "dial_edit" && editingDial && (!invoke || dialSessionState === "ready") && (
            <div className="flex-1 flex flex-col bg-[#F5F6F9] overflow-hidden page-enter-anim relative">
              
              {/* 顶栏返回 */}
              <div className="px-6 safe-top pb-2 flex items-center justify-between shrink-0 z-20">
                <button
                  type="button"
                  onClick={() => goBackOr("main/manage")}
                  className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 cursor-pointer active:scale-95 transition-all"
                >
                  <CaretLeft className="w-6 h-6 stroke-[2]" />
                </button>

                <span className="text-[17px] font-bold text-black tracking-tight">
                  {editingDial.name}
                </span>

                <div className="w-11 h-11" />
              </div>

              {/* 顶部固定：画布 + 设备行 —— 在滚动容器外面，卡片怎么滚都不会叠到画布上 */}
              <div className="px-6 pt-1 pb-3 flex flex-col items-center shrink-0">
                  <div 
                    className="relative rounded-[30px] overflow-hidden flex flex-col justify-between p-3.5 bg-black select-none shadow-[0_8px_24px_rgba(0,0,0,0.14)]"
                    style={{
                      width: `${canvasFrame.w}px`,
                      height: `${canvasFrame.h}px`,
                      borderRadius: `${canvasFrame.radius}px`
                    }}
                  >
                    {renderPreviewUrl ? (
                      /* 真实渲染图：后端按设备像素出图，这里等比显示（contain 不拉伸），圆角由框裁切 */
                      <img
                        src={renderPreviewUrl}
                        alt="表盘预览"
                        className="absolute inset-0 w-full h-full object-contain transition-opacity duration-500"
                      />
                    ) : (
                      <>
                        {/* 背景壁纸实时联动预览 */}
                        {editingDial.isPhotoDial ? (
                          <img
                            src={wallpapers[currentWallpaperIndex] || wallpapers[0] || undefined}
                            alt="表盘壁纸"
                            className="absolute inset-0 w-full h-full object-cover opacity-90 transition-all duration-300"
                          />
                        ) : (
                          <div 
                            className="absolute inset-0 transition-opacity duration-500 opacity-25"
                            style={{
                              background: `radial-gradient(circle at 50% 35%, ${currentColorHex}, transparent 75%)`
                            }}
                          />
                        )}

                        {selectedStyle.includes("息屏常显") && (
                          <div className="absolute inset-0 bg-black/80 backdrop-grayscale z-20 flex flex-col items-center justify-center pointer-events-none">
                            <span className="text-[34px] font-bold text-white font-mono tracking-tighter">09:41</span>
                            <span className="text-[11px] text-white/40 font-medium mt-1">AOD 常亮</span>
                          </div>
                        )}

                        {/* 设计稿那层槽位装饰：按 208×248 基准等比缩放进设备框，避免不同比例下走形 */}
                        <div className="absolute inset-0 flex items-center justify-center pointer-events-none">
                          <div
                            className="relative flex flex-col justify-between p-3.5"
                            style={{
                              width: "208px",
                              height: "248px",
                              flex: "none",
                              transform: `scale(${mockScale})`,
                              transformOrigin: "center center"
                            }}
                          >
                            {/* 表盘【容器 1：顶部状态槽位】（演示画布跟着第 1 张容器卡片的选择变） */}
                            <div className="relative z-10">
                              <div 
                                className={`w-full px-2.5 py-1.5 flex items-center justify-between transition-all duration-300 text-[11px] font-semibold ${
                                  mockTopContainer === "玻璃-细" 
                                    ? "bg-white/10 backdrop-blur-md rounded-full text-white/90 border border-white/15" 
                                    : mockTopContainer === "玻璃-粗"
                                    ? "bg-white/25 backdrop-blur-xl rounded-full text-white border border-white/25"
                                    : mockTopContainer === "普通-中"
                                    ? "bg-[#1C1C1E] rounded-full text-white/85"
                                    : "text-white/80"
                                }`}
                              >
                                <span className="font-mono text-[10px]">10/06 周二</span>
                                <div className="flex items-center gap-1">
                                  <Footprints className="w-3 h-3 text-[#34C759]" />
                                  <span className="font-mono text-[10.5px]">6,820</span>
                                </div>
                              </div>
                            </div>

                            {/* 核心时间展示 */}
                            <div className="relative z-10 flex flex-col items-center justify-center my-auto">
                              <div 
                                className="text-[44px] font-extrabold tracking-tighter font-mono leading-none transition-colors duration-300 drop-shadow-sm"
                                style={{ color: currentColorHex }}
                              >
                                09:41
                              </div>
                              <span className="text-[11px] font-bold text-white/80 tracking-widest uppercase mt-0.5">
                                XIAOMI FIT
                              </span>
                            </div>

                            {/* 表盘【容器 2：底部数据槽位】（跟着第 2 张容器卡片的选择变） */}
                            <div className="relative z-10">
                              <div 
                                className={`w-full p-2 flex items-center justify-around transition-all duration-300 ${
                                  mockBottomContainer === "玻璃-细" 
                                    ? "bg-white/10 backdrop-blur-md rounded-2xl border border-white/15 text-white" 
                                    : mockBottomContainer === "玻璃-粗"
                                    ? "bg-white/25 backdrop-blur-xl rounded-2xl border border-white/25 text-white"
                                    : mockBottomContainer === "普通-中"
                                    ? "bg-[#1C1C1E] rounded-2xl text-white/90"
                                    : "text-white"
                                }`}
                              >
                                <div className="flex items-center gap-1 text-[11px] font-bold">
                                  <Flame className="w-3 h-3 text-[#D4973B]" />
                                  <span>420 kcal</span>
                                </div>
                                <div className="w-[1px] h-3 bg-white/15" />
                                <div className="flex items-center gap-1 text-[11px] font-bold">
                                  <Heart className="w-3 h-3 text-[#E63946] fill-[#E63946]" />
                                  <span>76 bpm</span>
                                </div>
                              </div>
                            </div>
                          </div>
                        </div>
                      </>
                    )}

                  </div>

                  {/* 设备标识行（设计稿没有，按 §5.2 补）：{设备名} · {宽}×{高} */}
                  <div
                    className={`pt-1.5 text-center text-[12px] font-medium ${
                      canvasSpecMismatch ? "text-[#D4973B]" : "text-black/40"
                    }`}
                  >
                    {canvasLabel}
                  </div>

                  {/* 表盘与当前设备规格不一致：该行变黄并给告警文案 */}
                  {canvasSpecMismatch && (
                    <div className="text-center text-[12px] font-medium text-[#D4973B] mt-1">
                      {`该表盘为 ${canvasSpecMismatch.dial.width}×${canvasSpecMismatch.dial.height}，与当前设备 ${canvasSpecMismatch.device.width}×${canvasSpecMismatch.device.height} 不一致`}
                    </div>
                  )}
              </div>

              {/* 可滑动编辑主体区：只滚下面的配置卡片 */}
              <div className="flex-1 overflow-y-auto no-scrollbar px-6 pb-32">
                {/* 2. 手风琴式展开配置区（收紧版间距） */}
                <div className="flex flex-col gap-2.5">
                  
                  {/* 上传壁纸卡片（仅照片表盘显示，支持 10+ 张正方形预览与快捷点击切换） */}
                  {editingDial.isPhotoDial && (
                    <div className="bg-white rounded-3xl p-4 shadow-[0_1px_3px_rgba(0,0,0,0.02)] flex flex-col gap-2.5">
                      <div className="flex items-center justify-between">
                        <div>
                          <span className="text-[16px] font-bold text-black tracking-tight">
                            上传壁纸
                          </span>
                        </div>

                        {/* 上传新增入口 */}
                        <button
                          type="button"
                          onClick={handleAddWallpaper}
                          className="flex items-center gap-1.5 px-3.5 h-[32px] rounded-full bg-[#4361EE]/10 text-[#4361EE] hover:bg-[#4361EE]/15 active:scale-95 transition-all text-[13px] font-bold cursor-pointer"
                        >
                          <Plus className="w-3.5 h-3.5 stroke-[2.5]" />
                          <span>添加照片</span>
                        </button>
                      </div>

                      {/* 正方形壁纸缩略图横向滑动网格（带 C5 空态样例 3：无照片） */}
                      {wallpapers.length === 0 ? (
                        <div className="py-6 flex flex-col items-center justify-center text-center">
                          <p className="text-[14px] font-medium text-black/40 mb-3">
                            还没有照片，点下方「添加照片」开始
                          </p>
                          <button
                            type="button"
                            onClick={handleAddWallpaper}
                            className="h-[44px] px-5 rounded-full bg-black/5 hover:bg-black/10 text-black/75 font-bold text-[14px] active:scale-95 transition-all cursor-pointer"
                          >
                            添加照片
                          </button>
                        </div>
                      ) : (
                        <div className="flex items-center gap-2.5 overflow-x-auto no-scrollbar py-1">
                          {wallpapers.map((url, idx) => {
                            const isSelected = currentWallpaperIndex === idx;
                            return (
                              <button
                                key={idx}
                                type="button"
                                onClick={() => {
                                  setCurrentWallpaperIndex(idx);
                                  showToast(`已切换至壁纸第 ${idx + 1} 张`);
                                }}
                                className={`relative w-15 h-15 rounded-xl overflow-hidden shrink-0 transition-all cursor-pointer active:scale-95 group ${
                                  isSelected
                                    ? "ring-2 ring-[#4361EE] scale-105"
                                    : "opacity-75 hover:opacity-100 ring-1 ring-black/5"
                                }`}
                              >
                                <img
                                  src={url}
                                  alt={`壁纸缩略图 ${idx + 1}`}
                                  className="w-full h-full object-cover"
                                />
                                {/* 删除按钮（右上角小圆叉）：阻断外层的「点击切换」 */}
                                <span
                                  onClick={e => {
                                    e.stopPropagation();
                                    handleRemoveWallpaper(idx);
                                  }}
                                  className="absolute top-1 right-1 w-6 h-6 rounded-full bg-black/55 text-white flex items-center justify-center active:scale-90 transition-transform cursor-pointer"
                                >
                                  <X className="w-3.5 h-3.5 stroke-[2.5]" />
                                </span>

                                {/* 选中勾号角标 */}
                                {isSelected && (
                                  <div className="absolute inset-0 bg-[#4361EE]/20 flex items-center justify-center">
                                    <div className="w-5 h-5 rounded-full bg-[#4361EE] text-white flex items-center justify-center shadow-xs">
                                      <Check className="w-3.5 h-3.5 stroke-[2]" />
                                    </div>
                                  </div>
                                )}
                              </button>
                            );
                          })}
                        </div>
                      )}
                    </div>
                  )}

                  {/* 卡片 1：表盘样式 */}
                  <div
                    id="acc-style"
                    className={`rounded-2xl transition-all duration-300 overflow-hidden ${
                      openSection === "style"
                        ? "bg-white ring-2 ring-[#4361EE]"
                        : "bg-white shadow-[0_1px_3px_rgba(0,0,0,0.02)] hover:bg-white/90"
                    }`}
                  >
                    <button
                      type="button"
                      onClick={() => toggleSection("style")}
                      className="w-full text-left p-3.5 flex items-center justify-between cursor-pointer"
                    >
                      <div>
                        <span className={`text-[12px] font-bold block transition-colors ${
                          openSection === "style" ? "text-[#4361EE]" : "text-black/45"
                        }`}>
                          表盘样式
                        </span>
                        <h4 className="text-[15px] font-bold text-black mt-0.5">
                          {selectedStyle}
                        </h4>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className={`text-[12px] font-semibold px-2 py-0.5 rounded-full transition-colors ${
                          openSection === "style" ? "bg-[#4361EE]/10 text-[#4361EE]" : "text-black/35"
                        }`}>
                          {`${styleOptions.length} 种样式`}
                        </span>
                        <div className={`w-6 h-6 rounded-full flex items-center justify-center transition-transform duration-300 ${
                          openSection === "style" ? "rotate-180 text-[#4361EE]" : "text-black/30"
                        }`}>
                          <CaretDown className="w-4 h-4 stroke-[2.2]" />
                        </div>
                      </div>
                    </button>

                    {openSection === "style" && (
                      <div className="px-4.5 pb-4 pt-1 border-t border-black/[0.05] animate-in fade-in slide-in-from-top-2 duration-200">
                        <div className="max-h-[200px] overflow-y-auto no-scrollbar flex flex-col gap-1 py-1">
                          {styleOptions.map((st) => {
                            const isSelected = selectedStyle === st;
                            return (
                              <button
                                key={st}
                                type="button"
                                onClick={() => {
                                  setSelectedStyle(st);
                                  // 配色跟随样式：新样式没有当前选中的配色时，回落到它的第一个可用配色
                                  const th = (binSession.current?.model?.themes || []).find(
                                    t => t.kind === "normal" && t.index === themeIndexOf(st)
                                  );
                                  const opt = dialOptions.current;
                                  if (th?.colorGroups?.length && opt) {
                                    const avail = opt.colorNames.filter((_, i) => {
                                      const cid = opt.colorIds[i];
                                      return cid != null && th.colorGroups.includes(cid);
                                    });
                                    if (avail.length && !avail.includes(selectedColorName)) {
                                      setSelectedColorName(avail[0]);
                                    }
                                  }
                                }}
                                className={`w-full py-2 px-3 rounded-xl text-left flex items-center justify-between text-[14px] font-semibold transition-all cursor-pointer ${
                                  isSelected
                                    ? "bg-[#4361EE] text-white"
                                    : "bg-[#F5F6F9] hover:bg-[#EAEAEF] text-black/80"
                                }`}
                              >
                                <span>{st}</span>
                                {isSelected && <Check className="w-4 h-4 stroke-[2.5]" />}
                              </button>
                            );
                          })}
                        </div>
                        <button
                          type="button"
                          onClick={() => setOpenSection(null)}
                          className="w-full mt-1.5 py-1.5 text-center text-[12px] font-bold text-[#4361EE] hover:bg-[#4361EE]/5 rounded-lg transition-colors cursor-pointer"
                        >
                          收起选项
                        </button>
                      </div>
                    )}
                  </div>

                  {/* 卡片 2：主题配色 —— 只有一种配色的表盘/样式不显示这张卡 */}
                  {visibleColors.length >= 2 && (
                  <div
                    id="acc-color"
                    className={`rounded-2xl transition-all duration-300 overflow-hidden ${
                      openSection === "color"
                        ? "bg-white ring-2 ring-[#4361EE]"
                        : "bg-white shadow-[0_1px_3px_rgba(0,0,0,0.02)] hover:bg-white/90"
                    }`}
                  >
                    <button
                      type="button"
                      onClick={() => toggleSection("color")}
                      className="w-full text-left p-3.5 flex items-center justify-between cursor-pointer"
                    >
                      <div>
                        <span className={`text-[12px] font-bold block transition-colors ${
                          openSection === "color" ? "text-[#4361EE]" : "text-black/45"
                        }`}>
                          主题配色
                        </span>
                        <h4 className="text-[15px] font-bold text-black mt-0.5">
                          {selectedColorName}
                        </h4>
                      </div>
                      <div className="flex items-center gap-2">
                        <span className={`text-[12px] font-semibold px-2 py-0.5 rounded-full transition-colors ${
                          openSection === "color" ? "bg-[#4361EE]/10 text-[#4361EE]" : "text-black/35"
                        }`}>
                          {`${visibleColors.length} 种配色`}
                        </span>
                        <div className={`w-6 h-6 rounded-full flex items-center justify-center transition-transform duration-300 ${
                          openSection === "color" ? "rotate-180 text-[#4361EE]" : "text-black/30"
                        }`}>
                          <CaretDown className="w-4 h-4 stroke-[2.2]" />
                        </div>
                      </div>
                    </button>

                    {openSection === "color" && (
                      <div className="px-4.5 pb-4 pt-1 border-t border-black/[0.05] animate-in fade-in slide-in-from-top-2 duration-200">
                        <div className="max-h-[200px] overflow-y-auto no-scrollbar flex flex-col gap-1 py-1">
                          {visibleColors.map((col) => {
                            const isSelected = selectedColorName === col.name;
                            return (
                              <button
                                key={col.name}
                                type="button"
                                onClick={() => setSelectedColorName(col.name)}
                                className={`w-full py-2 px-3 rounded-xl text-left flex items-center justify-between text-[14px] font-semibold transition-all cursor-pointer ${
                                  isSelected
                                    ? "bg-[#4361EE] text-white"
                                    : "bg-[#F5F6F9] hover:bg-[#EAEAEF] text-black/80"
                                }`}
                              >
                                <span>{col.name}</span>
                                {isSelected && <Check className="w-4 h-4 stroke-[2.5]" />}
                              </button>
                            );
                          })}
                        </div>
                        <button
                          type="button"
                          onClick={() => setOpenSection(null)}
                          className="w-full mt-1.5 py-1.5 text-center text-[12px] font-bold text-[#4361EE] hover:bg-[#4361EE]/5 rounded-lg transition-colors cursor-pointer"
                        >
                          收起选项
                        </button>
                      </div>
                    )}
                  </div>
                  )}

                  {/* 卡片 3..N：容器卡片 —— 只渲染当前样式引用的槽位（每个样式有自己的容器集合），下滑可见数量不写死 */}
                  {visibleContainers.map((card, idx) => {
                    // 手风琴 id / key 按槽位定位：切换样式后列表成员变化，按可见下标会串卡
                    const section: AccordionSection = `container-${card.slot ?? idx}`;
                    const isOpen = openSection === section;
                    return (
                      <div
                        key={`container-${card.slot ?? idx}`}
                        id={`acc-container-${card.slot ?? idx}`}
                        className={`rounded-2xl transition-all duration-300 overflow-hidden ${
                          isOpen
                            ? "bg-white ring-2 ring-[#4361EE]"
                            : "bg-white shadow-[0_1px_3px_rgba(0,0,0,0.02)] hover:bg-white/90"
                        }`}
                      >
                        <button
                          type="button"
                          onClick={() => toggleSection(section)}
                          className="w-full text-left p-3.5 flex items-center justify-between cursor-pointer"
                        >
                          <div>
                            <span className={`text-[12px] font-bold block transition-colors ${
                              isOpen ? "text-[#4361EE]" : "text-black/45"
                            }`}>
                              {`容器 ${idx + 1} · ${card.label}`}
                            </span>
                            <h4 className="text-[15px] font-bold text-black mt-0.5">
                              {card.selected}
                            </h4>
                          </div>
                          <div className="flex items-center gap-2">
                            <span className={`text-[12px] font-semibold px-2 py-0.5 rounded-full transition-colors ${
                              isOpen ? "bg-[#4361EE]/10 text-[#4361EE]" : "text-black/35"
                            }`}>
                              {`${card.options.length} 种容器`}
                            </span>
                            <div className={`w-6 h-6 rounded-full flex items-center justify-center transition-transform duration-300 ${
                              isOpen ? "rotate-180 text-[#4361EE]" : "text-black/30"
                            }`}>
                              <CaretDown className="w-4 h-4 stroke-[2.2]" />
                            </div>
                          </div>
                        </button>

                        {isOpen && (
                          <div className="px-4.5 pb-4 pt-1 border-t border-black/[0.05] animate-in fade-in slide-in-from-top-2 duration-200">
                            <div className="max-h-[200px] overflow-y-auto no-scrollbar flex flex-col gap-1 py-1">
                              {card.options.map((con, oi) => {
                                // 选中判定按**动作序号**：两个候选同名时也能各自独立高亮
                                const isSelected =
                                  card.selectedId != null
                                    ? card.ids[oi] === card.selectedId
                                    : card.selected === con;
                                return (
                                  <button
                                    key={card.slot != null ? `${card.slot}:${card.ids[oi] ?? oi}` : `${card.label}:${oi}`}
                                    type="button"
                                    onClick={() => selectContainerAt(card.slot, card.label, oi)}
                                    className={`w-full py-2 px-3 rounded-xl text-left flex items-center justify-between text-[14px] font-semibold transition-all cursor-pointer ${
                                      isSelected
                                        ? "bg-[#4361EE] text-white"
                                        : "bg-[#F5F6F9] hover:bg-[#EAEAEF] text-black/80"
                                    }`}
                                  >
                                    <span>{con}</span>
                                    {isSelected && <Check className="w-4 h-4 stroke-[2.5]" />}
                                  </button>
                                );
                              })}
                            </div>
                            <button
                              type="button"
                              onClick={() => setOpenSection(null)}
                              className="w-full mt-1.5 py-1.5 text-center text-[12px] font-bold text-[#4361EE] hover:bg-[#4361EE]/5 rounded-lg transition-colors cursor-pointer"
                            >
                              收起选项
                            </button>
                          </div>
                        )}
                      </div>
                    );
                  })}

                </div>
              </div>

              {/* 3. 底部固定主按钮「应用」（不透明纯色底 + 原有分隔线） */}
              <div data-pf-bottombar className="absolute bottom-0 left-0 right-0 p-5 safe-bottom-bar bg-white border-t border-black/5 flex flex-col pt-3 z-30">
                <button
                  type="button"
                  onClick={handleApplyDialConfig}
                  disabled={isApplying || appliedSuccess}
                  className="w-full h-[52px] rounded-full bg-[#4361EE] disabled:bg-[#34C759] text-white font-bold text-[17px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center gap-2 shadow-[0_8px_24px_rgba(0,0,0,0.10)]"
                >
                  {isApplying ? (
                    <>
                      <CircleNotch className="w-5 h-5 animate-spin" />
                      <span>正在同步到手环...</span>
                    </>
                  ) : appliedSuccess ? (
                    <>
                      <Check className="w-5 h-5 stroke-[2]" />
                      <span>已应用至手环</span>
                    </>
                  ) : (
                    <span>应用</span>
                  )}
                </button>
              </div>

            </div>
          )}

          {/* 7.5 图片裁剪与旋转页面 */}
          {currentStep === "crop_image" && (
            <div className="flex-1 flex flex-col justify-between overflow-hidden bg-[#0A0A0C] text-white page-enter-anim select-none relative">
              {/* 顶部轻质导航栏 */}
              <div className="flex items-center justify-between px-5 safe-top pb-3 z-20 bg-linear-to-b from-[#0A0A0C] via-[#0A0A0C]/90 to-transparent">
                <button
                  type="button"
                  onClick={() => goBackOr("main/manage")}
                  className="h-9 px-4 rounded-full bg-white/10 hover:bg-white/15 active:scale-95 transition-all text-white/90 hover:text-white text-[14px] font-semibold cursor-pointer flex items-center justify-center backdrop-blur-md"
                >
                  取消
                </button>
                <div className="flex flex-col items-center">
                  <span className="text-[16px] font-bold tracking-tight text-white">
                    移动和缩放
                  </span>
                  <span className="text-[11px] text-white/40 font-medium">
                    拖动画面调整表盘显示
                  </span>
                </div>
                <button
                  type="button"
                  onClick={handleCropConfirm}
                  className="h-9 px-4 rounded-full bg-[#4361EE] hover:bg-[#3651D4] text-white text-[14px] font-bold cursor-pointer active:scale-95 transition-all shadow-[0_2px_8px_rgba(67,97,238,0.35)] flex items-center justify-center"
                >
                  完成
                </button>
              </div>

              {/* 核心视口与裁剪遮罩 */}
              <div 
                ref={cropViewRef}
                className="flex-1 relative flex items-center justify-center overflow-hidden cursor-grab active:cursor-grabbing touch-none"
                onMouseDown={(e) => {
                  isCropDragging.current = true;
                  cropDragStart.current = { x: e.clientX - cropOffset.x, y: e.clientY - cropOffset.y };
                }}
                onMouseMove={(e) => {
                  if (!isCropDragging.current) return;
                  setCropOffset({
                    x: e.clientX - cropDragStart.current.x,
                    y: e.clientY - cropDragStart.current.y
                  });
                }}
                onMouseUp={() => { isCropDragging.current = false; }}
                onMouseLeave={() => { isCropDragging.current = false; }}
                onTouchStart={(e) => {
                  isCropDragging.current = true;
                  cropDragStart.current = { 
                    x: e.touches[0].clientX - cropOffset.x, 
                    y: e.touches[0].clientY - cropOffset.y 
                  };
                }}
                onTouchMove={(e) => {
                  if (!isCropDragging.current) return;
                  setCropOffset({
                    x: e.touches[0].clientX - cropDragStart.current.x,
                    y: e.touches[0].clientY - cropDragStart.current.y
                  });
                }}
                onTouchEnd={() => { isCropDragging.current = false; }}
              >
                {/* 底图：canvas 2D 绘制（支持拖拽平移、无极旋转及无极缩放；
                    安卓/vivo WebView 上 <img> + CSS transform 会黑屏，故用 canvas） */}
                <div className="absolute inset-0 pointer-events-none flex items-center justify-center">
                  <canvas
                    ref={cropCanvasRef}
                    className="block pointer-events-none select-none"
                    style={{
                      width: `${cropFrame.w}px`,
                      height: `${cropFrame.h}px`,
                      borderRadius: `${cropFrame.radius}px`
                    }}
                  />
                </div>

                {/* 半透明蒙层 + 按设备屏幕比例的视口镂空框 */}
                <div className="absolute inset-0 pointer-events-none flex items-center justify-center">
                  <div
                    className="w-[208px] h-[248px] rounded-[30px] ring-[999px] ring-black/80 relative shadow-[inset_0_0_0_1.5px_rgba(255,255,255,0.7)]"
                    style={{
                      width: `${cropFrame.w}px`,
                      height: `${cropFrame.h}px`,
                      borderRadius: `${cropFrame.radius}px`
                    }}
                  >
                    {/* 九宫格辅助参考线 */}
                    <div className="absolute inset-0 grid grid-cols-3 grid-rows-3 pointer-events-none opacity-40">
                      <div className="border-r border-b border-white/30" />
                      <div className="border-r border-b border-white/30" />
                      <div className="border-b border-white/30" />
                      <div className="border-r border-b border-white/30" />
                      <div className="border-r border-b border-white/30" />
                      <div className="border-b border-white/30" />
                      <div className="border-r border-white/30" />
                      <div className="border-r border-white/30" />
                      <div />
                    </div>
                  </div>
                </div>
              </div>

              {/* 底部无极进度条控制面板 */}
              <div data-pf-bottombar className="px-6 pt-4 safe-bottom-panel bg-linear-to-t from-black via-[#0A0A0C]/95 to-transparent flex flex-col gap-4 z-20">
                {/* 控制条 1：无极缩放 Slider */}
                <div className="flex items-center gap-3">
                  <div className="flex items-center gap-1.5 w-16 shrink-0 text-white/60">
                    <MagnifyingGlassPlus className="w-3.5 h-3.5" />
                    <span className="text-[12px] font-medium">缩放</span>
                  </div>
                  <input
                    type="range"
                    min="0.8"
                    max="3.0"
                    step="0.01"
                    value={cropScale}
                    onChange={(e) => setCropScale(parseFloat(e.target.value))}
                    className="flex-1 h-1.5 bg-white/20 rounded-lg appearance-none cursor-pointer accent-[#4361EE]"
                  />
                  <span className="text-[12px] font-mono font-medium text-white/80 w-12 text-right">
                    {cropScale.toFixed(2)}x
                  </span>
                </div>

                {/* 控制条 2：无极旋转 Slider */}
                <div className="flex items-center gap-3">
                  <div className="flex items-center gap-1.5 w-16 shrink-0 text-white/60">
                    <ArrowClockwise className="w-3.5 h-3.5" />
                    <span className="text-[12px] font-medium">旋转</span>
                  </div>
                  <input
                    type="range"
                    min="-180"
                    max="180"
                    step="1"
                    value={cropRotation}
                    onChange={(e) => setCropRotation(parseInt(e.target.value, 10))}
                    className="flex-1 h-1.5 bg-white/20 rounded-lg appearance-none cursor-pointer accent-[#4361EE]"
                  />
                  <span className="text-[12px] font-mono font-medium text-white/80 w-12 text-right">
                    {cropRotation > 0 ? `+${cropRotation}°` : `${cropRotation}°`}
                  </span>
                </div>

                {/* 独立重置按钮栏 */}
                <div className="flex items-center justify-between pt-1">
                  <div className="text-[11px] text-white/40 font-medium">
                    双手双指或滑块无极调节
                  </div>
                  <button
                    type="button"
                    onClick={() => {
                      setCropOffset({ x: 0, y: 0 });
                      setCropRotation(0);
                      setCropScale(1);
                    }}
                    className="flex items-center gap-1.5 px-3.5 py-1.5 rounded-full bg-white/10 hover:bg-white/15 active:scale-95 transition-all text-white/80 hover:text-white text-[12px] font-semibold cursor-pointer"
                  >
                    <ArrowCounterClockwise className="w-3.5 h-3.5" />
                    <span>重置参数</span>
                  </button>
                </div>

                <div className="h-1.5 flex items-center justify-center pt-2">
                  <div className="w-32 h-1 bg-white/20 rounded-full" />
                </div>
              </div>
            </div>
          )}

          {/* 8. 关于应用主页（一级页面，简约清晰） */}
          {currentStep === "about_app" && (
            <div className="flex-1 flex flex-col justify-between overflow-y-auto no-scrollbar px-6 safe-top pb-8 page-enter-anim select-none">
              <div>
                {/* 顶部返回导航 */}
                <div className="flex items-center justify-between pb-4">
                  <button
                    type="button"
                    onClick={() => goBackOr("main/settings")}
                    className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 cursor-pointer active:scale-95 transition-all"
                  >
                    <CaretLeft className="w-6 h-6 stroke-[2]" />
                  </button>
                  <span className="text-[17px] font-bold text-black tracking-tight">
                    关于应用
                  </span>
                  <div className="w-11 h-11" />
                </div>

                {/* 应用主视觉：图标 + 盘坊 + v0.1.0 */}
                <div className="flex flex-col items-center text-center pt-5 pb-6">
                  {/* 参考参考图风格重新设计的应用图标：清透蓝青双色微光背景 + 悬浮白透毛玻璃表盘 + 立体卷角折纸与核心刻度 */}
                  {/* 应用图标：与桌面图标同一套手绘设计稿（Figma 节点 21:77 导出，白底 + 内容 62%） */}
                  <div className="w-24 h-24 rounded-[26px] overflow-hidden relative mb-4 select-none shadow-[0_14px_34px_rgba(0,0,0,0.12),0_2px_8px_rgba(0,0,0,0.04)]">
                    <img src={appIcon} alt="盘坊" className="w-full h-full object-cover" />
                  </div>
                  <h2 className="text-[24px] font-bold text-black tracking-tight">
                    盘坊
                  </h2>
                  <span className="text-[13px] font-semibold text-black/40 mt-1">
                    v{appVersion ?? "0.3.20"}
                  </span>
                </div>

                {/* 开源声明行：「本应用以 AGPL-3.0 分发 · 查看源码」 */}
                <div className="text-center pb-6 px-4">
                  <a
                    href="https://github.com/Hobjl-lian/panfang"
                    target="_blank"
                    rel="noopener noreferrer"
                    onClick={(e) => {
                      openLink(e, "https://github.com/Hobjl-lian/panfang");
                      showToast("正在前往 GitHub 源码仓库...");
                    }}
                    className="inline-flex items-center gap-1.5 text-[13px] font-medium text-black/50 hover:text-[#4361EE] transition-colors group cursor-pointer"
                  >
                    <span>本应用以 AGPL-3.0 分发 · 查看源码</span>
                    <ArrowSquareOut className="w-3.5 h-3.5 stroke-[2] text-black/35 group-hover:text-[#4361EE]" />
                  </a>
                </div>

                {/* 功能与详情列表组（包含关于开发者与开源许可） */}
                <div className="bg-white rounded-3xl overflow-hidden shadow-[0_1px_3px_rgba(0,0,0,0.02)] border border-black/[0.02] divide-y divide-black/[0.05]">
                  {/* 一级入口：检查更新（App 自更新通道；发现新版本时副标题变蓝） */}
                  <button
                    type="button"
                    onClick={() => void runAppUpdateCheck(false)}
                    className="w-full p-4.5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer group"
                  >
                    <div className="flex items-center gap-3.5 min-w-0">
                      <div className="w-9 h-9 shrink-0 rounded-xl bg-[#4361EE]/10 text-[#4361EE] flex items-center justify-center">
                        <ArrowClockwise className="w-5 h-5 stroke-[2]" />
                      </div>
                      <div className="min-w-0">
                        <h4 className="text-[16px] font-bold text-black leading-tight">
                          检查更新
                        </h4>
                        <p
                          className={`text-[13px] font-medium mt-0.5 ${
                            appUpdateKnown ? "text-[#4361EE]" : "text-black/40"
                          }`}
                        >
                          {appUpdateKnown
                            ? `有新版本 v${appUpdateKnown}`
                            : `当前版本 v${appVersion ?? "0.3.20"}`}
                        </p>
                      </div>
                    </div>
                    <CaretRight className="w-5 h-5 shrink-0 text-black/25 stroke-[2] group-hover:text-black/50 transition-colors" />
                  </button>

                  {/* 二级入口：关于开发者 */}
                  <button
                    type="button"
                    onClick={() => pushRoute("about/developer")}
                    className="w-full p-4.5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer group"
                  >
                    <div className="flex items-center gap-3.5 min-w-0">
                      <img
                        src={developerAvatar}
                        alt="Hobjl"
                        className="w-9 h-9 shrink-0 rounded-full object-cover ring-2 ring-[#4361EE]/15 shadow-sm"
                      />
                      <div className="min-w-0">
                        <h4 className="text-[16px] font-bold text-black leading-tight">
                          关于开发者
                        </h4>
                        <p className="text-[13px] font-medium text-black/40 mt-0.5">
                          Hobjl · 小米官方表盘设计师
                        </p>
                      </div>
                    </div>
                    <CaretRight className="w-5 h-5 shrink-0 text-black/25 stroke-[2] group-hover:text-black/50 transition-colors" />
                  </button>

                  {/* 二级入口：开源许可与致谢 */}
                  <button
                    type="button"
                    onClick={() => pushRoute("about/licenses")}
                    className="w-full p-4.5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer group"
                  >
                    <div className="flex items-center gap-3.5 min-w-0">
                      <div className="w-9 h-9 shrink-0 rounded-xl bg-black/5 flex items-center justify-center text-black/70">
                        <FileText className="w-5 h-5 stroke-[2]" />
                      </div>
                      <div className="min-w-0">
                        <h4 className="text-[16px] font-bold text-black leading-tight">
                          开源许可与致谢
                        </h4>
                        <p className="text-[13px] font-medium text-black/40 mt-0.5">
                          AGPL-3.0 协议及 AstroBox-NG 致谢
                        </p>
                      </div>
                    </div>
                    <CaretRight className="w-5 h-5 shrink-0 text-black/25 stroke-[2] group-hover:text-black/50 transition-colors" />
                  </button>

                  {/* 外部链接：隐私政策（挂在已备案域名上，App 内只做跳转） */}
                  <button
                    type="button"
                    onClick={(e) => {
                      openLink(e, `${LEGAL_BASE}/privacy`);
                      showToast("正在打开隐私政策…");
                    }}
                    className="w-full p-4.5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer group"
                  >
                    <div className="flex items-center gap-3.5 min-w-0">
                      <div className="w-9 h-9 shrink-0 rounded-xl bg-black/5 flex items-center justify-center text-black/70">
                        <ShieldCheck className="w-5 h-5 stroke-[2]" />
                      </div>
                      <div className="min-w-0">
                        <h4 className="text-[16px] font-bold text-black leading-tight">
                          隐私政策
                        </h4>
                        <p className="text-[13px] font-medium text-black/40 mt-0.5">
                          收集什么、存在哪里、保留多久
                        </p>
                      </div>
                    </div>
                    <ArrowSquareOut className="w-5 h-5 shrink-0 text-black/25 stroke-[2] group-hover:text-black/50 transition-colors" />
                  </button>

                  {/* 外部链接：用户协议 */}
                  <button
                    type="button"
                    onClick={(e) => {
                      openLink(e, `${LEGAL_BASE}/terms`);
                      showToast("正在打开用户协议…");
                    }}
                    className="w-full p-4.5 flex items-center justify-between text-left active:bg-black/[0.02] transition-colors cursor-pointer group"
                  >
                    <div className="flex items-center gap-3.5 min-w-0">
                      <div className="w-9 h-9 shrink-0 rounded-xl bg-black/5 flex items-center justify-center text-black/70">
                        <Scroll className="w-5 h-5 stroke-[2]" />
                      </div>
                      <div className="min-w-0">
                        <h4 className="text-[16px] font-bold text-black leading-tight">
                          用户协议
                        </h4>
                        <p className="text-[13px] font-medium text-black/40 mt-0.5">
                          授权范围、付费与内容使用说明
                        </p>
                      </div>
                    </div>
                    <ArrowSquareOut className="w-5 h-5 shrink-0 text-black/25 stroke-[2] group-hover:text-black/50 transition-colors" />
                  </button>
                </div>
              </div>

              {/* 底部极简版权 */}
              <div className="text-center pt-6 pb-2">
                <p className="text-[12px] font-medium text-black/25">
                  Designed & Developed by Hobjl
                </p>
              </div>
            </div>
          )}

          {/* 9. 关于开发者界面（二级页：克制、留白的 Apple 极简风格） */}
          {currentStep === "about_developer" && (
            <div className="flex-1 flex flex-col justify-between px-7 safe-top pb-12 page-enter-anim select-none">
              {/* 顶部返回导航 */}
              <div className="flex items-center justify-between">
                <button
                  type="button"
                  onClick={() => goBackOr("about")}
                  className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 cursor-pointer active:scale-95 transition-all"
                >
                  <CaretLeft className="w-6 h-6 stroke-[2]" />
                </button>
                <span className="text-[17px] font-bold text-black tracking-tight">
                  关于开发者
                </span>
                <div className="w-11 h-11" />
              </div>

              {/* 中间核心留白信息区 */}
              <div className="flex flex-col items-center text-center my-auto px-2">
                {/* 开发者头像 */}
                <div className="mb-6">
                  <img
                    src={developerAvatar}
                    alt="Hobjl"
                    className="w-24 h-24 rounded-full object-cover shadow-[0_1px_3px_rgba(0,0,0,0.02)]"
                  />
                </div>

                {/* 名字 */}
                <h2 className="text-[26px] font-bold text-black tracking-tight">
                  Hobjl
                </h2>

                {/* 身份小标签 */}
                <p className="text-[14px] font-bold text-[#4361EE] mt-1.5">
                  开发者
                </p>

                {/* 简介文案：自然留白，纯净排版 */}
                <p className="text-[14px] font-medium text-black/45 leading-relaxed mt-6 max-w-[270px]">
                  小米官方表盘设计师，米坛社区认证创作者。无论是否发电，希望你开心。
                </p>
              </div>

              {/* 底部极简交互区：跳转爱发电 */}
              <div className="w-full">
                <a
                  href="https://afdian.com/a/Hobjl?utm_source=copylink&utm_medium=link"
                  target="_blank"
                  rel="noopener noreferrer"
                  onClick={(e) => {
                    openLink(e, "https://afdian.com/a/Hobjl?utm_source=copylink&utm_medium=link");
                    showToast("正在前往爱发电平台...");
                  }}
                  className="w-full h-[52px] rounded-full bg-[#4361EE] hover:bg-[#3853db] text-white font-bold text-[17px] active:scale-[0.98] transition-all flex items-center justify-center gap-2 cursor-pointer shadow-[0_8px_24px_rgba(0,0,0,0.10)] tracking-tight"
                >
                  <span>前往爱发电</span>
                  <ArrowSquareOut className="w-5 h-5 stroke-[2]" />
                </a>
                {/* 找开发者时先把「版本 + 机型 + 最近问题」带上（比描述现象有用得多） */}
                <div className="flex justify-center mt-4">
                  <button
                    type="button"
                    onClick={() => void copyDiag()}
                    className="text-[13px] font-bold text-black/45 hover:text-black/70 transition-colors cursor-pointer"
                  >
                    复制诊断信息
                  </button>
                </div>
                {diagText && (
                  <textarea
                    readOnly
                    value={diagText}
                    rows={Math.min(8, diagText.split("\n").length + 1)}
                    className="w-full mt-3 text-[12px] font-medium text-black/55 bg-black/[0.03] rounded-2xl p-3 outline-none resize-none select-text leading-relaxed"
                  />
                )}
              </div>
            </div>
          )}

          {/* 10. 开源许可与致谢（二级页：长文本可滚动、严格规范排版） */}
          {currentStep === "open_source_licenses" && (
            <div className="flex-1 flex flex-col justify-between overflow-y-auto no-scrollbar px-6 safe-top pb-10 page-enter-anim">
              <div>
                {/* 顶部返回导航 */}
                <div className="flex items-center justify-between pb-3">
                  <button
                    type="button"
                    onClick={() => goBackOr("about")}
                    className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 hover:bg-black/5 cursor-pointer active:scale-95 transition-all"
                  >
                    <CaretLeft className="w-6 h-6 stroke-[2]" />
                  </button>
                  <span className="text-[17px] font-bold text-black tracking-tight">
                    开源许可与致谢
                  </span>
                  <div className="w-11 h-11" />
                </div>

                <div className="space-y-4 mt-3">
                  {/* 致谢项目：AstroBox-NG */}
                  <div className="bg-white rounded-3xl p-5 shadow-[0_1px_3px_rgba(0,0,0,0.02)] border border-black/5">
                    <div className="flex items-center justify-between pb-3 border-b border-black/5">
                      <div>
                        <h3 className="text-[17px] font-bold text-black tracking-tight">
                          AstroBox-NG
                        </h3>
                        <p className="text-[12px] font-bold text-[#4361EE] mt-0.5">
                          AGPL-3.0 + 附加署名条款
                        </p>
                      </div>
                      <div className="w-8 h-8 rounded-full bg-[#4361EE]/10 flex items-center justify-center text-[#4361EE]">
                        <Code className="w-4 h-4 stroke-[2]" />
                      </div>
                    </div>

                    <div className="pt-3.5 space-y-2.5 text-[13px]">
                      <div>
                        <span className="text-black/40 block text-[12px] font-bold">作者</span>
                        <span className="font-bold text-black/80">AstralSight Studios</span>
                      </div>
                      <div>
                        <span className="text-black/40 block text-[12px] font-bold">仓库地址</span>
                        <a
                          href="https://github.com/AstralSightStudios/AstroBox-NG"
                          target="_blank"
                          rel="noopener noreferrer"
                          onClick={(e) => {
                            openLink(e, "https://github.com/AstralSightStudios/AstroBox-NG");
                            showToast("正在前往 AstroBox-NG 仓库...");
                          }}
                          className="font-medium text-[#4361EE] break-all inline-flex items-center gap-1 hover:underline"
                        >
                          <span>https://github.com/AstralSightStudios/AstroBox-NG</span>
                          <ArrowSquareOut className="w-3.5 h-3.5 stroke-[2] shrink-0" />
                        </a>
                      </div>
                      <div className="bg-[#F5F6F9] rounded-2xl p-3 text-[13px] text-black/45 leading-relaxed mt-2">
                        遵循其 AGPL-3.0 附加署名条款，特此明确保留原项目仓库地址及作者 AstralSight Studios 署名，由衷感谢开源社区的杰出贡献。
                      </div>
                    </div>
                  </div>

                  {/* 本项目源码许可：GNU AGPL-3.0 */}
                  <div className="bg-white rounded-3xl p-5 shadow-[0_1px_3px_rgba(0,0,0,0.02)] border border-black/5">
                    <div className="flex items-center justify-between pb-3 border-b border-black/5">
                      <div>
                        <h3 className="text-[17px] font-bold text-black tracking-tight">
                          GNU AGPL-3.0
                        </h3>
                        <p className="text-[12px] font-bold text-black/40 mt-0.5">
                          GNU Affero General Public License v3.0
                        </p>
                      </div>
                      <div className="w-8 h-8 rounded-full bg-black/5 flex items-center justify-center text-black/40">
                        <ShieldCheck className="w-4 h-4 stroke-[2]" />
                      </div>
                    </div>

                    <div className="pt-3.5 text-[13px] text-black/45 leading-relaxed space-y-2.5">
                      <p>
                        本项目「盘坊」源码遵循 GNU Affero General Public License v3.0 协议分发。
                      </p>
                      <p>
                        任何人均有权自由运行、研究、修改及分发本软件代码。若通过网络向用户提供修改后的版本服务，必须向该网络服务的所有使用者公开对应版本的完整源代码。
                      </p>
                      <div className="pt-1">
                        <a
                          href="https://www.gnu.org/licenses/agpl-3.0.html"
                          target="_blank"
                          rel="noopener noreferrer"
                          onClick={(e) => openLink(e, "https://www.gnu.org/licenses/agpl-3.0.html")}
                          className="inline-flex items-center gap-1 text-[13px] font-bold text-[#4361EE] hover:underline"
                        >
                          <span>阅读 GNU AGPL-3.0 完整条款 (gnu.org)</span>
                          <ArrowSquareOut className="w-3.5 h-3.5 stroke-[2]" />
                        </a>
                      </div>
                    </div>
                  </div>
                </div>
              </div>

              {/* 底部次级返回按钮：高度 44，全圆角，底 black/5，字 black/75，15/700 */}
              <div className="pt-6">
                <button
                  type="button"
                  onClick={() => goBackOr("about")}
                  className="w-full h-[44px] rounded-full bg-black/5 hover:bg-black/10 active:scale-[0.98] transition-all text-black/75 font-bold text-[15px] cursor-pointer flex items-center justify-center tracking-tight"
                >
                  返回关于应用
                </button>
              </div>
            </div>
          )}

        </div>

        {/* 「⋯」菜单的全屏透明遮罩：铺在手机容器整屏、菜单（弹层在卡片里 z-40 之上）下方，
            点遮罩任意一处即关闭；不随页面滚动、不受任何形变影响 */}
        {currentStep === "main" && activeActionSheetDial && (
          <div
            className="absolute inset-0 z-[35] cursor-default"
            onClick={() => setActiveActionSheetDial(null)}
          />
        )}

        {/* 底部 Tab 栏（不透明纯色底 + 原有分隔线） */}
        {currentStep === "main" && (
          <div data-pf-bottombar className="absolute bottom-0 left-0 right-0 bg-white border-t border-black/[0.04] px-10 pt-2.5 safe-bottom flex items-center justify-around z-30">
            <button
              type="button"
              onClick={() => switchTab("home")}
              className={`flex flex-col items-center gap-1 transition-colors cursor-pointer ${
                activeTab === "home" ? "text-[#4361EE]" : "text-black/35 hover:text-black/60"
              }`}
            >
              <House className="w-6 h-6 stroke-[2.2]" />
              <span className="text-[11px] font-semibold">首页</span>
            </button>

            <button
              type="button"
              onClick={() => switchTab("manage")}
              className={`flex flex-col items-center gap-1 transition-colors cursor-pointer ${
                activeTab === "manage" ? "text-[#4361EE]" : "text-black/35 hover:text-black/60"
              }`}
            >
              <SlidersHorizontal className="w-6 h-6 stroke-[2.2]" />
              <span className="text-[11px] font-semibold">管理</span>
            </button>

            <button
              type="button"
              onClick={() => switchTab("settings")}
              className={`flex flex-col items-center gap-1 transition-colors cursor-pointer ${
                activeTab === "settings" ? "text-[#4361EE]" : "text-black/35 hover:text-black/60"
              }`}
            >
              <GearSix className="w-6 h-6 stroke-[2.2]" />
              <span className="text-[11px] font-semibold">设置</span>
            </button>
          </div>
        )}

        {/* 屏幕正中悬浮对话框：设备与账号（精雕 Apple 大气居中卡片） */}
        {isDeviceDrawerOpen && (
          <div
            className="absolute inset-0 z-50 bg-black/40 backdrop-blur-md flex items-center justify-center p-6 animate-in fade-in duration-200"
            onClick={closeDeviceDrawer}
          >
            <div
              className="bg-white w-full max-w-[340px] rounded-3xl p-6 flex flex-col max-h-[85%] animate-in zoom-in-95 duration-200 shadow-[0_8px_24px_rgba(0,0,0,0.10)] border border-black/5"
              onClick={(e) => e.stopPropagation()}
            >
              {/* 顶部：账号信息与关闭按钮 */}
              <div className="flex items-center justify-between pb-4 border-b border-black/5">
                <div className="flex items-center gap-3.5">
                  <div className="w-11 h-11 rounded-full bg-[#4361EE] text-white font-bold text-[18px] flex items-center justify-center shadow-[0_1px_3px_rgba(0,0,0,0.02)] shrink-0">
                    {userProfile.avatarInitial}
                  </div>
                  <div>
                    <h4 className="text-[17px] font-bold text-black leading-tight tracking-tight">
                      {userProfile.nickname}
                    </h4>
                    <p className="text-[13px] font-medium text-black/40 mt-1">
                      {userProfile.phoneTail ? `小米账号 · 尾号 ${userProfile.phoneTail}` : "小米账号"}
                    </p>
                  </div>
                </div>

                <button
                  type="button"
                  onClick={closeDeviceDrawer}
                  className="w-11 h-11 -mr-2.5 rounded-full flex items-center justify-center text-black/40 cursor-pointer hover:bg-black/5 active:scale-95 transition-all shrink-0"
                >
                  <X className="w-5 h-5 stroke-[2]" />
                </button>
              </div>

              {/* 中间：手环设备列表 */}
              <div className="py-4 overflow-y-auto no-scrollbar">
                <span className="text-[13px] font-bold text-black/40 px-1 mb-2.5 block">
                  切换手环设备
                </span>

                {/* 连不上时的引导卡：放在列表**上方**，保证一眼能看到（列表会把下方内容挤出屏幕） */}
                {connectGuideCard}

                <div className="flex flex-col gap-2.5">
                  {mockDevicesList.length === 0 && (
                    <p className="text-[13px] font-medium text-black/40 text-center py-3 leading-relaxed">
                      列表里没有设备了，点下方「重新导入设备列表」找回
                    </p>
                  )}
                  {mockDevicesList.map((device) => {
                    // 高亮只跟随「真实连接中」的设备：没连上时列表一律静默，
                    // 不默认勾上次连过的设备（避免被误读成已经连上了）。
                    // 用户点了哪个，哪个转圈（connectingId）；连上后这里才变蓝。
                    const isSelected = isDeviceConnected && selectedDevice.id === device.id;
                    const isConnecting = connectingId === device.id;

                    return (
                      <div
                        key={device.id}
                        className={`w-full p-4 rounded-3xl transition-all flex items-center justify-between shadow-[0_1px_3px_rgba(0,0,0,0.02)] ${
                          isSelected
                            ? "bg-[#4361EE]/10 border-2 border-[#4361EE]"
                            : "bg-[#F5F6F9] hover:bg-[#ECEEF2] border-2 border-transparent"
                        }`}
                      >
                        {/* 点「已连接」的那台 = 主动断开（不报错、不再自动重连）；其它设备 = 连接 */}
                        <button
                          type="button"
                          onClick={() =>
                            isSelected ? handleDisconnectDevice(device) : handleSelectDevice(device)
                          }
                          className="flex-1 min-w-0 text-left flex items-center gap-3.5 cursor-pointer active:scale-[0.98] transition-transform"
                        >
                          <div className={`w-11 h-11 shrink-0 rounded-xl flex items-center justify-center ${
                            isSelected ? "bg-[#4361EE] text-white" : "bg-white text-black/80"
                          }`}>
                            <Watch className="w-5 h-5 stroke-[2]" />
                          </div>
                          <div className="min-w-0">
                            <h5 className={`text-[16px] font-bold leading-tight tracking-tight ${
                              isSelected ? "text-[#4361EE]" : "text-black"
                            }`}>
                              {device.name}
                            </h5>
                            {/* 断开引导：用户明确要求「一眼看得出怎么断开」（2026-10-10）；
                                文案必须短 —— 系统字号放大（用户机 font_scale=1.12）时设备名会换行，
                                长文案会被挤成「已连接 · 点…」（2026-10-10 真机实测） */}
                            <p className="text-[13px] font-medium text-black/40 mt-0.5 truncate">
                              {isConnecting ? "正在连接…" : isSelected ? "点击断开连接" : "点击连接"}
                            </p>
                          </div>
                        </button>

                        <div className="flex items-center gap-1.5 shrink-0 pl-2">
                          {isConnecting ? (
                            <CircleNotch className="w-5 h-5 animate-spin text-[#4361EE]" />
                          ) : isSelected ? (
                            <div className="w-6 h-6 rounded-full bg-[#4361EE] text-white flex items-center justify-center animate-in zoom-in-75">
                              <Check className="w-3.5 h-3.5 stroke-[2]" />
                            </div>
                          ) : null}
                          {/* 移除：只从列表里去掉（小米账号里不动），「重新导入设备列表」可找回 */}
                          <button
                            type="button"
                            aria-label={`移除 ${device.name}`}
                            onClick={() => setDeviceRemoveConfirm(device)}
                            className="w-8 h-8 rounded-full flex items-center justify-center text-black/25 hover:text-[#FF3B30] hover:bg-black/5 active:scale-95 transition-all cursor-pointer"
                          >
                            <X className="w-4 h-4 stroke-[2.5]" />
                          </button>
                        </div>
                      </div>
                    );
                  })}
                </div>
              </div>

              {/* 底部独立按钮操作区：规范高 44，全圆角 */}
              <div className="pt-3 border-t border-black/5 flex flex-col gap-2">
                <button
                  type="button"
                  onClick={() => {
                    // 重新导入 = 把账号下的设备**全拉回来**：先清掉「已移除」名单，再拉一次
                    clearHiddenDevices();
                    // App 内：重新拉一次账号设备 / 已连接设备则刷新表盘列表
                    if (invoke) {
                      const addr = connected.current.addr;
                      void (async () => {
                        try {
                          if (addr) await refreshDials(addr);
                          const devices = (await invoke("xiaomi_restore")) as CloudDevice[] | null;
                          applyCloudDevices(devices);
                          showToast("已重新拉取手环设备列表");
                        } catch (err) {
                          console.error("[xiaomi_restore] 重新导入失败：", err);
                          showToast("重新导入失败，请稍后重试", "error");
                        }
                      })();
                      return;
                    }
                    // 浏览器预览：把演示列表恢复原样（删除过的设备一并回来）
                    setMockDevicesList(MOCK_DEVICES);
                    showToast("已重新拉取手环设备列表");
                  }}
                  className="w-full h-[44px] rounded-full bg-black/5 hover:bg-black/10 text-black font-bold text-[15px] active:scale-[0.98] transition-all cursor-pointer flex items-center justify-center gap-2"
                >
                  <ArrowsClockwise className="w-4 h-4 text-black/60 stroke-[2]" />
                  <span>重新导入设备列表</span>
                </button>

                {/* 退出登录只保留在「设置」页（用户要求：浮窗里去掉，列表可操作空间更大） */}
              </div>

            </div>
          </div>
        )}

        {/* 卸载表盘二次确认：精致居中 Apple 风格 Alert 对话框 */}
        {uninstallConfirmDial && (
          <div
            className="absolute inset-0 z-50 bg-black/40 backdrop-blur-sm flex items-center justify-center p-6 animate-in fade-in duration-200"
            onClick={() => setUninstallConfirmDial(null)}
          >
            <div
              className="bg-white/95 backdrop-blur-2xl w-full max-w-[280px] rounded-3xl overflow-hidden shadow-[0_8px_24px_rgba(0,0,0,0.10)] animate-in zoom-in-95 duration-200 border border-black/5 text-center"
              onClick={(e) => e.stopPropagation()}
            >
              <div className="pt-6 px-6 pb-5">
                <h3 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                  卸载「{uninstallConfirmDial.name}」？
                </h3>
                <p className="text-[13px] text-black/40 font-medium mt-1.5 leading-relaxed">
                  卸载后将从手环中移除此表盘，后续可随时重新安装
                </p>
              </div>

              <div className="grid grid-cols-2 border-t border-black/5 divide-x divide-black/5">
                <button
                  type="button"
                  onClick={() => setUninstallConfirmDial(null)}
                  className="py-3.5 text-[16px] font-bold text-black active:bg-black/5 transition-colors cursor-pointer"
                >
                  取消
                </button>
                <button
                  type="button"
                  onClick={() => {
                    const id = uninstallConfirmDial.id;
                    const name = uninstallConfirmDial.name;
                    setDials((prev) => prev.filter((d) => d.id !== id));
                    setUninstallConfirmDial(null);
                    // App 内：真实卸载（失败则回读列表纠正显示）
                    if (invoke && connected.current.addr) {
                      const addr = connected.current.addr;
                      invoke("watchface_uninstall", { addr, watchfaceId: id })
                        .then(() => showToast(`已从手环卸载「${name}」`))
                        .catch((err) => {
                          showToast(humanize(err, "uninstall"), "error");
                          void refreshDials(addr);
                        });
                      return;
                    }
                    showToast(`已从手环卸载「${name}」`);
                  }}
                  className="py-3.5 text-[16px] font-bold text-[#FF3B30] active:bg-[#FF3B30]/10 transition-colors cursor-pointer"
                >
                  卸载
                </button>
              </div>
            </div>
          </div>
        )}

        {/* 退出登录确认：精致居中 Apple 风格 Alert 对话框 */}
        {/* 连续 3 次连不上：主动问一句要不要看帮助（样式与「退出登录确认」同一套） */}
        {showConnectHelpPrompt && (
          <div
            className="absolute inset-0 z-[60] bg-black/40 backdrop-blur-sm flex items-center justify-center p-6 animate-in fade-in duration-200"
            onClick={() => setShowConnectHelpPrompt(false)}
          >
            <div
              className="bg-white/95 backdrop-blur-2xl w-full max-w-[300px] rounded-3xl overflow-hidden shadow-[0_8px_24px_rgba(0,0,0,0.10)] animate-in zoom-in-95 duration-200 border border-black/5 text-center"
              onClick={(e) => e.stopPropagation()}
            >
              <div className="pt-6 px-6 pb-5">
                <h3 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                  连了 3 次都没成功
                </h3>
                <p className="text-[13px] text-black/40 font-medium mt-1.5 leading-relaxed">
                  要不要看看正确的连接方法？
                </p>
              </div>
              <div className="grid grid-cols-2 border-t border-black/5 divide-x divide-black/5">
                <button
                  type="button"
                  onClick={() => setShowConnectHelpPrompt(false)}
                  className="py-3.5 text-[16px] font-bold text-black active:bg-black/5 transition-colors cursor-pointer"
                >
                  再试一次
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setShowConnectHelpPrompt(false);
                    helpFromDrawer.current = true;
                    pushRoute("connect-help");
                  }}
                  className="py-3.5 text-[16px] font-bold text-[#4361EE] active:bg-black/5 transition-colors cursor-pointer"
                >
                  看看怎么连
                </button>
              </div>
            </div>
          </div>
        )}

        {/* 爱发电账号二次确认：退出 / 换账号 */}
        {paidAccountConfirm && (
          <div
            className="absolute inset-0 z-50 bg-black/40 backdrop-blur-sm flex items-center justify-center p-6 animate-in fade-in duration-200"
            onClick={() => setPaidAccountConfirm(null)}
          >
            <div
              className="bg-white/95 backdrop-blur-2xl w-full max-w-[280px] rounded-3xl overflow-hidden shadow-[0_8px_24px_rgba(0,0,0,0.10)] animate-in zoom-in-95 duration-200 border border-black/5 text-center"
              onClick={(e) => e.stopPropagation()}
            >
              <div className="pt-6 px-6 pb-5">
                <h3 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                  {paidAccountConfirm === "switch" ? "换一个爱发电账号？" : "退出爱发电账号？"}
                </h3>
                <p className="text-[13px] text-black/40 font-medium mt-1.5 leading-relaxed">
                  {paidAccountConfirm === "switch"
                    ? "将退出当前账号，并打开爱发电授权页，用新账号重新登录"
                    : "退出后需重新绑定，才能查看已购表盘"}
                </p>
              </div>

              <div className="grid grid-cols-2 border-t border-black/5 divide-x divide-black/5">
                <button
                  type="button"
                  onClick={() => setPaidAccountConfirm(null)}
                  className="py-3.5 text-[16px] font-bold text-black active:bg-black/5 transition-colors cursor-pointer"
                >
                  取消
                </button>
                <button
                  type="button"
                  onClick={() => doPaidUnbind(paidAccountConfirm)}
                  className="py-3.5 text-[16px] font-bold text-[#FF3B30] active:bg-[#FF3B30]/10 transition-colors cursor-pointer"
                >
                  {paidAccountConfirm === "switch" ? "继续" : "退出"}
                </button>
              </div>
            </div>
          </div>
        )}

        {/* 移除设备二次确认：只从列表移除，小米账号不动 */}
        {deviceRemoveConfirm && (
          <div
            className="absolute inset-0 z-50 bg-black/40 backdrop-blur-sm flex items-center justify-center p-6 animate-in fade-in duration-200"
            onClick={() => setDeviceRemoveConfirm(null)}
          >
            <div
              className="bg-white/95 backdrop-blur-2xl w-full max-w-[280px] rounded-3xl overflow-hidden shadow-[0_8px_24px_rgba(0,0,0,0.10)] animate-in zoom-in-95 duration-200 border border-black/5 text-center"
              onClick={(e) => e.stopPropagation()}
            >
              <div className="pt-6 px-6 pb-5">
                <h3 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                  从列表移除「{deviceRemoveConfirm.name}」？
                </h3>
                <p className="text-[13px] text-black/40 font-medium mt-1.5 leading-relaxed">
                  小米账号里的设备不受影响；点「重新导入设备列表」即可找回来
                </p>
              </div>

              <div className="grid grid-cols-2 border-t border-black/5 divide-x divide-black/5">
                <button
                  type="button"
                  onClick={() => setDeviceRemoveConfirm(null)}
                  className="py-3.5 text-[16px] font-bold text-black active:bg-black/5 transition-colors cursor-pointer"
                >
                  取消
                </button>
                <button
                  type="button"
                  onClick={() => {
                    const dev = deviceRemoveConfirm;
                    setDeviceRemoveConfirm(null);
                    removeDeviceFromList(dev);
                  }}
                  className="py-3.5 text-[16px] font-bold text-[#FF3B30] active:bg-[#FF3B30]/10 transition-colors cursor-pointer"
                >
                  移除
                </button>
              </div>
            </div>
          </div>
        )}

        {/* App 更新：发现新版本（样式与「退出登录确认」同一套 Apple 风格） */}
        {appUpdatePrompt && appUpdate && (
          <div
            className="absolute inset-0 z-[60] bg-black/40 backdrop-blur-sm flex items-center justify-center p-6 animate-in fade-in duration-200"
            onClick={() => setAppUpdatePrompt(false)}
          >
            <div
              className="bg-white/95 backdrop-blur-2xl w-full max-w-[320px] rounded-3xl overflow-hidden shadow-[0_8px_24px_rgba(0,0,0,0.10)] animate-in zoom-in-95 duration-200 border border-black/5 text-center"
              onClick={(e) => e.stopPropagation()}
            >
              <div className="pt-6 px-6 pb-5">
                <h3 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                  发现新版本 v{appUpdate.version_name}
                </h3>
                <p className="text-[13px] text-black/45 font-medium mt-1.5 leading-relaxed whitespace-pre-line">
                  {appUpdate.notes?.trim() || "建议更新到最新版本，体验会更好"}
                </p>
                {appUpdate.mandatory && (
                  <p className="text-[12px] font-bold text-[#FF3B30] mt-2">
                    当前版本过旧，更新后才能继续使用
                  </p>
                )}
                {appUpdate.size ? (
                  <p className="text-[12px] font-medium text-black/30 mt-2">
                    {Math.round((appUpdate.size / 1048576) * 10) / 10} MB · 当前 v{appUpdate.current}
                  </p>
                ) : null}
              </div>
              <div className="grid grid-cols-2 border-t border-black/5 divide-x divide-black/5">
                <button
                  type="button"
                  onClick={() => setAppUpdatePrompt(false)}
                  className="py-3.5 text-[16px] font-bold text-black active:bg-black/5 transition-colors cursor-pointer"
                >
                  稍后
                </button>
                <button
                  type="button"
                  onClick={() => void startAppUpdate()}
                  className="py-3.5 text-[16px] font-bold text-[#4361EE] active:bg-black/5 transition-colors cursor-pointer"
                >
                  立即更新
                </button>
              </div>
            </div>
          </div>
        )}

        {/* App 更新：下载 / 调起安装器的进度浮层（下载期间保持屏幕常亮） */}
        {appUpdatePhase && (
          <div className="absolute inset-0 z-[70] bg-black/40 backdrop-blur-sm flex items-center justify-center p-6 animate-in fade-in duration-200">
            <div className="bg-white/95 backdrop-blur-2xl w-full max-w-[300px] rounded-3xl p-6 shadow-[0_8px_24px_rgba(0,0,0,0.10)] border border-black/5 text-center">
              <h3 className="text-[16px] font-bold text-black tracking-tight">
                {appUpdatePhase.step === "downloading"
                  ? `正在下载 v${appUpdate?.version_name ?? ""}`
                  : "正在打开安装器"}
              </h3>
              <div className="mt-4 h-2 rounded-full bg-black/[0.08] overflow-hidden">
                <div
                  className="h-full rounded-full bg-[#4361EE] transition-all duration-200"
                  style={{ width: `${appUpdatePhase.step === "downloading" ? appUpdatePhase.percent : 100}%` }}
                />
              </div>
              <p className="text-[12px] font-medium text-black/40 mt-2.5">
                {appUpdatePhase.step === "downloading"
                  ? `${appUpdatePhase.percent}% · 请保持应用在前台`
                  : "随后按系统提示点「安装」"}
              </p>
            </div>
          </div>
        )}

        {showLogoutConfirm && (
          <div
            className="absolute inset-0 z-50 bg-black/40 backdrop-blur-sm flex items-center justify-center p-6 animate-in fade-in duration-200"
            onClick={() => setShowLogoutConfirm(false)}
          >
            <div
              className="bg-white/95 backdrop-blur-2xl w-full max-w-[280px] rounded-3xl overflow-hidden shadow-[0_8px_24px_rgba(0,0,0,0.10)] animate-in zoom-in-95 duration-200 border border-black/5 text-center"
              onClick={(e) => e.stopPropagation()}
            >
              <div className="pt-6 px-6 pb-5">
                <h3 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                  退出当前小米账号？
                </h3>
                <p className="text-[13px] text-black/40 font-medium mt-1.5 leading-relaxed">
                  退出后需重新验证并登录
                </p>
              </div>

              <div className="grid grid-cols-2 border-t border-black/5 divide-x divide-black/5">
                <button
                  type="button"
                  onClick={() => setShowLogoutConfirm(false)}
                  className="py-3.5 text-[16px] font-bold text-black active:bg-black/5 transition-colors cursor-pointer"
                >
                  取消
                </button>
                <button
                  type="button"
                  onClick={() => {
                    setShowLogoutConfirm(false);
                    // App 内：清掉本地小米凭证缓存（token 文件 + 内存态），并复位会话
                    if (invoke) {
                      void invoke("xiaomi_logout").catch(() => {});
                      connected.current = { addr: null, model: null, name: null };
                      accountDevices.current = [];
                      batteryOf.current = {};
                      binSession.current = null;
                      dialOptions.current = null;
                      wfItems.current = [];
                      photoDialCache.current = {};
                      setMockDevicesList([]);
                      setDials([]);
                      setRenderPreviewUrl(null);
                      setConnectedId(null);
                      setIsDeviceDrawerOpen(false);
                    }
                    // 退出登录：账号资料一并清掉（别把上一个账号的名字留在界面上），登录页成为根页面
                    setUserProfile(profileFromAccount(""));
                    setEditingDial(null);
                    setIsDeviceConnected(false); // 断线：红点 + 未连接
                    resetStackTo("login");
                    showToast("已安全退出");
                  }}
                  className="py-3.5 text-[16px] font-bold text-[#FF3B30] active:bg-[#FF3B30]/10 transition-colors cursor-pointer"
                >
                  退出登录
                </button>
              </div>
            </div>
          </div>
        )}

        {/* 多包体安装：勾选要装哪些（表盘1..N = 安装顺序；默认全选，支持全选） */}
        {installPicker && (() => {
          const pickedCount = installPicker.picked.filter(Boolean).length;
          const allOn = pickedCount === installPicker.files.length;
          const someOn = pickedCount > 0 && !allOn;
          return (
            <div
              className="absolute inset-0 z-[80] bg-black/40 flex items-center justify-center p-6 animate-in fade-in duration-200"
              onClick={cancelInstallPicker}
            >
              <div
                className="bg-white w-full max-w-[320px] rounded-3xl overflow-hidden shadow-[0_8px_24px_rgba(0,0,0,0.10)] animate-in zoom-in-95 duration-200 border border-black/5"
                onClick={(e) => e.stopPropagation()}
              >
                <div className="pt-6 px-6 pb-3 text-center">
                  <h3 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                    选择要安装的表盘
                  </h3>
                  <p className="text-[13px] text-black/40 font-medium mt-1.5 leading-relaxed">
                    默认全部安装，可以只勾选需要的
                  </p>
                </div>

                <div className="max-h-[46vh] overflow-y-auto px-2 pb-1">
                  <button
                    type="button"
                    onClick={toggleInstallPickAll}
                    className="w-full flex items-center gap-3 px-4 py-3 rounded-2xl active:bg-black/5 transition-colors cursor-pointer"
                  >
                    <span
                      className={`w-[22px] h-[22px] rounded-[7px] flex items-center justify-center shrink-0 border-2 ${
                        allOn || someOn ? "bg-[#4361EE] border-[#4361EE]" : "bg-white border-black/20"
                      }`}
                    >
                      {allOn ? (
                        <Check className="w-3.5 h-3.5 text-white stroke-[3]" />
                      ) : someOn ? (
                        <span className="w-2.5 h-[2px] bg-white rounded-full" />
                      ) : null}
                    </span>
                    <span className="text-[15px] font-bold text-black">全选</span>
                  </button>
                  <div className="h-px bg-black/5 mx-4" />
                  {installPicker.files.map((f, i) => (
                    <button
                      key={f.file_id}
                      type="button"
                      onClick={() => toggleInstallPick(i)}
                      className="w-full flex items-center gap-3 px-4 py-3 rounded-2xl active:bg-black/5 transition-colors cursor-pointer"
                    >
                      <span
                        className={`w-[22px] h-[22px] rounded-[7px] flex items-center justify-center shrink-0 border-2 ${
                          installPicker.picked[i] ? "bg-[#4361EE] border-[#4361EE]" : "bg-white border-black/20"
                        }`}
                      >
                        {installPicker.picked[i] && (
                          <Check className="w-3.5 h-3.5 text-white stroke-[3]" />
                        )}
                      </span>
                      <span className="text-[15px] font-medium text-black">表盘{i + 1}</span>
                    </button>
                  ))}
                </div>

                <div className="grid grid-cols-2 border-t border-black/5 divide-x divide-black/5">
                  <button
                    type="button"
                    onClick={cancelInstallPicker}
                    className="py-3.5 text-[16px] font-bold text-black active:bg-black/5 transition-colors cursor-pointer"
                  >
                    取消
                  </button>
                  <button
                    type="button"
                    onClick={confirmInstallPicker}
                    disabled={pickedCount === 0}
                    className={`py-3.5 text-[16px] font-bold transition-colors ${
                      pickedCount === 0
                        ? "text-black/25 cursor-default"
                        : "text-[#4361EE] active:bg-[#4361EE]/10 cursor-pointer"
                    }`}
                  >
                    开始安装
                  </button>
                </div>
              </div>
            </div>
          );
        })()}

        {/* 悬浮全局提示条：规范高 44 · 全圆角 · 底 #0C0D0E · 白字 13/700
            error 类（失败/拒绝/需先操作）用红叉，success 类用绿勾 */}
        {toast && (
          <div
            className="absolute left-6 right-6 z-60 flex justify-center pointer-events-none"
            // 浮窗跟随底栏：底栏低就往下、底栏高（编辑页应用栏/裁剪页面板）跟着抬，
            // 与底栏顶边恒定间隔 TOAST_GAP；无底栏页面用 80px 基础位置
            style={{ bottom: bottomBarH > 0 ? bottomBarH + TOAST_GAP : 80 }}
          >
            <div className="bg-[#0C0D0E] text-white text-[13px] font-bold h-[44px] px-5 rounded-full flex items-center gap-2 animate-in fade-in slide-in-from-bottom-2 duration-200 shadow-[0_8px_24px_rgba(0,0,0,0.10)]">
              {toast.kind === "error" ? (
                <X className="w-4 h-4 text-[#FF3B30] stroke-[2.5] shrink-0" />
              ) : (
                <Check className="w-4 h-4 text-[#34C759] stroke-[2] shrink-0" />
              )}
              <span>{toast.msg}</span>
            </div>
          </div>
        )}

      </div>
    </div>
    </IconContext.Provider>
  );
}
