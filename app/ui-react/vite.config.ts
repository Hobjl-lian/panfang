import { defineConfig, type HtmlTagDescriptor, type Plugin } from 'vite'
import react from '@vitejs/plugin-react'
import tailwindcss from '@tailwindcss/vite'
import path from 'node:path'

import siteConfiguration from './.figma/make/site.json'


/**
 * 旧内核产物开关：**默认开启**（`PF_LEGACY_WEBVIEW=0` 可临时关掉，只用于排查构建问题）。
 *
 * 为什么需要：国内还有一批机器的系统 WebView 停在 Chromium 8x（Android 7~11 的老 ROM，
 * 或用户/商店从不更新 WebView）。默认产物在那一档上不是「丑一点」而是直接打不开：
 * ① JS 里有 19 处逻辑赋值 `??= / ||= / &&=`（Chrome 85 语法）→ 模块级 SyntaxError，一行都不执行；
 * ② Tailwind v4 把全部样式包在 `@layer` 里（Chrome 99+）→ 旧内核把整块丢掉，页面变裸 HTML；
 * ③ `px-*` / `py-*` 走逻辑属性简写（`padding-inline` / `padding-block`，Chrome 87+）→ 横向内边距归零。
 * 开这个开关就把产物整体降到 Chrome 83：esbuild/oxc 展开逻辑赋值，CSS 拆 `@layer` 壳、
 * 展开逻辑属性与 `inset`、给 `100dvh` 补 `100vh`。
 *
 * 2026-11-05 实测（Chromium 83 + 现代内核对照）：现代内核**零差异**；83 上从「整页黑屏」
 * 变完整界面，剩余差异只有 flex `gap`（Chrome 84）导致的 4~12px 间距收紧。
 */
const legacyWebview = process.env.PF_LEGACY_WEBVIEW !== '0'

// Vite config — https://vitejs.dev/config/
export default defineConfig(({ mode }) => {
  // .figma/make/deploy-preview passes `--mode development` for cached-preview builds.
  const emitSourcemaps = mode === 'development'

  return {
    base: process.env.FIGMA_PUBLIC_URL ? `${process.env.FIGMA_PUBLIC_URL}/` : '/',
    build: {
      sourcemap: emitSourcemaps ? 'inline' : false,
      minify: !emitSourcemaps,
      target: legacyWebview ? 'chrome83' : undefined,
      cssTarget: legacyWebview ? 'chrome83' : undefined,
    },
    // 依赖包（React 等）的语法也要跟着降级，只设 build.target 管不到它们
    esbuild: legacyWebview ? { target: 'chrome83' } : undefined,
    plugins: [
react(),
      ...(legacyWebview ? [legacyWebviewCss()] : []),
      tailwindcss(),
      figmaSiteConfiguration(siteConfiguration),
      figmaErrorOverlayReplay(),
      figmaReactRefreshBoundaryFallback(),
      figmaMakeKitPlugin({ storiesGlob: '/src/**/*.stories.{ts,tsx,js,jsx}' }),
    ],
    resolve: {
      alias: {
        '@': path.resolve(__dirname, './src'),
      },
    },
    server: {
      host: process.env.FIGMA_DEV_SERVER_HOST || '0.0.0.0',
      port: parseInt(process.env.PORT || '8443'),
      strictPort: true,
      watch: {
        ignored: [
          '**/.figma/**',
],
      },
    },
    preview: {
      host: process.env.FIGMA_DEV_SERVER_HOST || '0.0.0.0',
      port: parseInt(process.env.PORT || '8443'),
    },
  }
})

type FigmaSiteConfiguration = {
  title?: string
  description?: string
  language?: string
  robots?: {
    index?: boolean
  }
  icons?: {
    icon?: string
  }
  openGraph?: {
    image?: string
  }
  analytics?: {
    googleAnalyticsId?: string
  }
  customScripts?: {
    headStart?: string
    headEnd?: string
    bodyStart?: string
    bodyEnd?: string
  }
  accessibility?: {
    addBypassLinks?: boolean
  }
}

