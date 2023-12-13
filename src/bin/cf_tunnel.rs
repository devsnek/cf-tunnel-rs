use base64::{engine::general_purpose::STANDARD, Engine};
use cf_tunnel::{CfApi, HttpBody, HttpService, QuickTunnel, Tunnel, TunnelConfig};
use clap::Parser;
use http::uri::{Authority, Scheme};
use rand::{thread_rng, Fill};
use reqwest::Client;

#[derive(Parser, Debug)]
#[command(version, propagate_version = true)]
struct Args {
    #[arg(short, long)]
    name: Option<String>,
    url: ProxyUrl,
    #[arg(short, long)]
    protocol: Option<cf_tunnel::Protocol>,
    #[arg(short, long)]
    account_token: Option<String>,
}

#[derive(Debug, Clone)]
struct ProxyUrl(Scheme, Authority);

impl std::str::FromStr for ProxyUrl {
    type Err = http::uri::InvalidUri;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let parts: Vec<_> = s.split("://").collect();
        let scheme = parts[0].parse()?;
        let authority = parts[1].parse()?;
        Ok(Self(scheme, authority))
    }
}

#[derive(Debug, Clone)]
struct ProxyService {
    client: Client,
    url: ProxyUrl,
}

impl tower::Service<http::Request<cf_tunnel::HttpBody>> for ProxyService {
    type Response = http::Response<cf_tunnel::HttpBody>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<Self::Response, Self::Error>> + Send>,
    >;

    fn poll_ready(
        &mut self,
        _cx: &mut std::task::Context,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: hyper::Request<cf_tunnel::HttpBody>) -> Self::Future {
        let url = self.url.clone();
        let client = self.client.clone();
        let f = async move {
            use http_body_util::BodyExt;

            let (head, body) = req.into_parts();

            let mut uri = head.uri.into_parts();
            uri.scheme = Some(url.0);
            uri.authority = Some(url.1);
            let uri = http::uri::Uri::from_parts(uri).unwrap();

            // mess because reqwest is on http 0.2 and hyper is on http 1.0

            let resw = client
                .request(
                    reqwest::Method::from_bytes(head.method.as_str().as_bytes()).unwrap(),
                    uri.to_string(),
                )
                .headers(
                    head.headers
                        .iter()
                        .map(|(k, v)| {
                            (
                                reqwest::header::HeaderName::from_bytes(k.as_ref()).unwrap(),
                                reqwest::header::HeaderValue::from_bytes(v.as_ref()).unwrap(),
                            )
                        })
                        .collect(),
                )
                .body(body.collect().await.unwrap().to_bytes())
                .send()
                .await
                .unwrap();

            let mut res = hyper::Response::builder()
                .status(hyper::StatusCode::from_u16(resw.status().as_u16()).unwrap());

            for (k, v) in resw.headers() {
                res = res.header(
                    hyper::header::HeaderName::from_bytes(k.as_ref()).unwrap(),
                    hyper::header::HeaderValue::from_bytes(v.as_ref()).unwrap(),
                );
            }

            use futures::StreamExt;
            let stream = resw.bytes_stream().map(|result| match result {
                Ok(v) => Ok(hyper::body::Frame::data(v)),
                Err(e) => Err(std::io::Error::other(e)),
            });
            let res = res
                .body(HttpBody::new(http_body_util::StreamBody::new(stream)))
                .unwrap();

            Ok(res)
        };

        Box::pin(f)
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CloudflaredTunnelConfig {
    #[serde(rename = "AccountTag")]
    account_tag: String,
    #[serde(rename = "TunnelSecret")]
    tunnel_secret: String,
    #[serde(rename = "TunnelId")]
    tunnel_id: String,
}

impl TunnelConfig for CloudflaredTunnelConfig {
    fn account_tag(&self) -> &str {
        &self.account_tag
    }

    fn secret(&self) -> &str {
        &self.tunnel_secret
    }

    fn id(&self) -> &str {
        &self.tunnel_id
    }
}

fn tunnel_config_path(tunnel_id: &str) -> String {
    std::env::home_dir()
        .unwrap()
        .join(".cloudflared")
        .join(format!("{}.json", tunnel_id))
        .as_path()
        .to_str()
        .unwrap()
        .to_owned()
}

#[tokio::main]
async fn main() {
    let Args {
        name,
        url,
        protocol,
        account_token,
    } = Args::parse();

    let config: Box<dyn TunnelConfig> = match name {
        Some(name) => {
            let cfapi = CfApi::new(account_token.unwrap()).await.unwrap();
            let mut tunnels = cfapi.list_tunnels(Some(name.clone())).await.unwrap();
            let tunnel = match tunnels.pop() {
                Some(tunnel) => {
                    println!("Reusing existing named tunnel {}", tunnel.id);
                    tunnel
                }
                None => {
                    let mut secret = vec![0; 32];
                    secret.try_fill(&mut thread_rng()).unwrap();
                    let tunnel = cfapi.create_tunnel(&name, secret.clone()).await.unwrap();
                    println!("Created new named tunnel {}", tunnel.id);

                    let config = serde_json::to_vec(&CloudflaredTunnelConfig {
                        account_tag: cfapi.account_id().to_owned(),
                        tunnel_secret: STANDARD.encode(secret),
                        tunnel_id: tunnel.id.to_string(),
                    })
                    .unwrap();
                    std::fs::write(tunnel_config_path(&tunnel.id.to_string()), config).unwrap();

                    tunnel
                }
            };

            let data = std::fs::read(tunnel_config_path(&tunnel.id.to_string())).unwrap();
            let config: CloudflaredTunnelConfig = serde_json::from_slice(&data).unwrap();
            Box::new(config)
        }
        None => {
            let tunnel = QuickTunnel::create().await.unwrap();
            println!("https://{}", tunnel.hostname);
            Box::new(tunnel)
        }
    };

    let tunnel = Tunnel::new().await.unwrap();

    let service = ProxyService {
        client: Client::new(),
        url: url.to_owned(),
    };

    tunnel
        .serve(&config, HttpService::new(service), protocol)
        .await
        .unwrap();
}
