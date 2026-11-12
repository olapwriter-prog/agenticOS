package os.agentic.bridge

import android.Manifest
import android.app.Activity
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Bundle
import android.provider.Settings
import android.view.ViewGroup
import android.widget.Button
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.app.NotificationManagerCompat
import androidx.core.content.ContextCompat
import androidx.core.app.ActivityCompat

/** One screen: three rows, each with what is granted and a button to grant it. */
class Setup : Activity() {
    private lateinit var notif: TextView
    private lateinit var files: TextView
    private lateinit var background: TextView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Hub.init(this)
        val col = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(48, 96, 48, 48)
        }
        col.addView(TextView(this).apply { text = "Agentic Bridge"; textSize = 26f })
        col.addView(TextView(this).apply {
            text = "Lets Agentic OS see messages you allow, and send replies and files. Nothing else leaves this sandbox."
            setPadding(0, 16, 0, 32)
        })
        notif = row(col, "Notification access", Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS)
        files = row(col, "File access", null) { ActivityCompat.requestPermissions(this, arrayOf(Manifest.permission.READ_MEDIA_IMAGES, Manifest.permission.READ_EXTERNAL_STORAGE), 1) }
        background = row(col, "Run in background", null) { startService(Intent(this, Pump::class.java)) }
        setContentView(col)
    }

    override fun onResume() {
        super.onResume()
        val notifOn = NotificationManagerCompat.getEnabledListenerPackages(this).contains(packageName)
        notif.text = if (notifOn) "Notification access: on" else "Notification access: off"
        val filesOn = ContextCompat.checkSelfPermission(this, Manifest.permission.READ_MEDIA_IMAGES) == PackageManager.PERMISSION_GRANTED ||
            ContextCompat.checkSelfPermission(this, Manifest.permission.READ_EXTERNAL_STORAGE) == PackageManager.PERMISSION_GRANTED
        files.text = if (filesOn) "File access: on" else "File access: off"
        background.text = "Run in background: started on boot"
    }

    private fun row(col: LinearLayout, label: String, action: String?, onTap: (() -> Unit)? = null): TextView {
        val status = TextView(this).apply { setPadding(0, 24, 0, 8) }
        col.addView(status, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT))
        col.addView(Button(this).apply {
            text = label
            setOnClickListener {
                when {
                    onTap != null -> onTap()
                    action != null -> startActivity(Intent(action))
                }
            }
        })
        return status
    }
}
