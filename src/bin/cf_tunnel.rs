use base64::{engine::general_purpose::STANDARD, Engine};
use cf_tunnel::{CfApi, QuickTunnel, Tunnel, TunnelConfig};
use clap::Parser;
use http::uri::Authority;
use rand::{thread_rng, Fill};

#[derive(Parser, Debug)]
#[command(version, propagate_version = true)]
struct Args {
    #[arg(short, long)]
    name: Option<String>,
    url: Authority,
    #[arg(short, long)]
    protocol: Option<cf_tunnel::Protocol>,
    #[arg(short, long)]
    account_token: Option<String>,
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

    let mut tunnel = Tunnel::new(&config, protocol).await.unwrap();

    while let Some(mut connection) = tunnel.accept().await.unwrap() {
        let url = url.clone().to_string();
        tokio::task::spawn(async move {
            let mut stream = tokio::net::TcpStream::connect(url).await.unwrap();
            tokio::io::copy_bidirectional(&mut connection, &mut stream)
                .await
                .unwrap();
        });
    }
}
