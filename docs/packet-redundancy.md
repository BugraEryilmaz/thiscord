# Microphone packet redundancy

Audio & voice → Advanced & troubleshooting contains **Microphone packet redundancy
(RED)**. `packet_redundancy` defaults to false, including existing `audio.json` files.
The existing local and administrator remote settings paths persist the preference.
The false field is omitted from serialized settings so older strict settings
readers can accept the disabled configuration. Enabling it on an older remote
client can be rejected until that client is updated.
Browser preview has no native voice transport.

When negotiated, RED repeats up to two previously admitted 20 ms Opus frames.
It can replace missing encoded bytes arriving before the existing playout deadline;
it does not cure arbitrary 20% loss. A burst longer than two packets, loss at the
end of speech, late delivery, or unavailable history can still require FEC/PLC.
No additional steady playout buffering is introduced. Copies arrive one or two
packet intervals later (20–40 ms), so existing jitter/device timing determines
whether they are still useful.

## Wire profile and fallback

The shared Rust contract is `shared/src/rtp_red.rs`, following
[RFC 2198](https://www.rfc-editor.org/rfc/rfc2198.txt). Dynamic payload type 112 is
`audio/red`, clock 48 kHz, two SDP channels (Opus mono microphone content remains
valid), with `a=fmtp:112 111/111/111`. Both RED and Opus 111 must appear on every
negotiated audio m-line. The server advertises the capability; a new client answers
only when supported. Older servers receive the unchanged answer shape and raw Opus.
If a legacy source replaces a previously protected slot in an active call, the
desktop automatically reconnects with fresh authorization and the existing
1–30 second backoff plus jitter. Audio briefly pauses; users do not need to join
manually. Only a new transport/session clears the old tombstones and restores
ordinary Opus fallback without weakening source ordering. Repeated legacy rejoins
on that fresh session do not trigger recovery unless a modern source has since
occupied the slot. Disconnect cancels recovery; access denial or a replacement
call remains terminal. Synthetic test bots retain their existing stop-on-error
behavior. This requires an updated desktop; older clients still require rejoining.
The server unwraps RED to the primary Opus payload for older listeners, retaining
the primary RTP sequence, timestamp and marker. System audio/video use their
existing formats. No decoding occurs on the SFU.

RED offsets are exactly 960 or 1,920 samples, oldest first, without duplicate
timestamps. At most two redundant blocks of 160 bytes each and a total 1,100-byte
RTP payload are accepted. Primary payloads too large for the RED container use
raw Opus within the same 1,100-byte cap; larger microphone packets are discarded.
All parser work, retained history and receiver queues are bounded. Unknown inner
payload types, malformed headers, excessive history and invalid offsets are rejected.

Receiver recovery uses timestamps only. RED does not preserve prior sequence
numbers or markers, so recovery does not manufacture them or decrement transport
loss counters. The primary wins when both original and redundant data are present.
Consumed, expired and pre-epoch timestamps cannot be replayed. First-packet history
after a new source/privacy anchor is conservatively discarded.

## Combination and bandwidth budget

Opus packets with code-3 padding (including DRED), no-data DTX packets and frames
larger than 160 bytes clear the **entire** sender RED history. Older unpadded frames
cannot survive across that boundary. Padded DRED packets may carry a primary-only
RED container, but receive no RED recovery copies. If every packet carries DRED
padding, RED adds no packet recovery protection; this is not additive RED+DRED
history. Adaptive in-band FEC remains inside the primary Opus payload and is subject
to the same size limits. Oversized, expired and rejected transport frames also
invalidate retained history.

Adaptive FEC feedback counts each successfully admitted outer RTP packet once,
regardless of redundant block count. Its source identity is stable for the unique
connection; privacy/PTT/refresh generations clear audio history without resetting
the numeric path estimate. Connection replacement starts fresh feedback, and
omitted or rejected packets never advance the outgoing sequence.

For an 80-byte/20 ms Opus primary, steady RED payload is 249 bytes: three copies
plus nine RED header bytes, **99.6 kbit/s versus 32 kbit/s** before RTP/UDP/IP/SRTP.
At the 160-byte repeat limit it is 489 bytes, or 195.6 kbit/s. A 161-byte primary
has no copies (162 bytes including the primary RED header). Small 3-byte primary
frames can total 18 bytes; no-data packets are never repeated. These are payload
rates, not whole-network bandwidth or an acoustic quality claim.

## Privacy and transport ordering

Updated peers use microphone privacy fencing even with the RED preference off.
WebSocket control and UDP media do not have a common delivery order. A publisher
therefore announces a monotonically increasing, nonzero `u64` generation over
authenticated signaling, waits for the matching acknowledgment, and carries the
generation as two RTP CSRC words. Old acknowledgments cannot release a newer local
generation. Only one request is pending; rapid changes coalesce, duplicates are
idempotent, stale requests cannot roll back server state, and exhaustion requires
a new connection. The five-second acknowledgment deadline applies while signaling
is attached; existing bounded signaling recovery retries the current generation.

The SFU checks the publisher tag before forwarding, reconstructs trusted output
metadata, and rechecks source/listener authority and epochs during pending sends.
It never gives a delayed old-tag UDP packet the current privacy revision.
Microphone packets sent to listeners have exactly two CSRC words:

- Stable `microphone_source_epoch`, allocated for this membership/connection.
- Effective privacy revision: `max(source.microphone_epoch, receiver.receive_floor)`.

Both use one checked, monotonic room allocator, reserving zero for legacy. This
lets the acknowledged receiver revision reject packets already in flight before
its local boundary. A future source restriction allocates above earlier receiver
revisions. Source identity must first be admitted by the roster; a packet can only
advance the privacy revision of that known source. Tombstones survive Remote removal
to reject stale rosters and preserve the separate effective RTP high-water mark
when a restricted stream is recreated. An absent source cannot reappear with its retired source
epoch; it must rejoin. New receiving sessions start with fresh fences.

Slot admission reserves capacity before SDP negotiation finishes. Such pending
members are omitted from participant rosters and media delivery until their
answer has been applied successfully. They must not temporarily appear as
epoch-zero legacy publishers: reusing a protected slot that way makes existing
clients reject the roster before the joining client's capabilities are known.

Mute/deafen, PTT edges, settings changes, device/delivery changes, replacement and
permission invalidation clear retained encoded history. Observed receive fences
clear jitter donors, decoder state, activity, rate correction and callback PCM.
Microphone rosters exclude muted, deafened or Speak-denied publishers. System audio
retains its separate policy. Data already rendered before a restriction was observed
cannot be recalled. Legacy peers cannot provide the publisher-generation guarantee;
they use ordinary Opus and do not enable the modern redundancy protocol.

A receiver boundary also challenges each modern publisher over authenticated
signaling. Delivery from that source stays suppressed until a fresh publisher
RTP generation echoes the current server challenge. Merely processing a newer
generation after the boundary is insufficient: an unrelated generation update
may itself have been delayed. The publisher clears encoder state, queued capture
and RED history before answering. Thus UDP held before SFU ingress cannot acquire
the new receiver floor after a challenge has completed.

Each publisher retains one immutable issued challenge and one coalesced pending
maximum, bound to its membership. New floors cannot stale a valid issued reply or
extend its deadline. After processing the proof ACK, the publisher sends a
current-generation Ready notice over signaling. The next challenge waits at
least 250 ms after that notice, so delayed/batched ACK delivery cannot consume
the service interval before the publisher observes it. Duplicate Ready notices
cannot extend the cooldown. Missing Ready also times out after five seconds.
Challenges are sent at most once per 250 ms, with at most
three transmissions within two seconds. After five seconds without a valid reply,
the server removes only that publisher and reports a refresh timeout; unrelated
sources remain eligible. A matching reply at the deadline is rejected. Muted and
silent publishers answer over signaling without waiting for RTP. The initial
challenge timer starts only after the client has established its audio session
and announced its first local generation.
While signaling is detached, the server neither consumes challenge transmissions
nor counts detached time against the proof/Ready deadline. The existing bounded
signaling grace still applies. Reattachment replays an outstanding challenge,
including one whose proof was accepted but whose Ready was lost, without
resetting its attached-time budget or releasing unproven media. Ordinary roster
changes use the current authoritative access epoch instead of querying the
database again for every boundary; access invalidation and periodic authorization
checks remain in force.
Server moderation boundaries advance the receiver floor and challenge every modern
publisher, including the moderated participant's own microphone. Ingress stays
closed until that participant proves the new challenge; egress history is filtered
against the new receiver floor. Restriction and release each fence retained RED
and embedded DRED history. A persistent moderation revision preserves coalesced
edges, so unchanged final booleans cannot reopen an old publication.

A source-only refresh changes publisher privacy and generation, while keeping its
own receive floor unchanged; it cannot recursively trigger other publishers.
Concurrent real local PTT/settings boundaries take priority. Only the current
connection, generation and challenge ACK can reopen admission. Sources resume
individually; legacy epoch-zero sources use ordinary Opus outside this strict
two-hop guarantee. Legacy packets cannot contribute RED/DRED recovery history.

Control boundaries interrupt microphone sending and listening. A local boundary
requires one signaling RTT; incoming modern sources additionally require a
publisher challenge exchange and its first fresh encoded frame. Challenge polling
can add up to 250 ms, and concurrent changes can require another exchange. A
source-only refresh also briefly holds that publisher's local microphone playback,
but does not change its server receive floor. This cost applies even when RED
copying is off on an updated connection. There is no added steady speech buffer.
Boundary pauses count as intentional silence/control discards, not network loss.

## Validation scope

Deterministic encrypted localhost tests use two DTLS-SRTP hops, loss/reordering,
sequence/timestamp wrap, timestamp remapping, malformed packets and old-peer fallback.
A 12-frame CBR fixture loses one isolated packet and two consecutive packets:
RED reconstructs all 12 encoded frames; raw Opus delivers nine. Offered payload is
2,736 versus 960 bytes; nine received primary packets total 1,989 versus 720 bytes.
Copies first arrive 20 or 40 ms after the missing frame. These measurements verify
encoded bytes, not intelligibility, internet loss behavior or hardware latency.

The final local production-SFU regression measured 383.149 ms from a receiver
boundary through publisher challenge to the first fresh RTP packet. This includes
challenge cadence and loopback signaling, excludes devices/DSP, and is not a
latency bound. Delayed/batched ACK tests separately verify current RTP and actual
decoded mixer PCM during a controlled 250 ms service window while a newer listener
floor remains blocked; they do not promise that window on arbitrary networks.

Additional native tests exercise real jitter decisions, padding/DTX/size boundaries,
pending-write expiry, control acknowledgments, callback-safe receive lifecycle and
the production SFU. PostgreSQL-backed SFU integration tests are ignored by default
and run with `--include-ignored` in CI. Local validation results and unavailable
platform checks belong in the PR; Windows tests do not establish macOS/Linux device
compatibility. Physical multi-client listening and adverse real network acceptance
remain separate from these deterministic checks.
