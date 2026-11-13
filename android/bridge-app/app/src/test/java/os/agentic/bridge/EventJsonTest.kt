package os.agentic.bridge

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class EventJsonTest {
    @Test
    fun a_notification_has_exactly_the_fields_of_the_format() {
        val j = EventJson.notification("1731400000123-7", 1731400000123L, "com.whatsapp", "WhatsApp",
            "Mum", "Mum", "Running 10 min late?", "0|com.whatsapp|1|abc|10123",
            listOf(Att("Invoice-0042.pdf", "files/1731400000123-Invoice-0042.pdf", "application/pdf", 48213)))
        assertEquals(1, j.getInt("v"))
        assertEquals("notification", j.getString("kind"))
        assertEquals("com.whatsapp", j.getString("package"))
        assertEquals("WhatsApp", j.getString("app"))
        assertEquals("Mum", j.getString("conversation"))
        assertEquals("Running 10 min late?", j.getString("text"))
        assertEquals("0|com.whatsapp|1|abc|10123", j.getString("reply_key"))
        val a = j.getJSONArray("attachments").getJSONObject(0)
        assertEquals("Invoice-0042.pdf", a.getString("name"))
        assertEquals(48213L, a.getLong("size"))
    }

    @Test
    fun text_is_cut_to_4000_characters() {
        val j = EventJson.notification("x", 1L, "p", "a", "c", "s", "é".repeat(5000), "k", emptyList())
        assertEquals(4000, j.getString("text").length)
    }

    @Test
    fun a_result_names_the_command_it_answers() {
        val j = EventJson.result("c-1", 2L, false, "no app")
        assertEquals("result", j.getString("kind"))
        assertEquals("c-1", j.getString("command_id"))
        assertTrue(!j.getBoolean("ok"))
    }
}
