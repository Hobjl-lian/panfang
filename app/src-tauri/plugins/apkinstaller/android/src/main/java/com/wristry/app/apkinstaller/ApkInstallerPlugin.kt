package com.wristry.app.apkinstaller

import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
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
import java.io.BufferedReader
import java.io.File
import java.io.InputStreamReader
import java.security.MessageDigest

@InvokeArg
class InstallArg {
    lateinit var path: String
}

@InvokeArg
class ShareArg {
    lateinit var path: String
    var mime: String = "application/zip"
}

@InvokeArg
class LogcatArg {
    var lines: Int = 2000
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

    /** 诊断导出：抓本应用 tag 的 logcat 尾部（读不到就返回空串，不当作失败）。 */
    @Command
    fun readLogcat(invoke: Invoke) {
        val args = invoke.parseArgs(LogcatArg::class.java)
        val lines = if (args.lines in 100..20000) args.lines else 2000
        val text = try {
            val proc = Runtime.getRuntime().exec(
                arrayOf("logcat", "-d", "-t", lines.toString(), "-s", "WFInstaller:*"),
            )
            val out = BufferedReader(InputStreamReader(proc.inputStream)).use { it.readText() }
            proc.waitFor()
            out
        } catch (_: Exception) {
            ""
        }
        val ret = JSObject()
        ret.put("text", text)
        invoke.resolve(ret)
    }

    /** 分享诊断包：校验源路径必须在应用私有数据/缓存目录内，复制到内部 cacheDir/diag 后分享。 */
    @Command
    fun shareFile(invoke: Invoke) {
        val args = invoke.parseArgs(ShareArg::class.java)
        try {
            val src = File(args.path).canonicalFile
            val allowedRoots = listOfNotNull(
                activity.cacheDir.canonicalFile,
                activity.dataDir.canonicalFile,
                activity.externalCacheDir?.canonicalFile,
            )
            if (allowedRoots.none { src.path.startsWith(it.path + File.separator) }) {
                invoke.reject("PATH_FORBIDDEN", "不允许分享应用目录之外的文件")
                return
            }
            if (!src.exists() || !src.isFile || src.length() <= 0) {
                invoke.reject("FILE_NOT_FOUND", "要分享的文件不存在")
                return
            }
            val dir = File(activity.cacheDir, "diag")
            if (!dir.exists()) dir.mkdirs()
            val dst = File(dir, src.name)
            src.copyTo(dst, overwrite = true)
            val uri = FileProvider.getUriForFile(
                activity,
                "${activity.packageName}.fileprovider",
                dst,
            )
            val send = Intent(Intent.ACTION_SEND).apply {
                type = args.mime.ifEmpty { "application/zip" }
                putExtra(Intent.EXTRA_STREAM, uri)
                addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
            }
            activity.runOnUiThread {
                activity.startActivity(Intent.createChooser(send, "分享诊断包"))
            }
            invoke.resolve()
        } catch (e: Exception) {
            invoke.reject("SHARE_FAILED", e.message ?: "分享失败")
        }
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
        val apk = File(args.path).canonicalFile
        val updatesRoot = File(activity.cacheDir, "updates").canonicalFile
        if (!apk.path.startsWith(updatesRoot.path + File.separator) || apk.extension.lowercase() != "apk") {
            invoke.reject("APK_PATH_FORBIDDEN", "安装包必须位于专用更新缓存目录中")
            return
        }
        if (!apk.exists() || !apk.isFile || apk.length() <= 0) {
            invoke.reject("APK_NOT_FOUND", "安装包不存在或为空")
            return
        }
        val mismatch = verifyApkMismatch(apk)
        if (mismatch != null) {
            // 把两侧证书指纹带进 reject 文本：屏幕上只会显示 humanize 的一句人话，
            // 原始串进 console 与「客户端诊断」，下次再有人遇到能直接看出是哪一侧不对。
            invoke.reject("APK_SIGNATURE_MISMATCH", "安装包签名或包名校验不通过，已拒绝安装（$mismatch）")
            return
        }
        val authority = "${activity.packageName}.fileprovider"
        val uri = try {
            FileProvider.getUriForFile(activity, authority, apk)
        } catch (e: IllegalArgumentException) {
            invoke.reject("APK_NOT_SHARABLE", e.message ?: "安装包位置无法共享给安装器")
            return
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

    /**
     * 校验下载包与已装版本同包名、同签名。
     *
     * ★ 2026-11-27 修复：**两个标志必须一起传**。Android 9/10 的
     *   `PackageManager.getPackageArchiveInfo()` 只在 flags 里带 `GET_SIGNATURES` 时才真的去
     *   解析证书（只传 `GET_SIGNING_CERTIFICATES` 时 `signingInfo` 恒为 null），于是新包证书
     *   永远读成空集 → 每次更新都被判「签名不一致」，安卓 9/10 用户点更新只能看到「操作失败」。
     *   （AOSP android-9/10 的 PackageManager.java 里那句 `if ((flags & GET_SIGNATURES) != 0)
     *   collectCertificates()` 就是这个坑；安卓 11 起才两种标志都解析。）
     *
     * 返回 null = 通过；否则返回一句能定位差异的原因（进诊断，不上屏）。
     */
    private fun verifyApkMismatch(apk: File): String? {
        return try {
            val pm = activity.packageManager
            @Suppress("DEPRECATION")
            val flags = PackageManager.GET_SIGNATURES or PackageManager.GET_SIGNING_CERTIFICATES
            val selfInfo = pm.getPackageInfo(activity.packageName, flags)
            val archInfo = pm.getPackageArchiveInfo(apk.absolutePath, flags) ?: return "系统读不出这个安装包"
            if (archInfo.packageName != activity.packageName) {
                return "包名不符：本机 ${activity.packageName} / 新包 ${archInfo.packageName}"
            }
            val selfSigs = signerDigests(selfInfo)
            val archSigs = signerDigests(archInfo)
            if (selfSigs.isEmpty()) return "读不到本机签名证书"
            if (archSigs.isEmpty()) return "读不到安装包签名证书"
            if (selfSigs == archSigs) null
            else "签名不符：本机 ${selfSigs.first().take(16)} / 新包 ${archSigs.first().take(16)}"
        } catch (e: Exception) {
            "校验安装包失败：${e.message}"
        }
    }

    /**
     * 取「当前签名者」的 SHA-256 集合。
     *
     * 只比当前签名者，不比历史证书链：轮换过密钥的包，两个包的历史链长度可能不同，
     * 那属于正常情况；`GET_SIGNATURES` 在老系统上还会回退成最老的证书，口径也不一致。
     */
    private fun signerDigests(info: android.content.pm.PackageInfo): Set<String> {
        // SigningInfo 在没有证书时会抛 IllegalStateException：读不出来就按「没有」处理（宁拒绝不错放）
        val raw: Array<android.content.pm.Signature>? = try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                val si = info.signingInfo
                when {
                    si == null -> null
                    si.hasMultipleSigners() -> si.apkContentsSigners
                    si.apkContentsSigners?.isNotEmpty() == true -> si.apkContentsSigners
                    else -> si.signingCertificateHistory
                }
            } else {
                @Suppress("DEPRECATION")
                info.signatures
            }
        } catch (_: Exception) {
            null
        }
        if (raw.isNullOrEmpty()) return emptySet()
        val sha256 = MessageDigest.getInstance("SHA-256")
        return raw.map { sig ->
            sha256.digest(sig.toByteArray()).joinToString("") { "%02x".format(it) }
        }.toSet()
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
