//! Bounded UDP burst capacity for native media. Windows' default receive buffer
//! can lose a 4K video batch on loopback before the WebRTC driver is scheduled.
use std::{
    future::Future,
    io,
    net::{SocketAddr, TcpListener, UdpSocket},
    pin::Pin,
    sync::Arc,
    time::Duration,
};
use webrtc::runtime::{
    AsyncInterval, AsyncTcpListener, AsyncTcpStream, AsyncUdpSocket, JoinHandle, Runtime,
};

#[derive(Debug)]
pub struct MediaRuntime(Arc<dyn Runtime>);
impl Default for MediaRuntime {
    fn default() -> Self {
        Self(webrtc::runtime::default_runtime().expect("Media peers require an async runtime"))
    }
}
impl Runtime for MediaRuntime {
    fn spawn(&self, task: Pin<Box<dyn Future<Output = ()> + Send>>) -> Box<dyn JoinHandle> {
        self.0.spawn(task)
    }
    fn spawn_reactor(
        &self,
        size: usize,
        task: Pin<Box<dyn Future<Output = ()> + Send>>,
    ) -> Box<dyn JoinHandle> {
        self.0.spawn_reactor(size, task)
    }
    fn wrap_udp_socket(&self, socket: UdpSocket) -> io::Result<Arc<dyn AsyncUdpSocket>> {
        // Fixed per-socket bounds, including ICE restart/rebind and TURN sockets.
        // These absorb scheduler bursts, not application-level media backlogs.
        let options = socket2::SockRef::from(&socket);
        options.set_recv_buffer_size(1024 * 1024)?;
        options.set_send_buffer_size(256 * 1024)?;
        self.0.wrap_udp_socket(socket)
    }
    fn wrap_tcp_listener(&self, listener: TcpListener) -> io::Result<Arc<dyn AsyncTcpListener>> {
        self.0.wrap_tcp_listener(listener)
    }
    fn connect_tcp<'a>(
        &'a self,
        remote: SocketAddr,
    ) -> Pin<Box<dyn Future<Output = io::Result<Arc<dyn AsyncTcpStream>>> + Send + 'a>> {
        self.0.connect_tcp(remote)
    }
    fn resolve_host<'a>(
        &'a self,
        host: &'a str,
    ) -> Pin<Box<dyn Future<Output = io::Result<Vec<SocketAddr>>> + Send + 'a>> {
        self.0.resolve_host(host)
    }
    fn sleep(&self, duration: Duration) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.0.sleep(duration)
    }
    fn interval(&self, period: Duration) -> Box<dyn AsyncInterval> {
        self.0.interval(period)
    }
    fn block_on(&self, task: Pin<Box<dyn Future<Output = ()> + '_>>) {
        self.0.block_on(task);
    }
    fn yield_now(&self) -> Pin<Box<dyn Future<Output = ()> + Send + 'static>> {
        self.0.yield_now()
    }
    fn name(&self) -> &'static str {
        "tokio-media"
    }
}

#[cfg(all(test, target_os = "windows"))]
mod tests {
    use super::*;
    #[tokio::test]
    async fn video_batch_survives_one_delayed_receive_wakeup() {
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let address = socket.local_addr().unwrap();
        socket.set_nonblocking(true).unwrap();
        let receiver = MediaRuntime::default().wrap_udp_socket(socket).unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        // At 38.4 Mbps a 16 ms pacing wake can enqueue ~77 KB. Do not poll
        // the receiver until the entire batch is in the kernel socket buffer.
        for index in 0..80u8 {
            sender.send_to(&[index; 1200], address).unwrap();
        }
        let mut received = [false; 80];
        for _ in 0..80 {
            let mut packet = [0; 1500];
            let (length, _) =
                tokio::time::timeout(Duration::from_secs(1), receiver.recv_from(&mut packet))
                    .await
                    .expect("UDP pacing batch was lost")
                    .unwrap();
            assert_eq!(length, 1200);
            assert!(packet[0] < 80);
            assert!(!received[usize::from(packet[0])]);
            received[usize::from(packet[0])] = true;
        }
        assert!(received.into_iter().all(|seen| seen));
    }
}
