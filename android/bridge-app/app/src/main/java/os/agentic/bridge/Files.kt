package os.agentic.bridge

import android.content.ContentUris
import android.content.Context
import android.database.ContentObserver
import android.net.Uri
import android.os.Handler
import android.os.Looper
import android.provider.MediaStore

/**
 * Watches MediaStore for new files in the folders the owner uses for documents, and sends each one
 * to Agentic OS. Needs the media permission granted once; without it nothing is sent.
 */
class Files(private val ctx: Context) {
    private var lastId = 0L

    companion object {
        /** Folders whose new files are sent. */
        val FOLDERS = listOf(
            "Download/",
            "Documents/",
            "DCIM/",
            "Android/media/com.whatsapp/WhatsApp/Media/WhatsApp Documents/",
        )
        private val collection: Uri = MediaStore.Files.getContentUri("external")
    }

    fun start() {
        lastId = newestId()
        ctx.contentResolver.registerContentObserver(collection, true,
            object : ContentObserver(Handler(Looper.getMainLooper())) {
                override fun onChange(selfChange: Boolean) = scan()
            })
    }

    private fun newestId(): Long = try {
        ctx.contentResolver.query(collection, arrayOf(MediaStore.Files.FileColumns._ID), null, null,
            "${MediaStore.Files.FileColumns._ID} DESC")?.use { c -> if (c.moveToFirst()) c.getLong(0) else 0L } ?: 0L
    } catch (e: SecurityException) {
        0L
    }

    @Synchronized
    fun scan() {
        val proj = arrayOf(
            MediaStore.Files.FileColumns._ID,
            MediaStore.Files.FileColumns.DISPLAY_NAME,
            MediaStore.MediaColumns.RELATIVE_PATH,
            MediaStore.Files.FileColumns.SIZE,
            MediaStore.Files.FileColumns.MIME_TYPE,
        )
        val rows = try {
            ctx.contentResolver.query(collection, proj, "${MediaStore.Files.FileColumns._ID} > ?",
                arrayOf(lastId.toString()), "${MediaStore.Files.FileColumns._ID} ASC")
        } catch (e: SecurityException) {
            return // no permission yet: send nothing
        } ?: return
        rows.use { c ->
            while (c.moveToNext()) {
                val id = c.getLong(0)
                lastId = id
                val name = c.getString(1) ?: continue
                val path = c.getString(2) ?: ""
                if (FOLDERS.none { path.startsWith(it) }) continue
                val uri = ContentUris.withAppendedId(collection, id)
                val input = ctx.contentResolver.openInputStream(uri) ?: continue
                val rel = Hub.spool.copyIn(input, name)
                val att = Att(name, rel, c.getString(4) ?: "", c.getLong(3))
                val pkg = if (path.contains("com.whatsapp")) "com.whatsapp" else ""
                val now = System.currentTimeMillis()
                Hub.spool.writeEvent(EventJson.file(Hub.spool.newId(now), now, pkg, listOf(att)))
            }
        }
    }
}
