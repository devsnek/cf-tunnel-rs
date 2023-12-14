use crate::{rpc::RpcClient, util::Join, Error};
use async_compat::CompatExt;
use bytes::Bytes;
use futures::TryStreamExt;
use http_body_util::{
    combinators::BoxBody, BodyExt, BodyStream, Empty as EmptyBody, Full as FullBody,
};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use quinn::{RecvStream, SendStream};
use std::{collections::HashMap, net::SocketAddr, sync::Arc};
use tokio::io::{duplex, AsyncRead, AsyncReadExt, DuplexStream};
use tokio_util::io::StreamReader;

pub(crate) mod quic_metadata_protocol_capnp {
    #![allow(clippy::all)]
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/quic_metadata_protocol_capnp.rs"));
}

pub const DATA_SIG: [u8; 6] = [0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E];
pub const RPC_SIG: [u8; 6] = [0x52, 0xBB, 0x82, 0x5C, 0xDB, 0x65];
pub const VERSION: [u8; 2] = *b"01";

async fn build_http_request(
    uri: hyper::Uri,
    metadata: HashMap<String, String>,
    mut rx: impl AsyncRead + Send + Sync + Unpin,
) -> Result<Request<BoxBody<Bytes, std::io::Error>>, Error> {
    let Some(method) = metadata.get("HttpMethod") else {
        return Err(Error::NoMethod);
    };
    let method = Method::from_bytes(method.as_bytes())?;

    let mut builder = Request::builder()
        .uri(
            uri.path_and_query()
                .map(|p| p.to_string())
                .unwrap_or("/".into()),
        )
        .method(method)
        .header("host", uri.host().unwrap());

    for (k, v) in &metadata {
        if k.starts_with("HttpHeader") {
            if let Some(key) = k.split(':').nth(1) {
                builder = builder.header(key, v);
            }
        }
    }

    let body = if let Some(len) = metadata.get("HttpHeader:Content-Length") {
        let mut body = vec![0; len.parse()?];
        rx.read_exact(&mut body).await?;
        BoxBody::new(FullBody::new(Bytes::from(body)).map_err(std::io::Error::other))
    } else {
        BoxBody::new(EmptyBody::new().map_err(std::io::Error::other))
    };

    let req = builder.body(body)?;

    Ok(req)
}

fn create_tls_client_config() -> Result<rustls::ClientConfig, Error> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs()? {
        roots.add(&rustls::Certificate((*cert).to_owned()))?;
    }
    let mut x = std::io::BufReader::new(include_str!("../cf_root.pem").as_bytes());
    for x in rustls_pemfile::read_all(&mut x).flatten() {
        if let rustls_pemfile::Item::X509Certificate(cert) = x {
            roots.add(&rustls::Certificate((*cert).to_owned()))?;
        }
    }

    let mut client_config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();

    client_config.key_log = Arc::new(rustls::KeyLogFile::new());

    Ok(client_config)
}

#[derive(Debug)]
pub struct Quic {
    conn: quinn::Connection,
    rpc: RpcClient,
}

impl Quic {
    pub async fn connect(remote: SocketAddr) -> Result<Self, Error> {
        let client_crypto = create_tls_client_config()?;
        let client_config = quinn::ClientConfig::new(Arc::new(client_crypto));
        let endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())?;
        let conn = endpoint
            .connect_with(client_config, remote, "quic.cftunnel.com")?
            .await?;

        let control_stream = conn.open_bi().await?;
        let rpc = RpcClient::new(control_stream.0, control_stream.1);

        Ok(Self { conn, rpc })
    }
}

#[async_trait::async_trait]
impl crate::ProtocolImpl for Quic {
    fn rpc(&self) -> &RpcClient {
        &self.rpc
    }

    async fn accept(&mut self) -> Result<Option<DuplexStream>, Error> {
        loop {
            let (tx, mut rx) = match self.conn.accept_bi().await {
                Ok(v) => v,
                Err(quinn::ConnectionError::LocallyClosed) => {
                    return Ok(None);
                }
                Err(e) => {
                    return Err(e.into());
                }
            };

            let mut sig = [0; 6];
            rx.read_exact(&mut sig).await?;

            if sig == DATA_SIG {
                let (virt, other) = duplex(4096);

                tokio::task::spawn(async {
                    handle_data_stream(tx, rx, other).await.unwrap();
                });

                return Ok(Some(virt));
            } else if sig == RPC_SIG {
                unimplemented!("RPC STREAM!");
            } else {
                // ??
            }
        }
    }
}

