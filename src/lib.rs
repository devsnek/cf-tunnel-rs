use bytes::Bytes;
use http_body_util::combinators::BoxBody;
use hyper::{Request, Response};
use rand::{seq::SliceRandom, thread_rng};
use std::net::SocketAddr;
use tower::{util::BoxCloneService, BoxError};
use uuid::Uuid;

mod config;
mod error;
mod http2;
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

pub type HttpBody = BoxBody<Bytes, std::io::Error>;
pub type HttpService = BoxCloneService<Request<HttpBody>, Response<HttpBody>, BoxError>;

pub struct Tunnel {
    edge_addrs: Vec<SocketAddr>,
    uuid: Uuid,
}

impl Tunnel {
    pub async fn new() -> Result<Self, Error> {
        let edge_addrs = edge_discovery().await?;
        let uuid = Uuid::new_v4();

        Ok(Tunnel { edge_addrs, uuid })
    }

    pub async fn serve(
        &self,
        config: &impl IntoTunnelConfig,
        service: HttpService,
        protocol: Option<Protocol>,
    ) -> Result<(), Error> {
        let mut rng = thread_rng();
        let edge_addr = self.edge_addrs.choose(&mut rng).unwrap();

        let mut conn: Box<dyn ProtocolImpl> = match protocol.unwrap_or(Protocol::Quic) {
            Protocol::Quic => Box::new(quic::Quic::connect(*edge_addr).await?),
            Protocol::Http2 => Box::new(http2::Http2::connect(*edge_addr).await?),
        };

        conn.rpc()
            .register_connection(
                config.account_tag(),
                config.secret(),
                config.id(),
                0,
                &self.uuid,
            )
            .await?;

        let r = conn.serve(tower::util::BoxCloneService::new(service)).await;

        conn.rpc().unregister_connection().await?;

        r
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Protocol {
    Quic,
    Http2,
}

#[async_trait::async_trait]
trait ProtocolImpl {
    fn rpc(&self) -> &rpc::RpcClient;
    async fn serve(&mut self, service: HttpService) -> Result<(), Error>;
}

pub trait IntoTunnelConfig {
    fn account_tag(&self) -> &str;
    fn secret(&self) -> &[u8];
    fn id(&self) -> &Uuid;
}
