package com.wristry.app

import android.graphics.Color
import android.os.Bundle
import android.webkit.WebSettings
import androidx.activity.SystemBarStyle
import androidx.activity.enableEdgeToEdge
import com.google.android.material.dialog.MaterialAlertDialogBuilder
import moe.astralsight.astrobox.plugin.inappbrowser.InstallBackLock

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    // App 以浅色页为主：系统栏透明（WebView 顶栏条上色）、图标强制深色，
    // 不随系统深色模式翻转（深色模式下白图标会消失在浅灰顶栏上）。
    // 深色页（裁剪页）由 inappbrowser 插件的 setSystemBars 命令切回白色图标。
    enableEdgeToEdge(
      statusBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
      navigationBarStyle = SystemBarStyle.light(Color.TRANSPARENT, Color.TRANSPARENT),
    )
    super.onCreate(savedInstanceState)
    warnIfWebViewTooOld()
  }

  /**
   * 系统 WebView 过旧的原生兜底：低于 CHROME_MIN_MAJOR_WITH_WORKING_JS 的 Chromium
   * 连前端 JS 都解析不了（成品按 ES2020 打包），页面是纯白，JS 侧的门禁页根本没机会显示。
   * 那一档只能在这里弹系统对话框说清原因；能跑 JS 但样式会坏的一档（88~110）
   * 交给前端门禁页（App.tsx 的 WEBVIEW_MIN_MAJOR），因为那边能给出「去更新」按钮。
   *
   * 版本从 WebView 自己的 UA 里取（Chrome/xxx），取不到就放行 —— 拿不准的机型宁可不拦。
   */
  private fun warnIfWebViewTooOld() {
    @Suppress("DEPRECATION")
    val ua = WebSettings.getDefaultUserAgent(this)
    val major = Regex("Chrome/(\\d+)").find(ua)?.groupValues?.get(1)?.toIntOrNull() ?: return
    if (major >= CHROME_MIN_MAJOR_WITH_WORKING_JS) return
    MaterialAlertDialogBuilder(this)
      .setTitle("系统组件版本过旧")
      .setMessage(
        "这台手机的系统 WebView 版本较低（Chrome $major），盘坊的界面无法显示。" +
          "请在应用商店更新「Android System WebView」后重新打开盘坊。",
      )
      .setPositiveButton("退出") { _, _ -> finish() }
      .setCancelable(false)
      .show()
  }

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
    /** 低于这个 Chromium 主版本，前端产物（ES2020）可能连解析都过不了 → 白屏 */
    const val CHROME_MIN_MAJOR_WITH_WORKING_JS = 88
  }
}
