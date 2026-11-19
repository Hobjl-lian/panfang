/**
 * 视频表盘（动态表盘）制作页。
 *
 * 流程：选模板/样式 → 选视频 → 裁画面 + 截 ≤5 秒 → 选帧数 → 抽帧上传 → 后台打包
 *      → 下载成品 → 本地加密留档 → 安装到手表（可退出 App，回来继续看结果）。
 *
 * 界面上不出现任何模板工程文件：客户端只拿到模板封面和最终 .bin。
 */
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  ArrowClockwise,
  ArrowsOutSimple,
  CaretLeft,
  CaretRight,
  CheckCircle,
  DeviceMobile,
  FilmStrip,
  LockSimple,
  Pause,
  Play,
  Plus,
  Scissors,
  Trash,
  VideoCamera,
  WarningCircle,
  X,
} from "@phosphor-icons/react";
import {
  bytesToB64,
  frameSizeFor,
  invoke,
  setInstallBackLock,
  setKeepScreenOn,
  type ScreenInfo,
} from "./tauri";
import { DragSlider, GenSlider } from "./Slider";

const API_BASE = "https://api.hobjl-shop.top";
const MAX_VIDEO_BYTES = 80 * 1024 * 1024;
/**
 * 流畅度档位：用户选的是「看起来多顺」，帧数由帧率 × 片段时长自动算。
 * 「丝滑」按 180 帧封顶（5 秒片段约 36 帧/秒），避免直接顶到 25MB 上限。
 */
const QUALITY_LEVELS = [
  { key: "standard", label: "标准", fps: 24, maxFrames: 300 },
  { key: "smooth", label: "流畅", fps: 30, maxFrames: 300 },
  { key: "silky", label: "丝滑", fps: 45, maxFrames: 180 },
] as const;
type QualityKey = (typeof QUALITY_LEVELS)[number]["key"] | "custom";
/** 结果性状态（已完成/失败）的「已看过」标记：进过历史记录页就不再在入口卡上常驻 */
const JOB_SEEN_KEY = "pf_video_job_seen_v1";
/** 轨道序列帧格数：放大后按新视野重新取帧，不是把旧图拉大 */
const THUMB_TILES = 16;

type Style = {
  id: string;
  name: string;
  device_type: string;
  canvas_w: number;
  canvas_h: number;
  frame_count: number;
  preview_url: string;
};

type Template = {
  id: string;
  name: string;
  afdian_plan_id: string;
  unlocked: boolean;
  styles: Style[];
  buy_url: string;
};

type Project = {
  id: string;
  title?: string;
  template_id?: string;
  template_name?: string;
  style_id?: string;
  style_name?: string;
  device_type?: string;
  canvas_w?: number;
  canvas_h?: number;
  preview_url?: string;
  /** 成品 bin 里解出来的真实预览图（首帧 + 布局），有就优先用它 */
  preview_b64?: string;
  video_name?: string;
  frame_count?: number;
  fps?: number;
  mode?: string;
  watchface_id?: string;
  watchface_ids?: string[];
  job_id?: string;
  job_status?: string;
  job_error?: string;
  result_size?: number;
  trim_start?: number;
  trim_end?: number;
  zoom?: number;
  pan_x?: number;
  pan_y?: number;
  created_at?: number;
  updated_at?: number;
  has_video?: boolean;
  has_frames?: boolean;
  has_bin?: boolean;
  video_size?: number;
  frames_size?: number;
  bin_size?: number;
};

type Props = {
  onBack: () => void;
  deviceAddr: string;
  deviceName: string;
  deviceModel: string;
  deviceConnected: boolean;
  /** 从表盘管理页点某个已装的视频表盘进来时，直接打开这个作品 */
  initialProjectId?: string;
  /** App 的返回守卫：页内不是首页时接管系统返回，逐级退 */
  backGuardRef?: { current: (() => boolean) | null };
  /** 页内进/退一级时同步 App 的 navDepth（页内也占历史条目，必须和 App 记的深度一致） */
  onInternalDepthChange?: (delta: number) => void;
  /** 安装委托给 App 的正式安装流程（复用带进度条的安装页） */
  onInstallPayload?: (payload: {
    addr: string;
    b64: string;
    ext: string;
    name: string;
    kind?: "watchface" | "quickapp";
    rewriteId?: string;
  }) => void;
  onInstalled?: () => void;
  onOpenPaid: () => void;
  showToast: (msg: string, kind?: "success" | "error") => void;
};

type Screen = "home" | "projects" | "templates" | "edit" | "job" | "project";

/** 截取轨道上的手势：单指拖选择框 / 单指平移视野 / 双指缩放 */
type TimelineGesture =
  | { kind: "pan"; startX: number; startView: number }
  | { kind: "move"; startX: number; startTrim: number; clip: number }
  | { kind: "pinch"; startDist: number; startZoom: number; anchorTime: number; startView: number };

function fmtSize(n?: number): string {
  const v = Number(n) || 0;
  if (v <= 0) return "—";
  if (v < 1024 * 1024) return `${Math.max(1, Math.round(v / 1024))} KB`;
  return `${(v / 1024 / 1024).toFixed(1)} MB`;
}

function fmtTime(sec?: number): string {
  const v = Math.max(0, Number(sec) || 0);
  return `${v.toFixed(1)}s`;
}

/** 项目缩略图：优先用成品 bin 里解出的真实预览，没有再退回模板预览 */
function previewSrc(p: { preview_b64?: string; preview_url?: string }): string {
  if (p.preview_b64) return `data:image/png;base64,${p.preview_b64}`;
  if (p.preview_url) return `${API_BASE}${p.preview_url}`;
  return "";
}

