import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import type { UIEvent } from "react";

/**
 * 页面滚动位置记忆。
 *
 * 为什么需要：页面/Tab 之间是条件渲染切换的，离开时滚动容器整个卸载，回来重新挂载
 * 浏览器只会给 scrollTop = 0 —— 用户下滑很远点进详情，返回就被弹回最顶端。
 *
 * 做法：key（一般是路由）→ 离开时的 scrollTop 记在模块级 Map 里（写 ref/Map 不触发 React
 * 重渲染）；容器挂载后的第一次布局里就把位置写回去，早于浏览器绘制，看不到闪回顶部。
 *
 * 方向约定（和页面栈保持一致）：
 *   · 前进进入新页面：调用方先 resetScroll(key)，新页面从顶部开始；
 *   · 从子页面返回：**不要** reset，本 hook 会把位置恢复回来；
 *   · 异步列表：传 ready / itemCount，数据到达后会再补一次恢复；恢复成功后清掉
 *     待恢复标记，后续用户手动滚动绝不会被旧位置覆盖。
 */

/** key → 离开时的 scrollTop。模块级：页面 DOM 卸载后位置还在 */
const scrollPositions = new Map<string, number>();

/** 全新进入某页（前进一层）时清零；从子页面返回时不要调用（保留原位） */
export function resetScroll(key: string): void {
  scrollPositions.delete(key);
}

/** 清掉一组页面的记忆（前缀匹配），例如视频表盘整页的 `video:` 前缀 */
export function resetScrollGroup(prefix: string): void {
  for (const key of scrollPositions.keys()) {
    if (key.startsWith(prefix)) scrollPositions.delete(key);
  }
}

/** 读某页记住的位置（顶栏大小标题这类需要跟着还原的地方用） */
export function readScroll(key: string): number {
  return scrollPositions.get(key) ?? 0;
}

type ScrollMemoryOptions = {
  /** 容器当前是否挂在页面上：条件渲染的 Tab / 二级页靠它触发恢复 */
  active?: boolean;
  /** 异步列表是否已拿到数据：false → true 时再补一次恢复 */
  ready?: boolean;
  /** 列表条目数量：变化时再补一次恢复（首帧内容不够高、位置被夹住的情况） */
  itemCount?: number;
};

/**
 * 给滚动容器绑定位置记忆：
 *
 *   const page = useScrollMemory("main/manage", { active });
 *   <div ref={page.ref} onScroll={page.onScroll} ... />
 */
export function useScrollMemory(key: string, options: ScrollMemoryOptions = {}) {
  const { active = true, ready = true, itemCount } = options;
  const ref = useRef<HTMLDivElement | null>(null);
  /** 还没恢复成功的目标位置（null = 没有待恢复） */
  const pendingRef = useRef<number | null>(null);
  /** 最近一次程序写入的 scrollTop：用来分辨随后的 scroll 事件是不是自己造成的 */
  const appliedRef = useRef<number | null>(null);
  const frameRef = useRef<number | null>(null);
  const attemptsRef = useRef(0);
  /** ready 的最新值（补恢复的帧循环里读，不参与 hook 依赖） */
  const readyRef = useRef(ready);
  readyRef.current = ready;

  const cancelFrame = useCallback(() => {
    if (frameRef.current !== null) {
      cancelAnimationFrame(frameRef.current);
      frameRef.current = null;
    }
  }, []);

  const applyPending = useCallback(() => {
    const el = ref.current;
    const target = pendingRef.current;
    if (!el || target === null) return;
    el.scrollTop = target;
    appliedRef.current = el.scrollTop;
    if (el.scrollTop + 1 >= target) {
      // 到位了：清掉待恢复标记，之后用户怎么滚都不会被旧位置覆盖
      pendingRef.current = null;
      cancelFrame();
      return;
    }
    // 内容还没铺满，scrollTop 被浏览器夹住：数据没来时下一帧再试；
    // 数据已经到了仍不够高就停下，等 itemCount 变化时再补
    attemptsRef.current += 1;
    if (!readyRef.current && attemptsRef.current <= 20) {
      frameRef.current = requestAnimationFrame(applyPending);
    } else {
      cancelFrame();
    }
  }, [cancelFrame]);

  // 容器挂载 / 页面重新激活：把离开时的位置写回去（在绘制前执行，避免先闪一下顶部）
  useLayoutEffect(() => {
    if (!active) {
      pendingRef.current = null;
      cancelFrame();
      return;
    }
    if (!ref.current) return;
    const saved = scrollPositions.get(key) ?? 0;
    pendingRef.current = saved > 0 ? saved : null;
    appliedRef.current = null;
    attemptsRef.current = 0;
    if (pendingRef.current !== null) applyPending();
  }, [key, active, applyPending, cancelFrame]);

  // 异步数据到达 / 列表长度变化：还没恢复成功就再补一次
  useLayoutEffect(() => {
    if (!active || pendingRef.current === null) return;
    attemptsRef.current = 0;
    applyPending();
  }, [key, active, ready, itemCount, applyPending]);

  // 用户一上手就放弃补恢复：别让「数据晚到」把用户刚滚到的位置拽回去
  useEffect(() => {
    const el = ref.current;
    if (!active || !el) return;
    const stop = () => {
      pendingRef.current = null;
      cancelFrame();
    };
    el.addEventListener("touchstart", stop, { passive: true });
    el.addEventListener("wheel", stop, { passive: true });
    el.addEventListener("pointerdown", stop, { passive: true });
    return () => {
      el.removeEventListener("touchstart", stop);
      el.removeEventListener("wheel", stop);
      el.removeEventListener("pointerdown", stop);
    };
  }, [key, active, cancelFrame]);

  // 卸载时取消还没跑完的补恢复
  useEffect(() => () => cancelFrame(), [cancelFrame]);

  const onScroll = useCallback(
    (e: UIEvent<HTMLDivElement>) => {
      const el = e.currentTarget;
      const top = el.scrollTop;
      const applied = appliedRef.current;
      if (pendingRef.current !== null && applied !== null) {
        if (Math.abs(top - applied) <= 1) {
          // 这条 scroll 是我们刚写进去的位置触发的
          appliedRef.current = null;
        } else {
          // 用户（或内容变化）把列表挪走了：以当前为准，放弃补恢复
          pendingRef.current = null;
          cancelFrame();
        }
      }
      scrollPositions.set(key, top);
    },
    [key, cancelFrame]
  );

  return { ref, onScroll };
}
