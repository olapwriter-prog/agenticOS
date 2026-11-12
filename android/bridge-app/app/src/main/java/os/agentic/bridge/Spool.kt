package os.agentic.bridge

import org.json.JSONObject
import java.io.File
import java.io.InputStream

/**
 * The spool folder shared with Agentic OS: out/ (events from here), in/ (commands to here),
 * files/ (attachments). Every file is written under a .tmp name and renamed when complete.
 */
class Spool(val root: File) {
    private var seq = 0

    init {
        for (sub in listOf("out", "in", "files")) File(root, sub).mkdirs()
    }

    /** <time_ms>-<seq>, the id of an event and of its file name. */
    @Synchronized
    fun newId(nowMs: Long): String = "$nowMs-${++seq}"

    fun writeEvent(json: JSONObject) {
        write(File(root, "out"), "${json.getString("id")}.json", json.toString())
    }

    /** Commands waiting in in/, oldest first. Each is deleted once read; a broken one is dropped. */
    fun readCommands(): List<JSONObject> {
        val files = File(root, "in").listFiles { f -> f.name.endsWith(".json") } ?: return emptyList()
        return files.sortedBy { it.name }.mapNotNull { f ->
            val parsed = try {
                JSONObject(f.readText())
            } catch (e: Exception) {
                null
            }
            f.delete()
            parsed
        }
    }

    /** Copies a file into files/ so Agentic OS can read it; returns its rel path. */
    fun copyIn(input: InputStream, name: String): String {
        val clean = name.replace(Regex("[^A-Za-z0-9 ._()-]"), "_").take(120).ifBlank { "file" }
        val target = File(File(root, "files"), "in-$clean")
        input.use { src -> target.outputStream().use { src.copyTo(it) } }
        return "files/${target.name}"
    }

    private fun write(dir: File, name: String, text: String) {
        val tmp = File(dir, "$name.tmp")
        tmp.writeText(text)
        tmp.renameTo(File(dir, name))
    }
}