/** Applies /.figma/make/site.json to the generated document shell. */
function figmaSiteConfiguration(config: FigmaSiteConfiguration): Plugin {
  function sanitizeHtmlValue(value: string | undefined): string {
    return value?.replace(/[^a-zA-Z0-9_-]/g, '') || ''
  }
  function escapeHtmlText(value: string): string {
    return value.replace(/&/g, '&amp;').replace(/</g, '&lt;').replace(/>/g, '&gt;')
  }
  function replaceHtmlCommentSlot(html: string, slotName: string, content: string): string {
    return html.replace(`<!-- ${slotName} -->`, content)
  }

  const title = config.title ?? "Figma Make App"
  const description = config.description ?? ''
  const favicon = config.icons?.icon ?? ''
  const socialImage = config.openGraph?.image ?? ''
  const language = sanitizeHtmlValue(config.language) || 'en'
  const googleAnalyticsId = sanitizeHtmlValue(config.analytics?.googleAnalyticsId)
  const headStart = config.customScripts?.headStart ?? ''
  const headEnd = config.customScripts?.headEnd ?? ''
  const bodyStart = config.customScripts?.bodyStart ?? ''
  const bodyEnd = config.customScripts?.bodyEnd ?? ''
  const robotsTxt = config.robots?.index === false ? 'User-agent: *\nDisallow: /\n' : ''

  return {
    name: 'figma-site-configuration',
    configureServer(server) {
      server.middlewares.use((req, res, next) => {
        if (!robotsTxt || req.url?.split('?')[0] !== '/robots.txt') return next()

        res.setHeader('Content-Type', 'text/plain; charset=utf-8')
        res.end(robotsTxt)
      })
    },
    generateBundle() {
      if (!robotsTxt) return

      this.emitFile({
        type: 'asset',
        fileName: 'robots.txt',
        source: robotsTxt,
      })
    },
    transformIndexHtml: {
      order: 'pre',
      handler(html) {
        let result = html
        result = replaceHtmlCommentSlot(result, 'figma:lang', language)
        result = replaceHtmlCommentSlot(result, 'figma:title', escapeHtmlText(title))
        result = replaceHtmlCommentSlot(result, 'figma:head-start', headStart)
        result = replaceHtmlCommentSlot(result, 'figma:head-end', headEnd)
        result = replaceHtmlCommentSlot(result, 'figma:body-start', bodyStart)
        result = replaceHtmlCommentSlot(result, 'figma:body-end', bodyEnd)

        const tags: HtmlTagDescriptor[] = []
        if (description) {
          tags.push({ tag: 'meta', attrs: { name: 'description', content: description }, injectTo: 'head' })
        }
        if (config.robots?.index === false) {
          tags.push({ tag: 'meta', attrs: { name: 'robots', content: 'noindex, nofollow' }, injectTo: 'head' })
        }
        if (favicon) {
          tags.push({ tag: 'link', attrs: { rel: 'icon', href: favicon }, injectTo: 'head' })
        }
        if (title) {
          tags.push({ tag: 'meta', attrs: { property: 'og:title', content: title }, injectTo: 'head' })
        }
        if (description) {
          tags.push({ tag: 'meta', attrs: { property: 'og:description', content: description }, injectTo: 'head' })
        }
        if (socialImage) {
          tags.push(
            { tag: 'meta', attrs: { property: 'og:image', content: socialImage }, injectTo: 'head' },
            { tag: 'meta', attrs: { name: 'twitter:card', content: 'summary_large_image' }, injectTo: 'head' },
            { tag: 'meta', attrs: { name: 'twitter:image', content: socialImage }, injectTo: 'head' },
          )
        }

        if (googleAnalyticsId) {
          tags.push(
            {
              tag: 'script',
              attrs: {
                async: true,
                src: `https://www.googletagmanager.com/gtag/js?id=${googleAnalyticsId}`,
              },
              injectTo: 'head',
            },
            {
              tag: 'script',
              children: `
  window.dataLayer = window.dataLayer || [];
  function gtag(){dataLayer.push(arguments);}
  gtag('js', new Date());
  gtag('config', ${JSON.stringify(googleAnalyticsId)});
`,
              injectTo: 'head',
            },
          )
        }

        if (config.accessibility?.addBypassLinks) {
          tags.push(
            {
              tag: 'style',
              children: `
  .figma-bypass-link {
    position: fixed;
    top: 8px;
    left: 8px;
    z-index: 2147483647;
    transform: translateY(-150%);
    border-radius: 6px;
    background: #111827;
    color: #fff;
    padding: 8px 12px;
    font: 600 14px/1.2 system-ui, sans-serif;
    text-decoration: none;
  }
  .figma-bypass-link:focus {
    transform: translateY(0);
  }
`,
              injectTo: 'head',
            },
            {
              tag: 'a',
              attrs: { class: 'figma-bypass-link', href: '#root' },
              children: 'Skip to content',
              injectTo: 'body-prepend',
            },
          )
        }

        return {
          html: result,
          tags,
        }
      },
    },
  }
}

