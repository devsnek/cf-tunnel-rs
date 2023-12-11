use rand::{seq::SliceRandom, thread_rng};
use std::net::SocketAddr;
use uuid::Uuid;

mod config;
mod error;
mod quic;
mod rpc;
pub mod try_tunnel;

// capnp assumes that it exists at the root of the crate :/
use quic::quic_metadata_protocol_capnp;
use rpc::tunnelrpc_capnp;

pub use error::Error;

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
    edge_addrs: Vec<SocketAddr>,
    uuid: Uuid,
}

pub type HttpBody = http_body_util::combinators::BoxBody<bytes::Bytes, std::io::Error>;

impl Tunnel {
    pub async fn new() -> Result<Self, Error> {
        let edge_addrs = edge_discovery().await?;
        let uuid = Uuid::new_v4();

        Ok(Tunnel { edge_addrs, uuid })
    }

    pub async fn serve<S>(&self, config: &impl IntoTunnelConfig, service: S) -> Result<(), Error>
    where
        S: tower::Service<hyper::Request<HttpBody>> + Send + Clone + 'static,
        S::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
        S::Response: Into<hyper::Response<HttpBody>> + Send,
        S::Future: Send,
    {
        let mut rng = thread_rng();
        let edge_addr = self.edge_addrs.choose(&mut rng).unwrap();

        let quic = quic::Quic::connect(*edge_addr).await?;

        quic.rpc
            .register_connection(
                config.account_tag(),
                config.secret(),
                config.id(),
                0,
                &self.uuid,
            )
            .await?;

        let r = quic.serve(service).await;

        quic.rpc.unregister_connection().await?;

        r
    }
}

pub trait IntoTunnelConfig {
    fn account_tag(&self) -> &str;
    fn secret(&self) -> &[u8];
    fn id(&self) -> &Uuid;
}
