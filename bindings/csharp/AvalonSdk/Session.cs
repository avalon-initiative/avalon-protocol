using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;

namespace Avalon.Sdk
{
    public sealed class CapabilityNotGrantedException : Exception
    {
        public CapabilityNotGrantedException(string capability)
            : base("capability not granted: " + capability)
        {
        }
    }

    public sealed class AchievementAttestation
    {
        public AchievementAttestation(string id, string issuer, string achievement, DateTimeOffset issuedAt)
        {
            Id = id;
            Issuer = issuer;
            Achievement = achievement;
            IssuedAt = issuedAt;
        }

        public string Id { get; }
        public string Issuer { get; }
        public string Achievement { get; }
        public DateTimeOffset IssuedAt { get; }
    }

    /// <summary>
    /// An authenticated identity session scoped to whichever capabilities were
    /// actually granted. Every read/write method checks its own required
    /// capability rather than trusting the caller — see docs/stakeholders/Proposal.md §13.
    /// </summary>
    public sealed class Session
    {
        private readonly IReadOnlyCollection<string> _grantedCapabilities;

        internal Session(string identityId, IReadOnlyCollection<string> grantedCapabilities)
        {
            IdentityId = identityId;
            _grantedCapabilities = grantedCapabilities;
        }

        public string IdentityId { get; }

        private void Require(string capability)
        {
            if (!_grantedCapabilities.Contains(capability))
            {
                throw new CapabilityNotGrantedException(capability);
            }
        }

        public Task<IReadOnlyList<AchievementAttestation>> GetAchievementsAsync(CancellationToken ct = default)
        {
            Require("achievements.read");
            throw new NotImplementedException();
        }

        public Task IssueAchievementAsync(string achievement, CancellationToken ct = default)
        {
            Require("achievements.issue");
            throw new NotImplementedException();
        }
    }
}
