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
Admins and the owner may create guilds; regular users join existing guilds.
Instance administration **does not grant membership or guild permissions**. Future
deployment-wide moderation, limits and configuration need explicit instance checks.

Owner deletion is blocked by foreign keys as well as friendly API validation.
Transfer instance ownership before deleting that account. A guild owner must
transfer or delete each owned guild before account deletion or leaving a guild.

## Guild rules

Each guild has exactly one owner who is also a member. Ownership transfer requires
recent reauthentication and an existing verified member. The former owner keeps
their ordinary membership and previously assigned roles, losing the owner bypass.
For now, a member with ManageInvites adds an existing verified username directly;
invite links, bans and acceptance workflows remain separate roadmap work.

The main screen has a left server rail with circular initials avatars and server
names on hover and accessible labels. Select a server to see its ID and visible channels. Custom
server image uploads are not implemented. Account settings remain accessible at
the bottom of the rail.

Verified instance owners/admins see **+** to create a server in a dialog with its
name and optional password. **Join** opens a dialog for a server UUID and optional
password. Verified accounts can join unprotected servers by ID; protected servers
require the password. Existing members can reopen/join their server without
creating another membership. A removal is not a ban: someone who still knows the
join credentials can rejoin until ban support is implemented. ManageInvites permits
direct member additions without requiring the server password.

Server passwords are salted Argon2id hashes on the backend, never returned in DTOs
or logged. Empty passwords mean no protection; passwords are bounded to 1024 bytes.
Join attempts are limited to six per account per minute, including failures, and
hashing/verification runs on bounded blocking workers. Membership creation, the
500-member capacity check and revision increment share the guild transaction lock.
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

Each operation authenticates the session, rechecks it under its account/session
lock, locks the guild and loads current permissions in the same transaction as
the action. Replay revocation commits separately. Every guild mutation increments
its revision. Editors submit the revision they viewed; a concurrent/stale write
returns 409 and must be refreshed. No client-provided role or permission claim is
trusted. Revoked roles/memberships/sessions take effect on the next HTTP operation.

For the initial personal deployment, the service caps the installation at 100
guilds, each with 500 members, 100 roles including Everyone, 200 channels and 1000 overrides. Lists
are therefore bounded rather than silently truncated; larger installations need
paginated management APIs. Only members with ManageRoles can inspect the full
editor state (including hidden-channel configuration). Members may preview their
own effective permissions; role managers may preview other members.

Sign in and open **Guilds & roles**. **Instance access** loads the instance controls.
Choose **Manage roles** on a guild, select a member and role, edit permissions or
assign/unassign roles. Channel overrides have separate allow/deny checkboxes and a
**Load saved override** action. Preview reports saved effective permissions, not
unsaved form changes. Destructive actions require typing the guild name; ownership
transfer also requires recent reauthentication through the account navigation.

Permission identifiers cover chat/history, moderation and voice, but no chat,
WebSocket, signaling or SFU endpoints exist yet. Their operation-specific checks,
subscription filtering and immediate socket/voice eviction are **not implemented**.
Do not treat an effective-permissions snapshot as a reusable authorization ticket.
Future transports must recheck authorization for every action and invalidate
active subscriptions/media on role, membership, override, session and account
changes, including deletion. Shared event envelopes and media lifecycle work remain
in their respective roadmap sections. There is no pretend in-memory eviction layer.

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
