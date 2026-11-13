package os.agentic.bridge

import android.app.Notification
import android.content.pm.PackageManager
import android.os.Bundle
import android.service.notification.NotificationListenerService
import android.service.notification.StatusBarNotification

/**
 * Reads notifications from apps and sends them to Agentic OS. The text is never logged.
 * Only the reply action is kept, so a reply can be sent later without opening the app.
 */
class Listener : NotificationListenerService() {

    override fun onListenerConnected() {
        Hub.init(this)
        Hub.listener = this
        Hub.spool.writeEvent(EventJson.hello(Hub.spool.newId(now()), now(), BuildConfig.VERSION_NAME, android.os.Build.VERSION.SDK_INT))
    }

    override fun onListenerDisconnected() {
        if (Hub.listener === this) Hub.listener = null
    }

    override fun onNotificationPosted(sbn: StatusBarNotification) {
        if (sbn.packageName == packageName) return
        val n = sbn.notification
        if (sbn.isOngoing || (n.flags and Notification.FLAG_GROUP_SUMMARY) != 0) return
        Hub.init(this)
        val extras: Bundle = n.extras
        val title = extras.getCharSequence(Notification.EXTRA_TITLE)?.toString().orEmpty()
        val conversation = extras.getCharSequence(Notification.EXTRA_CONVERSATION_TITLE)?.toString().orEmpty()
        var text = extras.getCharSequence(Notification.EXTRA_TEXT)?.toString().orEmpty()
        var sender = title
        // a chat notification carries its messages; the newest is the one to show
        @Suppress("DEPRECATION")
        val parcels = extras.getParcelableArray(Notification.EXTRA_MESSAGES)
        if (parcels != null && parcels.isNotEmpty()) {
            val msgs = Notification.MessagingStyle.Message.getMessagesFromBundleArray(parcels)
            val last = msgs.lastOrNull()
            if (last != null) {
                text = last.text?.toString().orEmpty()
                sender = last.person?.name?.toString().orEmpty().ifBlank { title }
            }
        }
        val replyAction = n.actions?.firstOrNull { it.remoteInputs?.isNotEmpty() == true }
        if (replyAction != null) Hub.remember(sbn.key, replyAction)
        val files = emptyList<Att>()
        val json = EventJson.notification(
            id = Hub.spool.newId(sbn.postTime),
            timeMs = sbn.postTime,
            pkg = sbn.packageName,
            app = appName(sbn.packageName),
            conversation = conversation.ifBlank { sender },
            sender = sender,
            text = text,
            replyKey = sbn.key,
            attachments = files,
        )
        Hub.spool.writeEvent(json)
    }

    override fun onNotificationRemoved(sbn: StatusBarNotification) {
        Hub.init(this)
        Hub.actions.remove(sbn.key)
        Hub.spool.writeEvent(EventJson.removed(Hub.spool.newId(now()), now(), sbn.key))
    }

    private fun appName(pkg: String): String = try {
        packageManager.getApplicationLabel(packageManager.getApplicationInfo(pkg, PackageManager.GET_META_DATA)).toString()
    } catch (e: Exception) {
        pkg
    }

    private fun now(): Long = System.currentTimeMillis()
}
