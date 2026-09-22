//! Persistent product service for Ascendancy.
//!
//! Gameplay stays in the deterministic rules core; this module owns durable
//! player/product state: accounts, profiles, collections, decks, matchmaking,
//! results, shop orders, and entitlements. Mutations are written as one atomic
//! JSON snapshot so a process restart cannot lose a half-granted purchase.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{de::DeserializeOwned, Deserialize, Serialize};

use crate::cards::{load_pack, open_pack, CardPool};
use crate::deck::validate_deck_slugs;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProductError {
    NotFound(&'static str),
    Conflict(String),
    Invalid(String),
    Payment(String),
    Persistence(String),
}

impl std::fmt::Display for ProductError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound(kind) => write!(f, "{kind} not found"),
            Self::Conflict(message) => write!(f, "conflict: {message}"),
            Self::Invalid(message) => write!(f, "invalid: {message}"),
            Self::Payment(message) => write!(f, "payment: {message}"),
            Self::Persistence(message) => write!(f, "persistence: {message}"),
        }
    }
}

impl std::error::Error for ProductError {}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub email: String,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Profile {
    pub account_id: String,
    pub display_name: String,
    pub rating: i32,
    pub wins: u32,
    pub losses: u32,
    pub mint_account_id: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerCollection {
    pub account_id: String,
    /// Card slug -> owned copies.
    pub cards: BTreeMap<String, u32>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlayerDeck {
    pub id: String,
    pub account_id: String,
    pub name: String,
    pub cards: Vec<String>,
    pub active: bool,
    pub updated_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueueEntry {
    pub account_id: String,
    pub rating: i32,
    pub deck_id: String,
    pub queued_at_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MatchStatus {
    Ready,
    Complete,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchRecord {
    pub id: String,
    pub players: [String; 2],
    pub deck_ids: [String; 2],
    /// Chosen by the server and persisted before the match begins.
    pub seed: u64,
    pub status: MatchStatus,
    pub winner: Option<String>,
    pub final_state_hash: Option<u64>,
    pub created_at_ms: i64,
    pub completed_at_ms: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopOffer {
    pub pack_id: String,
    pub title: String,
    pub price_micros: i64,
    pub enabled: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum OrderStatus {
    PendingPayment,
    Paid,
    Granted,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShopOrder {
    pub id: String,
    pub account_id: String,
    pub idempotency_key: String,
    pub pack_id: String,
    pub price_micros: i64,
    pub status: OrderStatus,
    pub mint_reference: Option<String>,
    pub pack_seed: Option<u64>,
    pub granted_cards: Vec<String>,
    pub error: Option<String>,
    pub created_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entitlement {
    pub id: String,
    pub account_id: String,
    pub sku: String,
    pub order_id: String,
    pub granted_at_ms: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PurchaseOutcome {
    pub order: ShopOrder,
    pub cards: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Snapshot {
    next_id: u64,
    accounts: BTreeMap<String, Account>,
    profiles: BTreeMap<String, Profile>,
    collections: BTreeMap<String, PlayerCollection>,
    decks: BTreeMap<String, PlayerDeck>,
    queue: BTreeMap<String, QueueEntry>,
    matches: BTreeMap<String, MatchRecord>,
    offers: BTreeMap<String, ShopOffer>,
    orders: BTreeMap<String, ShopOrder>,
    order_keys: BTreeMap<String, String>,
    entitlements: BTreeMap<String, Entitlement>,
}

impl Default for Snapshot {
    fn default() -> Self {
        let offer = ShopOffer {
            pack_id: "standard".to_string(),
            title: "Standard Pack".to_string(),
            price_micros: 990_000,
            enabled: true,
        };
        Self {
            next_id: 1,
            accounts: BTreeMap::new(),
            profiles: BTreeMap::new(),
            collections: BTreeMap::new(),
            decks: BTreeMap::new(),
            queue: BTreeMap::new(),
            matches: BTreeMap::new(),
            offers: BTreeMap::from([(offer.pack_id.clone(), offer)]),
            orders: BTreeMap::new(),
            order_keys: BTreeMap::new(),
            entitlements: BTreeMap::new(),
        }
    }
}

impl Snapshot {
    fn id(&mut self, prefix: &str) -> String {
        let id = format!("{prefix}_{:016x}", self.next_id);
        self.next_id += 1;
        id
    }
}

/// A restart-safe store. Every successful mutation is persisted by temp-file +
/// rename, so readers see either the old complete snapshot or the new one.
pub struct ProductStore {
    path: PathBuf,
    state: Mutex<Snapshot>,
}

impl ProductStore {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, ProductError> {
        let path = path.into();
        let state = if path.exists() {
            read_json(&path)?
        } else {
            Snapshot::default()
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn in_memory() -> Self {
        Self {
            path: PathBuf::new(),
            state: Mutex::new(Snapshot::default()),
        }
    }

    fn save(&self, state: &Snapshot) -> Result<(), ProductError> {
        if self.path.as_os_str().is_empty() {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ProductError::Persistence(e.to_string()))?;
        }
        let bytes = serde_json::to_vec_pretty(state)
            .map_err(|e| ProductError::Persistence(e.to_string()))?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, bytes).map_err(|e| ProductError::Persistence(e.to_string()))?;
        std::fs::rename(&temp, &self.path)
            .map_err(|e| ProductError::Persistence(e.to_string()))
    }

    pub fn create_account(
        &self,
        email: impl Into<String>,
        display_name: impl Into<String>,
        mint_account_id: impl Into<String>,
        starter_cards: &[String],
    ) -> Result<(Account, Profile), ProductError> {
        let email = email.into().trim().to_ascii_lowercase();
        let display_name = display_name.into().trim().to_string();
        let mint_account_id = mint_account_id.into();
        if !email.contains('@') || display_name.is_empty() || mint_account_id.is_empty() {
            return Err(ProductError::Invalid(
                "email, display name, and Mint account are required".to_string(),
            ));
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.accounts.values().any(|account| account.email == email) {
            return Err(ProductError::Conflict("email already registered".to_string()));
        }
        let id = state.id("player");
        let account = Account {
            id: id.clone(),
            email,
            created_at_ms: now_ms(),
        };
        let profile = Profile {
            account_id: id.clone(),
            display_name,
            rating: 1_000,
            wins: 0,
            losses: 0,
            mint_account_id,
        };
        let mut cards = BTreeMap::new();
        for slug in starter_cards {
            *cards.entry(slug.clone()).or_insert(0) += 1;
        }
        state.accounts.insert(id.clone(), account.clone());
        state.profiles.insert(id.clone(), profile.clone());
        state.collections.insert(
            id.clone(),
            PlayerCollection {
                account_id: id,
                cards,
            },
        );
        self.save(&state)?;
        Ok((account, profile))
    }

    pub fn profile(&self, account_id: &str) -> Result<Profile, ProductError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .profiles
            .get(account_id)
            .cloned()
            .ok_or(ProductError::NotFound("profile"))
    }

    pub fn collection(&self, account_id: &str) -> Result<PlayerCollection, ProductError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .collections
            .get(account_id)
            .cloned()
            .ok_or(ProductError::NotFound("collection"))
    }

    pub fn save_deck(
        &self,
        account_id: &str,
        deck_id: Option<&str>,
        name: impl Into<String>,
        cards: Vec<String>,
        make_active: bool,
    ) -> Result<PlayerDeck, ProductError> {
        validate_deck_slugs(&cards, crate::CONTENT_CHAMPIONS_DIR)
            .map_err(|e| ProductError::Invalid(e.to_string()))?;
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let collection = state
            .collections
            .get(account_id)
            .ok_or(ProductError::NotFound("collection"))?;
        for slug in &cards {
            if collection.cards.get(slug).copied().unwrap_or(0) == 0 {
                return Err(ProductError::Invalid(format!("card not owned: {slug}")));
            }
        }
        let id = deck_id
            .map(str::to_string)
            .unwrap_or_else(|| state.id("deck"));
        if let Some(existing) = state.decks.get(&id) {
            if existing.account_id != account_id {
                return Err(ProductError::NotFound("deck"));
            }
        }
        if make_active {
            for deck in state.decks.values_mut() {
                if deck.account_id == account_id {
                    deck.active = false;
                }
            }
        }
        let deck = PlayerDeck {
            id: id.clone(),
            account_id: account_id.to_string(),
            name: name.into(),
            cards,
            active: make_active,
            updated_at_ms: now_ms(),
        };
        state.decks.insert(id, deck.clone());
        self.save(&state)?;
        Ok(deck)
    }

    pub fn decks(&self, account_id: &str) -> Vec<PlayerDeck> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .decks
            .values()
            .filter(|deck| deck.account_id == account_id)
            .cloned()
            .collect()
    }

    pub fn enqueue(&self, account_id: &str) -> Result<QueueEntry, ProductError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.queue.contains_key(account_id) {
            return Err(ProductError::Conflict("already queued".to_string()));
        }
        let rating = state
            .profiles
            .get(account_id)
            .ok_or(ProductError::NotFound("profile"))?
            .rating;
        let deck_id = state
            .decks
            .values()
            .find(|deck| deck.account_id == account_id && deck.active)
            .map(|deck| deck.id.clone())
            .ok_or_else(|| ProductError::Invalid("an active deck is required".to_string()))?;
        let entry = QueueEntry {
            account_id: account_id.to_string(),
            rating,
            deck_id,
            queued_at_ms: now_ms(),
        };
        state.queue.insert(account_id.to_string(), entry.clone());
        self.save(&state)?;
        Ok(entry)
    }

    pub fn form_match(
        &self,
        rating_band: i32,
        entropy: &dyn ServerEntropy,
    ) -> Result<Option<MatchRecord>, ProductError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut queued: Vec<_> = state.queue.values().cloned().collect();
        queued.sort_by(|a, b| a.rating.cmp(&b.rating).then(a.account_id.cmp(&b.account_id)));
        let pair = queued.windows(2).find(|pair| {
            (pair[1].rating - pair[0].rating).abs() <= rating_band
        });
        let Some(pair) = pair else { return Ok(None) };
        let a = pair[0].clone();
        let b = pair[1].clone();
        state.queue.remove(&a.account_id);
        state.queue.remove(&b.account_id);
        let record = MatchRecord {
            id: state.id("match"),
            players: [a.account_id, b.account_id],
            deck_ids: [a.deck_id, b.deck_id],
            seed: entropy.next_u64(),
            status: MatchStatus::Ready,
            winner: None,
            final_state_hash: None,
            created_at_ms: now_ms(),
            completed_at_ms: None,
        };
        state.matches.insert(record.id.clone(), record.clone());
        self.save(&state)?;
        Ok(Some(record))
    }

    pub fn record_result(
        &self,
        match_id: &str,
        winner: &str,
        final_state_hash: u64,
    ) -> Result<MatchRecord, ProductError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let mut record = state
            .matches
            .get(match_id)
            .cloned()
            .ok_or(ProductError::NotFound("match"))?;
        if record.status == MatchStatus::Complete {
            return Ok(record);
        }
        if !record.players.iter().any(|id| id == winner) {
            return Err(ProductError::Invalid("winner is not in match".to_string()));
        }
        let loser = record
            .players
            .iter()
            .find(|id| id.as_str() != winner)
            .cloned()
            .expect("1v1 match has a loser");
        if let Some(profile) = state.profiles.get_mut(winner) {
            profile.rating += 20;
            profile.wins += 1;
        }
        if let Some(profile) = state.profiles.get_mut(&loser) {
            profile.rating = (profile.rating - 20).max(0);
            profile.losses += 1;
        }
        record.status = MatchStatus::Complete;
        record.winner = Some(winner.to_string());
        record.final_state_hash = Some(final_state_hash);
        record.completed_at_ms = Some(now_ms());
        state.matches.insert(match_id.to_string(), record.clone());
        self.save(&state)?;
        Ok(record)
    }

    pub fn offers(&self) -> Vec<ShopOffer> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .offers
            .values()
            .filter(|offer| offer.enabled)
            .cloned()
            .collect()
    }

    pub fn purchase_pack(
        &self,
        account_id: &str,
        pack_id: &str,
        idempotency_key: &str,
        rail: &dyn PaymentRail,
        entropy: &dyn ServerEntropy,
    ) -> Result<PurchaseOutcome, ProductError> {
        if idempotency_key.trim().is_empty() {
            return Err(ProductError::Invalid("idempotency key is required".to_string()));
        }
        let pool = CardPool::load_default().map_err(|e| ProductError::Invalid(e.to_string()))?;
        let pack = load_pack(crate::CONTENT_CARDS_DIR, pack_id)
            .map_err(|e| ProductError::Invalid(e.to_string()))?;
        let (mut order, payer) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let order_key = format!("{account_id}:{idempotency_key}");
            if let Some(order_id) = state.order_keys.get(&order_key) {
                let existing = state.orders.get(order_id).cloned().expect("order index valid");
                return match existing.status {
                    OrderStatus::Granted => Ok(PurchaseOutcome {
                        cards: existing.granted_cards.clone(),
                        order: existing,
                    }),
                    _ => Err(ProductError::Conflict(format!(
                        "order {} is {:?}", existing.id, existing.status
                    ))),
                };
            }
            let payer = state
                .profiles
                .get(account_id)
                .ok_or(ProductError::NotFound("profile"))?
                .mint_account_id
                .clone();
            let offer = state
                .offers
                .get(pack_id)
                .filter(|offer| offer.enabled)
                .cloned()
                .ok_or(ProductError::NotFound("shop offer"))?;
            let order = ShopOrder {
                id: state.id("order"),
                account_id: account_id.to_string(),
                idempotency_key: idempotency_key.to_string(),
                pack_id: pack_id.to_string(),
                price_micros: offer.price_micros,
                status: OrderStatus::PendingPayment,
                mint_reference: None,
                pack_seed: None,
                granted_cards: Vec::new(),
                error: None,
                created_at_ms: now_ms(),
            };
            state.order_keys.insert(order_key, order.id.clone());
            state.orders.insert(order.id.clone(), order.clone());
            self.save(&state)?;
            (order, payer)
        };

        let charge = match rail.charge(&payer, order.price_micros, &order.id) {
            Ok(charge) => charge,
            Err(error) => {
                let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(saved) = state.orders.get_mut(&order.id) {
                    saved.status = OrderStatus::Failed;
                    saved.error = Some(error.clone());
                    order = saved.clone();
                }
                self.save(&state)?;
                return Err(ProductError::Payment(format!("{} ({})", error, order.id)));
            }
        };

        let seed = entropy.next_u64();
        let drops = open_pack(&pool, &pack, seed);
        let name_to_slug: BTreeMap<_, _> = pool
            .entries()
            .iter()
            .map(|entry| (entry.card.name().to_string(), entry.slug.clone()))
            .collect();
        let slugs: Vec<String> = drops
            .iter()
            .filter_map(|card| name_to_slug.get(card.name()).cloned())
            .collect();
        if slugs.len() != pack.cards as usize {
            return Err(ProductError::Invalid("pack contained an unknown card".to_string()));
        }

        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        {
            let saved = state.orders.get_mut(&order.id).expect("pending order exists");
            saved.status = OrderStatus::Paid;
            saved.mint_reference = Some(charge.reference);
            saved.pack_seed = Some(seed);
            order = saved.clone();
        }
        self.save(&state)?;
        grant_order(&mut state, &order.id, &slugs)?;
        self.save(&state)?;
        let order = state.orders.get(&order.id).cloned().expect("granted order");
        Ok(PurchaseOutcome { order, cards: slugs })
    }

    /// Finish orders that were durably marked paid before a restart but not yet
    /// granted. Pack contents are reproduced from the persisted server seed.
    pub fn reconcile_paid_orders(&self) -> Result<usize, ProductError> {
        let pool = CardPool::load_default().map_err(|e| ProductError::Invalid(e.to_string()))?;
        let pending: Vec<ShopOrder> = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .orders
            .values()
            .filter(|order| order.status == OrderStatus::Paid)
            .cloned()
            .collect();
        let mut count = 0;
        for order in pending {
            let pack = load_pack(crate::CONTENT_CARDS_DIR, &order.pack_id)
                .map_err(|e| ProductError::Invalid(e.to_string()))?;
            let cards = open_pack(&pool, &pack, order.pack_seed.ok_or_else(|| {
                ProductError::Invalid(format!("paid order {} has no seed", order.id))
            })?);
            let name_to_slug: BTreeMap<_, _> = pool
                .entries()
                .iter()
                .map(|entry| (entry.card.name().to_string(), entry.slug.clone()))
                .collect();
            let slugs: Vec<String> = cards
                .iter()
                .filter_map(|card| name_to_slug.get(card.name()).cloned())
                .collect();
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            grant_order(&mut state, &order.id, &slugs)?;
            self.save(&state)?;
            count += 1;
        }
        Ok(count)
    }
}

fn grant_order(state: &mut Snapshot, order_id: &str, slugs: &[String]) -> Result<(), ProductError> {
    let order = state
        .orders
        .get(order_id)
        .cloned()
        .ok_or(ProductError::NotFound("order"))?;
    if order.status == OrderStatus::Granted {
        return Ok(());
    }
    if order.status != OrderStatus::Paid {
        return Err(ProductError::Conflict("order is not paid".to_string()));
    }
    let collection = state
        .collections
        .get_mut(&order.account_id)
        .ok_or(ProductError::NotFound("collection"))?;
    for slug in slugs {
        *collection.cards.entry(slug.clone()).or_insert(0) += 1;
    }
    let entitlement = Entitlement {
        id: state.id("entitlement"),
        account_id: order.account_id.clone(),
        sku: format!("pack:open:{}", order.pack_id),
        order_id: order.id.clone(),
        granted_at_ms: now_ms(),
    };
    state.entitlements.insert(entitlement.id.clone(), entitlement);
    let saved = state.orders.get_mut(order_id).expect("order exists");
    saved.status = OrderStatus::Granted;
    saved.granted_cards = slugs.to_vec();
    Ok(())
}

pub trait ServerEntropy: Send + Sync {
    fn next_u64(&self) -> u64;
}

#[derive(Default)]
pub struct OsEntropy;

impl ServerEntropy for OsEntropy {
    fn next_u64(&self) -> u64 {
        rand::random()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentCharge {
    pub reference: String,
}

pub trait PaymentRail: Send + Sync {
    fn charge(
        &self,
        payer_account: &str,
        amount_micros: i64,
        order_id: &str,
    ) -> Result<PaymentCharge, String>;
}

/// Live Mint hold→capture implementation. Credentials are injected at startup;
/// tests use a fake `PaymentRail` and never require keys or network access.
pub struct MintHttpRail {
    client: reqwest::blocking::Client,
    base_url: String,
    api_key: String,
    merchant_account: String,
}

impl MintHttpRail {
    pub fn new(
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        merchant_account: impl Into<String>,
    ) -> Result<Self, ProductError> {
        let client = reqwest::blocking::Client::builder()
            .build()
            .map_err(|e| ProductError::Payment(e.to_string()))?;
        Ok(Self {
            client,
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            merchant_account: merchant_account.into(),
        })
    }

    fn post<T: Serialize, R: DeserializeOwned>(&self, path: &str, body: &T) -> Result<R, String> {
        let response = self
            .client
            .post(format!("{}{}", self.base_url, path))
            .header("x-api-key", &self.api_key)
            .json(body)
            .send()
            .map_err(|e| e.to_string())?;
        let status = response.status();
        if !status.is_success() {
            return Err(format!("Mint returned {status}"));
        }
        let envelope: MintEnvelope<R> = response.json().map_err(|e| e.to_string())?;
        Ok(envelope.data)
    }
}

impl PaymentRail for MintHttpRail {
    fn charge(
        &self,
        payer_account: &str,
        amount_micros: i64,
        order_id: &str,
    ) -> Result<PaymentCharge, String> {
        #[derive(Serialize)]
        struct HoldReq<'a> {
            from: &'a str,
            amount_micros: i64,
            idem_key: String,
        }
        #[derive(Deserialize)]
        struct HoldData {
            hold_id: String,
        }
        #[derive(Serialize)]
        struct CaptureReq<'a> {
            amount_micros: i64,
            to: &'a str,
            idem_key: String,
        }
        let hold: HoldData = self.post(
            "/v1/holds",
            &HoldReq {
                from: payer_account,
                amount_micros,
                idem_key: format!("aeonrift:{order_id}:hold"),
            },
        )?;
        let capture: Result<serde_json::Value, String> = self.post(
            &format!("/v1/holds/{}/capture", hold.hold_id),
            &CaptureReq {
                amount_micros,
                to: &self.merchant_account,
                idem_key: format!("aeonrift:{order_id}:capture"),
            },
        );
        if let Err(error) = capture {
            let _: Result<serde_json::Value, String> = self.post(
                &format!("/v1/holds/{}/release", hold.hold_id),
                &serde_json::json!({ "idem_key": format!("aeonrift:{order_id}:release") }),
            );
            return Err(error);
        }
        Ok(PaymentCharge {
            reference: hold.hold_id,
        })
    }
}

#[derive(Deserialize)]
struct MintEnvelope<T> {
    data: T,
}

fn read_json<T: DeserializeOwned>(path: &Path) -> Result<T, ProductError> {
    let bytes = std::fs::read(path).map_err(|e| ProductError::Persistence(e.to_string()))?;
    serde_json::from_slice(&bytes).map_err(|e| ProductError::Persistence(e.to_string()))
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::*;
    use crate::deck::load_default_presets;

    struct FixedEntropy(AtomicU64);

    impl FixedEntropy {
        fn new(value: u64) -> Self {
            Self(AtomicU64::new(value))
        }
    }

    impl ServerEntropy for FixedEntropy {
        fn next_u64(&self) -> u64 {
            self.0.fetch_add(1, Ordering::SeqCst)
        }
    }

    struct Paid;

    impl PaymentRail for Paid {
        fn charge(&self, _: &str, amount: i64, order: &str) -> Result<PaymentCharge, String> {
            assert_eq!(amount, 990_000);
            Ok(PaymentCharge {
                reference: format!("hold:{order}"),
            })
        }
    }

    fn starter() -> Vec<String> {
        load_default_presets().unwrap()[0].slugs.clone()
    }

    #[test]
    fn persistent_account_deck_match_and_result_roundtrip() {
        let path = std::env::temp_dir().join(format!("aeonrift-product-{}.json", rand::random::<u64>()));
        let store = ProductStore::open(&path).unwrap();
        let cards = starter();
        let (a, _) = store
            .create_account("a@example.com", "A", "acct_a", &cards)
            .unwrap();
        let (b, _) = store
            .create_account("b@example.com", "B", "acct_b", &cards)
            .unwrap();
        store.save_deck(&a.id, None, "A deck", cards.clone(), true).unwrap();
        store.save_deck(&b.id, None, "B deck", cards, true).unwrap();
        store.enqueue(&a.id).unwrap();
        store.enqueue(&b.id).unwrap();
        let record = store
            .form_match(100, &FixedEntropy::new(77))
            .unwrap()
            .unwrap();
        assert_eq!(record.seed, 77);
        store.record_result(&record.id, &a.id, 1234).unwrap();
        drop(store);

        let reopened = ProductStore::open(&path).unwrap();
        assert_eq!(reopened.profile(&a.id).unwrap().wins, 1);
        assert_eq!(reopened.profile(&b.id).unwrap().losses, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn mint_paid_pack_is_server_rolled_granted_and_idempotent() {
        let store = ProductStore::in_memory();
        let cards = starter();
        let (account, _) = store
            .create_account("pack@example.com", "Pack", "acct_pack", &cards)
            .unwrap();
        let entropy = FixedEntropy::new(9001);
        let first = store
            .purchase_pack(&account.id, "standard", "buy-1", &Paid, &entropy)
            .unwrap();
        assert_eq!(first.order.status, OrderStatus::Granted);
        assert_eq!(first.cards.len(), 5);
        let total_after = store.collection(&account.id).unwrap().cards.values().sum::<u32>();
        let replay = store
            .purchase_pack(&account.id, "standard", "buy-1", &Paid, &entropy)
            .unwrap();
        assert_eq!(replay.cards, first.cards);
        assert_eq!(
            store.collection(&account.id).unwrap().cards.values().sum::<u32>(),
            total_after,
            "idempotent purchase must not grant twice"
        );
    }
}
