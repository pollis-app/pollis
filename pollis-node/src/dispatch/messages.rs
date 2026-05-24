// Port of `src-tauri/src/commands/messages.rs`. All sixteen commands are
// pure CRUD/MLS — no Channel<T>, no AppHandle, no Window. Maps 1:1 onto the
// `pollis_core::commands::messages::*` re-exports.

use napi::bindgen_prelude::*;

use pollis_core::commands::messages::MessageCursor;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "list_messages" => Some(list_messages(args).await),
        "send_message" => Some(send_message(args).await),
        "get_channel_messages" => Some(get_channel_messages(args).await),
        "get_dm_messages" => Some(get_dm_messages(args).await),
        "read_channel_messages" => Some(read_channel_messages(args).await),
        "read_dm_messages" => Some(read_dm_messages(args).await),
        "ingest_channel_envelopes" => Some(ingest_channel_envelopes(args).await),
        "ingest_dm_envelopes" => Some(ingest_dm_envelopes(args).await),
        "list_messages_by_sender" => Some(list_messages_by_sender(args).await),
        "list_channel_previews" => Some(list_channel_previews(args).await),
        "search_messages" => Some(search_messages(args).await),
        "add_reaction" => Some(add_reaction(args).await),
        "remove_reaction" => Some(remove_reaction(args).await),
        "get_reactions" => Some(get_reactions(args).await),
        "delete_message" => Some(delete_message(args).await),
        "edit_message" => Some(edit_message(args).await),
        _ => None,
    }
}

async fn list_messages(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        conversation_id: String,
        limit: Option<i64>,
        before_id: Option<String>,
    }
    let Args {
        conversation_id,
        limit,
        before_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::list_messages(
        conversation_id,
        limit,
        before_id,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn send_message(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        conversation_id: String,
        sender_id: String,
        content: String,
        reply_to_id: Option<String>,
        sender_username: Option<String>,
    }
    let Args {
        conversation_id,
        sender_id,
        content,
        reply_to_id,
        sender_username,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::send_message(
        conversation_id,
        sender_id,
        content,
        reply_to_id,
        sender_username,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_channel_messages(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        channel_id: String,
        limit: Option<i64>,
        cursor: Option<MessageCursor>,
    }
    let Args {
        user_id,
        channel_id,
        limit,
        cursor,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::get_channel_messages(
        user_id, channel_id, limit, cursor, &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_dm_messages(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        dm_channel_id: String,
        limit: Option<i64>,
        cursor: Option<MessageCursor>,
    }
    let Args {
        user_id,
        dm_channel_id,
        limit,
        cursor,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::get_dm_messages(
        user_id,
        dm_channel_id,
        limit,
        cursor,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn read_channel_messages(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        channel_id: String,
        limit: Option<i64>,
        cursor: Option<MessageCursor>,
    }
    let Args {
        channel_id,
        limit,
        cursor,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::read_channel_messages(
        channel_id, limit, cursor, &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn read_dm_messages(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        dm_channel_id: String,
        limit: Option<i64>,
        cursor: Option<MessageCursor>,
    }
    let Args {
        dm_channel_id,
        limit,
        cursor,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::read_dm_messages(
        dm_channel_id,
        limit,
        cursor,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn ingest_channel_envelopes(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        channel_id: String,
    }
    let Args { user_id, channel_id } =
        serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::messages::ingest_channel_envelopes(user_id, channel_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn ingest_dm_envelopes(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
        dm_channel_id: String,
    }
    let Args {
        user_id,
        dm_channel_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::messages::ingest_dm_envelopes(user_id, dm_channel_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn list_messages_by_sender(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        sender_id: String,
    }
    let Args { sender_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::list_messages_by_sender(sender_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn list_channel_previews(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::list_channel_previews(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn search_messages(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        query: String,
        limit: Option<i64>,
    }
    let Args { query, limit } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::search_messages(query, limit, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn add_reaction(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        message_id: String,
        user_id: String,
        emoji: String,
    }
    let Args {
        message_id,
        user_id,
        emoji,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::messages::add_reaction(message_id, user_id, emoji, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn remove_reaction(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        message_id: String,
        user_id: String,
        emoji: String,
    }
    let Args {
        message_id,
        user_id,
        emoji,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::messages::remove_reaction(message_id, user_id, emoji, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn get_reactions(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        message_id: String,
    }
    let Args { message_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::messages::get_reactions(message_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn delete_message(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        message_id: String,
        user_id: String,
    }
    let Args { message_id, user_id } =
        serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::messages::delete_message(message_id, user_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn edit_message(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        conversation_id: String,
        message_id: String,
        user_id: String,
        new_content: String,
    }
    let Args {
        conversation_id,
        message_id,
        user_id,
        new_content,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::messages::edit_message(
        conversation_id,
        message_id,
        user_id,
        new_content,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}
