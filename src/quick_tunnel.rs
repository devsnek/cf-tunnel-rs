use crate::Error;

#[derive(Debug, serde::Deserialize)]
struct QuickTunnelResponse {
    success: bool,
    errors: Vec<()>,
    result: QuickTunnelTunnel,
}

#[derive(Debug, serde::Deserialize)]
struct QuickTunnelTunnel {
    r#type: String,
    id: String,
    name: String,
    hostname: String,
    account_tag: String,
    secret: String,
}

pub struct QuickTunnel {
    pub hostname: String,
    pub account_tag: String,
    pub id: String,
    pub secret: String,
}

impl QuickTunnel {
    pub async fn create() -> Result<Self, Error> {
        let client = reqwest::Client::new();
        let r: QuickTunnelResponse = client
            .post("https://api.trycloudflare.com/tunnel")
            .send()
            .await?
            .json()
            .await?;

        Ok(QuickTunnel {
            hostname: r.result.hostname,
            account_tag: r.result.account_tag,
            id: r.result.id,
            secret: r.result.secret,
        })
    }
}

impl crate::TunnelConfig for QuickTunnel {
    fn account_tag(&self) -> &str {
        &self.account_tag
    }

    fn secret(&self) -> &str {
        &self.secret
    }

    fn id(&self) -> &str {
        &self.id
    }
}
