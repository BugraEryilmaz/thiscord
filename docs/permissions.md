# Membership and permissions

## First owner and instance administration

Register and verify your account, then run the local backend command from
`backend/` in PowerShell:

```powershell
.\run-wsl.ps1 -BootstrapOwner YOUR_USERNAME
```

The command uses the backend's private database credentials. It only accepts an
existing verified username and atomically fills an empty instance-owner slot.
There is no HTTP bootstrap, shared bootstrap password, first-signup promotion or
automatic selection of an existing account. Repeating the command fails, including
when two processes race. Protect server shell access and `backend/.env`.

An instance has exactly one owner after bootstrap. The owner can appoint/remove
instance admins and transfer ownership to another verified account after recent
reauthentication. A transfer makes the former owner a regular instance user.
All verified users may create guilds within their current instance role's limits.
Instance administration **does not grant membership or guild permissions**. Future
deployment-wide moderation and configuration need explicit instance checks.

## Extensible instance roles and owner-based limits

**Settings > Instance access** shows each account's assigned role, owned-server
usage and per-server allowances. The instance owner can create named roles such
as Supporter or Partner, edit their names/limits, assign verified accounts, and
delete unused custom roles. The owner can page through a role's assigned accounts
and select an account for reassignment, including after inheriting ownership.
Administrators can inspect role policies; standard-user API responses contain
only their assigned policy. Every policy/assignment mutation requires current
instance ownership and recent reauthentication. Policy edits and deletions submit
the viewed policy revision; stale writes return 409 without automatic retry.

Each account has one instance role. Role definitions have UUID identities, names,
quotas and an immutable **standard user** or **administrator** access level. A
larger allocation does not confer administrative authority. Custom administrator
roles grant the existing instance diagnostics/audio administration access, never
guild access or role-policy administration. To change authority, assign a different
role. Ownership is a protected built-in role and can only be acquired through
bootstrap or the explicit ownership-transfer command. Built-in roles cannot be
deleted or renamed; their limits can be configured. Up to 32 definitions, including
the three built-ins, are supported so management responses remain bounded.

Defaults (all configurable by the instance owner):

| Instance role | Owned servers | Text/server | Voice/server | Members/server | Roles/server |
| --- | ---: | ---: | ---: | ---: | ---: |
| Free | 1 | 2 | 2 | 25 | 10 |
| Administrator | 10 | 25 | 25 | 250 | 50 |
| Instance owner | 100 | 100 | 100 | 500 | 100 |

Limits are positive integers, capped at 100 owned servers, 100 channels of each
kind, 500 members and 100 guild roles. Members include the owner; roles include
Everyone. Joined servers do not consume owned-server allowance. The installation
also retains its aggregate 100-server safety ceiling. Storage, bandwidth, billing,
expiring subscriptions and voice concurrency are not part of these allocations.

Every guild derives its limits from its **current owner's** current instance role,
including when another member creates a channel, adds members or creates guild
roles. Server settings display usage and limits. Both join-by-ID and direct member
addition enforce capacity, while existing members may rejoin/open at capacity.
Exceeding an allocation returns HTTP 409 with `limit_exceeded`, a specific safe
message and the standard request ID.

Downgrades, lower policy limits and instance ownership transfers preserve existing
servers, channels, memberships and roles. Growth in an already-full category is
blocked; reading, editing, removing content, moderation and leaving remain governed
by normal guild permissions. There is no automatic content deletion or eviction.
A guild ownership transfer requires a free owned-server slot and sufficient room
for **all** current channels, members and roles under the recipient's role. The
former instance owner becomes Free when instance ownership is transferred; both
participants' previous explicit assignments are cleared.

Existing instance admins retain the built-in Administrator allocation on upgrade;
other non-owners default to Free. The legacy `set_instance_admin` command still
works and resets the target to the corresponding built-in allocation. Ordinary
assignments use `set_instance_role`. Deleting an assigned role is rejected until
its accounts have been explicitly reassigned. The migration rollback removes role
definitions/assignments while preserving guild content and legacy admin grants.

Allocation transactions lock the instance before the guild. Shared instance locks
stabilize role assignments/policies during guild writes; exclusive instance locks
serialize role configuration, ownership transfers and owned-server creation.
Guild permission/revision checks stay under the same guild lock as the write.
Quota changes do not revoke media/chat access because they only restrict future
allocation; changing administrator authority invalidates instance diagnostics access.

