package os.agentic.bridge

import android.app.Notification
import android.content.Context
import java.io.File

/**
 * Shared state for the services in this app: the spool folder, the reply actions of
 * notifications that can be answered, and the notification listener while it is connected.
 */
object Hub {
    private var initialised = false
    lateinit var spool: Spool
        private set
    /** notification key -> the action that answers it. Capped; the oldest goes first. */
    val actions = LinkedHashMap<String, Notification.Action>()
    var listener: Listener? = null

    @Synchronized
    fun init(ctx: Context) {
        if (initialised) return
        // Waydroid shares this folder with the host; no storage permission is needed for it
        val root: File = ctx.externalMediaDirs.firstOrNull() ?: ctx.filesDir
        spool = Spool(root)
        initialised = true
    }

    @Synchronized
    fun remember(key: String, action: Notification.Action) {
        actions[key] = action
        while (actions.size > 200) {
            val first = actions.keys.first()
            actions.remove(first)
        }
    }
}
