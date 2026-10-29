//! Operating-system adapters: the real resolver and free-disk probe.
//!
//! This is the only module that touches libc; everything above it takes the behaviour as
//! a trait or a closure so it can be exercised offline.

#![allow(unsafe_code)]

#[cfg(unix)]
use std::ffi::CStr;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::Path;

use crate::dns::{DnsFailure, Resolver};

/// Resolver backed by the system's `getnameinfo` and `getaddrinfo`.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemResolver;

#[cfg(unix)]
fn reverse_failure(code: libc::c_int) -> DnsFailure {
    // SAFETY: gai_strerror returns a pointer to a static NUL-terminated string.
    let text = || {
        unsafe { CStr::from_ptr(libc::gai_strerror(code)) }
            .to_string_lossy()
            .into_owned()
    };
    match code {
        libc::EAI_NONAME => DnsFailure::NotFound,
        libc::EAI_AGAIN => DnsFailure::TryAgain,
        _ => DnsFailure::Other(text()),
    }
}

/// Winsock reports `EAI_*` failures as `WSA*` codes; the message text comes
/// from the OS error table, the same place `gai_strerror` reads on Unix.
#[cfg(windows)]
fn reverse_failure(code: i32) -> DnsFailure {
    use windows_sys::Win32::Networking::WinSock::{WSAHOST_NOT_FOUND, WSATRY_AGAIN};
    match code {
        WSAHOST_NOT_FOUND => DnsFailure::NotFound,
        WSATRY_AGAIN => DnsFailure::TryAgain,
        _ => DnsFailure::Other(std::io::Error::from_raw_os_error(code).to_string()),
    }
}

#[cfg(windows)]
fn winsock() {
    use std::sync::Once;
    use windows_sys::Win32::Networking::WinSock::{WSAStartup, WSADATA};
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SAFETY: `data` is a writable WSADATA out-parameter; the version pair
        // 2.2 is the Winsock level every supported Windows ships.
        let mut data: WSADATA = unsafe { std::mem::zeroed() };
        unsafe {
            WSAStartup(0x0202, &mut data);
        }
    });
}

/// The socket address `getnameinfo` resolves, kept alive for the call.
#[cfg(windows)]
enum SockAddr {
    V4(windows_sys::Win32::Networking::WinSock::SOCKADDR_IN),
    V6(windows_sys::Win32::Networking::WinSock::SOCKADDR_IN6),
}

#[cfg(windows)]
fn sockaddr_of(ip: &IpAddr) -> (SockAddr, i32) {
    use windows_sys::Win32::Networking::WinSock::{
        ADDRESS_FAMILY, AF_INET, AF_INET6, IN6_ADDR, IN6_ADDR_0, IN_ADDR, IN_ADDR_0, SOCKADDR_IN,
        SOCKADDR_IN6, SOCKADDR_IN6_0,
    };
    match ip {
        IpAddr::V4(v4) => (
            SockAddr::V4(SOCKADDR_IN {
                sin_family: AF_INET as ADDRESS_FAMILY,
                sin_port: 0,
                sin_addr: IN_ADDR {
                    S_un: IN_ADDR_0 {
                        S_addr: u32::from_ne_bytes(v4.octets()),
                    },
                },
                sin_zero: [0; 8],
            }),
            std::mem::size_of::<SOCKADDR_IN>() as i32,
        ),
        IpAddr::V6(v6) => (
            SockAddr::V6(SOCKADDR_IN6 {
                sin6_family: AF_INET6 as ADDRESS_FAMILY,
                sin6_port: 0,
                sin6_flowinfo: 0,
                sin6_addr: IN6_ADDR {
                    u: IN6_ADDR_0 { Byte: v6.octets() },
                },
                Anonymous: SOCKADDR_IN6_0 { sin6_scope_id: 0 },
            }),
            std::mem::size_of::<SOCKADDR_IN6>() as i32,
        ),
    }
}