async fn handle_data_stream(
    mut tun_tx: SendStream,
    mut tun_rx: RecvStream,
    mut other_virt: DuplexStream,
) -> Result<(), Error> {
    let mut v = [0; 2];
    tun_rx.read_exact(&mut v).await?;
    if v != VERSION {
        return Err(Error::VersionMismatch);
    }

    let r = capnp_futures::serialize::read_message(&mut tun_rx.compat_mut(), Default::default())
        .await?;
    let r = r.into_typed::<quic_metadata_protocol_capnp::connect_request::Owned>();
    let mtype = r.get()?.get_type()?;
    match mtype {
        quic_metadata_protocol_capnp::ConnectionType::Http
        | quic_metadata_protocol_capnp::ConnectionType::Websocket => {
            let uri = r.get()?.get_dest()?.to_string()?.parse().unwrap();
            let mut metadata = HashMap::new();
            {
                let m = r.get()?.get_metadata()?;
                for i in 0..m.len() {
                    let entry = m.reborrow().get(i);
                    metadata.insert(entry.get_key()?.to_string()?, entry.get_val()?.to_string()?);
                }
            }
            drop(r);

            if mtype == quic_metadata_protocol_capnp::ConnectionType::Websocket {
                metadata.insert("HttpHeader:Connection".into(), "upgrade".into());
                metadata.insert("HttpHeader:Upgrade".into(), "websocket".into());
            }

            let (mut send_request, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(other_virt)).await?;
            tokio::task::spawn(async move {
                connection.with_upgrades().await.unwrap();
            });

            let request = build_http_request(uri, metadata, &mut tun_rx).await?;
            send_request.ready().await?;
            let response = send_request.send_request(request).await?;

            let mut builder = capnp::message::TypedBuilder::<
                quic_metadata_protocol_capnp::connect_response::Owned,
            >::new_default();

            {
                let root = builder.init_root();
                let mut m = root.init_metadata((response.headers().len() + 1) as _);

                let mut entry = m.reborrow().get(0);
                entry.set_key("HttpStatus".into());
                entry.set_val(format!("{}", response.status().as_u16()).as_str().into());

                let mut i = 1;
                for (k, v) in response.headers().iter() {
                    let mut entry = m.reborrow().get(i);
                    i += 1;
                    entry.set_key(format!("HttpHeader:{}", k.as_str()).as_str().into());
                    entry.set_val(v.to_str()?.into());
                }
            }

            tun_tx.write_all(&DATA_SIG).await?;
            tun_tx.write_all(&VERSION).await?;

            capnp_futures::serialize::write_message(&mut tun_tx.compat_mut(), builder.into_inner())
                .await?;

            if response.status() == 101 {
                let upgrade = hyper::upgrade::on(response).await?;
                let parts = upgrade.downcast::<TokioIo<DuplexStream>>().unwrap();
                let mut other_virt = parts.io.into_inner();
                tokio::io::copy_bidirectional(&mut other_virt, &mut Join::new(tun_tx, tun_rx))
                    .await?;
            } else {
                let (_, body) = response.into_parts();

                let mut body_reader = StreamReader::new(
                    BodyStream::new(body)
                        .map_ok(|f| f.into_data().unwrap())
                        .map_err(std::io::Error::other),
                );

                tokio::io::copy(&mut body_reader, &mut tun_tx).await?;
            }
        }
        quic_metadata_protocol_capnp::ConnectionType::Tcp => {
            let mut builder = capnp::message::TypedBuilder::<
                quic_metadata_protocol_capnp::connect_response::Owned,
            >::new_default();
            {
                let root = builder.init_root();
                root.init_metadata(0);
            }
            capnp_futures::serialize::write_message(&mut tun_tx.compat_mut(), builder.into_inner())
                .await?;
            tokio::io::copy_bidirectional(&mut other_virt, &mut Join::new(tun_tx, tun_rx)).await?;
        }
    }

    Ok(())
}