function fmtDate(ts?: number): string {
  if (!ts) return "";
  const d = new Date(ts * 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
}

function newProjectId(): string {
  return `vp${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`;
}

function statusLabel(s?: string): string {
  switch (s) {
    case "queued":
      return "排队中";
    case "running":
      return "制作中";
    case "done":
      return "已完成";
    case "failed":
      return "制作失败";
    case "deleted":
      return "已取消";
    case "cleared":
      return "已清理";
    default:
      return "未提交";
  }
}

/** 服务端/worker 的原始错误 → 用户能看懂的下一步 */
function friendlyJobError(err?: string): string {
  const s = String(err || "");
  if (/2[05]\s*MB/i.test(s)) {
    return "成品超过 25MB 上限，请把流畅度调低一档，或缩短视频后重试。";
  }
  return s || "这次没有制作成功，请稍后重试。";
}

/** 把视频当前帧按「cover + 缩放 + 平移」画进画布（与预览的 CSS 同一套算法）。 */
function drawCropped(
  ctx: CanvasRenderingContext2D,
  video: HTMLVideoElement,
  cw: number,
  ch: number,
  zoom: number,
  panX: number,
  panY: number
) {
  const vw = video.videoWidth || cw;
  const vh = video.videoHeight || ch;
  const cover = Math.max(cw / vw, ch / vh) * Math.max(1, zoom);
  const dw = vw * cover;
  const dh = vh * cover;
  const maxX = Math.max(0, (dw - cw) / 2);
  const maxY = Math.max(0, (dh - ch) / 2);
  const dx = (cw - dw) / 2 + panX * maxX;
  const dy = (ch - dh) / 2 + panY * maxY;
  ctx.fillStyle = "#000";
  ctx.fillRect(0, 0, cw, ch);
  ctx.drawImage(video, dx, dy, dw, dh);
}

function seek(video: HTMLVideoElement, t: number): Promise<void> {
  return new Promise((resolve) => {
    let done = false;
    const finish = () => {
      if (done) return;
      done = true;
      video.removeEventListener("seeked", finish);
      resolve();
    };
    video.addEventListener("seeked", finish, { once: true });
    try {
      video.currentTime = Math.max(0, t);
    } catch {
      finish();
    }
    // 某些机型 seek 到关键帧附近不触发事件：兜底放行，避免整页卡死
    setTimeout(finish, 1600);
  });
}

/** 从隐藏的 video 抓一张低清缩略图（轨道用，糊一点没关系） */
function captureThumb(el: HTMLVideoElement): string | null {
  const canvas = document.createElement("canvas");
  canvas.width = 48;
  canvas.height = 48;
  const ctx = canvas.getContext("2d");
  if (!ctx) return null;
  const vw = el.videoWidth || 16;
  const vh = el.videoHeight || 9;
  const k = Math.max(48 / vw, 48 / vh);
  const dw = vw * k;
  const dh = vh * k;
  ctx.fillStyle = "#000";
  ctx.fillRect(0, 0, 48, 48);
  try {
    ctx.drawImage(el, (48 - dw) / 2, (48 - dh) / 2, dw, dh);
    return canvas.toDataURL("image/jpeg", 0.5);
  } catch {
    return null;
  }
}

/**
 * 播放中抓帧：逐帧 seek 在部分机型上要等解码关键帧（实测约 3~6 秒/帧），
 * 这里改成「播放到目标时间 → 暂停 → 抓一帧 → 继续播放」，不再随机 seek。
 */
async function captureByPlaying(
  video: HTMLVideoElement,
  cw: number,
  ch: number,
  count: number,
  start: number,
  duration: number,
  zoom: number,
  panX: number,
  panY: number,
  onFrame: (b64: string) => void,
  onProgress: (n: number) => void
): Promise<void> {
  await seek(video, start);
  // 先让首帧真正解出来，避免抓到黑帧
  await new Promise((r) => setTimeout(r, 60));
  video.muted = true;
  const canvas = document.createElement("canvas");
  canvas.width = cw;
  canvas.height = ch;
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("画布不可用");
  const step = duration / count;

  return new Promise<void>((resolve, reject) => {
    let i = 0;
    let done = false;
    let lastProgress = Date.now();
    const finish = () => {
      if (done) return;
      done = true;
      window.clearInterval(guard);
      video.pause();
      resolve();
    };
    const grab = () => {
      drawCropped(ctx, video, cw, ch, zoom, panX, panY);
      const data = canvas.toDataURL("image/png");
      onFrame(data.slice(data.indexOf(",") + 1));
      i++;
      lastProgress = Date.now();
      onProgress(i);
    };
    const fillToEnd = () => {
      while (i < count) grab();
      finish();
    };
    // 片尾 / 播放被中断时 requestVideoFrameCallback 可能不再回调：
    // 2.5 秒没有任何新帧就直接用最后一帧补满，保证任务能结束。
    const guard = window.setInterval(() => {
      if (done) return;
      if (Date.now() - lastProgress > 2500) fillToEnd();
    }, 500);
    const schedule = () => {
      if (done) return;
      const anyV = video as any;
      if (typeof anyV.requestVideoFrameCallback === "function") {
        anyV.requestVideoFrameCallback(() => tick());
      } else {
        requestAnimationFrame(() => tick());
      }
    };
    const atEnd = () =>
      video.ended || video.currentTime >= start + duration - 0.02;
    const tick = () => {
      if (done) return;
      if (i >= count || atEnd()) {
        fillToEnd();
        return;
      }
      const target = start + i * step;
      if (video.currentTime + 0.0005 >= target) {
        video.pause();
        grab();
        if (i >= count || atEnd()) {
          fillToEnd();
          return;
        }
        void video.play().then(schedule).catch(reject);
        return;
      }
      schedule();
    };
    void video.play().then(schedule).catch(reject);
  });
}

export default function VideoWatchface({
  onBack,
  deviceAddr,
  deviceName,
  deviceModel,
  deviceConnected,
  initialProjectId,
  backGuardRef,
  onInternalDepthChange,
  onInstallPayload,
  onInstalled,
  onOpenPaid,
  showToast,
}: Props) {
  const [screen, setScreen] = useState<Screen>(initialProjectId ? "project" : "home");
  const [projects, setProjects] = useState<Project[]>([]);
  /** projectId → jobId:status。进过历史记录页的结果不再在入口卡上提示 */
  const [seenJobs, setSeenJobs] = useState<Record<string, string>>(() => {
    try {
      const raw = localStorage.getItem(JOB_SEEN_KEY);
      const parsed = raw ? JSON.parse(raw) : null;
      return parsed && typeof parsed === "object" ? parsed : {};
    } catch {
      return {};
    }
  });
  const [loadingProjects, setLoadingProjects] = useState(true);
  const [templates, setTemplates] = useState<Template[]>([]);
  const [templatesError, setTemplatesError] = useState("");
  const [loadingTemplates, setLoadingTemplates] = useState(false);
  const [deviceScreen, setDeviceScreen] = useState<[number, number] | null>(null);

  const [style, setStyle] = useState<Style | null>(null);
  const [template, setTemplate] = useState<Template | null>(null);
  const [projectId, setProjectId] = useState("");
  const [project, setProject] = useState<Project | null>(null);

  const [videoUrl, setVideoUrl] = useState("");
  const [videoName, setVideoName] = useState("");
  const [videoDim, setVideoDim] = useState<{ w: number; h: number } | null>(null);
  const [duration, setDuration] = useState(0);
  const [trimStart, setTrimStart] = useState(0);
  const [trimEnd, setTrimEnd] = useState(5);
  const [zoom, setZoom] = useState(1);
  const [panX, setPanX] = useState(0);
  const [panY, setPanY] = useState(0);
  const [count, setCount] = useState(120);
  /** 当前流畅度档位；从历史记录恢复时按每帧率反推，认不出就是自定义 */
  const [quality, setQuality] = useState<QualityKey>("standard");
  const [customCount, setCustomCount] = useState("");
  /** 自定义帧数输入框是否展开（和内容是否为空分开：清空不能把输入框卸载） */
  const [customOpen, setCustomOpen] = useState(false);
  const [mode, setMode] = useState<"loop" | "once">("loop");
  const [playing, setPlaying] = useState(false);
  /** 播放头（秒）：进度条与轨道上的白线共用 */
  const [playhead, setPlayhead] = useState(0);
  /** 轨道缩放（1 = 整段视频铺满；最大 12 倍）与视野起点 */
  const [tlZoom, setTlZoom] = useState(1);
  const [viewStart, setViewStart] = useState(0);
  /** 轨道上的序列帧（按当前视野实时生成，缩放后会换成更密的帧） */
  const [tiles, setTiles] = useState<Array<{ key: string; src: string | null }>>([]);

  const [busy, setBusy] = useState("");
  const [progress, setProgress] = useState(0);
  const [job, setJob] = useState<{ id: string; status: string; error?: string } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState(false);
  /** 清理表盘文件前确认：删掉后编辑页读不到成品 bin，作品就不能继续编辑了 */
  const [clearBinAsk, setClearBinAsk] = useState(false);
  /** 本次新选的视频还没保存到历史记录（退出编辑页要问一下） */
  const [draftDirty, setDraftDirty] = useState(false);
  const [exitAsk, setExitAsk] = useState(false);

  const videoRef = useRef<HTMLVideoElement | null>(null);
  const cropRef = useRef<HTMLDivElement | null>(null);
  const fileRef = useRef<HTMLInputElement | null>(null);
  const dragRef = useRef<{ x: number; y: number; px: number; py: number } | null>(null);
  const fetchedRef = useRef(false);
  const videoUrlRef = useRef("");
  const restoreRef = useRef<{
    start?: number;
    end?: number;
    zoom?: number;
    px?: number;
    py?: number;
  } | null>(null);
  const toastRef = useRef(showToast);
  toastRef.current = showToast;
  const genLockRef = useRef(0);
  const genDownRef = useRef<{ x: number; y: number } | null>(null);
  const openedFromDialRef = useRef("");
  /** 截取轨道：正在拖的是起点还是终点 */
  const trimDragRef = useRef<null | "start" | "end">(null);
  const trimTrackRef = useRef<HTMLDivElement | null>(null);
  /** 进度条拖动后的 seek（节流，不重置计时器：拖到哪就跟着显示到哪） */
  const seekTimerRef = useRef<number | undefined>(undefined);
  /** 进度条最近一次的目标时间（节流期间只记最新值） */
  const scrubTargetRef = useRef<number | null>(null);
  /** 正在拖进度条：这段时间忽略 timeupdate，别把白点拽回去 */
  const scrubbingRef = useRef(false);
  const scrubEndTimerRef = useRef<number | undefined>(undefined);
  /** 轨道缩略图缓存（时间是 key，反复缩放/平移不用重新 seek） */
  const thumbCacheRef = useRef(new Map<string, string>());
  /** 轨道手势：按 pointerId 记位置，第二根手指落下就切到捏合 */
  const tlPointersRef = useRef(new Map<number, { x: number }>());
  const tlGestureRef = useRef<TimelineGesture | null>(null);
  /** 当前项目是不是本次新建的草稿（退出不保存时要删掉） */
  const draftCreatedRef = useRef(false);
  /** 退出确认后放行一次返回，避免守卫再弹一次 */
  const exitConfirmedRef = useRef(false);

  // 离开页面时清掉待执行的 seek
  useEffect(
    () => () => {
      if (seekTimerRef.current) window.clearTimeout(seekTimerRef.current);
      if (scrubEndTimerRef.current) window.clearTimeout(scrubEndTimerRef.current);
    },
    []
  );

  /**
   * 页内页面栈：每进一级先压一条历史（地址仍然是 #video），系统返回时由守卫弹一级。
   * 以前是「返回时再补一条历史」，安卓 WebView 的 canGoBack() 会瞬时为 false → 直接退出 App。
   */
  const screensRef = useRef<Screen[]>([initialProjectId ? "project" : "home"]);

  const pushScreen = (next: Screen) => {
    if (screensRef.current[screensRef.current.length - 1] === next) return;
    screensRef.current = [...screensRef.current, next];
    setScreen(next);
    try {
      window.history.pushState({ panfang: true, route: "video" }, "", "#video");
    } catch {
      /* 无 history：至少页内状态对 */
    }
    onInternalDepthChange?.(1);
  };

  const replaceScreen = (next: Screen) => {
    screensRef.current = [...screensRef.current.slice(0, -1), next];
    setScreen(next);
    try {
      window.history.replaceState({ panfang: true, route: "video" }, "", "#video");
    } catch {
      /* 忽略 */
    }
  };

  /** 退一级（popstate 到达时历史已经被系统弹掉，这里只改页内状态与 depth） */
  const popScreen = (): boolean => {
    const stack = screensRef.current;
    if (stack.length <= 1) return false;
    if (!exitConfirmedRef.current && screen === "edit" && videoUrl && draftDirty) {
      // 系统返回已经把 edit 这条历史弹掉了：补回来（不动 navDepth，净变化为 0），
      // 用户选「继续编辑」后层级不会乱。
      try {
        window.history.pushState({ panfang: true, route: "video" }, "", "#video");
      } catch {
        /* 忽略 */
      }
      setExitAsk(true);
      return true;
    }
    exitConfirmedRef.current = false;
    // 从制作进度退出：不回编辑/模板，直接落到历史记录，让用户能看到任务还在跑
    if (screen === "job") {
      const depthBefore = stack.length - 1;
      screensRef.current = ["home", "projects"];
      setScreen("projects");
      onInternalDepthChange?.(1 - depthBefore);
      return true;
    }
    screensRef.current = stack.slice(0, -1);
    setScreen(screensRef.current[screensRef.current.length - 1]);
    onInternalDepthChange?.(-1);
    return true;
  };
  if (backGuardRef) backGuardRef.current = popScreen;

  /** 页内 Header 的返回：真的走一次历史返回，让 guard / popstate 统一处理 */
  const backFromScreen = () => {
    if (screensRef.current.length <= 1) {
      onBack();
      return;
    }
    try {
      window.history.back();
    } catch {
      popScreen();
    }
  };

  useEffect(
    () => () => {
      if (backGuardRef) backGuardRef.current = null;
    },
    [backGuardRef]
  );

  const cw = style?.canvas_w || 336;
  const ch = style?.canvas_h || 480;
  const clipLen = Math.max(0, trimEnd - trimStart);
  const fps = clipLen > 0 ? count / clipLen : 0;
  const fpsBad = fps > 60.001 || (clipLen > 0 && fps < 0.999);
  const durationBad = clipLen > 5.001;
  const maxFrames = clipLen > 0 ? Math.max(1, Math.floor(clipLen * 60)) : 300;
  const videoReady = !!videoDim && duration > 0.01;

  /** 某一档在给定时长下对应的总帧数（不超过档位上限，也不超过 60fps） */
  const framesForQuality = (key: Exclude<QualityKey, "custom">, len: number) => {
    const level = QUALITY_LEVELS.find((l) => l.key === key) || QUALITY_LEVELS[0];
    return Math.max(
      1,
      Math.min(300, Math.floor(Math.max(len, 0) * 60), level.maxFrames, Math.round(level.fps * Math.max(len, 0)))
    );
  };

  /** 历史记录恢复时按帧数反推档位：只有和某一档算出来完全一致才算命中，否则保留为自定义 */
  const classifyQuality = (frames: number, len: number): QualityKey => {
    if (len <= 0.01) return "custom";
    const hit = QUALITY_LEVELS.find((level) => framesForQuality(level.key, len) === frames);
    return hit ? hit.key : "custom";
  };

  // 片段时长变了：档位跟着重算帧数（自定义不动，只压到 60fps 以内）
  useEffect(() => {
    if (clipLen <= 0.01) return;
    if (quality !== "custom") {
      const next = framesForQuality(quality, clipLen);
      if (next !== count) {
        setCount(next);
        setCustomCount(String(next));
      }
      return;
    }
    const max = Math.max(1, Math.floor(clipLen * 60));
    if (count > max) {
      setCount(max);
      setCustomCount(String(max));
    }
  }, [clipLen, quality, count]);

  // ---------------------------------------------------------------- 项目/模板

  const loadProjects = useCallback(async () => {
    if (!invoke) {
      setLoadingProjects(false);
      return [];
    }
    try {
      const v = (await invoke("video_project_list")) as { items?: Project[] };
      const items = Array.isArray(v?.items) ? v.items : [];
      setProjects(items);
      return items;
    } catch (e) {
      toastRef.current(String(e), "error");
      return [];
    } finally {
      setLoadingProjects(false);
    }
  }, []);

  const loadTemplates = useCallback(async () => {
    if (!invoke) return;
    setLoadingTemplates(true);
    setTemplatesError("");
    try {
      const v = (await invoke("video_templates")) as { items?: Template[] };
      setTemplates(Array.isArray(v?.items) ? v.items : []);
    } catch (e) {
      setTemplatesError(String(e));
    } finally {
      setLoadingTemplates(false);
    }
  }, []);

  useEffect(() => {
    void loadProjects();
    void loadTemplates();
    const inv = invoke;
    if (inv) {
      void inv("paid_device_screen", {
        model: deviceModel || null,
        addr: deviceAddr || null,
        device: deviceName || null,
      })
        .then((v: any) => {
          const s = v?.screen;
          if (Array.isArray(s) && s.length === 2 && s[0] && s[1]) {
            setDeviceScreen([Number(s[0]), Number(s[1])]);
          }
        })
        .catch(() => {});
    }
    return () => {
      if (videoUrlRef.current) URL.revokeObjectURL(videoUrlRef.current);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // 从爱发电买完回到 App：解锁状态当场刷新，别让用户对着「未解锁」自己找刷新。
  useEffect(() => {
    if (screen !== "templates") return;
    const onVisible = () => {
      if (document.visibilityState === "visible") void loadTemplates();
    };
    document.addEventListener("visibilitychange", onVisible);
    return () => document.removeEventListener("visibilitychange", onVisible);
  }, [screen, loadTemplates]);

  // 首页：把「排队中/制作中」的任务补问一次；有任务在跑时每 4 秒刷新一次入口卡状态。
  useEffect(() => {
    const inv = invoke;
    if (screen !== "home" || !inv) return;
    let alive = true;
    let timer: number | undefined;
    const tick = async () => {
      const items = await loadProjects();
      if (!alive) return;
      for (const p of items) {
        const active = ["queued", "running"].includes(p.job_status || "");
        const doneNoBin = p.job_status === "done" && !p.has_bin;
        if (!p.job_id || (!active && !doneNoBin)) continue;
        try {
          const v = (await inv("video_job_status", { projectId: p.id })) as any;
          const st = v?.job?.status || "";
          if (st === "done") await inv("video_job_fetch", { projectId: p.id });
          if (!alive) return;
        } catch {
          /* 网络抖动：下次再问 */
        }
      }
      if (!alive) return;
      const fresh = await loadProjects();
      if (alive && fresh.some((p) => ["queued", "running"].includes(p.job_status || ""))) {
        timer = window.setTimeout(tick, 4000);
      }
    };
    void tick();
    return () => {
      alive = false;
      if (timer) window.clearTimeout(timer);
    };
  }, [screen, loadProjects]);

  // 历史记录页：每 4 秒问一次进行中、以及「已完成但还没下下来」的任务
  // （后者超过服务端保留期后会被判过期，必须在列表里如实标出来）
  useEffect(() => {
    const inv = invoke;
    if (screen !== "projects" || !inv) return;
    let alive = true;
    let timer: number | undefined;
    const triedFetch = new Set<string>();
    const tick = async () => {
      const items = await loadProjects();
      if (!alive) return;
      for (const p of items) {
        const st = p.job_status || "";
        const active = ["queued", "running"].includes(st);
        const doneNoBin = st === "done" && !p.has_bin;
        if (!p.job_id || (!active && !doneNoBin)) continue;
        try {
          const v = (await inv("video_job_status", { projectId: p.id })) as any;
          if (!alive) return;
          if (v?.job?.status === "done" && !p.has_bin && !triedFetch.has(p.id)) {
            triedFetch.add(p.id);
            await inv("video_job_fetch", { projectId: p.id });
            if (!alive) return;
          }
        } catch {
          /* 网络抖动：下一轮再问 */
        }
      }
      if (!alive) return;
      await loadProjects();
      if (alive) timer = window.setTimeout(tick, 4000);
    };
    void tick();
    return () => {
      alive = false;
      if (timer) window.clearTimeout(timer);
    };
  }, [screen, loadProjects]);

  // 进历史记录页就算「用户已经看到结果」：把已完成/失败的标记写下来，
  // 回到首页时入口卡不再常驻「已完成 / 制作失败」；排队中、制作中不受影响。
  useEffect(() => {
    if (screen !== "projects") return;
    setSeenJobs((prev) => {
      const next = { ...prev };
      let changed = false;
      for (const p of projects) {
        const st = p.job_status || "";
        if (!p.job_id || (st !== "done" && st !== "failed")) continue;
        const key = `${st}:${p.job_id}`;
        if (next[p.id] !== key) {
          next[p.id] = key;
          changed = true;
        }
      }
      if (!changed) return prev;
      try {
        localStorage.setItem(JOB_SEEN_KEY, JSON.stringify(next));
      } catch {
        /* 写不进去时本次会话内仍然生效 */
      }
      return next;
    });
  }, [screen, projects]);

  const openTemplates = () => {
    pushScreen("templates");
    void loadTemplates();
  };

  const startNew = (tpl: Template, st: Style) => {
    if (!tpl.unlocked) {
      if (invoke) void invoke("open_external", { url: tpl.buy_url });
      return;
    }
    setTemplate(tpl);
    setStyle(st);
    setProject(null);
    setProjectId("");
    setVideoName("");
    setVideoDim(null);
    setDuration(0);
    setPlayhead(0);
    setTlZoom(1);
    setViewStart(0);
    setTrimStart(0);
    setTrimEnd(5);
    setZoom(1);
    setPanX(0);
    setPanY(0);
    setCount(120);
    setQuality("standard");
    setCustomCount("");
    setCustomOpen(false);
    setMode("loop");
    setJob(null);
    fetchedRef.current = false;
    if (videoUrlRef.current) URL.revokeObjectURL(videoUrlRef.current);
    videoUrlRef.current = "";
    setVideoUrl("");
    draftCreatedRef.current = false;
    setDraftDirty(false);
    setExitAsk(false);
    pushScreen("edit");
  };

  const openProject = (p: Project) => {
    setProject(p);
    setConfirmDelete(false);
    pushScreen("project");
  };

  // 表盘管理页点「视频」表盘进来的：等本地作品列表读完，直接打开对应作品
  useEffect(() => {
    if (!initialProjectId || openedFromDialRef.current === initialProjectId) return;
    const hit = projects.find((p) => p.id === initialProjectId);
    if (hit) {
      openedFromDialRef.current = initialProjectId;
      // 只在还停在首页时自动打开；用户已经点进别的页面就不要硬跳
      if (screensRef.current.length === 1) openProject(hit);
    }
  }, [initialProjectId, projects]);

  const editExisting = async (p: Project, nav: "push" | "replace" = "push") => {
    if (!invoke) return;
    setBusy("正在读取本地视频…");
    try {
      const st: Style = {
        id: p.style_id || "",
        name: p.style_name || "样式",
        device_type: p.device_type || "",
        canvas_w: p.canvas_w || 336,
        canvas_h: p.canvas_h || 480,
        frame_count: p.frame_count || 0,
        preview_url: p.preview_url || "",
      };
      setStyle(st);
      setTemplate(null);
      setProject(p);
      setProjectId(p.id);
      setVideoName(p.video_name || "");
      setVideoDim(null);
      setDuration(0);
      const restoredLen = Math.max(0, (p.trim_end ?? 5) - (p.trim_start ?? 0));
      const restoredQuality = classifyQuality(p.frame_count || 120, restoredLen);
      setCount(p.frame_count || 120);
      setQuality(restoredQuality);
      setCustomCount(String(p.frame_count || 120));
      setCustomOpen(restoredQuality === "custom");
      setMode(p.mode === "once" ? "once" : "loop");
      setTlZoom(1);
      setViewStart(0);
      setPlayhead(0);
      draftCreatedRef.current = false;
      setDraftDirty(false);
      setExitAsk(false);
      restoreRef.current = {
        start: p.trim_start,
        end: p.trim_end,
        zoom: p.zoom,
        px: p.pan_x,
        py: p.pan_y,
      };
      setTrimStart(p.trim_start ?? 0);
      setTrimEnd(p.trim_end ?? 5);
      setZoom(p.zoom ?? 1);
      setPanX(p.pan_x ?? 0);
      setPanY(p.pan_y ?? 0);
      setJob(p.job_id ? { id: p.job_id, status: p.job_status || "queued" } : null);
      fetchedRef.current = false;
      if (p.has_video) {
        const v = (await invoke("video_project_get", {
          projectId: p.id,
          kind: "video",
        })) as { data_b64: string };
        const bytes = Uint8Array.from(atob(v.data_b64), (c) => c.charCodeAt(0));
        const blob = new Blob([bytes], { type: "video/mp4" });
        if (videoUrlRef.current) URL.revokeObjectURL(videoUrlRef.current);
        const url = URL.createObjectURL(blob);
        videoUrlRef.current = url;
        setVideoUrl(url);
      } else {
        if (videoUrlRef.current) URL.revokeObjectURL(videoUrlRef.current);
        videoUrlRef.current = "";
        setVideoUrl("");
      }
      if (nav === "replace") replaceScreen("edit");
      else pushScreen("edit");
    } catch (e) {
      showToast(String(e), "error");
    } finally {
      setBusy("");
    }
  };

  // ---------------------------------------------------------------- 视频与裁剪

  const onLoadedMeta = () => {
    const v = videoRef.current;
    if (!v) return;
    setVideoDim({ w: v.videoWidth, h: v.videoHeight });
    const d = Number.isFinite(v.duration) ? v.duration : 0;
    const r = restoreRef.current;
    restoreRef.current = null;
    setDuration(d);
    const start = Math.max(0, Math.min(r?.start ?? 0, Math.max(0, d - 0.3)));
    const end = Math.min(
      d || 5,
      Math.max(start + 0.3, Math.min(r?.end ?? Math.min(5, d || 5), start + 5))
    );
    setTrimStart(start);
    setTrimEnd(end);
    if (!r) {
      const len = Math.max(0, end - start);
      setQuality("standard");
      const mid = framesForQuality("standard", len);
      setCount(mid);
      setCustomCount("");
    }
    setZoom(Math.max(1, Math.min(3, r?.zoom ?? 1)));
    setPanX(Math.max(-1, Math.min(1, r?.px ?? 0)));
    setPanY(Math.max(-1, Math.min(1, r?.py ?? 0)));
    setPlaying(false);
    // 停在第 0 帧时部分机型只显示纯黑；往前挪一小步，让预览直接有画面
    const head = Math.min(d || 0, start + 0.05);
    setPlayhead(head);
    try {
      v.currentTime = head;
    } catch {
      /* 忽略 */
    }
  };

  const saveVideo = async (file: File) => {
    if (!invoke) return;
    if (file.size > MAX_VIDEO_BYTES) {
      showToast("视频超过 80MB，请先在相册里裁剪到 5 秒以内再试", "error");
      return;
    }
    setBusy("正在保存视频…");
    try {
      const buf = await file.arrayBuffer();
      const b64 = bytesToB64(new Uint8Array(buf));
      const isNew = !projectId;
      const id = projectId || newProjectId();
      const now = Math.floor(Date.now() / 1000);
      const meta = {
        id,
        title: project?.title || videoName || file.name.replace(/\.[^.]+$/, "") || "我的视频表盘",
        template_id: template?.id || project?.template_id || "",
        template_name: template?.name || project?.template_name || "",
        style_id: style?.id || project?.style_id || "",
        style_name: style?.name || project?.style_name || "",
        device_type: style?.device_type || project?.device_type || "",
        canvas_w: cw,
        canvas_h: ch,
        preview_url: style?.preview_url || project?.preview_url || "",
        video_name: file.name,
        created_at: project?.created_at || now,
        // 换视频时保留旧成品的身份：管理页的「视频」角标还要能对上已经装在表上的那张
        watchface_id: project?.watchface_id,
        job_id: project?.job_id,
        job_status: project?.job_status,
        result_size: project?.result_size,
      };
      await invoke("video_project_create", {
        metaJson: JSON.stringify(meta),
        videoB64: b64,
      });
      setProjectId(id);
      setVideoName(file.name);
      if (isNew) {
        draftCreatedRef.current = true;
        setDraftDirty(true);
      }
      setExitAsk(false);
      if (videoUrlRef.current) URL.revokeObjectURL(videoUrlRef.current);
      const url = URL.createObjectURL(file);
      videoUrlRef.current = url;
      setVideoUrl(url);
      setVideoDim(null);
      setDuration(0);
      setPlayhead(0);
      setTlZoom(1);
      setViewStart(0);
    } catch (e) {
      showToast(String(e), "error");
    } finally {
      setBusy("");
    }
  };

  const onPickFile = (e: React.ChangeEvent<HTMLInputElement>) => {
    const f = e.target.files?.[0];
    e.target.value = "";
    if (f) void saveVideo(f);
  };

  const videoBox = useMemo(() => {
    if (!videoDim) return { left: 0, top: 0, width: 100, height: 100 };
    const cover = Math.max(cw / videoDim.w, ch / videoDim.h) * zoom;
    const dw = videoDim.w * cover;
    const dh = videoDim.h * cover;
    const maxX = Math.max(1, (dw - cw) / 2);
    const maxY = Math.max(1, (dh - ch) / 2);
    const dx = (cw - dw) / 2 + panX * maxX;
    const dy = (ch - dh) / 2 + panY * maxY;
    return {
      left: (dx / cw) * 100,
      top: (dy / ch) * 100,
      width: (dw / cw) * 100,
      height: (dh / ch) * 100,
    };
  }, [videoDim, cw, ch, zoom, panX, panY]);

  /** 预览框：按画布比例等比，最大 168×210（与表盘编辑页同一个 frameSizeFor） */
  const previewFrame = useMemo(() => {
    const radius = (() => {
      switch (`${cw}x${ch}`) {
        case "212x520":
          return 104;
        case "192x490":
          return 280;
        case "336x480":
          return 45;
        case "432x514":
          return 104;
        default:
          return Math.round(Math.min(cw, ch) * 0.13);
      }
    })();
    const info: ScreenInfo = {
      width: cw,
      height: ch,
      radius,
      shape: cw === ch ? "circle" : "square",
    };
    return frameSizeFor(info, 168, 210);
  }, [cw, ch]);

  /** 轨道视野：缩放后只显示 [viewStartSafe, viewEndSafe] 这一段 */
  const viewSpan = duration > 0 ? duration / tlZoom : 1;
  const maxViewStart = Math.max(0, duration - viewSpan);
  const viewStartSafe = Math.max(0, Math.min(maxViewStart, viewStart));
  const viewEndSafe = viewStartSafe + viewSpan;
  const timeToPct = (t: number) =>
    viewSpan > 0 ? ((t - viewStartSafe) / viewSpan) * 100 : 0;
  const startPct = timeToPct(trimStart);
  const endPct = timeToPct(trimEnd);
  const playPct = timeToPct(playhead);

  /**
   * 进度条 → 预览：立刻 seek 一次，之后每 60ms 追一次最新值。
   * 不能用「防抖」：连续拖动时防抖会一直重置，手指不停就一直不画。
   * 拖动期间忽略 timeupdate，否则解码器回传的旧时间会把白点拽回去。
   */
  const scrubTo = (t: number) => {
    const next = Math.max(0, Math.min(duration > 0 ? duration : t, t));
    setPlayhead(next);
    scrubTargetRef.current = next;
    scrubbingRef.current = true;
    if (scrubEndTimerRef.current) window.clearTimeout(scrubEndTimerRef.current);
    scrubEndTimerRef.current = window.setTimeout(() => {
      scrubbingRef.current = false;
    }, 260);
    if (seekTimerRef.current !== undefined) return;
    const v = videoRef.current;
    if (v) {
      try {
        v.currentTime = next;
      } catch {
        /* 忽略 */
      }
    }
    seekTimerRef.current = window.setTimeout(() => {
      seekTimerRef.current = undefined;
      const target = scrubTargetRef.current;
      const vv = videoRef.current;
      if (target === null || !vv) return;
      try {
        vv.currentTime = target;
      } catch {
        /* 忽略 */
      }
      scrubTargetRef.current = null;
    }, 60);
  };

  const timeAtX = (clientX: number) => {
    const el = trimTrackRef.current;
    if (!el || duration <= 0) return 0;
    const r = el.getBoundingClientRect();
    if (r.width <= 0) return 0;
    const k = Math.min(1, Math.max(0, (clientX - r.left) / r.width));
    return viewStartSafe + k * viewSpan;
  };

  /** 轨道把手：窗口最长 5 秒、最短 0.3 秒，拖一端时另一端当锚 */
  const applyTrim = (which: "start" | "end", clientX: number) => {
    if (duration <= 0) return;
    const rect = trimTrackRef.current?.getBoundingClientRect();
    const nearLeft = rect ? clientX - rect.left < 12 : false;
    const nearRight = rect ? rect.right - clientX < 12 : false;
    const t = timeAtX(clientX);
    if (which === "start") {
      const next = Math.max(0, Math.max(trimEnd - 5, Math.min(t, trimEnd - 0.3)));
      setTrimStart(next);
      // 播放头只跟进度条走：拖把手/拖选区都不碰它
      // 拖出边缘时视野跟过去（贴着边缘继续拖 = 自动平移）
      if (next < viewStartSafe) setViewStart(Math.max(0, next));
      else if (nearLeft && viewStartSafe > 0) {
        setViewStart(Math.max(0, viewStartSafe - viewSpan * 0.1));
      } else if (nearRight) {
        setViewStart(Math.min(maxViewStart, viewStartSafe + viewSpan * 0.1));
      }
      return;
    }
    const next = Math.min(duration, Math.min(trimStart + 5, Math.max(t, trimStart + 0.3)));
    setTrimEnd(next);
    // 拖出边缘时视野跟过去
    if (next > viewEndSafe) setViewStart(Math.min(maxViewStart, next - viewSpan));
    else if (nearRight && viewStartSafe < maxViewStart) {
      setViewStart(Math.min(maxViewStart, viewStartSafe + viewSpan * 0.1));
    } else if (nearLeft) {
      setViewStart(Math.max(0, viewStartSafe - viewSpan * 0.1));
    }
  };

  const trackWidth = () =>
    trimTrackRef.current?.getBoundingClientRect().width || 1;

  /** 轨道空白处：单指平移（放大后）/ 单指拖整个选择框；两指捏合缩放 */
  const onTrackDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!videoDim || duration <= 0) return;
    try {
      e.currentTarget.setPointerCapture(e.pointerId);
    } catch {
      /* 老内核 */
    }
    tlPointersRef.current.set(e.pointerId, { x: e.clientX });
    if (tlPointersRef.current.size === 2) {
      const xs = [...tlPointersRef.current.values()].map((p) => p.x);
      const mid = (Math.min(...xs) + Math.max(...xs)) / 2;
      tlGestureRef.current = {
        kind: "pinch",
        startDist: Math.max(1, Math.abs(xs[0] - xs[1])),
        startZoom: tlZoom,
        anchorTime: timeAtX(mid),
        startView: viewStartSafe,
      };
      return;
    }
    if (tlPointersRef.current.size !== 1) return;
    const t = timeAtX(e.clientX);
    if (t >= trimStart && t <= trimEnd) {
      tlGestureRef.current = {
        kind: "move",
        startX: e.clientX,
        startTrim: trimStart,
        clip: clipLen,
      };
    } else if (tlZoom > 1.01) {
      tlGestureRef.current = { kind: "pan", startX: e.clientX, startView: viewStartSafe };
    } else {
      tlGestureRef.current = null;
    }
  };

  const onTrackMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!tlPointersRef.current.has(e.pointerId)) return;
    tlPointersRef.current.set(e.pointerId, { x: e.clientX });
    const g = tlGestureRef.current;
    if (!g || duration <= 0) return;
    if (g.kind === "pinch") {
      if (tlPointersRef.current.size < 2) return;
      const xs = [...tlPointersRef.current.values()]
        .map((p) => p.x)
        .sort((a, b) => a - b);
      const dist = Math.max(1, xs[xs.length - 1] - xs[0]);
      const z = Math.max(1, Math.min(12, g.startZoom * (dist / g.startDist)));
      const baseSpan = duration / g.startZoom;
      const ratio = Math.max(
        0,
        Math.min(1, (g.anchorTime - g.startView) / Math.max(0.001, baseSpan))
      );
      const span = duration / z;
      let ns = g.anchorTime - ratio * span;
      // 缩放后选区完全不在视野里时，把视野拉回选区（别让用户找不到蓝块）
      if (trimEnd < ns) ns = trimStart;
      else if (trimStart > ns + span) ns = trimEnd - span;
      setTlZoom(z);
      setViewStart(Math.max(0, Math.min(Math.max(0, duration - span), ns)));
      return;
    }
    if (g.kind === "pan") {
      const dt = ((e.clientX - g.startX) / trackWidth()) * viewSpan;
      setViewStart(Math.max(0, Math.min(maxViewStart, g.startView - dt)));
      return;
    }
    const dt = ((e.clientX - g.startX) / trackWidth()) * viewSpan;
    const ns = Math.max(0, Math.min(duration - g.clip, g.startTrim + dt));
    setTrimStart(ns);
    setTrimEnd(ns + g.clip);
    if (ns < viewStartSafe) setViewStart(Math.max(0, ns));
    else if (ns + g.clip > viewStartSafe + viewSpan) {
      setViewStart(Math.min(maxViewStart, ns + g.clip - viewSpan));
    }
  };

  const onTrackUp = (e: React.PointerEvent<HTMLDivElement>) => {
    tlPointersRef.current.delete(e.pointerId);
    if (tlPointersRef.current.size === 0) tlGestureRef.current = null;
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      /* 已经释放 */
    }
  };

  // 换视频就清空缩略图与缓存（时间是 key，不能跨视频复用，也不能露出上一个视频的帧）
  useEffect(() => {
    thumbCacheRef.current.clear();
    setTiles([]);
  }, [videoUrl]);

  // 轨道序列帧：按当前视野生成，放大后换成更密的帧（不是把旧图拉大）
  useEffect(() => {
    if (!videoUrl || duration <= 0) {
      setTiles([]);
      return;
    }
    const span = viewSpan;
    const start = viewStartSafe;
    const keyOf = (t: number) => (Math.round(t * 10) / 10).toFixed(1);
    const times = Array.from({ length: THUMB_TILES }, (_, i) =>
      Math.min(Math.max(0, duration - 0.03), start + ((i + 0.5) * span) / THUMB_TILES)
    );
    setTiles((prev) =>
      times.map((t, i) => {
        const k = keyOf(t);
        return { key: k, src: thumbCacheRef.current.get(k) ?? prev[i]?.src ?? null };
      })
    );
    const missing: Array<{ t: number; k: string }> = [];
    const seen = new Set<string>();
    times.forEach((t) => {
      const k = keyOf(t);
      if (seen.has(k) || thumbCacheRef.current.has(k)) return;
      seen.add(k);
      missing.push({ t, k });
    });
    if (!missing.length) return;

    let alive = true;
    let tv: HTMLVideoElement | null = null;
    const timer = window.setTimeout(async () => {
      if (!alive) return;
      tv = document.createElement("video");
      tv.src = videoUrl;
      tv.muted = true;
      tv.playsInline = true;
      tv.preload = "auto";
      tv.style.position = "fixed";
      tv.style.left = "-9999px";
      tv.style.width = "1px";
      tv.style.height = "1px";
      tv.style.opacity = "0";
      document.body.appendChild(tv);
      await new Promise<void>((resolve) => {
        let done = false;
        const ok = () => {
          if (done) return;
          done = true;
          resolve();
        };
        tv!.addEventListener("loadeddata", ok, { once: true });
        tv!.addEventListener("error", ok, { once: true });
        window.setTimeout(ok, 2000);
      });
      for (const m of missing) {
        if (!alive) break;
        if (thumbCacheRef.current.has(m.k)) continue;
        await seek(tv, m.t);
        if (!alive) break;
        const src = captureThumb(tv);
        if (!src) continue;
        const cache = thumbCacheRef.current;
        cache.set(m.k, src);
        // 缓存别无限长：超过 400 帧就丢最旧的
        if (cache.size > 400) {
          const first = cache.keys().next().value;
          if (first !== undefined) cache.delete(first);
        }
        setTiles((prev) => prev.map((tile) => (tile.key === m.k ? { ...tile, src } : tile)));
      }
      try {
        tv.removeAttribute("src");
        tv.load();
        tv.remove();
      } catch {
        /* 忽略 */
      }
    }, 180);

    return () => {
      alive = false;
      window.clearTimeout(timer);
      if (tv) {
        try {
          tv.removeAttribute("src");
          tv.load();
          tv.remove();
        } catch {
          /* 忽略 */
        }
      }
    };
  }, [videoUrl, duration, viewStartSafe, viewSpan]);

  const onTrimDown =
    (which: "start" | "end") => (e: React.PointerEvent<HTMLDivElement>) => {
      if (!videoDim) return;
      e.stopPropagation();
      trimDragRef.current = which;
      try {
        e.currentTarget.setPointerCapture(e.pointerId);
      } catch {
        /* 老内核拿不到 capture 也能靠后续 move 事件 */
      }
      applyTrim(which, e.clientX);
    };

  const onTrimMove = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!trimDragRef.current) return;
    e.stopPropagation();
    applyTrim(trimDragRef.current, e.clientX);
  };

  const onTrimUp = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!trimDragRef.current) return;
    e.stopPropagation();
    trimDragRef.current = null;
    try {
      e.currentTarget.releasePointerCapture(e.pointerId);
    } catch {
      /* 已经释放 */
    }
  };

  const onCropDown = (e: React.PointerEvent<HTMLDivElement>) => {
    if (!videoDim) return;
    // 点播放按钮不算拖拽，否则 pointer capture 会把 click 吞掉
    if ((e.target as HTMLElement).closest("button")) return;
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    dragRef.current = { x: e.clientX, y: e.clientY, px: panX, py: panY };
  };

  const onCropMove = (e: React.PointerEvent<HTMLDivElement>) => {
    const d = dragRef.current;
    const box = cropRef.current;
    if (!d || !box || !videoDim) return;
    const rect = box.getBoundingClientRect();
    const cover = Math.max(cw / videoDim.w, ch / videoDim.h) * zoom;
    const dw = videoDim.w * cover;
    const dh = videoDim.h * cover;
    const maxX = Math.max(1, (dw - cw) / 2);
    const maxY = Math.max(1, (dh - ch) / 2);
    const dxCanvas = ((e.clientX - d.x) / Math.max(1, rect.width)) * cw;
    const dyCanvas = ((e.clientY - d.y) / Math.max(1, rect.height)) * ch;
    const nx = Math.max(-1, Math.min(1, d.px + dxCanvas / maxX));
    const ny = Math.max(-1, Math.min(1, d.py + dyCanvas / maxY));
    setPanX(nx);
    setPanY(ny);
  };

  const onCropUp = () => {
    dragRef.current = null;
  };

  const previewPlay = () => {
    const v = videoRef.current;
    if (!v) return;
    if (seekTimerRef.current) {
      window.clearTimeout(seekTimerRef.current);
      seekTimerRef.current = undefined;
    }
    if (scrubEndTimerRef.current) {
      window.clearTimeout(scrubEndTimerRef.current);
      scrubEndTimerRef.current = undefined;
    }
    scrubbingRef.current = false;
    if (playing) {
      v.pause();
      setPlaying(false);
      return;
    }
    setPlayhead(trimStart);
    v.currentTime = trimStart;
    void v.play().then(() => setPlaying(true)).catch(() => setPlaying(false));
  };

  const onTimeUpdate = () => {
    const v = videoRef.current;
    if (!v) return;
    // 拖进度条时以白点为准：解码器回传的旧时间会把白点拽回去
    if (!scrubbingRef.current) setPlayhead(v.currentTime);
    if (playing && v.currentTime >= trimEnd - 0.01) {
      v.pause();
      setPlaying(false);
    }
  };

  // ---------------------------------------------------------------- 生成任务

  const generate = async () => {
    const inv = invoke;
    if (!inv) return;
    const v = videoRef.current;
    if (!v || !projectId || !style || !videoReady) {
      showToast("视频还没加载完，请等预览出现后再试", "error");
      return;
    }
    if (!(count >= 1 && count <= 300)) {
      showToast("帧数需在 1~300", "error");
      return;
    }
    if (fpsBad) {
      showToast(`帧率 ${fps.toFixed(1)} 超过 60，请减少帧数或延长视频`, "error");
      return;
    }
    if (durationBad) {
      showToast("播放时长不能超过 5 秒", "error");
      return;
    }
    setBusy("正在抽帧…");
    setProgress(0);
    if (seekTimerRef.current) {
      window.clearTimeout(seekTimerRef.current);
      seekTimerRef.current = undefined;
    }
    try {
      await inv("video_frame_reset", { projectId });
      const total = count;
      let sent = 0;
      let pending: string[] = [];
      let sendError: unknown = null;
      const sends: Promise<unknown>[] = [];
      const flush = () => {
        if (!pending.length) return;
        const batch = pending;
        pending = [];
        const startIndex = sent;
        sent += batch.length;
        sends.push(
          inv("video_frame_put", { projectId, startIndex, framesB64: batch }).catch((e) => {
            sendError = sendError || e;
          })
        );
      };
      await captureByPlaying(
        v,
        cw,
        ch,
        total,
        trimStart,
        clipLen,
        zoom,
        panX,
        panY,
        (b64) => {
          pending.push(b64);
          if (pending.length >= 6) flush();
        },
        (n) => setProgress(Math.round((n / total) * 100))
      );
      flush();
      await Promise.all(sends);
      if (sendError) throw sendError;
      setBusy("正在上传帧包…");
      const v2 = (await inv("video_job_create", {
        projectId,
        styleId: style.id,
        count,
        fps,
        mode,
      })) as any;
      await inv("video_project_update_meta", {
        projectId,
        patchJson: JSON.stringify({
          trim_start: trimStart,
          trim_end: trimEnd,
          zoom,
          pan_x: panX,
          pan_y: panY,
        }),
      });
      const jobId = v2?.job?.id || "";
      setJob({ id: jobId, status: "queued" });
      fetchedRef.current = false;
      draftCreatedRef.current = false;
      setDraftDirty(false);
      replaceScreen("job");
      void loadProjects();
      showToast("已提交，制作期间可以退出 App");
    } catch (e) {
      showToast(String(e), "error");
    } finally {
      setBusy("");
      setProgress(0);
    }
  };

  // 任务页轮询：完成 → 自动下载并保存成品
  useEffect(() => {
    const inv = invoke;
    const jobId = job?.id;
    if (screen !== "job" || !projectId || !jobId || !inv) return;
    let alive = true;
    let timer: number | undefined;
    const tick = async () => {
      try {
        const v = (await inv("video_job_status", { projectId })) as any;
        const st = v?.job?.status || "running";
        if (!alive) return;
        setJob({ id: jobId, status: st, error: v?.job?.error || "" });
        if (st === "done" && !fetchedRef.current) {
          fetchedRef.current = true;
          try {
            await inv("video_job_fetch", { projectId });
            if (!alive) return;
            toastRef.current("视频表盘已生成");
            void loadProjects();
            return;
          } catch (e) {
            // 下载失败（网络抖动/结果刚过期）→ 允许下一轮重试，别把用户卡在「已完成但没文件」
            fetchedRef.current = false;
            toastRef.current(`成品下载失败：${String(e)}`, "error");
          }
        }
        if (st === "failed") {
          toastRef.current(friendlyJobError(v?.job?.error), "error");
          return;
        }
      } catch (e) {
        if (alive) setJob((j) => (j ? { ...j, status: j.status || "running" } : j));
      }
      if (alive) timer = window.setTimeout(tick, 4000);
    };
    void tick();
    return () => {
      alive = false;
      if (timer) window.clearTimeout(timer);
    };
  }, [screen, projectId, job?.id, invoke, loadProjects]);

  /** 生成按钮的双通道触发：click + pointerup。
   *  部分安卓 WebView 在滚动容器/手势区里会把合成 click 吞掉，pointerup 仍然会到。 */
  const triggerGenerate = () => {
    const now = Date.now();
    if (now - genLockRef.current < 900) return;
    genLockRef.current = now;
    void generate();
  };
  const onGenDown = (e: React.PointerEvent<HTMLButtonElement>) => {
    genDownRef.current = { x: e.clientX, y: e.clientY };
  };
  const onGenUp = (e: React.PointerEvent<HTMLButtonElement>) => {
    const d = genDownRef.current;
    genDownRef.current = null;
    if (!d) return;
    if (Math.abs(e.clientX - d.x) + Math.abs(e.clientY - d.y) > 16) return;
    triggerGenerate();
  };

  // ---------------------------------------------------------------- 安装/清理

  const installBin = async (pid: string, binB64?: string) => {
    if (!invoke) return;
    if (!deviceConnected || !deviceAddr) {
      showToast("请先连接手表再安装", "error");
      return;
    }
    let data = binB64;
    try {
      if (!data) {
        const v = (await invoke("video_project_get", {
          projectId: pid,
          kind: "bin",
        })) as { data_b64: string };
        data = v.data_b64;
      }
    } catch (e) {
      showToast(String(e), "error");
      return;
    }
    const title =
      projects.find((x) => x.id === pid)?.title || project?.title || "视频表盘";
    // 走 App 的正式安装流程：进度条、阶段文案、成功页、失败重试都由那一页负责
    if (onInstallPayload) {
      onInstallPayload({
        addr: deviceAddr,
        b64: data,
        ext: "bin",
        name: title,
        kind: "watchface",
      });
      return;
    }
    // 兜底（浏览器预览/没有宿主回调时）：直接装，不经过正式进度页
    setBusy("正在安装…");
    setInstallBackLock(true);
    setKeepScreenOn(true);
    try {
      await invoke("device_install_payload", {
        addr: deviceAddr,
        dataB64: data,
        ext: "bin",
        rewriteWatchfaceId: null,
      });
      showToast("已安装到手表");
      onInstalled?.();
      void loadProjects();
    } catch (e) {
      showToast(String(e), "error");
    } finally {
      setInstallBackLock(false);
      setKeepScreenOn(false);
      setBusy("");
    }
  };

  const clearKinds = async (pid: string, kinds: string[]) => {
    if (!invoke) return;
    setBusy("正在清理…");
    try {
      await invoke("video_project_clear", { projectId: pid, kinds });
      showToast("已清理");
      const items = await loadProjects();
      const cur = items.find((x) => x.id === pid);
      if (cur) setProject(cur);
    } catch (e) {
      showToast(String(e), "error");
    } finally {
      setBusy("");
    }
  };

  const deleteProject = async (pid: string) => {
    if (!invoke) return;
    // 排队中/制作中禁止删除：服务端也拒，但先在本机拦住，避免出现「本地删了、服务器还在做」的孤儿成品
    const target = projects.find((x) => x.id === pid) || project;
    if (target && ["queued", "running"].includes(target.job_status || "")) {
      setConfirmDelete(false);
      showToast("任务还在制作中，暂时不能删除", "error");
      return;
    }
    setBusy("正在删除…");
    try {
      // 顺手取消服务端任务；运行中的任务会被服务端拒绝，结果到期自动清理。
      try {
        await invoke("video_job_delete", { projectId: pid });
      } catch {
        /* 没有任务/任务已结束都不影响本地删除 */
      }
      await invoke("video_project_delete", { projectId: pid });
      setConfirmDelete(false);
      showToast("项目已删除");
      backFromScreen();
      void loadProjects();
    } catch (e) {
      showToast(String(e), "error");
    } finally {
      setBusy("");
    }
  };

  /** 保存当前草稿到历史记录（视频选的时候已经落盘，这里补参数） */
  const saveDraft = async (): Promise<boolean> => {
    const inv = invoke;
    if (!inv || !projectId) {
      showToast("还没有可保存的视频", "error");
      return false;
    }
    try {
      await inv("video_project_update_meta", {
        projectId,
        patchJson: JSON.stringify({
          title: project?.title || videoName || "我的视频表盘",
          frame_count: count,
          fps: clipLen > 0 ? count / clipLen : 0,
          mode,
          trim_start: trimStart,
          trim_end: trimEnd,
          zoom,
          pan_x: panX,
          pan_y: panY,
        }),
      });
      draftCreatedRef.current = false;
      setDraftDirty(false);
      showToast("已保存到历史记录");
      void loadProjects();
      return true;
    } catch (e) {
      showToast(String(e), "error");
      return false;
    }
  };

  /** 删除本次未保存的草稿 */
  const discardDraft = async () => {
    const inv = invoke;
    if (!inv || !draftCreatedRef.current || !projectId) return;
    try {
      await inv("video_project_delete", { projectId });
    } catch {
      /* 删不掉也不拦着返回 */
    }
    draftCreatedRef.current = false;
    setDraftDirty(false);
    void loadProjects();
  };

  /** 编辑页的返回：有未保存草稿先问 */
  const requestLeaveEdit = () => {
    if (videoUrl && draftDirty) {
      setExitAsk(true);
      return;
    }
    backFromScreen();
  };

  /** 退出弹窗：保存 / 不保存后真正返回 */
  const confirmExit = async (mode: "save" | "discard") => {
    // 先收起弹窗：保存要走异步，避免用户连点两次把返回多退一级
    setExitAsk(false);
    if (mode === "save") {
      const ok = await saveDraft();
      if (!ok) return;
    } else {
      await discardDraft();
    }
    exitConfirmedRef.current = true;
    try {
      window.history.back();
    } catch {
      exitConfirmedRef.current = false;
      popScreen();
    }
  };

  const openJob = (p: Project) => {
    if (!p.job_id) return;
    setProject(p);
    setProjectId(p.id);
    setJob({ id: p.job_id, status: p.job_status || "queued", error: p.job_error });
    fetchedRef.current = false;
    pushScreen("job");
  };

  // ---------------------------------------------------------------- 小部件

  const Header = ({
    title,
    onBackPress,
    right,
  }: {
    title: string;
    onBackPress: () => void;
    right?: React.ReactNode;
  }) => (
    <div className="relative flex items-center justify-between pb-3">
      <button
        type="button"
        onClick={onBackPress}
        className="w-11 h-11 -ml-2.5 rounded-full flex items-center justify-center text-black/80 active:scale-95 transition-all cursor-pointer"
      >
        <CaretLeft className="w-6 h-6 stroke-[2]" />
      </button>
      <span className="absolute left-1/2 -translate-x-1/2 max-w-[58%] truncate text-[17px] font-bold text-black tracking-tight">
        {title}
      </span>
      <div className="w-14 h-11 flex items-center justify-end">{right}</div>
    </div>
  );

  const Empty = ({ text }: { text: string }) => (
    <div className="bg-white rounded-3xl p-6 text-center">
      <p className="text-[13px] font-medium text-black/40">{text}</p>
    </div>
  );

  // ---------------------------------------------------------------- 首页

  if (screen === "home") {
    const totalSize = projects.reduce(
      (n, p) => n + (p.video_size || 0) + (p.frames_size || 0) + (p.bin_size || 0),
      0
    );
    // 入口卡上的任务状态：取最近更新的、提交过制作的记录；
    // 结果性状态（已完成/失败）只在用户还没进历史记录看过时提示
    const latestJob = projects.find((p) => !!p.job_id);
    const latestStatus = latestJob?.job_status || "";
    const latestActive = ["queued", "running"].includes(latestStatus);
    const latestTerminal = latestStatus === "done" || latestStatus === "failed";
    const latestSeen =
      !!latestJob && seenJobs[latestJob.id] === `${latestStatus}:${latestJob.job_id || ""}`;
    const showJobBadge = latestActive || (latestTerminal && !latestSeen);
    return (
      <div
        data-rb="y"
        data-rb-always="1"
        className="flex-1 flex flex-col overflow-y-auto no-scrollbar px-6 safe-top pb-10 page-enter-anim select-none"
      >
        <Header title="视频表盘" onBackPress={onBack} />

        <div className="flex-1 flex flex-col justify-center gap-3">
          <div className="bg-white rounded-3xl p-5">
            <div className="w-12 h-12 rounded-2xl bg-[#4361EE]/10 text-[#4361EE] flex items-center justify-center">
              <VideoCamera className="w-6 h-6" />
            </div>
            <h2 className="text-[22px] font-bold text-black tracking-tight mt-4">制作视频表盘</h2>
            <p className="text-[13px] font-medium text-black/45 mt-1.5 leading-relaxed">
              选择一段视频，把它制作成动态表盘
            </p>
            <button
              type="button"
              onClick={openTemplates}
              className="mt-4 w-full h-12 rounded-2xl bg-[#4361EE] text-white text-[16px] font-bold active:scale-[0.99] transition-all cursor-pointer flex items-center justify-center gap-2"
            >
              <Plus className="w-5 h-5 stroke-[2.4]" />
              开始制作
            </button>
          </div>

          <button
            type="button"
            onClick={() => pushScreen("projects")}
            className="w-full text-left bg-white rounded-3xl p-4 flex items-center justify-between active:scale-[0.98] transition-all cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.02)]"
          >
            <div className="flex items-center gap-3.5">
              <div className="w-11 h-11 rounded-xl bg-[#4361EE]/10 text-[#4361EE] flex items-center justify-center shrink-0">
                <FilmStrip className="w-5 h-5 stroke-[2]" />
              </div>
              <div className="min-w-0">
                <h3 className="text-[17px] font-bold text-black leading-snug tracking-tight">
                  历史记录
                </h3>
                <p className="text-[13px] font-medium text-black/40 mt-0.5 truncate">
                  {projects.length > 0
                    ? `${projects.length} 个，${fmtSize(totalSize)}`
                    : "还没有历史记录"}
                </p>
              </div>
            </div>
            <div className="flex items-center gap-2 shrink-0">
              {showJobBadge && (
                <span
                  className={`flex items-center gap-1 text-[12px] font-bold ${
                    latestActive
                      ? "text-[#4361EE]"
                      : latestStatus === "done"
                        ? "text-[#34A853]"
                        : "text-[#FF3B30]"
                  }`}
                >
                  {latestActive && <ArrowClockwise className="w-3.5 h-3.5 animate-spin" />}
                  {statusLabel(latestStatus)}
                </span>
              )}
              <CaretRight className="w-5 h-5 text-black/25 stroke-[2]" />
            </div>
          </button>
        </div>

        {busy && (
          <div className="fixed inset-0 z-[80] bg-black/30 flex items-center justify-center px-8">
            <div className="bg-white rounded-2xl px-5 py-4 flex items-center gap-3 shadow-xl">
              <ArrowClockwise className="w-5 h-5 text-[#4361EE] animate-spin" />
              <span className="text-[14px] font-bold text-black">{busy}</span>
            </div>
          </div>
        )}
      </div>
    );
  }

  // ---------------------------------------------------------------- 历史记录

  if (screen === "projects") {
    return (
      <div
        data-rb="y"
        data-rb-always="1"
        className="flex-1 flex flex-col overflow-y-auto no-scrollbar px-6 safe-top pb-10 page-enter-anim select-none"
      >
        <Header title="历史记录" onBackPress={backFromScreen} />

        {loadingProjects ? (
          <Empty text="正在读取历史记录…" />
        ) : projects.length === 0 ? (
          <Empty text="还没有历史记录" />
        ) : (
          <div className="flex flex-col gap-3">
            {projects.map((p) => {
              const active = ["queued", "running"].includes(p.job_status || "");
              const done = p.has_bin || p.job_status === "done";
              const failed = p.job_status === "failed";
              const total =
                (p.video_size || 0) + (p.frames_size || 0) + (p.bin_size || 0);
              return (
                <button
                  key={p.id}
                  type="button"
                  onClick={() => openProject(p)}
                  className="w-full text-left bg-white rounded-3xl p-4 flex items-center gap-3 active:scale-[0.98] transition-all cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.02)]"
                >
                  <div className="w-12 h-16 rounded-xl bg-black/5 overflow-hidden shrink-0">
                    {previewSrc(p) ? (
                      <img
                        src={previewSrc(p)}
                        alt=""
                        className="w-full h-full object-cover"
                      />
                    ) : (
                      <div className="w-full h-full flex items-center justify-center text-black/20">
                        <FilmStrip className="w-5 h-5" />
                      </div>
                    )}
                  </div>
                  <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                      <h4 className="text-[16px] font-bold text-black truncate">
                        {p.title || "视频表盘"}
                      </h4>
                      {active && (
                        <span className="shrink-0 text-[11px] font-bold text-[#D4973B] bg-[#D4973B]/10 px-2 py-0.5 rounded-full flex items-center gap-1">
                          <ArrowClockwise className="w-3 h-3 animate-spin" />
                          {statusLabel(p.job_status)}
                        </span>
                      )}
                      {done && !active && (
                        <span className="shrink-0 text-[11px] font-bold text-[#34A853] bg-[#34A853]/10 px-2 py-0.5 rounded-full">
                          已完成
                        </span>
                      )}
                      {failed && !active && (
                        <span className="shrink-0 text-[11px] font-bold text-[#FF3B30] bg-[#FF3B30]/10 px-2 py-0.5 rounded-full">
                          制作失败
                        </span>
                      )}
                    </div>
                    <p className="text-[12px] font-medium text-black/40 mt-1 truncate">
                      {p.style_name || "样式"}，{p.frame_count || 0} 帧
                    </p>
                    <p className="text-[12px] font-medium text-black/30 mt-0.5">
                      {fmtDate(p.updated_at || p.created_at)}，{fmtSize(total)}
                    </p>
                    {active && (
                      <div className="mt-2 h-1 rounded-full bg-[#D4973B]/15 overflow-hidden">
                        <div className="wf-indeterminate h-full w-1/3 rounded-full bg-[#D4973B]" />
                      </div>
                    )}
                  </div>
                  <CaretRight className="w-5 h-5 text-black/20 shrink-0" />
                </button>
              );
            })}
          </div>
        )}
      </div>
    );
  }

  // ---------------------------------------------------------------- 模板列表

  if (screen === "templates") {
    // 只给当前设备能用的样式（画布一致）；认不出设备时不筛，交给用户判断。
    const shownTemplates = deviceScreen
      ? templates
          .map((t) => ({
            ...t,
            styles: t.styles.filter(
              (s) => s.canvas_w === deviceScreen[0] && s.canvas_h === deviceScreen[1]
            ),
          }))
          .filter((t) => t.styles.length > 0)
      : templates;
    // 全部模板都没解锁：顶部只留一条状态 + 一个购买动作，卡片照常展示（能看见就是卖点）
    const allLocked = shownTemplates.length > 0 && shownTemplates.every((t) => !t.unlocked);
    const buyUrl = shownTemplates.find((t) => !t.unlocked)?.buy_url || "https://afdian.com/a/Hobjl";
    return (
      <div
        data-rb="y"
        data-rb-always="1"
        className="flex-1 flex flex-col overflow-y-auto no-scrollbar px-6 safe-top pb-10 page-enter-anim select-none"
      >
        <Header title="选择模板" onBackPress={backFromScreen} />
        {loadingTemplates ? (
          <Empty text="正在读取模板…" />
        ) : templatesError ? (
          <div className="bg-white rounded-3xl p-5 text-center">
            <p className="text-[13px] font-medium text-black/45">{templatesError}</p>
            <button
              type="button"
              onClick={() => {
                if (templatesError.includes("绑定")) onOpenPaid();
                else void loadTemplates();
              }}
              className="mt-3 h-11 px-5 rounded-full bg-[#4361EE] text-white text-[14px] font-bold cursor-pointer"
            >
              {templatesError.includes("绑定") ? "去绑定爱发电" : "重试"}
            </button>
          </div>
        ) : shownTemplates.length === 0 ? (
          <Empty
            text={
              deviceScreen
                ? `当前设备（${deviceScreen[0]}×${deviceScreen[1]}）还没有可用的视频表盘模板。`
                : "还没有可用的视频表盘模板。"
            }
          />
        ) : (
          <div className="flex flex-col gap-4">
            {allLocked && (
              <div className="flex items-center justify-between gap-3 bg-white rounded-2xl px-4 py-3">
                <div className="flex items-center gap-2 min-w-0">
                  <LockSimple className="w-4 h-4 shrink-0 text-[#D4973B]" />
                  <span className="text-[14px] font-bold text-black truncate">未解锁</span>
                </div>
                <button
                  type="button"
                  onClick={() => invoke && void invoke("open_external", { url: buyUrl })}
                  className="h-9 px-4 shrink-0 rounded-full bg-[#4361EE] text-white text-[13px] font-bold active:scale-95 transition-all cursor-pointer"
                >
                  去爱发电购买
                </button>
              </div>
            )}
            {shownTemplates.map((tpl) => (
              <div key={tpl.id} className="bg-white rounded-3xl p-4 flex flex-col gap-3">
                <div className="flex items-center justify-between">
                  <div>
                    <h3 className="text-[17px] font-bold text-black">{tpl.name}</h3>
                    {!allLocked && (
                      <p className="text-[12px] font-medium text-black/40 mt-0.5">
                        {tpl.unlocked ? "已解锁全部样式" : "购买后解锁这个模板包的全部样式"}
                      </p>
                    )}
                  </div>
                  {tpl.unlocked ? (
                    <CheckCircle className="w-5 h-5 text-[#34A853]" />
                  ) : (
                    <LockSimple className="w-5 h-5 text-black/25" />
                  )}
                </div>
                <div className="flex flex-col gap-2">
                  {tpl.styles.map((st) => (
                    <button
                      key={st.id}
                      type="button"
                      onClick={() => startNew(tpl, st)}
                      className={`flex items-center gap-3 rounded-2xl p-2.5 text-left active:scale-[0.99] transition-all cursor-pointer ${
                        tpl.unlocked ? "bg-black/[0.03]" : "bg-black/[0.02] opacity-70"
                      }`}
                    >
                      <div className="w-10 h-14 rounded-lg bg-black/5 overflow-hidden shrink-0">
                        {st.preview_url ? (
                          <img
                            src={`${API_BASE}${st.preview_url}`}
                            alt=""
                            className="w-full h-full object-cover"
                          />
                        ) : (
                          <div className="w-full h-full flex items-center justify-center text-black/20">
                            <FilmStrip className="w-4 h-4" />
                          </div>
                        )}
                      </div>
                      <div className="flex-1 min-w-0">
                        <p className="text-[14px] font-bold text-black truncate">{st.name}</p>
                        <p className="text-[12px] font-medium text-black/40 mt-0.5">
                          {st.canvas_w}×{st.canvas_h} 画布
                        </p>
                      </div>
                      {tpl.unlocked ? (
                        <CaretRight className="w-4 h-4 text-black/20" />
                      ) : (
                        <LockSimple className="w-4 h-4 text-black/25" />
                      )}
                    </button>
                  ))}
                </div>
                {!allLocked && !tpl.unlocked && (
                  <button
                    type="button"
                    onClick={() => invoke && void invoke("open_external", { url: tpl.buy_url })}
                    className="h-11 rounded-xl bg-[#4361EE] text-white text-[14px] font-bold active:scale-[0.99] transition-all cursor-pointer"
                  >
                    去爱发电解锁
                  </button>
                )}
              </div>
            ))}
          </div>
        )}
      </div>
    );
  }

  // ---------------------------------------------------------------- 制作页

  if (screen === "edit") {
    return (
      <div className="flex-1 flex flex-col overflow-hidden bg-[#F5F6F9] page-enter-anim select-none relative">
        <div className="px-4 safe-top pb-1 shrink-0 z-20">
          <Header
            title={project ? "继续制作" : "制作视频表盘"}
            onBackPress={requestLeaveEdit}
            right={
              <button
                type="button"
                disabled={!videoUrl || !projectId || !!busy}
                onClick={() => void saveDraft()}
                className={`h-8 px-3 rounded-full text-[12px] font-bold whitespace-nowrap transition-all ${
                  !videoUrl || !projectId || busy
                    ? "bg-black/[0.05] text-black/25"
                    : "bg-[#4361EE] text-white cursor-pointer active:scale-95"
                }`}
              >
                保存
              </button>
            }
          />
        </div>

        {/* 顶部固定：设备预览（往下滚不会把画布带走） */}
        <div className="shrink-0 px-4 pb-1">
          <div
            ref={cropRef}
            className="relative mx-auto overflow-hidden bg-black shadow-[0_8px_24px_rgba(0,0,0,0.10)]"
            style={{
              width: `${previewFrame.w}px`,
              height: `${previewFrame.h}px`,
              borderRadius: `${previewFrame.radius}px`,
              touchAction: "none",
            }}
            onPointerDown={onCropDown}
            onPointerMove={onCropMove}
            onPointerUp={onCropUp}
            onPointerCancel={onCropUp}
          >
            {videoUrl ? (
              <video
                ref={videoRef}
                src={videoUrl}
                playsInline
                muted
                preload="auto"
                onLoadedMetadata={onLoadedMeta}
                onTimeUpdate={onTimeUpdate}
                onEnded={() => setPlaying(false)}
                onError={() =>
                  toastRef.current("这个视频手机解不了，请换一段 MP4 视频", "error")
                }
                className="absolute max-w-none select-none"
                style={{
                  left: `${videoBox.left}%`,
                  top: `${videoBox.top}%`,
                  width: `${videoBox.width}%`,
                  height: `${videoBox.height}%`,
                  objectFit: "fill",
                }}
              />
            ) : (
              <button
                type="button"
                onClick={() => fileRef.current?.click()}
                className="absolute inset-0 flex flex-col items-center justify-center gap-2 text-white/70 cursor-pointer"
              >
                <VideoCamera className="w-8 h-8" />
                <span className="text-[14px] font-bold">选择视频</span>
                <span className="text-[11px] font-medium text-white/40">5 秒以内，最大 80MB</span>
              </button>
            )}
            {videoUrl && (
              <button
                type="button"
                onClick={previewPlay}
                className="absolute left-1/2 top-1/2 -translate-x-1/2 -translate-y-1/2 w-12 h-12 rounded-full bg-black/45 text-white flex items-center justify-center backdrop-blur-sm active:scale-95 transition-all cursor-pointer"
              >
                {playing ? <Pause className="w-5 h-5" /> : <Play className="w-5 h-5" />}
              </button>
            )}
          </div>

          <p className="mt-1.5 text-center text-[11px] font-medium text-black/35">
            拖动画面调整位置
          </p>
        </div>

        <input
          ref={fileRef}
          type="file"
          accept="video/*"
          className="hidden"
          onChange={onPickFile}
        />

        <div
          data-rb="y"
          data-rb-always="1"
          className="flex-1 min-h-0 overflow-y-auto no-scrollbar px-4 pb-4 flex flex-col gap-3"
        >

          {videoUrl && (
            <button
              type="button"
              onClick={() => fileRef.current?.click()}
              className="w-full bg-white rounded-2xl p-4 flex items-center justify-between active:scale-[0.99] transition-all cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.02)]"
            >
              <div className="flex items-center gap-3 min-w-0">
                <div className="w-10 h-10 rounded-xl bg-[#4361EE]/10 text-[#4361EE] flex items-center justify-center shrink-0">
                  <VideoCamera className="w-5 h-5" />
                </div>
                <div className="min-w-0 text-left">
                  <p className="text-[15px] font-bold text-black">更换视频</p>
                  <p className="text-[12px] font-medium text-black/40 mt-0.5 truncate">
                    {videoName || "已选视频"}
                  </p>
                </div>
              </div>
              <CaretRight className="w-5 h-5 text-black/20 shrink-0" />
            </button>
          )}

          <section className="bg-white rounded-2xl p-4 flex flex-col gap-3">
            <div className="flex items-center justify-between">
              <h3 className="text-[16px] font-bold text-black flex items-center gap-2">
                <Scissors className="w-4 h-4 text-[#4361EE]" />
                截取片段
              </h3>
              <span className="text-[12px] font-bold text-black/40">
                {duration > 0
                  ? tlZoom > 1.01
                    ? `${tlZoom.toFixed(1)}x，已选 ${fmtTime(clipLen)}`
                    : `已选 ${fmtTime(clipLen)}`
                  : "—"}
              </span>
            </div>

            {/* 轨道：缩略图 + 选区；单指拖选区/平移，双指缩放 */}
            <div
              ref={trimTrackRef}
              className="relative h-14 rounded-xl bg-black/[0.06]"
              style={{ touchAction: "none" }}
              onPointerDown={onTrackDown}
              onPointerMove={onTrackMove}
              onPointerUp={onTrackUp}
              onPointerCancel={onTrackUp}
            >
              <div className="absolute inset-0 rounded-xl overflow-hidden">
                {/* 序列帧：16 格铺满当前视野，放大后按新视野重新取帧 */}
                <div className="absolute inset-0 flex h-full">
                  {Array.from({ length: THUMB_TILES }).map((_, i) => {
                    const src = tiles[i]?.src;
                    return src ? (
                      <img key={i} src={src} alt="" className="h-full flex-1 object-cover" />
                    ) : (
                      <div key={i} className="h-full flex-1 bg-black/[0.05]" />
                    );
                  })}
                </div>

                {/* 选区外压暗（全部裁在轨道里，蓝色不会跑到外面） */}
                <div
                  className="absolute inset-y-0 left-0 bg-white/55"
                  style={{ width: `${Math.max(0, Math.min(100, startPct))}%` }}
                />
                <div
                  className="absolute inset-y-0 right-0 bg-white/55"
                  style={{
                    width: `${Math.max(0, 100 - Math.max(0, Math.min(100, endPct)))}%`,
                  }}
                />
                <div
                  className="absolute inset-y-0 bg-[#4361EE]/20"
                  style={{
                    left: `${startPct}%`,
                    width: `${Math.max(0, endPct - startPct)}%`,
                  }}
                />
                <div
                  className="absolute inset-y-0 w-0.5 bg-[#4361EE]"
                  style={{ left: `${startPct}%` }}
                />
                <div
                  className="absolute inset-y-0 w-0.5 bg-[#4361EE]"
                  style={{ left: `${endPct}%` }}
                />
                <div
                  className="absolute inset-y-0 w-0.5 -translate-x-1/2 bg-white shadow-[0_0_0_1px_rgba(0,0,0,0.2)]"
                  style={{ left: `${playPct}%` }}
                />
              </div>

              {/* 两端把手（在裁切层外面，靠边也不会被切掉） */}
              {(["start", "end"] as const).map((which) => {
                const pct = which === "start" ? startPct : endPct;
                const inView = pct >= -0.5 && pct <= 100.5;
                return (
                  <div
                    key={which}
                    role="slider"
                    aria-valuemin={0}
                    aria-valuemax={duration}
                    aria-valuenow={which === "start" ? trimStart : trimEnd}
                    onPointerDown={onTrimDown(which)}
                    onPointerMove={onTrimMove}
                    onPointerUp={onTrimUp}
                    onPointerCancel={onTrimUp}
                    className="absolute inset-y-0 w-6 -translate-x-1/2 flex items-center justify-center cursor-grab active:cursor-grabbing"
                    style={{
                      left: `${pct}%`,
                      touchAction: "none",
                      opacity: inView ? 1 : 0,
                      pointerEvents: inView ? "auto" : "none",
                    }}
                  >
                    <div className="w-3.5 h-10 rounded-md bg-white shadow-[0_1px_4px_rgba(0,0,0,0.22)] flex items-center justify-center">
                      <div className="w-0.5 h-4 rounded-full bg-[#4361EE]" />
                    </div>
                  </div>
                );
              })}
            </div>

            <div className="flex items-center gap-2">
              <span className="w-9 shrink-0 text-[11px] font-mono font-medium text-black/40">
                {fmtTime(Math.min(clipLen, Math.max(0, playhead - trimStart)))}
              </span>
              <DragSlider
                value={Math.min(clipLen, Math.max(0, playhead - trimStart))}
                min={0}
                max={Math.max(0.1, clipLen)}
                step={0.05}
                onChange={(v) => scrubTo(trimStart + v)}
                disabled={!videoDim}
              />
              <span className="w-9 shrink-0 text-right text-[11px] font-mono font-medium text-black/40">
                {fmtTime(clipLen)}
              </span>
            </div>
          </section>

          <section className="bg-white rounded-2xl p-4">
            <GenSlider
              icon={<ArrowsOutSimple className="w-4 h-4" />}
              label="缩放"
              value={zoom}
              ctl={{ default: 1, min: 1, max: 3, step: 0.01, adjustable: true }}
              text={`${zoom.toFixed(2)}x`}
              onChange={(v) => setZoom(v)}
              disabled={!videoDim}
            />
          </section>

          <section className="bg-white rounded-2xl p-4 flex flex-col gap-3">
            <div className="flex items-center justify-between">
              <h3 className="text-[16px] font-bold text-black">流畅度</h3>
              <button
                type="button"
                onClick={() => {
                  if (!customOpen) setCustomCount(String(count));
                  setCustomOpen(!customOpen);
                }}
                className={`h-7 px-3 rounded-full text-[12px] font-bold active:scale-95 transition-all cursor-pointer ${
                  customOpen || quality === "custom"
                    ? "bg-[#4361EE]/10 text-[#4361EE]"
                    : "bg-black/[0.04] text-black/45"
                }`}
              >
                高级
              </button>
            </div>
            <div className="grid grid-cols-3 gap-2">
              {QUALITY_LEVELS.map((level) => (
                <button
                  key={level.key}
                  type="button"
                  onClick={() => {
                    setQuality(level.key);
                    const n = framesForQuality(level.key, clipLen);
                    setCount(n);
                    setCustomCount(String(n));
                    setCustomOpen(false);
                  }}
                  className={`h-10 rounded-xl text-[13px] font-bold active:scale-[0.98] transition-all cursor-pointer ${
                    quality === level.key
                      ? "bg-[#4361EE] text-white"
                      : "bg-black/[0.04] text-black/60"
                  }`}
                >
                  {level.label}
                </button>
              ))}
            </div>
            {customOpen && (
              <div className="flex flex-col gap-1.5">
                <span className="text-[12px] font-semibold text-black/50">自定义帧数</span>
                <div className="flex items-center gap-2">
                  <input
                    type="text"
                    inputMode="numeric"
                    pattern="[0-9]*"
                    value={customCount}
                    onChange={(e) => {
                      const s = e.target.value.replace(/[^\d]/g, "").slice(0, 3);
                      setCustomCount(s);
                      const n = parseInt(s, 10);
                      if (Number.isFinite(n) && n >= 1 && n <= 300) {
                        setCount(n);
                        setQuality("custom");
                      }
                    }}
                    onBlur={() => {
                      const n = parseInt(customCount, 10);
                      if (!customCount) {
                        setCustomCount(String(count));
                        return;
                      }
                      if (Number.isFinite(n)) {
                        const v = Math.max(1, Math.min(300, n));
                        setCustomCount(String(v));
                        setCount(v);
                        setQuality("custom");
                      }
                    }}
                    className="w-full h-10 rounded-xl bg-black/[0.04] px-3 text-[14px] font-semibold text-black outline-none"
                    placeholder="1~300"
                  />
                  <span className="text-[12px] font-medium text-black/35 shrink-0">帧</span>
                </div>
                <p className="text-[12px] font-medium text-black/35">
                  {clipLen > 0
                    ? `每秒约 ${(count / clipLen).toFixed(1)} 帧，最多 ${maxFrames} 帧`
                    : "选择视频后可自定义帧数"}
                </p>
              </div>
            )}
            {fpsBad && (
              <p className="text-[11px] font-medium text-[#FF3B30]">
                帧率超过 60，请减少帧数或延长片段。
              </p>
            )}

          </section>

          <section className="bg-white rounded-2xl p-4 flex flex-col gap-3">
            <h3 className="text-[16px] font-bold text-black">播放方式</h3>
            <div className="flex rounded-full bg-black/5 p-1">
              <button
                type="button"
                onClick={() => setMode("loop")}
                className={`flex-1 h-9 rounded-full text-[13px] font-semibold transition-all cursor-pointer ${
                  mode === "loop" ? "bg-white text-black shadow-sm" : "text-black/50"
                }`}
              >
                循环播放
              </button>
              <button
                type="button"
                onClick={() => setMode("once")}
                className={`flex-1 h-9 rounded-full text-[13px] font-semibold transition-all cursor-pointer ${
                  mode === "once" ? "bg-white text-black shadow-sm" : "text-black/50"
                }`}
              >
                播放一次
              </button>
            </div>

          </section>
        </div>

        <div
          data-pf-bottombar
          className="px-4 pt-2 safe-bottom-panel bg-white border-t border-black/5 shrink-0 z-30"
        >
          <button
            type="button"
            disabled={!videoReady || !projectId || fpsBad || durationBad || !!busy}
            onClick={triggerGenerate}
            onPointerDown={onGenDown}
            onPointerUp={onGenUp}
            className={`w-full h-12 rounded-2xl text-[16px] font-bold active:scale-[0.99] transition-all ${
              !videoReady || !projectId || fpsBad || durationBad || busy
                ? "bg-black/[0.06] text-black/25"
                : "bg-[#4361EE] text-white cursor-pointer"
            }`}
          >
            {busy
              ? busy.startsWith("正在抽帧") && progress > 0
                ? `正在抽帧 ${progress}%`
                : busy
              : "生成视频表盘"}
          </button>
        </div>

        {busy && progress > 0 && (
          <div className="fixed inset-0 z-[80] bg-black/30 flex items-center justify-center px-8">
            <div className="bg-white rounded-2xl px-5 py-4 w-full max-w-xs text-center">
              <p className="text-[14px] font-bold text-black">{busy}</p>
              <div className="h-2 rounded-full bg-black/10 mt-3 overflow-hidden relative">
                <div
                  className="h-full bg-[#4361EE] transition-[width] duration-200"
                  style={{ width: `${progress}%` }}
                />
                <div className="wf-shimmer absolute inset-y-0 left-0 w-1/3 bg-gradient-to-r from-transparent via-white/50 to-transparent" />
              </div>
            </div>
          </div>
        )}

        {exitAsk && (
          <div
            className="absolute inset-0 z-50 bg-black/40 flex items-center justify-center p-6"
            onClick={() => setExitAsk(false)}
          >
            <div
              className="bg-white w-full max-w-[280px] rounded-3xl overflow-hidden shadow-[0_8px_24px_rgba(0,0,0,0.10)] border border-black/5 text-center"
              onClick={(e) => e.stopPropagation()}
            >
              <div className="pt-6 px-6 pb-5">
                <h3 className="text-[17px] font-bold text-black tracking-tight leading-snug">
                  当前表盘还未保存
                </h3>
                <p className="text-[13px] text-black/40 font-medium mt-1.5 leading-relaxed">
                  是否保存到历史记录？
                </p>
              </div>
              <div className="grid grid-cols-2 border-t border-black/5 divide-x divide-black/5">
                <button
                  type="button"
                  onClick={() => void confirmExit("discard")}
                  className="py-3.5 text-[16px] font-bold text-[#C7332B] active:bg-black/5 transition-colors cursor-pointer"
                >
                  不保存
                </button>
                <button
                  type="button"
                  onClick={() => void confirmExit("save")}
                  className="py-3.5 text-[16px] font-bold text-[#4361EE] active:bg-black/5 transition-colors cursor-pointer"
                >
                  保存
                </button>
              </div>
            </div>
          </div>
        )}
      </div>
    );
  }

  // ---------------------------------------------------------------- 任务进度

  if (screen === "job") {
    const st = job?.status || "queued";
    const done = st === "done";
    const failed = st === "failed";
    return (
      <div className="flex-1 flex flex-col overflow-hidden bg-[#F5F6F9] page-enter-anim select-none">
        <div className="px-4 safe-top pb-1 shrink-0">
          <Header title="制作进度" onBackPress={backFromScreen} />
        </div>
        <div className="flex-1 min-h-0 overflow-y-auto no-scrollbar px-6 pb-10 flex flex-col items-center justify-center">
          <div
            className={`w-16 h-16 rounded-full flex items-center justify-center ${
              done ? "bg-[#34A853]/10 text-[#34A853]" : failed ? "bg-[#FF3B30]/10 text-[#FF3B30]" : "bg-[#4361EE]/10 text-[#4361EE]"
            }`}
          >
            {done ? (
              <CheckCircle className="w-8 h-8" />
            ) : failed ? (
              <WarningCircle className="w-8 h-8" />
            ) : (
              <ArrowClockwise className="w-8 h-8 animate-spin" />
            )}
          </div>
          <h2 className="text-[20px] font-bold text-black mt-4">
            {done ? "视频表盘已生成" : failed ? "制作失败" : statusLabel(st)}
          </h2>
          <p className="text-[13px] font-medium text-black/45 mt-2 text-center leading-relaxed">
            {done
              ? "成品已保存，可以安装到手表了。"
              : failed
                ? friendlyJobError(job?.error)
                : "预计 5~6 分钟，制作期间可以退出 App。"}
          </p>
          {!done && !failed && (
            <p className="text-[12px] font-medium text-black/30 mt-4">
              任务 {job?.id || "—"}
            </p>
          )}
          {done && (
            <button
              type="button"
              onClick={() => projectId && void installBin(projectId)}
              className="mt-6 w-full max-w-xs h-12 rounded-2xl bg-[#4361EE] text-white text-[16px] font-bold active:scale-[0.99] transition-all cursor-pointer flex items-center justify-center gap-2"
            >
              <DeviceMobile className="w-5 h-5" />
              安装到手表
            </button>
          )}
          {(failed || done) && (
            <button
              type="button"
              onClick={() => {
                const p = projects.find((x) => x.id === projectId) || project;
                if (p) void editExisting(p, "replace");
                else backFromScreen();
              }}
              className="mt-3 h-11 px-5 rounded-full bg-white text-[14px] font-bold text-black/60 active:scale-95 transition-all cursor-pointer"
            >
              {failed ? "调整后重试" : "继续制作"}
            </button>
          )}
        </div>
        {busy && (
          <div className="fixed inset-0 z-[80] bg-black/30 flex items-center justify-center px-8">
            <div className="bg-white rounded-2xl px-5 py-4 flex items-center gap-3 shadow-xl">
              <ArrowClockwise className="w-5 h-5 text-[#4361EE] animate-spin" />
              <span className="text-[14px] font-bold text-black">{busy}</span>
            </div>
          </div>
        )}
      </div>
    );
  }

  // ---------------------------------------------------------------- 作品详情

  const p = project;
  const total = (p?.video_size || 0) + (p?.frames_size || 0) + (p?.bin_size || 0);
  const jobActive = ["queued", "running"].includes(p?.job_status || "");
  return (
    <div
      data-rb="y"
      data-rb-always="1"
      className="flex-1 flex flex-col overflow-y-auto no-scrollbar px-6 safe-top pb-10 page-enter-anim select-none"
    >
      <Header title="作品详情" onBackPress={backFromScreen} />

      {!p ? (
        <Empty text={loadingProjects ? "正在读取作品…" : "作品不存在"} />
      ) : (
        <div className="flex flex-col gap-3">
          <div className="bg-white rounded-3xl p-4 flex gap-4">
            <div className="w-20 h-28 rounded-2xl bg-black/5 overflow-hidden shrink-0">
              {previewSrc(p) ? (
                <img src={previewSrc(p)} alt="" className="w-full h-full object-cover" />
              ) : (
                <div className="w-full h-full flex items-center justify-center text-black/20">
                  <FilmStrip className="w-6 h-6" />
                </div>
              )}
            </div>
            <div className="flex-1 min-w-0 flex flex-col justify-between py-0.5">
              <div className="min-w-0">
                <h2 className="text-[19px] font-bold text-black leading-tight truncate">
                  {p.title || "视频表盘"}
                </h2>
                <p className="text-[12px] font-medium text-black/40 mt-1 truncate">
                  {[p.template_name, p.style_name].filter(Boolean).join("，")}
                </p>
              </div>
              <p className="text-[12px] font-medium text-black/30 truncate">
                {p.mode === "once" ? "播放一次" : "循环播放"}，{fmtDate(p.created_at)}
              </p>
            </div>
          </div>

          <div className="bg-white rounded-2xl p-4 flex flex-col gap-3">
            <div className="flex items-center justify-between">
              <h3 className="text-[16px] font-bold text-black">本地占用</h3>
              <span className="text-[12px] font-bold text-black/40">{fmtSize(total)}</span>
            </div>
            {[
              { key: "video", label: "视频", size: p.video_size, has: p.has_video },
              { key: "frames", label: "序列帧包", size: p.frames_size, has: p.has_frames },
              { key: "bin", label: "表盘文件", size: p.bin_size, has: p.has_bin },
            ].map((it) => (
              <div key={it.key} className="flex items-center justify-between h-10">
                <div className="flex items-center gap-2">
                  <span className="text-[13px] font-semibold text-black/70">{it.label}</span>
                  <span className="text-[12px] font-medium text-black/35">{fmtSize(it.size)}</span>
                </div>
                {it.has ? (
                  <button
                    type="button"
                    onClick={() => {
                      if (it.key === "bin") setClearBinAsk(true);
                      else void clearKinds(p.id, [it.key]);
                    }}
                    className="h-8 px-3 rounded-full bg-black/[0.04] text-[12px] font-bold text-black/55 cursor-pointer active:scale-95 transition-all"
                  >
                    清理
                  </button>
                ) : (
                  <span className="text-[12px] font-medium text-black/20">无</span>
                )}
              </div>
            ))}
          </div>

          {p.job_status === "failed" && (
            <div className="bg-[#FF3B30]/8 rounded-2xl p-4">
              <p className="text-[12px] font-bold text-[#FF3B30]">上次制作失败</p>
              <p className="text-[12px] font-medium text-black/55 mt-1">
                {friendlyJobError(p.job_error)}
              </p>
            </div>
          )}

          {jobActive && (
            <button
              type="button"
              onClick={() => openJob(p)}
              className="h-11 rounded-2xl bg-[#D4973B]/10 text-[#D4973B] text-[14px] font-bold cursor-pointer flex items-center justify-center gap-2"
            >
              <ArrowClockwise className="w-4 h-4 animate-spin" />
              查看制作进度（{statusLabel(p.job_status)}）
            </button>
          )}

          <button
            type="button"
            disabled={!p.has_bin}
            onClick={() => void installBin(p.id)}
            className={`h-12 rounded-2xl text-[16px] font-bold active:scale-[0.99] transition-all flex items-center justify-center gap-2 ${
              p.has_bin
                ? "bg-[#4361EE] text-white cursor-pointer"
                : "bg-black/[0.05] text-black/25"
            }`}
          >
            <DeviceMobile className="w-5 h-5" />
            安装到手表
          </button>
          {!deviceConnected && (
            <p className="text-[11px] font-medium text-black/30 text-center -mt-1">
              当前未连接设备（{deviceName || "未知设备"}）
            </p>
          )}

          <div className="grid grid-cols-2 gap-3 mt-1">
            <button
              type="button"
              onClick={() => void editExisting(p)}
              className="h-11 rounded-2xl bg-white text-[14px] font-bold text-black/65 cursor-pointer active:scale-[0.99] transition-all"
            >
              继续制作
            </button>
            <button
              type="button"
              aria-disabled={jobActive || undefined}
              onClick={() => {
                if (jobActive) {
                  showToast("任务还在制作中，暂时不能删除", "error");
                  return;
                }
                setConfirmDelete(true);
              }}
              className={`h-11 rounded-2xl bg-white text-[14px] font-bold text-[#FF3B30] cursor-pointer active:scale-[0.99] transition-all ${
                jobActive ? "opacity-40" : ""
              }`}
            >
              删除项目
            </button>
          </div>
        </div>
      )}

      {confirmDelete && p && (
        <div className="fixed inset-0 z-[90] bg-black/35 flex items-end justify-center px-4 pb-6">
          <div className="w-full max-w-sm bg-white rounded-3xl p-5 text-center">
            <h3 className="text-[18px] font-bold text-black">删除这个作品？</h3>
            <p className="text-[13px] font-medium text-black/45 mt-2 leading-relaxed">
              会删除本机的视频、帧包和表盘文件；已经装在手表上的表盘不会自动卸载。
            </p>
            <div className="grid grid-cols-2 gap-3 mt-5">
              <button
                type="button"
                onClick={() => setConfirmDelete(false)}
                className="h-11 rounded-2xl bg-black/[0.05] text-[14px] font-bold text-black/65 cursor-pointer"
              >
                取消
              </button>
              <button
                type="button"
                onClick={() => void deleteProject(p.id)}
                className="h-11 rounded-2xl bg-[#FF3B30] text-white text-[14px] font-bold cursor-pointer"
              >
                删除
              </button>
            </div>
          </div>
        </div>
      )}

      {clearBinAsk && p && (
        <div className="fixed inset-0 z-[90] bg-black/35 flex items-end justify-center px-4 pb-6">
          <div className="w-full max-w-sm bg-white rounded-3xl p-5 text-center">
            <h3 className="text-[18px] font-bold text-black">清理表盘文件？</h3>
            <p className="text-[13px] font-medium text-black/45 mt-2 leading-relaxed">
              清理后这个作品将无法在 App 里继续编辑，需要重新生成才能恢复。已装在手表上的表盘不受影响。
            </p>
            <div className="grid grid-cols-2 gap-3 mt-5">
              <button
                type="button"
                onClick={() => setClearBinAsk(false)}
                className="h-11 rounded-2xl bg-black/[0.05] text-[14px] font-bold text-black/65 cursor-pointer"
              >
                取消
              </button>
              <button
                type="button"
                onClick={() => {
                  setClearBinAsk(false);
                  void clearKinds(p.id, ["bin"]);
                }}
                className="h-11 rounded-2xl bg-[#FF3B30] text-white text-[14px] font-bold cursor-pointer"
              >
                清理
              </button>
            </div>
          </div>
        </div>
      )}

      {busy && (
        <div className="fixed inset-0 z-[80] bg-black/30 flex items-center justify-center px-8">
          <div className="bg-white rounded-2xl px-5 py-4 flex items-center gap-3 shadow-xl">
            <ArrowClockwise className="w-5 h-5 text-[#4361EE] animate-spin" />
            <span className="text-[14px] font-bold text-black">{busy}</span>
          </div>
        </div>
      )}
    </div>
  );
}
