// Local durable event journal — the storage half of offline
// participation described in docs/architecture/synchronization.md, mirroring
// crates/sdk/src/sync_journal.rs. Records intent locally and durably; does
// not submit, dedupe, sign, or judge worth of a recorded entry.
//
// FileJournal is an append-only JSON-lines file, fsync'd on every write,
// replayed in full on FileJournal.Open to reconstitute in-memory state — the
// same reference-implementation shape and crash-recovery behavior as the
// Rust FileJournal, so the file format is a deliberate parallel, not just a
// coincidence: {"op":"append"|"submitted"|"failed", ...}.
//
// Limitation, accepted here same as the Rust side: submitted entries are
// never compacted out of the file.

using System;
using System.Collections.Generic;
using System.IO;
using System.Linq;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;
using System.Threading;

namespace Avalon.Sdk
{
    /// <summary>Client-generated, stable, never server-assigned — a future submission
    /// engine depends on this for idempotency.</summary>
    public sealed class JournalEntry
    {
        public JournalEntry(Guid id, string kind, JsonElement payload, DateTimeOffset recordedAt, DateTimeOffset? submittedAt)
        {
            Id = id;
            Kind = kind;
            Payload = payload;
            RecordedAt = recordedAt;
            SubmittedAt = submittedAt;
        }

        public Guid Id { get; }
        public string Kind { get; }
        public JsonElement Payload { get; }
        public DateTimeOffset RecordedAt { get; }
        public DateTimeOffset? SubmittedAt { get; internal set; }
    }

    public sealed class JournalEntryNotFoundException : Exception
    {
        public JournalEntryNotFoundException(Guid id) : base($"no journal entry with id {id}")
        {
            Id = id;
        }

        public Guid Id { get; }
    }

    /// <summary>
    /// A durable local queue of offline-capable operations, behind an interface so an
    /// integrator can back it with whatever local storage fits — FileJournal is one reference
    /// implementation, not the only one. Deliberately synchronous: append is meant to be a
    /// fast, always-available local operation, never a network round trip to await.
    /// </summary>
    public interface ISyncJournal
    {
        /// <summary>Records kind/payload as a new pending entry and returns its
        /// client-generated id. Two calls with identical kind/payload produce two distinct
        /// entries — the journal never deduplicates.</summary>
        Guid Append(string kind, JsonElement payload);

        /// <summary>All entries not yet marked submitted, oldest first.</summary>
        IReadOnlyList<JournalEntry> Pending();

        /// <summary>Marks an entry submitted. Idempotent: calling it again for the same id is
        /// a no-op, never an error.</summary>
        void MarkSubmitted(Guid id);

        /// <summary>Records that a submission attempt for id failed, for diagnostics. Does not
        /// remove the entry from Pending() — retry/backoff policy belongs to a future
        /// submission engine, not the journal.</summary>
        void MarkFailed(Guid id, string reason);
    }

    /// <summary>One line of the on-disk log — see LogRecordConverter for the wire shape.</summary>
    internal abstract class LogRecord
    {
        public sealed class Append : LogRecord
        {
            public Guid Id { get; set; }
            public string Kind { get; set; } = "";
            public JsonElement Payload { get; set; }
            public DateTimeOffset RecordedAt { get; set; }
        }

        public sealed class Submitted : LogRecord
        {
            public Guid Id { get; set; }
            public DateTimeOffset At { get; set; }
        }

        public sealed class Failed : LogRecord
        {
            public Guid Id { get; set; }
            public string Reason { get; set; } = "";
        }
    }

    /// <summary>
    /// The default reference ISyncJournal implementation: an append-only JSON-lines file with
    /// an fsync (FileStream.Flush(true)) after every write.
    /// </summary>
    public sealed class FileJournal : ISyncJournal, IDisposable
    {
        private readonly object _lock = new object();
        private readonly List<Guid> _order = new List<Guid>();
        private readonly Dictionary<Guid, JournalEntry> _entries = new Dictionary<Guid, JournalEntry>();
        private FileStream _appendStream;

        private FileJournal(string path, FileStream appendStream)
        {
            Path = path;
            _appendStream = appendStream;
        }

        public string Path { get; }

