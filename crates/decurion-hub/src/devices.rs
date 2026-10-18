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

/// A signed-in device, found from its cookie.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignedIn {
    pub id: i64,
    /// This lookup wrote its "last used" time (at most every five minutes):
    /// the moment to renew its cookie.
    pub seen_now: bool,
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
pub const PAIRING_MINUTES: i64 = 30;

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

    /// The device a cookie token belongs to, if it is still signed in. Its
    /// "last used" time is written at most once every five minutes, so the
    /// board's polling does not turn into constant database writes. When
    /// this call wrote it, `seen_now` is set and the caller sends the cookie
    /// again with a fresh lifetime, so a device in use never expires.
    pub async fn device_for_token(&self, token: &str) -> Result<Option<SignedIn>> {
        let h = hash(token);
        let id: Option<i64> = sqlx::query_scalar(
            "SELECT id FROM devices WHERE token_hash = ? AND revoked_at IS NULL",
        )
        .bind(&h)
        .fetch_optional(self.pool())
        .await?;
        let Some(id) = id else {
            return Ok(None);
        };
        let done = sqlx::query(
            "UPDATE devices SET last_seen = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE id = ? AND (last_seen IS NULL
                 OR last_seen < strftime('%Y-%m-%dT%H:%M:%fZ', 'now', '-5 minutes'))",
        )
        .bind(id)
        .execute(self.pool())
        .await?;
        Ok(Some(SignedIn {
            id,
            seen_now: done.rows_affected() == 1,
        }))
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
    /// Any earlier unused code stops working, so only the latest QR is live.
    pub async fn new_pairing_code(&self) -> Result<String> {
        let code = crate::new_secret();
        sqlx::query(
            "UPDATE pairing_codes SET used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE used_at IS NULL",
        )
        .execute(self.pool())
        .await?;
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

    /// Uses up a pairing code and creates the device in one transaction, so
    /// a failure never burns the code without a device to show for it.
    pub async fn pair_device(&self, code: &str, name: &str) -> Result<Option<String>> {
        let mut tx = self.pool().begin().await?;
        let done = sqlx::query(
            "UPDATE pairing_codes SET used_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE code_hash = ? AND used_at IS NULL
               AND expires_at > strftime('%Y-%m-%dT%H:%M:%fZ', 'now')",
        )
        .bind(hash(code))
        .execute(&mut *tx)
        .await?;
        if done.rows_affected() != 1 {
            return Ok(None);
        }
        let token = crate::new_secret();
        let name: String = name.chars().take(60).collect();
        sqlx::query("INSERT INTO devices (name, token_hash) VALUES (?, ?)")
            .bind(&name)
            .bind(hash(&token))
            .execute(&mut *tx)
            .await?;
        tx.commit().await?;
        Ok(Some(token))
    }

    /// Saves where to notify this device. A browser has one push address, so
    /// if another (older) device row had it, it moves here: removing this
    /// device then really stops the notifications.
    pub async fn save_push(&self, device_id: i64, s: &PushSubscription) -> Result<()> {
        if !s.endpoint.starts_with("https://") {
            return Err(StoreError::Invalid("push endpoint must be https".into()));
        }
        let mut tx = self.pool().begin().await?;
        sqlx::query("DELETE FROM push_subscriptions WHERE endpoint = ? AND device_id != ?")
            .bind(&s.endpoint)
            .bind(device_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "INSERT INTO push_subscriptions (device_id, endpoint, p256dh, auth) VALUES (?, ?, ?, ?)
             ON CONFLICT (device_id) DO UPDATE SET endpoint = excluded.endpoint,
                 p256dh = excluded.p256dh, auth = excluded.auth",
        )
        .bind(device_id)
        .bind(&s.endpoint)
        .bind(&s.p256dh)
        .bind(&s.auth)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
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

    /// Drops a subscription the push service says is gone, unless the device
    /// has registered a new one since.
    pub async fn drop_push(&self, device_id: i64, endpoint: &str) -> Result<()> {
        sqlx::query("DELETE FROM push_subscriptions WHERE device_id = ? AND endpoint = ?")
            .bind(device_id)
            .bind(endpoint)
            .execute(self.pool())
            .await?;
        Ok(())
    }
}
