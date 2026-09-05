//! Port of `config/kafka/kafka.go` — build a librdkafka `ClientConfig` from the
//! service config, including SCRAM/TLS. (franz-go → rdkafka.)

use rdkafka::ClientConfig;

pub const SCRAM_256: &str = "SCRAM-SHA-256";
pub const SCRAM_512: &str = "SCRAM-SHA-512";

#[derive(Debug, Clone, Default)]
pub struct ServerConfig {
    pub scram_algorithm: String,
    pub tls: bool,
    pub servers: Vec<String>,
    pub username: String,
    pub password: String,
}

impl ServerConfig {
    /// Base client config shared by producers and consumers.
    pub fn base_client_config(&self) -> ClientConfig {
        let mut cfg = ClientConfig::new();
        cfg.set("bootstrap.servers", self.servers.join(","));

        let scram = !self.scram_algorithm.is_empty();
        match (scram, self.tls) {
            (true, true) => {
                cfg.set("security.protocol", "SASL_SSL");
            }
            (true, false) => {
                cfg.set("security.protocol", "SASL_PLAINTEXT");
            }
            (false, true) => {
                cfg.set("security.protocol", "SSL");
            }
            (false, false) => {}
        }

        if scram {
            cfg.set("sasl.mechanism", &self.scram_algorithm);
            cfg.set("sasl.username", &self.username);
            cfg.set("sasl.password", &self.password);
        }

        cfg
    }
}
