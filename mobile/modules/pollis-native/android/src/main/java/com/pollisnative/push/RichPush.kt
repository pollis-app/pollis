package com.pollisnative.push

import org.json.JSONObject

/**
 * The pure half of the Android rich-push handler (#1599): which FCM messages are ours, and what the
 * core's fetch reply says to post. No Android APIs, so JVM unit tests cover it.
 */
object RichPush {
  /** A handle is the DS's 16 random bytes, base64url without padding (as `lib/push/dismiss.ts`). */
  private val HANDLE = Regex("^[A-Za-z0-9_-]{1,64}$")

  /**
   * The handle of a rich push, or null when expo-notifications should get the message unchanged.
   *
   * Ours = data-only (no FCM notification block, no Expo `title`/`message`) and carrying `h`, either
   * top level (raw FCM) or inside the Expo push service's JSON `body` envelope. A dismissal push
   * (`{d}`), today's alert push and anything else falls through.
   */
  fun richHandle(data: Map<String, String>, hasNotificationBlock: Boolean): String? {
    if (hasNotificationBlock) {
      return null
    }
    if (!data["title"].isNullOrEmpty() || !data["message"].isNullOrEmpty()) {
      return null
    }
    val h = data["h"] ?: data["body"]?.let { body ->
      try {
        val parsed = JSONObject(body)
        if (parsed.has("h") && !parsed.isNull("h")) parsed.optString("h") else null
      } catch (e: Exception) {
        null
      }
    }
    return if (h != null && HANDLE.matches(h)) h else null
  }

  enum class BodyKey { ATTACHMENT, IMAGE, VIDEO, VOICE, SENSITIVE }

  sealed class Text {
    data class Literal(val text: String) : Text()
    data class Placeholder(val key: BodyKey) : Text()
    /** Name-only mode, or nothing to preview: the generic "New message". */
    object NameOnly : Text()
  }

  data class Message(val messageId: String, val sender: String, val text: Text)

  sealed class Plan {
    /**
     * The outcome class for logs. A literal, not the class name: R8 renames these classes in a
     * release build, where `javaClass.simpleName` logs "a" / "b".
     */
    abstract val outcome: String

    /** Every item is muted: post nothing. */
    object Silent : Plan() {
      override val outcome = "silent"
    }
    /** Failure, nothing fetched, or the user's setting is "Nothing": today's text. */
    object Generic : Plan() {
      override val outcome = "generic"
    }
    data class Rich(
      val conversationId: String,
      val isDm: Boolean,
      val conversationTitle: String?,
      val messages: List<Message>,
    ) : Plan() {
      override val outcome = "rich"
    }
  }

  /**
   * True when the user is already looking at this push's conversation: the app is in the foreground
   * and that conversation is the one on screen. Posting would only flash a heads-up that #1508 then
   * clears; the open chat shows the message itself. A generic plan (conversation unknown) and any
   * other conversation still post.
   */
  fun suppressForOpenConversation(plan: Plan, foreground: Boolean, openConversationId: String?): Boolean =
    foreground && openConversationId != null && plan is Plan.Rich && plan.conversationId == openConversationId

  private fun bodyKey(raw: String?): BodyKey? = when (raw) {
    "attachment" -> BodyKey.ATTACHMENT
    "image" -> BodyKey.IMAGE
    "video" -> BodyKey.VIDEO
    "voice" -> BodyKey.VOICE
    "sensitive" -> BodyKey.SENSITIVE
    else -> null
  }

  private fun JSONObject.str(key: String): String? =
    if (has(key) && !isNull(key)) optString(key).takeIf { it.isNotEmpty() } else null

  /** What to post for one push, from `pollis_notify_fetch`'s JSON (pollis-core/include/pollis_notify.h). */
  fun plan(json: String?): Plan {
    val root = try {
      JSONObject(json ?: return Plan.Generic)
    } catch (e: Exception) {
      return Plan.Generic
    }
    if (!root.optBoolean("ok", false)) {
      return Plan.Generic
    }
    val mode = root.optString("mode", "full")
    if (mode == "none") {
      return Plan.Generic
    }
    val items = root.optJSONArray("items") ?: return Plan.Generic
    if (items.length() == 0) {
      return Plan.Generic
    }
    data class Item(val obj: JSONObject, val conversationId: String)
    val visible = ArrayList<Item>()
    var muted = 0
    for (i in 0 until items.length()) {
      val obj = items.optJSONObject(i) ?: continue
      if (obj.optBoolean("muted", false)) {
        muted += 1
        continue
      }
      val conversationId = obj.str("conversation_id") ?: continue
      visible.add(Item(obj, conversationId))
    }
    if (visible.isEmpty()) {
      // Only when everything fetched is muted; an unreadable item never counts as muted.
      return if (muted == items.length()) Plan.Silent else Plan.Generic
    }
    // One notification per push, for the newest message's conversation.
    val latest = visible.last()
    val sameConversation = visible.filter { it.conversationId == latest.conversationId }
    val messages = sameConversation.mapNotNull { item ->
      val sender = item.obj.str("sender_name") ?: return@mapNotNull null
      val key = bodyKey(item.obj.str("body_key"))
      val body = item.obj.str("body")
      val text = when {
        mode == "name" -> Text.NameOnly
        key != null -> Text.Placeholder(key)
        body != null && body.isNotBlank() -> Text.Literal(body)
        else -> Text.NameOnly
      }
      Message(item.obj.str("message_id") ?: "", sender, text)
    }
    if (messages.isEmpty()) {
      return Plan.Generic
    }
    val isDm = latest.obj.optBoolean("is_dm", false)
    return Plan.Rich(
      conversationId = latest.conversationId,
      isDm = isDm,
      conversationTitle = if (isDm) null else latest.obj.str("conversation_title"),
      messages = messages,
    )
  }
}
