using System;
using System.Collections.Generic;
using System.IO;
using System.Reflection;
using System.Text;
using System.Text.Json;
using Avalon.Sdk;
using Xunit;

namespace Avalon.Sdk.Tests;

/// <summary>
/// Cross-SDK conformance suite — loads the shared test vectors under
/// <c>conformance/vectors/</c> (repo root) and asserts this SDK's real implementation produces
/// byte-for-byte identical output to the Rust/TypeScript SDKs for the same input. Offline, no
/// server needed — runs in the default <c>dotnet test</c> job alongside every other unit test
/// here.
///
/// Client-side wire-shape codegen already covers plain request/response shapes;
/// this suite exists for "smart client" behavior codegen can't produce — see
/// <c>conformance/vectors/SCHEMA.md</c> for the full rationale and the standing
/// "add a vector when you add smart-client behavior" requirement.
///
/// Where this SDK has no client-side implementation of a vector's behavior at all
/// (<c>notSupported.csharp</c> in the vector file), this class documents the gap with an
/// explicit, passing skip test rather than fabricating an implementation to satisfy its own
/// suite — see each such test's own body for exactly what's missing.
/// </summary>
public class ConformanceTests
{
    private static string VectorsDir()
    {
        // bin/Debug/net10.0 (or similar) under AvalonSdk.Tests -> walk up to the repo root,
        // then into conformance/vectors. AppContext.BaseDirectory is the build output dir.
        var dir = new DirectoryInfo(AppContext.BaseDirectory);
        while (dir is not null && !Directory.Exists(Path.Combine(dir.FullName, "conformance", "vectors")))
        {
            dir = dir.Parent;
        }
        if (dir is null)
        {
            throw new InvalidOperationException(
                $"could not find conformance/vectors/ walking up from {AppContext.BaseDirectory}");
        }
        return Path.Combine(dir.FullName, "conformance", "vectors");
    }

    private static JsonDocument LoadVector(string name)
    {
        var path = Path.Combine(VectorsDir(), name);
        return JsonDocument.Parse(File.ReadAllText(path));
    }

    private static bool SupportedIn(JsonElement root, string lang)
    {
        foreach (var v in root.GetProperty("supportedIn").EnumerateArray())
        {
            if (v.GetString() == lang) return true;
        }
        return false;
    }

    private static byte[] HexToBytes(string hex)
    {
        var bytes = new byte[hex.Length / 2];
        for (var i = 0; i < bytes.Length; i++)
        {
            bytes[i] = Convert.ToByte(hex.Substring(i * 2, 2), 16);
        }
        return bytes;
    }

    private static string ToLowerHex(byte[] bytes)
    {
        var sb = new StringBuilder(bytes.Length * 2);
        foreach (var b in bytes) sb.Append(b.ToString("x2"));
        return sb.ToString();
    }

    // Invokes AvalonClient's private static CrossNodeLoginSigningBytes/SignWithIdentityKey via
    // reflection, so this test drives the exact production implementation
    // (bindings/csharp/AvalonSdk/CrossNodeLogin.cs) with the shared vectors' fixed inputs,
    // rather than reimplementing the signing-bytes format here — issue #725 is concurrently
    // migrating AvalonSdk/*.cs's own DTOs onto generated types, so this test avoids touching
    // that file directly and reaches its private members through reflection instead.
    private static byte[] InvokeCrossNodeLoginSigningBytes(
        Guid identityId, Guid signingKeyId, string destinationBaseUrl, string requestingContext,
        Guid nonce, DateTimeOffset issuedAt, DateTimeOffset expiresAt)
    {
        var method = typeof(AvalonClient).GetMethod(
            "CrossNodeLoginSigningBytes", BindingFlags.NonPublic | BindingFlags.Static)
            ?? throw new InvalidOperationException(
                "AvalonClient.CrossNodeLoginSigningBytes not found by reflection — did CrossNodeLogin.cs rename it?");
        var result = method.Invoke(null, new object[]
        {
            identityId, signingKeyId, destinationBaseUrl, requestingContext, nonce, issuedAt, expiresAt,
        });
        return (byte[])result!;
    }

    private static byte[] InvokeSignWithIdentityKey(byte[] signingKeySeed, byte[] message)
    {
        var method = typeof(AvalonClient).GetMethod(
            "SignWithIdentityKey", BindingFlags.NonPublic | BindingFlags.Static)
            ?? throw new InvalidOperationException(
                "AvalonClient.SignWithIdentityKey not found by reflection — did CrossNodeLogin.cs rename it?");
        var result = method.Invoke(null, new object[] { signingKeySeed, message });
        return (byte[])result!;
    }

