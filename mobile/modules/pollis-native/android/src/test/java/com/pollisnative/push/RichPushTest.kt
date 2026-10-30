package com.pollisnative.push

import org.json.JSONObject
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
  fun theChatOnScreenPostsNothingInTheForegroundOnly() {
    val plan = RichPush.plan(reply("full", item(conv = "c1")))
    assertTrue(RichPush.suppressForOpenConversation(plan, foreground = true, openConversationId = "c1"))
    // Another conversation, the app in the background, or no chat open: post as usual.
    assertEquals(false, RichPush.suppressForOpenConversation(plan, foreground = true, openConversationId = "c2"))
    assertEquals(false, RichPush.suppressForOpenConversation(plan, foreground = false, openConversationId = "c1"))
    assertEquals(false, RichPush.suppressForOpenConversation(plan, foreground = true, openConversationId = null))
    // Generic means the conversation is unknown: never suppressed.
    assertEquals(false, RichPush.suppressForOpenConversation(RichPush.Plan.Generic, foreground = true, openConversationId = "c1"))
  }

  @Test
  fun letterAvatarMatchesTheAppTheme() {
    // derive.ts with the defaults (#fabf5a on #0a0907): high = #332c20, dim = #c8b698 (theme/derive.ts).
    val (fill, fg) = LetterAvatar.colors(LetterAvatar.DEFAULT_ACCENT, LetterAvatar.DEFAULT_BG)
    assertEquals(0x332C20, fill)
    assertEquals(0xC8B698, fg)
    assertEquals("A", LetterAvatar.initial("  ana"))
    assertEquals("😀", LetterAvatar.initial("😀 smile"))
    assertNull(LetterAvatar.initial("   "))
    assertEquals(0xFABF5A, LetterAvatar.parseHex("#fabf5a"))
    assertNull(LetterAvatar.parseHex("nope"))
  }

  @Test
  fun noSenderIsGeneric() {
    assertEquals(RichPush.Plan.Generic, RichPush.plan(reply("full", item(sender = ""))))
  }

  // #1614: every natively posted notification shares one tap intent; these are its extras.

  @Test
  fun tapOfAnExpoEnvelopePushRoutesByItsHandle() {
    val data = mapOf("body" to """{"h":"$h"}""", "scopeKey" to "@pollis/mobile", "experienceId" to "@pollis/mobile")
    val extras = RichPush.tapExtras(h, data, "0:123%abc")
    // The delivery's data rides along unchanged, plus the key expo requires.
    assertEquals(data["body"], extras["body"])
    assertEquals("@pollis/mobile", extras["scopeKey"])
    assertEquals("0:123%abc", extras[RichPush.GOOGLE_MESSAGE_ID])
    assertEquals(h, routedHandle(extras))
  }

  @Test
  fun tapOfARawFcmPushRoutesByItsHandle() {
    val extras = RichPush.tapExtras(h, mapOf("h" to h), "m1")
    assertEquals(h, extras["h"])
    assertEquals(h, routedHandle(extras))
  }

  @Test
  fun tapWithoutAMessageIdStillCountsAsAPushTap() {
    // The debug hook (and any delivery FCM gave no id) has none: expo would drop the tap without one.
    for (id in listOf(null, "")) {
      val extras = RichPush.tapExtras(h, mapOf("body" to """{"h":"$h"}"""), id)
      assertEquals(h, extras[RichPush.GOOGLE_MESSAGE_ID])
      assertEquals(h, routedHandle(extras))
    }
  }

  @Test
  fun theHandleIsWrittenWhereExpoWillReadIt() {
    // A JSON body without `h` (the handle came top level): expo reads only the body, so it goes in there.
    val mixed = RichPush.tapExtras(h, mapOf("h" to h, "body" to """{"x":1}"""), "m")
    assertEquals(h, routedHandle(mixed))
    // A body that is not JSON: expo copies the extras, so a top-level `h` is enough.
    val plain = RichPush.tapExtras(h, mapOf("body" to "hello"), "m")
    assertEquals(h, plain["h"])
    assertEquals(h, routedHandle(plain))
    // No data at all.
    assertEquals(h, routedHandle(RichPush.tapExtras(h, emptyMap(), "m")))
  }

  @Test
  fun routedHandleModelsWhatExpoAndJsDo() {
    // Without google.message_id expo's lifecycle listener ignores the intent.
    assertNull(routedHandle(mapOf("h" to h)))
    assertNull(routedHandle(mapOf(RichPush.GOOGLE_MESSAGE_ID to "m")))
    // A JSON body hides top-level keys.
    assertNull(routedHandle(mapOf(RichPush.GOOGLE_MESSAGE_ID to "m", "h" to h, "body" to """{"x":1}""")))
    assertNull(routedHandle(mapOf(RichPush.GOOGLE_MESSAGE_ID to "m", "h" to h, "body" to "[1]")))
  }

  @Test
  fun outcomeDetailNamesTheReasonNeverContent() {
    assertEquals("reason=not_logged_in", RichPush.outcomeDetail("""{"ok":false,"reason":"not_logged_in"}"""))
    assertEquals("mode=none items=0", RichPush.outcomeDetail(reply("none")))
    assertEquals("mode=full items=1", RichPush.outcomeDetail(reply("full", item(body = "secret"))))
    assertEquals("no reply", RichPush.outcomeDetail(null))
  }

  @Test
  fun outcomeLabelsSurviveMinification() {
    assertEquals("silent", RichPush.Plan.Silent.outcome)
    assertEquals("generic", RichPush.Plan.Generic.outcome)
    assertEquals("rich", RichPush.Plan.Rich("c", true, null, emptyList()).outcome)
  }

  private fun obj(s: String): JSONObject? = try {
    JSONObject(s)
  } catch (e: Exception) {
    null
  }

  /**
   * The handle JS will route a tap by, given the intent's string extras: expo-notifications'
   * `toResponseBundleFromExtras` (JSON `body` → data, else the extras) followed by
   * `lib/push/routing.ts` `readPayloadRouting` (`data.h`). Null = the tap opens the app only.
   */
  private fun routedHandle(extras: Map<String, String>): String? {
    if (!extras.containsKey(RichPush.GOOGLE_MESSAGE_ID)) {
      // expo ignores a launch intent without it: no response at all.
      return null
    }
    val body = extras["body"]
    val envelope = body?.let { obj(it) }
    val h = if (envelope != null) {
      if (envelope.has("h") && !envelope.isNull("h")) envelope.opt("h") as? String else null
    } else if (body != null && isJson(body)) {
      // A JSON array: expo still takes it as the data, which has no `h`.
      null
    } else {
      extras["h"]
    }
    return h?.takeIf { it.isNotEmpty() }
  }

  /** expo-notifications' `isValidJSONString`: a JSON object or array. */
  private fun isJson(s: String): Boolean = obj(s) != null || try {
    org.json.JSONArray(s)
    true
  } catch (e: Exception) {
    false
  }
}
