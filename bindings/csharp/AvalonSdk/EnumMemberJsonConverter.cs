using System;
using System.Collections.Generic;
using System.Reflection;
using System.Runtime.Serialization;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Avalon.Sdk
{
    /// <summary>
    /// Issue #725: a System.Text.Json converter factory for the enums
    /// <c>Generated.cs</c> defines. NJsonSchema decorates each variant with
    /// <see cref="EnumMemberAttribute"/> — its own (Newtonsoft-oriented)
    /// convention for recording the variant's real wire string — which
    /// System.Text.Json's built-in <c>JsonStringEnumConverter</c> does not
    /// read at all (it only ever serializes by C# member name). Left
    /// unhandled, an enum whose wire values don't already match their
    /// PascalCase member name 1:1 — e.g. <see cref="Avalon.Sdk.Generated.Genre"/>'s
    /// lowercase <c>"action"</c>/<c>"adventure"</c>/... versus the C# member
    /// names <c>Action</c>/<c>Adventure</c>/... — would round-trip wrong: with
    /// no converter at all (NSwag's own output leaves <c>Genre</c> bare, a
    /// real bug this migration found by generating against the real schema),
    /// System.Text.Json defaults to the numeric underlying value instead of a
    /// string, so a real server response fails to deserialize outright.
    /// Applied globally via <see cref="JsonSerializerOptions.Converters"/>
    /// rather than per property, so it covers every generated enum uniformly
    /// — including ones NSwag *did* attach its own (also EnumMember-blind)
    /// <c>JsonStringEnumConverter&lt;T&gt;</c> to, since an explicit
    /// per-property attribute converter always wins over the options-level
    /// list, and those specific enums' member names already match their wire
    /// values 1:1, so it stays a correct no-op there either way.
    /// </summary>
    internal sealed class EnumMemberJsonConverterFactory : JsonConverterFactory
    {
        public override bool CanConvert(Type typeToConvert) => typeToConvert.IsEnum;

        public override JsonConverter CreateConverter(Type typeToConvert, JsonSerializerOptions options)
        {
            var converterType = typeof(EnumMemberJsonConverter<>).MakeGenericType(typeToConvert);
            return (JsonConverter)Activator.CreateInstance(converterType)!;
        }
    }

    internal sealed class EnumMemberJsonConverter<T> : JsonConverter<T> where T : struct, Enum
    {
        private static readonly Dictionary<string, T> ByWireValue = new Dictionary<string, T>(StringComparer.Ordinal);
        private static readonly Dictionary<T, string> ToWireValue = new Dictionary<T, string>();

        static EnumMemberJsonConverter()
        {
            foreach (var field in typeof(T).GetFields(BindingFlags.Public | BindingFlags.Static))
            {
                var value = (T)field.GetValue(null)!;
                var wireValue = field.GetCustomAttribute<EnumMemberAttribute>()?.Value ?? field.Name;
                ByWireValue[wireValue] = value;
                ToWireValue[value] = wireValue;
            }
        }

        public override T Read(ref Utf8JsonReader reader, Type typeToConvert, JsonSerializerOptions options)
        {
            var raw = reader.GetString() ?? throw new JsonException($"expected a string for {typeof(T).Name}");
            if (ByWireValue.TryGetValue(raw, out var value))
            {
                return value;
            }
            throw new JsonException($"unknown {typeof(T).Name} value: {raw}");
        }

        public override void Write(Utf8JsonWriter writer, T value, JsonSerializerOptions options)
        {
            writer.WriteStringValue(ToWireValue.TryGetValue(value, out var wireValue) ? wireValue : value.ToString());
        }
    }
}