/**
 * Replay the most recent build error to clients that connect after
 * it was first broadcast. Vite buffers an error payload only while
 * no clients are connected and clears the buffer on the first
 * reconnect (see `bufferedMessage` in `createWebSocketServer`), so
 * if the preview iframe reloads after Vite already delivered an
 * error to a live socket, the new socket misses the payload and
 * the overlay stays hidden even though the build is still broken.
 * We intercept `ws.send` to remember the latest error and replay
 * it on every new connection; the cache clears on a successful
 * `update` or `full-reload` so a stale overlay can't survive a
 * fixed build.
 */
function figmaErrorOverlayReplay(): Plugin {
  return {
    name: 'figma-error-overlay-replay',
    apply: 'serve',
    configureServer(server) {
      let lastError: object | null = null

      const origSend = server.ws.send.bind(server.ws) as (...args: any[]) => void
      server.ws.send = ((...args: any[]) => {
        const payload = args[0]
        if (payload && typeof payload === 'object' && !Array.isArray(payload)) {
          const type = (payload as { type?: string }).type
          if (type === 'error') {
            lastError = payload as object
          } else if (type === 'update' || type === 'full-reload') {
            lastError = null
          }
        }
        return origSend(...args)
      }) as typeof server.ws.send

      server.ws.on('connection', (socket) => {
        if (lastError !== null) {
          socket.send(JSON.stringify(lastError))
        }
      })
    },
  }
}

/**
 * Reload when a module that previously defined a React Refresh boundary stops
 * defining one. This happens when an agent moves a component into a new file
 * and replaces the old module with a re-export:
 *
 *   export { default } from './app/App'
 *
 * Vite otherwise accepts the update using the previous module's HMR boundary,
 * but the re-export-only transform no longer registers a replacement for the
 * mounted component family. React reports a successful refresh while leaving
 * the old tree mounted until the page is reloaded.
 */
function figmaReactRefreshBoundaryFallback(): Plugin {
  const hadRefreshBoundary = new Map<string, boolean>()
  let sendFullReload: (() => void) | null = null

  return {
    name: 'figma-react-refresh-boundary-fallback',
    apply: 'serve',
    enforce: 'post',
    configureServer(server) {
      sendFullReload = () => server.ws.send({ type: 'full-reload', path: '*' })
    },
    transform(code, id) {
      if (!/\.[jt]sx?(?:\?|$)/.test(id) || id.includes('/node_modules/')) return null

      const moduleId = id.split('?')[0] ?? id
      const hasRefreshBoundary = code.includes('registerExportsForReactRefresh')
      const previousHadRefreshBoundary = hadRefreshBoundary.get(moduleId)
      hadRefreshBoundary.set(moduleId, hasRefreshBoundary)

      if (previousHadRefreshBoundary && !hasRefreshBoundary) {
        queueMicrotask(() => sendFullReload?.())
      }

      return null
    },
  }
}

/**
 * Serves a blank render-target page at /.figma/make/kit.html that
 * the Figma preview script drives directly. The page exposes a
 * registry of every file matching `storiesGlob` on
 * window.__FIGMA__.stories so the design surface can dynamically
 * import + mount each entry into its own grid view.
 *
 * Dev-only: `apply: 'serve'` gates the plugin to `vite dev`. Prod
 * builds (`vite build`) skip it entirely so the route doesn't leak
 * into shipped bundles.
 */
