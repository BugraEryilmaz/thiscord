//! Synthetic codec/packetization cost only: no devices, DSP or network claims.
use std::time::Instant;
use thiscord_frontend::audio::{mixer::FRAME, transmit::Encoder};

fn main() {
    for scenario in ["silence", "alternating"] {
        for dtx in [false, true] {
            let mut encoder = Encoder::new(dtx).unwrap();
            let mut packet = [0; 4000];
            let mut packets = 0u64;
            let mut payload_bytes = 0u64;
            let start = Instant::now();
            for frame in 0..3000 {
                let speech = scenario == "alternating" && frame % 100 < 20;
                let pcm = std::array::from_fn(|i| {
                    if speech {
                        ((frame * FRAME + i) as f32 * 0.04).sin() * 0.01
                    } else {
                        0.0
                    }
                });
                let (n, omitted, _) = encoder.encode(&pcm, speech, &mut packet).unwrap();
                if !omitted {
                    packets += 1;
                    payload_bytes += n as u64;
                }
            }
            println!(
                "{}",
                serde_json::json!({
                    "scenario": scenario, "dtx": dtx, "media_seconds": 60,
                    "packets": packets, "packets_per_second": packets as f64 / 60.0,
                    "payload_bytes": payload_bytes, "rtp_header_bytes": packets * 12,
                    "encode_ms": encoder.stats.encode_ns as f64 / 1_000_000.0,
                    "loop_ms": start.elapsed().as_secs_f64() * 1000.0,
                    "os": std::env::consts::OS, "arch": std::env::consts::ARCH,
                    "scope": "synthetic codec and packet decisions; RTP header only; excludes SRTP/UDP/IP/RTCP/ICE/TURN, DSP and devices"
                })
            );
        }
    }
}
