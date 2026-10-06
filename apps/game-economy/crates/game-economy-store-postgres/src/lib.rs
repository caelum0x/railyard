//! Transactional Postgres [`GameStateRepository`] for horizontally-scaled
//! storefronts.
//!
//! The JSON adapter (`game-economy-store`) is correct for ONE process: its
//! single-owner reservations and read-modify-write inventory updates are guarded
//! by an in-process mutex. This adapter moves every guard into the database so
//! any number of `game-economy-server` instances can share one state:
//!
//! - inventories are row-locked (`SELECT ... FOR UPDATE`) for every
//!   read-modify-write (purchase grant, pack open, pass advance, refund);
//! - single-owner reservations are rows keyed by `(player, ownership_key)`,
//!   so two instances cannot both reserve the same cosmetic / pass;
//! - a purchase commit, its inventory grant and a reversal each run in one
//!   transaction.
//!
//! Lock order is always purchase row, then inventory row, so concurrent
//! commits and reversals cannot deadlock each other.
//!
//! The port is synchronous (the store core has no async storage calls), so each
//! call bridges onto the ambient tokio runtime with `block_in_place`. That
//! requires the multi-thread runtime (`#[tokio::main]` default); the adapter
//! returns a clear storage error on a current-thread runtime instead of
//! deadlocking.
//!
//! Uses the sqlx RUNTIME query API only, so it compiles without a live
//! `DATABASE_URL`.

use std::future::Future;

use game_economy_core::{
    CatalogFile, CatalogRepository, CompletedPurchase, GameError, GameStateRepository, Inventory,
    Product, PurchaseOutcome, PurchaseRecord, PurchaseReversal, PurchaseSummary, Rarity,
    ReverseOutcome, StoredCatalog,
};
use sqlx::postgres::{PgPool, PgPoolOptions, PgRow};
use sqlx::{Postgres, Row, Transaction};

/// Default connection-pool size per server instance.
pub const DEFAULT_MAX_CONNECTIONS: u32 = 10;

#[derive(Clone)]
pub struct PostgresGameState {
    pool: PgPool,
}

impl PostgresGameState {
    /// Connect to `database_url` with [`DEFAULT_MAX_CONNECTIONS`].
    pub async fn connect(database_url: &str) -> Result<Self, GameError> {
        Self::connect_with(database_url, DEFAULT_MAX_CONNECTIONS).await
    }

    pub async fn connect_with(database_url: &str, max_connections: u32) -> Result<Self, GameError> {
        let pool = PgPoolOptions::new()
            .max_connections(max_connections.max(1))
            .connect(database_url)
            .await
            .map_err(storage)?;
        Ok(Self { pool })
    }