    public static IEnumerable<object[]> CrossNodeLoginVectors()
    {
        using var doc = LoadVector("cross-node-login.json");
        var vectors = new List<object[]>();
        foreach (var v in doc.RootElement.GetProperty("vectors").EnumerateArray())
        {
            vectors.Add(new object[] { v.GetProperty("name").GetString()! });
        }
        return vectors;
    }

    [Fact]
    public void CrossNodeLogin_VectorFile_ListsCSharpAsSupported()
    {
        using var doc = LoadVector("cross-node-login.json");
        Assert.True(
            SupportedIn(doc.RootElement, "csharp"),
            "cross-node-login.json must list csharp in supportedIn — CrossNodeLogin.cs implements it");
    }

    [Theory]
    [MemberData(nameof(CrossNodeLoginVectors))]
    public void CrossNodeLogin_SigningMatchesSharedVector(string vectorName)
    {
        using var doc = LoadVector("cross-node-login.json");
        var root = doc.RootElement;
        var secretKey = HexToBytes(root.GetProperty("signingKeySeedHex").GetString()!);

        JsonElement? found = null;
        foreach (var v in root.GetProperty("vectors").EnumerateArray())
        {
            if (v.GetProperty("name").GetString() == vectorName) { found = v; break; }
        }
        Assert.True(found.HasValue, $"vector '{vectorName}' not found in cross-node-login.json");
        var vector = found!.Value;
        var input = vector.GetProperty("input");
        var expected = vector.GetProperty("expected");

        var identityId = Guid.Parse(input.GetProperty("identityId").GetString()!);
        var signingKeyId = Guid.Parse(input.GetProperty("signingKeyId").GetString()!);
        var destinationBaseUrl = input.GetProperty("destinationBaseUrl").GetString()!;
        var requestingContext = input.GetProperty("requestingContext").GetString()!;
        var nonce = Guid.Parse(input.GetProperty("nonce").GetString()!);
        var issuedAt = DateTimeOffset.FromUnixTimeSeconds(input.GetProperty("issuedAtUnixSeconds").GetInt64());
        var expiresAt = DateTimeOffset.FromUnixTimeSeconds(input.GetProperty("expiresAtUnixSeconds").GetInt64());

        var bytes = InvokeCrossNodeLoginSigningBytes(
            identityId, signingKeyId, destinationBaseUrl, requestingContext, nonce, issuedAt, expiresAt);

        Assert.Equal(expected.GetProperty("signingBytesUtf8").GetString(), Encoding.UTF8.GetString(bytes));

        var signature = InvokeSignWithIdentityKey(secretKey, bytes);
        Assert.Equal(
            expected.GetProperty("signatureHex").GetString(),
            ToLowerHex(signature));
    }

    [Fact]
    public void SessionContinuation_IsAKnownCSharpSdkGap()
    {
        using var doc = LoadVector("session-continuation.json");
        var root = doc.RootElement;
        Assert.False(
            SupportedIn(root, "csharp"),
            "session-continuation.json now lists csharp in supportedIn, but this test only " +
            "documents the gap — implement real ContinuationToken minting in AvalonSdk and real " +
            "assertions here before flipping supportedIn");
        var gap = root.GetProperty("notSupported").GetProperty("csharp").GetString();
        Assert.False(string.IsNullOrWhiteSpace(gap), "notSupported.csharp must explain the gap");
    }

    [Fact]
    public void WebSocketInterestClaim_IsAKnownCSharpSdkGap()
    {
        using var doc = LoadVector("websocket-interest-claim.json");
        var root = doc.RootElement;
        Assert.False(
            SupportedIn(root, "csharp"),
            "websocket-interest-claim.json now lists csharp in supportedIn, but this test only " +
            "documents the gap — implement real AccountSession websocket subscribe + interest-claim " +
            "minting and real assertions here before flipping supportedIn");
        var gap = root.GetProperty("notSupported").GetProperty("csharp").GetString();
        Assert.False(string.IsNullOrWhiteSpace(gap), "notSupported.csharp must explain the gap");
    }

    [Fact]
    public void Bip39Mnemonic_IsAKnownCSharpSdkGap()
    {
        using var doc = LoadVector("bip39-mnemonic.json");
        var root = doc.RootElement;
        Assert.False(
            SupportedIn(root, "csharp"),
            "bip39-mnemonic.json now lists csharp in supportedIn, but this test only documents " +
            "the gap — add a real BIP39 dependency/derivation and real assertions here before " +
            "flipping supportedIn");
        var gap = root.GetProperty("notSupported").GetProperty("csharp").GetString();
        Assert.False(string.IsNullOrWhiteSpace(gap), "notSupported.csharp must explain the gap");
    }

