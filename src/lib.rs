use base64::{engine::general_purpose::STANDARD, Engine};
use rand::{seq::SliceRandom, thread_rng};
use std::net::SocketAddr;
use tokio::io::DuplexStream;
use uuid::Uuid;

mod cfapi;
mod config;
mod error;
mod http2;
mod quic;
mod quick_tunnel;
mod rpc;
mod util;

// capnp assumes that it exists at the root of the crate :/
use quic::quic_metadata_protocol_capnp;
use rpc::tunnelrpc_capnp;

pub use cfapi::CfApi;
pub use error::Error;
pub use quick_tunnel::QuickTunnel;

async fn edge_discovery() -> Result<Vec<SocketAddr>, Error> {
    const SRV_SERVICE: &str = "v2-origintunneld";
    const SRV_NAME: &str = "argotunnel.com";

    let name = format!("_{SRV_SERVICE}._tcp.{SRV_NAME}.");

    let resolver = hickory_resolver::AsyncResolver::tokio(
        hickory_resolver::config::ResolverConfig::default(),
        hickory_resolver::config::ResolverOpts::default(),
    );

    let srvs = resolver.srv_lookup(name).await?;

    let mut result: Vec<SocketAddr> = vec![];

    for srv in srvs.iter() {
        let ips = resolver.lookup_ip(srv.target().to_ascii()).await?;
        for ip in ips {
            result.push((ip, srv.port()).into());
        }
    }

    if result.is_empty() {
        Err(Error::EdgeDiscoveryFailed)
    } else {
        Ok(result)
    }
}

pub struct Tunnel {
    conn: Box<dyn ProtocolImpl>,
}

impl Tunnel {
    pub async fn new(
        config: &impl TunnelConfig,
        protocol: Option<Protocol>,
    ) -> Result<Self, Error> {
        let edge_addrs = edge_discovery().await?;
        let uuid = Uuid::new_v4();

        let mut rng = thread_rng();
        let edge_addr = edge_addrs.choose(&mut rng).unwrap();

        let conn: Box<dyn ProtocolImpl> = match protocol.unwrap_or(Protocol::Quic) {
            Protocol::Quic => Box::new(quic::Quic::connect(*edge_addr).await?),
            Protocol::Http2 => Box::new(http2::Http2::connect(*edge_addr).await?),
        };

        let id = Uuid::parse_str(config.id())?;
        let secret = STANDARD.decode(config.secret())?;

        conn.rpc()
            .register_connection(config.account_tag(), &secret, &id, 0, &uuid)
            .await?;

        Ok(Tunnel { conn })
    }

    pub async fn accept(&mut self) -> Result<Option<DuplexStream>, Error> {
        self.conn.accept().await
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Protocol {
    Quic,
    Http2,
}

impl std::str::FromStr for Protocol {
    type Err = std::io::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "quic" => Ok(Self::Quic),
            "http2" => Ok(Self::Http2),
            _ => Err(std::io::Error::other(Error::InvalidProtocol)),
        }
    }
}

#[async_trait::async_trait]
trait ProtocolImpl {
    fn rpc(&self) -> &rpc::RpcClient;
    async fn accept(&mut self) -> Result<Option<DuplexStream>, Error>;
}

pub trait TunnelConfig {
    fn account_tag(&self) -> &str;
    fn secret(&self) -> &str;
    fn id(&self) -> &str;
}

impl TunnelConfig for Box<dyn TunnelConfig> {
    fn account_tag(&self) -> &str {
        (**self).account_tag()
    }

    fn secret(&self) -> &str {
        (**self).secret()
    }

    fn id(&self) -> &str {
        (**self).id()
    }
}