impl Resolver for SystemResolver {
    #[cfg(unix)]
    fn reverse(&self, ip: IpAddr) -> Result<String, DnsFailure> {
        let mut host = [0 as libc::c_char; 1025];
        let code = match ip {
            IpAddr::V4(v4) => {
                let addr = libc::sockaddr_in {
                    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
                    sin_len: std::mem::size_of::<libc::sockaddr_in>() as u8,
                    sin_family: libc::AF_INET as libc::sa_family_t,
                    sin_port: 0,
                    sin_addr: libc::in_addr {
                        s_addr: u32::from_ne_bytes(v4.octets()),
                    },
                    sin_zero: [0; 8],
                };
                // SAFETY: `addr` is a fully initialised sockaddr_in and `host` is a
                // writable buffer whose length is passed alongside it.
                unsafe {
                    libc::getnameinfo(
                        std::ptr::from_ref(&addr).cast(),
                        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                        host.as_mut_ptr(),
                        host.len() as libc::socklen_t,
                        std::ptr::null_mut(),
                        0,
                        libc::NI_NAMEREQD,
                    )
                }
            }
            IpAddr::V6(v6) => {
                let addr = libc::sockaddr_in6 {
                    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
                    sin6_len: std::mem::size_of::<libc::sockaddr_in6>() as u8,
                    sin6_family: libc::AF_INET6 as libc::sa_family_t,
                    sin6_port: 0,
                    sin6_flowinfo: 0,
                    sin6_addr: libc::in6_addr {
                        s6_addr: v6.octets(),
                    },
                    sin6_scope_id: 0,
                };
                // SAFETY: as above, for a sockaddr_in6.
                unsafe {
                    libc::getnameinfo(
                        std::ptr::from_ref(&addr).cast(),
                        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                        host.as_mut_ptr(),
                        host.len() as libc::socklen_t,
                        std::ptr::null_mut(),
                        0,
                        libc::NI_NAMEREQD,
                    )
                }
            }
        };
        if code != 0 {
            return Err(reverse_failure(code));
        }
        // SAFETY: on success getnameinfo wrote a NUL-terminated name into `host`.
        let name = unsafe { CStr::from_ptr(host.as_ptr()) };
        Ok(name.to_string_lossy().into_owned())
    }

    #[cfg(windows)]
    fn reverse(&self, ip: IpAddr) -> Result<String, DnsFailure> {
        use windows_sys::Win32::Networking::WinSock::{GetNameInfoW, NI_NAMEREQD};
        winsock();
        let mut host = [0u16; 1025];
        let (addr, addr_len) = sockaddr_of(&ip);
        let ptr = match &addr {
            SockAddr::V4(a) => std::ptr::from_ref(a).cast(),
            SockAddr::V6(a) => std::ptr::from_ref(a).cast(),
        };
        // SAFETY: `ptr` is a fully initialised SOCKADDR_IN(6) and `host` is a
        // writable wide buffer whose element count is passed alongside it.
        let code = unsafe {
            GetNameInfoW(
                ptr,
                addr_len,
                host.as_mut_ptr(),
                host.len() as u32,
                std::ptr::null_mut(),
                0,
                NI_NAMEREQD as i32,
            )
        };
        if code != 0 {
            return Err(reverse_failure(code));
        }
        let end = host.iter().position(|c| *c == 0).unwrap_or(host.len());
        Ok(String::from_utf16_lossy(&host[..end]))
    }

    fn forward(&self, host: &str, ipv6: bool) -> Result<Vec<String>, DnsFailure> {
        let query = (host, 0u16)
            .to_socket_addrs()
            .map_err(|error| DnsFailure::Other(error.to_string()))?;
        Ok(query
            .filter(|a| a.is_ipv6() == ipv6)
            .map(|a| a.ip().to_string())
            .collect())
    }
}

/// Free bytes on the filesystem holding `path`.
pub fn free_space(path: &Path) -> std::io::Result<u64> {
    spiderhead_core::filesystem::free_space(path)
}