    // Same reflection approach as CrossNodeLoginSigningBytes above, for Session's three
    // attestation signing-byte constructions (bindings/csharp/AvalonSdk/Achievements.cs).
    // Those are private because nothing outside the SDK should be hand-rolling a signature;
    // this suite still has to drive the real implementation rather than a copy of it.
    private static byte[] InvokeSessionSigningBytes(string name, params object[] args)
    {
        var method = typeof(Session).GetMethod(name, BindingFlags.NonPublic | BindingFlags.Static)
            ?? throw new InvalidOperationException(
                $"Session.{name} not found by reflection — did Achievements.cs rename it?");
        return (byte[])method.Invoke(null, args)!;
    }

    public static IEnumerable<object[]> AttestationSigningVectors()
    {
        using var doc = LoadVector("attestation-signing.json");
        var vectors = new List<object[]>();
        foreach (var v in doc.RootElement.GetProperty("vectors").EnumerateArray())
        {
            vectors.Add(new object[] { v.GetProperty("name").GetString()! });
        }
        return vectors;
    }

    [Fact]
    public void AttestationSigning_VectorFile_ListsCSharpAsSupported()
    {
        using var doc = LoadVector("attestation-signing.json");
        Assert.True(
            SupportedIn(doc.RootElement, "csharp"),
            "attestation-signing.json must list csharp in supportedIn — Achievements.cs implements it");
    }

    [Theory]
    [MemberData(nameof(AttestationSigningVectors))]
    public void AttestationSigning_MatchesSharedVector(string vectorName)
    {
        using var doc = LoadVector("attestation-signing.json");
        var root = doc.RootElement;
        var secretKey = HexToBytes(root.GetProperty("signingKeySeedHex").GetString()!);

        JsonElement? found = null;
        foreach (var v in root.GetProperty("vectors").EnumerateArray())
        {
            if (v.GetProperty("name").GetString() == vectorName) { found = v.Clone(); break; }
        }
        Assert.True(found.HasValue, $"vector {vectorName} not found");
        var vector = found!.Value;
        var input = vector.GetProperty("input");
        var expected = vector.GetProperty("expected");

        var claimKind = input.GetProperty("claimKind").GetString()!;
        var issuerRef = input.GetProperty("issuerRef").GetString()!;
        var operation = input.GetProperty("operation").GetString()!;

        byte[] bytes;
        switch (operation)
        {
            case "issue":
                bytes = InvokeSessionSigningBytes(
                    "AttestationSigningBytes",
                    claimKind, issuerRef, Guid.Parse(input.GetProperty("subject").GetString()!),
                    input.GetProperty("achievement").GetString()!);
                break;
            case "bulk_issue":
                var refs = new List<string>();
                foreach (var a in input.GetProperty("achievements").EnumerateArray())
                {
                    refs.Add(a.GetString()!);
                }
                bytes = InvokeSessionSigningBytes(
                    "BulkAttestationSigningBytes",
                    claimKind, issuerRef, Guid.Parse(input.GetProperty("subject").GetString()!),
                    (IReadOnlyList<string>)refs);
                break;
            case "revoke":
                bytes = InvokeSessionSigningBytes(
                    "RevocationSigningBytes",
                    claimKind, issuerRef, Guid.Parse(input.GetProperty("attestationId").GetString()!),
                    input.GetProperty("reasonCode").GetString()!);
                break;
            default:
                throw new InvalidOperationException($"unknown operation {operation}");
        }

        Assert.Equal(expected.GetProperty("signingBytesHex").GetString(), ToLowerHex(bytes));
        var signature = InvokeSignWithIdentityKey(secretKey, bytes);
        Assert.Equal(expected.GetProperty("signatureHex").GetString(), ToLowerHex(signature));
    }

    [Fact]
    public void SignedTreeHead_IsAKnownCSharpSdkGap()
    {
        using var doc = LoadVector("signed-tree-head.json");
        var root = doc.RootElement;
        Assert.False(
            SupportedIn(root, "csharp"),
            "signed-tree-head.json now lists csharp in supportedIn, but this test only documents " +
            "the gap — implement real STH signing/trust-anchor verification in AvalonSdk and real " +
            "assertions here before flipping supportedIn");
        var gap = root.GetProperty("notSupported").GetProperty("csharp").GetString();
        Assert.False(string.IsNullOrWhiteSpace(gap), "notSupported.csharp must explain the gap");
    }
}