    /// Wrap an existing pool.
    pub fn from_pool(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Apply the bundled migrations (idempotent).
    pub async fn migrate(&self) -> Result<(), GameError> {
        sqlx::migrate!()
            .run(&self.pool)
            .await
            .map_err(|error| GameError::Storage(format!("migrate game state: {error}")))
    }

    /// Run an async database operation from the synchronous port.
    fn run<T>(&self, fut: impl Future<Output = Result<T, GameError>>) -> Result<T, GameError> {
        let handle = tokio::runtime::Handle::try_current().map_err(|_| {
            GameError::Storage("postgres game state must be used inside a tokio runtime".into())
        })?;
        if matches!(
            handle.runtime_flavor(),
            tokio::runtime::RuntimeFlavor::CurrentThread
        ) {
            return Err(GameError::Storage(
                "postgres game state requires the multi-thread tokio runtime".into(),
            ));
        }
        tokio::task::block_in_place(|| handle.block_on(fut))
    }

    async fn upsert_completed(
        tx: &mut Transaction<'_, Postgres>,
        idem_key: &str,
        completed: &CompletedPurchase,
    ) -> Result<(), GameError> {
        let outcome = serde_json::to_value(&completed.outcome).map_err(encode)?;
        sqlx::query(
            "INSERT INTO game_purchases \
               (idem_key, player, product_sku, state, receipt_id, settlement_seq, charged_micros, outcome, completed_at) \
             VALUES ($1, $2, $3, 'completed', $4, $5, $6, $7, now()) \
             ON CONFLICT (idem_key) DO UPDATE SET \
               state = 'completed', receipt_id = EXCLUDED.receipt_id, \
               settlement_seq = EXCLUDED.settlement_seq, charged_micros = EXCLUDED.charged_micros, \
               outcome = EXCLUDED.outcome, completed_at = now()",
        )
        .bind(idem_key)
        .bind(&completed.player)
        .bind(&completed.outcome.product_sku)
        .bind(&completed.outcome.receipt_id)
        .bind(seq_to_i64(completed.outcome.settlement_seq))
        .bind(completed.outcome.charged_micros)
        .bind(outcome)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
        Ok(())
    }
}

/// Ensure the player's inventory row exists and lock it for this transaction.
async fn lock_inventory(
    tx: &mut Transaction<'_, Postgres>,
    player: &str,
) -> Result<Inventory, GameError> {
    let empty = serde_json::to_value(Inventory::default()).map_err(encode)?;
    sqlx::query(
        "INSERT INTO game_inventories (player, inventory) VALUES ($1, $2) \
         ON CONFLICT (player) DO NOTHING",
    )
    .bind(player)
    .bind(empty)
    .execute(&mut **tx)
    .await
    .map_err(storage)?;
    let row = sqlx::query("SELECT inventory FROM game_inventories WHERE player = $1 FOR UPDATE")
        .bind(player)
        .fetch_one(&mut **tx)
        .await
        .map_err(storage)?;
    decode_json(row.try_get::<serde_json::Value, _>("inventory").map_err(storage)?)
}

async fn write_inventory(
    tx: &mut Transaction<'_, Postgres>,
    player: &str,
    inventory: &Inventory,
) -> Result<(), GameError> {
    let value = serde_json::to_value(inventory).map_err(encode)?;
    sqlx::query("UPDATE game_inventories SET inventory = $2, updated_at = now() WHERE player = $1")
        .bind(player)
        .bind(value)
        .execute(&mut **tx)
        .await
        .map_err(storage)?;
    Ok(())
}

impl GameStateRepository for PostgresGameState {
    fn inventory(&self, player: &str) -> Result<Inventory, GameError> {
        self.run(async {
            let row = sqlx::query("SELECT inventory FROM game_inventories WHERE player = $1")
                .bind(player)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?;
            match row {
                Some(row) => decode_json(row.try_get("inventory").map_err(storage)?),
                None => Ok(Inventory::default()),
            }
        })
    }

