namespace Aurora.Contracts.Common.V1;

/// <summary>
/// Reports a deterministic semantic validation failure at a Protobuf trust boundary.
/// </summary>
public sealed class CommonContractValidationException : Exception
{
  internal CommonContractValidationException(
      CommonContractValidationError error,
      string contractPath,
      string message)
      : base(message)
  {
    Error = error;
    ContractPath = contractPath;
  }

  /// <summary>Gets the machine-readable validation failure.</summary>
  public CommonContractValidationError Error { get; }

  /// <summary>Gets the field path at which validation failed.</summary>
  public string ContractPath { get; }
}
