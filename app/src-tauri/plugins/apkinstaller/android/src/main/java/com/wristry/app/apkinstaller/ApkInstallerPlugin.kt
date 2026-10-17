package com.wristry.app.apkinstaller

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.provider.Settings
import androidx.core.content.FileProvider
import app.tauri.annotation.Command
import app.tauri.annotation.InvokeArg
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin
import java.io.File

@InvokeArg
class InstallArg {
    lateinit var path: String
}

/**
 * 盘坊自更新：把下载到 cacheDir 的 APK 交给系统安装器。
 *
 * 三个命令（都只从 Rust 侧调用，不暴露给网页层）：
 *  - status：自己的 versionCode + 有没有「安装未知来源应用」授权
 *  - openInstallSettings：跳到本应用的「安装未知来源应用」设置页
 *    （这个授权是**按 App 逐个给**的：买家当初用浏览器装的盘坊，所以盘坊自己没有这个权限）
 *  - install：FileProvider 出 content:// URI → ACTION_VIEW 调起安装器
 *
 * 装完之后系统会杀掉本进程，重开时 versionCode 就是新的了。
 */
@TauriPlugin
class ApkInstallerPlugin(private val activity: Activity) : Plugin(activity) {

    init {
        // 把跨卸载重装稳定的系统设备标识写到应用数据目录，Rust 侧启动时直接读文件
        // （不走命令桥：fat LTO 下曾出现命令名字面量没落进 .so，而这条路径只是一次文件读）。
        // 路径必须与 Tauri 的 app_data_dir 对齐：安卓上它就是 activity.dataDir（不拼包名）。
        // ⚠️ 原始 ANDROID_ID 只落在本机私有目录，上传的是 Rust 侧 sha256 之后的值。
        try {
            val id = Settings.Secure.getString(activity.contentResolver, Settings.Secure.ANDROID_ID)
            if (!id.isNullOrEmpty()) {
                val dir = File(activity.dataDir, "paid")
                if (!dir.exists()) dir.mkdirs()
                File(dir, "android_id").writeText(id)
            }
        } catch (_: Exception) {
            // 读不到就保持默认：Rust 侧退回原来的随机 uuid
        }
    }

    @Command
    fun status(invoke: Invoke) {
        val ret = JSObject()
        ret.put("version_code", versionCode())
        ret.put("can_install", canInstall())
        invoke.resolve(ret)
    }

    /**
     * 内部 cache 目录（`context.cacheDir`）—— 下载的 APK 必须落在这里。
     *
     * 为什么不用 Tauri 的 `cache_dir()`：它在安卓上映射的是 **getExternalCacheDir**（外部存储），
     * 而 `file_paths.xml` 里给 FileProvider 配的是 `<cache-path>`（内部 cache）—— 对不上就
     * `getUriForFile` 抛 IllegalArgumentException，真机上表现为「点更新没反应」。
     */
    @Command
    fun cacheDir(invoke: Invoke) {
        val ret = JSObject()
        ret.put("path", activity.cacheDir.absolutePath)
        invoke.resolve(ret)
    }

    @Command
    fun openInstallSettings(invoke: Invoke) {
        try {
            val intent = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
                Intent(
                    Settings.ACTION_MANAGE_UNKNOWN_APP_SOURCES,
                    Uri.parse("package:${activity.packageName}"),
                )
            } else {
                // Android 7 及以下没有逐应用开关，只能落到安全设置页
                Intent(Settings.ACTION_SECURITY_SETTINGS)
            }
            activity.runOnUiThread { activity.startActivity(intent) }
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("OPEN_SETTINGS_FAILED", e.message ?: "无法打开系统设置页")
        }
    }

    @Command
    fun install(invoke: Invoke) {
        val args = invoke.parseArgs(InstallArg::class.java)
        if (!canInstall()) {
            invoke.reject("NEED_INSTALL_PERMISSION", "还没有允许盘坊安装应用")
            return
        }
        val apk = File(args.path)
        if (!apk.exists() || apk.length() <= 0) {
            invoke.reject("APK_NOT_FOUND", "安装包不存在或为空")
            return
        }
        val authority = "${activity.packageName}.fileprovider"
        // 路径不在 FileProvider 白名单里（例如下载落到了别处）→ 先复制进内部 cache 再来一次
        val uri = try {
            FileProvider.getUriForFile(activity, authority, apk)
        } catch (e: IllegalArgumentException) {
            try {
                val moved = File(activity.cacheDir, "updates/${apk.name}")
                moved.parentFile?.mkdirs()
                apk.copyTo(moved, overwrite = true)
                FileProvider.getUriForFile(activity, authority, moved)
            } catch (e2: Exception) {
                invoke.reject("APK_NOT_SHARABLE", e2.message ?: "安装包位置无法共享给安装器")
                return
            }
        }
        try {
            val intent = Intent(Intent.ACTION_VIEW).apply {
                setDataAndType(uri, "application/vnd.android.package-archive")
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
            }
            activity.runOnUiThread { activity.startActivity(intent) }
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("INSTALL_INTENT_FAILED", e.message ?: "调起安装器失败")
        }
    }

    private fun canInstall(): Boolean = try {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            activity.packageManager.canRequestPackageInstalls()
        } else {
            true
        }
    } catch (e: Exception) {
        true // 判断不了就按「能装」走，交给系统安装器去拒绝（别在这里替用户决定）
    }

    private fun versionCode(): Long = try {
        val info = activity.packageManager.getPackageInfo(activity.packageName, 0)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
            info.longVersionCode
        } else {
            @Suppress("DEPRECATION")
            info.versionCode.toLong()
        }
    } catch (e: Exception) {
        0L
    }
}