    fn purchase(&self, idem_key: &str) -> Result<Option<PurchaseRecord>, GameError> {
        self.run(async {
            let row = sqlx::query(
                "SELECT player, product_sku, state, outcome, reversal \
                 FROM game_purchases WHERE idem_key = $1",
            )
            .bind(idem_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
            row.map(|row| record_from_row(&row)).transpose()
        })
    }

    fn purchases_for(&self, player: &str) -> Result<Vec<CompletedPurchase>, GameError> {
        self.run(async {
            let rows = sqlx::query(
                "SELECT player, outcome, reversal FROM game_purchases \
                 WHERE player = $1 AND state = 'completed' \
                 ORDER BY settlement_seq DESC",
            )
            .bind(player)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            rows.iter().map(completed_from_row).collect()
        })
    }

    fn begin_purchase(
        &self,
        idem_key: &str,
        player: &str,
        product_sku: &str,
    ) -> Result<(), GameError> {
        self.run(async {
            let inserted = sqlx::query(
                "INSERT INTO game_purchases (idem_key, player, product_sku, state) \
                 VALUES ($1, $2, $3, 'pending') ON CONFLICT (idem_key) DO NOTHING",
            )
            .bind(idem_key)
            .bind(player)
            .bind(product_sku)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
            if inserted.rows_affected() == 1 {
                return Ok(());
            }
            let row = sqlx::query(
                "SELECT player, product_sku FROM game_purchases WHERE idem_key = $1",
            )
            .bind(idem_key)
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
            let existing_player: String = row.try_get("player").map_err(storage)?;
            let existing_sku: String = row.try_get("product_sku").map_err(storage)?;
            if existing_player == player && existing_sku == product_sku {
                Ok(())
            } else {
                Err(GameError::IdempotencyConflict)
            }
        })
    }

    fn commit_purchase(
        &self,
        idem_key: &str,
        completed: &CompletedPurchase,
        inventory: &Inventory,
    ) -> Result<(), GameError> {
        let replacement = inventory.clone();
        self.commit_purchase_with(idem_key, completed, &|inv: &mut Inventory| {
            *inv = replacement.clone();
        })
    }

    fn commit_purchase_with(
        &self,
        idem_key: &str,
        completed: &CompletedPurchase,
        grant: &dyn Fn(&mut Inventory),
    ) -> Result<(), GameError> {
        self.run(async {
            let mut tx = self.pool.begin().await.map_err(storage)?;
            let existing = sqlx::query(
                "SELECT player, product_sku, state, outcome, reversal \
                 FROM game_purchases WHERE idem_key = $1 FOR UPDATE",
            )
            .bind(idem_key)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
            if let Some(row) = existing {
                match record_from_row(&row)? {
                    PurchaseRecord::Completed(done) => {
                        tx.rollback().await.map_err(storage)?;
                        return if done.same_settlement(completed) {
                            Ok(())
                        } else {
                            Err(GameError::IdempotencyConflict)
                        };
                    }
                    PurchaseRecord::Pending {
                        player,
                        product_sku,
                    } if player != completed.player
                        || product_sku != completed.outcome.product_sku =>
                    {
                        tx.rollback().await.map_err(storage)?;
                        return Err(GameError::IdempotencyConflict);
                    }
                    PurchaseRecord::Pending { .. } => {}
                }
            }
            let mut inventory = lock_inventory(&mut tx, &completed.player).await?;
            grant(&mut inventory);
            write_inventory(&mut tx, &completed.player, &inventory).await?;
            Self::upsert_completed(&mut tx, idem_key, completed).await?;
            tx.commit().await.map_err(storage)
        })
    }

    fn save_inventory(&self, player: &str, inventory: &Inventory) -> Result<(), GameError> {
        self.run(async {
            let value = serde_json::to_value(inventory).map_err(encode)?;
            sqlx::query(
                "INSERT INTO game_inventories (player, inventory) VALUES ($1, $2) \
                 ON CONFLICT (player) DO UPDATE SET inventory = EXCLUDED.inventory, updated_at = now()",
            )
            .bind(player)
            .bind(value)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
            Ok(())
        })
    }

    fn update_inventory(
        &self,
        player: &str,
        apply: &mut dyn FnMut(&mut Inventory) -> Result<(), GameError>,
    ) -> Result<Inventory, GameError> {
        self.run(async {
            let mut tx = self.pool.begin().await.map_err(storage)?;
            let mut inventory = lock_inventory(&mut tx, player).await?;
            if let Err(error) = apply(&mut inventory) {
                tx.rollback().await.map_err(storage)?;
                return Err(error);
            }
            write_inventory(&mut tx, player, &inventory).await?;
            tx.commit().await.map_err(storage)?;
            Ok(inventory)
        })
    }

    fn reserve_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        sku: &str,
        idem_key: &str,
    ) -> Result<(), GameError> {
        self.run(async {
            // Two attempts: the holder may release between our INSERT and SELECT.
            for _ in 0..2 {
                let inserted = sqlx::query(
                    "INSERT INTO game_reservations (player, ownership_key, idem_key) \
                     VALUES ($1, $2, $3) ON CONFLICT (player, ownership_key) DO NOTHING",
                )
                .bind(player)
                .bind(ownership_key)
                .bind(idem_key)
                .execute(&self.pool)
                .await
                .map_err(storage)?;
                if inserted.rows_affected() == 1 {
                    return Ok(());
                }
                let holder: Option<String> = sqlx::query(
                    "SELECT idem_key FROM game_reservations WHERE player = $1 AND ownership_key = $2",
                )
                .bind(player)
                .bind(ownership_key)
                .fetch_optional(&self.pool)
                .await
                .map_err(storage)?
                .map(|row| row.try_get("idem_key"))
                .transpose()
                .map_err(storage)?;
                match holder {
                    Some(holder) if holder == idem_key => return Ok(()),
                    Some(_) => return Err(GameError::AlreadyOwned(sku.to_string())),
                    None => continue,
                }
            }
            Err(GameError::Storage(
                "single-owner reservation contended; retry the purchase".into(),
            ))
        })
    }

    fn release_single_owner(&self, player: &str, ownership_key: &str, idem_key: &str) {
        let result = self.run(async {
            sqlx::query(
                "DELETE FROM game_reservations \
                 WHERE player = $1 AND ownership_key = $2 AND idem_key = $3",
            )
            .bind(player)
            .bind(ownership_key)
            .bind(idem_key)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
            Ok(())
        });
        if let Err(error) = result {
            // The port is infallible here (release runs on an error path that
            // is already being reported); log so a stuck reservation is visible.
            tracing::error!(%error, player, ownership_key, "failed to release single-owner reservation");
        }
    }