function figmaMakeKitPlugin(options: { storiesGlob: string | string[] }): Plugin {
  const storiesGlob = Array.isArray(options.storiesGlob) ? options.storiesGlob : [options.storiesGlob]
  const ROUTE = '/.figma/make/kit.html'
  const VIRTUAL_ID = 'virtual:figma-stories'
  const RESOLVED_ID = '\0' + VIRTUAL_ID
  const STORIES_MODULE = `export const stories = import.meta.glob(${JSON.stringify(storiesGlob)})`
  const HTML_BOOTSTRAP = `<!doctype html>
<html lang="en">
<head>
<meta charset="UTF-8" />
<meta name="viewport" content="width=device-width, initial-scale=1.0" />
</head>
<body>
<div id="figma-make-kit-root"></div>
<script type="module">
  import { stories } from 'virtual:figma-stories'
  window.__FIGMA__ = Object.assign(window.__FIGMA__ ?? {}, { stories })
  window.dispatchEvent(new CustomEvent('figma.ready'))
</script>
</body>
</html>`

  return {
    name: 'figma-make-kit',
    apply: 'serve',
    resolveId(id) {
      if (id === VIRTUAL_ID) return RESOLVED_ID
      return null
    },
    load(id) {
      if (id !== RESOLVED_ID) return null
      return STORIES_MODULE
    },
    configureServer(server) {
      server.middlewares.use(async (req, res, next) => {
        const url = req.url || ''
        if (url.split('?')[0] !== ROUTE) return next()

        try {
          res.setHeader('Content-Type', 'text/html')
          res.end(await server.transformIndexHtml(url, HTML_BOOTSTRAP))
        } catch (err) {
          next(err as Error)
        }
      })
    },
  }
}

/**
 * 把 Tailwind v4 产物降到 Chromium 83 能吃的写法（只在 PF_LEGACY_WEBVIEW=1 时挂上）。
 *
 * 四道处理，都是「旧内核认不出 → 整条声明被丢 → 界面塌」的硬伤：
 * ① `@layer` 外层包壳（Chrome 99+）：不展开的话 theme/base/utilities 全是空的，页面没有样式；
 * ② 逻辑属性 `padding-block` / `padding-inline` / `margin-inline` / `inset` 等（简写是 Chrome 87+）：
 *    展开成 top/right/bottom/left。**这条影响最大** —— 旧内核上所有横向内边距和边距会归零，
 *    整屏元素会一路顶到屏幕左边（实测：83 上 46/58 个元素盒模型错位，根因就是它）；
 * ③ `height:100dvh`（Chrome 108+）：补一条 `100vh` 在前，拿不到 dvh 时退回视口高度；
 * ④ `inset:0`（Chrome 87+）：展开成 top/right/bottom/left。
 *
 * 已知仍有差距、但判断为可接受（都不影响「能不能用」）：flex `gap`（Chrome 84，间距归零，
 * 只能改源码换 `space-*` 才能补）、`:where()` 里的 group-hover（Chrome 88，一处悬停位移）、
 * `color-mix()`（Chrome 111，Tailwind 自带等价的 hex 兜底，实测只是 40%/45% 这类透明度微差）。
 */
function legacyWebviewCss(): Plugin {
  return {
    name: 'legacy-webview-css',
    apply: 'build',
    generateBundle(_options, bundle) {
      for (const file of Object.values(bundle)) {
        if (file.type !== 'asset' || !file.fileName.endsWith('.css')) continue
        const raw = typeof file.source === 'string' ? file.source : new TextDecoder().decode(file.source)
        file.source = downlevelCssForLegacyWebview(raw)
      }
    },
  }
}

/** 降级总入口（导出给测试脚本用） */
export function downlevelCssForLegacyWebview(css: string): string {
  return expandLogicalProps(expandInset(unwrapCascadeLayers(addDvhFallback(css))))
}

/** `@layer a,b;` 整条删掉（只是声明层顺序），`@layer x{…}` 拆壳留内容 */
function unwrapCascadeLayers(css: string): string {
  let out = css.replace(/@layer\s+[^{};]+;/g, '')
  for (let pass = 0; pass < 8; pass++) {
    const at = out.indexOf('@layer')
    if (at < 0) break
    const open = out.indexOf('{', at)
    if (open < 0) break
    const close = matchBrace(out, open)
    if (close < 0) break
    out = out.slice(0, at) + out.slice(open + 1, close) + out.slice(close + 1)
  }
  return out
}

/** 从 open（指向 `{`）开始找配对的 `}`，跳过引号里的括号 */
function matchBrace(css: string, open: number): number {
  let depth = 0
  let quote: string | null = null
  for (let i = open; i < css.length; i++) {
    const ch = css[i]
    if (quote) {
      if (ch === '\\') i++
      else if (ch === quote) quote = null
      continue
    }
    if (ch === '"' || ch === "'") quote = ch
    else if (ch === '{') depth++
    else if (ch === '}') {
      depth--
      if (depth === 0) return i
    }
  }
  return -1
}

