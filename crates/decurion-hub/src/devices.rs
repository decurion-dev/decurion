//! Signed-in browsers and phones. A device holds a random token in an HttpOnly
//! cookie; the hub keeps only its SHA-256, so the database alone cannot be used
//! to sign in. Devices survive hub restarts and can be removed from the board.

use crate::store::{Result, Store, StoreError};
use serde::Serialize;
use sha2::{Digest, Sha256};
use sqlx::Row;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Device {
    pub id: i64,
    pub name: String,
    pub created_at: String,
    pub last_seen: Option<String>,
    pub has_push: bool,
}

#[derive(Debug, Clone, serde::Deserialize, Serialize)]
pub struct PushSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
}

pub fn hash(secret: &str) -> String {
    Sha256::digest(secret.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// How long a pairing QR code works.
pub const PAIRING_MINUTES: i64 = 10;

impl Store {
    /// Creates a device and returns it with its token. The token is shown once,
    /// in the cookie, and never stored.
    pub async fn add_device(&self, name: &str) -> Result<(Device, String)> {
        let token = crate::new_secret();
        let name: String = name.chars().take(60).collect();
        let id: i64 =
            sqlx::query_scalar("INSERT INTO devices (name, token_hash) VALUES (?, ?) RETURNING id")
                .bind(&name)
                .bind(hash(&token))
                .fetch_one(self.pool())
                .await?;
        let d = self
            .devices()
            .await?
            .into_iter()
            .find(|d| d.id == id)
            .ok_or(StoreError::NotFound)?;
        Ok((d, token))
    }

    /// The device a cookie token belongs to, if it is still signed in.
    pub async fn device_for_token(&self, token: &str) -> Result<Option<i64>> {
        let id: Option<i64> = sqlx::query_scalar(
            "UPDATE devices SET last_seen = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE token_hash = ? AND revoked_at IS NULL RETURNING id",
        )
        .bind(hash(token))
        .fetch_optional(self.pool())
        .await?;
        Ok(id)
    }

    pub async fn devices(&self) -> Result<Vec<Device>> {
        let rows = sqlx::query(
            "SELECT d.id, d.name, d.created_at, d.last_seen, p.device_id IS NOT NULL AS has_push
             FROM devices d LEFT JOIN push_subscriptions p ON p.device_id = d.id
             WHERE d.revoked_at IS NULL ORDER BY d.id",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok(Device {
                    id: r.try_get("id")?,
                    name: r.try_get("name")?,
                    created_at: r.try_get("created_at")?,
                    last_seen: r.try_get("last_seen")?,
                    has_push: r.try_get("has_push")?,
                })
            })
            .collect()
    }

    /// Signs a device out for good and stops its notifications.
    pub async fn remove_device(&self, id: i64) -> Result<()> {
        let mut tx = self.pool().begin().await?;
        let done = sqlx::query(
            "UPDATE devices SET revoked_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ? AND revoked_at IS NULL",
        )
        .bind(id)
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() != 1 {
            return Err(StoreError::NotFound);
        }
        sqlx::query("DELETE FROM push_subscriptions WHERE device_id = ?")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// A fresh pairing code, valid for [`PAIRING_MINUTES`] and usable once.
    pub async fn new_pairing_code(&self) -> Result<String> {
        let code = crate::new_secret();
        sqlx::query(
            "INSERT INTO pairing_codes (code_hash, expires_at)
             VALUES (?, strftime('%Y-%m-%dT%H:%M:%fZ', 'now', ?))",
        )
        .bind(hash(&code))
        .bind(format!("+{PAIRING_MINUTES} minutes"))
        .execute(self.pool())
        .await?;
        Ok(code)
    }

    /// Uses up a pairing code. True only for an unused, unexpired code, and only
    /// once even if two phones race.
    pub async fn redeem_pairing_code(&self, code: &str) -> Result<bool> {
        let done = sqlx::query(
            "UPDATE pairing_codes SET used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE code_hash = ? AND used_at IS NULL
               AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(hash(code))
        .execute(self.pool())
        .await?;
        Ok(done.rows_affected() == 1)
    }

    pub async fn save_push(&self, device_id: i64, s: &PushSubscription) -> Result<()> {
        if !s.endpoint.starts_with("https://") {
            return Err(StoreError::Invalid("push endpoint must be https".into()));
        }
        sqlx::query(
            "INSERT INTO push_subscriptions (device_id, endpoint, p256dh, auth) VALUES (?, ?, ?, ?)
             ON CONFLICT (device_id) DO UPDATE SET endpoint = excluded.endpoint,
                 p256dh = excluded.p256dh, auth = excluded.auth",
        )
        .bind(device_id)
        .bind(&s.endpoint)
        .bind(&s.p256dh)
        .bind(&s.auth)
        .execute(self.pool())
        .await?;
        Ok(())
    }

    pub async fn push_targets(&self) -> Result<Vec<(i64, PushSubscription)>> {
        let rows = sqlx::query(
            "SELECT p.device_id, p.endpoint, p.p256dh, p.auth FROM push_subscriptions p
             JOIN devices d ON d.id = p.device_id WHERE d.revoked_at IS NULL",
        )
        .fetch_all(self.pool())
        .await?;
        rows.iter()
            .map(|r| {
                Ok((
                    r.try_get("device_id")?,
                    PushSubscription {
                        endpoint: r.try_get("endpoint")?,
                        p256dh: r.try_get("p256dh")?,
                        auth: r.try_get("auth")?,
                    },
                ))
            })
            .collect()
    }

    /// Drops a subscription the push service says is gone.
    pub async fn drop_push(&self, device_id: i64) -> Result<()> {
        sqlx::query("DELETE FROM push_subscriptions WHERE device_id = ?")
            .bind(device_id)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
