# HTTPS
- [x] Self signed certificate
- [ ] A real certificate
# Signalling server
- [x] Database connection management (postgresql)
- [x] Register
  - [x] Activation email
  - [x] Resend activation email
- [x] Login
  - [ ] Move session to the database
  - [x] Permissions management
  - [ ] Cache permissions per user per server
  - [ ] Password reset
  - [ ] Password change
- [ ] Logout
- [x] Create server
- [ ] Join server
- [ ] Leave server
- [ ] List my servers
- [ ] Create room
- [ ] Join room
- [ ] Leave room
- [ ] List rooms
- [ ] List users
- [ ] List messages
- [ ] Send message
# SFU Media server
- [x] WebRTC connection management
  - [x] SDP negotiation
  - [x] ICE candidate exchange
- [ ] Quality of service detection
- [ ] Media stream management
# WebRTC Frontend
- [x] WebRTC connection management
  - [x] SDP negotiation
  - [x] ICE candidate exchange
- [x] Audio stream management
  - [x] Background receive audio should not assume single audio track
  - [ ] Microphone selection
  - [ ] Microphone volume control
  - [ ] Microphone mute/unmute
  - [ ] Activity detection
  - [ ] Audio effects (e.g., noise suppression)
  - [ ] Speaker selection
  - [ ] Speaker volume control per user
  - [ ] Speaker deafen/undeafen
- [ ] Video stream management
- [ ] Screen sharing
- [ ] Chat management
- [ ] User management
- [ ] Room management
- [ ] Server management

# Random