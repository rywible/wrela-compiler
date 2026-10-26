//! This machine's network addresses, which a server on the LAN answers for
//! (`loopback::listen`). The standard library has no list of them, so `getifaddrs` gives it: the
//! one `unsafe` here walks the list that call returns, and frees it.
#![allow(unsafe_code)]

/// This machine's IPv4 addresses on its networks, its loopback's aside: what a device on the
/// LAN names it by.
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
                        if !ip.is_loopback() && !out.contains(&ip) {
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