`list_instance_role_members` is owner-only and accepts the shared `PageRequest`
(50 accounts by default, maximum 100). It returns usernames and account IDs using
`Page`, ordered by `(created_at, id)` with an exclusive versioned keyset cursor.
It includes implicit built-in assignments and the
current instance owner. Each page rechecks authority and assignments; it is not a
frozen account snapshot. It exposes no email addresses or account credentials.

Owner deletion is blocked by foreign keys as well as friendly API validation.
Transfer instance ownership before deleting that account. A guild owner must
transfer or delete each owned guild before account deletion or leaving a guild.

## Guild rules

Each guild has exactly one owner who is also a member. Ownership transfer requires
recent reauthentication and an existing verified member. The former owner keeps
their ordinary membership and previously assigned roles, losing the owner bypass.
For now, a member with ManageInvites adds an existing verified username directly;
invite links and acceptance workflows remain separate roadmap work.

The main screen has a left server rail with initials avatars and immediate server
names on hover or keyboard focus, with accessible labels. Select a server to see
its ID and visible channels. Custom server image uploads are not implemented.
Account settings sit in the bottom ribbon spanning the rail and channel list.
Server settings sit in the channel header beside the name and copy-ID control.
Right-click menus replace the browser menu: servers offer copy ID, settings,
moderation and leave; channels offer personal pins/mutes, mark-as-read for text,
voice join/disconnect, permissions and deletion when authorized. Participant menus
offer local voice volume, role editing for lower-ranked members and the specific
actions returned by `InspectModeration`. Destructive actions require confirmation.
Menus fetch current capabilities when opened, submit the viewed guild revision,
and never automatically retry a rejected mutation. Backend checks remain authoritative.
Shift+F10 opens menus from focused items; arrows, Home/End, Enter and Escape
support keyboard use. Outside clicks, scrolling and window changes dismiss menus.
Pins/mutes persist per account in local device storage, contain no credentials and
do not sync across devices. Notification mute hides unread/mention badges; it does
not alter read state or audio. Mark-as-read uses the existing read endpoint, whose
server-side cursor clamp prevents future messages from being marked read.
Voice participant rosters require membership and ViewChannel, but not JoinVoice;
they expose no hidden channels and grant no media or signaling access.

Verified accounts see **+** to create a server in a dialog showing their allowance, with its
name and optional password. The **Join a server** icon opens a dialog for a server UUID and optional
password. Verified accounts can join unprotected servers by ID; protected servers
require the password. Existing members can reopen/join their server without
creating another membership. A removal is not a ban: someone who still knows the
join credentials can rejoin. A ban blocks both join-by-ID and direct additions until
a member with BanMembers removes it. ManageInvites permits
direct member additions without requiring the server password.

Server passwords are salted Argon2id hashes on the backend, never returned in DTOs
or logged. Empty passwords mean no protection; passwords are bounded to 1024 bytes.
Join attempts are limited to six per account per minute, including failures, and
hashing/verification runs on bounded blocking workers. Membership creation, the
owner-based member capacity check and revision increment share the guild transaction lock.
`ViewGuild` returns only channels where the current member has ViewChannel.
Changing an existing server password is not yet exposed.

Everyone is implicit on every membership, cannot be assigned/deleted, and has rank
zero. Default permissions are view, history, send, edit/delete own messages and
voice join/speak. Other roles have ranks 1–10000; equal rank grants no authority
over one another. Base permissions are the union of Everyone and all assigned roles.

Guild owners and members with Administrator receive every permission and bypass
channel overrides. Administrator does not bypass role hierarchy or ownership-only
actions. The owner's hierarchy rank is higher than every role. Other members use
the highest rank of their assigned roles, independently of permissions.

Creating a guild makes its creator the owner immediately, even with no explicit
role assignments. The editor labels that membership Owner with full administrator
access. To delegate access, save a named role with the desired permissions (including
Administrator when appropriate), select another member and assign it. Newly saved
roles are selected automatically. Everyone is implicit, not an assignable admin role.

