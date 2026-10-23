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

## P1 integration dependency

This isolated branch starts at feaae9a, before private edit/delete/blocking. Its
private access seam in `backend/src/search.rs` must be replaced with
`friends::store::pair_access(c, id, actor, false)` for every selected conversation,
inside the existing transaction. Replace the temporary JSON-based deleted predicate
with the final typed `NOT m.deleted` column. The sibling's helper checks both block
directions; unblocking does not restore friendship consent. Final acceptance must
exercise block, unblock, removal, deleted messages, and concurrent revocation for
both search and context against the combined migrations. The baseline branch tests
accepted consent, stranger isolation, deletion filtering with an isolated test column,
removal, and waiting behind a removal commit;
it does not claim combined DM safety acceptance.

Guild rich-chat migration rollback removes both search indexes. Existing content
remains; reapply restores indexes. Search does not claim scale measurements for
large installations or native browser runtime acceptance on three platforms.
