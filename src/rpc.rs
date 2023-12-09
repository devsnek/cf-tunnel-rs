use crate::Error;
use async_compat::CompatExt;
use capnp_rpc::{rpc_twoparty_capnp::Side, twoparty::VatNetwork, RpcSystem};
use std::marker::Unpin;
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) mod tunnelrpc_capnp {
    #![allow(unused)]
    #![allow(clippy::all)]
    include!(concat!(env!("OUT_DIR"), "/tunnelrpc_capnp.rs"));
}

use tunnelrpc_capnp::registration_server::Client as RegistrationClient;

enum RpcTransfer {
    RegisterConnection {
        args: RegisterConnectionRequest,
        tx: tokio::sync::oneshot::Sender<Result<RegisterConnectionResponse, Error>>,
    },
    UnregisterConnection {
        tx: tokio::sync::oneshot::Sender<Result<(), Error>>,
    },
    UpdateLocalConfiguration {
        config: Vec<u8>,
        tx: tokio::sync::oneshot::Sender<Result<(), Error>>,
    },
}

pub struct RpcClient {
    sender: tokio::sync::mpsc::Sender<RpcTransfer>,
}

async fn register_connection(
    rpc: &RegistrationClient,
    args: RegisterConnectionRequest,
) -> Result<RegisterConnectionResponse, Error> {
    use tunnelrpc_capnp::connection_response::result::Which;

    let mut request = rpc.register_connection_request();

    request
        .get()
        .get_auth()?
        .set_account_tag(args.account_tag[..].into());
    request
        .get()
        .get_auth()?
        .set_tunnel_secret(&args.tunnel_secret);

    request.get().set_tunnel_id(&args.tunnel_id);
    request.get().set_conn_index(args.conn_index);

    request
        .get()
        .get_options()?
        .get_client()?
        .set_client_id(&args.client_id);

    let result = request.send().promise.await?;

    let result = result.get()?.get_result()?.get_result().which().unwrap();

    match result {
        Which::ConnectionDetails(r) => {
            let r = r?;
            let uuid = r.get_uuid()?.to_owned();
            let location = r.get_location_name()?.to_string()?;
            Ok(RegisterConnectionResponse { uuid, location })
        }
        Which::Error(e) => {
            let e = e?;
            Err(Error::RpcError(e.get_cause()?.to_string()?))
        }
    }
}

async fn unregister_connection(rpc: &RegistrationClient) -> Result<(), Error> {
    let request = rpc.unregister_connection_request();
    let result = request.send().promise.await?;
    result.get()?;
    Ok(())
}

async fn update_local_connection(rpc: &RegistrationClient, config: Vec<u8>) -> Result<(), Error> {
    let mut request = rpc.update_local_configuration_request();
    request.get().set_config(&config);
    let result = request.send().promise.await?;
    result.get()?;
    Ok(())
}

impl RpcClient {
    pub fn new(
        (tx, rx): (
            impl AsyncWrite + Unpin + Send + Sync + 'static,
            impl AsyncRead + Unpin + Send + Sync + 'static,
        ),
    ) -> Self {
        let (sender, mut receiver) = tokio::sync::mpsc::channel(8);

        let rt = tokio::runtime::Handle::current();
        std::thread::spawn(move || {
            let _guard = rt.enter();
            let local = tokio::task::LocalSet::new();
            let fut = local.run_until(async {
                let network = Box::new(VatNetwork::new(
                    rx.compat(),
                    tx.compat(),
                    Side::Client,
                    Default::default(),
                ));

                let mut rpc_system = RpcSystem::new(network, None);
                let disconnector = rpc_system.get_disconnector();
                let reg_rpc: RegistrationClient = rpc_system.bootstrap(Side::Server);

                tokio::task::spawn_local(async {
                    rpc_system.await.unwrap();
                });

                while let Some(message) = receiver.recv().await {
                    match message {
                        RpcTransfer::RegisterConnection { args, tx } => {
                            let result = register_connection(&reg_rpc, args).await;
                            let _ = tx.send(result);
                        }
                        RpcTransfer::UnregisterConnection { tx } => {
                            let result = unregister_connection(&reg_rpc).await;
                            let _ = tx.send(result);
                        }
                        RpcTransfer::UpdateLocalConfiguration { config, tx } => {
                            let result = update_local_connection(&reg_rpc, config).await;
                            let _ = tx.send(result);
                        }
                    }
                }

                disconnector.await.unwrap();
            });
            rt.block_on(fut);
        });

        Self { sender }
    }

    // registerConnection @0 (auth :TunnelAuth, tunnelId :Data, connIndex :UInt8, options :ConnectionOptions) -> (result :ConnectionResponse);
    pub async fn register_connection(
        &self,
        account_tag: String,
        tunnel_secret: Vec<u8>,
        tunnel_id: Vec<u8>,
        conn_index: u8,
        client_id: Vec<u8>,
    ) -> Result<RegisterConnectionResponse, Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(RpcTransfer::RegisterConnection {
                args: RegisterConnectionRequest {
                    account_tag,
                    tunnel_secret,
                    tunnel_id,
                    conn_index,
                    client_id,
                },
                tx,
            })
            .await
            .unwrap();

        rx.await?
    }

    // unregisterConnection @1 () -> ();
    pub async fn unregister_connection(&self) -> Result<(), Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(RpcTransfer::UnregisterConnection { tx })
            .await
            .unwrap();

        rx.await?
    }

    // updateLocalConfiguration @2 (config :Data) -> ();
    pub async fn update_local_configuration(&self, config: Vec<u8>) -> Result<(), Error> {
        let (tx, rx) = tokio::sync::oneshot::channel();

        self.sender
            .send(RpcTransfer::UpdateLocalConfiguration { config, tx })
            .await
            .unwrap();

        rx.await?
    }
}

#[derive(Debug)]
pub struct RegisterConnectionRequest {
    account_tag: String,
    tunnel_secret: Vec<u8>,
    tunnel_id: Vec<u8>,
    conn_index: u8,
    client_id: Vec<u8>,
}

#[derive(Debug)]
pub struct RegisterConnectionResponse {
    uuid: Vec<u8>,
    location: String,
}