        /// <summary>Opens (creating if needed) the journal at path and replays it to
        /// reconstitute pending/submitted state. Safe to call again after a crash: a trailing
        /// truncated/malformed line (exactly what a crash looks like on disk) is dropped from
        /// disk before any future write can glue a new record onto the end of it.</summary>
        public static FileJournal Open(string path)
        {
            var directory = System.IO.Path.GetDirectoryName(path);
            if (!string.IsNullOrEmpty(directory))
            {
                Directory.CreateDirectory(directory);
            }

            byte[] bytes;
            using (var readStream = new FileStream(path, FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.ReadWrite))
            {
                using var ms = new MemoryStream();
                readStream.CopyTo(ms);
                bytes = ms.ToArray();
            }

            var order = new List<Guid>();
            var entries = new Dictionary<Guid, JournalEntry>();

            var goodEnd = 0;
            var pos = 0;
            while (pos < bytes.Length)
            {
                var relNl = Array.IndexOf(bytes, (byte)'\n', pos);
                if (relNl < 0)
                {
                    // No trailing newline: the last line was never fully written — never
                    // fsynced as complete, so never acknowledged to a caller. Stop replay.
                    break;
                }
                var lineEnd = relNl;
                var nextPos = lineEnd + 1;
                string line;
                try
                {
                    line = Encoding.UTF8.GetString(bytes, pos, lineEnd - pos);
                }
                catch (Exception)
                {
                    break;
                }
                if (line.Trim().Length == 0)
                {
                    goodEnd = nextPos;
                    pos = nextPos;
                    continue;
                }

                JsonDocument doc;
                try
                {
                    doc = JsonDocument.Parse(line);
                }
                catch (JsonException)
                {
                    // A truncated/malformed trailing line is exactly what a crash looks like
                    // on disk — drop it, but don't advance goodEnd past it.
                    break;
                }

                using (doc)
                {
                    var root = doc.RootElement;
                    var op = root.GetProperty("op").GetString();
                    switch (op)
                    {
                        case "append":
                        {
                            var id = root.GetProperty("id").GetGuid();
                            var kind = root.GetProperty("kind").GetString() ?? "";
                            var payload = root.GetProperty("payload").Clone();
                            var recordedAt = root.GetProperty("recorded_at").GetDateTimeOffset();
                            order.Add(id);
                            entries[id] = new JournalEntry(id, kind, payload, recordedAt, null);
                            break;
                        }
                        case "submitted":
                        {
                            var id = root.GetProperty("id").GetGuid();
                            var at = root.GetProperty("at").GetDateTimeOffset();
                            if (entries.TryGetValue(id, out var entry))
                            {
                                entry.SubmittedAt = at;
                            }
                            break;
                        }
                        case "failed":
                            // Recorded on disk for diagnostics only — nothing to change in
                            // memory; a failed submission stays pending.
                            break;
                    }
                }

                goodEnd = nextPos;
                pos = nextPos;
            }

            if (goodEnd < bytes.Length)
            {
                using var truncate = new FileStream(path, FileMode.Open, FileAccess.Write, FileShare.ReadWrite);
                truncate.SetLength(goodEnd);
            }

            var appendStream = new FileStream(path, FileMode.Append, FileAccess.Write, FileShare.ReadWrite);
            var journal = new FileJournal(path, appendStream);
            lock (journal._lock)
            {
                journal._order.AddRange(order);
                foreach (var kv in entries)
                {
                    journal._entries[kv.Key] = kv.Value;
                }
            }
            return journal;
        }

        private static void WriteRecord(FileStream stream, object record)
        {
            var json = JsonSerializer.Serialize(record, record.GetType());
            var bytes = Encoding.UTF8.GetBytes(json + "\n");
            stream.Write(bytes, 0, bytes.Length);
            // The whole point: durable before control returns to the caller, not durable
            // "eventually" via OS write-back.
            stream.Flush(true);
        }

        public Guid Append(string kind, JsonElement payload)
        {
            var id = Guid.NewGuid();
            var recordedAt = DateTimeOffset.UtcNow;
            lock (_lock)
            {
                WriteRecord(_appendStream, new
                {
                    op = "append",
                    id,
                    kind,
                    payload,
                    recorded_at = recordedAt,
                });
                _order.Add(id);
                _entries[id] = new JournalEntry(id, kind, payload.Clone(), recordedAt, null);
            }
            return id;
        }

        public IReadOnlyList<JournalEntry> Pending()
        {
            lock (_lock)
            {
                return _order
                    .Select(id => _entries.TryGetValue(id, out var e) ? e : null)
                    .Where(e => e != null && e.SubmittedAt == null)
                    .Select(e => e!)
                    .ToList();
            }
        }

        public void MarkSubmitted(Guid id)
        {
            lock (_lock)
            {
                if (!_entries.TryGetValue(id, out var entry))
                {
                    throw new JournalEntryNotFoundException(id);
                }
                // Idempotent: already submitted, nothing to do.
                if (entry.SubmittedAt != null)
                {
                    return;
                }

                var at = DateTimeOffset.UtcNow;
                WriteRecord(_appendStream, new { op = "submitted", id, at });
                entry.SubmittedAt = at;
            }
        }

        public void MarkFailed(Guid id, string reason)
        {
            lock (_lock)
            {
                if (!_entries.ContainsKey(id))
                {
                    throw new JournalEntryNotFoundException(id);
                }
                WriteRecord(_appendStream, new { op = "failed", id, reason });
            }
        }

        public void Dispose()
        {
            lock (_lock)
            {
                _appendStream.Dispose();
            }
        }
    }
}
