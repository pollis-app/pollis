package com.pollisnative.push

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import androidx.core.app.Person
import androidx.core.graphics.drawable.IconCompat
import androidx.core.content.ContextCompat
import com.pollisnative.R

/**
 * Posts what [RichPush.plan] decided (#1599). Every notification goes on channel `default` with
 * tag = the push handle `h` and id 0 (what FCM uses for a system-posted one), so the #1508
 * dismissal paths (`lib/push/dismiss.ts`) find and clear it exactly as before.
 */
class RichPushNotifier(private val context: Context) {
  companion object {
    const val CHANNEL_ID = "default"
    const val NOTIFICATION_ID = 0
    /** Local-only: which conversation a posted notification is for, so the next one folds it in. */
    const val EXTRA_CONVERSATION = "pollis.conversation"
    private const val EXTRA_MESSAGE_ID = "pollis.message_id"
    private const val MAX_STACKED = 7
  }

  private val manager = NotificationManagerCompat.from(context)

  fun post(handle: String, plan: RichPush.Plan, data: Map<String, String>, messageId: String?, sentTime: Long) {
    when (plan) {
      RichPush.Plan.Silent -> manager.cancel(handle, NOTIFICATION_ID)
      RichPush.Plan.Generic -> notify(handle, generic(handle, data, messageId, sentTime).build())
      is RichPush.Plan.Rich -> notify(handle, rich(handle, plan, data, messageId, sentTime))
    }
  }

  private fun notify(handle: String, notification: android.app.Notification) {
    if (!manager.areNotificationsEnabled()) {
      return
    }
    ensureChannel()
    try {
      manager.notify(handle, NOTIFICATION_ID, notification)
    } catch (e: SecurityException) {
      // POST_NOTIFICATIONS revoked between the check and the post.
    }
  }

