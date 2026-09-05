//! Port of `processors/main_processor.go` — wires Kafka → processor → outputs.

use std::sync::Arc;

use futures::future::FutureExt;
use tokio_util::sync::CancellationToken;

use crate::cache::Cache;
use crate::config::database::{Database, DbConfig};
use crate::config::kafka::{
    ConsumerGroup, ConsumerGroupConfig, MessageProducer, Producer, ProducerConfig, Record,
    ServerConfig,
};
use crate::config::redis::{RedisConfig, RedisDb};
use crate::models::{ApiStore, CacheStore, Cacher, ChargeCache, FlagStore, Flagger};
use crate::processors::events_processor::{
    CacheService, EventEnrichmentService, EventProcessor, EventProducerService,
    SubscriptionRefreshService,
};
use crate::utils::{env as envutil, error_tracker, RagoError};

// --- environment variable names (verbatim from the Go service) ---
const ENV_ENV: &str = "ENV";
const ENV_DB_MAX_CONNS: &str = "LAGO_EVENTS_PROCESSOR_DATABASE_MAX_CONNECTIONS";
const ENV_KAFKA_BOOTSTRAP_SERVERS: &str = "LAGO_KAFKA_BOOTSTRAP_SERVERS";
const ENV_KAFKA_CONSUMER_GROUP: &str = "LAGO_KAFKA_CONSUMER_GROUP";
const ENV_KAFKA_ENRICHED_EXPANDED_TOPIC: &str = "LAGO_KAFKA_ENRICHED_EVENTS_EXPANDED_TOPIC";
const ENV_KAFKA_ENRICHED_TOPIC: &str = "LAGO_KAFKA_ENRICHED_EVENTS_TOPIC";
const ENV_KAFKA_IN_ADVANCE_TOPIC: &str = "LAGO_KAFKA_EVENTS_CHARGED_IN_ADVANCE_TOPIC";
const ENV_KAFKA_DEAD_LETTER_TOPIC: &str = "LAGO_KAFKA_EVENTS_DEAD_LETTER_TOPIC";
const ENV_KAFKA_PASSWORD: &str = "LAGO_KAFKA_PASSWORD";
const ENV_KAFKA_RAW_EVENTS_TOPIC: &str = "LAGO_KAFKA_RAW_EVENTS_TOPIC";
const ENV_KAFKA_SCRAM_ALGORITHM: &str = "LAGO_KAFKA_SCRAM_ALGORITHM";
const ENV_KAFKA_TLS: &str = "LAGO_KAFKA_TLS";
const ENV_KAFKA_USERNAME: &str = "LAGO_KAFKA_USERNAME";
const ENV_REDIS_CACHE_DB: &str = "LAGO_REDIS_CACHE_DB";
const ENV_REDIS_CACHE_PASSWORD: &str = "LAGO_REDIS_CACHE_PASSWORD";
const ENV_REDIS_CACHE_URL: &str = "LAGO_REDIS_CACHE_URL";
const ENV_REDIS_CACHE_TLS: &str = "LAGO_REDIS_CACHE_TLS";
const ENV_REDIS_STORE_DB: &str = "LAGO_REDIS_STORE_DB";
const ENV_REDIS_STORE_PASSWORD: &str = "LAGO_REDIS_STORE_PASSWORD";
const ENV_REDIS_STORE_URL: &str = "LAGO_REDIS_STORE_URL";
const ENV_REDIS_STORE_TLS: &str = "LAGO_REDIS_STORE_TLS";

const FLAG_STORE_NAME: &str = "subscription_refreshed_v2";

pub struct Config {
    pub cache: Option<Arc<Cache>>,
}

fn getenv(key: &str) -> String {
    std::env::var(key).unwrap_or_default()
}

fn init_producer(
    server_config: &ServerConfig,
    topic_env: &str,
) -> anyhow::Result<Arc<dyn MessageProducer>> {
    let topic = getenv(topic_env);
    if topic.is_empty() {
        anyhow::bail!("{topic_env} variable is required");
    }
    let producer = Producer::new(server_config, ProducerConfig { topic })?;
    producer.ping()?;
    Ok(Arc::new(producer))
}

async fn init_flag_store(name: &str) -> anyhow::Result<Arc<dyn Flagger>> {
    let redis_db = envutil::get_env_as_int(ENV_REDIS_STORE_DB, 0)?;
    // Deprecated: use LAGO_REDIS_STORE_TLS instead.
    let legacy_tls = getenv(ENV_ENV) == "production";

    let redis_config = RedisConfig {
        address: getenv(ENV_REDIS_STORE_URL),
        password: getenv(ENV_REDIS_STORE_PASSWORD),
        db: redis_db,
        use_tls: envutil::get_env_as_bool(ENV_REDIS_STORE_TLS, legacy_tls),
        use_tracer: false,
    };

    let db = RedisDb::new(redis_config).await?;
    Ok(Arc::new(FlagStore::new(&db, name)))
}

