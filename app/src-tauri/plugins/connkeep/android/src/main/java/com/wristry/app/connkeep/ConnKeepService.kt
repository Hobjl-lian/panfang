package com.wristry.app.connkeep

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import androidx.core.app.NotificationCompat

/**
 * 连接保活前台服务。
 *
 * 它不做任何业务：socket、读写线程都在插件/进程里，**服务只负责钉住进程** ——
 * 有前台服务 = 进程不是 cached 状态 → Android 12+ 的 app freezer 不会冻它、
 * 系统也不会随内存压力回收它。这样：
 *  ① 息屏 / 按 Home 切走时，表盘传输能继续走完（此前靠强制屏幕常亮绕过去）；
 *  ② Rust 侧的连接总管（掉线检测 + 自动重连）在后台也有机会运行。
 *
 * 通知是系统对前台服务的硬性要求，做成最低优先级 + 静默 + 无角标：
 * 只在通知栏占一行，不响、不震、不打扰。用户从最近任务划掉 App 时服务随之结束
 * （manifest 里 stopWithTask=true）—— 那是预期行为，不做复活。
 */
class ConnKeepService : Service() {

    companion object {
        const val CHANNEL_ID = "panfang_conn_keep"
        const val NOTIF_ID = 4201
        const val EXTRA_NAME = "device_name"

        /** 服务在不在（同进程静态标记，与插件同进程所以可信）。
         *  插件侧发起 start 时是异步的、回调还没跑，所以那个 setter 必须对外可写。 */
        @Volatile
        var running: Boolean = false

        /**
         * 改写通知正文（连接状态变了由 Rust 侧调，见 conn.rs 的 set_keepalive_text）。
         *
         * 为什么要有：通知以前只在 start 时写死「<设备名> · 已连接」，之后**再不改**。
         * 手环断了、正在重连、放弃了，通知还挂着「已连接」——买家据此以为链路好好的，
         * 装表盘失败时也想不到是连接没了。现在让通知跟着连接总管的状态走。
         *
         * 服务没在跑（已断开/已停止）就什么都不做：那时不该有这条通知。
         */
        fun updateText(ctx: Context, text: String) {
            if (!running) return
            try {
                val nm = ctx.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
                // 同一个 id notify = 原地改内容，不会多出一条
                nm.notify(NOTIF_ID, build(ctx, text))
            } catch (e: Exception) {
                // 权限被撤销（Android 13+）等情况：静默失败，不影响连接
                android.util.Log.w("ConnKeep", "更新通知失败: ${e.message}")
            }
        }

        private fun build(ctx: Context, text: String): Notification {
            val launch = ctx.packageManager.getLaunchIntentForPackage(ctx.packageName)
            val contentIntent = launch?.let {
                PendingIntent.getActivity(
                    ctx,
                    0,
                    it,
                    PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE
                )
            }
            val builder = NotificationCompat.Builder(ctx, CHANNEL_ID)
                .setSmallIcon(android.R.drawable.stat_sys_data_bluetooth)
                .setContentTitle("盘坊正在保持与手环的连接")
                .setContentText(text)
                .setOngoing(true)
                .setShowWhen(false)
                .setSilent(true)
                .setPriority(NotificationCompat.PRIORITY_LOW)
                .setCategory(NotificationCompat.CATEGORY_SERVICE)
            if (contentIntent != null) builder.setContentIntent(contentIntent)
            return builder.build()
        }
    }

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onCreate() {
        super.onCreate()
        createChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val name = intent?.getStringExtra(EXTRA_NAME)?.trim().orEmpty().ifEmpty { "手环" }
        val notification = build(this, "$name · 已连接")
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            startForeground(NOTIF_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_CONNECTED_DEVICE)
        } else {
            startForeground(NOTIF_ID, notification)
        }
        running = true
        // 不自愈重启：连接状态在 Rust 侧，服务被系统单独重启只会变成一条没有连接的假通知
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        running = false
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            stopForeground(STOP_FOREGROUND_REMOVE)
        } else {
            @Suppress("DEPRECATION")
            stopForeground(true)
        }
        super.onDestroy()
    }

    private fun createChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val nm = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (nm.getNotificationChannel(CHANNEL_ID) != null) return
        val channel = NotificationChannel(CHANNEL_ID, "连接保活", NotificationManager.IMPORTANCE_LOW).apply {
            description = "与手环保持连接期间显示"
            setShowBadge(false)
            enableVibration(false)
            setSound(null, null)
        }
        nm.createNotificationChannel(channel)
    }
}
