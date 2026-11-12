package os.agentic.bridge

import android.app.PendingIntent
import android.app.RemoteInput
import android.content.Context
import android.content.Intent
import android.os.Bundle

/** Sends a reply through the notification's own reply action, so the app never has to open. */
object Replies {
    /** Returns null on success, or the reason it failed. */
    fun reply(ctx: Context, key: String, text: String): String? {
        val action = Hub.actions[key] ?: return "conversation no longer open"
        val inputs = action.remoteInputs ?: return "this notification cannot be answered"
        if (inputs.isEmpty()) return "this notification cannot be answered"
        val intent = Intent()
        val bundle = Bundle().apply { putCharSequence(inputs[0].resultKey, text) }
        RemoteInput.addResultsToIntent(inputs, intent, bundle)
        return try {
            action.actionIntent.send(ctx, 0, intent)
            null
        } catch (e: PendingIntent.CanceledException) {
            "conversation no longer open"
        }
    }
}