    fn single_owner_holder(
        &self,
        player: &str,
        ownership_key: &str,
    ) -> Result<Option<game_economy_core::ReservationHolder>, GameError> {
        self.run(async {
            let row = sqlx::query(
                "SELECT idem_key, (EXTRACT(EPOCH FROM (now() - created_at)) * 1000)::bigint AS age_ms \
                 FROM game_reservations WHERE player = $1 AND ownership_key = $2",
            )
            .bind(player)
            .bind(ownership_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(storage)?;
            row.map(|r| {
                Ok(game_economy_core::ReservationHolder {
                    idem_key: r.try_get("idem_key").map_err(storage)?,
                    age_ms: r.try_get("age_ms").map_err(storage)?,
                })
            })
            .transpose()
        })
    }

    fn take_over_single_owner(
        &self,
        player: &str,
        ownership_key: &str,
        from_idem: &str,
        to_idem: &str,
    ) -> Result<bool, GameError> {
        self.run(async {
            let moved = sqlx::query(
                "UPDATE game_reservations SET idem_key = $4, created_at = now() \
                 WHERE player = $1 AND ownership_key = $2 AND idem_key = $3",
            )
            .bind(player)
            .bind(ownership_key)
            .bind(from_idem)
            .bind(to_idem)
            .execute(&self.pool)
            .await
            .map_err(storage)?;
            Ok(moved.rows_affected() == 1)
        })
    }

    fn reverse_purchase(
        &self,
        player: &str,
        receipt_id: &str,
        reversal: &PurchaseReversal,
        ownership_key: Option<&str>,
        revoke: &dyn Fn(&CompletedPurchase, &mut Inventory),
    ) -> Result<ReverseOutcome, GameError> {
        self.run(async {
            let mut tx = self.pool.begin().await.map_err(storage)?;
            let row = sqlx::query(
                "SELECT idem_key, player, outcome, reversal FROM game_purchases \
                 WHERE player = $1 AND receipt_id = $2 AND state = 'completed' FOR UPDATE",
            )
            .bind(player)
            .bind(receipt_id)
            .fetch_optional(&mut *tx)
            .await
            .map_err(storage)?;
            let Some(row) = row else {
                tx.rollback().await.map_err(storage)?;
                return Ok(ReverseOutcome::NotFound);
            };
            let idem_key: String = row.try_get("idem_key").map_err(storage)?;
            let mut purchase = completed_from_row(&row)?;
            if purchase.reversal.is_some() {
                tx.rollback().await.map_err(storage)?;
                return Ok(ReverseOutcome::AlreadyReversed(purchase));
            }

            let mut inventory = lock_inventory(&mut tx, player).await?;
            revoke(&purchase, &mut inventory);
            write_inventory(&mut tx, player, &inventory).await?;

            let value = serde_json::to_value(reversal).map_err(encode)?;
            sqlx::query(
                "UPDATE game_purchases SET reversal_kind = $2, reversal = $3, reversed_at = now() \
                 WHERE idem_key = $1",
            )
            .bind(&idem_key)
            .bind(reversal.kind.as_str())
            .bind(value)
            .execute(&mut *tx)
            .await
            .map_err(storage)?;

            if let Some(key) = ownership_key {
                sqlx::query(
                    "DELETE FROM game_reservations WHERE player = $1 AND ownership_key = $2",
                )
                .bind(player)
                .bind(key)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            }
            tx.commit().await.map_err(storage)?;
            purchase.reversal = Some(reversal.clone());
            Ok(ReverseOutcome::Reversed(purchase))
        })
    }

    fn recent_purchases(&self, limit: usize) -> Result<Vec<CompletedPurchase>, GameError> {
        let limit = i64::try_from(limit).unwrap_or(i64::MAX);
        self.run(async {
            let rows = sqlx::query(
                "SELECT player, outcome, reversal FROM game_purchases \
                 WHERE state = 'completed' ORDER BY completed_at DESC, settlement_seq DESC LIMIT $1",
            )
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(storage)?;
            rows.iter().map(completed_from_row).collect()
        })
    }

    fn summary(&self) -> Result<PurchaseSummary, GameError> {
        self.run(async {
            let row = sqlx::query(
                "SELECT \
                   COUNT(*) FILTER (WHERE state = 'completed') AS completed, \
                   COUNT(*) FILTER (WHERE state = 'pending') AS pending, \
                   COUNT(DISTINCT player) FILTER (WHERE state = 'completed') AS players, \
                   COALESCE(SUM(charged_micros) FILTER (WHERE state = 'completed'), 0)::BIGINT AS gross, \
                   COUNT(*) FILTER (WHERE reversal_kind = 'refunded') AS refunded_count, \
                   COALESCE(SUM((reversal->>'refunded_micros')::BIGINT) \
                     FILTER (WHERE reversal_kind = 'refunded'), 0)::BIGINT AS refunded_micros, \
                   COUNT(*) FILTER (WHERE reversal_kind = 'revoked') AS revoked_count \
                 FROM game_purchases",
            )
            .fetch_one(&self.pool)
            .await
            .map_err(storage)?;
            let count = |name: &str| -> Result<u64, GameError> {
                let v: i64 = row.try_get(name).map_err(storage)?;
                Ok(u64::try_from(v).unwrap_or(0))
            };
            let gross: i64 = row.try_get("gross").map_err(storage)?;
            let refunded_micros: i64 = row.try_get("refunded_micros").map_err(storage)?;
            Ok(PurchaseSummary {
                completed_purchases: count("completed")?,
                pending_purchases: count("pending")?,
                players: count("players")?,
                gross_micros: gross,
                refunded_count: count("refunded_count")?,
                refunded_micros,
                revoked_count: count("revoked_count")?,
                net_micros: gross - refunded_micros,
            })
        })
    }
}

fn record_from_row(row: &PgRow) -> Result<PurchaseRecord, GameError> {
    let state: String = row.try_get("state").map_err(storage)?;
    match state.as_str() {
        "pending" => Ok(PurchaseRecord::Pending {
            player: row.try_get("player").map_err(storage)?,
            product_sku: row.try_get("product_sku").map_err(storage)?,
        }),
        "completed" => Ok(PurchaseRecord::Completed(completed_from_row(row)?)),
        other => Err(GameError::Storage(format!("unknown purchase state {other}"))),
    }
}

fn completed_from_row(row: &PgRow) -> Result<CompletedPurchase, GameError> {
    let outcome: Option<serde_json::Value> = row.try_get("outcome").map_err(storage)?;
    let outcome: PurchaseOutcome = decode_json(
        outcome.ok_or_else(|| GameError::Storage("completed purchase missing outcome".into()))?,
    )?;
    let reversal: Option<serde_json::Value> = row.try_get("reversal").map_err(storage)?;
    let reversal: Option<PurchaseReversal> = reversal.map(decode_json).transpose()?;
    Ok(CompletedPurchase {
        player: row.try_get("player").map_err(storage)?,
        outcome,
        reversal,
    })
}

fn decode_json<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Result<T, GameError> {
    serde_json::from_value(value)
        .map_err(|error| GameError::Storage(format!("decode game state row: {error}")))
}

fn seq_to_i64(seq: u64) -> i64 {
    i64::try_from(seq).unwrap_or(i64::MAX)
}

fn storage(error: sqlx::Error) -> GameError {
    GameError::Storage(format!("postgres: {error}"))
}

fn encode(error: serde_json::Error) -> GameError {
    GameError::Storage(format!("encode game state: {error}"))
}

// ---------------------------------------------------------------------------
// Runtime-editable catalog (migrations/0002_game_catalog.sql)
// ---------------------------------------------------------------------------

impl CatalogRepository for PostgresGameState {
    fn load_catalog(&self) -> Result<Option<StoredCatalog>, GameError> {
        let pool = self.pool.clone();
        self.run(async move {
            let Some(meta) = sqlx::query("SELECT revision FROM game_catalog_meta WHERE id = 1")
                .fetch_optional(&pool)
                .await
                .map_err(storage)?
            else {
                return Ok(None);
            };
            let revision: i64 = meta.try_get("revision").map_err(storage)?;
            let rows = sqlx::query(
                "SELECT sku, product, archived FROM game_catalog_products ORDER BY position, sku",
            )
            .fetch_all(&pool)
            .await
            .map_err(storage)?;
            let mut products = Vec::with_capacity(rows.len());
            let mut archived = std::collections::BTreeSet::new();
            for row in rows {
                let product: Product =
                    decode_json(row.try_get::<serde_json::Value, _>("product").map_err(storage)?)?;
                if row.try_get::<bool, _>("archived").map_err(storage)? {
                    archived.insert(row.try_get::<String, _>("sku").map_err(storage)?);
                }
                products.push(product);
            }
            let pool_rows = sqlx::query("SELECT rarity, items FROM game_catalog_pools")
                .fetch_all(&pool)
                .await
                .map_err(storage)?;
            let mut pools = std::collections::BTreeMap::new();
            for row in pool_rows {
                let rarity: Rarity = decode_json(serde_json::Value::String(
                    row.try_get::<String, _>("rarity").map_err(storage)?,
                ))?;
                let items: Vec<String> =
                    decode_json(row.try_get::<serde_json::Value, _>("items").map_err(storage)?)?;
                pools.insert(rarity, items);
            }
            Ok(Some(StoredCatalog {
                file: CatalogFile {
                    products,
                    pools,
                    archived,
                },
                revision: u64::try_from(revision).unwrap_or(0),
            }))
        })
    }

