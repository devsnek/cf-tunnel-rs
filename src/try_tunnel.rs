use crate::Error;
use base64::{engine::general_purpose::STANDARD, Engine};
use uuid::Uuid;

#[derive(Debug, serde::Deserialize)]
struct TryTunnelResponse {
    success: bool,
    errors: Vec<()>,
    result: TryTunnelTunnel,
}

#[derive(Debug, serde::Deserialize)]
struct TryTunnelTunnel {
    r#type: String,
    id: String,
    name: String,
    hostname: String,
    account_tag: String,
    secret: String,
}

pub struct TryTunnel {
    pub hostname: String,
    pub account_tag: String,
    pub id: Uuid,
    pub secret: Vec<u8>,
}

pub async fn create_try_tunnel() -> Result<TryTunnel, Error> {
    let client = reqwest::Client::new();
    let r: TryTunnelResponse = client
        .post("https://api.trycloudflare.com/tunnel")
        .send()
        .await?
        .json()
        .await?;

    let id = Uuid::parse_str(&r.result.id)?;
    let secret = STANDARD.decode(r.result.secret)?;

    Ok(TryTunnel {
        hostname: r.result.hostname,
        account_tag: r.result.account_tag,
        id,
        secret,
    })
}
