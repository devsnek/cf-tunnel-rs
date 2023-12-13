use crate::Error;
use cloudflare::{
    endpoints::{
        account::list_accounts::ListAccounts,
        argo_tunnel::{
            Tunnel,
            {
                create_tunnel::{CreateTunnel, Params as CreateTunnelParams},
                list_tunnels::{ListTunnels, Params as ListTunnelsParams},
            },
        },
    },
    framework::{async_api::Client, auth::Credentials, Environment},
};

pub struct CfApi {
    client: Client,
    account_id: String,
}

impl CfApi {
    pub async fn new(token: String) -> Result<Self, Error> {
        let credentials = Credentials::UserAuthToken { token };
        let client = Client::new(credentials, Default::default(), Environment::Production)?;

        let accounts = client.request(&ListAccounts { params: None }).await?;
        let account_id = accounts.result[0].id.to_owned();

        Ok(Self { client, account_id })
    }

    pub fn account_id(&self) -> &str {
        &self.account_id
    }

    pub async fn list_tunnels(&self, name: Option<String>) -> Result<Vec<Tunnel>, Error> {
        let response = self
            .client
            .request(&ListTunnels {
                account_identifier: &self.account_id,
                params: ListTunnelsParams {
                    name,
                    is_deleted: Some(false),
                    ..Default::default()
                },
            })
            .await?;

        Ok(response.result)
    }

    pub async fn create_tunnel(&self, name: &str, secret: Vec<u8>) -> Result<Tunnel, Error> {
        let response = self
            .client
            .request(&CreateTunnel {
                account_identifier: &self.account_id,
                params: CreateTunnelParams {
                    name,
                    tunnel_secret: &secret,
                    metadata: Default::default(),
                },
            })
            .await?;

        Ok(response.result)
    }
}
