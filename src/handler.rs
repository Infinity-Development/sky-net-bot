use log::{info, error, warn};
use poise::serenity_prelude::{GuildId, UserId};
use sqlx::PgPool;

use crate::{Error, cache::CacheHttpImpl, limits};

pub async fn create_guild_if_not_exists(
    guild_id: GuildId,
    pool: &PgPool
) -> Result<(), Error> {
    sqlx::query!(
        "
            INSERT INTO guilds (guild_id)
            VALUES ($1)
            ON CONFLICT (guild_id) DO NOTHING
        ",
        guild_id.to_string()
    )
    .execute(pool)
    .await?;

    Ok(())
}

pub async fn handle_mod_action(
    guild_id: GuildId,
    user_id: UserId,
    pool: &PgPool,
    cache_http: &CacheHttpImpl,
    action: limits::UserLimitTypes,
    action_target: String
) -> Result<(), Error> {
    create_guild_if_not_exists(guild_id, pool).await?;

    // Insert into user_actions
    sqlx::query!(
        "
            INSERT INTO user_actions (action_id, guild_id, user_id, limit_type, action_target)
            VALUES ($1, $2, $3, $4, $5)
        ",
        crate::crypto::gen_random(48),
        guild_id.to_string(),
        user_id.to_string(),
        action.to_string(),
        action_target
    )
    .execute(pool)
    .await?;

    // Check if they hit any limits yet
    let hit = limits::UserLimitsHit::hit(guild_id, pool).await?;

    for hit_limit in hit {
        // We have a hit limit for this user
        info!("Hit limit: {:?}", hit_limit);

        // Immediately handle the limit
        let cur_uid = cache_http.cache.current_user().id;
        let can_mod = {
            let guild = cache_http.cache.guild(guild_id).ok_or("Guild not found")?;
        
            guild.greater_member_hierarchy(cache_http.cache.clone(), cur_uid, user_id)
        }.unwrap_or(cur_uid);
    
        if can_mod == cur_uid {
            info!("Moderating user");
            match hit_limit.limit.limit_action {
                limits::UserLimitActions::RemoveAllRoles => {
                    // Get all user roles
                    if let Ok(mut member) = guild_id.member(cache_http, user_id).await {
                        let roles = member.roles.clone();
                        for role in roles.iter() {
                            if let Err(e) = member.remove_role(&cache_http.http, role).await {
                                error!("Failed to remove role: {}", e);
                            }
                        }
                    }
                },
                limits::UserLimitActions::KickUser => {
                    if let Err(e) = guild_id.kick(&cache_http.http, user_id).await {
                        error!("Failed to kick user: {}", e);
                    }
                },
                limits::UserLimitActions::BanUser => {
                    if let Err(e) = guild_id.ban(&cache_http.http, user_id, 0).await {
                        error!("Failed to kick user: {}", e);
                    }
                },
            }
        } else {
            warn!("Cannot moderate user, not enough permissions: {}, {}", can_mod, cur_uid);
	    return Ok(())
        }

        for action in hit_limit.cause.iter() {
            sqlx::query!(
                "
                UPDATE user_actions
                SET handled_for = array_append(handled_for, $1)
                WHERE action_id = $2",
                hit_limit.limit.limit_id,
                action.action_id
            )
            .execute(pool)
            .await?;   
        }

        sqlx::query!(
            "
            INSERT INTO hit_limits
            (id, guild_id, user_id, limit_id, cause)
            VALUES ($1, $2, $3, $4, $5)",
            crate::crypto::gen_random(16),
            guild_id.to_string(),
            user_id.to_string(),
            hit_limit.limit.limit_id,
            &hit_limit.cause.iter().map(|a| a.action_id.clone()).collect::<Vec<_>>()
        )
        .execute(pool)
        .await?;
    }

    Ok(())
}
