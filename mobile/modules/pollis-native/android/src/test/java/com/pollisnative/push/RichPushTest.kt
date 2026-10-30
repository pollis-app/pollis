package com.pollisnative.push

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class RichPushTest {
  private val h = "AAAAAAAAAAAAAAAAAAAAAA"

  @Test
  fun richHandleFromTheExpoEnvelope() {
    val data = mapOf("body" to """{"h":"$h"}""", "scopeKey" to "@pollis/mobile", "experienceId" to "@pollis/mobile")
    assertEquals(h, RichPush.richHandle(data, false))
  }

  @Test
  fun richHandleFromRawFcmData() {
    assertEquals(h, RichPush.richHandle(mapOf("h" to h), false))
  }

  @Test
  fun everythingElseFallsThroughToExpo() {
    // Today's alert push, with a notification block.
    assertNull(RichPush.richHandle(mapOf("body" to """{"h":"$h"}"""), true))
    // Today's alert push as Expo encodes it for a data delivery.
    assertNull(RichPush.richHandle(mapOf("title" to "New message", "message" to "You have a new message", "body" to """{"h":"$h"}"""), false))
    // #1508 dismissal.
    assertNull(RichPush.richHandle(mapOf("body" to """{"d":["$h"]}"""), false))
    // Not a handle.
    assertNull(RichPush.richHandle(mapOf("h" to "not a handle!"), false))
    assertNull(RichPush.richHandle(mapOf("body" to "not json"), false))
    assertNull(RichPush.richHandle(emptyMap(), false))
  }

  private fun item(
    conv: String = "c1",
    sender: String = "Ana",
    body: String = "hello",
    bodyKey: String? = null,
    muted: Boolean = false,
    dm: Boolean = true,
    title: String? = null,
    id: String = "m1",
  ): String {
    val bk = if (bodyKey == null) "null" else "\"$bodyKey\""
    val t = if (title == null) "null" else "\"$title\""
    return """{"conversation_id":"$conv","message_id":"$id","sender_name":"$sender","conversation_title":$t,"is_dm":$dm,"body":"$body","body_key":$bk,"muted":$muted}"""
  }

  private fun reply(mode: String, vararg items: String) = """{"ok":true,"mode":"$mode","items":[${items.joinToString(",")}]}"""

  @Test
  fun failuresAndNothingAreGeneric() {
    assertEquals(RichPush.Plan.Generic, RichPush.plan(null))
    assertEquals(RichPush.Plan.Generic, RichPush.plan("garbage"))
    for (reason in listOf("not_logged_in", "lock_busy", "timeout", "network", "decrypt", "internal")) {
      assertEquals(RichPush.Plan.Generic, RichPush.plan("""{"ok":false,"reason":"$reason"}"""))
    }
    assertEquals(RichPush.Plan.Generic, RichPush.plan(reply("full")))
    assertEquals(RichPush.Plan.Generic, RichPush.plan(reply("none", item())))
  }

  @Test
  fun mutedPostsNothing() {
    assertEquals(RichPush.Plan.Silent, RichPush.plan(reply("full", item(muted = true), item(conv = "c2", muted = true))))
  }

  @Test
  fun mutedItemsAreDroppedFromAMixedReply() {
    val plan = RichPush.plan(reply("full", item(conv = "c1", id = "a"), item(conv = "c2", muted = true, id = "b"))) as RichPush.Plan.Rich
    assertEquals("c1", plan.conversationId)
    assertEquals(1, plan.messages.size)
  }

  @Test
  fun fullModeDm() {
    val plan = RichPush.plan(reply("full", item())) as RichPush.Plan.Rich
    assertTrue(plan.isDm)
    assertNull(plan.conversationTitle)
    assertEquals(RichPush.Text.Literal("hello"), plan.messages.single().text)
    assertEquals("Ana", plan.messages.single().sender)
  }

  @Test
  fun groupKeepsItsTitleAndOnlyTheNewestConversation() {
    val plan = RichPush.plan(
      reply(
        "full",
        item(conv = "other", id = "x"),
        item(conv = "g", dm = false, title = "general", id = "y"),
        item(conv = "g", dm = false, title = "general", sender = "Bo", id = "z"),
      ),
    ) as RichPush.Plan.Rich
    assertEquals("g", plan.conversationId)
    assertEquals("general", plan.conversationTitle)
    assertEquals(listOf("y", "z"), plan.messages.map { it.messageId })
  }

  @Test
  fun nameModeHidesTheBody() {
    val plan = RichPush.plan(reply("name", item(body = ""))) as RichPush.Plan.Rich
    assertEquals(RichPush.Text.NameOnly, plan.messages.single().text)
  }

  @Test
  fun placeholdersAreLocalisedByKey() {
    val plan = RichPush.plan(reply("full", item(body = "x", bodyKey = "image"))) as RichPush.Plan.Rich
    assertEquals(RichPush.Text.Placeholder(RichPush.BodyKey.IMAGE), plan.messages.single().text)
  }

  @Test
  fun noSenderIsGeneric() {
    assertEquals(RichPush.Plan.Generic, RichPush.plan(reply("full", item(sender = ""))))
  }

  @Test
  fun outcomeLabelsSurviveMinification() {
    assertEquals("silent", RichPush.Plan.Silent.outcome)
    assertEquals("generic", RichPush.Plan.Generic.outcome)
    assertEquals("rich", RichPush.Plan.Rich("c", true, null, emptyList()).outcome)
  }
}