ManageRoles permits editing/deleting roles below the actor's highest role and
creating/moving roles strictly below it. Actors can only grant permissions they
already have in their guild base permissions; editing/removing roles also checks
the old permissions. Assignment/unassignment requires a lower-ranked target member,
a lower-ranked role, and permission to grant both the role's base permissions and
its channel overrides. Self-assignment is forbidden. Everyone can be edited by an
authorized ranked member but cannot be moved above zero.

KickMembers requires a strictly lower-ranked member. Ownership transfer and guild
deletion are owner-only, even for administrators. All IDs are scoped to the guild;
composite foreign keys also prevent cross-guild assignments and overrides.

## Channel overrides

Apply the following sequence to base permissions:

1. Everyone override: remove denied permissions, then add allowed permissions.
2. All assigned-role overrides combined: remove the union of denials, then add
   the union of allowances. An allowance wins a conflict between different roles;
   role ordering and rank do not change this result.
3. Member override: remove denied permissions, then add allowed permissions.
4. Without ViewChannel, strip history, message and voice permissions. Without
   JoinVoice, strip Speak. A missing membership/channel always yields no permissions.

A single override cannot both allow and deny a permission. Administrator cannot
appear in an override. ManageRoles is required to manage overrides; the role/member
target must be below the actor. Both the old and new override permissions must be
within the actor's base grants. Guild management operations always use base
permissions; putting ManageGuild/ManageRoles in a channel override does not grant
guild administration. The evaluator is authoritative in
`backend/src/permissions/evaluator.rs`; the frontend only renders its result.

## API, consistency and scope

`POST /api/v1/permissions` uses the tagged request/response contracts in
`shared/src/permissions.rs`, bearer authentication and the standard request-ID
error envelope. Inputs are limited to 16 KiB, requests are rate-limited per account
and blocking workers are bounded. Responses are `no-store`.

Each operation authenticates the session, rechecks it under shared account/session
locks, locks the guild and loads current permissions in the same transaction as
the action. Routine authorization uses shared guild locks and focused actor/channel
inputs; full member/role lists are loaded only when management needs them. Guild
writes keep exclusive guild locks. The existing in-process access gate and
post-commit invalidation remain in place. Replay revocation commits separately.
Every guild mutation increments its revision. Editors submit the revision they viewed; a concurrent/stale write
returns 409 and must be refreshed. No client-provided role or permission claim is
trusted. Revoked roles/memberships/sessions take effect on the next HTTP operation.

For the initial personal deployment, the service caps the installation at 100
guilds, each with 500 members, 100 roles including Everyone, 200 channels and 1000 overrides. Lists
are therefore bounded rather than silently truncated; larger installations need
paginated management APIs. Only members with ManageRoles can inspect the full
editor state (including hidden-channel configuration). Members may preview their
own effective permissions; role managers may preview other members.

Open the account ribbon’s **Settings > Instance access** for installation-wide administration. Open the server-header gear for server settings. Overview, Roles, Members, Channels & permissions, Moderation, and Ownership & deletion are separate sections in the server modal. Only sections allowed by the current server snapshot are shown; the backend rechecks every action.
Choose **Server settings** on a guild, select a member and role, edit permissions or
assign/unassign roles. Channel overrides have separate allow/deny checkboxes and a
**Load saved override** action. Preview reports saved effective permissions, not
unsaved form changes. Destructive actions require typing the guild name; ownership
transfer also requires recent reauthentication through the account navigation.

Opening settings from a server selects that server automatically. Owners see a
Delete server form at the bottom, with its own server-name confirmation and submit
button. Deletion requires recent authentication and removes channels and messages.
Owners cannot leave without transferring ownership; their Leave server button is
disabled with an explanatory hover tooltip. Other members can confirm Leave server.
The server list refreshes after a successful leave or deletion.

Chat HTTP operations and socket deliveries now use current database permissions.
Membership/role changes invalidate sockets subscribed to the affected guild; session
changes invalidate that account's sockets. Scoped delivery barriers preserve commit
ordering without blocking unrelated accounts/guilds; eligible clients reconnect.
See [chat.md](chat.md). Do not treat client snapshots as reusable authorization tickets.
Voice signaling checks current sessions, membership, ViewChannel and JoinVoice.
SFU routing also enforces Speak and self mute/deafen. Publisher/receiver writes
use separate atomic media permits and check their generation; access-changing
workers drain admitted media writes and commits pause media until sessions revalidate; revoked sessions are evicted. See [audio.md](audio.md). Moderator voice
disconnects now enforce the target hierarchy; mute/deafen/move remain pending.

