package os.agentic.bridge

import org.json.JSONArray
import org.json.JSONObject

/** One attachment as the bridge reports it (plan section 1.1). */
data class Att(val name: String, val rel: String, val mime: String, val size: Long)

/**
 * Builds the JSON events of the spool format, version 1. Pure code: no Android classes,
 * so the JVM unit tests can check every field name.
 */
object EventJson {
    const val VERSION = 1
    const val MAX_TEXT = 4000

    fun notification(
        id: String, timeMs: Long, pkg: String, app: String, conversation: String,
        sender: String, text: String, replyKey: String, attachments: List<Att>,
    ): JSONObject = base(id, "notification", timeMs)
        .put("package", pkg)
        .put("app", app)
        .put("conversation", conversation)
        .put("sender", sender)
        .put("text", text.take(MAX_TEXT))
        .put("reply_key", replyKey)
        .put("attachments", attachmentsJson(attachments))

    fun file(id: String, timeMs: Long, pkg: String, attachments: List<Att>): JSONObject =
        base(id, "file", timeMs)
            .put("package", pkg)
            .put("attachments", attachmentsJson(attachments))

    fun share(id: String, timeMs: Long, attachments: List<Att>, text: String): JSONObject =
        base(id, "share", timeMs)
            .put("text", text.take(MAX_TEXT))
            .put("attachments", attachmentsJson(attachments))

    fun removed(id: String, timeMs: Long, replyKey: String): JSONObject =
        base(id, "removed", timeMs).put("reply_key", replyKey)

    fun hello(id: String, timeMs: Long, appVersion: String, sdk: Int): JSONObject =
        base(id, "hello", timeMs).put("app_version", appVersion).put("android_sdk", sdk)

    fun result(commandId: String, timeMs: Long, ok: Boolean, error: String): JSONObject =
        base("r-$commandId", "result", timeMs).put("command_id", commandId).put("ok", ok).put("error", error)

    fun attachmentsJson(list: List<Att>): JSONArray {
        val arr = JSONArray()
        for (a in list) {
            arr.put(JSONObject()
                .put("name", a.name)
                .put("rel", a.rel)
                .put("mime", a.mime)
                .put("size", a.size))
        }
        return arr
    }

    private fun base(id: String, kind: String, timeMs: Long): JSONObject =
        JSONObject().put("v", VERSION).put("id", id).put("kind", kind).put("time_ms", timeMs)
}
