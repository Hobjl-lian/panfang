package com.wristry.app.connkeep

import android.Manifest
import android.app.Activity
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import app.tauri.plugin.PluginManager

@InvokeArg
class StartArg {
    lateinit var name: String
    lateinit var addr: String
}

/**
 * 连接保活插件（只从 Rust 侧调用，不暴露给网页层）。
 *
 *  - start：拉起 / 刷新前台服务（保活）
 *  - stop：停掉前台服务（用户主动断开 / 切换设备时）
 *  - isRunning：服务在不在
 *  - openAppSettings：打开本应用的应用详情页（国产 ROM 的电池 / 后台设置入口）
 *  - requestIgnoreBattery：尝试弹「不限制电池优化」系统对话框，弹不出来返回 shown=false
 *
 * 通知权限（Android 13+）在第一次 start 时申请一次；用户拒绝也不影响保活：
 * 前台服务照跑，只是通知不显示（系统仍把进程视为前台服务进程）。
 */
@TauriPlugin
class ConnKeepPlugin(private val activity: Activity) : Plugin(activity) {

    @Command
    fun start(invoke: Invoke) {
        val args = invoke.parseArgs(StartArg::class.java)
        val intent = Intent(activity, ConnKeepService::class.java).apply {
            putExtra(ConnKeepService.EXTRA_NAME, args.name)
            putExtra("device_addr", args.addr)
        }
        try {
            // 前台服务只能在前台启动（Android 12+ 限制）；本命令都在用户操作链路里发起，
            // 但重连路径可能在后台跑到这里 —— 届时系统会抛异常，捕获后如实上报。
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                activity.startForegroundService(intent)
            } else {
                activity.startService(intent)
            }
            // startForegroundService 是异步的：服务回调还没跑，静态标记此刻还是旧值。
            // 调用方问的是「服务现在起没起」——刚发起的这次就是要它起来，所以这里直接置位。
            ConnKeepService.running = true
        } catch (e: Exception) {
            invoke.reject("FGS_START_FAILED", e.message ?: "无法启动保活服务")
            return
        }
        requestNotificationPermissionOnce()
        val ret = JSObject()
        ret.put("running", ConnKeepService.running)
        ret.put("notification_allowed", hasNotificationPermission())
        invoke.resolve(ret)
    }

    @Command
    fun stop(invoke: Invoke) {
        try {
            activity.stopService(Intent(activity, ConnKeepService::class.java))
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("FGS_STOP_FAILED", e.message ?: "无法停止保活服务")
        }
    }

    @Command
    fun isRunning(invoke: Invoke) {
        val ret = JSObject()
        ret.put("running", ConnKeepService.running)
        invoke.resolve(ret)
    }

    /**
     * 连接前把「附近的设备」两个权限一起要齐（Android 12+）。
     *
     * 为什么不能只申请 BLUETOOTH_CONNECT：SPP 建链本身只要 CONNECT，但实测 vivo ROM
     * 在 BLUETOOTH_SCAN 被拒时 SDP / RFCOMM 通道探测会直接失败 —— 上层只能看到
     * 「未发现设备」，表现就是「哪个手环都连不上」。用户清数据 / 重装后权限被重置，
     * 而插件里只有 connect 那条路会申请权限（只含 CONNECT），SCAN 永远不会被问到。
     * 所以连接入口先调这个方法；已经齐了则不发弹窗。
     */
    @Command
    fun ensureBtPermissions(invoke: Invoke) {
        val missing = missingBtPermissions()
        if (missing.isEmpty()) {
            val ret = JSObject()
            ret.put("granted", true)
            invoke.resolve(ret)
            return
        }
        val request = {
            try {
                PluginManager.requestPermissions(missing) { _ ->
                    // 不看回调里的 map（不同 ROM 回报字段不一致），以系统当前的真实授权状态为准
                    val ret = JSObject()
                    ret.put("granted", missingBtPermissions().isEmpty())
                    invoke.resolve(ret)
                }
            } catch (e: Exception) {
                invoke.reject("BT_PERMISSION_FAILED", e.message ?: "无法申请蓝牙权限")
            }
        }
        if (Looper.myLooper() == Looper.getMainLooper()) request() else activity.runOnUiThread(request)
    }

    /**
     * 手机型号与系统版本（只给失败打点用）。
     *
     * 为什么需要：第一次连不上手环时，诊断里的「手环型号」是空的 —— 后台只能看到
     * 一串 IP，CGNAT 下不同买家会同 IP，根本分不清是谁。手机型号 + App 安装标识
     * 才能把「同一台手环、不同手机/不同人」区分开。不含任何账号信息。
     */
    @Command
    fun deviceInfo(invoke: Invoke) {
        val manufacturer = (Build.MANUFACTURER ?: "").trim()
        val model = (Build.MODEL ?: "").trim()
        val ret = JSObject()
        ret.put("model", listOf(manufacturer, model).filter { it.isNotEmpty() }.joinToString(" "))
        ret.put("os", "Android ${Build.VERSION.RELEASE} (SDK ${Build.VERSION.SDK_INT})")
        invoke.resolve(ret)
    }

    /** Android 12 以下没有 BLUETOOTH_SCAN/CONNECT；那套系统上 SPP 连接不需要运行时权限。 */
    private fun missingBtPermissions(): Array<String> {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return emptyArray()
        return listOf(
            Manifest.permission.BLUETOOTH_SCAN,
            Manifest.permission.BLUETOOTH_CONNECT
        ).filter {
            ContextCompat.checkSelfPermission(activity, it) != PackageManager.PERMISSION_GRANTED
        }.toTypedArray()
    }

    @Command
    fun openAppSettings(invoke: Invoke) {
        // 拉起系统页要吞掉异常（在 UI 线程里抛 = 直接崩，不能只 try 外层）
        val launch = {
            if (startActivitySafely(appDetailsIntent())) invoke.resolve()
            else invoke.reject("OPEN_SETTINGS_FAILED", "无法打开系统设置页")
        }
        if (Looper.myLooper() == Looper.getMainLooper()) launch() else activity.runOnUiThread(launch)
    }

    /**
     * 「后台保持连接」一键入口：按最能帮到用户的那一步走，并如实回报做了什么。
     *
     * 判定顺序：
     *  1. 已经允许（或系统 < 6.0，本来就豁免）→ `already`
     *  2. 从没弹过系统对话框 → 弹「是否允许应用不受电池优化限制？」→ `dialog`
     *     （ROM 把这个对话框藏了/拉不起来 → 退到应用信息页 → `settings`）
     *  3. 弹过一次还没允许（上次被关掉/拒绝）→ 直接开应用信息页 → `settings`
     *     ★ 为什么必须有第 3 步：真机实测（vivo）第二次点这个对话框**什么都不显示**，
     *       用户看到的就是「点了没反应」；第二次起直接给设置页，保证每次点都有东西出来。
     *  任何界面都打不开 → `failed`（前端给一句手动操作指引）
     */
    @Command
    fun batteryHelp(invoke: Invoke) {
        val planned = when {
            Build.VERSION.SDK_INT < Build.VERSION_CODES.M -> "already"
            isIgnoringBatteryOptimizations() -> "already"
            !prefs.getBoolean(ASKED_DIALOG_KEY, false) -> {
                prefs.edit().putBoolean(ASKED_DIALOG_KEY, true).apply()
                "dialog"
            }
            else -> "settings"
        }
        val launch = {
            val actual = when (planned) {
                "dialog" ->
                    if (startActivitySafely(ignoreBatteryIntent())) "dialog"
                    else if (startActivitySafely(appDetailsIntent())) "settings"
                    else "failed"
                "settings" -> if (startActivitySafely(appDetailsIntent())) "settings" else "failed"
                else -> planned
            }
            val ret = JSObject()
            ret.put("result", actual)
            invoke.resolve(ret)
        }
        if (Looper.myLooper() == Looper.getMainLooper()) launch() else activity.runOnUiThread(launch)
    }

    private fun isIgnoringBatteryOptimizations(): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) return true
        val pm = activity.getSystemService(Context.POWER_SERVICE) as? PowerManager ?: return false
        return runCatching { pm.isIgnoringBatteryOptimizations(activity.packageName) }.getOrDefault(false)
    }

    private fun ignoreBatteryIntent() = Intent(
        Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
        Uri.parse("package:${activity.packageName}")
    )

    private fun appDetailsIntent() = Intent(
        Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
        Uri.parse("package:${activity.packageName}")
    )

    /** 拉起一个系统页面；失败返回 false（不崩） */
    private fun startActivitySafely(intent: Intent): Boolean = try {
        activity.startActivity(intent)
        true
    } catch (e: Exception) {
        android.util.Log.w("ConnKeep", "startActivity 失败: ${e.message}")
        false
    }

    private val prefs by lazy {
        activity.getSharedPreferences("connkeep", Context.MODE_PRIVATE)
    }

    private fun hasNotificationPermission(): Boolean =
        Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
            ContextCompat.checkSelfPermission(activity, Manifest.permission.POST_NOTIFICATIONS) ==
            PackageManager.PERMISSION_GRANTED

    /** 只在第一次 start 时申请一次；被拒不再打扰（保活本身不依赖它） */
    private fun requestNotificationPermissionOnce() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        if (hasNotificationPermission() || notificationAsked) return
        notificationAsked = true
        val ask = {
            ActivityCompat.requestPermissions(
                activity,
                arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                REQ_NOTIFICATION
            )
        }
        activity.runOnUiThread(ask)
    }

    private var notificationAsked = false

    private companion object {
        const val REQ_NOTIFICATION = 1002
        /** 「电池优化对话框已经弹过」的本地标记（见 batteryHelp 的判定顺序第 3 步） */
        const val ASKED_DIALOG_KEY = "asked_battery_dialog"
    }
}