  /** JS creates it at startup (`lib/push/index.ts`); recreate with the same settings if it is gone. */
  private fun ensureChannel() {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) {
      return
    }
    val system = context.getSystemService(NotificationManager::class.java) ?: return
    if (system.getNotificationChannel(CHANNEL_ID) != null) {
      return
    }
    val channel = NotificationChannel(
      CHANNEL_ID,
      context.getString(R.string.pollis_push_channel_messages),
      NotificationManager.IMPORTANCE_DEFAULT,
    )
    channel.setShowBadge(false)
    system.createNotificationChannel(channel)
  }

  private fun base(handle: String, data: Map<String, String>, messageId: String?, sentTime: Long): NotificationCompat.Builder {
    val builder = NotificationCompat.Builder(context, CHANNEL_ID)
      .setSmallIcon(smallIcon())
      .setAutoCancel(true)
      .setCategory(NotificationCompat.CATEGORY_MESSAGE)
      .setPriority(NotificationCompat.PRIORITY_DEFAULT)
      .setWhen(if (sentTime > 0) sentTime else System.currentTimeMillis())
      .setShowWhen(true)
      // A late rich result replaces the generic one in place without alerting twice.
      .setOnlyAlertOnce(true)
      .setContentIntent(tapIntent(handle, data, messageId, sentTime))
    accentColor()?.let { builder.setColor(it) }
    return builder
  }

  private fun generic(handle: String, data: Map<String, String>, messageId: String?, sentTime: Long): NotificationCompat.Builder =
    base(handle, data, messageId, sentTime)
      .setContentTitle(context.getString(R.string.pollis_push_generic_title))
      .setContentText(context.getString(R.string.pollis_push_generic_body))

  private fun text(t: RichPush.Text): String = when (t) {
    is RichPush.Text.Literal -> t.text
    RichPush.Text.NameOnly -> context.getString(R.string.pollis_push_name_only_body)
    is RichPush.Text.Placeholder -> when (t.key) {
      RichPush.BodyKey.ATTACHMENT -> "📎 " + context.getString(R.string.pollis_push_body_attachment)
      RichPush.BodyKey.IMAGE -> "📷 " + context.getString(R.string.pollis_push_body_image)
      RichPush.BodyKey.VIDEO -> "🎥 " + context.getString(R.string.pollis_push_body_video)
      RichPush.BodyKey.VOICE -> "🎤 " + context.getString(R.string.pollis_push_body_voice)
      RichPush.BodyKey.SENSITIVE -> context.getString(R.string.pollis_push_body_sensitive)
    }
  }

  private fun rich(handle: String, plan: RichPush.Plan.Rich, data: Map<String, String>, messageId: String?, sentTime: Long): android.app.Notification {
    val me = Person.Builder().setName(context.getString(R.string.pollis_push_you)).build()
    val style = NotificationCompat.MessagingStyle(me)
      .setGroupConversation(!plan.isDm)
    if (!plan.isDm && plan.conversationTitle != null) {
      style.setConversationTitle(plan.conversationTitle)
    }
    val now = if (sentTime > 0) sentTime else System.currentTimeMillis()
    // One notification per conversation, like Signal: fold in the one already showing, then
    // replace it. The newest carries the newest handle, which #1508 clears once that conversation
    // is read (a read clears every handle up to the cursor).
    val previous = takePreviousFor(plan.conversationId, handle)
    // The core may hand back, once, the newest message the app already had: never stack it twice.
    val seen = HashSet<String>()
    val stacked = ArrayList<NotificationCompat.MessagingStyle.Message>()
    for (m in previous) {
      m.extras.getString(EXTRA_MESSAGE_ID)?.let { seen.add(it) }
      stacked.add(m)
    }
    // The app's own letter avatar, drawn locally: no network, a few ms per distinct sender.
    val people = HashMap<String, Person>()
    for (m in plan.messages) {
      if (m.messageId.isNotEmpty() && !seen.add(m.messageId)) {
        continue
      }
      val sender = people.getOrPut(m.sender) { person(m.sender) }
      val message = NotificationCompat.MessagingStyle.Message(text(m.text), now, sender)
      if (m.messageId.isNotEmpty()) {
        message.extras.putString(EXTRA_MESSAGE_ID, m.messageId)
      }
      stacked.add(message)
    }
    for (m in stacked.takeLast(MAX_STACKED)) {
      style.addMessage(m)
    }
    val last = plan.messages.last()
    val extras = Bundle().apply { putString(EXTRA_CONVERSATION, plan.conversationId) }
    val builder = base(handle, data, messageId, sentTime)
    // The conversation's icon: a group's rounded letter avatar (as Search draws groups), or the
    // DM sender's round one. Collapsed and grouped rows show only this, not the Person icons.
    val icon = if (!plan.isDm && plan.conversationTitle != null) {
      safeAvatar(plan.conversationTitle, rounded = true)
    } else {
      safeAvatar(last.sender, rounded = false)
    }
    icon?.let { builder.setLargeIcon(it) }
    return builder
      .setStyle(style)
      .setContentTitle(
        if (plan.isDm || plan.conversationTitle == null) {
          last.sender
        } else {
          context.getString(R.string.pollis_push_group_title, last.sender, plan.conversationTitle)
        },
      )
      .setContentText(text(last.text))
      .addExtras(extras)
      .build()
  }

  private fun safeAvatar(label: String, rounded: Boolean): android.graphics.Bitmap? = try {
    LetterAvatar.bitmap(context, label, rounded)
  } catch (e: Exception) {
    null
  }

  private fun person(name: String): Person {
    val builder = Person.Builder().setName(name)
    safeAvatar(name, rounded = false)?.let { builder.setIcon(IconCompat.createWithBitmap(it)) }
    return builder.build()
  }

  /** Messages of the notification already showing for this conversation, which is then removed. */
  private fun takePreviousFor(conversationId: String, handle: String): List<NotificationCompat.MessagingStyle.Message> {
    if (Build.VERSION.SDK_INT < Build.VERSION_CODES.M) {
      return emptyList()
    }
    val system = context.getSystemService(NotificationManager::class.java) ?: return emptyList()
    val active = try {
      system.activeNotifications
    } catch (e: Exception) {
      return emptyList()
    }
    for (sbn in active) {
      val tag = sbn.tag ?: continue
      if (tag == handle || sbn.id != NOTIFICATION_ID) {
        continue
      }
      if (sbn.notification.extras?.getString(EXTRA_CONVERSATION) != conversationId) {
        continue
      }
      val style = NotificationCompat.MessagingStyle.extractMessagingStyleFromNotification(sbn.notification)
      manager.cancel(tag, NOTIFICATION_ID)
      return style?.messages?.toList() ?: emptyList()
    }
    return emptyList()
  }

  /**
   * The ONE tap intent, shared by every notification posted here (rich, name-only, generic, the
   * deadline fallback and its late replacement), so all of them route alike (#1614). It opens
   * [PushTapActivity] with the push's data plus `google.message_id`, the extras FCM's own system
   * notification carries; that activity gives them to expo-notifications, which turns them into
   * the tap response JS routes by `h`, cold or warm.
   */
  private fun tapIntent(handle: String, data: Map<String, String>, messageId: String?, sentTime: Long): PendingIntent {
    val tap = Intent(context, PushTapActivity::class.java)
      .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION)
    for ((k, v) in RichPush.tapExtras(handle, data, messageId)) {
      tap.putExtra(k, v)
    }
    tap.putExtra(RichPush.GOOGLE_SENT_TIME, sentTime)
    return PendingIntent.getActivity(
      context,
      handle.hashCode(),
      tap,
      PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
    )
  }

  private fun metaData(): Bundle? = try {
    context.packageManager.getApplicationInfo(context.packageName, PackageManager.GET_META_DATA).metaData
  } catch (e: Exception) {
    null
  }

  /**
   * The app's monochrome notification glyph: the `notification_icon` drawable the expo-notifications
   * plugin's `icon` option generates; else whatever FCM itself would use for a system-posted one.
   */
  private fun smallIcon(): Int {
    val glyph = context.resources.getIdentifier("notification_icon", "drawable", context.packageName)
    if (glyph != 0) {
      return glyph
    }
    val meta = metaData()
    val configured = meta?.getInt("com.google.firebase.messaging.default_notification_icon", 0) ?: 0
    if (configured != 0) {
      return configured
    }
    return context.applicationInfo.icon
  }

  private fun accentColor(): Int? {
    val res = metaData()?.getInt("com.google.firebase.messaging.default_notification_color", 0) ?: 0
    if (res == 0) {
      return null
    }
    return try {
      ContextCompat.getColor(context, res)
    } catch (e: Exception) {
      null
    }
  }
}
