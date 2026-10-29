//! IP address and network parsing for log filters.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use crate::error::{LogsError, Result};

/// Parse a textual IP address; `None` when the text is neither IPv4 nor IPv6.
pub fn parse_ip(text: &str) -> Option<IpAddr> {
    if let Ok(v4) = text.parse::<Ipv4Addr>() {
        return Some(IpAddr::V4(v4));
    }
    // A zone suffix (`fe80::1%eth0`) does not change which network the address is in.
    let bare = text.split_once('%').map_or(
        text,
        |(head, zone)| {
            if zone.is_empty() {
                text
            } else {
                head
            }
        },
    );
    bare.parse::<Ipv6Addr>().ok().map(IpAddr::V6)
}

/// A CIDR network with its host bits cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Network {
    base: u128,
    prefix: u8,
    v6: bool,
}

fn bits(address: IpAddr) -> (u128, bool) {
    match address {
        IpAddr::V4(v4) => (u128::from(u32::from(v4)), false),
        IpAddr::V6(v6) => (u128::from(v6), true),
    }
}

impl Network {
    /// Parse `address`, `address/prefix`, `address/netmask` or `address/hostmask`;
    /// host bits are cleared rather than rejected.
    pub fn parse(text: &str) -> Result<Self> {
        let invalid = || {
            LogsError::value(format!(
                "'{text}' does not appear to be an IPv4 or IPv6 network"
            ))
        };
        let (address, mask) = match text.split_once('/') {
            Some((a, m)) => (a, Some(m)),
            None => (text, None),
        };
        let address = parse_ip(address).ok_or_else(invalid)?;
        let (value, v6) = bits(address);
        let width: u32 = if v6 { 128 } else { 32 };
        let prefix = match mask {
            None => width,
            Some(m) if !m.is_empty() && m.bytes().all(|b| b.is_ascii_digit()) => {
                let n: u32 = m.parse().map_err(|_| invalid())?;
                if n > width {
                    return Err(invalid());
                }
                n
            }
            Some(m) => {
                let mask_address = parse_ip(m).ok_or_else(invalid)?;
                let (mask_bits, mask_v6) = bits(mask_address);
                if mask_v6 != v6 {
                    return Err(invalid());
                }
                let full = if v6 { u128::MAX } else { u128::from(u32::MAX) };
                let ones = mask_bits.count_ones();
                if ones == 0 {
                    0
                } else if mask_bits == (full << (width - ones)) & full {
                    ones
                } else if mask_bits == (full >> (width - ones)) & full {
                    width - ones
                } else {
                    return Err(invalid());
                }
            }
        };
        let full = if v6 { u128::MAX } else { u128::from(u32::MAX) };
        let keep = if prefix == 0 {
            0
        } else {
            (full << (width - prefix)) & full
        };
        Ok(Self {
            base: value & keep,
            prefix: prefix as u8,
            v6,
        })
    }

    /// Whether `address` belongs to the network; addresses of the other family never do.
    pub fn contains(&self, address: IpAddr) -> bool {
        let (value, v6) = bits(address);
        if v6 != self.v6 {
            return false;
        }
        let width: u32 = if v6 { 128 } else { 32 };
        let full = if v6 { u128::MAX } else { u128::from(u32::MAX) };
        let prefix = u32::from(self.prefix);
        let keep = if prefix == 0 {
            0
        } else {
            (full << (width - prefix)) & full
        };
        value & keep == self.base
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_bits_are_cleared_and_families_never_mix() {
        let net = Network::parse("192.0.2.77/24").unwrap();
        assert!(net.contains(parse_ip("192.0.2.1").unwrap()));
        assert!(!net.contains(parse_ip("192.0.3.1").unwrap()));
        assert!(!net.contains(parse_ip("2001:db8::1").unwrap()));
        let v6 = Network::parse("2001:db8::/32").unwrap();
        assert!(v6.contains(parse_ip("2001:db8:1::1").unwrap()));
    }

    #[test]
    fn masks_in_netmask_form_are_accepted() {
        let net = Network::parse("10.1.0.0/255.255.0.0").unwrap();
        assert!(net.contains(parse_ip("10.1.200.3").unwrap()));
        assert!(Network::parse("garbage").is_err());
        assert!(Network::parse("10.0.0.0/33").is_err());
        assert!(Network::parse("10.0.0.0/255.0.255.0").is_err());
    }
}
