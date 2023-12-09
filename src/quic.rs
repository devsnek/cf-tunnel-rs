use crate::{rpc::RpcClient, Error};
use async_compat::CompatExt;
use http_body::Body;
use std::{collections::HashMap, net::SocketAddr, pin::Pin};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use uuid::Uuid;

pub(crate) mod quic_metadata_protocol_capnp {
    #![allow(clippy::all)]
    #![allow(unused)]
    include!(concat!(env!("OUT_DIR"), "/quic_metadata_protocol_capnp.rs"));
}

pub const DATA_SIG: [u8; 6] = [0x0A, 0x36, 0xCD, 0x12, 0xA1, 0x3E];
pub const RPC_SIG: [u8; 6] = [0x52, 0xBB, 0x82, 0x5C, 0xDB, 0x65];
pub const VERSION: [u8; 2] = *b"01";

pub async fn connect_quic(remote: SocketAddr) -> Result<quinn::Connection, Error> {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs()? {
        roots.add(&rustls::Certificate((*cert).to_owned()))?;
    }
    let mut x = std::io::BufReader::new(include_str!("./cf_root.pem").as_bytes());
    for x in rustls_pemfile::read_all(&mut x).flatten() {
        if let rustls_pemfile::Item::X509Certificate(cert) = x {
            roots.add(&rustls::Certificate((*cert).to_owned()))?;
        }
    }

    let client_crypto = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_root_certificates(roots)
        .with_no_client_auth();
    let client_config = quinn::ClientConfig::new(std::sync::Arc::new(client_crypto));
    let mut endpoint = quinn::Endpoint::client("[::]:0".parse().unwrap())?;
    endpoint.set_default_client_config(client_config);

    let conn = endpoint.connect(remote, "quic.cftunnel.com")?.await?;

    Ok(conn)
}

async fn handle_data_stream(
    mut tx: impl AsyncWrite + Send + Sync + Unpin,
    mut rx: impl AsyncRead + Send + Sync + Unpin,
) -> Result<(), Error> {
    let mut v = [0; 2];
    rx.read_exact(&mut v).await?;
    if v != VERSION {
        // AAA
    }

    let r =
        capnp_futures::serialize::read_message(&mut rx.compat_mut(), Default::default()).await?;
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
                    metadata.insert(entry.get_key()?.to_string()?, entry.get_val()?.to_string()?);
                }
            }
            drop(r);
            let method = metadata.get("HttpMethod").unwrap();
            let method = http::method::Method::from_bytes(method.as_bytes())?;

            let mut builder = http::request::Request::builder().uri(uri).method(method);

            for (k, v) in &metadata {
                if k.starts_with("HttpHeader") {
                    let key = k.split(':').nth(1).unwrap();
                    builder = builder.header(key, v);
                }
            }

            let body = if let Some(len) = metadata.get("HttpHeader:Content-Length") {
                let mut body = vec![0; len.parse()?];
                rx.read_exact(&mut body).await?;
                crate::BoxBody::new(http_body_util::Full::new(body.into()))
            } else {
                crate::BoxBody::new(http_body_util::Empty::new())
            };

            let req = builder.body(body)?;

            let mut builder = capnp::message::TypedBuilder::<
                quic_metadata_protocol_capnp::connect_response::Owned,
            >::new_default();

            let res = match crate::handle_http_request(req).await {
                Ok(mut res) => {
                    // res.headers_mut().insert("Date", http::header::HeaderValue::from_str(chrono::Utc::now().to_rfc2822().as_str()).unwrap());
                    // res.headers_mut().insert("Connection", http::header::HeaderValue::from_static("keep-alive"));
                    // res.headers_mut().insert("Keep-Alive", http::header::HeaderValue::from_static("timeout=5"));
                    if let Some(len) = res.body().size_hint().exact() {
                        res.headers_mut().insert(
                            "Content-Length",
                            http::header::HeaderValue::from_str(format!("{len}").as_str()).unwrap(),
                        );
                    } else {
                        panic!()
                    }

                    let root = builder.init_root();
                    let mut m = root.init_metadata((res.headers().len() + 1) as _);
                    let mut entry = m.reborrow().get(0);
                    entry.set_key("HttpStatus".into());
                    entry.set_val(format!("{}", res.status().as_u16()).as_str().into());
                    let mut i = 1;
                    for (k, v) in res.headers() {
                        let mut entry = m.reborrow().get(i);
                        i += 1;
                        entry.set_key(format!("HttpHeader:{}", k.as_str()).as_str().into());
                        entry.set_val(v.to_str()?.into());
                    }
                    Some(res)
                }
                Err(e) => {
                    let mut root = builder.init_root();
                    root.set_error(format!("{e}")[..].into());
                    None
                }
            };

            tx.write_all(&DATA_SIG).await?;
            tx.write_all(&VERSION).await?;
            capnp_futures::serialize::write_message(&mut tx.compat_mut(), builder.into_inner())
                .await?;

            if let Some(mut res) = res {
                loop {
                    let chunk =
                        std::future::poll_fn(|cx| Pin::new(res.body_mut()).poll_frame(cx)).await;
                    match chunk {
                        Some(Ok(frame)) => {
                            if let Ok(chunk) = frame.into_data() {
                                tx.write_all(&chunk[..]).await?;
                            }
                        }
                        Some(Err(_)) => {}
                        None => {
                            break;
                        }
                    }
                }
            }
        }
        quic_metadata_protocol_capnp::ConnectionType::Tcp => {
            unimplemented!("raw tcp is not yet supported");
        }
    }

    Ok(())
}

pub struct Quic {
    conn: quinn::Connection,
    rpc: RpcClient,
}

impl Quic {
    pub async fn connect(remote: SocketAddr) -> Result<Self, Error> {
        let conn = connect_quic(remote).await?;

        let control_stream = conn.open_bi().await.unwrap();

        let rpc = RpcClient::new(control_stream);

        Ok(Self { conn, rpc })
    }

    pub async fn register_connection(
        &self,
        account_tag: &str,
        secret: &[u8],
        id: &Uuid,
        conn_index: u8,
        connector_uuid: &Uuid,
    ) -> Result<(), Error> {
        self.rpc
            .register_connection(
                account_tag.to_owned(),
                secret.to_owned(),
                id.as_bytes().to_vec(),
                conn_index,
                connector_uuid.as_bytes().to_vec(),
            )
            .await?;

        Ok(())
    }

    pub async fn serve(&self) -> Result<(), Error> {
        loop {
            let (tx, mut rx) = self.conn.accept_bi().await?;

            let mut sig = [0; 6];
            rx.read_exact(&mut sig).await?;

            if sig == DATA_SIG {
                tokio::task::spawn(async move {
                    handle_data_stream(tx, rx).await.unwrap();
                });
            } else if sig == RPC_SIG {
                unimplemented!("RPC STREAM!");
            } else {
                continue;
            }
        }

        // Ok(())
    }
}
