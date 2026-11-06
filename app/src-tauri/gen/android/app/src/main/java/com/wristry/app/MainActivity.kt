package com.wristry.app

import android.content.Intent
import android.graphics.Color
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.provider.Settings
import android.webkit.WebSettings
import android.webkit.WebView
import android.webkit.WebViewClient
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import moe.astralsight.astrobox.plugin.inappbrowser.InstallBackLock
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean

class MainActivity : TauriActivity() {
  /** 本进程读到的系统 WebView UA（onCreate 早期取一次，Rust 侧启动时也要用）。 */
  private var webViewUa: String? = null

  override fun onCreate(savedInstanceState: Bundle?) {
    // App 以浅色页为主：系统栏透明（WebView 顶栏条上色）、图标强制深色，
    // 不随系统深色模式翻转（深色模式下白图标会消失在浅灰顶栏上）。
    // 深色页（裁剪页）由 inappbrowser 插件的 setSystemBars 命令切回白色图标。
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
    )
    // WebView 版本探测必须赶在 super.onCreate 之前落盘：Rust 的 setup 在 super 里跑，
    // 过了那一刻它再读就只能是上一次启动的值（见 diag::report_webview_probe）。
    webViewUa = runCatching { WebSettings.getDefaultUserAgent(this) }.getOrNull()
    webViewUa?.let { writeWebViewProbe(it) }
    super.onCreate(savedInstanceState)
    warnIfWebViewTooOld(webViewUa ?: runCatching { WebSettings.getDefaultUserAgent(this) }.getOrNull())
  }

  /**
   * 把当前 WebView 的版本写进应用数据目录，交给 Rust 侧启动时上报（本机同一个版本只报一次）。
   *
   * 为什么走文件：这一档 WebView 连前端 JS 都解析不了，命令桥没有任何调用方；
   * 而 Kotlin → Rust 只有这条路是现成的（apkinstaller 插件写 `paid/android_id` 就是同一招）。
   * 路径必须与 Tauri 的 app_data_dir 对齐 —— 安卓上它就是 activity.dataDir（不拼包名）。
   */
  private fun writeWebViewProbe(ua: String) {
    try {
      val major = chromeMajor(ua) ?: -1
      val provider = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        runCatching {
          val pkg = WebView.getCurrentWebViewPackage()
          listOfNotNull(pkg?.packageName, pkg?.versionName).joinToString(" ")
        }.getOrNull().orEmpty()
      } else {
        ""
      }
      File(dataDir, WEBVIEW_PROBE_FILE).writeText(
        "major=$major\n" +
          "provider=$provider\n" +
          // 机型由这里带上：Rust 侧登记手机型号要等 connkeep 的异步探测，
          // 这条上报赶在它前面，不带就只剩一串 UA（光看 UA 型号会被 ROM 改写）。
          "model=${Build.MODEL} (Android ${Build.VERSION.RELEASE})\n" +
          "ua=$ua\n",
      )
    } catch (_: Exception) {
      // 写不进就算了：这是纯统计，不能影响启动
    }
  }

  /**
   * 系统 WebView 过旧的原生兜底：低于 CHROME_MIN_MAJOR_WITH_WORKING_JS 的 Chromium
   * 连前端 JS 都解析不了，页面是纯白，JS 侧的门禁页根本没机会显示。
   *
   * 2026-11-05 起产物整体降到 Chrome 83 打包（`PF_LEGACY_WEBVIEW`，默认开启），
   * 逻辑赋值已在构建期展开，所以门槛从 88 降到 83、探针改探**成品里仍然存在**的
   * `?.` / `??`（Chrome 80 语法）：能解析说明 JS 跑得起来，就交给前端门禁页去提示。
   *
   * UA 里的版本号在部分国产 ROM 上会被冻住（引擎其实不旧），所以这里不直接信 UA：
   * 先问引擎能不能解析，能解析就放行 —— 别把一个其实能用的用户关在门外。
   */
  private fun warnIfWebViewTooOld(ua: String?) {
    val major = ua?.let(::chromeMajor) ?: return // 取不到版本就放行：拿不准的机型宁可不拦
    if (major >= CHROME_MIN_MAJOR_WITH_WORKING_JS) return
    val probe = runCatching { WebView(this) }.getOrNull()
    if (probe == null) {
      showTooOldDialog(major)
      return
    }
    // 探测是异步的，而极旧内核上回调可能永远不来：留一个超时兜底，
    // 超时按「跑不起来」处理（与改造前一致，不算回归）。
    val answered = AtomicBoolean(false)
    val handler = Handler(mainLooper)
    val judge: (String?) -> Unit = { result ->
      if (answered.compareAndSet(false, true)) {
        handler.removeCallbacksAndMessages(null)
        runCatching { probe.destroy() }
        if (result?.trim('"', ' ') != "1") showTooOldDialog(major)
      }
    }
    handler.postDelayed({ judge(null) }, PROBE_TIMEOUT_MS)
    // ⚠️ 两个真机坑（2026-10-23 在这台 iQOO 上踩出来的，别改回去）：
    //   1) **WebView 默认是关着 JavaScript 的**：不打开的话 evaluateJavascript 根本不会执行，
    //      回调永远不来 —— 超时兜底就把所有人都判成「跑不起来」（Chrome 148 也照拦）。
    //   2) 空 WebView 直接问也不行，要先 loadDataWithBaseURL，等 onPageFinished 之后才能问。
    probe.settings.javaScriptEnabled = true
    probe.webViewClient = object : WebViewClient() {
      override fun onPageFinished(view: WebView?, url: String?) {
        view?.evaluateJavascript(JS_PARSE_PROBE) { result -> judge(result) }
      }
    }
    probe.loadDataWithBaseURL(
      "about:blank",
      "<!DOCTYPE html><html><body></body></html>",
      "text/html",
      "utf-8",
      null,
    )
  }

  private fun showTooOldDialog(major: Int) {
    MaterialAlertDialogBuilder(this)
      .setTitle("系统组件版本过旧")
      .setMessage(
        "这台手机的系统 WebView 版本较低（Chrome $major），盘坊的界面无法显示。" +
          "请先更新手机系统；如果系统已经是最新的，把这张截图发给卖家。",
      )
      // 「去更新」原来是指向 WebView 组件的应用信息页 —— 但国产应用商店里根本搜不到
      // 「Android System WebView」（2026-10-23 真机实测：vivo 商店没有这个包），
      // 那条路对买家是死胡同。系统更新才是这类组件真正跟着走的地方。
      .setPositiveButton("系统更新") { _, _ ->
        openSystemUpdate()
        finish()
      }
      .setNegativeButton("退出") { _, _ -> finish() }
      .setCancelable(false)
      .show()
  }

  /**
   * 打开系统更新。各家落点不一样，按顺序试：
   * 「系统更新」页（originOS 落到 Google 系统更新，带「检查更新」）→ 设置首页
   * （国产 ROM 的「系统升级」多是设置里的一级项）→ 关于手机 → WebView 组件的应用信息页。
   */
  private fun openSystemUpdate() {
    val candidates = listOf(
      Intent("android.settings.SYSTEM_UPDATE_SETTINGS"),
      Intent(Settings.ACTION_SETTINGS),
      Intent(Settings.ACTION_DEVICE_INFO_SETTINGS),
    )
    for (intent in candidates) {
      try {
        startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK))
        return
      } catch (_: Exception) {
        // 这家 ROM 没有这个页面，试下一个
      }
    }
    // 一路都打不开就退回 WebView 组件的应用信息页，至少能看到版本号
    openWebViewAppInfo()
  }

  /**
   * 打开**当前 WebView 组件**的应用详情页（不是本 App 的）：兜底用，至少能看到它现在装的是哪个版本。
   * 别指望这里能更新它 —— 国产商店里没有「Android System WebView」这个包。
   */
  private fun openWebViewAppInfo() {
    val pkg = runCatching {
      if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
        WebView.getCurrentWebViewPackage()?.packageName
      } else {
        null
      }
    }.getOrNull() ?: packageName
    try {
      startActivity(
        Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS).apply {
          data = Uri.parse("package:$pkg")
          addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        },
      )
    } catch (_: Exception) {
      // 系统没有这个页面也不影响退出
    }
  }

  private fun chromeMajor(ua: String): Int? =
    Regex("Chrome/(\\d+)").find(ua)?.groupValues?.get(1)?.toIntOrNull()

  /**
   * 安装表盘期间吞掉返回（含侧滑手势）：装表盘不能被返回打断，也退不出 App；
   * 每次都通知前端弹一条「安装中，请勿退出」（这条路径不会走到插件回调，所以各自通知一次）。
   * 插件侧还有一条 OnBackPressedCallback 先拦；这里是 Activity 级兜底 ——
   * Tauri 在「WebView 无历史可退」时会直接从 Activity 这一层退出，那条路不经过插件回调。
   * 只拦返回，不拦 Home（切后台不打断传输）。
   */
  override fun onBackPressed() {
    if (InstallBackLock.locked) {
      InstallBackLock.notifyBackBlocked()
      return
    }
    super.onBackPressed()
  }

  private companion object {
    /**
     * 低于这个 Chromium 主版本，前端产物（构建期降到 chrome83）可能连解析都过不了 → 白屏。
     * 与前端 `App.tsx` 的 `WEBVIEW_MIN_MAJOR`、Rust `diag.rs` 的上报门槛三处保持一致。
     */
    const val CHROME_MIN_MAJOR_WITH_WORKING_JS = 83

    /** Kotlin 写给 Rust 的探测文件（放在 app_data_dir 根下） */
    const val WEBVIEW_PROBE_FILE = "webview_probe"

    /** 探测回调的等待上限：要等它加载完空白页再问，极旧内核上还可能永不回调 */
    const val PROBE_TIMEOUT_MS = 4000L

    /**
     * 问引擎「成品里这几个语法你解析得了吗」：可选链与空值合并是 Chrome 80 的语法，
     * 构建期没有也不需要降级它们 —— 解析不了就说明成品跑不起来，解析得了就交给前端门禁页。
     * （原来的 `??=` 探针随产物降级一起作废：成品里已经没有逻辑赋值了，再探它只会误杀。）
     */
    const val JS_PARSE_PROBE =
      "(function(){try{eval('var a={b:1};var c=a?.b??2;var d=[1]?.length;');return '1'}catch(e){return '0'}})()"
  }
}
