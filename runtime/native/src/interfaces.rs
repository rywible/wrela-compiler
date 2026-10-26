//! This machine's network addresses, which a server on the LAN answers for
//! (`loopback::listen`). The standard library has no list of them, so `getifaddrs` gives it: the
//! one `unsafe` here walks the list that call returns, and frees it.
#![allow(unsafe_code)]

/// This machine's private IPv4 addresses ([`private`]), its loopback's aside: what a device on
/// the LAN names it by. A public address isn't one: a server on the LAN isn't on the internet.
pub fn lan_addresses() -> Vec<std::net::Ipv4Addr> {
    let mut out = Vec::new();
    #[cfg(unix)]
    {
        let mut ifs: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: `getifaddrs` gives a list that lives until `freeifaddrs`; each entry, and the
        // address it points to, is read only while it does.
        unsafe {
            if libc::getifaddrs(&mut ifs) == 0 {
                let mut p = ifs;
                while !p.is_null() {
                    let a = (*p).ifa_addr;
                    if !a.is_null() && i32::from((*a).sa_family) == libc::AF_INET {
                        let sin = &*a.cast::<libc::sockaddr_in>();
                        let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                        if private(ip) && !out.contains(&ip) {
                            out.push(ip);
                        }
                    }
                    p = (*p).ifa_next;
                }
                libc::freeifaddrs(ifs);
            }
        }
    }
    out
}

/// Whether `ip` is on a private network: RFC 1918's (10/8, 172.16/12, 192.168/16), a carrier's
/// or an overlay's shared space (100.64/10, RFC 6598: Tailscale's, say) or link-local
/// (169.254/16). The browser's test mode takes the same (runtime/browser `mayTest`).
pub fn private(ip: std::net::Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b))
}

/// Whether a connection from `peer` may be answered by a server on the LAN: one from this
/// machine, or from a private network. Listening on every interface, a server refuses the rest.
pub fn local_peer(peer: std::net::SocketAddr) -> bool {
    match peer.ip() {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || private(ip),
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback()
                || ip.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback() || private(v4))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_lan_server_answers_this_machine_and_private_networks_alone() {
        let v4 = |s: &str| s.parse::<std::net::Ipv4Addr>().expect("ip");
        for ip in ["10.0.0.109", "172.16.4.1", "192.168.1.5", "100.74.48.38", "169.254.1.1"] {
            assert!(private(v4(ip)), "{ip}");
        }
        for ip in ["8.8.8.8", "172.32.0.1", "100.128.0.1", "127.0.0.1"] {
            assert!(!private(v4(ip)), "{ip}");
        }
        let peer = |s: &str| s.parse::<std::net::SocketAddr>().expect("peer");
        assert!(local_peer(peer("127.0.0.1:5000")) && local_peer(peer("[::1]:5000")));
        assert!(local_peer(peer("192.168.1.5:5000")) && !local_peer(peer("8.8.8.8:5000")));
        assert!(!local_peer(peer("[2001:db8::1]:5000")));
    }
}