/** `height:100dvh` → `height:100vh;height:100dvh`（同一属性写两遍，旧内核只认前一条） */
function addDvhFallback(css: string): string {
  return css.replace(/([{;])([-\w]+)\s*:\s*([^;{}]*dvh[^;{}]*)/g, (_all, lead, prop, value) => {
    const fallback = String(value).replace(/(\d|\.)d(v[hw])/g, '$1$2')
    return `${lead}${prop}:${fallback};${prop}:${value}`
  })
}

/**
 * 按顶层空格切分值：`calc(calc(var(--spacing) * 2) * var(--x))` 里嵌套括号内的空格不切。
 * （早先版本用正则 `\s+(?![^()]*\))` 判断，嵌套一层就会在括号中间切断，
 *  生成不配对的 `calc(` —— 浏览器从那里起把后面整段样式丢掉，页面看起来像没加载 CSS。）
 */
function splitTopLevel(value: string): string[] {
  const out: string[] = []
  let depth = 0
  let cur = ''
  for (const ch of value) {
    if (ch === '(') depth++
    else if (ch === ')') depth--
    if (depth === 0 && /\s/.test(ch)) {
      if (cur) out.push(cur)
      cur = ''
      continue
    }
    cur += ch
  }
  if (cur) out.push(cur)
  return out
}

/** `inset:0` → `top:0;right:0;bottom:0;left:0`（1~4 值按 CSS 简写规则展开） */
function expandInset(css: string): string {
  return css.replace(/([{;])inset\s*:\s*([^;{}]+)/g, (_all, lead, raw) => {
    const parts = splitTopLevel(String(raw).trim())
    if (!parts.length || parts.length > 4) return `${lead}inset:${raw}`
    const [t, r = t, b = t, l = r] = parts
    return `${lead}top:${t};right:${r};bottom:${b};left:${l}`
  })
}

/**
 * 逻辑属性 → 物理属性。
 *
 * `padding-block` / `padding-inline` / `margin-inline` 这类**简写**是 Chrome 87 才有的
 * （只有 `-start` / `-end` 长写是 69）。Tailwind v4 的 `px-*` / `py-*` / `mx-*` 全走简写，
 * 所以不展开的话旧内核上整批横向内边距直接归零，元素一路顶到屏幕左边。
 * 本 App 只有 LTR，不存在 RTL 语义问题，按 left/right、top/bottom 一一对应即可。
 */
function expandLogicalProps(css: string): string {
  // 先处理带属性的 border 逻辑写法：border-inline-start-width / border-block-style ...
  let out = css.replace(
    /([{;])border-(block|inline)(?:-(start|end))?-(width|style|color)\s*:\s*([^;{}]+)/g,
    (_all, lead, axis: string, edge: string | undefined, part: string, value: string) => {
      const sides = edge
        ? [axis === 'block' ? (edge === 'start' ? 'top' : 'bottom') : edge === 'start' ? 'left' : 'right']
        : axis === 'block'
          ? ['top', 'bottom']
          : ['left', 'right']
      const decls = sides.map((side) => `border-${side}-${part}:${value}`).join(';')
      return `${lead}${decls};border-${axis}${edge ? `-${edge}` : ''}-${part}:${value}`
    },
  )

  // 再处理 margin / padding / inset
  out = out.replace(
    /([{;])(margin|padding|inset)-(block|inline)(?:-(start|end))?\s*:\s*([^;{}]+)/g,
    (_all, lead, prop: string, axis: string, edge: string | undefined, raw: string) => {
      const values = splitTopLevel(String(raw).trim())
      const pairs: Array<[string, string]> = []
      if (edge) {
        const side =
          axis === 'block' ? (edge === 'start' ? 'top' : 'bottom') : edge === 'start' ? 'left' : 'right'
        pairs.push([`${prop}-${side}`, values[0]])
      } else if (axis === 'block') {
        pairs.push([`${prop}-top`, values[0]], [`${prop}-bottom`, values[1] ?? values[0]])
      } else {
        pairs.push([`${prop}-left`, values[0]], [`${prop}-right`, values[1] ?? values[0]])
      }
      const decls = pairs.map(([name, v]) => `${name}:${v}`).join(';')
      return `${lead}${decls};${prop}-${axis}${edge ? `-${edge}` : ''}:${raw}`
    },
  )

  return out
}
