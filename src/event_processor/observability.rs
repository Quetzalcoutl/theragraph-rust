use crate::error::Result;
use crate::kafka::BlockchainEvent;
use tracing::info;

impl super::EventDispatcher {
    // ── Observability-only handlers (log and return Ok) ────────────────────

    pub(super) fn log_username_registration(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("👤 Username registered: {} → {}",
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("username").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_profile_update(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("📝 Profile update: {}", d.get("user").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_profile_update_extended(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("📝 ProfileUpdatedExtended: {} (username={}, hash={}, bio={}, website={})",
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("username").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("profile_hash").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("bio").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("website").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_tip(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("💸 Tip: {} sent {} to {}",
                d.get("from").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("amount").and_then(|v| v.as_u64()).unwrap_or(0),
                d.get("to").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_badge(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🏆 Badge: {} earned {}",
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("badgeType").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_user_verified(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("✅ User verified: {}", d.get("user").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_user_blocked(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🚫 Block: {} blocked {}",
                d.get("blockedBy").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_earnings_withdrawn(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("💸 Earnings withdrawn: {} withdrew {}",
                d.get("user").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("amount").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_content_burned(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🔥 Content burned: {} burned token {}",
                d.get("burner").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("tokenId").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_burned_content_revenue(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("💰 Burned content revenue: {} received {}",
                d.get("recipient").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("amount").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    pub(super) fn log_treasury_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🏦 Treasury update: {} performed {}",
                d.get("updater").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("action").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    /// Replaces the old `PricesUpdated` admin event — the lazy-mint contract no
    /// longer has copy/like/comment/follow prices, only a single platform fee.
    pub(super) fn log_platform_fee_updated(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("💰 Platform fee updated: {}",
                d.get("fee").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }

    // `ListingUpdated` moved to `event_processor::interaction::handle_listing_updated`
    // (2026-09-13, lazy-mint economy) — no longer observability-only, now persists
    // price/max_copies onto the Nebula post vertex (migration 26).

    pub(super) fn log_tokens_recovered(&self, event: &BlockchainEvent) -> Result<()> {
        if let Some(d) = &event.data {
            info!("🔄 Tokens recovered: {} recovered {}",
                d.get("recoverer").and_then(|v| v.as_str()).unwrap_or(""),
                d.get("amount").and_then(|v| v.as_str()).unwrap_or(""));
        }
        Ok(())
    }
}
