// Phase 2: port of `src-tauri/src/commands/groups.rs` into the napi dispatch
// pattern. See pollis-node/src/lib.rs for the invoke() entry point. Each arm
// here corresponds to a single tauri::command from the legacy shim.

use napi::bindgen_prelude::*;

use crate::state::{core_err, ensure_state, json_err};

pub async fn dispatch(
    cmd: &str,
    args: &serde_json::Value,
) -> Option<Result<serde_json::Value>> {
    match cmd {
        "list_user_groups" => Some(list_user_groups(args).await),
        "list_user_groups_with_channels" => Some(list_user_groups_with_channels(args).await),
        "list_group_channels" => Some(list_group_channels(args).await),
        "create_group" => Some(create_group(args).await),
        "create_channel" => Some(create_channel(args).await),
        "send_group_invite" => Some(send_group_invite(args).await),
        "get_pending_invites" => Some(get_pending_invites(args).await),
        "accept_group_invite" => Some(accept_group_invite(args).await),
        "decline_group_invite" => Some(decline_group_invite(args).await),
        "request_group_access" => Some(request_group_access(args).await),
        "get_group_join_requests" => Some(get_group_join_requests(args).await),
        "get_my_join_request" => Some(get_my_join_request(args).await),
        "approve_join_request" => Some(approve_join_request(args).await),
        "reject_join_request" => Some(reject_join_request(args).await),
        "update_group" => Some(update_group(args).await),
        "delete_group" => Some(delete_group(args).await),
        "get_group_members" => Some(get_group_members(args).await),
        "remove_member_from_group" => Some(remove_member_from_group(args).await),
        "leave_group" => Some(leave_group(args).await),
        "update_channel" => Some(update_channel(args).await),
        "delete_channel" => Some(delete_channel(args).await),
        "set_member_role" => Some(set_member_role(args).await),
        "search_group_by_slug" => Some(search_group_by_slug(args).await),
        _ => None,
    }
}

async fn list_user_groups(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::list_user_groups(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn list_user_groups_with_channels(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::list_user_groups_with_channels(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn list_group_channels(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
    }
    let Args { group_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::list_group_channels(group_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn create_group(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        name: String,
        description: Option<String>,
        owner_id: String,
        create_default_text_channel: Option<bool>,
        create_default_voice_channel: Option<bool>,
    }
    let Args {
        name,
        description,
        owner_id,
        create_default_text_channel,
        create_default_voice_channel,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::create_group(
        name,
        description,
        owner_id,
        create_default_text_channel,
        create_default_voice_channel,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn create_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        name: String,
        description: Option<String>,
        channel_type: Option<String>,
        #[serde(rename = "_creator_id")]
        _creator_id: String,
    }
    let Args {
        group_id,
        name,
        description,
        channel_type,
        _creator_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::create_channel(
        group_id,
        name,
        description,
        channel_type,
        _creator_id,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn send_group_invite(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        inviter_id: String,
        invitee_identifier: String,
    }
    let Args {
        group_id,
        inviter_id,
        invitee_identifier,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::send_group_invite(
        group_id,
        inviter_id,
        invitee_identifier,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn get_pending_invites(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        user_id: String,
    }
    let Args { user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::get_pending_invites(user_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn accept_group_invite(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        invite_id: String,
        user_id: String,
    }
    let Args { invite_id, user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::accept_group_invite(invite_id, user_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn decline_group_invite(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        invite_id: String,
        user_id: String,
    }
    let Args { invite_id, user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::decline_group_invite(invite_id, user_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn request_group_access(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        requester_id: String,
    }
    let Args {
        group_id,
        requester_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::request_group_access(group_id, requester_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn get_group_join_requests(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        requester_id: String,
    }
    let Args {
        group_id,
        requester_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::get_group_join_requests(group_id, requester_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn get_my_join_request(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        requester_id: String,
    }
    let Args {
        group_id,
        requester_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::get_my_join_request(group_id, requester_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn approve_join_request(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        request_id: String,
        approver_id: String,
    }
    let Args {
        request_id,
        approver_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::approve_join_request(request_id, approver_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn reject_join_request(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        request_id: String,
        approver_id: String,
    }
    let Args {
        request_id,
        approver_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::reject_join_request(request_id, approver_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn update_group(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        requester_id: String,
        name: Option<String>,
        description: Option<String>,
        icon_url: Option<String>,
    }
    let Args {
        group_id,
        requester_id,
        name,
        description,
        icon_url,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::update_group(
        group_id,
        requester_id,
        name,
        description,
        icon_url,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn delete_group(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        requester_id: String,
    }
    let Args {
        group_id,
        requester_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::delete_group(group_id, requester_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn get_group_members(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
    }
    let Args { group_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::get_group_members(group_id, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn remove_member_from_group(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        user_id: String,
        requester_id: String,
    }
    let Args {
        group_id,
        user_id,
        requester_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::remove_member_from_group(
        group_id,
        user_id,
        requester_id,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn leave_group(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        user_id: String,
    }
    let Args { group_id, user_id } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::leave_group(group_id, user_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn update_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        channel_id: String,
        requester_id: String,
        name: Option<String>,
        description: Option<String>,
    }
    let Args {
        channel_id,
        requester_id,
        name,
        description,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::update_channel(
        channel_id,
        requester_id,
        name,
        description,
        &state,
    )
    .await
    .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}

async fn delete_channel(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        channel_id: String,
        requester_id: String,
    }
    let Args {
        channel_id,
        requester_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::delete_channel(channel_id, requester_id, &state)
        .await
        .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn set_member_role(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        group_id: String,
        user_id: String,
        role: String,
        requester_id: String,
    }
    let Args {
        group_id,
        user_id,
        role,
        requester_id,
    } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    pollis_core::commands::groups::set_member_role(
        group_id,
        user_id,
        role,
        requester_id,
        &state,
    )
    .await
    .map_err(core_err)?;
    Ok(serde_json::Value::Null)
}

async fn search_group_by_slug(args: &serde_json::Value) -> Result<serde_json::Value> {
    #[derive(serde::Deserialize)]
    struct Args {
        slug: String,
    }
    let Args { slug } = serde_json::from_value(args.clone()).map_err(json_err)?;
    let state = ensure_state().await?;
    let out = pollis_core::commands::groups::search_group_by_slug(slug, &state)
        .await
        .map_err(core_err)?;
    serde_json::to_value(out).map_err(json_err)
}
