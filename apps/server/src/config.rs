use color_eyre::eyre::{Result, eyre};
use std::{env, path::PathBuf};
use url::Url;
use uuid::Uuid;

pub struct Config {
    pub backend_origin: String,
    pub auth_origin: String,
    pub rp_id: String,
    pub cookie_domain: String,
    pub allowed_origins: Vec<String>,
    pub csrf_secret: String,
    pub allow_http: bool,
    pub chatgpt_connection_dir: Option<PathBuf>,
    pub web_dir: PathBuf,
    pub local_mcp_user_id: Option<Uuid>,
}

impl Config {
    /// Resolve the most specific configured cookie domain for this request host.
    ///
    /// # Errors
    /// Returns an error when the request host is absent or outside the configured candidates.
    pub fn cookie_domain_for_host(&self, request_host: Option<&str>) -> Result<String> {
        let host = request_host
            .and_then(normalize_host)
            .ok_or_else(|| eyre!("Request carries no usable Host header for COOKIE_DOMAIN"))?;
        self.cookie_domain
            .split(',')
            .filter_map(normalize_host)
            .filter(|candidate| {
                host == *candidate
                    || host
                        .strip_suffix(candidate)
                        .is_some_and(|prefix| prefix.ends_with('.'))
            })
            .max_by_key(String::len)
            .ok_or_else(|| eyre!("Request host is outside the configured COOKIE_DOMAIN candidates"))
    }

    /// Read and validate a backend or combined service configuration.
    ///
    /// # Errors
    /// Returns an error for absent secrets or incompatible origins.
    pub fn from_env(web_dir: PathBuf) -> Result<Self> {
        Self::load(web_dir, true)
    }

    /// Read auth-service or owner-CLI settings without a backend CSRF secret.
    ///
    /// # Errors
    /// Returns an error for absent or invalid relying-party settings.
    pub fn for_auth(web_dir: PathBuf) -> Result<Self> {
        Self::load(web_dir, false)
    }

    fn load(web_dir: PathBuf, require_csrf: bool) -> Result<Self> {
        let allow_http = env::var("NODE_ENV").is_ok_and(|value| value == "development")
            && env::var("LOCAL_AUTH_ALLOW_HTTP").is_ok_and(|value| value == "true");
        let auth_origin = origin(&env::var("PUBLIC_AUTH_BASE_URL")?, allow_http)?;
        let parsed_auth = Url::parse(&auth_origin)?;
        let rp_id = env::var("WEBAUTHN_RP_ID")
            .unwrap_or_else(|_| parsed_auth.host_str().unwrap_or_default().to_owned());
        if parsed_auth.host_str() != Some(rp_id.as_str())
            || (rp_id != "localhost"
                && (!rp_id.contains('.') || rp_id.parse::<std::net::IpAddr>().is_ok()))
        {
            return Err(eyre!(
                "WEBAUTHN_RP_ID must equal the stable auth DNS hostname"
            ));
        }
        let allowed_origins = env::var("ALLOWED_REDIRECT_URIS")
            .or_else(|_| env::var("BACKEND_ALLOWED_ORIGINS"))?
            .split(',')
            .filter(|value| !value.trim().is_empty())
            .map(|value| origin(value.trim(), allow_http))
            .collect::<Result<Vec<_>>>()?;
        if allowed_origins.is_empty() {
            return Err(eyre!("ALLOWED_REDIRECT_URIS must contain an app origin"));
        }
        let backend_origin = origin(
            &env::var("PUBLIC_APP_BASE_URL")
                .unwrap_or_else(|_| allowed_origins.first().cloned().unwrap_or_default()),
            allow_http,
        )?;
        let csrf_secret = env::var("BACKEND_CSRF_SECRET").unwrap_or_default();
        if require_csrf && csrf_secret.len() < 32 {
            return Err(eyre!("BACKEND_CSRF_SECRET must contain at least 32 bytes"));
        }
        let cookie_domain = env::var("COOKIE_DOMAIN")
            .unwrap_or_else(|_| parsed_auth.host_str().unwrap_or_default().to_owned());
        let cookie_candidates = cookie_domain
            .split(',')
            .filter_map(normalize_host)
            .collect::<Vec<_>>();
        if cookie_candidates.is_empty()
            || cookie_candidates
                .iter()
                .any(|candidate| candidate.contains(['/', ':', ' ', ';']))
        {
            return Err(eyre!(
                "COOKIE_DOMAIN must contain comma-separated hostnames"
            ));
        }
        let local_mcp_user_id = match env::var("LOCAL_MCP_ENABLED").ok().as_deref() {
            None | Some("false") => None,
            Some("true") => {
                if !env::var("AUTH_MODE").is_ok_and(|value| value == "local") {
                    return Err(eyre!("Local MCP requires AUTH_MODE=local"));
                }
                env::var("PUBLIC_APP_BASE_URL")
                    .map_err(|_| eyre!("Local MCP requires PUBLIC_APP_BASE_URL"))?;
                let value = env::var("LOCAL_MCP_USER_ID")?;
                let id = value.parse::<Uuid>()?;
                if !id.hyphenated().to_string().eq_ignore_ascii_case(&value) {
                    return Err(eyre!("LOCAL_MCP_USER_ID must be a canonical UUID"));
                }
                Some(id)
            }
            Some(_) => return Err(eyre!("LOCAL_MCP_ENABLED must be true or false")),
        };
        Ok(Self {
            backend_origin,
            auth_origin,
            rp_id,
            cookie_domain,
            allowed_origins,
            csrf_secret,
            allow_http,
            chatgpt_connection_dir: env::var_os("CHATGPT_CONNECTION_DIR").map(PathBuf::from),
            web_dir,
            local_mcp_user_id,
        })
    }
}

fn normalize_host(value: &str) -> Option<String> {
    let value = value.trim().to_lowercase();
    let without_port = value
        .rsplit_once(':')
        .map_or(value.as_str(), |(host, port)| {
            if !port.is_empty()
                && port.bytes().all(|byte| byte.is_ascii_digit())
                && (!value.starts_with('[') || host.ends_with(']'))
            {
                host
            } else {
                &value
            }
        });
    let host = without_port.strip_suffix('.').unwrap_or(without_port);
    (!host.is_empty()).then(|| host.to_owned())
}

fn origin(value: &str, allow_http: bool) -> Result<String> {
    let url = Url::parse(value)?;
    let loopback = matches!(url.host_str(), Some("localhost" | "127.0.0.1"));
    if !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
        || !(url.scheme() == "https" || allow_http && loopback && url.scheme() == "http")
    {
        return Err(eyre!(
            "Origins require HTTPS and no path, except explicit development loopback HTTP"
        ));
    }
    Ok(url.origin().ascii_serialization())
}
