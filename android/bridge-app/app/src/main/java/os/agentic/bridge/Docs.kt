package os.agentic.bridge

import android.database.Cursor
import android.database.MatrixCursor
import android.os.CancellationSignal
import android.os.ParcelFileDescriptor
import android.provider.DocumentsContract
import android.provider.DocumentsContract.Document
import android.provider.DocumentsContract.Root
import android.provider.DocumentsProvider
import java.io.File

/**
 * Shows the files Agentic OS offered (files/out-*) to Android's file picker. Read-only.
 * A document id is the file's rel path, for example files/out-Invoice.pdf.
 */
class Docs : DocumentsProvider() {
    private fun base(): File = Hub.spool.root

    override fun onCreate(): Boolean {
        return true
    }

    override fun queryRoots(projection: Array<String>?): Cursor {
        val c = MatrixCursor(projection ?: arrayOf(Root.COLUMN_ROOT_ID, Root.COLUMN_TITLE, Root.COLUMN_DOCUMENT_ID, Root.COLUMN_FLAGS))
        c.newRow()
            .add(Root.COLUMN_ROOT_ID, "agentic-os")
            .add(Root.COLUMN_TITLE, "Agentic OS")
            .add(Root.COLUMN_DOCUMENT_ID, "files")
            .add(Root.COLUMN_FLAGS, Root.FLAG_LOCAL_ONLY)
        return c
    }

    override fun queryDocument(documentId: String, projection: Array<String>?): Cursor {
        val c = MatrixCursor(projection ?: arrayOf(Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE, Document.COLUMN_SIZE))
        val f = resolve(documentId)
        if (f != null) addRow(c, documentId, f)
        return c
    }

    override fun queryChildDocuments(parentDocumentId: String, projection: Array<String>?, sortOrder: String?): Cursor {
        val c = MatrixCursor(projection ?: arrayOf(Document.COLUMN_DOCUMENT_ID, Document.COLUMN_DISPLAY_NAME, Document.COLUMN_MIME_TYPE, Document.COLUMN_SIZE))
        if (parentDocumentId == "files") {
            File(base(), "files").listFiles { f -> f.name.startsWith("out-") }?.sortedBy { it.name }?.forEach {
                addRow(c, "files/${it.name}", it)
            }
        }
        return c
    }

    override fun openDocument(documentId: String, mode: String, signal: CancellationSignal?): ParcelFileDescriptor {
        val f = resolve(documentId) ?: throw java.io.FileNotFoundException(documentId)
        return ParcelFileDescriptor.open(f, ParcelFileDescriptor.MODE_READ_ONLY)
    }

    /** Only files/out-* under the spool root; anything else is refused. */
    private fun resolve(docId: String): File? {
        if (!docId.startsWith("files/out-") || docId.contains("..")) return null
        val f = File(base(), docId)
        return if (f.isFile) f else null
    }

    private fun addRow(c: MatrixCursor, docId: String, f: File) {
        c.newRow()
            .add(Document.COLUMN_DOCUMENT_ID, docId)
            .add(Document.COLUMN_DISPLAY_NAME, f.name.removePrefix("out-"))
            .add(Document.COLUMN_MIME_TYPE, "application/octet-stream")
            .add(Document.COLUMN_SIZE, f.length())
            .add(Document.COLUMN_FLAGS, 0)
    }

    companion object {
        const val AUTHORITY = "os.agentic.bridge.documents"
        /** The content uri a share to Android uses for a file the host offered. */
        fun uriFor(rel: String): android.net.Uri = DocumentsContract.buildDocumentUri(AUTHORITY, rel)
    }
}