EditOwnMessages/DeleteOwnMessages additionally require message authorship;
ManageMessages permits moderation. MoveMembers will require access to both source
and destination channels. Mute/deafen/move/kick/ban must enforce target hierarchy.
These object-specific rules must be tested when those handlers are introduced.

## Storage and tests

Migration `20260927020000_permissions` adds instance ownership/admins, guilds,
memberships, roles, assignments, channels and overrides. Rollback removes all those
records and privileges while preserving accounts. It is not a reset procedure.
Migration `20260927030000_guild_passwords` adds optional password protection;
existing servers remain unprotected. Its rollback removes that protection.

Run backend/shared tests in WSL with `--include-ignored`; permission integration
tests only create/drop random schemas in `TEST_DATABASE_URL`. Coverage includes
bootstrap races, ownership/deletion protection, multiple roles, overrides,
cross-guild IDs, hierarchy/escalation attempts, stale grants and concurrent writes.
The existing migration test verifies rollback/reapply. WASM checks cover the editor;
native platform runtime acceptance remains a separate check.

Screen and system-audio publishing currently require `Speak` in addition to
`ViewChannel`/`JoinVoice`. Microphone mute does not mute an explicitly active
screen share. Deafen stops publishing and blocks reception. See
[screen-sharing.md](screen-sharing.md) for the media state and revocation rules.

## Server moderation

Select a server and open **Moderation**, or choose **Moderate** beside a voice
participant. This panel is available to members with KickMembers, BanMembers,
ModerateMembers or MoveMembers; ManageRoles is not required. It returns a bounded
member list and per-target allowed actions without exposing hidden channels or
role configuration. Bans are visible only to BanMembers holders (maximum 1000
active bans per guild). Confirmations identify the target and consequences.

- **Kick from server** requires KickMembers. It removes membership and assigned
  roles; the user may rejoin with the server's credentials.
- **Ban / Unban** requires BanMembers. Banning removes membership/roles and blocks
  both join paths. Unbanning permits joining but does not restore membership or
  roles. Previous messages remain. Bans are scoped to one server.
- **Timeout / Remove timeout** requires ModerateMembers. The API accepts 1 second
  through 28 days; the UI offers 1 minute, 10 minutes, 1 hour, 1 day, 1 week and
  28 days. A null duration clears it. Timeouts retain only ViewChannel/ReadHistory
  from the member's channel grants: sending, edits/deletes, typing, moderation,
  voice and screen sharing are blocked. Overrides and Administrator cannot bypass
  an active timeout; the owner is protected. The restriction survives leaving and
  rejoining. It expires by UTC time without requiring a scheduled database write.
- **Disconnect from voice** requires guild-base MoveMembers. It ends all current
  voice/screen connections for that member in this guild, without banning future
  explicit joins. This is a guild-wide disconnect, not a move to another channel.

Kick, ban, timeout (including clearing) and disconnect require a strictly lower
ranked target member, never the actor or owner. Instance administrators receive no
implicit access. Unban operates on a ban record after membership has been removed.
All writes recheck current grants and the submitted guild revision under the guild
transaction lock. Stale requests return 409; the panel reloads for review without
retrying the action automatically. Revoking a moderator's role takes effect on the
next request, including from a previously opened panel.

The moderation migration stores restrictions independently of membership, with
account/guild deletion cascades. Its rollback permanently removes restrictions.
A persistent per-member voice revision makes existing transports fail revalidation
following a disconnect, kick, ban or applied timeout, even if access is restored
before revalidation. Other users revalidate and continue. Media admission
is paused/drained through commit, preventing packets in either direction from
using stale grants. The existing terminal `revoked` event stops native automatic
reconnection. Chat sockets are invalidated after commits; eligible readers reconnect.
On timeout expiry, the ten-second session maintenance check refreshes composer
permissions through reconnection (or selecting the channel reloads them immediately).

Tests cover DTO compatibility, hierarchy/self/owner/foreign targets, delegated
moderators without ManageRoles, stale requests, timeout bounds/expiry/removal and
rejoin persistence, chat restrictions, both banned join paths, unban, socket expiry,
and live voice/screen removal with unaffected participants and explicit rejoining.
Three-platform interactive moderation acceptance remains a manual check.
