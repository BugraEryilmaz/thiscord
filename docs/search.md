# Message search

Use Search messages in a guild channel header, the friends sidebar, or a private
conversation. Guild search spans the current guild's authorized text channels;
choose a channel to narrow it. Private search spans accepted conversations, or
only the open conversation. There is no cross-instance search.

Search matches whole words with PostgreSQL's `simple` full-text dictionary (all
query words must match), case-insensitively and without language stemming. Enter
1–200 characters. Filters accept an exact @username and UTC dates: From is
inclusive, Before is exclusive. Results are newest first with stable timestamp/UUID
keyset pagination. One page contains at most 100 messages (default 50); the UI
replaces pages rather than accumulating history. View original and context loads
up to 20 earlier and 20 later messages, highlights the original and rechecks access.
Open channel jumps to a guild original; private Open channel opens the conversation,
while the context view provides the exact private original and nearby history.

`POST /api/v1/search` uses `shared/src/search.rs`; the backend module and frontend
component are independent of the message composer. Results never include tombstones,
request hashes or internal models. Context requests cannot turn a message UUID into
access. Guild requests retain session/guild locks and apply authoritative membership,
ViewChannel and ReadHistory checks before selecting any message. Private requests
lock accepted friendship rows before reading history. Cursors never grant access.
No-store responses, four-worker admission, per-account throttling, query/lock deadlines,
200-character query limits and result caps bound work. Private broad searches lock at
most 501 relationships and require narrowing if more than 500 match. GIN indexes
support content lookup; no attachment bytes enter search results.

## Private access and deletion

Every selected conversation passes `friends::store::pair_access` inside the
retained session transaction. This shares the history/download rules for accepted
friendship and blocks in either direction. Queries use the typed `NOT m.deleted`
predicate. Blocking removes search and context access for both people; unblocking
alone does not restore consent. A new accepted request restores retained history,
while deleted messages remain excluded.

Combined database regressions cover block/unblock, pending and restored consent,
unrelated conversations, deletion, stranger isolation, removal, and a context
request waiting behind a removal commit. Bot credentials cannot access search or
private safety endpoints.

Guild rich-chat migration rollback removes both search indexes. Existing content
remains; reapply restores indexes. Search does not claim scale measurements for
large installations or native browser runtime acceptance on three platforms.
