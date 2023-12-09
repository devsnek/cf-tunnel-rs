use std::net::SocketAddr;
use uuid::Uuid;

mod config;
mod error;
mod quic;
mod rpc;
mod try_tunnel;

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

    Ok(result)
}

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

    pub async fn test(&self) -> Result<(), Error> {
        let try_tunnel = try_tunnel::create_try_tunnel().await.unwrap();
        println!("https://{}", try_tunnel.hostname);

        let quic = quic::Quic::connect(self.edge_addrs[0]).await.unwrap();

        quic.register_connection(
            &try_tunnel.account_tag,
            &try_tunnel.secret,
            &try_tunnel.id,
            0,
            &self.uuid,
        )
        .await?;

        quic.serve().await?;

        Ok(())
    }
}

pub type BoxBody =
    http_body_util::combinators::UnsyncBoxBody<bytes::Bytes, std::convert::Infallible>;

async fn handle_http_request(
    req: http::request::Request<BoxBody>,
) -> Result<http::response::Response<BoxBody>, Error> {
    let (_head, body) = req.into_parts();
    Ok(http::response::Response::builder()
        .status(200)
        .body(body)
        .unwrap())
}
