use crate::{rpc::RpcClient, Error, HttpBody, HttpService};
use async_compat::CompatExt;
use futures::TryFutureExt;
use http_body_util::{combinators::BoxBody, BodyExt, Empty as EmptyBody, Full as FullBody};
use hyper::{body::Body, http::Method, Request};
use std::{collections::HashMap, net::SocketAddr, pin::Pin, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tower::Service;

pub(crate) mod quic_metadata_protocol_capnp {
    #![allow(clippy::all)]
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/quic_metadata_protocol_capnp.rs"));
}

pub const DATA_SIG: [u8; 6] = [0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E];
pub const RPC_SIG: [u8; 6] = [0x52, 0xBB, 0x82, 0x5C, 0xDB, 0x65];
pub const VERSION: [u8; 2] = *b"01";

async fn build_http_request(
    uri: String,
    metadata: HashMap<String, String>,
    mut rx: impl AsyncRead + Send + Sync + Unpin,
) -> Result<Request<HttpBody>, Error> {
    let Some(method) = metadata.get("HttpMethod") else {
        return Err(Error::NoMethod);
    };
    let method = Method::from_bytes(method.as_bytes())?;

    let mut builder = Request::builder().uri(uri).method(method);

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
        BoxBody::new(FullBody::new(body.into()).map_err(std::io::Error::other))
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
    inner: QuicInner,
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

        let inner = QuicInner {};

        Ok(Self { conn, rpc, inner })
    }
}

#[async_trait::async_trait]
impl crate::ProtocolImpl for Quic {
    fn rpc(&self) -> &RpcClient {
        &self.rpc
    }

    async fn serve(&mut self, service: HttpService) -> Result<(), Error> {
        loop {
            let (tx, mut rx) = self.conn.accept_bi().await?;

            let mut sig = [0; 6];
            rx.read_exact(&mut sig).await?;

            if sig == DATA_SIG {
                let inner = self.inner.clone();
                let service = service.clone();
                tokio::task::spawn(async move {
                    inner.handle_data_stream(tx, rx, service).await.unwrap();
                });
            } else if sig == RPC_SIG {
                unimplemented!("RPC STREAM!");
            } else {
                // ??
            }
        }
    }
}

#[derive(Debug, Clone)]
struct QuicInner {}

impl QuicInner {
    async fn handle_data_stream(
        &self,
        mut tx: impl AsyncWrite + Send + Sync + Unpin,
        mut rx: impl AsyncRead + Send + Sync + Unpin,
        mut service: HttpService,
    ) -> Result<(), Error> {
        let mut v = [0; 2];
        rx.read_exact(&mut v).await?;
        if v != VERSION {
            return Err(Error::VersionMismatch);
        }

        let r = capnp_futures::serialize::read_message(&mut rx.compat_mut(), Default::default())
            .await?;
        let r = r.into_typed::<quic_metadata_protocol_capnp::connect_request::Owned>();
        let mtype = r.get()?.get_type()?;
        match mtype {
            quic_metadata_protocol_capnp::ConnectionType::Http
            | quic_metadata_protocol_capnp::ConnectionType::Websocket => {
                let uri = r.get()?.get_dest()?.to_string()?;
                let mut metadata = HashMap::new();
                {
                    let m = r.get()?.get_metadata()?;
                    for i in 0..m.len() {
                        let entry = m.reborrow().get(i);
                        metadata
                            .insert(entry.get_key()?.to_string()?, entry.get_val()?.to_string()?);
                    }
                }
                drop(r);

                let result: Result<_, Box<dyn std::error::Error + Send + Sync>> =
                    build_http_request(uri, metadata, &mut rx)
                        .err_into()
                        .and_then(|req| service.call(req))
                        .and_then(|res| async {
                            let mut builder = capnp::message::TypedBuilder::<
                                quic_metadata_protocol_capnp::connect_response::Owned,
                            >::new_default();

                            let (head, body) = res.into_parts();

                            let root = builder.init_root();
                            let mut m = root.init_metadata((head.headers.len() + 1) as _);

                            let mut entry = m.reborrow().get(0);
                            entry.set_key("HttpStatus".into());
                            entry.set_val(format!("{}", head.status.as_u16()).as_str().into());

                            let mut i = 1;
                            for (k, v) in head.headers.iter() {
                                let mut entry = m.reborrow().get(i);
                                i += 1;
                                entry.set_key(format!("HttpHeader:{}", k.as_str()).as_str().into());
                                entry.set_val(v.to_str()?.into());
                            }

                            Ok((builder, body))
                        })
                        .await;

                tx.write_all(&DATA_SIG).await?;
                tx.write_all(&VERSION).await?;

                match result {
                    Ok((builder, mut body)) => {
                        capnp_futures::serialize::write_message(
                            &mut tx.compat_mut(),
                            builder.into_inner(),
                        )
                        .await?;
                        loop {
                            let chunk =
                                std::future::poll_fn(|cx| Pin::new(&mut body).poll_frame(cx)).await;
                            match chunk {
                                Some(Ok(frame)) => {
                                    if let Ok(data) = frame.into_data() {
                                        tx.write_all(&data).await?;
                                    }
                                }
                                Some(Err(_)) => {
                                    unreachable!();
                                }
                                None => {
                                    break;
                                }
                            }
                        }
                    }
                    Err(e) => {
                        let mut builder = capnp::message::TypedBuilder::<
                            quic_metadata_protocol_capnp::connect_response::Owned,
                        >::new_default();
                        {
                            let mut root = builder.init_root();
                            root.set_error(format!("{e}")[..].into());
                        }
                        capnp_futures::serialize::write_message(
                            &mut tx.compat_mut(),
                            builder.into_inner(),
                        )
                        .await?;
                    }
                }
            }
            quic_metadata_protocol_capnp::ConnectionType::Tcp => {
                unimplemented!("raw tcp is not yet supported");
            }
        }

        Ok(())
    }
}
