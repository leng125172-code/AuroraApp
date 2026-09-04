namespace Aurora.Contracts.Common.V1;

/// <summary>
/// Identifies the semantic invariant rejected by <see cref="CommonContractValidator"/>.
/// </summary>
public enum CommonContractValidationError
{
  /// <summary>A required message value is absent.</summary>
  MissingValue,

  /// <summary>A UUID is not a canonical RFC 9562 UUIDv7 value.</summary>
  InvalidUuid,

  /// <summary>A UTC timestamp is not normalized.</summary>
  InvalidTimestamp,

  /// <summary>An enum is unknown or uses its unspecified sentinel.</summary>
  InvalidEnum,

  /// <summary>A packed quality code uses a reserved representation.</summary>
  InvalidQualityCode,

  /// <summary>A packed error code violates its zero-value invariant.</summary>
  InvalidErrorCode,

  /// <summary>A semantic or contract version is not canonical.</summary>
  InvalidVersion,

  /// <summary>A capability identifier is not canonical or exceeds its capacity.</summary>
  InvalidCapability,

  /// <summary>An error parameter is incomplete or parameters are not strictly ordered.</summary>
  InvalidErrorParameter,
}
