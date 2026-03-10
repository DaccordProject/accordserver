//! `accord-seed` — populate an Accord database with realistic example data.
//!
//! Usage:
//!   DATABASE_URL=sqlite:data/accord.db?mode=rwc cargo run --bin accord-seed
//!
//! Idempotent: skips seeding if user "alice" already exists.

use std::error::Error;

use argon2::password_hash::rand_core::OsRng;
use argon2::password_hash::SaltString;
use argon2::{Argon2, PasswordHasher};
use sha2::{Digest, Sha256};

use accordserver::db;
use accordserver::models::channel::CreateChannel;
use accordserver::models::invite::CreateInvite;
use accordserver::models::message::CreateMessage;
use accordserver::models::role::CreateRole;
use accordserver::models::space::CreateSpace;
use accordserver::snowflake;

/// Hash a password with the same Argon2id params used by the auth routes.
fn hash_password(password: &str) -> Result<String, Box<dyn Error>> {
    let salt = SaltString::generate(&mut OsRng);
    let params = argon2::Params::new(19456, 3, 1, None)
        .map_err(|e| format!("argon2 params: {e}"))?;
    let argon2 = Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let hash = argon2
        .hash_password(password.as_bytes(), &salt)
        .map_err(|e| format!("password hash: {e}"))?
        .to_string();
    Ok(hash)
}

