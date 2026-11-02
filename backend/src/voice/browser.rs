//! Browser publishers (`Answer.browser_publisher`). Browsers cannot choose RTP
//! SSRCs and do not handle `ServerEvent::ScreenKeyframe`, so their tracks are
//! identified by the negotiated m-line that received them and keyframe demand
//! reaches them as RTCP PLI. Native publishers never take these paths.
use super::*;
use rtc::rtcp::payload_feedbacks::picture_loss_indication::PictureLossIndication;

/// A browser video track and the SSRC it chose.
type VideoTrack = (u32, Arc<dyn TrackRemote>);

#[derive(Default)]
pub(super) struct Publisher {
    /// Set from the Answer before the remote description is applied.
    pub enabled: AtomicBool,
    peer: OnceLock<Weak<dyn PeerConnection>>,
    /// Local mid -> `MediaKind` track index, from the offer's m-line order.
    mids: OnceLock<HashMap<String, usize>>,
    /// Own-slot screen and camera tracks (`view_index(0) / ROOM_CAPACITY`),
    /// kept only to address PLI to their browser-chosen SSRC.
    video: std::sync::Mutex<[Option<VideoTrack>; 2]>,
}

/// Media sections' mids in m-line order. The SFU adds one track per
/// `MediaKind` track index, so m-line position is the track index.
pub(super) fn mids(sdp: &str) -> HashMap<String, usize> {
    let mut mids = HashMap::new();
    let mut index = None::<usize>;
    for line in sdp.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with("m=") {
            index = Some(index.map_or(0, |i| i + 1));
        } else if let (Some(mid), Some(index)) = (line.strip_prefix("a=mid:"), index)
            && index < MediaKind::TRACK_COUNT
        {
            mids.entry(mid.trim().to_owned()).or_insert(index);
        }
    }
    mids
}

fn video_index(kind: MediaKind) -> Option<usize> {
    kind.is_video().then(|| kind.view_index(0) / ROOM_CAPACITY)
}

impl Publisher {
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    pub fn bind(&self, peer: &Arc<dyn PeerConnection>, offer_sdp: &str) {
        let _ = self.peer.set(Arc::downgrade(peer));
        let _ = self.mids.set(mids(offer_sdp));
    }
    /// The kind of a browser track, from the transceiver whose receiver owns
    /// it. Only the member's own microphone, screen, shared-audio and camera
    /// m-lines publish; tracks on any other m-line are `None` (left unread).
    pub async fn kind(&self, track: &Arc<dyn TrackRemote>, slot: usize) -> Option<MediaKind> {
        let peer = self.peer.get()?.upgrade()?;
        let mids = self.mids.get()?;
        // Compare addresses only (as integers, so the future stays Send).
        let target = Arc::as_ptr(track) as *const () as usize;
        for transceiver in peer.get_transceivers().await {
            let Ok(Some(receiver)) = transceiver.receiver().await else {
                continue;
            };
            if Arc::as_ptr(receiver.track()) as *const () as usize != target {
                continue;
            }
            let mid = transceiver.mid().await.ok().flatten()?;
            let (kind, owner) = MediaKind::from_track(*mids.get(&mid)?)?;
            return (owner == slot).then_some(kind);
        }
        None
    }
    pub fn publishing(&self, kind: MediaKind, ssrc: u32, track: &Arc<dyn TrackRemote>) {
        if let Some(index) = video_index(kind) {
            self.video.lock().unwrap()[index] = Some((ssrc, track.clone()));
        }
    }
    /// Asks the browser encoder for an IDR; browsers answer PLI on their media
    /// SSRC. Returns false when no such track exists yet.
    pub async fn keyframe(&self, kind: MediaKind, slot: usize) -> bool {
        let Some(index) = video_index(kind) else {
            return false;
        };
        let Some((ssrc, track)) = self.video.lock().unwrap()[index].clone() else {
            return false;
        };
        track
            .write_rtcp(vec![Box::new(PictureLossIndication {
                sender_ssrc: media_ssrc(kind.track_index(slot)),
                media_ssrc: ssrc,
            })])
            .await
            .is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mids_follow_m_line_order_and_ignore_session_attributes() {
        let mut sdp = String::from("v=0\r\na=mid:session\r\n");
        for index in 0..MediaKind::TRACK_COUNT + 1 {
            sdp.push_str(&format!(
                "m=audio 9 UDP/TLS/RTP/SAVPF 111\r\na=mid:m{index}\r\n"
            ));
        }
        let mids = mids(&sdp);
        assert_eq!(mids.len(), MediaKind::TRACK_COUNT);
        assert_eq!(mids.get("m0"), Some(&0));
        assert_eq!(
            mids.get(&format!("m{}", MediaKind::TRACK_COUNT - 1)),
            Some(&(MediaKind::TRACK_COUNT - 1))
        );
        assert!(!mids.contains_key("session"));
        assert!(!mids.contains_key(&format!("m{}", MediaKind::TRACK_COUNT)));
        assert_eq!(video_index(MediaKind::ScreenVideo), Some(0));
        assert_eq!(video_index(MediaKind::Camera), Some(1));
        assert_eq!(video_index(MediaKind::SystemAudio), None);
    }
}
