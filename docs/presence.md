# Member presence

`GET /api/v1/spaces/{space_id}/presences` returns a complete member-scoped
`{"data": [...]}` presence snapshot. Authentication and space membership are
required; guest tokens can query only their scoped space. Missing sessions are
represented by explicit `offline` records. Other users' Invisible status,
client-status metadata, and custom activities are masked. Only the local
user's own snapshot and gateway sessions retain their private Invisible choice.

READY uses the same privacy rules. Joining a space while connected sends
`presence.update` records for existing members and announces the joining user's
current public status. Clients can also fetch the REST snapshot on opening or
refreshing a roster; messages alone do not imply visible online status.

## Federation

Mutually trusted user-home servers exchange ephemeral presence using signed
`POST /federation/v1/presence` requests. This endpoint uses the existing Ed25519
request signature, replay protection, trusted-peer verification, URL/DNS
validation, and peer rate limit. Presence never enters the durable content
outbox or changes user profiles/membership.

Request and response bodies use:

```json
{
  "observed_at": 1791504000000,
  "presences": [
    {
      "user_id": "123@a.example",
      "status": "online",
      "client_status": {"desktop": "online", "mobile": null, "web": null},
      "activities": [],
      "space_id": null
    }
  ],
  "after": null
}
```

`observed_at` is the originating server's UTC timestamp in milliseconds. Each
record must be homed on the authenticated signing peer and already belong to a
space on the receiving server. The full batch is validated before application;
unknown users, local-user spoofing, nonmembers, invalid statuses, duplicate IDs,
and timestamps more than five seconds ahead are rejected. Old/out-of-order or
expired updates cannot restore a superseded online status. Keep server clocks
synchronized.

Only local authoritative accounts sharing a space with the peer are returned.
Sharing includes a mirrored space homed on that peer and mirrored aliases of
this server's own accounts. Invisible is always sent as Offline to peers, with
activities and client-status metadata removed.

Batches contain at most 500 records. A non-null response `after` is a qualified
user-ID cursor: send it in the next request to read the remaining snapshot.
Outbound updates are also split into batches. Cursors are checked for progress.

Status changes trigger a coalesced exchange after one second. A periodic
exchange every 15 seconds supplies initial state and renews leases. Remote
presence expires after 90 seconds without renewal; an independent five-second
expiry loop emits a local offline update. Expiry does not wait for slow peers.
Requests use a three-second deadline and at most four concurrent peer exchanges.
READY and REST snapshots can refresh peer state, with concurrent refreshes
coalesced. A peer without this endpoint remains compatible with content
federation, but cannot supply remote presence.

Deploy this server update on the participating trusted user-home servers and
the client roster update to receive the complete behavior. No database migration
is required; all remote presence and leases are held in memory.

Validation covers signed initial snapshots, membership/authority/privacy,
expiry, stale deliveries, pagination beyond 500 members, READY, mid-session
joins, and private self status across live gateway updates.
