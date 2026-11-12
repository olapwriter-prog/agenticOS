package os.agentic.bridge

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Intent
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import org.json.JSONObject

/**
 * The foreground service: once a second it runs the commands Agentic OS has written, and sends a
 * result for each. It also keeps the file watcher running.
 */
class Pump : Service() {
    private val handler = Handler(Looper.getMainLooper())
    private lateinit var files: Files
    private val tick = object : Runnable {
        override fun run() {
            runCommands()
            handler.postDelayed(this, 1000)
        }
    }

    override fun onCreate() {
        super.onCreate()
        Hub.init(this)
        startForeground(NOTE_ID, notification())
        Hub.spool.writeEvent(EventJson.hello(Hub.spool.newId(now()), now(), BuildConfig.VERSION_NAME, android.os.Build.VERSION.SDK_INT))
        files = Files(this)
        files.start()
        handler.post(tick)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int = START_STICKY

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onDestroy() {
        handler.removeCallbacks(tick)
        super.onDestroy()
    }

    private fun runCommands() {
        for (c in Hub.spool.readCommands()) {
            val id = c.optString("id")
            val (ok, error) = when (c.optString("kind")) {
                "reply" -> {
                    val err = Replies.reply(this, c.optString("reply_key"), c.optString("text"))
                    (err == null) to (err ?: "")
                }
                "open" -> open(c.optString("package"))
                "share" -> share(c)
                "dismiss" -> dismiss(c.optString("reply_key"))
                else -> false to "unknown command"
            }
            Hub.spool.writeEvent(EventJson.result(id, now(), ok, error))
        }
    }

    private fun open(pkg: String): Pair<Boolean, String> {
        val launch = packageManager.getLaunchIntentForPackage(pkg)
            ?: return false to "no app called $pkg"
        launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        startActivity(launch)
        return true to ""
    }

    private fun share(c: JSONObject): Pair<Boolean, String> {
        val pkg = c.optString("package")
        val rel = c.optString("rel")
        val uri = Docs.uriFor(rel)
        val send = Intent(Intent.ACTION_SEND).apply {
            type = c.optString("mime").ifBlank { "*/*" }
            putExtra(Intent.EXTRA_STREAM, uri)
            addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION or Intent.FLAG_ACTIVITY_NEW_TASK)
            if (pkg.isNotEmpty()) setPackage(pkg)
        }
        return try {
            startActivity(send)
            true to ""
        } catch (e: Exception) {
            false to "could not share: ${e.javaClass.simpleName}"
        }
    }

    private fun dismiss(key: String): Pair<Boolean, String> {
        val l = Hub.listener ?: return false to "notification access is off"
        l.cancelNotification(key)
        return true to ""
    }

    private fun notification(): Notification {
        val nm = getSystemService(NotificationManager::class.java)
        nm.createNotificationChannel(NotificationChannel(CHANNEL, "Agentic Bridge", NotificationManager.IMPORTANCE_MIN))
        return Notification.Builder(this, CHANNEL)
            .setContentTitle("Shared with Agentic OS")
            .setContentText("Messages and files are sent while this runs")
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .build()
    }

    private fun now(): Long = System.currentTimeMillis()

    companion object {
        const val NOTE_ID = 7
        const val CHANNEL = "agentic-bridge"
    }
}
