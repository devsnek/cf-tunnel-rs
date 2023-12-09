struct CustomDuration(std::time::Duration);

impl serde::Serialize for CustomDuration {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_f64(self.0.as_secs_f64())
    }
}

#[derive(serde::Serialize)]
struct WarpRoutingConfig {
    #[serde(rename = "connectTimeout", skip_serializing_if = "Option::is_none")]
    connect_timeout: Option<CustomDuration>,
    #[serde(rename = "tcpKeepAlive", skip_serializing_if = "Option::is_none")]
    tcp_keep_alive: Option<CustomDuration>,
}

#[derive(serde::Serialize)]
struct OriginRequestConfig {}

#[derive(serde::Serialize)]
struct UnvalidatedIngressRule {
    #[serde(skip_serializing_if = "Option::is_none")]
    hostname: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    service: Option<String>,
    #[serde(rename = "originRequest")]
    origin_request: OriginRequestConfig,
}

#[derive(serde::Serialize)]
struct LocalConfig {
    #[serde(rename = "__configuration_flags")]
    configuration_flags: std::collections::HashMap<String, String>,
    #[serde(rename = "warp-routing")]
    warp_routing: WarpRoutingConfig,
    ingress: Vec<UnvalidatedIngressRule>,
}
