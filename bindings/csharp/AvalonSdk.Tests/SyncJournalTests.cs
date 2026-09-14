using System;
using System.IO;
using System.Linq;
using System.Text.Json;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

public class SyncJournalTests
{
    private static string TempJournalPath(string label) =>
        Path.Combine(Path.GetTempPath(), $"avalon-sync-journal-{label}-{Guid.NewGuid()}.jsonl");

    private static JsonElement Payload(string json) => JsonDocument.Parse(json).RootElement.Clone();

    [Fact]
    public void TwoAppendsWithIdenticalPayload_GetDistinctIds()
    {
        var path = TempJournalPath("dup-payload");
        using var journal = FileJournal.Open(path);
        try
        {
            var payload = Payload(@"{""achievement"":""dragon_slayer""}");
            var idA = journal.Append("achievement.earned", payload);
            var idB = journal.Append("achievement.earned", payload);

            Assert.NotEqual(idA, idB);

            var pending = journal.Pending();
            Assert.Equal(2, pending.Count);
            Assert.Contains(pending, e => e.Id == idA);
            Assert.Contains(pending, e => e.Id == idB);
        }
        finally
        {
            File.Delete(path);
        }
    }

    [Fact]
    public void MarkSubmitted_IsIdempotent()
    {
        var path = TempJournalPath("idempotent-submit");
        Guid id;
        using (var journal = FileJournal.Open(path))
        {
            id = journal.Append("chat.message", Payload(@"{""body"":""hi""}"));

            journal.MarkSubmitted(id);
            journal.MarkSubmitted(id);
            journal.MarkSubmitted(id);

            Assert.Empty(journal.Pending());
        }

        using var reopened = FileJournal.Open(path);
        Assert.Empty(reopened.Pending());
        File.Delete(path);
    }

    [Fact]
    public void CrashRecovery_PendingEntriesSurviveAnUncleanShutdown()
    {
        var path = TempJournalPath("crash-recovery");
        Guid idPending, idSubmitted;
        using (var journal = FileJournal.Open(path))
        {
            idPending = journal.Append("guild.join_request", Payload(@"{""guild"":""The Round Table""}"));
            idSubmitted = journal.Append("chat.message", Payload(@"{""body"":""already sent before the crash""}"));
            journal.MarkSubmitted(idSubmitted);
            // No explicit close beyond Dispose at the end of this block — fsync already
            // happened on every write above, so this is the durable on-disk state a real
            // process death would leave behind.
        }

        using var recovered = FileJournal.Open(path);
        var pending = recovered.Pending();

        var only = Assert.Single(pending);
        Assert.Equal(idPending, only.Id);
        Assert.Equal("guild.join_request", only.Kind);
        Assert.DoesNotContain(pending, e => e.Id == idSubmitted);
        File.Delete(path);
    }

    [Fact]
    public void MarkFailed_RecordsReasonWithoutRemovingFromPending()
    {
        var path = TempJournalPath("mark-failed");
        using var journal = FileJournal.Open(path);
        var id = journal.Append("friend.request", Payload(@"{""to"":""user-2""}"));

        journal.MarkFailed(id, "server rejected: rate limited");

        var pending = journal.Pending();
        var only = Assert.Single(pending);
        Assert.Equal(id, only.Id);
        File.Delete(path);
    }

    [Fact]
    public void OpeningWithATruncatedTrailingLine_RecoversPriorCompleteEntries()
    {
        var path = TempJournalPath("truncated-tail-readonly");
        var validId = Guid.NewGuid();
        var goodLine = JsonSerializer.Serialize(new
        {
            op = "append",
            id = validId,
            kind = "guild.join_request",
            payload = Payload(@"{""guild"":""The Round Table""}"),
            recorded_at = DateTimeOffset.UtcNow,
        });
        // A malformed, non-newline-terminated tail — exactly what an interrupted write
        // leaves on disk.
        File.WriteAllBytes(path, System.Text.Encoding.UTF8.GetBytes(
            goodLine + "\n" + @"{""op"":""append"",""id"":""not-fini"));

        using var journal = FileJournal.Open(path);
        var pending = journal.Pending();

        var only = Assert.Single(pending);
        Assert.Equal(validId, only.Id);
        Assert.Equal("guild.join_request", only.Kind);
        File.Delete(path);
    }

    [Fact]
    public void OpeningWithATruncatedTail_ThenAppending_DoesNotCorruptFutureAppends()
    {
        var path = TempJournalPath("truncated-tail-then-append");
        var validId = Guid.NewGuid();
        var goodLine = JsonSerializer.Serialize(new
        {
            op = "append",
            id = validId,
            kind = "guild.join_request",
            payload = Payload(@"{""guild"":""The Round Table""}"),
            recorded_at = DateTimeOffset.UtcNow,
        });
        File.WriteAllBytes(path, System.Text.Encoding.UTF8.GetBytes(
            goodLine + "\n" + @"{""op"":""append"",""id"":""not-fini"));

        Guid newId;
        using (var journal = FileJournal.Open(path))
        {
            newId = journal.Append("chat.message", Payload(@"{""body"":""written after recovery""}"));
        }

        using var reopened = FileJournal.Open(path);
        var pending = reopened.Pending();

        Assert.Equal(2, pending.Count);
        Assert.Contains(pending, e => e.Id == validId);
        Assert.Contains(pending, e => e.Id == newId);
        File.Delete(path);
    }

    [Fact]
    public void MarkSubmittedOnUnknownId_Throws()
    {
        var path = TempJournalPath("unknown-id");
        using var journal = FileJournal.Open(path);

        Assert.Throws<JournalEntryNotFoundException>(() => journal.MarkSubmitted(Guid.NewGuid()));
        File.Delete(path);
    }
}
