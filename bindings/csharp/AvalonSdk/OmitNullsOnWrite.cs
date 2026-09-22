namespace Avalon.Sdk.Generated
{
    /// <summary>
    /// Issue #725: marks a generated request type whose optional fields mean
    /// "leave untouched" when omitted from the request body — the three-state
    /// `PATCH`-style convention (omitted = untouched, explicit value = set,
    /// `""`/empty-list = clear) several endpoints use. `AccountSession`'s
    /// request serializer checks for this marker and only then applies
    /// <see cref="System.Text.Json.Serialization.JsonIgnoreCondition.WhenWritingNull"/>
    /// — everywhere else (notably every signature-carrying request, e.g.
    /// <c>RevokePasskeyRequest</c>) a null <c>signing_key_id</c>/<c>signature</c>
    /// has to reach the server as an explicit JSON <c>null</c>, not be silently
    /// dropped: the server's own `NO_REGISTERED_SIGNING_KEY`/
    /// `FRESH_SIGNATURE_REQUIRED` split needs to see the fields, not a missing
    /// body (see `AccountSessionTests.cs`'s own header comment). Applying
    /// omit-on-null globally would have silently broken that for every
    /// signature-required call made without a local signing key — found by a
    /// pre-existing unit test failing after this migration switched request
    /// serialization off the old hand-written per-property
    /// <c>[JsonIgnore(Condition = WhenWritingNull)]</c> attributes (which only
    /// ever existed on the genuinely-three-state DTOs to begin with) onto one
    /// shared serializer. This marker restores that same per-type distinction
    /// without needing per-property attributes on generated code.
    /// </summary>
    internal interface IOmitNullsOnWrite
    {
    }

    public partial class UpdateProfileRequest : IOmitNullsOnWrite
    {
    }

    public partial class UpdateGuildRequest : IOmitNullsOnWrite
    {
    }

    public partial class UpdateChannelRequest : IOmitNullsOnWrite
    {
    }

    public partial class UpdateRoleRequest : IOmitNullsOnWrite
    {
    }
}
