use std::net::{Ipv4Addr, SocketAddrV4, TcpListener, TcpStream};
use std::time::Duration;

use wt_core::{CoreError, ExitClass};

use crate::Result;

/// Probes one IPv4 loopback port by both binding and attempting a bounded connect.
pub fn squat_probe(port: u16, connect_timeout: Duration) -> Result<bool> {
    let address = SocketAddrV4::new(Ipv4Addr::LOCALHOST, port);
    if let Ok(stream) = TcpStream::connect_timeout(&address.into(), connect_timeout) {
        drop(stream);
        return Ok(true);
    }
    match bind_probe(address, port)? {
        // A port that refuses a connection but will not accept a bind is as
        // likely to be another allocator's probe holding it for its own
        // microsecond as it is a squatter, and two `wt new` runs in parallel
        // must not each read the other as one. A squatter is still there on
        // the second look; a probe is long gone.
        true => {
            std::thread::sleep(Duration::from_millis(20));
            bind_probe(address, port)
        }
        false => Ok(false),
    }
}

fn bind_probe(address: SocketAddrV4, port: u16) -> Result<bool> {
    match TcpListener::bind(address) {
        Ok(listener) => {
            drop(listener);
            Ok(false)
        }
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            Ok(true)
        }
        Err(error) => Err(CoreError::new(
            ExitClass::Internal,
            "PORT_PROBE_FAILED",
            format!("could not probe IPv4 port {port}: {error}"),
            "retry allocation or inspect local network permissions",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_a_bound_v4_port_and_a_free_one() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        assert!(squat_probe(port, Duration::from_millis(50)).unwrap());
        drop(listener);
        assert!((40_000..40_100)
            .any(|candidate| { !squat_probe(candidate, Duration::from_millis(50)).unwrap() }));
    }
}