    fn save_catalog(&self, file: &CatalogFile, expected_revision: u64) -> Result<u64, GameError> {
        let pool = self.pool.clone();
        let file = file.clone();
        self.run(async move {
            let mut tx = pool.begin().await.map_err(storage)?;
            // Serialize concurrent saves on the single meta row.
            sqlx::query(
                "INSERT INTO game_catalog_meta (id, revision) VALUES (1, 0) \
                 ON CONFLICT (id) DO NOTHING",
            )
            .execute(&mut *tx)
            .await
            .map_err(storage)?;
            let current: i64 = sqlx::query("SELECT revision FROM game_catalog_meta WHERE id = 1 FOR UPDATE")
                .fetch_one(&mut *tx)
                .await
                .map_err(storage)?
                .try_get("revision")
                .map_err(storage)?;
            let current = u64::try_from(current).unwrap_or(0);
            if current != expected_revision {
                return Err(GameError::CatalogConflict {
                    expected: expected_revision,
                    actual: current,
                });
            }
            let skus: Vec<String> = file.products.iter().map(|p| p.sku().to_string()).collect();
            sqlx::query("DELETE FROM game_catalog_products WHERE NOT (sku = ANY($1))")
                .bind(&skus)
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            for (position, product) in file.products.iter().enumerate() {
                let kind = match product {
                    Product::CardPack(_) => "card_pack",
                    Product::Cosmetic(_) => "cosmetic",
                    Product::BattlePass(_) => "battle_pass",
                };
                sqlx::query(
                    "INSERT INTO game_catalog_products (sku, kind, product, archived, position, updated_at) \
                     VALUES ($1, $2, $3, $4, $5, now()) \
                     ON CONFLICT (sku) DO UPDATE SET kind = EXCLUDED.kind, product = EXCLUDED.product, \
                       archived = EXCLUDED.archived, position = EXCLUDED.position, updated_at = now() \
                     WHERE game_catalog_products.product IS DISTINCT FROM EXCLUDED.product \
                        OR game_catalog_products.archived IS DISTINCT FROM EXCLUDED.archived \
                        OR game_catalog_products.position IS DISTINCT FROM EXCLUDED.position",
                )
                .bind(product.sku())
                .bind(kind)
                .bind(serde_json::to_value(product).map_err(encode)?)
                .bind(file.archived.contains(product.sku()))
                .bind(i32::try_from(position).unwrap_or(i32::MAX))
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            }
            sqlx::query("DELETE FROM game_catalog_pools")
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            for (rarity, items) in &file.pools {
                let rarity_name = serde_json::to_value(rarity).map_err(encode)?;
                let rarity_name = rarity_name.as_str().unwrap_or_default().to_string();
                sqlx::query("INSERT INTO game_catalog_pools (rarity, items) VALUES ($1, $2)")
                    .bind(rarity_name)
                    .bind(serde_json::to_value(items).map_err(encode)?)
                    .execute(&mut *tx)
                    .await
                    .map_err(storage)?;
            }
            let next = current + 1;
            sqlx::query("UPDATE game_catalog_meta SET revision = $1, updated_at = now() WHERE id = 1")
                .bind(seq_to_i64(next))
                .execute(&mut *tx)
                .await
                .map_err(storage)?;
            tx.commit().await.map_err(storage)?;
            Ok(next)
        })
    }
}
