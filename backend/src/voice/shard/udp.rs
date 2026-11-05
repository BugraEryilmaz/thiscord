//! A shard's UDP socket. On Linux, receives and sends use `recvmmsg` and
//! `sendmmsg`, one system call per batch of datagrams (each to its own peer);
//! elsewhere they fall back to one call per datagram.
use bytes::BytesMut;
use std::{io, net::SocketAddr};
use tokio::io::Interest;

/// Datagrams per receive call. A full batch is drained again before the shard
/// returns to its other inputs only up to `MAX_RECEIVE_ROUNDS` times.
pub(super) const RECEIVE_BATCH: usize = 64;
/// Larger than any WebRTC datagram this SFU accepts or sends (MTU ~1200-1500).
const DATAGRAM: usize = 2048;
/// Datagrams per send call; Linux accepts at most UIO_MAXIOV (1024).
const SEND_BATCH: usize = 128;
/// Kernel socket buffer request. The kernel clamps it to `net.core.*mem_max`.
#[cfg(target_os = "linux")]
const SOCKET_BUFFER: libc::c_int = 4 * 1024 * 1024;

pub(super) fn bind(address: SocketAddr) -> io::Result<std::net::UdpSocket> {
    let socket = std::net::UdpSocket::bind(address)?;
    socket.set_nonblocking(true)?;
    #[cfg(target_os = "linux")]
    linux::buffers(&socket, SOCKET_BUFFER);
    Ok(socket)
}

pub(super) struct Socket {
    io: tokio::net::UdpSocket,
    local: SocketAddr,
    buffers: Vec<[u8; DATAGRAM]>,
}

impl Socket {
    /// Must run inside the shard's runtime, which then polls the socket.
    pub fn new(socket: std::net::UdpSocket) -> io::Result<Self> {
        let local = socket.local_addr()?;
        Ok(Self {
            io: tokio::net::UdpSocket::from_std(socket)?,
            local,
            buffers: vec![[0; DATAGRAM]; RECEIVE_BATCH],
        })
    }

    pub fn local_addr(&self) -> SocketAddr {
        self.local
    }

    pub async fn readable(&self) -> io::Result<()> {
        self.io.readable().await
    }

    pub async fn writable(&self) -> io::Result<()> {
        self.io.writable().await
    }

