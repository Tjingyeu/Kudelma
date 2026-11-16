pub mod anthropic;
pub mod gemini;
pub mod openai;

use std::time::Duration;

use reqwest::Client;

pub fn http_client() -> Client {
    Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(600))
        .user_agent(format!("kudelma/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client with static settings")
}
