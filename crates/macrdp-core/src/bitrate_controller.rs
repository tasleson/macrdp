use std::net::IpAddr;

/// Determine if an IP address belongs to a private/local network.
pub fn is_private_ip(addr: IpAddr) -> bool {
    match addr {
        IpAddr::V4(ip) => ip.is_private() || ip.is_loopback() || ip.is_link_local(),
        IpAddr::V6(ip) => ip.is_loopback() || (ip.segments()[0] & 0xffc0) == 0xfe80,
    }
}

/// Two-phase LAN detection result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetworkType {
    Lan,
    Wan,
}

/// Phase 1: IP-based initial detection.
pub fn detect_network_phase1(peer_addr: IpAddr) -> NetworkType {
    if is_private_ip(peer_addr) {
        NetworkType::Lan
    } else {
        NetworkType::Wan
    }
}

/// Phase 2: RTT-based correction. Call after GFX channel has ~5 RTT samples.
pub fn detect_network_phase2(phase1: NetworkType, rtt_ewma_ms: f64) -> NetworkType {
    const LAN_RTT_THRESHOLD_MS: f64 = 10.0;
    match phase1 {
        NetworkType::Lan if rtt_ewma_ms > LAN_RTT_THRESHOLD_MS => NetworkType::Wan,
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn private_ipv4_ranges() {
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(10, 255, 255, 255))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 16, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(172, 31, 255, 255))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1))));
        assert!(is_private_ip(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 1))));
    }

    #[test]
    fn public_ipv4() {
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(1, 1, 1, 1))));
        assert!(!is_private_ip(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 1))));
    }

    #[test]
    fn ipv6() {
        assert!(is_private_ip(IpAddr::V6(Ipv6Addr::LOCALHOST)));
        assert!(is_private_ip(IpAddr::V6(Ipv6Addr::new(
            0xfe80, 0, 0, 0, 0, 0, 0, 1
        ))));
        assert!(!is_private_ip(IpAddr::V6(Ipv6Addr::new(
            0x2001, 0xdb8, 0, 0, 0, 0, 0, 1
        ))));
    }

    #[test]
    fn phase1_detection() {
        assert_eq!(
            detect_network_phase1(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1))),
            NetworkType::Lan
        );
        assert_eq!(
            detect_network_phase1(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8))),
            NetworkType::Wan
        );
    }

    #[test]
    fn phase2_rtt_correction() {
        assert_eq!(
            detect_network_phase2(NetworkType::Lan, 50.0),
            NetworkType::Wan
        );
        assert_eq!(
            detect_network_phase2(NetworkType::Lan, 3.0),
            NetworkType::Lan
        );
        assert_eq!(
            detect_network_phase2(NetworkType::Wan, 3.0),
            NetworkType::Wan
        );
    }
}
