mod applications;
mod auth;
mod bans;
pub mod channels;
mod emojis;
mod gateway;
mod health;
mod interactions;
mod invites;
mod members;
pub mod messages;
mod reactions;
mod roles;
mod sfu;
mod soundboard;
pub mod spaces;
mod test_seed;
mod users;
mod voice;

use axum::middleware as axum_mw;
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::middleware::rate_limit::rate_limit_middleware;
use crate::state::AppState;

/// Build the full application router. Consumes the state so middleware
/// layers that need `State<AppState>` (e.g. rate limiter) can be wired up.
pub fn router(state: AppState) -> Router {
    let api = api_routes(&state);
    let cdn_service = ServeDir::new(&state.storage_path);

    Router::new()
        .route("/health", get(health::health))
        .route("/ws", get(crate::gateway::ws_upgrade))
        .route("/test/seed", post(test_seed::seed))
        .nest_service("/cdn", cdn_service)
        .nest("/api/v1", api)
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive())
        .with_state(state)
}

fn api_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        // Auth (register/login are public, logout requires auth)
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        // Gateway (public, no auth needed)
        .route("/gateway", get(gateway::get_gateway))
        // Users
        .route(
            "/users/@me",
            get(users::get_current_user).patch(users::update_current_user),
        )
        .route("/users/@me/spaces", get(users::get_current_user_spaces))
        .route("/users/@me/channels", get(users::get_current_user_channels))
        .route("/users/{user_id}", get(users::get_user))
        // Spaces
        .route("/spaces/public", get(spaces::list_public_spaces))
        .route("/spaces", post(spaces::create_space))
        .route(
            "/spaces/{space_id}",
            get(spaces::get_space)
                .patch(spaces::update_space)
                .delete(spaces::delete_space),
        )
        .route(
            "/spaces/{space_id}/channels",
            get(spaces::list_channels)
                .post(spaces::create_channel)
                .patch(spaces::reorder_channels),
        )
        // Members
        .route("/spaces/{space_id}/members", get(members::list_members))
        .route(
            "/spaces/{space_id}/members/search",
            get(members::search_members),
        )
        .route(
            "/spaces/{space_id}/members/@me",
            patch(members::update_own_member),
        )
        .route(
            "/spaces/{space_id}/members/{user_id}",
            get(members::get_member)
                .patch(members::update_member)
                .delete(members::kick_member),
        )
        .route(
            "/spaces/{space_id}/members/{user_id}/roles/{role_id}",
            put(members::add_role).delete(members::remove_role),
        )
        // Message search
        .route(
            "/spaces/{space_id}/messages/search",
            get(messages::search_messages),
        )
        // Bans
        .route("/spaces/{space_id}/bans", get(bans::list_bans))
        .route(
            "/spaces/{space_id}/bans/{user_id}",
            get(bans::get_ban)
                .put(bans::create_ban)
                .delete(bans::delete_ban),
        )
        // Roles
        .route(
            "/spaces/{space_id}/roles",
            get(roles::list_roles)
                .post(roles::create_role)
                .patch(roles::reorder_roles),
        )
        .route(
            "/spaces/{space_id}/roles/{role_id}",
            patch(roles::update_role).delete(roles::delete_role),
        )
        // Channels
        .route(
            "/channels/{channel_id}",
            get(channels::get_channel)
                .patch(channels::update_channel)
                .delete(channels::delete_channel),
        )
        .route(
            "/channels/{channel_id}/permissions",
            get(channels::list_overwrites),
        )
        .route(
            "/channels/{channel_id}/permissions/{overwrite_id}",
            put(channels::upsert_overwrite).delete(channels::delete_overwrite),
        )
        // Messages
        .route(
            "/channels/{channel_id}/messages",
            get(messages::list_messages).post(messages::create_message),
        )
        .route(
            "/channels/{channel_id}/messages/upload",
            post(messages::create_message_multipart),
        )
        .route(
            "/channels/{channel_id}/messages/{message_id}",
            get(messages::get_message)
                .patch(messages::update_message)
                .delete(messages::delete_message),
        )
        .route(
            "/channels/{channel_id}/messages/bulk-delete",
            post(messages::bulk_delete_messages),
        )
        .route("/channels/{channel_id}/pins", get(messages::list_pins))
        .route(
            "/channels/{channel_id}/pins/{message_id}",
            put(messages::pin_message).delete(messages::unpin_message),
        )
        .route(
            "/channels/{channel_id}/typing",
            post(messages::typing_indicator),
        )
        // Reactions
        .route(
            "/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/@me",
            put(reactions::add_reaction).delete(reactions::remove_own_reaction),
        )
        .route(
            "/channels/{channel_id}/messages/{message_id}/reactions/{emoji}/{user_id}",
            delete(reactions::remove_user_reaction),
        )
        .route(
            "/channels/{channel_id}/messages/{message_id}/reactions/{emoji}",
            get(reactions::list_reactions).delete(reactions::remove_all_reactions_emoji),
        )
        .route(
            "/channels/{channel_id}/messages/{message_id}/reactions",
            delete(reactions::remove_all_reactions),
        )
        // Invites
        .route(
            "/invites/{code}",
            get(invites::get_invite).delete(invites::delete_invite),
        )
        .route("/invites/{code}/accept", post(invites::accept_invite))
        .route(
            "/spaces/{space_id}/invites",
            get(invites::list_space_invites).post(invites::create_space_invite),
        )
        .route("/spaces/{space_id}/join", post(spaces::join_public_space))
        .route(
            "/channels/{channel_id}/invites",
            get(invites::list_channel_invites).post(invites::create_channel_invite),
        )
        // Emojis
        .route(
            "/spaces/{space_id}/emojis",
            get(emojis::list_emojis).post(emojis::create_emoji),
        )
        .route(
            "/spaces/{space_id}/emojis/{emoji_id}",
            get(emojis::get_emoji)
                .patch(emojis::update_emoji)
                .delete(emojis::delete_emoji),
        )
        // Soundboard
        .route(
            "/spaces/{space_id}/soundboard",
            get(soundboard::list_sounds).post(soundboard::create_sound),
        )
        .route(
            "/spaces/{space_id}/soundboard/{sound_id}",
            get(soundboard::get_sound)
                .patch(soundboard::update_sound)
                .delete(soundboard::delete_sound),
        )
        .route(
            "/spaces/{space_id}/soundboard/{sound_id}/play",
            post(soundboard::play_sound),
        )
        // Voice
        .route("/voice/info", get(voice::voice_info))
        .route(
            "/spaces/{space_id}/voice-regions",
            get(voice::list_voice_regions),
        )
        .route(
            "/channels/{channel_id}/voice-status",
            get(voice::get_voice_status),
        )
        .route("/channels/{channel_id}/voice/join", post(voice::join_voice))
        .route(
            "/channels/{channel_id}/voice/leave",
            delete(voice::leave_voice),
        )
        // SFU node management (internal/admin)
        .route("/sfu/nodes", post(sfu::register_node).get(sfu::list_nodes))
        .route("/sfu/nodes/{node_id}/heartbeat", post(sfu::heartbeat))
        .route("/sfu/nodes/{node_id}", delete(sfu::deregister_node))
        // Applications
        .route("/applications", post(applications::create_application))
        .route(
            "/applications/@me",
            get(applications::get_current_application)
                .patch(applications::update_current_application),
        )
        .route(
            "/applications/@me/reset-token",
            post(applications::reset_token),
        )
        // Interactions (stubs)
        .route(
            "/applications/{app_id}/commands",
            get(interactions::list_global_commands).post(interactions::create_global_command),
        )
        .route(
            "/interactions/{interaction_id}/{token}/callback",
            post(interactions::interaction_callback),
        )
        // Version
        .route("/version", get(health::version))
        // Gateway info (authenticated)
        .route("/gateway/bot", get(gateway::get_gateway_bot))
        // Rate limit on all API routes
        .layer(axum_mw::from_fn_with_state(
            state.clone(),
            rate_limit_middleware,
        ))
}