async fn init_charge_cache_store() -> anyhow::Result<ChargeCache> {
    let redis_db = envutil::get_env_as_int(ENV_REDIS_CACHE_DB, 0)?;

    let redis_config = RedisConfig {
        address: getenv(ENV_REDIS_CACHE_URL),
        password: getenv(ENV_REDIS_CACHE_PASSWORD),
        db: redis_db,
        use_tls: envutil::get_env_as_bool(ENV_REDIS_CACHE_TLS, false),
        use_tracer: false,
    };

    let db = RedisDb::new(redis_config).await?;
    let cache_store: Arc<dyn Cacher> = Arc::new(CacheStore::new(&db));
    Ok(ChargeCache::new(cache_store))
}

/// `StartProcessingEvents`.
pub async fn start_processing_events(token: CancellationToken, config: Config) {
    let server_brokers = envutil::parse_brokers_env(&getenv(ENV_KAFKA_BOOTSTRAP_SERVERS));
    if server_brokers.is_empty() {
        ::tracing::error!("brokers not found");
        panic!("brokers not found");
    }

    let kafka_config = ServerConfig {
        scram_algorithm: getenv(ENV_KAFKA_SCRAM_ALGORITHM),
        tls: envutil::get_env_as_bool(ENV_KAFKA_TLS, false),
        servers: server_brokers,
        username: getenv(ENV_KAFKA_USERNAME),
        password: getenv(ENV_KAFKA_PASSWORD),
    };

    let enriched_producer =
        init_producer(&kafka_config, ENV_KAFKA_ENRICHED_TOPIC).unwrap_or_else(|e| {
            error_tracker::log_and_panic(
                &RagoError::from(e),
                "failed to initialize enriched events producer",
            )
        });
    let enriched_expanded_producer =
        init_producer(&kafka_config, ENV_KAFKA_ENRICHED_EXPANDED_TOPIC).unwrap_or_else(|e| {
            error_tracker::log_and_panic(
                &RagoError::from(e),
                "failed to initialize enriched events expanded producer",
            )
        });
    let in_advance_producer = init_producer(&kafka_config, ENV_KAFKA_IN_ADVANCE_TOPIC)
        .unwrap_or_else(|e| {
            error_tracker::log_and_panic(
                &RagoError::from(e),
                "failed to initialize events charged in advance producer",
            )
        });
    let dead_letter_producer = init_producer(&kafka_config, ENV_KAFKA_DEAD_LETTER_TOPIC)
        .unwrap_or_else(|e| {
            error_tracker::log_and_panic(
                &RagoError::from(e),
                "failed to initialize events dead letter queue producer",
            )
        });

    // API store (only when the in-memory cache is not used).
    let mut api_store: Option<ApiStore> = None;
    let mut db_handle: Option<Database> = None;
    if config.cache.is_none() {
        let max_conns = envutil::get_env_as_int(ENV_DB_MAX_CONNS, 200).unwrap_or_else(|e| {
            error_tracker::log_and_panic(
                &RagoError::Message(e.to_string()),
                "Error converting max connections into integer",
            )
        });

        let db_config = DbConfig {
            url: getenv("DATABASE_URL"),
            max_conns: max_conns as u32,
        };
        let db = Database::new_connection(db_config)
            .await
            .unwrap_or_else(|e| {
                error_tracker::log_and_panic(
                    &RagoError::from(e),
                    "Error connecting to the database",
                )
            });
        api_store = Some(ApiStore::new(db.clone()));
        db_handle = Some(db);
    }

    let flagger = init_flag_store(FLAG_STORE_NAME).await.unwrap_or_else(|e| {
        error_tracker::log_and_panic(&RagoError::from(e), "Error connecting to the flag store")
    });

    let charge_cache_store = init_charge_cache_store().await.unwrap_or_else(|e| {
        error_tracker::log_and_panic(
            &RagoError::from(e),
            "Error connecting to the charge cache store",
        )
    });

    let processor = Arc::new(EventProcessor::new(
        EventEnrichmentService::new(api_store, config.cache.clone()),
        EventProducerService::new(
            enriched_producer,
            enriched_expanded_producer,
            in_advance_producer,
            dead_letter_producer,
        ),
        SubscriptionRefreshService::new(flagger),
        CacheService::new(charge_cache_store),
    ));

    let process_records = {
        let processor = processor.clone();
        Arc::new(move |records: Vec<Record>| {
            let processor = processor.clone();
            async move { processor.process_events(records).await }.boxed()
        })
    };

    let cg = ConsumerGroup::new(
        &kafka_config,
        ConsumerGroupConfig {
            topic: getenv(ENV_KAFKA_RAW_EVENTS_TOPIC),
            consumer_group: getenv(ENV_KAFKA_CONSUMER_GROUP),
            process_records,
        },
    )
    .unwrap_or_else(|e| {
        error_tracker::log_and_panic(&RagoError::from(e), "Error starting the event consumer")
    });

    ::tracing::info!("Starting event consumer");
    cg.start(token).await;
    ::tracing::info!("Event processor stopped");

    if let Some(db) = db_handle {
        db.close().await;
    }
}
