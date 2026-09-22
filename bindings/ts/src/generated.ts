export interface paths {
    "/attestations/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** `GET /attestations/{id}` (#33) — public, unauthenticated. */
        get: operations["get_attestation"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/attestations/{id}/revoke": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /attestations/{id}/revoke` (#85). Only the attestation's original
         *     issuer may revoke it — authenticated via the same challenge-response
         *     scheme every issuer-credentialed endpoint uses, plus (like issuance) an
         *     independently-checked embedded signature over
         *     [`revocation_signing_bytes`], so the revocation record itself carries
         *     cryptographic proof of who authorized it, not just an HTTP-layer claim.
         */
        post: operations["revoke_attestation"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/cross-node/deny": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /auth/cross-node/deny` — deliberately unauthenticated, unlike
         *     `device_pairing::deny_pairing`. That module's approver always already
         *     holds a session *on the same node* the pairing was started on, so
         *     requiring it there is free; here the approver's session (if any) lives
         *     wherever the identity's signing key does, which is routinely a
         *     *different* node than the one this request was started on — requiring
         *     local auth would make denial impossible for the exact case cross-node
         *     login exists to handle. Safe to leave unauthenticated regardless: a
         *     denial grants nothing, so the worst case of a guessed `user_code` (8
         *     chars from a 32-symbol alphabet) is griefing one's own pending request,
         *     not a security bypass.
         */
        post: operations["deny"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/cross-node/lookup": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /auth/cross-node/lookup?user_code=...` — unauthenticated, epic
         *     #623 issue #639's own gap: the Hub/mobile-hub approval screen has to
         *     show real context (#642's decided phishing-context requirement)
         *     *before* a human decides whether to approve, but `submit`/`deny` only
         *     ever take a `user_code` with no read path to go with it. Deliberately
         *     returns nothing beyond what's needed to render the prompt — never
         *     `request_code` (the polling device's own bearer credential, not the
         *     approver's business).
         */
        get: operations["lookup"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/cross-node/poll": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /auth/cross-node/poll` — unauthenticated; the bearer token here is
         *     the opaque `request_code`, not a session. Same single-use-on-approved
         *     shape as `device_pairing::poll_pairing`.
         */
        post: operations["poll"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/cross-node/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /auth/cross-node/start` — unauthenticated, called on the
         *     requesting node. Mints an opaque `request_code` (known only to this
         *     client and this server) and a short human-typeable `user_code` (shown
         *     as a QR code / typed on the approving device), and stores a pending row
         *     binding this node's own `base_url` into what the approver will
         *     eventually sign over.
         */
        post: operations["start"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/cross-node/submit": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /auth/cross-node/submit` — unauthenticated (the grant itself is
         *     the credential). Mints an ordinary session, same mechanism
         *     `device_pairing::approve_pairing` uses. With `user_code` present,
         *     attaches the session to that pending request for the waiting client to
         *     pick up via `poll`; with `user_code` absent (same-device fast path),
         *     returns the session token directly.
         */
        post: operations["submit"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/device/approve": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /auth/device/approve` — requires the approver's own existing
         *     authenticated session (normal session-bearer auth). Mints a real session
         *     for the approver's own identity via `auth::generate_session_token`/the
         *     `sessions` table — the exact same mechanism `handlers::session_finish`
         *     uses for a normal login — and attaches it to the pairing so the waiting
         *     client picks it up on its next poll.
         */
        post: operations["approve_pairing"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/device/deny": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /auth/device/deny` — same auth shape as [`approve_pairing`], the
         *     explicit rejection path. No session is ever minted.
         */
        post: operations["deny_pairing"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/device/poll": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /auth/device/poll` — unauthenticated; the bearer token here is the
         *     opaque `device_code` itself, not a session. Single-use on `approved`:
         *     the winning poll atomically flips the row to `expired` in the same
         *     `UPDATE ... RETURNING` that reads the session, so a concurrent or later
         *     poll of the same `device_code` can never observe the token twice.
         */
        post: operations["poll_pairing"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/auth/device/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /auth/device/start` — unauthenticated. The waiting client's
         *     entrypoint: mints an opaque `device_code` (known only to this client and
         *     the server, never shown to the user) and a short human-typeable
         *     `user_code` (shown to the user, e.g. as a QR code), and stores a
         *     pending pairing row. Retries on a code collision — with 32^8 possible
         *     `user_code`s this only ever matters once a huge number are pending at
         *     once.
         */
        post: operations["start_pairing"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/blocks": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /blocks` — the caller's own block list, and *only* the caller's own
         *     — this endpoint (and every endpoint in this crate) never returns "who
         *     has blocked me" for any identity, per this module's own invariant.
         */
        get: operations["list_blocks"];
        put?: never;
        /**
         * `POST /blocks` — session-authenticated, blocker-only. Also resolves any
         *     pending friend request between the two, in either direction, as part of
         *     the same transaction — the ticket's own acceptance criteria requires a
         *     pending request be auto-rejected the moment a block is created. Reuses
         *     `friend_requests`' existing `'withdrawn'` outcome rather than adding a
         *     new one: it's a projection update, not durable history, the same
         *     treatment `friends::decline_or_withdraw_friend_request` already gives a
         *     resolved request, and a fourth precise outcome label isn't worth a
         *     schema change for what's an internal bookkeeping value never surfaced
         *     to either party as "resolved because of a block."
         */
        post: operations["create_block"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/blocks/{identity_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * `DELETE /blocks/:identity_id` — session-authenticated, blocker-only.
         *     Simply removes the row: no history, no event, mirrors presence's
         *     ephemerality rather than friendship's durability (module docs).
         */
        delete: operations["remove_block"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/conversations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /conversations` — the caller's own conversation list. A
         *     conversation with a block anywhere in its participant set is left out —
         *     see the module doc comment's note on why, and
         *     [`require_unblocked_participant`] for the same rule applied to a single
         *     conversation.
         */
        get: operations["list_my_conversations"];
        put?: never;
        /**
         * `POST /conversations` — session-authenticated. The caller is always
         *     added to the participant set, then deduplicated; rejects fewer than two
         *     distinct identities or any participant the caller isn't related to (see
         *     the module doc comment's "Relationship gate" section). Idempotent on the
         *     final participant set — see "Idempotent creation" above.
         */
        post: operations["create_conversation"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/conversations/{id}/messages": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /conversations/{id}/messages?before=&limit=` — newest first,
         *     cursor-paginated. Requires current participation and no block among the
         *     conversation's participants — same gate [`send_message`] uses, so a
         *     blocked participant's read fails exactly as their write does. See the
         *     module doc comment's "Blocking, enforced on both read and write"
         *     section.
         */
        get: operations["chat_list_messages"];
        put?: never;
        /**
         * `POST /conversations/{id}/messages` — requires current participation,
         *     and — unlike `guild_messages::send_message` — an additional group-wide
         *     block check. See the module doc comment's "Blocking, enforced on both
         *     read and write" section for why the rejection is indistinguishable from
         *     a non-participant's, on both this endpoint and [`list_messages`].
         * @description **Idempotent when `client_entry_id` is set** (issue #111): inserts with
         *     `ON CONFLICT (conversation_id, client_entry_id) DO NOTHING` against the
         *     partial unique index from migration `0037_conversation_message_idempotency`
         *     and, if that hit an existing row instead of inserting a new one, looks
         *     the existing row up and returns it — the exact same "the unique
         *     constraint is what actually prevents duplicates, the query just
         *     discovers which case it's in" shape [`create_conversation`] already uses
         *     for `participants_key`. This is what lets the SDK's deferred submission
         *     engine retry a submission whose response was dropped without ever
         *     double-applying it.
         */
        post: operations["chat_send_message"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/friends": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_friends"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/friends/handle/{handle}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Resolves a `display_name` handle (issue #128, updated by #510 to a
         *     globally-unique, case-insensitive `display_name` — no discriminator) to
         *     an identity id for the "add friend" flow — exact match only, never
         *     partial/fuzzy. Fuzzy name search is a separate, bigger question (issue
         *     #129) with its own privacy tradeoffs, deliberately not folded in here.
         *     Session-authenticated like every other route in this module, both so an
         *     anonymous caller can't use it to enumerate handles and so it matches
         *     this module's existing "no integrator-credential auth path" convention.
         */
        get: operations["resolve_handle"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/friends/requests": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_friend_requests"];
        put?: never;
        post: operations["create_friend_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/friends/requests/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["decline_or_withdraw_friend_request"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/friends/requests/{id}/accept": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["accept_friend_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/friends/{identity_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["remove_friend"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["create_guild"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/discover": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["discover_guilds"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["get_guild"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch: operations["update_guild"];
        trace?: never;
    };
    "/guilds/{id}/channels": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/channels` — a member sees every channel they hold
         *     `view` on (baseline: all of them, unless a role override says
         *     otherwise — issue #458). A non-member of a
         *     [`crate::guilds::GuildResponse::public`] guild sees only `public`
         *     channels instead of being 403'd outright — same shape
         *     `guild_events::list_events` already established for events (#448),
         *     extended to channels here since they had no non-member visibility
         *     concept before this ticket. A non-member of a non-public guild is
         *     still 403'd, unchanged. Lists both active and archived channels; the
         *     client distinguishes via `archived`.
         */
        get: operations["list_channels"];
        put?: never;
        /** `POST /guilds/{id}/channels` — requires `manage_channels`. */
        post: operations["create_channel"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/channels/{cid}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /**
         * `PATCH /guilds/{id}/channels/{cid}` — rename, retopic, and/or toggle
         *     announcement-only/public. Requires `manage_channels` (resource-aware,
         *     issue #250). Renaming/retoggling an archived channel is allowed (it's
         *     still the same durable channel, just not accepting new posts).
         */
        patch: operations["update_channel"];
        trace?: never;
    };
    "/guilds/{id}/channels/{cid}/archive": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /guilds/{id}/channels/{cid}/archive` — requires `manage_channels`
         *     (resource-aware, issue #250). A soft flag (`archived_at`), not a
         *     delete: history and past messages stay reachable, the channel simply
         *     stops accepting new posts (enforced in
         *     `crate::guild_messages::send_message`).
         */
        post: operations["archive_channel"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/channels/{cid}/messages": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/channels/{cid}/messages?before=&limit=` — newest
         *     first, cursor-paginated. Requires `view_details` on this channel
         *     (issue #458) — baseline for a member is exactly the old plain
         *     membership gate (unchanged for a channel with no overrides), and a
         *     non-member of a `public` channel in a public guild can now read it
         *     too, same "public flag widens exposure" shape events already have.
         *     Works for archived channels too (history stays readable — only
         *     posting stops).
         */
        get: operations["guilds_list_messages"];
        put?: never;
        /**
         * `POST /guilds/{id}/channels/{cid}/messages` — requires current guild
         *     membership; rejected if the channel is archived. No transaction, no
         *     outbox — see module doc comment.
         */
        post: operations["guilds_send_message"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/channels/{cid}/messages/archive": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/channels/{cid}/messages/archive?before=&limit=` — same
         *     newest-first, cursor-paginated shape as [`list_messages`], over
         *     `guild_messages_archive` instead of the live table. Requires
         *     *current* `view_details` on the channel (issue #458), same gate
         *     [`list_messages`] uses — see the module doc comment's "Archive read
         *     access" section for why this doesn't try to reconstruct membership as
         *     of when each message was originally sent.
         */
        get: operations["list_archive"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/channels/{cid}/messages/{mid}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * `DELETE /guilds/{id}/channels/{cid}/messages/{mid}` — moderation, requires
         *     `manage_channels`. A real hard delete, and one that reaches both tiers
         *     on purpose: it first tries the live `guild_messages` row, and if that
         *     finds nothing, falls back to `guild_messages_archive` — see the module
         *     doc comment's "Moderation deletion and the archive" section for why a
         *     moderator's takedown shouldn't be defeated just because cap-based
         *     pruning already moved the row into the archive.
         */
        delete: operations["delete_message"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/events": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/events?from=&to=` — current members see every event.
         *     A non-member of a [`crate::guilds::GuildResponse::public`] guild (issue
         *     #448) sees only `public` events instead of being 403'd outright — the
         *     same "guild-level flag widens exposure of an otherwise-gated resource"
         *     shape `list_members`'s roster override already established for #449,
         *     scoped per-event here since (unlike a roster) some events genuinely
         *     need to stay internal even in a public guild. A non-member of a
         *     non-public guild is still 403'd, unchanged. Optionally filtered to a
         *     `starts_at` date range; omitted bounds are unbounded.
         */
        get: operations["list_events"];
        put?: never;
        /**
         * `POST /guilds/{id}/events` — requires `event_manage`. No outbox
         *     write — see module doc comment.
         */
        post: operations["create_event"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/events/{eid}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * `DELETE /guilds/{id}/events/{eid}` — requires `event_manage` (resource-
         *     aware, issue #250). A real hard delete: events aren't history (see
         *     module doc comment). Removes its RSVPs too, via the `ON DELETE CASCADE`
         *     FK on `guild_event_rsvps` (migration 0028) — nothing app-level to do
         *     here beyond deleting the event row itself.
         */
        delete: operations["delete_event"];
        options?: never;
        head?: never;
        /**
         * `PATCH /guilds/{id}/events/{eid}` — reschedule/edit. Requires
         *     `event_manage` (resource-aware, issue #250). Full replace of the mutable fields, same "resend the
         *     whole thing" convention other guild PATCH endpoints use.
         */
        patch: operations["update_event"];
        trace?: never;
    };
    "/guilds/{id}/events/{eid}/rsvp": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * `PUT /guilds/{id}/events/{eid}/rsvp` — requires current guild
         *     membership. Self-service only: always upserts the caller's own row,
         *     there is no way to target another identity's RSVP through this route.
         *     Idempotent per (event, identity): a second call with a new status
         *     replaces the row in place, never inserts a duplicate — enforced by the
         *     `(event_id, identity_id)` primary key from migration 0028 plus
         *     `ON CONFLICT DO UPDATE` below.
         */
        put: operations["upsert_rsvp"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/events/{eid}/rsvps": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/events/{eid}/rsvps` — any current guild member.
         *     Returns every `guild_event_rsvps` row for the event (`identity_id`,
         *     `status`, `responded_at`), unaggregated — the per-member roster behind
         *     `rsvp_counts`. Same membership gate as `list_events`/`rsvp_counts`'s
         *     query, no `manage_*` permission required: RSVP status is ordinary
         *     guild-internal social info, same posture the member roster already
         *     takes (see module doc comment).
         */
        get: operations["list_rsvps"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/favorite-integrators": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/favorite-integrators` (issue #207). Same "any authenticated
         *     identity may read a guild's public metadata" visibility as `GET
         *     /guilds/{id}` itself (see that handler's doc comment) — the favorites
         *     list is exactly the curated subset of affinity data a guild has chosen
         *     to put on public display, so it carries no additional gate beyond
         *     session authentication.
         */
        get: operations["list_favorite_games"];
        /**
         * `PUT /guilds/{id}/favorite-integrators` (issue #207). Gated by the same
         *     `manage_guild`/owner permission as #206's breakdown-visibility toggle
         *     (via [`has_guild_permission`]) — reuses that check rather than inventing
         *     a new one, per the ticket. Validates every id against the guild's real,
         *     current affinity (see [`validate_favorite_game_ids`]) before writing
         *     anything; on success, replaces the stored list atomically (delete +
         *     reinsert, same "small enough this doesn't need per-row diffing" call
         *     `update_guild`'s `links` replace already makes) and records a
         *     `guild.favorite_games_updated` outbox event, matching every other guild
         *     mutation in this module.
         */
        put: operations["set_favorite_games"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/integrations/{integrator_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["associate_integrator"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/integrator-breakdown": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/integrator-breakdown` (issue #206, implementing decision
         *     #160). Milestone-1 stand-in: a direct query over `guild_members` JOIN
         *     `bindings` JOIN `integrators`, same precedent [`discover_guilds`] (#154)
         *     already set, not #42's real indexer read model. Derived/computed on
         *     every read — no protocol event, no durable table backs this (see the
         *     module doc comment).
         * @description Gated by [`can_view_game_breakdown`]: a `manage_guild` holder (or the
         *     owner) can always see it; anyone else only when the guild has set
         *     `game_breakdown_public`. A non-member with neither gets
         *     [`AppError::MissingGuildPermission`], same 403 the rest of this module
         *     already uses for "authenticated fine, just not authorized here".
         */
        get: operations["game_breakdown"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/invites": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["create_invite"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/invites/{invite_id}/accept": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["accept_invite"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/invites/{invite_id}/decline": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["decline_invite"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/join": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["join_guild"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/join-requests": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/join-requests` — `manage_members`-gated. Lists
         *     pending requests by default (`?status=all` for every status).
         */
        get: operations["list_join_requests"];
        put?: never;
        /**
         * `POST /guilds/{id}/join-requests` — any authenticated identity not
         *     already a member may apply to a `recruiting` guild. Applying to a
         *     non-recruiting guild is rejected, same gating #154's own discovery board
         *     applies to strangers browsing it. A second apply while one is already
         *     pending is idempotent (returns the existing pending row) rather than an
         *     error or a duplicate, same posture [`create_invite`] takes for a
         *     duplicate invite.
         */
        post: operations["create_join_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/join-requests/mine": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/join-requests/mine` — issue #256. Any authenticated
         *     caller, no `manage_members` gate: this is the caller's own data, not a
         *     moderation view, unlike [`list_join_requests`]. Returns the caller's own
         *     pending join request for this guild if one exists, or `null` if it
         *     doesn't — same `Json<Option<T>>` "single item belonging to the caller,
         *     or none" shape `recovery::my_recovery_status` already established,
         *     rather than a 404 for the "none" case.
         */
        get: operations["my_join_request"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/join-requests/{request_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * `DELETE /guilds/{id}/join-requests/{request_id}` — the applicant
         *     withdrawing their own pending request only; unlike approve/reject this
         *     is not `manage_members`-gated, same "consent from the other side" shape
         *     as `decline_invite`, just from the opposite party.
         */
        delete: operations["withdraw_join_request"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/join-requests/{request_id}/approve": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /guilds/{id}/join-requests/{request_id}/approve` —
         *     `manage_members`-gated. Adds the applicant as a member (base role)
         *     through [`add_member`] — the same membership-add path [`accept_invite`]
         *     and [`join_guild`] already use, not a second one — and marks the
         *     request approved.
         */
        post: operations["approve_join_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/join-requests/{request_id}/reject": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /guilds/{id}/join-requests/{request_id}/reject` —
         *     `manage_members`-gated. Not durable history — see this section's module
         *     doc comment.
         */
        post: operations["reject_join_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/leave": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["leave_guild"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/members": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_members"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/members/{identity_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["remove_member"];
        options?: never;
        head?: never;
        patch: operations["update_member_role"];
        trace?: never;
    };
    "/guilds/{id}/permission-overrides": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /guilds/{id}/permission-overrides?resource_kind=&resource_id=` —
         *     every role's override rows for one resource. Requires `manage_roles`.
         */
        get: operations["list_permission_overrides"];
        /**
         * `PUT /guilds/{id}/permission-overrides` — set (upsert) a grant/deny
         *     override for one role on one resource. Requires `manage_roles`.
         */
        put: operations["set_permission_override"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/permission-overrides/{override_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["delete_permission_override"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/roles": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_roles"];
        put?: never;
        post: operations["create_role"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/guilds/{id}/roles/{idx}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete: operations["delete_role"];
        options?: never;
        head?: never;
        patch: operations["update_role"];
        trace?: never;
    };
    "/guilds/{id}/transfer-ownership": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["transfer_ownership"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/identities/profiles": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /identities/profiles?ids=…` — issue #161. Closes the gap every
         *     roster-shaped surface built so far (`friends::list_friends`,
         *     `guilds::list_members`, and their SDK/Hub consumers) has had to leave as
         *     a raw identity id: there was never an endpoint that resolved *another*
         *     identity's display name. Session-authenticated only, no further
         *     visibility gating — display name and avatar are already the
         *     least-sensitive public-face fields, same exposure level
         *     `friends::resolve_handle` already has. Unknown ids are silently omitted
         *     rather than erroring, so one bad id in a roster doesn't 500 the whole
         *     batch.
         */
        get: operations["list_profiles"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/identities/register/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["identity_register_finish"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/identities/register/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["identity_register_start"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/identities/search": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /identities/search?q=&limit=` (issue #205) — session-authenticated
         *     open name/handle search, the opt-in counterpart to [`discover_people`]'s
         *     always-on scoped surfacing. Matches only identities with
         *     `discoverable = true` (see module doc comment); a non-opted-in identity
         *     never appears here, full stop — not even to a caller who already knows
         *     their exact handle (that's the separate, untouched
         *     `friends::resolve_handle` exact-match path). Excludes the caller
         *     themselves and any blocked relationship in either direction, same as
         *     [`discover_people`].
         */
        get: operations["search_identities"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/identities/{id}/integrator-data": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /identities/{id}/integrator-data` (#384) — public, unauthenticated (see
         *     module doc comment). Every current (non-superseded) instance published
         *     about `id`, across every integrator/schema, each filtered to only the fields
         *     its schema currently makes visible. A schema whose visibility metadata
         *     isn't found in the indexer's projection (should not happen for any
         *     instance the same projection itself produced) is skipped defensively
         *     rather than ever guessing a default — see the loop below.
         */
        get: operations["get_identity_integrator_data"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/identities/{id}/locations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /identities/{id}/locations` — deliberately unauthenticated: this
         *     runs *before* cross-node login can complete (a requesting node resolving
         *     where to even ask), so there is routinely no session to require yet, and
         *     the response (a set of server base URLs) carries no personal data —
         *     same public-discovery posture `crate::nodes`'s peer-listing routes
         *     already take.
         */
        get: operations["get_locations"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/identities/{id}/profile": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /identities/{id}/profile` — issue #403. Session-authenticated, no
         *     further visibility gating (same posture as `list_profiles`): every field
         *     here is already unauthenticated-readable on the viewed identity's own
         *     `GET /me`, so a single-identity read of the same fields adds no new
         *     exposure, only a more convenient shape than "batch-resolve one id."
         */
        get: operations["get_identity_profile"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/identities/{id}/recovery/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /identities/:id/recovery/status` — same public data as
         *     [`get_request`], but keyed by identity rather than request id, for a
         *     caller (the real owner, glancing at their own profile from a device
         *     that still has *some* access, or literally anyone else per the "public"
         *     requirement above) who doesn't already know a request id. Reports "no
         *     active recovery" rather than searching historical/cancelled ones — the
         *     at-most-one-active-request index means there is at most one row to
         *     find.
         */
        get: operations["identity_recovery_status"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /integrations?q=&sort=&limit=&cursor=` (issue #270). Public, unauthenticated
         *     — same visibility level [`get_integrator`] already uses. See the module doc
         *     comment for the pagination/sort design.
         */
        get: operations["list_integrators"];
        put?: never;
        post: operations["register_integrator"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/whoami": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Exists only to prove [`authenticate_integrator`] works end to end over real
         *     HTTP (this ticket's own suggestion) — not a real capability-bearing
         *     endpoint; #27 owns those.
         */
        get: operations["integrator_whoami"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * A public read of an integrator's registration — no credential fields, unlike
         *     [`IntegratorResponse`] (which only `register_integrator` itself ever returns, to the
         *     registrant, once). This is what the Hub's consent view (#27) and
         *     `connections.rs`'s `POST /integrations/{slug}/connect` (to validate approved
         *     capabilities against what the integrator actually declared) both read; same
         *     visibility level `crates/server/src/guilds.rs`'s `get_guild` uses — no
         *     auth required, nothing here is sensitive.
         */
        get: operations["get_integrator"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/achievements": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** `GET /integrations/{slug}/achievements`. */
        get: operations["list_achievement_definitions"];
        put?: never;
        /** `POST /integrations/{slug}/achievements`. */
        post: operations["create_achievement_definition"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/achievements/bulk-issue": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** `POST /integrations/{slug}/achievements/bulk-issue` (#495). */
        post: operations["bulk_issue_achievements"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/achievements/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** `PATCH /integrations/{slug}/achievements/{key}`. */
        patch: operations["update_achievement_definition"];
        trace?: never;
    };
    "/integrations/{slug}/achievements/{key}/issue": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** `POST /integrations/{slug}/achievements/{key}/issue` (#32). */
        post: operations["issue_achievement"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/challenge": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["create_integrator_challenge"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/connect": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /integrations/{slug}/connect` — the consent flow (#27) and the endpoint
         *     that establishes a `IntegratorBinding` (#83). Idempotent: reconnecting to a
         *     integrator the caller already has an active binding to does not create a
         *     second binding or emit a second `game.binding_established`, but it does
         *     still grant any newly-approved capabilities.
         */
        post: operations["connect"];
        /**
         * `DELETE /integrations/{slug}/connect` — ends the binding and revokes every
         *     active grant under it, in the same transaction (#83's invariant).
         */
        delete: operations["disconnect"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/grants/{capability}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * `DELETE /integrations/{slug}/grants/{capability}` — revokes one capability
         *     without ending the binding.
         */
        delete: operations["revoke_grant"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/keys": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /integrations/{slug}/keys` (#90) — public, unauthenticated: an issuer's
         *     full key history (any role, any status), the read side of
         *     [`add_issuer_key`]/[`revoke_issuer_key`]. Public keys are already public
         *     by definition, and #90's design calls for the Hub to show an integrator's "key
         *     history and status" on its profile page — nothing here is sensitive the
         *     way the integrator's own root-key-authenticated endpoints are. Ordered oldest
         *     first so a viewer reads it as a timeline.
         */
        get: operations["list_issuer_keys"];
        put?: never;
        /**
         * `POST /integrations/{slug}/keys` (#84, implementing #80's decided two-tier key
         *     model) — adds a new key to the issuer's key set. Requires the caller to
         *     authenticate as the named `slug` with a currently-valid **root** key
         *     ([`authenticate_integrator_root`]); an operational key, or a root key
         *     belonging to a different integrator, is rejected. Emits `issuer.key_added`.
         */
        post: operations["add_issuer_key"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/keys/{key_id}/revoke": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /integrations/{slug}/keys/{key_id}/revoke` (#84) — revokes a key in the
         *     issuer's key set (root or operational; a root key can revoke itself, the
         *     same "any key genuinely under your control" trust already implied by
         *     authenticating as root at all). Same root-key-of-the-named-issuer
         *     requirement as [`add_issuer_key`]. Revoking an already-revoked or
         *     nonexistent key returns [`AppError::IssuerKeyForbidden`] rather than
         *     silently succeeding — same posture `remove_friend`-style "no-op success"
         *     endpoints elsewhere in this repo deliberately don't take, since a caller
         *     retrying a revoke against a key it no longer controls is exactly the
         *     kind of thing worth surfacing, not swallowing. Emits `issuer.key_revoked`.
         */
        post: operations["revoke_issuer_key"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/mappings": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /integrations/{slug}/mappings` — every published mapping for this
         *     integrator, oldest first. Public, unauthenticated, same posture as
         *     schema-version listing. Empty for an integrator that has never
         *     published one.
         */
        get: operations["list_mappings"];
        put?: never;
        /**
         * `POST /integrations/{slug}/mappings` — publish a mapping between two of
         *     this integrator's own schema versions. Always an insert; mappings have
         *     no lineage/superseding concept (see module doc comment), so unlike
         *     schema publication this never touches an existing row.
         */
        post: operations["publish_mapping"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/mappings/{seq}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /integrations/{slug}/mappings/{seq}` — one published mapping,
         *     verbatim. Public, unauthenticated.
         */
        get: operations["get_mapping"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/milestones": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /** `GET /integrations/{slug}/milestones` (#324/#325). */
        get: operations["list_milestone_definitions"];
        put?: never;
        /** `POST /integrations/{slug}/milestones` (#324/#325). */
        post: operations["create_milestone_definition"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/milestones/bulk-issue": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** `POST /integrations/{slug}/milestones/bulk-issue` (#495). */
        post: operations["bulk_issue_milestones"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/milestones/{key}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /** `PATCH /integrations/{slug}/milestones/{key}` (#324/#325). */
        patch: operations["update_milestone_definition"];
        trace?: never;
    };
    "/integrations/{slug}/milestones/{key}/issue": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /** `POST /integrations/{slug}/milestones/{key}/issue` (#32/#324/#325). */
        post: operations["issue_milestone"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/recognitions": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /integrations/{slug}/recognitions` — every integrator `slug`
         *     currently, actively recognizes. Public, unauthenticated.
         */
        get: operations["list_recognitions"];
        put?: never;
        /**
         * `POST /integrations/{slug}/recognitions` — publishes (or updates)
         *     `slug`'s recognition of `recognized_slug`. Always the caller's own
         *     declared policy about itself; never anything read from or written
         *     about the target beyond this one directional fact.
         */
        post: operations["publish_recognition"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/recognitions/revoke": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /integrations/{slug}/recognitions/revoke` — marks `slug`'s
         *     recognition of `recognized_slug` revoked (`revoked_at` set, row kept).
         *     A no-op, not an error, if no recognition was ever published.
         */
        post: operations["revoke_recognition"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/recognized-by": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /integrations/{slug}/recognized-by` — every integrator that
         *     currently, actively recognizes `slug`. Public, unauthenticated.
         */
        get: operations["list_recognized_by"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/registry": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Registered at `/integrations/{slug}/registry` (this macro's own path)
         *     and, per issue #95, identically at `/registry/{slug}` — same handler,
         *     two routes; see this module's own doc comment.
         */
        get: operations["get_integrator_registry"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/schemas": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /integrations/{slug}/schemas` — every published version for this integrator,
         *     oldest first. Public, unauthenticated (see module doc comment). Empty
         *     for an integrator that has never published.
         */
        get: operations["list_schema_versions"];
        put?: never;
        /**
         * `POST /integrations/{slug}/schemas` — publish the next version. Always an
         *     insert, never an update to an existing row (see module doc comment).
         */
        post: operations["publish_schema_version"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/schemas/{version}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /integrations/{slug}/schemas/{version}` — one published version, verbatim.
         *     Public, unauthenticated. This is the endpoint a round-trip fetch of a
         *     just-published version calls to prove the stored `proto_source` matches
         *     what was submitted exactly.
         */
        get: operations["get_schema_version"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/schemas/{version}/data": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /integrations/{slug}/schemas/{version}/data` — publish (or supersede)
         *     this integrator's instance data for `subject` against the named schema
         *     version. Always an insert, never an update to an existing row (see
         *     module doc comment).
         */
        post: operations["publish_instance"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/integrations/{slug}/schemas/{version}/data/{subject}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * `DELETE /integrations/{slug}/schemas/{version}/data/{subject}` (#533) —
         *     append-only tombstone for the schema's current (non-superseded,
         *     non-deleted) instance belonging to `subject`, following
         *     `docs/architecture/revocation.md`'s pattern: the original
         *     `integrator_data_instances` row's `instance`/`published_at` are never
         *     touched, only `deleted_at`/`delete_reason_code`/`delete_reason` are
         *     set — the same "add a lifecycle marker, never mutate the substantive
         *     content" shape this module already uses for `superseded_by` above. The
         *     original `game_data.published` event, and the new `game_data.deleted`
         *     event this appends, both stay observable in raw ledger history.
         */
        delete: operations["delete_instance"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/issuers/register": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /issuers/register` (#481) — explicit, self-service, never
         *     reviewed/approved on any tier (see module doc comment); the "gate" is
         *     which networks admit an unregistered key implicitly, not who may call
         *     this endpoint. Idempotent: registering an already-registered key
         *     updates its `issuer_ref` rather than erroring, matching this
         *     endpoint's "admission, not gatekeeping" posture.
         */
        post: operations["register_issuer"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/issuers/registration-challenge": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /issuers/registration-challenge` — issues a short-lived,
         *     single-use nonce, mirroring `integrators::create_integrator_challenge`'s
         *     shape. No auth: obtaining a challenge proves nothing by itself, only
         *     actually signing it (see [`register_issuer`]) does.
         */
        post: operations["create_registration_challenge"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["me"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch: operations["update_profile"];
        trace?: never;
    };
    "/me/achievements": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/achievements?integrator_id=&claim_kind=&before=&limit=` (#34,
         *     paginated/filtered per #377) — bearer-authenticated as the reading
         *     identity, returning that identity's own attestation history (every
         *     issuer, active and revoked alike): the identity reading its own
         *     record, not a per-consumer trust question, so no additional
         *     authorization beyond "this is genuinely you" is needed — matching
         *     `GET /attestations/{id}`'s own "authenticity/validity are facts, never
         *     gated behind a specific issuer's permission" posture. Also the read
         *     path `crates/sdk/src/achievements.rs::Session::achievements` (#34) and
         *     the Hub's achievements view (#35) are designed against.
         * @description The N+1 `build_attestation_response` had (one integrator-category, one
         *     integrator-status, one issuer-keys, one revocation query — *per row*)
         *     is fixed here by batching all four lookups across the whole page: a
         *     fixed four queries regardless of how many attestations are on the
         *     page, not `1 + 4*page_size`.
         */
        get: operations["list_my_achievements"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/connections": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/connections` — every binding the caller has, each with its
         *     currently-active grants. Ended bindings are not included; a future
         *     history view can add them separately without changing this endpoint's
         *     meaning.
         */
        get: operations["list_my_connections"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/devices": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/devices` — every signing key (active or revoked) registered to
         *     the caller's identity, for the Hub's device-list/revoke UI.
         */
        get: operations["list_devices"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/devices/grants": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/devices/grants?status=…` — every grant the caller's identity
         *     has requested, from any device (used both by a trusted device polling
         *     for pending requests to approve, and by the requesting device polling
         *     its own request's status).
         */
        get: operations["list_device_grants"];
        put?: never;
        /**
         * `POST /me/devices/grants` — a device with a session but no local signing
         *     key asks to be granted one. Requires only the caller's existing session
         *     (already proven by a real WebAuthn ceremony, per this repo's usual
         *     "every route just accepts a session bearer token" pattern) — approval,
         *     not this request, is where the stronger signature check lives.
         */
        post: operations["request_device_grant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/devices/grants/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/devices/grants/:id` — the requesting device polls this for its
         *     own grant's status until it flips to `approved`. Scoped to the caller's
         *     own identity like every other read here, so one identity can never poll
         *     another's pending grant.
         */
        get: operations["get_device_grant"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/devices/grants/{id}/approve": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /me/devices/grants/:id/approve` — the security-bearing step this
         *     whole module exists for. Verifies the approving key is one of the
         *     caller's own, still active (not revoked), and that its signature really
         *     covers this exact grant and requested key before ever inserting
         *     anything — satisfies the ticket's invariant that a grant only ever
         *     comes from a device that itself already passed a real WebAuthn ceremony
         *     (every row in `identity_signing_keys` only exists because
         *     `register_finish` or a prior approval put it there).
         */
        post: operations["approve_device_grant"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/devices/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /**
         * `PATCH /me/devices/:id` — issue #145: the first device (registered by
         *     `handlers::register_finish`) previously had no way to be labeled after
         *     the fact, and no device could be renamed at all. Same ownership check as
         *     `revoke_device` — any authenticated session for the identity may rename
         *     any of its own signing-key rows, active or revoked, unilaterally.
         */
        patch: operations["rename_device"];
        trace?: never;
    };
    "/me/devices/{id}/revoke": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /me/devices/:id/revoke` — unilateral, per the ticket's invariant:
         *     any currently-authenticated session for the identity can revoke any
         *     signing-key row (including the one it's revoking itself with, for a
         *     deliberate self-rotation), independent of the revoked device's
         *     cooperation. Network-attributed, not individually signed — same
         *     milestone-1 precedent `friends.rs`'s `friend.requested` already uses;
         *     revocation only ever narrows trust, so it doesn't need the higher
         *     signing bar grant approval does.
         */
        post: operations["revoke_device"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/grants": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/grants` — the calling integrator's own active grants for the
         *     authenticating user, read by `crates/sdk/src/lib.rs`'s
         *     `AvalonClient::authenticate()` to populate `Session.granted`.
         * @description Authenticated by the caller's own user session
         *     (`Authorization: Bearer <user token>`), same as every other endpoint
         *     in this module — **not** the integrator challenge-response scheme
         *     (`integrators::authenticate_integrator`). The `x-avalon-integrator-key-id` header only
         *     says *which* integrator's grants to read; it is not itself a security
         *     boundary here, since the answer ("what has this user granted this
         *     integrator") is the user's own information to ask about their own
         *     connections, not something that needs an integrator to prove key possession —
         *     the SDK already knows its own `integrator_credential_key_id`
         *     (`AvalonConfig`) and just needs a way to tell the server which integrator it
         *     is asking on behalf of.
         */
        get: operations["my_grants"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/guild-announcements": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/guild-announcements` (issue #280) — the most recent posts to
         *     any announcement-only channel in any guild the caller currently belongs
         *     to, newest first. This is a plain read, not a notification/unread
         *     tracker: read/unread state is the Hub's own client-local concern (see
         *     `docs/architecture/guilds.md`'s "Guild announcement alerts" section),
         *     matching #22/#74/#253's "chat is operational-tier, not protocol
         *     history" posture — there is nothing here to promote to durable state,
         *     so there is nothing here to track server-side either.
         * @description Scoped to *current* membership by construction: the `JOIN indexer_guild_members`
         *     below means a guild the caller has left simply produces no rows for
         *     that guild, the same "no historical-membership machinery for
         *     non-durable data" posture `list_archive`'s own doc comment already
         *     takes, applied here without needing a separate cleanup step — a member
         *     who leaves a guild stops seeing its announcements on their very next
         *     poll, automatically.
         */
        get: operations["list_my_guild_announcements"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/guild-invites": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/guild-invites` — every unresolved invite where the caller is
         *     the invitee (issue #442). Without this, the only way an invitee learns
         *     an invite exists at all is being told its raw id out of band by the
         *     sender — this is the "receiving end" listing `Guild.vue`'s invite flow
         *     has been missing since #21, mirroring the shape
         *     `recovery::guardian_requests` already established for the same "every
         *     active thing where the caller is on the receiving end" need.
         */
        get: operations["my_guild_invites"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/guilds": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get: operations["list_my_guilds"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/history": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * Only the caller's own events, never another identity's — enforced by
         *     construction, not by a filter a caller could omit: the issuer prefix is
         *     always built from the authenticated identity id here, never accepted as
         *     a request parameter. Reads the ledger directly (issuer-filtered, see
         *     `avalon_chain::PostgresSettlementProvider::list_entries_for_issuer_prefix`)
         *     — a deliberate, permanent exception to issue #44's "no request handler
         *     queries the ledger" invariant, not a stopgap: this endpoint's whole job
         *     is exposing the raw append-only history itself, which is fundamentally
         *     a settlement-native read (a full historical log), not a current-state
         *     one — projecting the entire per-issuer event history into the indexer
         *     just to re-serve it here would duplicate the ledger, not replace it.
         *     `crates/server/tests/read_model_boundary.rs`'s guard test names this
         *     function as the one allowed exception; any other `PostgresSettlementProvider`
         *     call added to this file should not be.
         */
        get: operations["my_history"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/passkeys": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/passkeys` — every passkey registered to the caller's identity,
         *     for the Hub's "your passkeys" list.
         */
        get: operations["list_passkeys"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/passkeys/register/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /me/passkeys/register/finish` — completes the ceremony
         *     `register_start` began and inserts the new `identity_keys` row. Requires
         *     the *same* authenticated session throughout (both `start` and `finish`
         *     re-derive `identity_id` from the caller's own bearer token; the ceremony
         *     row's `identity_id` is checked against it too) rather than trusting
         *     whatever identity the stored ceremony state says — defense in depth
         *     against a captured ticket id being replayed from a different identity's
         *     session, on top of the ceremony `kind` already keeping this flow's
         *     tickets out of `handlers::register_finish`'s unauthenticated
         *     identity-creation path.
         */
        post: operations["devices_register_finish"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/passkeys/register/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /me/passkeys/register/start` — begins a WebAuthn registration
         *     ceremony for an *additional* passkey bound to the caller's already-
         *     existing identity, gated by the caller's session rather than by an
         *     unclaimed identity id the way `handlers::register_start` is. Existing
         *     credential ids are passed as `exclude_credentials` so an authenticator
         *     that already registered one of them (e.g. the same physical key) won't
         *     silently re-register itself as a second, functionally duplicate
         *     credential.
         */
        post: operations["devices_register_start"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/passkeys/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        /**
         * `PATCH /me/passkeys/:id` — same unilateral-rename convention as
         *     `devices::rename_device`.
         */
        patch: operations["rename_passkey"];
        trace?: never;
    };
    "/me/passkeys/{id}/revoke": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /me/passkeys/:id/revoke` — deletes one passkey. Revoking the
         *     identity's last remaining passkey requires a fresh signature from one of
         *     the identity's registered `identity_signing_keys` (issue #704/#698 —
         *     upgraded from the old client-side `?confirm=true` speed bump); revoking
         *     one of several never does. The count check and the delete happen inside
         *     one transaction so a concurrent registration/revoke from another session
         *     can't race past the guard.
         */
        post: operations["revoke_passkey"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/presence": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * `PUT /me/presence` — a user publishing their own status. Deliberately
         *     cannot set `active_in`: that's reserved for an integrator's own credential
         *     (`update_integrator_presence` below).
         */
        put: operations["update_my_presence"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/recovery/guardian-of": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/recovery/guardian-of` — every identity that currently names the
         *     caller as one of their recovery guardians (issue #443's opt-out consent
         *     model: a guardian can always see who's relying on them and self-remove
         *     via [`resign_guardian`] below, without the owner's cooperation — there is
         *     no accept step, matching `set_guardians`'s existing "active the moment
         *     the owner names you" behavior).
         */
        get: operations["guardian_of"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/recovery/guardian-of/{identity_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post?: never;
        /**
         * `DELETE /me/recovery/guardian-of/{identity_id}` — a guardian removing
         *     themselves from someone else's guardian set, without that owner's
         *     cooperation (issue #443). If this drops the owner's guardian count below
         *     their configured threshold, the threshold is clamped down to the new
         *     count instead — the same "recovery must stay satisfiable" invariant
         *     [`validate_guardian_settings`] enforces on the owner's own writes, kept
         *     true here too rather than left as a silent trap the owner discovers only
         *     when trying to actually recover.
         */
        delete: operations["resign_guardian"];
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/recovery/guardian-requests": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/recovery/guardian-requests` — every active recovery request
         *     (across every identity, not just one) where the caller is currently a
         *     guardian, for the Hub's guardian-approval UI: "a friend of yours is
         *     trying to recover their identity, here's the pending request."
         */
        get: operations["guardian_requests"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/recovery/guardians": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/recovery/guardians` — the caller's own current configuration.
         *     An identity with none configured gets an empty list and threshold 0,
         *     not a 404 — "not configured yet" is a normal state, not an error.
         */
        get: operations["get_guardians"];
        /**
         * `PUT /me/recovery/guardians` — (re)configures the caller's guardian set
         *     and threshold in one call, requiring the caller's *current* session
         *     (`authenticate`) the whole invariant rests on. Every guardian must be a
         *     current friend (issue #15's network-level primitive is deliberately the
         *     only pool this draws from — see the ticket) and not the caller
         *     themselves; the full set is validated together via
         *     [`validate_guardian_settings`] rather than incrementally, so a client
         *     can't build up an invalid configuration one add-guardian call at a
         *     time. Replaces the set wholesale (delete-then-insert in one
         *     transaction) rather than diffing — simpler, and this isn't a
         *     high-frequency operation.
         */
        put: operations["set_guardians"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/me/recovery/status": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /me/recovery/status` — the session-authenticated equivalent of
         *     [`identity_recovery_status`] for the caller's own identity. Exists
         *     alongside the public endpoint specifically so the Hub can surface a
         *     prominent "a recovery is in progress against your identity" notice
         *     wherever the owner still has *some* working session — reusing the same
         *     data shape rather than inventing a separate notification channel, per
         *     the ticket's "reuse rather than invent" guidance.
         */
        get: operations["my_recovery_status"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/people/discover": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /people/discover` — session-authenticated, no query parameters by
         *     design (see module doc comment: this is never a name/handle search).
         *     Computes candidates from the caller's own friends-of-friends and
         *     mutual-guild relationships, excluding the caller, existing friends, and
         *     any blocked relationship in either direction.
         */
        get: operations["discover_people"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/presence": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /presence?ids=…` — session-authenticated. Reads default to
         *     friends-only visibility (see [`presence_visible`] and module doc
         *     comment); a caller-hidden `active_in` preference
         *     (`presence_preferences.hide_active_in`, see [`hide_active_in_for`]) is
         *     applied independently on top, so an identity visible to the caller can
         *     still have `active_in` come back `null` if they've opted out of it.
         */
        get: operations["get_presence"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/presence/{identity_id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        /**
         * `PUT /presence/:identity_id` — an integrator publishing presence on behalf of
         *     a user it's bound to. Authenticated via `crate::authz`'s
         *     `Caller`/`require_capability` (issue #28): the caller must be
         *     `Caller::Integrator` (a user session hitting this route is rejected — that
         *     endpoint is `PUT /me/presence` above), the path `identity_id` must
         *     match the identity the integrator claims to act for
         *     (`x-avalon-identity-id`, resolved by `authenticate_caller` — see
         *     `authz`'s own doc comment for why this heads off an integrator naming one
         *     identity in the path and another in the header), and the integrator must
         *     hold an active `presence.publish` grant under an active binding to
         *     that identity — `require_capability` alone is what rejects an unbound
         *     identity or a revoked/missing grant, no separate hand-rolled check
         *     here (see `authz`'s own module doc comment on why that's the one
         *     authorization decision, not two).
         */
        put: operations["update_integrator_presence"];
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/recovery/requests/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /recovery/requests/finish` — completes the ceremony and creates
         *     the `recovery_requests` row. Re-checks guardian configuration and the
         *     rate limit (both may have changed since `start`) and relies on
         *     `recovery_requests_one_active_per_identity`'s unique index as the final
         *     word on "at most one active attempt" — a second `finish` racing this
         *     one for the same identity loses to the constraint, not to a
         *     check-then-act gap in application code.
         */
        post: operations["finish_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/recovery/requests/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /recovery/requests/start` — begins the new device's WebAuthn
         *     registration ceremony. Unauthenticated by necessity (the caller has no
         *     valid session for `identity_id` — that's the entire premise of
         *     recovery), so this and `finish_request` below are the one deliberate
         *     exception to this crate's "every route requires a session" norm.
         *     Guarded three ways rather than left as an open door: `identity_id` must
         *     name a real identity, and must actually have guardians configured (an
         *     unconfigured identity can never satisfy any M, so there's nothing to
         *     spam toward) — both cases return the exact same
         *     `AppError::RecoveryNotAvailable` (same status, same body), so an
         *     unauthenticated prober can never distinguish "this identity doesn't
         *     exist" from "this identity exists but has no guardians set up." The
         *     rolling-window rate limit ([`guard_rate_limit`]) is checked *before*
         *     any WebAuthn ceremony work happens, since that ceremony is the
         *     expensive part.
         */
        post: operations["start_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/recovery/requests/{id}": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        /**
         * `GET /recovery/requests/:id` — public, deliberately: the ticket's
         *     "mandatory *public* time-delay" invariant means the fact of an
         *     in-flight recovery, and when its delay ends, must be checkable by
         *     anyone, not just the owner or guardians — a public marker on the
         *     identity, same alternative #99 itself named. Never exposes which
         *     specific guardians have approved, only the count.
         */
        get: operations["get_request"];
        put?: never;
        post?: never;
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/recovery/requests/{id}/approve": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /recovery/requests/:id/approve` — a guardian's independent
         *     approval. Requires the caller to currently be one of the identity's
         *     guardians (not just at request time — a guardian removed since can no
         *     longer approve, mirroring [`guard_cancel_authority`]'s same "current,
         *     not historical, guardian" rule). Idempotency: a guardian approving
         *     twice hits `recovery_approvals`'s primary key and gets
         *     `AppError::AlreadyApproved`, not a double-counted approval. Reaching
         *     [`meets_threshold`] against `threshold_at_request` (frozen at request
         *     creation, not the identity's possibly-since-changed live threshold)
         *     transitions the row into the delay phase in the same transaction as
         *     this approval.
         */
        post: operations["approve_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/recovery/requests/{id}/cancel": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /recovery/requests/:id/cancel` — the veto path. Either the
         *     identity's own owner (any session for `identity_id` itself — including,
         *     deliberately, a session opened on a still-trusted device the owner
         *     never lost) or any of its *current* guardians may cancel an in-flight
         *     attempt they believe is malicious, per [`guard_cancel_authority`]. A
         *     request already `completed`/`cancelled` cannot be cancelled again.
         */
        post: operations["cancel_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/recovery/requests/{id}/finalize": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        /**
         * `POST /recovery/requests/:id/finalize` — deliberately public and
         *     idempotent (see module docs): it grants nothing beyond what
         *     `approve_request`/the elapsed delay already durably authorized, so
         *     there is no meaningful caller identity to check. Calling it on an
         *     already-`completed` request simply returns the current (already
         *     finalized) state rather than erroring, so a client that finalizes
         *     eagerly and loses the race to another caller (or to a future
         *     auto-finalize sweep, not built this pass — see PR description) doesn't
         *     need special-case handling.
         */
        post: operations["finalize_request"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sessions/finish": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["session_finish"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
    "/sessions/start": {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        get?: never;
        put?: never;
        post: operations["session_start"];
        delete?: never;
        options?: never;
        head?: never;
        patch?: never;
        trace?: never;
    };
}
export type webhooks = Record<string, never>;
export interface components {
    schemas: {
        AchievementDefinitionResponse: {
            /** Format: date-time */
            created_at: string;
            description: string;
            /**
             * @description Always populated — falls back to [`DEFAULT_ICON`] when the
             *     definition has neither `icon` nor `icon_url` set, so every reader
             *     (the Hub's `AvalonAchievementCard`) always has *something* to
             *     render (issue #332's invariant), never a blank slot.
             */
            icon: string;
            /**
             * @description When present, takes precedence over `icon` on the client — never a
             *     silent fallback to the default just because both happen to be set.
             */
            icon_url?: string | null;
            id: string;
            /** Format: uuid */
            integrator_id: string;
            key: string;
            name: string;
            retired: boolean;
            /** Format: date-time */
            retired_at: string | null;
            schema?: string | null;
            /** Format: date-time */
            updated_at: string;
            /** Format: int32 */
            version: number;
        };
        AddIssuerKeyRequest: {
            algorithm: string;
            /**
             * @description Standard-base64-encoded raw public key bytes, same shape
             *     [`InitialKeyRequest`] uses at registration.
             */
            public_key: string;
            /**
             * @description `"attestation"` (the default, omit for existing pre-#543 caller
             *     behavior) or `"shard_settlement"` — see
             *     `avalon_protocol::integrators::KeyPurpose`, issue #543.
             */
            purpose?: string;
            /** @description `"root"` or `"operational"` — see `avalon_protocol::integrators::KeyRole`. */
            role: string;
            /** Format: date-time */
            valid_until?: string | null;
        };
        AddPasskeyFinishRequest: {
            /**
             * @description A user-chosen label for the passkey being added (e.g. "Work
             *     laptop's fingerprint sensor") — purely descriptive, same convention
             *     as `identity_signing_keys.label` / `handlers::RegisterFinishRequest::device_label`.
             */
            label?: string | null;
            /** Format: uuid */
            ticket_id: string;
            webauthn_credential: Record<string, never>;
        };
        AddPasskeyStartResponse: {
            challenge: Record<string, never>;
            /** Format: uuid */
            ticket_id: string;
        };
        ApproveDeviceGrantRequest: {
            /**
             * Format: uuid
             * @description Which of the caller's own `identity_signing_keys` rows is approving
             *     this grant — must belong to the caller's identity and not be
             *     revoked.
             */
            approver_signing_key_id: string;
            /**
             * @description Base64-encoded Ed25519 signature over
             *     `device_grant_approval_signing_bytes(grant_id, identity_id, requested_signing_public_key)`,
             *     produced by `approver_signing_key_id`'s key.
             */
            signature: string;
        };
        /**
         * @description Issue #704: `POST /auth/device/approve` mints a brand-new, independently-
         *     usable session for a different device off nothing but the approver's
         *     ambient session today — #697/#698's signature-required tier closes that
         *     gap. `signing_key_id`/`signature` are optional on the wire (so
         *     deserialization never fails outright) but enforced as required by
         *     [`require_fresh_signature`] below.
         */
        ApprovePairingRequest: {
            signature?: string | null;
            /** Format: uuid */
            signing_key_id?: string | null;
            user_code: string;
        };
        ArchivedMessageResponse: {
            /** Format: date-time */
            archived_at: string;
            /** Format: uuid */
            author: string;
            body: string;
            /** Format: uuid */
            channel_id: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            sent_at: string;
        };
        /**
         * @description One entry in an attestation's history — `"issued"` always, plus
         *     `"revoked"` if a revocation entry exists (#85). Reinstatement/
         *     supersession entries would append here too, once either exists.
         */
        AttestationHistoryEntry: {
            /** Format: date-time */
            at: string;
            event: string;
            reason?: string | null;
            reason_code?: string | null;
        };
        AttestationProofResponse: {
            algorithm: string;
            key_id: string;
        };
        AttestationReadResponse: {
            achievement: string;
            authenticity: components["schemas"]["AuthenticityResponse"];
            history: components["schemas"]["AttestationHistoryEntry"][];
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            issued_at: string;
            issuer: string;
            proof: components["schemas"]["AttestationProofResponse"];
            /** Format: uuid */
            subject: string;
            validity: components["schemas"]["ValidityResponse"];
        };
        AttestationResponse: {
            achievement: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            issued_at: string;
            issuer: string;
            proof: components["schemas"]["AttestationSignatureResponse"];
            /** Format: uuid */
            subject: string;
        };
        AttestationSignatureResponse: {
            algorithm: string;
            bytes: string;
            /** Format: uuid */
            key_id: string;
        };
        AuthenticityResponse: {
            key_id: string;
            /** @enum {string} */
            status: "authentic";
        } | {
            reason: string;
            /** @enum {string} */
            status: "not_authentic";
        };
        BlockListEntry: {
            /** Format: uuid */
            blocked: string;
            /** Format: date-time */
            created_at: string;
        };
        BlockResponse: {
            /** Format: uuid */
            blocked: string;
            /** Format: date-time */
            created_at: string;
        };
        /**
         * @description One claim in a [`BulkIssueAttestationRequest`] — just enough to look up
         *     its definition; the proof covering the whole ordered list lives once,
         *     at the request's top level (see that struct's own doc comment).
         */
        BulkClaimRequest: {
            evidence?: Record<string, never> | null;
            key: string;
        };
        /**
         * @description One claim's own outcome — a bulk call is never all-or-nothing (#495's
         *     own invariant): a claim referencing an unknown or retired definition
         *     fails on its own, every other claim in the same call still succeeds.
         */
        BulkClaimResult: {
            attestation: components["schemas"]["AttestationResponse"];
            key: string;
            /** @enum {string} */
            status: "issued";
        } | {
            /**
             * @description A stable machine-readable code, matching `AppError::code`'s own
             *     convention elsewhere in this codebase — never just the free-text
             *     `error` string alone.
             */
            code: string;
            error: string;
            key: string;
            /** @enum {string} */
            status: "failed";
        };
        /**
         * @description `POST /integrations/{slug}/achievements/bulk-issue` /
         *     `.../milestones/bulk-issue` (issue #495, implementing #492's decided
         *     shape). One challenge-response proof that this integrator's key is
         *     making the call, plus **one** signature over
         *     [`bulk_attestation_signing_bytes`] of the whole ordered `claims` list —
         *     never a per-claim signature. Every claim still becomes its own ordinary
         *     attestation server-side, through the exact same write path
         *     [`issue_attestation`] uses per-item; this endpoint is purely an
         *     API/transport-layer convenience over that, per #492's own invariant.
         */
        BulkIssueAttestationRequest: {
            claims: components["schemas"]["BulkClaimRequest"][];
            /**
             * Format: uuid
             * @description Which of the issuer's own keys signed the whole ordered list.
             */
            key_id: string;
            /**
             * @description Standard-base64-encoded detached Ed25519 signature over
             *     [`bulk_attestation_signing_bytes`] of `claims`, in order.
             */
            signature: string;
        };
        BulkIssueAttestationResponse: {
            /**
             * @description Same order as the request's `claims` — a caller matches results
             *     back to what it submitted by position, not by searching for `key`
             *     (which isn't itself guaranteed unique within one call).
             */
            results: components["schemas"]["BulkClaimResult"][];
        };
        CancelRecoveryRequest: {
            reason?: string | null;
        };
        ChannelResponse: {
            announcement_only: boolean;
            archived: boolean;
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            guild_id: string;
            /** Format: uuid */
            id: string;
            name: string;
            /**
             * @description Issue #458. Non-member visibility baseline for this channel —
             *     same meaning as `guild_events.public` (#448), just newly added
             *     for channels, which had no non-member visibility concept before
             *     this ticket at all.
             */
            public: boolean;
            topic?: string | null;
        };
        ConnectRequest: {
            capabilities?: string[];
            signature?: string | null;
            /**
             * Format: uuid
             * @description #697/#698: hands a third party standing permission over the
             *     identity's data going forward — signature-required.
             */
            signing_key_id?: string | null;
        };
        ConnectResponse: {
            /** Format: uuid */
            binding_id: string;
            /** Format: date-time */
            established_at: string;
            granted_capabilities: string[];
            /** Format: uuid */
            integrator_id: string;
        };
        Connection: {
            /** Format: uuid */
            binding_id: string;
            /** Format: date-time */
            established_at: string;
            grants: components["schemas"]["ConnectionGrant"][];
            /** Format: uuid */
            integrator_id: string;
            name: string;
            slug: string;
        };
        ConnectionGrant: {
            capability: string;
            /** Format: date-time */
            granted_at: string;
        };
        ConversationMessageResponse: {
            /** Format: uuid */
            author: string;
            body: string;
            /** Format: uuid */
            conversation_id: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            sent_at: string;
        };
        ConversationResponse: {
            /** Format: uuid */
            id: string;
            participants: string[];
        };
        ConversationSendMessageRequest: {
            body: string;
            /**
             * Format: uuid
             * @description The submitting client's journal `EntryId` (issue #110/#111), when
             *     this request came from the SDK's deferred submission engine rather
             *     than a direct online send. Optional — a message sent directly online
             *     never sets this and never needs to dedupe against anything (see
             *     migration `0037_conversation_message_idempotency`).
             *
             *     A retried request after a dropped response carries the *same*
             *     `client_entry_id` as the original attempt — that's the whole
             *     mechanism: [`send_message`] treats a conflict on
             *     `(conversation_id, client_entry_id)` as "already applied" and
             *     returns the existing row instead of erroring or inserting a
             *     duplicate.
             */
            client_entry_id?: string | null;
        };
        CreateAchievementDefinitionRequest: {
            description: string;
            /**
             * @description One of [`BUILTIN_ICONS`]; omitted/`null` falls back to
             *     [`DEFAULT_ICON`] at read time (issue #332).
             */
            icon?: string | null;
            /**
             * @description An integrator-hosted image URL, taking precedence over `icon` when
             *     present. `http`/`https` only.
             */
            icon_url?: string | null;
            key: string;
            name: string;
            /**
             * @description A `GlobalId` (`<namespace>:<owner>:<kind>:<key>`), serialized as a
             *     plain string — `avalon_protocol::ids::GlobalId` has no `ToSchema`
             *     impl of its own.
             */
            schema?: string | null;
        };
        CreateBlockRequest: {
            /** Format: uuid */
            identity_id: string;
        };
        CreateChannelRequest: {
            name: string;
        };
        CreateConversationRequest: {
            participants: string[];
        };
        CreateEventRequest: {
            /** Format: uuid */
            channel_id?: string | null;
            description?: string | null;
            /** Format: date-time */
            ends_at?: string | null;
            /**
             * @description Issue #448. Omitted defaults to `false` — member-only, same as
             *     every event before this field existed. Gated by the same
             *     `event_manage` check as the rest of this request, no new
             *     permission needed.
             */
            public?: boolean;
            /** Format: date-time */
            starts_at: string;
            title: string;
        };
        CreateFriendRequestRequest: {
            /** Format: uuid */
            to: string;
        };
        CreateGuildInviteRequest: {
            /** Format: uuid */
            to: string;
        };
        CreateGuildRequest: {
            description?: string;
            name: string;
            tag: string;
        };
        CreateIntegratorRequest: {
            /** @description `integrator` / `app` / `service` (#282); omitted means `integrator`. */
            category?: string | null;
            initial_key: components["schemas"]["InitialKeyRequest"];
            name: string;
            owner_name: string;
            requested_capabilities?: string[];
            slug: string;
        };
        CreateJoinRequestRequest: {
            message?: string | null;
        };
        CreateRoleRequest: {
            badge?: null | components["schemas"]["RoleBadgeRequest"];
            /**
             * @description Issue #152. Defaults to an empty string, same "no explicit
             *     `Option` needed, empty is a valid value" treatment `Guild.description`
             *     already gets.
             */
            description?: string;
            name: string;
            permissions?: string[];
            signature?: string | null;
            /**
             * Format: uuid
             * @description #697/#698: role definitions are the guild's permission structure —
             *     signature-required.
             */
            signing_key_id?: string | null;
        };
        /**
         * @description A self-signed assertion that a human, shown `requesting_context`, just
         *     approved logging `identity_id` (acting through `signing_key_id`) into
         *     `destination_base_url`.
         */
        CrossNodeLoginGrant: {
            /**
             * @description The node this grant is good for logging into, and nowhere else —
             *     see this module's doc comment on why it must live inside the signed
             *     bytes.
             */
            destination_base_url: string;
            /** Format: date-time */
            expires_at: string;
            /** Format: uuid */
            identity_id: string;
            /** Format: date-time */
            issued_at: string;
            /**
             * Format: uuid
             * @description Anti-replay: unique per grant, checked against a
             *     consumed-nonce table by the verifying node on
             *     `POST /auth/cross-node/submit`, same pattern
             *     `ContinuationToken::nonce`'s own doc comment establishes.
             */
            nonce: string;
            /**
             * @description The human-legible context shown to the approver before they
             *     approved (requesting integrator/node name, at minimum) — carried in
             *     the signed bytes so the grant itself is evidence of what was shown,
             *     not just a claim about it. #642 (decision, open) still owns what
             *     this minimally has to contain.
             */
            requesting_context: string;
            /** @description Lowercase hex-encoded Ed25519 signature over [`signing_bytes`]. */
            signature: string;
            /**
             * Format: uuid
             * @description Which of the identity's (possibly several) signing keys approved
             *     this — same multi-key model `identity_signing_keys`/`devices.rs`
             *     already supports.
             */
            signing_key_id: string;
        };
        DeleteInstanceRequest: {
            reason?: string | null;
            reason_code?: string;
        };
        /**
         * @description `DELETE /guilds/{id}/permission-overrides/{override_id}` — clears an
         *     override, reverting that (role, resource, permission) triple back to
         *     the role's base permission list. Requires `manage_roles`.
         *     #697/#698: no other body fields, exists only to carry the fresh-signature
         *     proof — same posture as [`DeleteRoleRequest`].
         */
        DeletePermissionOverrideRequest: {
            signature?: string | null;
            /** Format: uuid */
            signing_key_id?: string | null;
        };
        /**
         * @description #697/#698: role deletion has no other body fields, so this exists only
         *     to carry the fresh-signature proof. `#[serde(default)]` so a bare `{}`
         *     (or, for a truly bodyless client, an empty body — `Json` still requires
         *     *some* valid JSON, so callers send `{}`) deserializes fine.
         */
        DeleteRoleRequest: {
            signature?: string | null;
            /** Format: uuid */
            signing_key_id?: string | null;
        };
        DenyResponse: {
            status: string;
        };
        DeviceGrantResponse: {
            device_label?: string | null;
            /** Format: date-time */
            expires_at: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            requested_at: string;
            /**
             * @description Base64-encoded — the approving device needs this exact value to
             *     reconstruct `device_grant_approval_signing_bytes` and sign it; the
             *     server never trusts a client-supplied copy of its own request back,
             *     but the *approver* is a different device that only ever learns this
             *     key by reading it back off this response.
             */
            requested_signing_public_key: string;
            status: string;
        };
        DeviceResponse: {
            /** Format: date-time */
            added_at: string;
            /** Format: uuid */
            id: string;
            label?: string | null;
            /**
             * @description Base64-encoded — lets a device that only knows its own local secret
             *     key (never sent anywhere) find which server-side row is *itself* by
             *     deriving and comparing its public key client-side, e.g. to learn
             *     its own `approver_signing_key_id` before approving someone else's
             *     grant. Public key material only; no risk in exposing it the same
             *     way `identity_created`'s payload already does.
             */
            public_key: string;
            /** Format: date-time */
            revoked_at?: string | null;
        };
        DiscoverGuildSummary: {
            /**
             * @description Issue #258: same already-public fields `GET /guilds/{id}` returns
             *     (#153/#246) — `null` when unset, no new visibility exposure.
             */
            banner?: string | null;
            /** Format: date-time */
            created_at: string;
            description: string;
            icon?: string | null;
            /** Format: uuid */
            id: string;
            /** Format: int64 */
            member_count: number;
            name: string;
            recruiting: boolean;
            tag: string;
        };
        DiscoverGuildsResponse: {
            guilds: components["schemas"]["DiscoverGuildSummary"][];
            /**
             * Format: uuid
             * @description `Some(id)` when another page exists — pass it back as `cursor=` to
             *     fetch it. `None` means this was the last page.
             */
            next_cursor?: string | null;
        };
        DiscoverPeopleResponse: {
            candidates: components["schemas"]["DiscoveryCandidate"][];
        };
        DiscoveryCandidate: {
            /** Format: uuid */
            identity_id: string;
        };
        EventResponse: {
            /** Format: uuid */
            channel_id?: string | null;
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            created_by: string;
            description?: string | null;
            /**
             * @description Issue #458. `false` when the caller has `view` but not
             *     `view_details` on this event: the event's existence is visible
             *     (`id`/`guild_id`/`title`/`starts_at`/`ends_at`/`created_by`/
             *     `created_at`/`public` are real), but `channel_id`/`description`/
             *     `rsvp_counts`/`my_rsvp` are placeholder values, not real data —
             *     never a 403, since existence itself is meant to stay visible.
             *     Always `true` for every event this module's other endpoints
             *     (create/update/RSVP) return, since those all require the actor to
             *     already hold `event_manage` or be RSVPing to their own record.
             */
            details_visible: boolean;
            /** Format: date-time */
            ends_at?: string | null;
            /** Format: uuid */
            guild_id: string;
            /** Format: uuid */
            id: string;
            /**
             * @description Issue #463. The caller's own RSVP status for this event, or `None`
             *     if they haven't RSVP'd — never another identity's. Lets a client
             *     pre-select `AvalonRsvpControl` correctly instead of always
             *     rendering unset, even after the caller has already responded.
             */
            my_rsvp?: string | null;
            /**
             * @description Issue #448. `false` (the default) keeps this event member-only even
             *     in a [`crate::guilds::GuildResponse::public`] guild.
             */
            public: boolean;
            rsvp_counts: components["schemas"]["RsvpCounts"];
            /** Format: date-time */
            starts_at: string;
            title: string;
        };
        /** @description One entry in a guild's favorite-integrators pin list, as read back. */
        FavoriteGameEntry: {
            /** Format: uuid */
            integrator_id: string;
            integrator_name: string;
            integrator_slug: string;
            /**
             * Format: int32
             * @description 0-indexed display order — the guild's curated ranking, not a
             *     popularity/member-count sort.
             */
            position: number;
            /**
             * @description True when this integrator no longer has any actively-bound guild member
             *     (per [`guild_bound_integrator_ids`]) — its last bound member left/unbound
             *     since the pin was added. Per #207's design, a stale pin is never
             *     auto-removed (that would churn the guild's public display on a
             *     single member's binding change); it's surfaced here so a
             *     `manage_guild` holder can choose to unpin it.
             */
            stale: boolean;
        };
        FavoriteGamesResponse: {
            favorites: components["schemas"]["FavoriteGameEntry"][];
            /** Format: uuid */
            guild_id: string;
        };
        FriendRequestResponse: {
            /** Format: uuid */
            from: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            requested_at: string;
            /** Format: uuid */
            to: string;
        };
        FriendshipResponse: {
            /** Format: uuid */
            a: string;
            /** Format: uuid */
            b: string;
            /** Format: date-time */
            since: string;
        };
        /**
         * @description One integrator's slice of a guild's integrator affinity breakdown: how many of the
         *     guild's current members hold an active [`IntegratorBinding`](avalon_protocol::integrators::IntegratorBinding)
         *     to it. Never includes an integrator with zero bound members — there's no
         *     "add" action here, only real binding data feeds this (see the module
         *     doc comment and `docs/architecture/guilds.md`).
         */
        GameBreakdownEntry: {
            /** Format: uuid */
            integrator_id: string;
            integrator_name: string;
            integrator_slug: string;
            /**
             * Format: int64
             * @description Distinct guild members with an active binding to this integrator.
             */
            member_count: number;
        };
        GameBreakdownResponse: {
            /**
             * @description No minimum-member threshold and no fixed cap — every integrator with at
             *     least one bound member appears, ordered by member count descending
             *     (ties broken alphabetically by name for a stable, readable order).
             *     This is a display of real counts, not a system verdict, per #160.
             */
            breakdown: components["schemas"]["GameBreakdownEntry"][];
            /** Format: uuid */
            guild_id: string;
            /**
             * Format: int64
             * @description Total current guild membership — the denominator for a
             *     "N of M members play X" display. Not the same as summing
             *     `breakdown[].member_count`, since a member can be bound to zero,
             *     one, or several integrators.
             */
            total_members: number;
        };
        /**
         * @description The fixed, small controlled vocabulary `Profile::favorite_genres` draws
         *     from (issue #155). Deliberately closed rather than free text — a bad
         *     value here is more likely a real client bug than a schema drift, so it is
         *     rejected server-side, not silently dropped (same reasoning
         *     `GuildPermission` already established in `crates/protocol/src/guilds.rs`).
         * @enum {string}
         */
        Genre: "action" | "adventure" | "rpg" | "strategy" | "simulation" | "puzzle" | "racing" | "sports" | "horror" | "sandbox" | "mmo" | "shooter" | "platformer" | "party";
        GuardianOfSummary: {
            /** Format: date-time */
            added_at: string;
            display_name: string;
            /** Format: uuid */
            identity_id: string;
        };
        GuardianRequestSummary: {
            already_approved: boolean;
            request: components["schemas"]["RecoveryRequestResponse"];
        };
        GuardianSettingsResponse: {
            guardian_ids: string[];
            /** Format: int32 */
            threshold: number;
            /** Format: date-time */
            updated_at?: string | null;
        };
        GuildAnnouncementAlert: {
            /** Format: uuid */
            author: string;
            body: string;
            /** Format: uuid */
            channel_id: string;
            channel_name: string;
            /** Format: uuid */
            guild_id: string;
            /** Format: uuid */
            message_id: string;
            /** Format: date-time */
            sent_at: string;
        };
        GuildInviteResponse: {
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            from: string;
            /** Format: uuid */
            guild_id: string;
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            to: string;
        };
        GuildJoinRequestResponse: {
            /** Format: uuid */
            applicant: string;
            /** Format: date-time */
            created_at: string;
            /** Format: date-time */
            decided_at?: string | null;
            /** Format: uuid */
            decided_by?: string | null;
            /** Format: uuid */
            guild_id: string;
            /** Format: uuid */
            id: string;
            message?: string | null;
            status: string;
        };
        /**
         * @description One entry in [`Guild::links`] (issue #153): a human label paired with the
         *     URL it points at. Both fields are validated/capped server-side
         *     (`crates/server/src/guilds.rs`) — this type carries no invariant of its
         *     own beyond "these are the two fields a link has."
         */
        GuildLink: {
            label: string;
            url: string;
        };
        /** @description Wire shape for one entry of `UpdateGuildRequest.links` (issue #153). */
        GuildLinkRequest: {
            label: string;
            url: string;
        };
        GuildMemberResponse: {
            /** Format: uuid */
            guild_id: string;
            /** Format: uuid */
            identity_id: string;
            /** Format: date-time */
            joined_at: string;
            /** Format: int32 */
            role_index: number;
        };
        GuildResponse: {
            /** @description Issue #153. */
            banner?: string | null;
            /** Format: date-time */
            created_at: string;
            description: string;
            /**
             * @description Issue #207. The guild's curated top-5 favorite integrators, in display
             *     order, each flagged `stale` if it no longer has an actively-bound
             *     member. Unlike `game_breakdown_public`'s full breakdown, this
             *     curated subset is always part of the guild's public profile — it's
             *     the guild's own deliberate choice of what to show, same "always
             *     public" treatment `links`/`motd` already get.
             */
            favorite_games: components["schemas"]["FavoriteGameEntry"][];
            /**
             * @description Issue #206. Whether the integrator affinity breakdown
             *     (`GET /guilds/{id}/integrator-breakdown`) is shown on this guild's public
             *     profile — a `manage_guild` holder can always fetch the breakdown
             *     regardless of this flag; it only gates exposure to everyone else.
             */
            game_breakdown_public: boolean;
            /** @description Issue #246. */
            icon?: string | null;
            /** Format: uuid */
            id: string;
            integrators: string[];
            join_policy: string;
            /** @description Issue #153. */
            links: components["schemas"]["GuildLink"][];
            /** Format: int64 */
            member_count: number;
            /** @description Issue #153. */
            motd?: string | null;
            name: string;
            /** Format: uuid */
            owner: string;
            /**
             * @description Issue #449 — see `avalon_protocol::guilds::Guild::public`'s doc
             *     comment. Independent of `recruiting`.
             */
            public: boolean;
            /** @description Issue #153. */
            recruiting: boolean;
            /**
             * @description Issue #87. `"public"`/`"guild_members"`/`"private"` — who may read
             *     `GET /guilds/{id}/members`. Defaults to `"guild_members"`.
             */
            roster_visibility: string;
            tag: string;
        };
        /**
         * @description One event from the caller's own protocol history (issue #121) — "what
         *     does the network know about me." `subject` is included since not every
         *     event an identity issues is *about* itself the same way (e.g.
         *     `friend.requested` is issued by the requester but its subject is the
         *     recipient); `payload` is passed through as-is rather than reduced to a
         *     canned summary string, matching this repo's general preference for
         *     exposing real data over a lossy client-unfriendly-format-agnostic gloss.
         */
        HistoryEntryResponse: {
            /** Format: uuid */
            event_id: string;
            kind: string;
            /**
             * @description `null` if this event's payload has been pruned locally (issue
             *     #208, a hot-tier node) — the event's existence and `kind` are still
             *     reported, just not its content.
             */
            payload: Record<string, never> | null;
            subject: string;
            /** Format: date-time */
            timestamp: string;
        };
        InitialKeyRequest: {
            algorithm: string;
            /** @description Standard-base64-encoded raw public key bytes. */
            public_key: string;
        };
        IntegratorChallengeResponse: {
            /** Format: uuid */
            challenge_id: string;
            /** Format: date-time */
            expires_at: string;
            /**
             * @description Standard-base64-encoded random nonce the integrator must sign with its
             *     registered key and echo back (see [`authenticate_integrator`]).
             */
            nonce: string;
        };
        IntegratorCredentialResponse: {
            /** Format: uuid */
            integrator_id: string;
            key_id: string;
        };
        IntegratorDataInstanceResponse: {
            id: string;
            instance: Record<string, never>;
            /** Format: uuid */
            integrator_id: string;
            /** Format: date-time */
            published_at: string;
            schema_id: string;
            /** Format: uuid */
            subject: string;
            superseded_by?: string | null;
        };
        IntegratorPublicResponse: {
            category: string;
            /** Format: uuid */
            id: string;
            name: string;
            owner_name: string;
            /** Format: date-time */
            registered_at: string;
            requested_capabilities: string[];
            slug: string;
            status: string;
        };
        IntegratorRegistryResponse: {
            achievements_issued: components["schemas"]["MetricResponse"];
            achievements_revoked: components["schemas"]["MetricResponse"];
            players: components["schemas"]["MetricResponse"];
            total_players_ever: components["schemas"]["MetricResponse"];
            unique_achievement_holders: components["schemas"]["MetricResponse"];
        };
        IntegratorResponse: {
            category: string;
            credential: components["schemas"]["IntegratorCredentialResponse"];
            /** Format: uuid */
            id: string;
            name: string;
            owner_name: string;
            /** Format: date-time */
            registered_at: string;
            requested_capabilities: string[];
            slug: string;
            status: string;
        };
        IntegratorSchemaMappingResponse: {
            description: string;
            field_correspondence: {
                [key: string]: string;
            };
            from_schema_id: string;
            id: string;
            /** Format: uuid */
            integrator_id: string;
            /** Format: date-time */
            published_at: string;
            to_schema_id: string;
        };
        IntegratorSchemaVersionResponse: {
            default_visibility: string;
            field_visibility: {
                [key: string]: string;
            };
            id: string;
            /** Format: uuid */
            integrator_id: string;
            proto_source: string;
            /** Format: date-time */
            published_at: string;
            superseded_by?: string | null;
            /** Format: int32 */
            version: number;
        };
        IntegratorSummary: {
            category: string;
            /** Format: uuid */
            id: string;
            name: string;
            owner_name: string;
            /** Format: date-time */
            registered_at: string;
            slug: string;
            status: string;
        };
        IntegratorWhoamiResponse: {
            /** Format: uuid */
            integrator_id: string;
        };
        IssueAttestationRequest: {
            /**
             * @description An optional, unverified pointer to supporting evidence (a replay
             *     id, a screenshot ref, whatever the issuer wants to attach) — carried
             *     through into the emitted event's payload only; not itself part of
             *     what's signed or stored as a column, since it's descriptive
             *     metadata, not something verification depends on.
             */
            evidence?: Record<string, never> | null;
            /**
             * Format: uuid
             * @description Which of the issuer's own keys signed this attestation — resolved
             *     against that issuer's full key history at the moment of issuance
             *     (#84's `resolve_valid_signing_key`), not assumed to be the key that
             *     authenticated this HTTP request.
             */
            key_id: string;
            /**
             * @description Standard-base64-encoded detached Ed25519 signature over
             *     [`attestation_signing_bytes`].
             */
            signature: string;
        };
        IssuerKeyResponse: {
            algorithm: string;
            /** Format: uuid */
            key_id: string;
            purpose: string;
            /** Format: date-time */
            revoked_at?: string | null;
            role: string;
            /** Format: date-time */
            valid_from: string;
            /** Format: date-time */
            valid_until?: string | null;
        };
        IssuerRegistrationResponse: {
            issuer_ref: string;
            /** Format: date-time */
            registered_at: string;
        };
        ListIntegratorsResponse: {
            integrators: components["schemas"]["IntegratorSummary"][];
            /**
             * Format: uuid
             * @description `Some(id)` when another page exists — pass it back as `cursor=` to
             *     fetch it. `None` means this was the last page.
             */
            next_cursor?: string | null;
        };
        ListMyAchievementsResponse: {
            achievements: components["schemas"]["AttestationReadResponse"][];
            /**
             * Format: uuid
             * @description `Some(id)` when another page exists — pass it back as `before=` to
             *     fetch it. `None` means this was the last page.
             */
            next_cursor?: string | null;
        };
        LocationsResponse: {
            locations: string[];
        };
        LookupCrossNodeLoginResponse: {
            /**
             * @description A real registered display name, only ever present when
             *     `integrator_verified` is `true` — `null` otherwise, so the
             *     approval screen has no ambiguous "empty string vs. never checked"
             *     state to handle.
             */
            display_name?: string | null;
            /** Format: int64 */
            expires_in: number;
            /**
             * @description Epic #623, issue #649, implementing #642's decided requirement:
             *     whether this node — the one the identity is being asked to log
             *     into — resolves to a real, registered integrator (or a known
             *     network anchor for the default shard). See
             *     [`resolve_requester_verification`]'s own doc comment for exactly
             *     what "verified" means here. `#639`/`#640` render this as a visual
             *     distinction, never a hard gate — an unverified requester still
             *     gets a prompt, just a clearly flagged one.
             */
            integrator_verified: boolean;
            requesting_context: string;
            /**
             * @description One of `pending`, `denied`, `expired`, `approved` — an approval
             *     screen only ever meaningfully acts on `pending`; the others let it
             *     show a clear "this code was already used/expired" state instead of
             *     a generic not-found.
             */
            status: string;
        };
        MessageResponse: {
            /** Format: uuid */
            author: string;
            body: string;
            /** Format: uuid */
            channel_id: string;
            /** Format: uuid */
            id: string;
            /** Format: date-time */
            sent_at: string;
        };
        MetricResponse: {
            class: string;
            definition: string;
            /**
             * @description Issue #96's minimum-cohort-size floor: `false` means `value` is the
             *     configured floor, not the real count — the real count is only known
             *     to be somewhere in `1..value`. A caller must render this as "fewer
             *     than `value`", never as an exact number, whenever `exact` is
             *     `false`.
             */
            exact: boolean;
            /** Format: int64 */
            value: number;
        };
        MyGrantsResponse: {
            capabilities: string[];
            /** Format: uuid */
            integrator_id: string;
        };
        MyGuildInviteResponse: {
            /** Format: date-time */
            created_at: string;
            /** Format: uuid */
            from: string;
            /** Format: uuid */
            guild_id: string;
            guild_name: string;
            /** Format: uuid */
            id: string;
        };
        MyGuildMembershipResponse: {
            /** Format: uuid */
            guild_id: string;
            /** Format: date-time */
            joined_at: string;
            /** Format: int32 */
            role_index: number;
        };
        PasskeyResponse: {
            /** Format: date-time */
            added_at: string;
            /** Format: uuid */
            id: string;
            label?: string | null;
        };
        PermissionOverrideResponse: {
            allow: boolean;
            /** Format: uuid */
            id: string;
            permission: string;
            /** Format: uuid */
            resource_id: string;
            resource_kind: string;
            /** Format: int32 */
            role_index: number;
        };
        PollCrossNodeLoginResponse: {
            /** Format: date-time */
            expires_at?: string | null;
            /** @description One of `pending`, `slow_down`, `denied`, `expired`, `approved`. */
            status: string;
            token?: string | null;
        };
        PollPairingResponse: {
            /** Format: date-time */
            expires_at?: string | null;
            /** @description One of `pending`, `slow_down`, `denied`, `expired`, `approved`. */
            status: string;
            token?: string | null;
        };
        PresenceResponse: {
            /** Format: uuid */
            active_in?: string | null;
            /** Format: uuid */
            identity_id: string;
            status: components["schemas"]["PresenceStatus"];
            /** Format: date-time */
            updated_at: string;
        };
        /**
         * @description `Online` is live/automatic: it reflects heartbeat/TTL state and can't be
         *     "stuck" on. `Away`, `DoNotDisturb`, and `Offline` are sticky manual
         *     overrides when set explicitly via `PUT /me/presence` — they persist
         *     (ignoring TTL expiry) until the caller explicitly sets `Online` again.
         *     See `crates/server/src/presence.rs::PresenceStore::get`.
         * @enum {string}
         */
        PresenceStatus: "Online" | "Away" | "DoNotDisturb" | "Offline";
        ProfileResponse: {
            avatar_url?: string | null;
            /**
             * @description Expanded self-described fields (issue #372) — same promised-durable
             *     tier and same public exposure level as `bio`/`favorite_genres`/
             *     `pronouns` above.
             */
            banner_url?: string | null;
            /**
             * @description Small, user-optional self-description fields (issue #155) — same
             *     promised-durable tier and same public exposure level as
             *     `display_name`/`avatar_url` above (no capability gate, no integrator ever
             *     sees more of it than `GET /me`/`GET /identities/profiles` already
             *     expose).
             */
            bio?: string | null;
            /**
             * @description Issue #205's opt-in global search toggle — `true` means this
             *     identity currently matches `GET /identities/search`. Surfaced here
             *     (rather than requiring a separate read) so the Hub's "you are
             *     currently publicly searchable" indicator never drifts out of sync
             *     with the actual `discovery_preferences` row — same reasoning
             *     `handle` is derived rather than separately fetched.
             */
            discoverable: boolean;
            /**
             * @description Issue #510: this identity's globally-unique, case-insensitive
             *     handle in its own right — no separate `handle`/discriminator field
             *     exists any more (issue #128's old scheme). Add-friend-by-handle
             *     resolves this field directly.
             */
            display_name: string;
            /**
             * Format: uuid
             * @description `main_guild` if explicitly set, otherwise the guild this identity
             *     joined earliest (by `guild_members.joined_at`), computed at read
             *     time and never stored — `None` only when the identity has no guild
             *     memberships at all. This, not `main_guild`, is what an integrator
             *     building a single-guild UI should read.
             */
            effective_main_guild?: string | null;
            favorite_genres: components["schemas"]["Genre"][];
            /** Format: date-time */
            identity_created_at: string;
            /** Format: uuid */
            identity_id: string;
            links: string[];
            /**
             * @description Self-described only — never IP-derived or geocoded. See
             *     `avalon_protocol::identity::Profile::location`'s doc comment.
             */
            location?: string | null;
            /**
             * Format: uuid
             * @description A self-chosen pointer to one of this identity's own current guild
             *     memberships (no ticket — see
             *     `avalon_protocol::identity::Profile::main_guild`'s doc comment).
             *     `None` means "not explicitly set," not "no guild" — see
             *     `effective_main_guild` below for the resolved value a UI should
             *     actually build around.
             */
            main_guild?: string | null;
            /**
             * @description Issue #87 — this identity's own presence-visibility setting
             *     (`"public"`/`"authenticated_only"`/`"friends"`/`"guild_members"`/`"private"`).
             *     Self-only, same posture `discoverable` takes — see this endpoint's
             *     own doc comment on why `PublicIdentityProfileResponse` omits both.
             */
            presence_visibility: string;
            pronouns?: string | null;
            status?: string | null;
            theme_color?: string | null;
            timezone?: string | null;
        };
        /**
         * @description Another identity's full self-description profile — issue #403's decided
         *     widening of #393's read-only profile card. Deliberately a **separate,
         *     single-identity endpoint** rather than a widened `list_profiles`: the
         *     batch endpoint above stays exactly as narrow as it already is (any
         *     session can resolve arbitrarily many ids at once, so it only ever
         *     returns the least-sensitive public-face fields), while this endpoint
         *     exposes the same fields `GET /me` already does — `bio`/`favorite_genres`/
         *     `pronouns` (#155) and `banner_url`/`status`/`links`/`timezone`/
         *     `theme_color`/`location` (#372) — but only for one identity per request,
         *     matching a real profile-card view rather than a roster resolve. Omits
         *     `discoverable` and `presence_visibility`: both describe the *viewed*
         *     identity's own settings preferences, not something the viewer needs
         *     once they've already found the profile.
         */
        PublicIdentityProfileResponse: {
            avatar_url?: string | null;
            banner_url?: string | null;
            bio?: string | null;
            display_name: string;
            /** Format: uuid */
            effective_main_guild?: string | null;
            favorite_genres: components["schemas"]["Genre"][];
            /** Format: date-time */
            identity_created_at: string;
            /** Format: uuid */
            identity_id: string;
            links: string[];
            location?: string | null;
            /** Format: uuid */
            main_guild?: string | null;
            pronouns?: string | null;
            status?: string | null;
            theme_color?: string | null;
            timezone?: string | null;
        };
        /**
         * @description Another identity's *public* profile fields only — never anything a
         *     stranger couldn't already learn via `friends::resolve_handle`'s
         *     name-to-id lookup run in reverse. No bio, no email, nothing beyond what
         *     `display_name`/`avatar_url` already are: the least-sensitive public-face
         *     fields. This endpoint has no further visibility gating (any session can
         *     batch-resolve arbitrary identity ids), so `bio`/`favorite_genres`/
         *     `pronouns` (#155) and `banner_url`/`status`/`links`/`timezone`/
         *     `theme_color`/`location` (#372) are deliberately withheld here even
         *     though they're unauthenticated-readable on one's own `GET /me` — batch
         *     stranger lookup is a materially wider exposure than a single
         *     self-disclosed profile view, and widening it is a scoping decision for
         *     its own ticket, not a side effect of adding the columns.
         */
        PublicProfileResponse: {
            avatar_url?: string | null;
            display_name: string;
            /** Format: uuid */
            identity_id: string;
        };
        PublishInstanceRequest: {
            instance: Record<string, never>;
            /** Format: uuid */
            subject: string;
        };
        PublishIntegratorSchemaMappingRequest: {
            /**
             * @description Free text documenting whatever `field_correspondence` can't
             *     capture (merges, splits, dropped fields, default values).
             */
            description?: string;
            /**
             * @description A simple old-field -> new-field correspondence map. Never
             *     interpreted or executed — see module doc comment.
             */
            field_correspondence?: {
                [key: string]: string;
            };
            /**
             * @description The schema version this mapping maps *from* — must be a real,
             *     already-published version owned by the same integrator publishing
             *     the mapping.
             */
            from_schema_id: string;
            /**
             * @description The schema version this mapping maps *to* — same ownership/
             *     existence requirement as `from_schema_id`.
             */
            to_schema_id: string;
        };
        PublishIntegratorSchemaVersionRequest: {
            /**
             * @description `"public"` (default) or `"private"` — #381's schema-level opt-out.
             *     Omitted entirely by a pre-#384 publisher, which keeps today's
             *     fully-open behavior.
             */
            default_visibility?: string;
            /**
             * @description Field name -> `"public"`/`"private"`, overriding `default_visibility`
             *     for that field specifically, in either direction (#381). Every key
             *     must name a real field of the parsed root message — see
             *     `proto_schema::validate_field_visibility_keys`.
             */
            field_visibility?: {
                [key: string]: string;
            };
            /**
             * @description Raw `.proto` source text — parsed for real as of #384 (see
             *     `crate::proto_schema`), no longer stored opaque. Must declare
             *     exactly one top-level `message`, which becomes this schema's root
             *     type for both `field_visibility` validation here and instance
             *     validation in `crate::integrator_data`.
             */
            proto_source: string;
        };
        PublishRecognitionRequest: {
            recognized_slug: string;
            scope: string[];
        };
        RecognitionResponse: {
            /** Format: date-time */
            published_at: string;
            recognized_slug: string;
            recognizer_slug: string;
            scope: string[];
        };
        RecoveryFinishRequest: {
            /** Format: uuid */
            ticket_id: string;
            webauthn_credential: Record<string, never>;
        };
        RecoveryRequestResponse: {
            /** Format: int64 */
            approvals_count: number;
            /** Format: date-time */
            delay_ends_at?: string | null;
            /** Format: uuid */
            id: string;
            /** Format: uuid */
            identity_id: string;
            /** Format: date-time */
            requested_at: string;
            status: string;
            /** Format: int32 */
            threshold: number;
        };
        RecoveryStartRequest: {
            device_label?: string | null;
            /** Format: uuid */
            identity_id: string;
        };
        RecoveryStartResponse: {
            challenge: Record<string, never>;
            /** Format: uuid */
            ticket_id: string;
        };
        RegisterFinishRequest: {
            /**
             * @description A user-chosen label for the device completing this ceremony (e.g.
             *     "Work laptop") — purely descriptive, never part of what's signed.
             *     Issue #145: previously the first device's `identity_signing_keys`
             *     row was always unlabeled, unlike every device added later through
             *     #135's grant flow (which does carry a label).
             */
            device_label?: string | null;
            /**
             * @description Base64-encoded Ed25519 signature over
             *     `identity_created_signing_bytes(identity_id, display_name)`.
             */
            event_signature: string;
            /**
             * @description Base64-encoded raw Ed25519 public key — the identity's event-signing
             *     key, distinct from the WebAuthn passkey above. See module docs.
             */
            event_signing_public_key: string;
            /** Format: uuid */
            ticket_id: string;
            webauthn_credential: Record<string, never>;
        };
        RegisterFinishResponse: {
            /** Format: uuid */
            identity_id: string;
        };
        RegisterIssuerRequest: {
            /** Format: uuid */
            challenge_id: string;
            declared_network_id: string;
            issuer_pubkey: string;
            issuer_ref: string;
            proof_of_possession_signature: string;
        };
        RegisterStartRequest: {
            display_name: string;
            /**
             * Format: uuid
             * @description Client-chosen, not server-assigned — identity is a wallet its holder
             *     creates themselves. Must also become the WebAuthn user handle, which
             *     is why it has to be decided here rather than at `finish`.
             */
            identity_id: string;
        };
        RegisterStartResponse: {
            /**
             * @description `webauthn-rs`'s own WebAuthn creation-challenge type — opaque here
             *     since it's an external crate's type with no `ToSchema` impl of its
             *     own; the real, authoritative shape is `webauthn-rs`'s
             *     `CreationChallengeResponse`, not this placeholder.
             */
            challenge: Record<string, never>;
            /** Format: uuid */
            ticket_id: string;
        };
        RegistrationChallengeRequest: {
            /** @description Standard-base64-encoded Ed25519 public key bytes. */
            issuer_pubkey: string;
        };
        RegistrationChallengeResponse: {
            /** Format: uuid */
            challenge_id: string;
            /** Format: date-time */
            expires_at: string;
            /**
             * @description Standard-base64-encoded random nonce — signed (as part of a larger
             *     canonical message, see [`proof_of_possession_message`]) and echoed
             *     back via [`register_issuer`].
             */
            nonce: string;
        };
        RenameDeviceRequest: {
            label: string;
        };
        RenamePasskeyRequest: {
            label: string;
        };
        RequestDeviceGrantRequest: {
            device_label?: string | null;
            /**
             * @description Base64-encoded raw Ed25519 public key — freshly generated
             *     client-side for this device, never persisted locally until this
             *     grant is approved.
             */
            requested_signing_public_key: string;
        };
        ResolveHandleResponse: {
            /** Format: uuid */
            identity_id: string;
        };
        ResolvePairingResponse: {
            status: string;
        };
        RevocationResponse: {
            /** Format: uuid */
            attestation_id: string;
            reason: string;
            reason_code: string;
            /** Format: date-time */
            revoked_at: string;
        };
        RevokeAttestationRequest: {
            /** Format: uuid */
            key_id: string;
            reason: string;
            /**
             * @description Issue #534: a real, extensible vocabulary (`RevocationReasonCode`),
             *     not a free-text string — see that type's own doc comment. Still
             *     deserializes from a plain JSON string, so no wire-format change
             *     for existing callers; an unrecognized code decodes to `Other`
             *     rather than a request error. Serializes/deserializes as a plain
             *     string via hand-written `serde` impls, so it has no `ToSchema` of
             *     its own — represented here as `String`.
             */
            reason_code: string;
            /**
             * @description Standard-base64-encoded detached Ed25519 signature over
             *     [`revocation_signing_bytes`].
             */
            signature: string;
        };
        RevokeIssuerKeyRequest: {
            reason?: string | null;
        };
        /**
         * @description #697/#698: replaces the old `?confirm=true` speed bump for the
         *     last-passkey case with a fresh-signature requirement (the ticket's own
         *     upgrade from "confirm" to "sign") — `signing_key_id`/`signature` are
         *     only enforced when this revoke would leave zero passkeys, per
         *     [`needs_fresh_signature`]. Revoking one of several passkeys stays
         *     unsigned/ambient and these fields go unused.
         */
        RevokePasskeyRequest: {
            signature?: string | null;
            /** Format: uuid */
            signing_key_id?: string | null;
        };
        RevokeRecognitionRequest: {
            recognized_slug: string;
        };
        /**
         * @description A role's small, fixed visual identity (issue #152): an icon id from a
         *     closed enum paired with a color id from a closed enum — deliberately
         *     not a free-form asset/upload, no user-supplied image hosting in scope
         *     for milestone 1. Shaped as icon+color today (rather than e.g. a single
         *     opaque badge id) so it can grow into a richer badge system later —
         *     more icons/colors, tiers, an uploaded custom asset as an additional
         *     variant — without a breaking change to callers that just want "an icon
         *     and a color" out of a role (`packages/ui`'s planned `AvalonRoleBadge`,
         *     #24, is the first such caller).
         */
        RoleBadge: {
            color: components["schemas"]["RoleBadgeColor"];
            icon: components["schemas"]["RoleBadgeIcon"];
        };
        /**
         * @description Fixed milestone-1 vocabulary of role badge colors — same closed-set
         *     reasoning as [`RoleBadgeIcon`].
         * @enum {string}
         */
        RoleBadgeColor: "gray" | "red" | "orange" | "gold" | "green" | "blue" | "purple";
        /**
         * @description Fixed milestone-1 vocabulary of role badge icons (issue #152). Not
         *     user-uploadable — a role's icon is chosen from this closed set, same
         *     "custom names allowed, custom permissions/values not yet" precedent
         *     [`GuildPermission`] already established for milestone 1.
         * @enum {string}
         */
        RoleBadgeIcon: "shield" | "crown" | "star" | "sword" | "wrench" | "heart" | "flag" | "bolt";
        /**
         * @description Wire shape for a badge in a create/update role request — plain strings
         *     rather than deserializing straight into `RoleBadgeIcon`/`RoleBadgeColor`,
         *     so an unrecognized id goes through the same explicit
         *     validate-and-reject path (`AppError::InvalidRoleBadge`) as every other
         *     guild input in this module, instead of a generic JSON-deserialization
         *     rejection a caller can't distinguish from a malformed request body.
         */
        RoleBadgeRequest: {
            color: string;
            icon: string;
        };
        RoleResponse: {
            badge: components["schemas"]["RoleBadge"];
            description: string;
            name: string;
            /** Format: int32 */
            name_index: number;
            permissions: string[];
        };
        RsvpCounts: {
            /** Format: int64 */
            going: number;
            /** Format: int64 */
            maybe: number;
            /** Format: int64 */
            not_going: number;
        };
        RsvpRequest: {
            status: string;
        };
        RsvpResponse: {
            /** Format: uuid */
            event_id: string;
            /** Format: uuid */
            identity_id: string;
            /** Format: date-time */
            responded_at: string;
            status: string;
        };
        RsvpRosterEntry: {
            /** Format: uuid */
            identity_id: string;
            /** Format: date-time */
            responded_at: string;
            status: string;
        };
        SearchIdentitiesResponse: {
            results: components["schemas"]["SearchResultIdentity"][];
        };
        SearchResultIdentity: {
            avatar_url?: string | null;
            display_name: string;
            /** Format: uuid */
            identity_id: string;
        };
        SendMessageRequest: {
            body: string;
        };
        SessionFinishRequest: {
            credential: Record<string, never>;
            /** Format: uuid */
            ticket_id: string;
        };
        SessionFinishResponse: {
            /** Format: date-time */
            expires_at: string;
            token: string;
        };
        SessionStartRequest: {
            /** Format: uuid */
            identity_id: string;
        };
        SessionStartResponse: {
            challenge: Record<string, never>;
            /** Format: uuid */
            ticket_id: string;
        };
        SetFavoriteGamesRequest: {
            /**
             * @description The full desired ordered list of pinned integrator ids — always a full
             *     replace, never a per-entry patch, same "resend the whole list"
             *     convention `UpdateGuildRequest::links` already established for #153.
             *     Position in this array is the new display order.
             */
            integrator_ids: string[];
        };
        SetGuardiansRequest: {
            guardian_ids: string[];
            signature?: string | null;
            /**
             * Format: uuid
             * @description #697/#698: only required when this write *removes* an existing
             *     guardian or *raises* the threshold — see the conditional check in
             *     `set_guardians` below. Naming/adding guardians or lowering the
             *     threshold stays ambient.
             */
            signing_key_id?: string | null;
            /** Format: int32 */
            threshold: number;
        };
        SetPermissionOverrideRequest: {
            allow: boolean;
            permission: string;
            /** Format: uuid */
            resource_id: string;
            resource_kind: string;
            /** Format: int32 */
            role_index: number;
            signature?: string | null;
            /**
             * Format: uuid
             * @description #697/#698: changes what an entire role/resource can do guild-wide —
             *     signature-required.
             */
            signing_key_id?: string | null;
        };
        StartCrossNodeLoginResponse: {
            /** Format: int64 */
            expires_in: number;
            /** Format: int64 */
            poll_interval: number;
            request_code: string;
            requesting_context: string;
            user_code: string;
        };
        StartPairingResponse: {
            device_code: string;
            /** Format: int64 */
            expires_in: number;
            /** Format: int64 */
            poll_interval: number;
            user_code: string;
            verification_uri: string;
        };
        SubmitGrantRequest: {
            grant: components["schemas"]["CrossNodeLoginGrant"];
            /**
             * @description Present for the cross-device flow: which pending `start`ed request
             *     this grant resolves. `None` for the same-device fast path, which
             *     mints a session directly with no pending row at all.
             */
            user_code?: string | null;
        };
        SubmitGrantResponse: {
            /** Format: date-time */
            expires_at?: string | null;
            /**
             * @description Present only on the same-device fast path (`user_code: None`) — the
             *     cross-device path's session token is picked up via `poll`, same as
             *     `device_pairing::approve_pairing`, not returned here directly.
             */
            token?: string | null;
        };
        TransferOwnershipRequest: {
            signature?: string | null;
            /**
             * Format: uuid
             * @description #704's gap #2 / #697/#698: permanently hands another identity full
             *     ownership — signature-required.
             */
            signing_key_id?: string | null;
            /** Format: uuid */
            to: string;
        };
        UpdateAchievementDefinitionRequest: {
            description?: string | null;
            /**
             * @description One of [`BUILTIN_ICONS`], `Some(None)`-style clearing is not
             *     supported (omit the field to leave it untouched, same "absent means
             *     untouched" convention every other field here already uses).
             */
            icon?: string | null;
            /** @description An integrator-hosted image URL; `http`/`https` only. */
            icon_url?: string | null;
            name?: string | null;
            /**
             * @description Set to `true` to retire the definition (see module doc comment).
             *     Never used to un-retire — retirement is one-way.
             */
            retired?: boolean | null;
            /** @description See `CreateAchievementDefinitionRequest::schema`'s doc comment. */
            schema: string | null;
        };
        UpdateChannelRequest: {
            /**
             * @description Issue #250. `None` leaves the existing value untouched, same
             *     partial-update convention `UpdateRoleRequest` uses.
             */
            announcement_only?: boolean | null;
            name: string;
            /**
             * @description Issue #458. `None` leaves the existing value untouched, same
             *     convention as `announcement_only` above.
             */
            public?: boolean | null;
            /**
             * @description Issue #276. `None` leaves the existing value untouched; `Some("")`
             *     (after trimming) clears it — same three-state convention
             *     `crate::guilds::UpdateGuildRequest::motd` already uses.
             */
            topic?: string | null;
        };
        UpdateEventRequest: {
            /** Format: uuid */
            channel_id?: string | null;
            description?: string | null;
            /** Format: date-time */
            ends_at?: string | null;
            /**
             * @description Issue #448. Full replace like the rest of this request — always
             *     resent, not three-state.
             */
            public?: boolean;
            /** Format: date-time */
            starts_at: string;
            title: string;
        };
        UpdateGuildMemberRequest: {
            /** Format: int32 */
            role_index: number;
            signature?: string | null;
            /**
             * Format: uuid
             * @description #697/#698: only required when the new role grants `manage_roles` or
             *     `manage_members` (an escalation) — see the conditional check in
             *     `update_member_role` below.
             */
            signing_key_id?: string | null;
        };
        UpdateGuildRequest: {
            /**
             * @description Issue #153. Same three-state convention as `motd`, same
             *     `http`/`https`-URL validation as a profile's `avatar_url`.
             */
            banner?: string | null;
            description?: string | null;
            /**
             * @description Issue #206. Omitted leaves it untouched. Controls only whether the
             *     integrator affinity breakdown is shown on this guild's *public* profile —
             *     a `manage_guild` holder can always see it internally either way.
             */
            game_breakdown_public?: boolean | null;
            /**
             * @description Issue #246. Same three-state convention as `banner`, same
             *     `http`/`https`-URL validation.
             */
            icon?: string | null;
            /**
             * @description "invite_only" or "open" (see [`JoinPolicy`]) — omitted leaves it
             *     untouched. `Open` lets any authenticated identity join instantly via
             *     `POST /guilds/{id}/join` (`can_join_directly`/`join_guild`), bypassing
             *     the invite (#21) and join-request/approval (#242) flows entirely.
             */
            join_policy?: string | null;
            /**
             * @description Issue #153. Two states, not three: omitted (untouched) or
             *     `Some(list)`, which always fully replaces the stored list —
             *     including `Some(vec![])` to clear it. Each entry is validated; an
             *     invalid entry rejects the whole request rather than being dropped.
             */
            links?: components["schemas"]["GuildLinkRequest"][] | null;
            /**
             * @description Issue #153. Three states, same as `UpdateProfileRequest::bio`:
             *     omitted (untouched), `Some("")` (clear to `NULL`), `Some(nonempty)`
             *     (validate against [`MAX_GUILD_MOTD_LEN`], then set).
             */
            motd?: string | null;
            name?: string | null;
            /**
             * @description Issue #449. Omitted leaves it untouched. Independent of
             *     `recruiting` — see `avalon_protocol::guilds::Guild::public`'s doc
             *     comment.
             */
            public?: boolean | null;
            /** @description Issue #153. Omitted leaves it untouched. */
            recruiting?: boolean | null;
            /**
             * @description Issue #87. `"public"` (anyone), `"guild_members"` (only current
             *     members), or `"private"` (nobody, via this endpoint, but a
             *     `manage_guild` holder — see `update_guild`'s own permission check —
             *     can always change it back). Omitted leaves it untouched.
             */
            roster_visibility?: string | null;
            tag?: string | null;
        };
        UpdateIntegratorPresenceRequest: {
            /**
             * Format: uuid
             * @description Must be the calling integrator's own id, or omitted/`null` — see
             *     [`validate_integrator_playing`].
             */
            active_in?: string | null;
            status: components["schemas"]["PresenceStatus"];
        };
        UpdatePresenceRequest: {
            /**
             * @description Opt out of (or back into) `active_in` ever being shown, independent
             *     of any integrator's `presence.publish` grant. `None` leaves the existing
             *     preference untouched — this endpoint publishes a status on every
             *     call, but the caller doesn't have to re-state its opt-out choice
             *     every heartbeat.
             */
            hide_active_in?: boolean | null;
            status: components["schemas"]["PresenceStatus"];
        };
        UpdateProfileRequest: {
            avatar_url?: string | null;
            /**
             * @description Three states, same as `avatar_url` — a second image slot, separate
             *     from the avatar, for the Hub profile page header (issue #372).
             */
            banner_url?: string | null;
            /**
             * @description Three states, same as `avatar_url`: omitted (untouched), `Some("")`
             *     (clear to `NULL`), `Some(nonempty)` (validate against
             *     [`MAX_BIO_LEN`], then set).
             */
            bio?: string | null;
            /**
             * @description Issue #205's opt-in global search toggle. Two states, not three
             *     (there's no "clear" state for a plain boolean): `None` leaves the
             *     existing preference untouched, `Some(bool)` sets it. Off by
             *     default for every identity (no row in `discovery_preferences` at
             *     all reads as `false`) — this field is the only way it ever
             *     becomes `true`. Not part of `profile.updated`/durable history, and
             *     not written through the same transaction as the rest of this
             *     request's changes — see `discovery::set_discoverable`'s doc
             *     comment for why, matching `presence_preferences.hide_active_in`'s
             *     identical precedent.
             */
            discoverable?: boolean | null;
            display_name?: string | null;
            /**
             * @description Two states, not three: omitted (untouched) or `Some(list)`, which
             *     always fully replaces the stored list — including `Some(vec![])` to
             *     clear it. Each entry must parse as a [`Genre`]; an unknown value is
             *     rejected outright rather than silently dropped (issue #155).
             */
            favorite_genres?: string[] | null;
            /**
             * @description Two states, not three: omitted (untouched) or `Some(list)`, which
             *     always fully replaces the stored list — including `Some(vec![])` to
             *     clear it. Same shape as `favorite_genres`, but each entry is a
             *     free-form URL rather than a fixed vocabulary value (issue #372).
             */
            links?: string[] | null;
            /**
             * @description Three states, same as `bio`. Self-described free text only — never
             *     IP-derived or geocoded.
             */
            location?: string | null;
            /**
             * @description Three states, same as `bio`: omitted (untouched), `""` (clear to
             *     `NULL`), or a guild id string. Unlike every other three-state field
             *     here, a non-empty value also needs a database check — it must name
             *     a guild `identity_id` is currently a member of (`AppError::NotGuildMember`
             *     otherwise) — so its validation lives in `validate_main_guild` rather
             *     than one of the pure `validate_*` functions above.
             */
            main_guild?: string | null;
            /**
             * @description Issue #87. `"public"`/`"authenticated_only"`/`"friends"` (the
             *     default)/`"private"` — who may read this identity's presence via
             *     `GET /presence`. `"guild_members"` is accepted (presence has no
             *     guild context, so it behaves like `"private"` — nobody but the
             *     subject — see `crate::presence::presence_visible`). Same
             *     not-durable-history treatment as `discoverable`: applied outside
             *     this request's transaction, no `profile.updated` payload entry.
             */
            presence_visibility?: string | null;
            /** @description Three states, same as `bio`. */
            pronouns?: string | null;
            /**
             * @description Three states, same as `bio`, capped at
             *     [`avalon_protocol::identity::MAX_STATUS_LEN`].
             */
            status?: string | null;
            /**
             * @description Three states, same as `bio`. Must match `^#[0-9a-fA-F]{6}$` when
             *     non-empty.
             */
            theme_color?: string | null;
            /**
             * @description Three states, same as `bio`. Length-checked only, not validated
             *     against the real IANA time zone database — see
             *     `avalon_protocol::identity::Profile::timezone`'s doc comment.
             */
            timezone?: string | null;
        };
        UpdateRoleRequest: {
            badge?: null | components["schemas"]["RoleBadgeRequest"];
            /**
             * @description Issue #152. `None` leaves the existing description untouched — same
             *     partial-update convention `name`/`permissions` already use.
             */
            description?: string | null;
            name?: string | null;
            permissions?: string[] | null;
            signature?: string | null;
            /**
             * Format: uuid
             * @description #697/#698: role definitions are the guild's permission structure —
             *     signature-required.
             */
            signing_key_id?: string | null;
        };
        UserCodeRequest: {
            user_code: string;
        };
        ValidityResponse: {
            /** @enum {string} */
            status: "valid";
        } | {
            reason: string;
            /** @enum {string} */
            status: "invalid";
        };
        VisibleIntegratorDataInstanceResponse: {
            /**
             * @description Only the fields the instance's schema currently makes visible —
             *     see `resolve_visible_fields`.
             */
            fields: Record<string, never>;
            /** Format: uuid */
            integrator_id: string;
            /** Format: date-time */
            published_at: string;
            schema: string;
        };
    };
    responses: never;
    parameters: never;
    requestBodies: never;
    headers: never;
    pathItems: never;
}
export type $defs = Record<string, never>;
export interface operations {
    get_attestation: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AttestationReadResponse"];
                };
            };
        };
    };
    revoke_attestation: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RevokeAttestationRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RevocationResponse"];
                };
            };
        };
    };
    deny: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UserCodeRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DenyResponse"];
                };
            };
        };
    };
    lookup: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                user_code: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LookupCrossNodeLoginResponse"];
                };
            };
        };
    };
    poll: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PollCrossNodeLoginResponse"];
                };
            };
        };
    };
    start: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StartCrossNodeLoginResponse"];
                };
            };
        };
    };
    submit: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SubmitGrantRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SubmitGrantResponse"];
                };
            };
        };
    };
    approve_pairing: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ApprovePairingRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ResolvePairingResponse"];
                };
            };
        };
    };
    deny_pairing: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UserCodeRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ResolvePairingResponse"];
                };
            };
        };
    };
    poll_pairing: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PollPairingResponse"];
                };
            };
        };
    };
    start_pairing: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["StartPairingResponse"];
                };
            };
        };
    };
    list_blocks: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BlockListEntry"][];
                };
            };
        };
    };
    create_block: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateBlockRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BlockResponse"];
                };
            };
        };
    };
    remove_block: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                identity_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Block removed */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    list_my_conversations: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationResponse"][];
                };
            };
        };
    };
    create_conversation: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateConversationRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationResponse"];
                };
            };
        };
    };
    chat_list_messages: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                /**
                 * @description Cursor: a message id already seen by the caller. Results are the
                 *     next page strictly older than it (by `sent_at`, `id` as tiebreak) —
                 *     same cursor style as `guild_messages::ListMessagesQuery`.
                 */
                before: string | null;
                limit: number | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationMessageResponse"][];
                };
            };
        };
    };
    chat_send_message: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ConversationSendMessageRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConversationMessageResponse"];
                };
            };
        };
    };
    list_friends: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FriendshipResponse"][];
                };
            };
        };
    };
    resolve_handle: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                handle: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ResolveHandleResponse"];
                };
            };
        };
    };
    list_friend_requests: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FriendRequestResponse"][];
                };
            };
        };
    };
    create_friend_request: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateFriendRequestRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FriendRequestResponse"];
                };
            };
        };
    };
    decline_or_withdraw_friend_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Friend request declined or withdrawn */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    accept_friend_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FriendshipResponse"];
                };
            };
        };
    };
    remove_friend: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                identity_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Friendship removed */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    create_guild: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateGuildRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildResponse"];
                };
            };
        };
    };
    discover_guilds: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description Free-text search over `name`/`tag`/`description` (case-insensitive
                 *     substring).
                 */
                q: string | null;
                /**
                 * @description `true`/`false` to filter exactly; omitted falls back to the
                 *     default-visibility rule documented on [`discover_guilds`].
                 */
                recruiting: boolean | null;
                /** @description Case-insensitive exact match on `guilds.tag`. */
                tag: string | null;
                /**
                 * @description Filter to guilds associated (issue #20's `associate_integrator`) with this
                 *     integrator id.
                 */
                integrator: string | null;
                /** @description `newest` (default) | `alphabetical` | `most_members`. */
                sort: string | null;
                limit: number | null;
                /**
                 * @description The last guild id from the previous page's results — see the module
                 *     doc comment for why this is a bare id (same shape as
                 *     `guild_messages::ListMessagesQuery::before`) rather than an opaque
                 *     blob.
                 */
                cursor: string | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DiscoverGuildsResponse"];
                };
            };
        };
    };
    get_guild: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildResponse"];
                };
            };
        };
    };
    update_guild: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateGuildRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildResponse"];
                };
            };
        };
    };
    list_channels: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ChannelResponse"][];
                };
            };
        };
    };
    create_channel: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateChannelRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ChannelResponse"];
                };
            };
        };
    };
    update_channel: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                cid: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateChannelRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ChannelResponse"];
                };
            };
        };
    };
    archive_channel: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                cid: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ChannelResponse"];
                };
            };
        };
    };
    guilds_list_messages: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                cid: string;
                /**
                 * @description Cursor: a message id already seen by the caller. Results are the
                 *     next page strictly older than it (by `sent_at`, `id` as tiebreak).
                 */
                before: string | null;
                limit: number | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MessageResponse"][];
                };
            };
        };
    };
    guilds_send_message: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                cid: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SendMessageRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MessageResponse"];
                };
            };
        };
    };
    list_archive: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                cid: string;
                /**
                 * @description Cursor: a message id already seen by the caller. Results are the
                 *     next page strictly older than it (by `sent_at`, `id` as tiebreak).
                 */
                before: string | null;
                limit: number | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ArchivedMessageResponse"][];
                };
            };
        };
    };
    delete_message: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                cid: string;
                mid: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Message deleted */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    list_events: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                /** @description Inclusive lower bound on `starts_at`. */
                from: string | null;
                /** @description Inclusive upper bound on `starts_at`. */
                to: string | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EventResponse"][];
                };
            };
        };
    };
    create_event: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateEventRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EventResponse"];
                };
            };
        };
    };
    delete_event: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                eid: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Event deleted */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    update_event: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                eid: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateEventRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["EventResponse"];
                };
            };
        };
    };
    upsert_rsvp: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                eid: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RsvpRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RsvpResponse"];
                };
            };
        };
    };
    list_rsvps: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                eid: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RsvpRosterEntry"][];
                };
            };
        };
    };
    list_favorite_games: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FavoriteGamesResponse"];
                };
            };
        };
    };
    set_favorite_games: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetFavoriteGamesRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["FavoriteGamesResponse"];
                };
            };
        };
    };
    associate_integrator: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                integrator_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildResponse"];
                };
            };
        };
    };
    game_breakdown: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GameBreakdownResponse"];
                };
            };
        };
    };
    create_invite: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateGuildInviteRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildInviteResponse"];
                };
            };
        };
    };
    accept_invite: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                invite_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildMemberResponse"];
                };
            };
        };
    };
    decline_invite: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                invite_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Invite declined */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    join_guild: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildMemberResponse"];
                };
            };
        };
    };
    list_join_requests: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                /** @description Defaults to `pending`-only; pass `all` to include every status. */
                status: string | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildJoinRequestResponse"][];
                };
            };
        };
    };
    create_join_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateJoinRequestRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildJoinRequestResponse"];
                };
            };
        };
    };
    my_join_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": null | components["schemas"]["GuildJoinRequestResponse"];
                };
            };
        };
    };
    withdraw_join_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                request_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Join request withdrawn */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    approve_join_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                request_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildMemberResponse"];
                };
            };
        };
    };
    reject_join_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                request_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Join request rejected */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    leave_guild: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Left the guild */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    list_members: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildMemberResponse"][];
                };
            };
        };
    };
    remove_member: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                identity_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Member removed */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    update_member_role: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                identity_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateGuildMemberRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildMemberResponse"];
                };
            };
        };
    };
    list_permission_overrides: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                resource_kind: string;
                resource_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PermissionOverrideResponse"][];
                };
            };
        };
    };
    set_permission_override: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetPermissionOverrideRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PermissionOverrideResponse"];
                };
            };
        };
    };
    delete_permission_override: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                override_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DeletePermissionOverrideRequest"];
            };
        };
        responses: {
            /** @description Permission override deleted */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    list_roles: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RoleResponse"][];
                };
            };
        };
    };
    create_role: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateRoleRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RoleResponse"];
                };
            };
        };
    };
    delete_role: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                idx: number;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DeleteRoleRequest"];
            };
        };
        responses: {
            /** @description Role deleted */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    update_role: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
                idx: number;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateRoleRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RoleResponse"];
                };
            };
        };
    };
    transfer_ownership: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["TransferOwnershipRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildResponse"];
                };
            };
        };
    };
    list_profiles: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description Comma-separated identity ids, e.g. `?ids=<uuid>,<uuid>` — same shape
                 *     `presence::PresenceQuery` already established for a batched read.
                 */
                ids: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PublicProfileResponse"][];
                };
            };
        };
    };
    identity_register_finish: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RegisterFinishRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RegisterFinishResponse"];
                };
            };
        };
    };
    identity_register_start: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RegisterStartRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RegisterStartResponse"];
                };
            };
        };
    };
    search_identities: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                q: string;
                limit: number | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SearchIdentitiesResponse"];
                };
            };
        };
    };
    get_identity_integrator_data: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["VisibleIntegratorDataInstanceResponse"][];
                };
            };
        };
    };
    get_locations: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["LocationsResponse"];
                };
            };
        };
    };
    get_identity_profile: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PublicIdentityProfileResponse"];
                };
            };
        };
    };
    identity_recovery_status: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": null | components["schemas"]["RecoveryRequestResponse"];
                };
            };
        };
    };
    list_integrators: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description Free-text search over `name`/`slug`/`developer` (case-insensitive
                 *     substring) — same shape `guilds::DiscoverGuildsQuery::q` uses.
                 */
                q: string | null;
                /** @description `newest` (default) | `name`. */
                sort: string | null;
                limit: number | null;
                /**
                 * @description The last integrator id from the previous page's results — same bare-id
                 *     cursor shape `guilds::DiscoverGuildsQuery::cursor` uses.
                 */
                cursor: string | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ListIntegratorsResponse"];
                };
            };
        };
    };
    register_integrator: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateIntegratorRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorResponse"];
                };
            };
        };
    };
    integrator_whoami: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorWhoamiResponse"];
                };
            };
        };
    };
    get_integrator: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorPublicResponse"];
                };
            };
        };
    };
    list_achievement_definitions: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AchievementDefinitionResponse"][];
                };
            };
        };
    };
    create_achievement_definition: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateAchievementDefinitionRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AchievementDefinitionResponse"];
                };
            };
        };
    };
    bulk_issue_achievements: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["BulkIssueAttestationRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BulkIssueAttestationResponse"];
                };
            };
        };
    };
    update_achievement_definition: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateAchievementDefinitionRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AchievementDefinitionResponse"];
                };
            };
        };
    };
    issue_achievement: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["IssueAttestationRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AttestationResponse"];
                };
            };
        };
    };
    create_integrator_challenge: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorChallengeResponse"];
                };
            };
        };
    };
    connect: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ConnectRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ConnectResponse"];
                };
            };
        };
    };
    disconnect: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description { "ended": true } */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    revoke_grant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                capability: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description { "revoked": true } */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    list_issuer_keys: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IssuerKeyResponse"][];
                };
            };
        };
    };
    add_issuer_key: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AddIssuerKeyRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IssuerKeyResponse"];
                };
            };
        };
    };
    revoke_issuer_key: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                key_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RevokeIssuerKeyRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IssuerKeyResponse"];
                };
            };
        };
    };
    list_mappings: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorSchemaMappingResponse"][];
                };
            };
        };
    };
    publish_mapping: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PublishIntegratorSchemaMappingRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorSchemaMappingResponse"];
                };
            };
        };
    };
    get_mapping: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                seq: number;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorSchemaMappingResponse"];
                };
            };
        };
    };
    list_milestone_definitions: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AchievementDefinitionResponse"][];
                };
            };
        };
    };
    create_milestone_definition: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CreateAchievementDefinitionRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AchievementDefinitionResponse"];
                };
            };
        };
    };
    bulk_issue_milestones: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["BulkIssueAttestationRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["BulkIssueAttestationResponse"];
                };
            };
        };
    };
    update_milestone_definition: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateAchievementDefinitionRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AchievementDefinitionResponse"];
                };
            };
        };
    };
    issue_milestone: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                key: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["IssueAttestationRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AttestationResponse"];
                };
            };
        };
    };
    list_recognitions: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecognitionResponse"][];
                };
            };
        };
    };
    publish_recognition: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PublishRecognitionRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecognitionResponse"];
                };
            };
        };
    };
    revoke_recognition: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RevokeRecognitionRequest"];
            };
        };
        responses: {
            /** @description { "revoked": bool } */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    list_recognized_by: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecognitionResponse"][];
                };
            };
        };
    };
    get_integrator_registry: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorRegistryResponse"];
                };
            };
        };
    };
    list_schema_versions: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorSchemaVersionResponse"][];
                };
            };
        };
    };
    publish_schema_version: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PublishIntegratorSchemaVersionRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorSchemaVersionResponse"];
                };
            };
        };
    };
    get_schema_version: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                version: number;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorSchemaVersionResponse"];
                };
            };
        };
    };
    publish_instance: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                version: number;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["PublishInstanceRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IntegratorDataInstanceResponse"];
                };
            };
        };
    };
    delete_instance: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                slug: string;
                version: number;
                subject: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["DeleteInstanceRequest"];
            };
        };
        responses: {
            /** @description Instance tombstoned */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    register_issuer: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RegisterIssuerRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["IssuerRegistrationResponse"];
                };
            };
        };
    };
    create_registration_challenge: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RegistrationChallengeRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RegistrationChallengeResponse"];
                };
            };
        };
    };
    me: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProfileResponse"];
                };
            };
        };
    };
    update_profile: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateProfileRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ProfileResponse"];
                };
            };
        };
    };
    list_my_achievements: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description Restrict to one issuer, by its `integrators.id` — the actual
                 *     indexed foreign key `achievement_attestations.integrator_id` names,
                 *     rather than parsing the `issuer` column's `"<category>:<slug>"`
                 *     wire string back apart.
                 */
                integrator_id: string | null;
                /**
                 * @description `"achievement"` (Game-category issuers) or `"milestone"` (App/
                 *     Service), matching `IntegratorCategory::claim_kind()`.
                 */
                claim_kind: string | null;
                /**
                 * @description Cursor: an attestation id already seen by the caller. Results are
                 *     the next page strictly older than it (`issued_at` desc, `id` as
                 *     tiebreak) — same `before`/`limit` shape
                 *     `guild_messages::ListMessagesQuery` and
                 *     `integrators::ListIntegratorsQuery` already established.
                 */
                before: string | null;
                limit: number | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["ListMyAchievementsResponse"];
                };
            };
        };
    };
    list_my_connections: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["Connection"][];
                };
            };
        };
    };
    list_devices: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeviceResponse"][];
                };
            };
        };
    };
    list_device_grants: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /**
                 * @description Filters to exactly this status when present (e.g. `?status=pending`
                 *     for the approval UI); returns every grant for the caller's identity
                 *     when omitted.
                 */
                status: string | null;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeviceGrantResponse"][];
                };
            };
        };
    };
    request_device_grant: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RequestDeviceGrantRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeviceGrantResponse"];
                };
            };
        };
    };
    get_device_grant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeviceGrantResponse"];
                };
            };
        };
    };
    approve_device_grant: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["ApproveDeviceGrantRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeviceResponse"];
                };
            };
        };
    };
    rename_device: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RenameDeviceRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DeviceResponse"];
                };
            };
        };
    };
    revoke_device: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Signing key revoked */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    my_grants: {
        parameters: {
            query?: never;
            header: {
                "x-avalon-integrator-key-id": string;
            };
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MyGrantsResponse"];
                };
            };
        };
    };
    list_my_guild_announcements: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuildAnnouncementAlert"][];
                };
            };
        };
    };
    my_guild_invites: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MyGuildInviteResponse"][];
                };
            };
        };
    };
    list_my_guilds: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["MyGuildMembershipResponse"][];
                };
            };
        };
    };
    my_history: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["HistoryEntryResponse"][];
                };
            };
        };
    };
    list_passkeys: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyResponse"][];
                };
            };
        };
    };
    devices_register_finish: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["AddPasskeyFinishRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyResponse"];
                };
            };
        };
    };
    devices_register_start: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["AddPasskeyStartResponse"];
                };
            };
        };
    };
    rename_passkey: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RenamePasskeyRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PasskeyResponse"];
                };
            };
        };
    };
    revoke_passkey: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RevokePasskeyRequest"];
            };
        };
        responses: {
            /** @description Passkey revoked */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    update_my_presence: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdatePresenceRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PresenceResponse"];
                };
            };
        };
    };
    guardian_of: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardianOfSummary"][];
                };
            };
        };
    };
    resign_guardian: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                identity_id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            /** @description Resigned as guardian */
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content?: never;
            };
        };
    };
    guardian_requests: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardianRequestSummary"][];
                };
            };
        };
    };
    get_guardians: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardianSettingsResponse"];
                };
            };
        };
    };
    set_guardians: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SetGuardiansRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["GuardianSettingsResponse"];
                };
            };
        };
    };
    my_recovery_status: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": null | components["schemas"]["RecoveryRequestResponse"];
                };
            };
        };
    };
    discover_people: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["DiscoverPeopleResponse"];
                };
            };
        };
    };
    get_presence: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                /** @description Comma-separated identity ids, e.g. `?ids=<uuid>,<uuid>`. */
                ids: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PresenceResponse"][];
                };
            };
        };
    };
    update_integrator_presence: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                identity_id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["UpdateIntegratorPresenceRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["PresenceResponse"];
                };
            };
        };
    };
    finish_request: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RecoveryFinishRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryRequestResponse"];
                };
            };
        };
    };
    start_request: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["RecoveryStartRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryStartResponse"];
                };
            };
        };
    };
    get_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryRequestResponse"];
                };
            };
        };
    };
    approve_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryRequestResponse"];
                };
            };
        };
    };
    cancel_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["CancelRecoveryRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryRequestResponse"];
                };
            };
        };
    };
    finalize_request: {
        parameters: {
            query?: never;
            header?: never;
            path: {
                id: string;
            };
            cookie?: never;
        };
        requestBody?: never;
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["RecoveryRequestResponse"];
                };
            };
        };
    };
    session_finish: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SessionFinishRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SessionFinishResponse"];
                };
            };
        };
    };
    session_start: {
        parameters: {
            query?: never;
            header?: never;
            path?: never;
            cookie?: never;
        };
        requestBody: {
            content: {
                "application/json": components["schemas"]["SessionStartRequest"];
            };
        };
        responses: {
            200: {
                headers: {
                    [name: string]: unknown;
                };
                content: {
                    "application/json": components["schemas"]["SessionStartResponse"];
                };
            };
        };
    };
}

// Issue #735: the info.version this file's types were generated from.
export const OPENAPI_SCHEMA_VERSION = "0.2.0" as const
