using System.Diagnostics.CodeAnalysis;
using System.Globalization;

namespace Aurora.Contracts.Common.V1;

/// <summary>
/// Validates generated <c>aurora.common.v1</c> DTOs before they enter C# domain code.
/// </summary>
/// <remarks>
/// Validation is synchronous, performs no I/O, and does not mutate the message. Transport code
/// must still enforce its byte limit before Protobuf parsing so an oversized frame cannot allocate
/// an unbounded generated message.
/// </remarks>
public static class CommonContractValidator
{
  /// <summary>Maximum encoded characters in a capability identifier.</summary>
  public const int MaximumCapabilityLength = 128;

  /// <summary>Validates an RFC 9562 UUIDv7 wire value.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when the value is absent, has a non-16-byte representation, or is not UUIDv7.
  /// </exception>
  public static void Validate(UuidValue? value)
  {
    if (value is null)
    {
      throw Failure(CommonContractValidationError.MissingValue, "$", "UUID value is required.");
    }

    ReadOnlySpan<byte> bytes = value.Value.Span;
    if (bytes.Length != 16 || (bytes[6] >> 4) != 7 || (bytes[8] & 0xc0) != 0x80)
    {
      throw Failure(
          CommonContractValidationError.InvalidUuid,
          "$.value",
          "UUID must contain 16 canonical RFC 9562 UUIDv7 bytes.");
    }
  }

  /// <summary>Validates a normalized UTC timestamp.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when the value is absent or its nanosecond component is not normalized.
  /// </exception>
  public static void Validate(UtcTimestamp? value)
  {
    Require(value, "$", "UTC timestamp is required.");
    if (value.Nanos >= 1_000_000_000)
    {
      throw Failure(
          CommonContractValidationError.InvalidTimestamp,
          "$.nanos",
          "UTC nanoseconds must be less than 1,000,000,000.");
    }
  }

  /// <summary>Validates a monotonic timestamp and its required boot epoch.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when the timestamp or boot epoch is absent or invalid.
  /// </exception>
  public static void Validate(MonotonicTimestamp? value)
  {
    Require(value, "$", "Monotonic timestamp is required.");
    try
    {
      Validate(value.BootEpochId);
    }
    catch (CommonContractValidationException exception)
    {
      throw Failure(exception.Error, "$.bootEpochId", exception.Message);
    }
  }

  /// <summary>Validates absolute-time quality and every nested field.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown for absent values, unknown enums, unspecified sentinels, or invalid nested UTC.
  /// </exception>
  public static void Validate(TimeQuality? value)
  {
    Require(value, "$", "Time quality is required.");
    ValidateDeclaredEnum(value.State, TimeQualityState.Unspecified, "$.state");
    ValidateDeclaredEnum(value.Source, TimeSource.Unspecified, "$.source");
    if (value.LastSyncUtc is not null)
    {
      try
      {
        Validate(value.LastSyncUtc);
      }
      catch (CommonContractValidationException exception)
      {
        throw Failure(exception.Error, "$.lastSyncUtc", exception.Message);
      }
    }
  }

  /// <summary>Validates the stable packed quality-code representation.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when the value is absent or uses the reserved severity.
  /// </exception>
  public static void Validate(QualityCode? value)
  {
    Require(value, "$", "Quality code is required.");
    if ((value.Value >> 30) == 3)
    {
      throw Failure(
          CommonContractValidationError.InvalidQualityCode,
          "$.value",
          "Quality severity value 3 is reserved.");
    }
  }

  /// <summary>Validates the stable packed error-code representation.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when the value is absent or exactly one packed component is zero.
  /// </exception>
  public static void Validate(ErrorCode? value)
  {
    Require(value, "$", "Error code is required.");
    uint domain = value.Value >> 16;
    uint code = value.Value & 0xffff;
    if ((domain == 0) != (code == 0))
    {
      throw Failure(
          CommonContractValidationError.InvalidErrorCode,
          "$.value",
          "Only the all-zero error code may contain zero components.");
    }
  }

  /// <summary>Validates an error status and its strictly ordered parameters.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when a required nested value, enum, identifier, or parameter invariant fails.
  /// </exception>
  public static void Validate(ErrorStatus? value)
  {
    Require(value, "$", "Error status is required.");
    Validate(value.Code);
    ValidateDeclaredEnum(value.RetryClass, RetryClass.Unspecified, "$.retryClass");
    if (value.RequestId is not null)
    {
      Validate(value.RequestId);
    }

    if (value.CorrelationId is not null)
    {
      Validate(value.CorrelationId);
    }

    string? previousKey = null;
    for (int index = 0; index < value.Parameters.Count; index++)
    {
      ErrorParameter parameter = value.Parameters[index];
      string path = $"$.parameters[{index}]";
      if (parameter.ValueCase == ErrorParameter.ValueOneofCase.None ||
          (previousKey is not null && StringComparer.Ordinal.Compare(previousKey, parameter.Key) >= 0))
      {
        throw Failure(
            CommonContractValidationError.InvalidErrorParameter,
            path,
            "Error parameters require a value and strictly increasing unique keys.");
      }

      previousKey = parameter.Key;
    }
  }

