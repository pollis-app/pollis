//! The local SOCKS5 shim: a generic anonymized-stream entry point on loopback.
//!
//! This is the §14.0 "generic CONNECT primitive". reqwest consumes it via
//! `socks5h://` (proxy-side DNS); a future in-app webview / VPN points at the
//! same port. On each SOCKS5 CONNECT the shim consults the [`RoutingPolicy`],
//! then either builds an overlay circuit, dials the target directly, or — in
//! Strict mode with no circuit — fails the SOCKS request cleanly (never hangs,
//! never silently succeeds to nowhere).
//!
//! Only SOCKS5 CONNECT with no authentication is supported (loopback side only).

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::circuit::CircuitFactory;
use crate::policy::{FinalAction, PlannedRoute, RoutingPolicy};
use crate::stream::BoxedStream;

const SOCKS5_VERSION: u8 = 0x05;
const SOCKS5_NO_AUTH: u8 = 0x00;
const SOCKS5_CMD_CONNECT: u8 = 0x01;
const ATYP_IPV4: u8 = 0x01;
const ATYP_DOMAIN: u8 = 0x03;
const ATYP_IPV6: u8 = 0x04;

// SOCKS5 reply codes.
const REP_SUCCESS: u8 = 0x00;
const REP_GENERAL_FAILURE: u8 = 0x01;
const REP_HOST_UNREACHABLE: u8 = 0x04;
const REP_CMD_NOT_SUPPORTED: u8 = 0x07;

/// Handle to a running shim. Dropping it aborts the accept loop.
pub struct OverlayHandle {
    socks_addr: SocketAddr,
    task: JoinHandle<()>,
}

impl OverlayHandle {
    /// The loopback `127.0.0.1:<port>` address callers point their SOCKS5 client
    /// (`socks5h://…`) at.
    pub fn socks_addr(&self) -> SocketAddr {
        self.socks_addr
    }
}

impl Drop for OverlayHandle {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// The shim, owning the policy and the circuit factory.
pub struct OverlayShim;

impl OverlayShim {
    /// Bind a SOCKS5 server on an ephemeral loopback port and start serving.
    pub async fn start(
        policy: RoutingPolicy,
        circuit_factory: Arc<dyn CircuitFactory>,
    ) -> anyhow::Result<OverlayHandle> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let socks_addr = listener.local_addr()?;

        let policy = Arc::new(policy);
        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((client, _peer)) => {
                        let policy = policy.clone();
                        let factory = circuit_factory.clone();
                        tokio::spawn(async move {
                            if let Err(e) = serve_conn(client, policy, factory).await {
                                tracing::debug!("shim: connection ended: {e}");
                            }
                        });
                    }
                    Err(e) => {
                        tracing::debug!("shim: accept failed: {e}");
                        break;
                    }
                }
            }
        });

        Ok(OverlayHandle { socks_addr, task })
    }
}

async fn serve_conn(
    mut client: TcpStream,
    policy: Arc<RoutingPolicy>,
    factory: Arc<dyn CircuitFactory>,
) -> anyhow::Result<()> {
    // --- greeting: version + method negotiation (we only offer no-auth) ---
    let ver = client.read_u8().await?;
    if ver != SOCKS5_VERSION {
        anyhow::bail!("unsupported SOCKS version {ver}");
    }
    let nmethods = client.read_u8().await? as usize;
    let mut methods = vec![0u8; nmethods];
    client.read_exact(&mut methods).await?;
    if !methods.contains(&SOCKS5_NO_AUTH) {
        // No acceptable methods.
        client.write_all(&[SOCKS5_VERSION, 0xFF]).await?;
        return Ok(());
    }
    client.write_all(&[SOCKS5_VERSION, SOCKS5_NO_AUTH]).await?;

    // --- request: VER CMD RSV ATYP DST.ADDR DST.PORT ---
    let ver = client.read_u8().await?;
    let cmd = client.read_u8().await?;
    let _rsv = client.read_u8().await?;
    let atyp = client.read_u8().await?;
    if ver != SOCKS5_VERSION {
        anyhow::bail!("unsupported SOCKS version {ver} in request");
    }

    let host = match atyp {
        ATYP_IPV4 => {
            let mut addr = [0u8; 4];
            client.read_exact(&mut addr).await?;
            std::net::Ipv4Addr::from(addr).to_string()
        }
        ATYP_IPV6 => {
            let mut addr = [0u8; 16];
            client.read_exact(&mut addr).await?;
            std::net::Ipv6Addr::from(addr).to_string()
        }
        ATYP_DOMAIN => {
            let len = client.read_u8().await? as usize;
            let mut name = vec![0u8; len];
            client.read_exact(&mut name).await?;
            String::from_utf8(name).map_err(|_| anyhow::anyhow!("invalid domain in SOCKS request"))?
        }
        other => {
            reply(&mut client, REP_CMD_NOT_SUPPORTED).await?;
            anyhow::bail!("unsupported ATYP {other}");
        }
    };
    let port = client.read_u16().await?;

    if cmd != SOCKS5_CMD_CONNECT {
        reply(&mut client, REP_CMD_NOT_SUPPORTED).await?;
        return Ok(());
    }

    // --- routing decision ---
    let plan = policy.plan(&host);
    let upstream = establish_upstream(plan, &factory, &host, port).await;

    match upstream {
        Ok(mut up) => {
            reply(&mut client, REP_SUCCESS).await?;
            // Pipe until either side closes.
            let _ = tokio::io::copy_bidirectional(&mut client, &mut up).await;
            Ok(())
        }
        Err(action) => {
            // Degraded / unreachable: surface a clean SOCKS failure. Never hang,
            // never a silent success (messages-must-work, design §7/§10.1).
            let code = match action {
                FinalAction::Degraded => REP_GENERAL_FAILURE,
                _ => REP_HOST_UNREACHABLE,
            };
            reply(&mut client, code).await?;
            Ok(())
        }
    }
}

/// Execute a plan into a live upstream byte pipe, applying Prefer/Strict
/// fallback semantics on overlay failure. On failure returns the [`FinalAction`]
/// so the caller can pick the SOCKS reply code.
async fn establish_upstream(
    plan: PlannedRoute,
    factory: &Arc<dyn CircuitFactory>,
    host: &str,
    port: u16,
) -> Result<BoxedStream, FinalAction> {
    match plan {
        PlannedRoute::Direct => direct_connect(host, port).await.map_err(|_| FinalAction::Direct),
        PlannedRoute::Overlay { fallback_to_direct } => match factory.connect(host, port).await {
            Ok(stream) => Ok(stream),
            Err(e) => {
                tracing::debug!("shim: overlay circuit to {host}:{port} failed: {e}");
                if fallback_to_direct {
                    direct_connect(host, port).await.map_err(|_| FinalAction::Direct)
                } else {
                    Err(FinalAction::Degraded)
                }
            }
        },
    }
}

async fn direct_connect(host: &str, port: u16) -> anyhow::Result<BoxedStream> {
    let tcp = TcpStream::connect((host, port)).await?;
    Ok(BoxedStream::new(tcp))
}

/// Write a SOCKS5 reply with a `0.0.0.0:0` bound address (clients ignore it for
/// CONNECT).
async fn reply(client: &mut TcpStream, code: u8) -> std::io::Result<()> {
    client
        .write_all(&[SOCKS5_VERSION, code, 0x00, ATYP_IPV4, 0, 0, 0, 0, 0, 0])
        .await
}