    /// Receives up to one batch without blocking. `Ok(0)` means nothing ready.
    pub fn try_receive(&mut self, mut deliver: impl FnMut(SocketAddr, &[u8])) -> io::Result<usize> {
        #[cfg(target_os = "linux")]
        {
            let buffers = &mut self.buffers;
            let result = self.io.try_io(Interest::READABLE, || {
                linux::receive(&self.io, buffers, &mut deliver)
            });
            match result {
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => Ok(0),
                other => other,
            }
        }
        #[cfg(not(target_os = "linux"))]
        {
            let mut count = 0;
            while count < RECEIVE_BATCH {
                match self.io.try_recv_from(&mut self.buffers[0]) {
                    Ok((len, from)) => {
                        deliver(from, &self.buffers[0][..len]);
                        count += 1;
                    }
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) if count == 0 => return Err(error),
                    Err(_) => break,
                }
            }
            Ok(count)
        }
    }

    /// Sends as many datagrams as the kernel accepts without blocking and
    /// returns how many left. A datagram the kernel rejects outright (as
    /// opposed to would-block) is skipped and counted as sent.
    pub fn try_send(&self, datagrams: &[(SocketAddr, BytesMut)]) -> usize {
        let mut sent = 0;
        while sent < datagrams.len() {
            let chunk = &datagrams[sent..datagrams.len().min(sent + SEND_BATCH)];
            #[cfg(target_os = "linux")]
            let result = self
                .io
                .try_io(Interest::WRITABLE, || linux::send(&self.io, chunk));
            #[cfg(not(target_os = "linux"))]
            let result = self.io.try_send_to(&chunk[0].1, chunk[0].0).map(|_| 1);
            match result {
                Ok(count) => sent += count.max(1),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                // Unreachable destination, invalid address family, oversized
                // datagram: the problem is this datagram, not the socket.
                Err(_) => sent += 1,
            }
        }
        sent
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        mem::{size_of, zeroed},
        net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6},
        os::fd::AsRawFd,
    };

    pub fn buffers(socket: &std::net::UdpSocket, bytes: libc::c_int) {
        for option in [libc::SO_RCVBUF, libc::SO_SNDBUF] {
            // SAFETY: a valid socket descriptor and a c_int option value.
            unsafe {
                libc::setsockopt(
                    socket.as_raw_fd(),
                    libc::SOL_SOCKET,
                    option,
                    (&bytes as *const libc::c_int).cast(),
                    size_of::<libc::c_int>() as libc::socklen_t,
                );
            }
        }
    }

    fn encode(address: &SocketAddr, storage: &mut libc::sockaddr_storage) -> libc::socklen_t {
        match address {
            SocketAddr::V4(address) => {
                // SAFETY: sockaddr_storage is large and aligned enough for sockaddr_in.
                let raw = unsafe { &mut *(storage as *mut _ as *mut libc::sockaddr_in) };
                raw.sin_family = libc::AF_INET as libc::sa_family_t;
                raw.sin_port = address.port().to_be();
                raw.sin_addr = libc::in_addr {
                    s_addr: u32::from_ne_bytes(address.ip().octets()),
                };
                size_of::<libc::sockaddr_in>() as libc::socklen_t
            }
            SocketAddr::V6(address) => {
                // SAFETY: sockaddr_storage is large and aligned enough for sockaddr_in6.
                let raw = unsafe { &mut *(storage as *mut _ as *mut libc::sockaddr_in6) };
                raw.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                raw.sin6_port = address.port().to_be();
                raw.sin6_flowinfo = address.flowinfo();
                raw.sin6_addr = libc::in6_addr {
                    s6_addr: address.ip().octets(),
                };
                raw.sin6_scope_id = address.scope_id();
                size_of::<libc::sockaddr_in6>() as libc::socklen_t
            }
        }
    }

    fn decode(storage: &libc::sockaddr_storage) -> Option<SocketAddr> {
        match libc::c_int::from(storage.ss_family) {
            libc::AF_INET => {
                // SAFETY: the kernel wrote a sockaddr_in for AF_INET.
                let raw = unsafe { &*(storage as *const _ as *const libc::sockaddr_in) };
                Some(SocketAddr::V4(SocketAddrV4::new(
                    Ipv4Addr::from(raw.sin_addr.s_addr.to_ne_bytes()),
                    u16::from_be(raw.sin_port),
                )))
            }
            libc::AF_INET6 => {
                // SAFETY: the kernel wrote a sockaddr_in6 for AF_INET6.
                let raw = unsafe { &*(storage as *const _ as *const libc::sockaddr_in6) };
                Some(SocketAddr::V6(SocketAddrV6::new(
                    Ipv6Addr::from(raw.sin6_addr.s6_addr),
                    u16::from_be(raw.sin6_port),
                    raw.sin6_flowinfo,
                    raw.sin6_scope_id,
                )))
            }
            _ => None,
        }
    }

    pub fn receive(
        socket: &tokio::net::UdpSocket,
        buffers: &mut [[u8; DATAGRAM]],
        deliver: &mut impl FnMut(SocketAddr, &[u8]),
    ) -> io::Result<usize> {
        let count = buffers.len().min(RECEIVE_BATCH);
        // SAFETY: plain C structures; all-zero is a valid initial state.
        let mut names: [libc::sockaddr_storage; RECEIVE_BATCH] = unsafe { zeroed() };
        let mut vectors: [libc::iovec; RECEIVE_BATCH] = unsafe { zeroed() };
        let mut headers: [libc::mmsghdr; RECEIVE_BATCH] = unsafe { zeroed() };
        for i in 0..count {
            vectors[i] = libc::iovec {
                iov_base: buffers[i].as_mut_ptr().cast(),
                iov_len: DATAGRAM,
            };
            headers[i].msg_hdr.msg_name = (&mut names[i] as *mut libc::sockaddr_storage).cast();
            headers[i].msg_hdr.msg_namelen = size_of::<libc::sockaddr_storage>() as libc::socklen_t;
            headers[i].msg_hdr.msg_iov = &mut vectors[i];
            headers[i].msg_hdr.msg_iovlen = 1;
        }
        // SAFETY: every header points at a live buffer and name for this call.
        let received = unsafe {
            libc::recvmmsg(
                socket.as_raw_fd(),
                headers.as_mut_ptr(),
                count as libc::c_uint,
                libc::MSG_DONTWAIT,
                std::ptr::null_mut(),
            )
        };
        if received < 0 {
            return Err(io::Error::last_os_error());
        }
        let received = received as usize;
        for i in 0..received {
            // Truncated datagrams are larger than any media this SFU accepts.
            if headers[i].msg_hdr.msg_flags & libc::MSG_TRUNC != 0 {
                continue;
            }
            if let Some(from) = decode(&names[i]) {
                deliver(from, &buffers[i][..headers[i].msg_len as usize]);
            }
        }
        Ok(received)
    }

    pub fn send(
        socket: &tokio::net::UdpSocket,
        datagrams: &[(SocketAddr, BytesMut)],
    ) -> io::Result<usize> {
        let count = datagrams.len().min(SEND_BATCH);
        // SAFETY: plain C structures; all-zero is a valid initial state.
        let mut names: [libc::sockaddr_storage; SEND_BATCH] = unsafe { zeroed() };
        let mut vectors: [libc::iovec; SEND_BATCH] = unsafe { zeroed() };
        let mut headers: [libc::mmsghdr; SEND_BATCH] = unsafe { zeroed() };
        for (i, (address, datagram)) in datagrams[..count].iter().enumerate() {
            let length = encode(address, &mut names[i]);
            vectors[i] = libc::iovec {
                iov_base: datagram.as_ptr() as *mut libc::c_void,
                iov_len: datagram.len(),
            };
            headers[i].msg_hdr.msg_name = (&mut names[i] as *mut libc::sockaddr_storage).cast();
            headers[i].msg_hdr.msg_namelen = length;
            headers[i].msg_hdr.msg_iov = &mut vectors[i];
            headers[i].msg_hdr.msg_iovlen = 1;
        }
        // SAFETY: every header points at a live datagram and name for this call;
        // the kernel only reads the iovec contents.
        let sent = unsafe {
            libc::sendmmsg(
                socket.as_raw_fd(),
                headers.as_mut_ptr(),
                count as libc::c_uint,
                libc::MSG_DONTWAIT,
            )
        };
        if sent < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(sent as usize)
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn socket_addresses_round_trip_through_c_storage() {
            for address in [
                "192.0.2.7:4242".parse::<SocketAddr>().unwrap(),
                "[2001:db8::1]:5000".parse().unwrap(),
            ] {
                // SAFETY: zeroed sockaddr_storage is valid.
                let mut storage: libc::sockaddr_storage = unsafe { zeroed() };
                encode(&address, &mut storage);
                assert_eq!(decode(&storage), Some(address));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn batches_reach_distinct_destinations_and_come_back_with_sources() {
        let mut server = Socket::new(bind("127.0.0.1:0".parse().unwrap()).unwrap()).unwrap();
        let clients: Vec<_> = (0..3)
            .map(|_| std::net::UdpSocket::bind("127.0.0.1:0").unwrap())
            .collect();
        let datagrams: Vec<_> = clients
            .iter()
            .enumerate()
            .map(|(i, c)| (c.local_addr().unwrap(), BytesMut::from(&[i as u8; 3][..])))
            .collect();
        // Tokio attempts the call only once the reactor has seen the socket writable.
        server.writable().await.unwrap();
        assert_eq!(server.try_send(&datagrams), 3);
        for (i, client) in clients.iter().enumerate() {
            let mut buf = [0; 16];
            let (len, from) = client.recv_from(&mut buf).unwrap();
            assert_eq!(
                (&buf[..len], from),
                (&[i as u8; 3][..], server.local_addr())
            );
            client
                .send_to(&[i as u8 + 10], server.local_addr())
                .unwrap();
        }
        let mut received = Vec::new();
        while received.len() < 3 {
            server.readable().await.unwrap();
            server
                .try_receive(|from, data| received.push((from, data.to_vec())))
                .unwrap();
        }
        received.sort();
        let mut expected: Vec<_> = clients
            .iter()
            .enumerate()
            .map(|(i, c)| (c.local_addr().unwrap(), vec![i as u8 + 10]))
            .collect();
        expected.sort();
        assert_eq!(received, expected);
    }
}