/// Generate a bearer token and insert it into user_tokens.
async fn create_bearer_token(
    pool: &sqlx::AnyPool,
    user_id: &str,
) -> Result<String, Box<dyn Error>> {
    use rand::Rng;
    let mut rng = rand::thread_rng();
    let a: u128 = rng.gen();
    let b: u128 = rng.gen();
    let token = format!("{a:032x}{b:032x}");

    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let token_hash = format!("{:x}", hasher.finalize());

    let expires_at = (chrono::Utc::now() + chrono::Duration::days(365))
        .format("%Y-%m-%dT%H:%M:%S")
        .to_string();

    sqlx::query("INSERT INTO user_tokens (token_hash, user_id, expires_at) VALUES (?, ?, ?)")
        .bind(&token_hash)
        .bind(user_id)
        .bind(&expires_at)
        .execute(pool)
        .await?;

    Ok(token)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let database_url =
        std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite:data/accord.db?mode=rwc".into());

    println!("accord-seed: connecting to {database_url}");
    // Ensure data directory exists (matches server startup behaviour)
    std::fs::create_dir_all("data").ok();
    let is_postgres = db::url_is_postgres(&database_url);
    let pool = db::create_pool(&database_url).await?;

    // ── Idempotency check ──────────────────────────────────────────
    let existing: Option<(String,)> =
        sqlx::query_as("SELECT id FROM users WHERE username = 'alice' AND bot = false")
            .fetch_optional(&pool)
            .await?;
    if existing.is_some() {
        println!("accord-seed: database already seeded (user 'alice' exists). Skipping.");
        return Ok(());
    }

    println!("accord-seed: seeding database...");

    // ── Users ──────────────────────────────────────────────────────
    struct SeedUser {
        username: &'static str,
        display_name: &'static str,
        is_admin: bool,
    }

    let seed_users = [
        SeedUser { username: "alice", display_name: "Alice", is_admin: true },
        SeedUser { username: "bob", display_name: "Bob", is_admin: false },
        SeedUser { username: "charlie", display_name: "Charlie", is_admin: false },
        SeedUser { username: "diana", display_name: "Diana", is_admin: false },
        SeedUser { username: "eve", display_name: "Eve", is_admin: false },
    ];

    let password_hash = hash_password("password")?;
    let mut user_ids: Vec<String> = Vec::new();

    for su in &seed_users {
        let id = snowflake::generate();
        sqlx::query(
            "INSERT INTO users (id, username, display_name, password_hash, is_admin) VALUES (?, ?, ?, ?, ?)"
        )
        .bind(&id)
        .bind(su.username)
        .bind(su.display_name)
        .bind(&password_hash)
        .bind(su.is_admin)
        .execute(&pool)
        .await?;

        create_bearer_token(&pool, &id).await?;
        user_ids.push(id);
    }

    let alice = &user_ids[0];
    let bob = &user_ids[1];
    let charlie = &user_ids[2];
    let diana = &user_ids[3];
    let eve = &user_ids[4];

    println!("  created 5 users (alice, bob, charlie, diana, eve)");

    // ── Space ──────────────────────────────────────────────────────
    let space = db::spaces::create_space(
        &pool,
        alice,
        &CreateSpace {
            name: "Accord Community".to_string(),
            slug: Some("accord-community".to_string()),
            description: Some("The official Accord community space. Welcome!".to_string()),
            public: Some(true),
        },
    )
    .await?;
    let space_id = &space.id;

    println!("  created space: Accord Community ({})", space_id);

    // Add all users as members (alice is already a member from create_space)
    for uid in &user_ids[1..] {
        db::members::add_member(&pool, space_id, uid, is_postgres).await?;
    }

    // ── Roles ──────────────────────────────────────────────────────
    // create_space already created: @everyone (pos 0), Moderator (pos 1), Admin (pos 2)
    // Fetch existing roles to get their IDs
    let roles = db::roles::list_roles(&pool, space_id).await?;
    let moderator_role_id = roles.iter().find(|r| r.name == "Moderator").map(|r| r.id.clone())
        .expect("Moderator role should exist");
    let _admin_role_id = roles.iter().find(|r| r.name == "Admin").map(|r| r.id.clone())
        .expect("Admin role should exist");

    // Create Developer role (green, hoisted)
    let developer_role = db::roles::create_role(
        &pool,
        space_id,
        &CreateRole {
            name: "Developer".to_string(),
            color: Some(3066993), // green
            hoist: Some(true),
            permissions: Some(vec![
                "send_messages".to_string(),
                "embed_links".to_string(),
                "attach_files".to_string(),
                "read_message_history".to_string(),
            ]),
            mentionable: Some(true),
        },
    )
    .await?;

    // Create Artist role (purple)
    let artist_role = db::roles::create_role(
        &pool,
        space_id,
        &CreateRole {
            name: "Artist".to_string(),
            color: Some(10181046), // purple
            hoist: Some(false),
            permissions: Some(vec![
                "send_messages".to_string(),
                "embed_links".to_string(),
                "attach_files".to_string(),
                "read_message_history".to_string(),
            ]),
            mentionable: Some(true),
        },
    )
    .await?;

    println!("  created 2 custom roles: Developer, Artist");

    // Role assignments: bob=Moderator, charlie=Developer, diana=Artist+Developer
    db::members::add_role_to_member(&pool, space_id, bob, &moderator_role_id).await?;
    db::members::add_role_to_member(&pool, space_id, charlie, &developer_role.id).await?;
    db::members::add_role_to_member(&pool, space_id, diana, &artist_role.id).await?;
    db::members::add_role_to_member(&pool, space_id, diana, &developer_role.id).await?;

    // ── Delete auto-created #general channel ───────────────────────
    // create_space makes a default #general — we'll recreate it under a category
    let channels = db::channels::list_channels_in_space(&pool, space_id).await?;
    if let Some(auto_general) = channels.iter().find(|c| c.name.as_deref() == Some("general")) {
        db::channels::delete_channel(&pool, &auto_general.id).await?;
    }

    // ── Categories ─────────────────────────────────────────────────
    let cat_info = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "Information".to_string(),
            channel_type: "category".to_string(),
            topic: None,
            parent_id: None,
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(0),
        },
    )
    .await?;

    let cat_general = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "General".to_string(),
            channel_type: "category".to_string(),
            topic: None,
            parent_id: None,
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(1),
        },
    )
    .await?;

    let cat_voice = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "Voice".to_string(),
            channel_type: "category".to_string(),
            topic: None,
            parent_id: None,
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(2),
        },
    )
    .await?;

    let cat_dev = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "Development".to_string(),
            channel_type: "category".to_string(),
            topic: None,
            parent_id: None,
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(3),
        },
    )
    .await?;

    println!("  created 4 categories");

    // ── Text channels ──────────────────────────────────────────────
    let ch_welcome = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "welcome".to_string(),
            channel_type: "text".to_string(),
            topic: Some("Welcome to the Accord Community!".to_string()),
            parent_id: Some(cat_info.id.clone()),
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(0),
        },
    )
    .await?;

    let ch_rules = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "rules".to_string(),
            channel_type: "text".to_string(),
            topic: Some("Please read and follow our community rules".to_string()),
            parent_id: Some(cat_info.id.clone()),
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(1),
        },
    )
    .await?;

    let ch_announcements = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "announcements".to_string(),
            channel_type: "text".to_string(),
            topic: Some("Official announcements and updates".to_string()),
            parent_id: Some(cat_info.id.clone()),
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(2),
        },
    )
    .await?;

    let ch_general = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "general".to_string(),
            channel_type: "text".to_string(),
            topic: Some("General chat — anything goes!".to_string()),
            parent_id: Some(cat_general.id.clone()),
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(0),
        },
    )
    .await?;

    let ch_offtopic = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "off-topic".to_string(),
            channel_type: "text".to_string(),
            topic: Some("Random conversations and fun".to_string()),
            parent_id: Some(cat_general.id.clone()),
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(1),
        },
    )
    .await?;

    let ch_intros = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "introductions".to_string(),
            channel_type: "text".to_string(),
            topic: Some("Introduce yourself to the community!".to_string()),
            parent_id: Some(cat_general.id.clone()),
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(2),
        },
    )
    .await?;

    let ch_programming = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "programming".to_string(),
            channel_type: "text".to_string(),
            topic: Some("Code discussions, snippets, and help".to_string()),
            parent_id: Some(cat_dev.id.clone()),
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(0),
        },
    )
    .await?;

    let ch_help = db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "help".to_string(),
            channel_type: "text".to_string(),
            topic: Some("Need help? Ask here!".to_string()),
            parent_id: Some(cat_dev.id.clone()),
            nsfw: None,
            bitrate: None,
            user_limit: None,
            rate_limit: None,
            position: Some(1),
        },
    )
    .await?;

    println!("  created 8 text channels");

    // ── Voice channels ─────────────────────────────────────────────
    db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "General Voice".to_string(),
            channel_type: "voice".to_string(),
            topic: None,
            parent_id: Some(cat_voice.id.clone()),
            nsfw: None,
            bitrate: Some(64000),
            user_limit: None,
            rate_limit: None,
            position: Some(0),
        },
    )
    .await?;

    db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "Gaming".to_string(),
            channel_type: "voice".to_string(),
            topic: None,
            parent_id: Some(cat_voice.id.clone()),
            nsfw: None,
            bitrate: Some(64000),
            user_limit: Some(10),
            rate_limit: None,
            position: Some(1),
        },
    )
    .await?;

    db::channels::create_channel(
        &pool,
        space_id,
        &CreateChannel {
            name: "Music".to_string(),
            channel_type: "voice".to_string(),
            topic: None,
            parent_id: Some(cat_voice.id.clone()),
            nsfw: None,
            bitrate: Some(96000),
            user_limit: None,
            rate_limit: None,
            position: Some(2),
        },
    )
    .await?;

    println!("  created 3 voice channels");

    // ── Helper to create a message and return its ID ───────────────
    async fn msg(
        pool: &sqlx::AnyPool,
        channel_id: &str,
        author_id: &str,
        space_id: &str,
        content: &str,
        reply_to: Option<&str>,
        thread_id: Option<&str>,
    ) -> Result<String, Box<dyn Error>> {
        let row = db::messages::create_message(
            pool,
            channel_id,
            author_id,
            Some(space_id),
            &CreateMessage {
                content: content.to_string(),
                tts: None,
                embeds: None,
                reply_to: reply_to.map(|s| s.to_string()),
                thread_id: thread_id.map(|s| s.to_string()),
            },
        )
        .await?;
        // Small delay so snowflake IDs are properly ordered
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        Ok(row.id)
    }

    // ── Messages: #welcome ─────────────────────────────────────────
    let welcome_msg = msg(
        &pool, &ch_welcome.id, alice, space_id,
        "Welcome to the Accord Community! We're glad you're here. This is a space for everyone interested in Accord — whether you're a developer, artist, or just curious. Feel free to explore the channels and say hi!",
        None, None,
    ).await?;

    msg(&pool, &ch_welcome.id, bob, space_id,
        "Hey everyone! Excited to be part of this community.",
        None, None).await?;

    msg(&pool, &ch_welcome.id, charlie, space_id,
        "This is awesome. Can't wait to start building things!",
        None, None).await?;

    msg(&pool, &ch_welcome.id, diana, space_id,
        "Hello! I'm Diana, I do art and some programming. Nice to meet you all!",
        None, None).await?;

    msg(&pool, &ch_welcome.id, eve, space_id,
        "Hi! Just joined. Looking forward to learning more about Accord.",
        None, None).await?;

    // ── Messages: #rules ───────────────────────────────────────────
    let rules_msg = msg(
        &pool, &ch_rules.id, alice, space_id,
        "**Community Rules**\n\n1. Be respectful to all members\n2. No spam or self-promotion\n3. Keep discussions on-topic in each channel\n4. No NSFW content\n5. Follow the Accord Terms of Service\n6. Have fun and help each other out!\n\nBreaking these rules may result in a warning, mute, or ban.",
        None, None,
    ).await?;

    // ── Messages: #announcements ───────────────────────────────────
    let announce1 = msg(
        &pool, &ch_announcements.id, alice, space_id,
        "**Accord v0.1.0 Released!**\n\nWe're excited to announce the first release of Accord. Features include:\n- Real-time messaging via WebSocket gateway\n- Voice channels powered by LiveKit\n- Role-based permissions\n- Public and private spaces\n\nCheck it out and let us know what you think!",
        None, None,
    ).await?;

    msg(&pool, &ch_announcements.id, alice, space_id,
        "**New Feature: Thread Replies**\n\nYou can now reply to messages in threads! Click on any message and select 'Reply in Thread' to start a conversation without cluttering the main channel.",
        None, None).await?;

    msg(&pool, &ch_announcements.id, alice, space_id,
        "**Maintenance Notice**\n\nWe'll be performing scheduled maintenance this weekend. Expect brief downtime on Saturday between 2-4 AM UTC.",
        None, None).await?;

    // ── Messages: #general ─────────────────────────────────────────
    msg(&pool, &ch_general.id, bob, space_id,
        "Good morning everyone! How's everyone doing today?",
        None, None).await?;

    let gen_alice = msg(&pool, &ch_general.id, alice, space_id,
        "Morning Bob! Pretty good, just pushed some new features.",
        None, None).await?;

    msg(&pool, &ch_general.id, charlie, space_id,
        "Nice! What kind of features?",
        Some(&gen_alice), None).await?;

    msg(&pool, &ch_general.id, alice, space_id,
        "Thread replies and some performance improvements to the gateway. Should make conversations much easier to follow.",
        None, None).await?;

    msg(&pool, &ch_general.id, diana, space_id,
        "That sounds great! I've been wanting threads for a while.",
        None, None).await?;

    msg(&pool, &ch_general.id, eve, space_id,
        "Has anyone tried the voice channels yet? They work really well!",
        None, None).await?;

    msg(&pool, &ch_general.id, bob, space_id,
        "Yeah the voice quality is surprisingly good. LiveKit was a solid choice.",
        None, None).await?;

    msg(&pool, &ch_general.id, charlie, space_id,
        "Agreed. I tested it with a few friends yesterday and it was smooth.",
        None, None).await?;

    msg(&pool, &ch_general.id, diana, space_id,
        "Anyone up for some gaming tonight? I was thinking we could try out the Gaming voice channel.",
        None, None).await?;

    msg(&pool, &ch_general.id, eve, space_id,
        "I'm in! What game were you thinking?",
        None, None).await?;

    msg(&pool, &ch_general.id, diana, space_id,
        "Maybe some Minecraft or Terraria? Something chill.",
        None, None).await?;

    msg(&pool, &ch_general.id, bob, space_id,
        "Count me in for Terraria!",
        None, None).await?;

    // ── Messages: #off-topic ───────────────────────────────────────
    msg(&pool, &ch_offtopic.id, eve, space_id,
        "What's everyone's favorite programming language?",
        None, None).await?;

    msg(&pool, &ch_offtopic.id, charlie, space_id,
        "Rust, obviously! Though I have a soft spot for Python too.",
        None, None).await?;

    msg(&pool, &ch_offtopic.id, bob, space_id,
        "TypeScript for me. Can't beat that developer experience.",
        None, None).await?;

    msg(&pool, &ch_offtopic.id, diana, space_id,
        "GDScript for game dev, but I've been learning Rust lately.",
        None, None).await?;

    msg(&pool, &ch_offtopic.id, alice, space_id,
        "Rust is what powers Accord, so I might be biased ;)",
        None, None).await?;

    // ── Messages: #introductions ───────────────────────────────────
    msg(&pool, &ch_intros.id, alice, space_id,
        "I'll start! I'm Alice, the creator of Accord. I love building open-source tools and playing strategy games in my free time.",
        None, None).await?;

    msg(&pool, &ch_intros.id, bob, space_id,
        "Hey! I'm Bob. Full-stack developer by day, moderator by night. I enjoy hiking and board games.",
        None, None).await?;

    msg(&pool, &ch_intros.id, charlie, space_id,
        "Charlie here! Systems programmer and Rust enthusiast. Currently building a game engine as a side project.",
        None, None).await?;

    msg(&pool, &ch_intros.id, diana, space_id,
        "Hi everyone! I'm Diana. I do digital art and indie game development with Godot. Excited to be here!",
        None, None).await?;

    msg(&pool, &ch_intros.id, eve, space_id,
        "I'm Eve! Comp sci student interested in networking and distributed systems. Accord's architecture is fascinating.",
        None, None).await?;

    // ── Messages: #programming (with a thread) ─────────────────────
    let prog_thread_start = msg(
        &pool, &ch_programming.id, charlie, space_id,
        "What's everyone's opinion on async runtimes in Rust? I've been comparing tokio vs async-std.",
        None, None,
    ).await?;

    msg(&pool, &ch_programming.id, alice, space_id,
        "Tokio is the de facto standard at this point. The ecosystem support is unmatched.",
        Some(&prog_thread_start), Some(&prog_thread_start)).await?;

    msg(&pool, &ch_programming.id, charlie, space_id,
        "That's what I figured. The documentation is also way better.",
        None, Some(&prog_thread_start)).await?;

    msg(&pool, &ch_programming.id, eve, space_id,
        "I've been reading the tokio internals — the work-stealing scheduler is really clever.",
        None, Some(&prog_thread_start)).await?;

    msg(&pool, &ch_programming.id, diana, space_id,
        "Has anyone used SQLx with tokio? How's the experience?",
        None, None).await?;

    msg(&pool, &ch_programming.id, alice, space_id,
        "It's great! We use it in Accord. Compile-time query checking is a game changer.",
        None, None).await?;

    msg(&pool, &ch_programming.id, bob, space_id,
        "Just discovered `cargo clippy` suggestions can be auto-applied with `cargo clippy --fix`. Mind blown.",
        None, None).await?;

    // ── Messages: #help ────────────────────────────────────────────
    let help_q = msg(
        &pool, &ch_help.id, eve, space_id,
        "How do I connect to the Accord WebSocket gateway? I'm building a custom client.",
        None, None,
    ).await?;

    msg(&pool, &ch_help.id, alice, space_id,
        "Connect to `GET /ws`, then you'll receive a HELLO event with a heartbeat interval. Send an IDENTIFY with your token and intents, and you'll get a READY event back!",
        Some(&help_q), None).await?;

    msg(&pool, &ch_help.id, eve, space_id,
        "Thanks Alice! That worked perfectly.",
        None, None).await?;

    msg(&pool, &ch_help.id, charlie, space_id,
        "Pro tip: make sure to send heartbeats on time or the server will disconnect you.",
        None, None).await?;

    let total_messages = 45;
    println!("  created ~{total_messages} messages across channels");

    // ── Reactions ──────────────────────────────────────────────────
    let reactions: &[(&str, &str, &str)] = &[
        // (message_id, user_id, emoji)
        (&welcome_msg, bob, "wave"),
        (&welcome_msg, charlie, "wave"),
        (&welcome_msg, diana, "tada"),
        (&welcome_msg, eve, "heart"),
        (&rules_msg, bob, "thumbsup"),
        (&rules_msg, charlie, "thumbsup"),
        (&rules_msg, diana, "thumbsup"),
        (&rules_msg, eve, "thumbsup"),
        (&announce1, bob, "tada"),
        (&announce1, charlie, "tada"),
        (&announce1, diana, "heart"),
        (&announce1, eve, "rocket"),
        (&announce1, alice, "rocket"),
    ];

    let reaction_sql = if is_postgres {
        "INSERT INTO reactions (message_id, user_id, emoji_name) VALUES (?, ?, ?) ON CONFLICT DO NOTHING"
    } else {
        "INSERT OR IGNORE INTO reactions (message_id, user_id, emoji_name) VALUES (?, ?, ?)"
    };
    for (message_id, user_id, emoji) in reactions {
        sqlx::query(reaction_sql)
            .bind(message_id)
            .bind(user_id)
            .bind(emoji)
            .execute(&pool)
            .await?;
    }

    println!("  added {} reactions", reactions.len());

    // ── Pinned messages ────────────────────────────────────────────
    db::messages::pin_message(&pool, &ch_welcome.id, &welcome_msg).await?;
    db::messages::pin_message(&pool, &ch_rules.id, &rules_msg).await?;
    db::messages::pin_message(&pool, &ch_announcements.id, &announce1).await?;

    println!("  pinned 3 messages");

    // ── Invite ─────────────────────────────────────────────────────
    let invite = db::invites::create_invite(
        &pool,
        space_id,
        None, // space-level invite
        alice,
        &CreateInvite {
            max_uses: None,
            max_age: None,
            temporary: Some(false),
        },
    )
    .await?;

    println!("  created permanent invite: {}", invite.code);

    // ── Done ───────────────────────────────────────────────────────
    println!("\naccord-seed: seeding complete!");
    println!("  Space: Accord Community ({})", space_id);
    println!("  Users: alice, bob, charlie, diana, eve (password: password)");
    println!("  Invite code: {}", invite.code);

    Ok(())
}
