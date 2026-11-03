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
macOS) for the in-app reference; **Settings → Keyboard shortcuts** (desktop:
**Shortcuts & overlay**) shows the same table.

| Keys | Action |
| --- | --- |
| Ctrl/⌘+K | Quick switcher: search joined servers, channels and friends (DMs) |
| Alt+↑ / Alt+↓ | Previous/next text channel in the open server; on Home, previous/next conversation |
| Alt+Shift+↑ / Alt+Shift+↓ | Previous/next unread (and unmuted) channel or conversation |
| Ctrl+Alt+↑ / Ctrl+Alt+↓ | Previous/next server, with Home first (Ctrl+Shift+↑/↓ also works outside text fields) |
| Ctrl/⌘+/ | Keyboard shortcut reference |
| Esc | Closes the open menu, popover, search or dialog; otherwise jumps to the latest message and marks the channel read |
| Ctrl/⌘+F | Search the open conversation (existing) |
| ↑ in an empty message box | Edit your last message (existing) |
| Shift+F10 or the Menu key | Context menu for the focused server, channel, member, message author or conversation |
| Ctrl+Shift+Space | Push-to-talk (fixed, existing) |

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
