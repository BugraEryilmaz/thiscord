# Accessibility and keyboard navigation

Thiscord's UI (desktop WebView and the `/app` web client) is designed to be
usable with a keyboard alone and with screen readers. This page lists the
shortcuts and the semantics the UI promises, and what has not been verified.
Matching, ranking and index logic lives in `frontend/src/keyboard.rs` (unit
tested natively); DOM wiring is in `frontend/src/app/keyboard.rs` and
`frontend/src/app/dialog.rs`.

## Shortcuts

All shortcuts apply only while Thiscord has focus. They have a Control/Command
or Alt modifier, so they also work in the message box. Press **Ctrl+/** (⌘/ on
macOS) for the in-app reference, which shows the current keys.

The app commands below (quick switcher, reference, search, channel, unread and
server cycling) are recordable in **Settings → Keyboard shortcuts** (desktop:
**Shortcuts & overlay**) under **Navigation**, like the voice and overlay
shortcuts: click, press the keys, save. They are stored on the device
(`thiscord.app-shortcuts.v1`), must use Ctrl, Alt or Super or be a function key,
and may not repeat each other or a voice/overlay shortcut on that device; an
empty value disables a command. The compatibility aliases (Ctrl or ⌘, `/` from
any key, Ctrl+F by character, Ctrl+Shift+↑/↓ for servers) apply only while that
command keeps its default. Esc, Enter, list arrows, Shift+F10 and ↑ to edit stay
fixed; settings lists them under **Fixed keys**.

Defaults:

| Keys | Action |
| --- | --- |
| Ctrl/⌘+K | Quick switcher: search joined servers, channels, friends (DMs) and groups |
| Alt+↑ / Alt+↓ | Previous/next text channel in the open server; on Home, previous/next conversation |
| Alt+Shift+↑ / Alt+Shift+↓ | Previous/next unread (and unmuted) channel or conversation |
| Ctrl+Alt+↑ / Ctrl+Alt+↓ | Previous/next server, with Home first (Ctrl+Shift+↑/↓ also works outside text fields) |
| Ctrl/⌘+/ | Keyboard shortcut reference |
| Esc | Closes the open menu, popover, search or dialog; otherwise jumps to the latest message and marks the channel read |
| Ctrl/⌘+F | Search the open conversation (existing) |
| ↑ in an empty message box | Edit your last message (existing) |
| Shift+F10 or the Menu key | Context menu for the focused server, channel, member, message author or conversation |
| Ctrl+Shift+Space | Push-to-talk (default; change it in Settings → Shortcuts) |

Notes and conflict rules:

- Command matching uses `KeyboardEvent.code` for letters (Ctrl+K works on
  non-Latin layouts) and accepts the `/` character for Ctrl+/ (Turkish Q layouts
  type `/` with Shift).
- Configured web mute/deafen shortcuts take precedence over navigation
  shortcuts with the same keys. Desktop global shortcuts are captured by the OS
  before the WebView sees them.
- While a modal dialog is open, navigation shortcuts are ignored; Ctrl+K and
  Ctrl+/ close their own dialogs.
- Ctrl+Shift+↑/↓ is ignored in text fields, where it selects text. On macOS,
  Option+↑/↓ in the message box navigates channels instead of moving the caret
  by paragraph (as in Discord).
- Alt+↑/↓ skips voice channels: selecting one joins voice.
- The quick switcher lists the open server's channels plus channel lists of
  other servers opened during this session (the cache is cleared on sign-out).
  Choosing one re-reads and re-authorizes the server before opening the channel;
  a channel that disappeared reports an error instead of opening.

## Lists, menus and grids

- **Roving focus**: the server rail, channel list, member list and direct-message
  list are a single Tab stop each (the last focused, else the selected, else the
  first item). ↑/↓ move, Home/End jump, Enter/Space activate.
- Settings navigation, friend rows and message option menus use ↑/↓/Home/End
  between their items; context menus and their submenus also use →/←.
- Emoji palettes (composer, custom status and "More reactions") are one Tab stop
  and use arrow keys spatially across rows and emoji groups. Quick reactions and
  soundboard sounds use the same arrow movement.
- Theme and accent pickers are radio groups: arrows move and select.
- Call media tiles are buttons with two Tab stops (the tile and its options
  button, plus the viewer avatars as one group): Enter/Space focuses or unfocuses
  a stream, F opens full screen, Alt+←/→ (or ↑/↓) moves it, Alt+Home/End moves it
  to the start or end, and Shift+F10 or the context-menu key opens its options.
  Focus stays on the tile (or the menu item) across repeated moves. The options
  menu holds only menu items (moves that are not possible stay focusable and are
  announced as unavailable); volume sliders follow it as a labelled group.
  Hiding a stream moves focus to its chip in the bar; restoring it from the chip
  focuses the tile again. Tabbing out of an open options menu closes it.
  Escape leaves focus mode only while the media view has focus, or nothing
  does and the last click was inside it. A polite live region announces moves, focus and hide/restore; tile animations
  stop under reduced motion.

Group conversations: the conversation list is one roving list of friends and
groups ordered by title. The group friend picker focuses its search field; Down
moves into the friend checkboxes, arrows move between them, and Space or Enter
toggles one. Selected friends appear as removable chips with labelled buttons. The
group title in the header is a button that turns into a name field (Enter saves,
Escape cancels). The member panel is a roving list; each owner action menu is
labelled with the member's name and closes on Escape. The call bar is a polite
live region, and its ringing pulse stops under reduced motion.

## Dialogs

Every modal is a native `<dialog>` opened through `dialog::open`, which adds
`aria-modal`, and returns focus to the invoking control when closing would
otherwise leave focus on the page body. Native modals make the page behind them
inert and close on Escape (dialogs with a running action block Escape). Tab and
Shift+Tab wrap inside the focused modal. Each dialog has a labelled title.
The confirmation dialog focuses Cancel first.

## Screen-reader semantics

- Landmarks: `nav` for the server rail, channel list, direct messages and
  settings sections; `main` (`#app-content`) for the conversation area;
  `aside` (complementary) for the member list and the account/voice dock. A
  visually hidden "Skip to content" link is the first Tab stop.
- `aria-current="page"` marks the open server, channel, conversation and settings
  page; `aria-pressed` marks toggle buttons (mute, deafen, camera, stream audio,
  filters) whose names stay constant; `aria-expanded` marks popovers, member list
  and search. Channel buttons announce kind, unread count, mentions, muted state
  and voice connection.
- Chat history is a labelled list; each message is an `article` named by author
  and full local time. The scrollable history is a focusable region.
- Spoilers are buttons ("Spoiler, press to reveal") until revealed; hidden
  content is `inert` and `aria-hidden`. Spoiler attachments have a labelled
  reveal button that moves focus to the revealed attachment.
- Voice-message players are labelled groups with a play/pause button, a
  `role=slider` waveform (arrows ±5 s, Page Up/Down ±15 s, Home/End) and a
  speed button. The recorder starts with Enter/Space on the microphone button,
  moves focus to Stop, and Escape discards. "(edited)" is a button that opens
  the edit-history dialog.
- A single polite live region announces navigation targets, "marked as read",
  connection changes (Connected/Reconnecting), voice connect/disconnect, and new
  messages from others **in the open channel only**: author plus at most 140
  characters, batched to one announcement per second ("N new messages. Latest
  from …"). Bodies from other channels are never announced. Typing indicators use
  a polite region; status toasts use `role=status`, errors `role=alert`.

## Visual

- Every interactive element gets a 2px `focus-visible` outline in
  `--focus-ring` (the accent mixed toward the theme's strongest text color, so
  custom themes keep contrast). Text fields with their own ring keep it.
- `prefers-reduced-motion: reduce` disables animations, transitions and smooth
  scrolling.
- Small secondary text uses the muted token at ≥90% opacity and Tailwind text
  opacities of at least 60%. A unit test checks muted text against every preset
  background at WCAG AA (4.5:1); the Daylight muted token was darkened slightly
  (`#5c5f68` → `#53565f`) to pass on the rail. Custom colors can still produce
  low contrast; Appearance settings warn about hard-to-read combinations.

## Not yet verified

- Screen-reader acceptance with NVDA (Windows/WebView2), VoiceOver (macOS
  WKWebView and iOS Safari) and Orca (Linux WebKitGTK) has not been performed;
  announcements and roving focus are implemented to ARIA patterns but untested
  with assistive technology.
- Shortcut behavior was checked only by compilation and unit tests on Windows;
  WebKit-based WebViews (macOS, Linux) and keyboard layouts other than US/Turkish
  Q need hands-on checks, as does Windows Ctrl+Alt+Arrow display-rotation
  hotkeys on Intel graphics drivers.
- The browser admin dashboard (`/admin`) has not been audited.
