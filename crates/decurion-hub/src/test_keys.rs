//! Which test keys each app has. The values are in the vault; the database
//! keeps only each key's name and last four characters, for the board.

use crate::store::{Result, Store};
use decurion_proto::TestKey;
use sqlx::Row;

impl Store {
    /// The app's test keys, by name.
    pub async fn test_keys(&self, app_id: i64) -> Result<Vec<TestKey>> {
        self.app(app_id).await?;
        let rows = sqlx::query(
            "SELECT name, last_four, added_at FROM test_keys WHERE app_id = ? ORDER BY name",
        )
        .bind(app_id)
        .fetch_all(self.pool())
        .await?;
        rows.iter().map(test_key_from).collect()
    }

    /// Records a key saved in the vault. Saving a name again replaces it.
    pub async fn save_test_key(&self, app_id: i64, name: &str, last_four: &str) -> Result<TestKey> {
        self.app(app_id).await?;
        let row = sqlx::query(
            "INSERT INTO test_keys (app_id, name, last_four) VALUES (?, ?, ?)
             ON CONFLICT (app_id, name) DO UPDATE SET last_four = excluded.last_four,
                 added_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             RETURNING name, last_four, added_at",
        )
        .bind(app_id)
        .bind(name)
        .bind(last_four)
        .fetch_one(self.pool())
        .await?;
        test_key_from(&row)
    }

    /// Forgets a key. Forgetting one that is not there is not an error.
    pub async fn remove_test_key(&self, app_id: i64, name: &str) -> Result<()> {
        self.app(app_id).await?;
        sqlx::query("DELETE FROM test_keys WHERE app_id = ? AND name = ?")
            .bind(app_id)
            .bind(name)
            .execute(self.pool())
            .await?;
        Ok(())
    }

    /// Just the names, for a claim: the runner fetches values from the vault.
    pub(crate) async fn test_key_names(&self, app_id: i64) -> Result<Vec<String>> {
        Ok(
            sqlx::query_scalar("SELECT name FROM test_keys WHERE app_id = ? ORDER BY name")
                .bind(app_id)
                .fetch_all(self.pool())
                .await?,
        )
    }
}

fn test_key_from(row: &sqlx::sqlite::SqliteRow) -> Result<TestKey> {
    Ok(TestKey {
        name: row.try_get("name")?,
        last_four: row.try_get("last_four")?,
        added_at: row.try_get("added_at")?,
    })
}
