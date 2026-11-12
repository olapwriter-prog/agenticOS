package os.agentic.bridge

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.provider.OpenableColumns
import android.widget.Toast

/** The "Agentic OS" entry in Android's share menu: copies what was shared and sends it across. */
class ShareIn : Activity() {
    @Suppress("DEPRECATION")
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        Hub.init(this)
        val uris: List<Uri> = when (intent?.action) {
            Intent.ACTION_SEND_MULTIPLE -> intent.getParcelableArrayListExtra<Uri>(Intent.EXTRA_STREAM).orEmpty()
            Intent.ACTION_SEND -> listOfNotNull(intent.getParcelableExtra<Uri>(Intent.EXTRA_STREAM))
            else -> emptyList()
        }
        val atts = uris.mapNotNull { uri ->
            val name = displayName(uri)
            val input = contentResolver.openInputStream(uri) ?: return@mapNotNull null
            val rel = Hub.spool.copyIn(input, name)
            Att(name, rel, contentResolver.getType(uri) ?: "", 0L)
        }
        val text = intent?.getStringExtra(Intent.EXTRA_TEXT).orEmpty()
        val now = System.currentTimeMillis()
        if (atts.isNotEmpty() || text.isNotEmpty()) {
            Hub.spool.writeEvent(EventJson.share(Hub.spool.newId(now), now, atts, text))
            Toast.makeText(this, "Sent to Agentic OS", Toast.LENGTH_SHORT).show()
        }
        finish()
    }

    private fun displayName(uri: Uri): String {
        contentResolver.query(uri, arrayOf(OpenableColumns.DISPLAY_NAME), null, null, null)?.use { c ->
            if (c.moveToFirst()) return c.getString(0) ?: "shared"
        }
        return uri.lastPathSegment ?: "shared"
    }
}