  /// <summary>Validates a semantic product or package version.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when prerelease or build metadata is not canonical SemVer.
  /// </exception>
  public static void Validate(SemanticVersion? value)
  {
    Require(value, "$", "Semantic version is required.");
    if (!IsValidIdentifiers(value.PreRelease, forbidNumericLeadingZero: true) ||
        !IsValidIdentifiers(value.BuildMetadata, forbidNumericLeadingZero: false))
    {
      throw Failure(
          CommonContractValidationError.InvalidVersion,
          "$",
          "Semantic version prerelease or build metadata is invalid.");
    }
  }

  /// <summary>Validates an independently versioned public contract.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when major is zero or lifecycle is unknown or unspecified.
  /// </exception>
  public static void Validate(ContractVersion? value)
  {
    Require(value, "$", "Contract version is required.");
    if (value.Major == 0)
    {
      throw Failure(
          CommonContractValidationError.InvalidVersion,
          "$.major",
          "Contract major must be greater than zero.");
    }

    ValidateDeclaredEnum(value.Lifecycle, ContractLifecycle.Unspecified, "$.lifecycle");
  }

  /// <summary>Validates a canonical, bounded capability identifier.</summary>
  /// <param name="value">Wire value to validate.</param>
  /// <exception cref="CommonContractValidationException">
  /// Thrown when the value is absent, exceeds 128 characters, or does not use a positive u32 major.
  /// </exception>
  public static void Validate(CapabilityId? value)
  {
    Require(value, "$", "Capability identifier is required.");
    string text = value.Value;
    int separator = text.LastIndexOf('@');
    if (text.Length is 0 or > MaximumCapabilityLength ||
        separator <= 0 ||
        separator == text.Length - 1 ||
        !IsValidCapabilityName(text.AsSpan(0, separator)) ||
        !IsCanonicalPositiveMajor(text.AsSpan(separator + 1)))
    {
      throw Failure(
          CommonContractValidationError.InvalidCapability,
          "$.value",
          "Capability must be a bounded lowercase namespace followed by a positive u32 major.");
    }
  }

  private static void Require<T>([NotNull] T? value, string path, string message)
      where T : class
  {
    if (value is null)
    {
      throw Failure(CommonContractValidationError.MissingValue, path, message);
    }
  }

  private static void ValidateDeclaredEnum<TEnum>(TEnum value, TEnum unspecified, string path)
      where TEnum : struct, Enum
  {
    if (!Enum.IsDefined(value) || EqualityComparer<TEnum>.Default.Equals(value, unspecified))
    {
      throw Failure(CommonContractValidationError.InvalidEnum, path, "Enum is unknown or unspecified.");
    }
  }

  private static bool IsValidCapabilityName(ReadOnlySpan<char> name)
  {
    bool sawSeparator = false;
    bool atSegmentStart = true;
    foreach (char character in name)
    {
      if (character == '.')
      {
        if (atSegmentStart)
        {
          return false;
        }

        sawSeparator = true;
        atSegmentStart = true;
        continue;
      }

      if ((atSegmentStart && character is not (>= 'a' and <= 'z')) ||
          (!atSegmentStart && character is not (>= 'a' and <= 'z') and not (>= '0' and <= '9') and not '-'))
      {
        return false;
      }

      atSegmentStart = false;
    }

    return sawSeparator && !atSegmentStart;
  }

  private static bool IsCanonicalPositiveMajor(ReadOnlySpan<char> major)
  {
    return major.Length > 0 &&
        major[0] != '0' &&
        uint.TryParse(major, NumberStyles.None, CultureInfo.InvariantCulture, out uint parsed) &&
        parsed > 0;
  }

  private static bool IsValidIdentifiers(string text, bool forbidNumericLeadingZero)
  {
    if (text.Length == 0)
    {
      return true;
    }

    foreach (string identifier in text.Split('.'))
    {
      if (identifier.Length == 0 || identifier.Any(character => !IsAsciiAlphaNumeric(character) && character != '-'))
      {
        return false;
      }

      if (forbidNumericLeadingZero && identifier.Length > 1 && identifier[0] == '0' && identifier.All(char.IsAsciiDigit))
      {
        return false;
      }
    }

    return true;
  }

  private static bool IsAsciiAlphaNumeric(char character)
  {
    return character is >= 'a' and <= 'z' or >= 'A' and <= 'Z' or >= '0' and <= '9';
  }

  private static CommonContractValidationException Failure(
      CommonContractValidationError error,
      string path,
      string message)
  {
    return new CommonContractValidationException(error, path, message);
  }
}
