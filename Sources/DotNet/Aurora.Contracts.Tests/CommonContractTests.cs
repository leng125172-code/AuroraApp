using System.Buffers.Binary;
using Aurora.Contracts.Common.V1;
using Google.Protobuf;
using Xunit;

namespace Aurora.Contracts.Tests;

/// <summary>
/// Verifies language-neutral wire representations used by Rust and C#.
/// </summary>
public sealed class CommonContractTests
{
  /// <summary>
  /// Confirms UTC fields produce the same committed golden bytes as prost.
  /// </summary>
  [Fact]
  public void UtcTimestampMatchesRustGoldenVector()
  {
    UtcTimestamp timestamp = new()
    {
      Seconds = 1_700_000_000,
      Nanos = 123_456_789,
    };

    byte[] expected = [0x08, 0x80, 0xc4, 0x9f, 0xd5, 0x0c, 0x10, 0x95, 0x9a, 0xef, 0x3a];
    Assert.Equal(expected, timestamp.ToByteArray());
  }

  /// <summary>
  /// Confirms UUID bytes use RFC 9562 canonical order rather than the legacy Guid layout.
  /// </summary>
  [Fact]
  public void UuidUsesNetworkByteOrder()
  {
    Guid id = Guid.Parse("01890f3e-4c7b-7cc2-98c4-dc0c0c07398f");
    UuidValue value = new()
    {
      Value = ByteString.CopyFrom(id.ToByteArray(bigEndian: true)),
    };

    Assert.Equal(
        "01890F3E4C7B7CC298C4DC0C0C07398F",
        Convert.ToHexString(value.Value.Span));
  }

  /// <summary>
  /// Confirms packed quality and error fields use protobuf fixed32 little-endian bytes.
  /// </summary>
  [Fact]
  public void PackedCodesMatchRustGoldenVectors()
  {
    QualityCode quality = new() { Value = 0x4512_3402 };
    ErrorCode error = new() { Value = 0x0009_0011 };

    Assert.Equal([0x0d, 0x02, 0x34, 0x12, 0x45], quality.ToByteArray());
    Assert.Equal([0x0d, 0x11, 0x00, 0x09, 0x00], error.ToByteArray());
  }

  /// <summary>
  /// Confirms the documented control header is exactly 64 bytes with little-endian fields.
  /// </summary>
  [Fact]
  public void ControlHeaderGoldenVectorUsesDocumentedOffsets()
  {
    byte[] header = Convert.FromHexString(
        "41555243544C30310100000040000000" +
        "4000000000000000" +
        new string('0', 64) +
        "0000000000000000");

    Assert.Equal(64, header.Length);
    Assert.Equal("AURCTL01", System.Text.Encoding.ASCII.GetString(header.AsSpan(0, 8)));
    Assert.Equal((ushort)1, BinaryPrimitives.ReadUInt16LittleEndian(header.AsSpan(8, 2)));
    Assert.Equal((ulong)64, BinaryPrimitives.ReadUInt64LittleEndian(header.AsSpan(16, 8)));
  }

  /// <summary>
  /// Confirms generated parsers reject a truncated length-delimited UUID payload.
  /// </summary>
  [Fact]
  public void TruncatedUuidPayloadIsRejected()
  {
    byte[] truncated = [0x0a, 0x10, 0x01];
    Assert.Throws<InvalidProtocolBufferException>(() => UuidValue.Parser.ParseFrom(truncated));
  }

  /// <summary>
  /// Confirms semantic validation rejects values that are valid Protobuf but invalid Aurora data.
  /// </summary>
  [Fact]
  public void SemanticValidationRejectsInvalidUuidTimestampAndEnum()
  {
    CommonContractValidationException uuidError = Assert.Throws<CommonContractValidationException>(
        () => CommonContractValidator.Validate(new UuidValue { Value = ByteString.CopyFrom(new byte[16]) }));
    Assert.Equal(CommonContractValidationError.InvalidUuid, uuidError.Error);

    CommonContractValidationException timestampError = Assert.Throws<CommonContractValidationException>(
        () => CommonContractValidator.Validate(new UtcTimestamp { Nanos = 1_000_000_000 }));
    Assert.Equal(CommonContractValidationError.InvalidTimestamp, timestampError.Error);

    CommonContractValidationException enumError = Assert.Throws<CommonContractValidationException>(
        () => CommonContractValidator.Validate(new TimeQuality
        {
          State = (TimeQualityState)99,
          Source = TimeSource.System,
        }));
    Assert.Equal(CommonContractValidationError.InvalidEnum, enumError.Error);

    CommonContractValidationException qualityError = Assert.Throws<CommonContractValidationException>(
        () => CommonContractValidator.Validate(new QualityCode { Value = 0xc000_0000 }));
    Assert.Equal(CommonContractValidationError.InvalidQualityCode, qualityError.Error);

    CommonContractValidationException errorCodeError = Assert.Throws<CommonContractValidationException>(
        () => CommonContractValidator.Validate(new ErrorCode { Value = 1 }));
    Assert.Equal(CommonContractValidationError.InvalidErrorCode, errorCodeError.Error);
  }

  /// <summary>
  /// Confirms capability limits match the JSON contracts and Rust u32 representation.
  /// </summary>
  [Fact]
  public void CapabilityValidationEnforcesCanonicalLengthAndMajor()
  {
    CommonContractValidator.Validate(new CapabilityId { Value = "aurora.io.read@4294967295" });

    foreach (string invalid in new[]
    {
            "aurora.io.read@01",
            "aurora.io.read@4294967296",
            $"a.{new string('a', 125)}@1",
        })
    {
      CommonContractValidationException error = Assert.Throws<CommonContractValidationException>(
          () => CommonContractValidator.Validate(new CapabilityId { Value = invalid }));
      Assert.Equal(CommonContractValidationError.InvalidCapability, error.Error);
    }
  }

  /// <summary>
  /// Confirms structured errors require declared enums and strictly ordered parameter keys.
  /// </summary>
  [Fact]
  public void ErrorStatusValidationRejectsUnsortedParameters()
  {
    ErrorStatus status = new()
    {
      Code = new ErrorCode { Value = 0x0001_0001 },
      RetryClass = RetryClass.Never,
    };
    status.Parameters.Add(new ErrorParameter { Key = "z", StringValue = "first" });
    status.Parameters.Add(new ErrorParameter { Key = "a", StringValue = "second" });

    CommonContractValidationException error = Assert.Throws<CommonContractValidationException>(
        () => CommonContractValidator.Validate(status));
    Assert.Equal(CommonContractValidationError.InvalidErrorParameter, error.Error);
  }

  /// <summary>
  /// Confirms version validation matches the Rust domain adapters.
  /// </summary>
  [Fact]
  public void VersionValidationRejectsNonCanonicalValues()
  {
    CommonContractValidationException semanticError = Assert.Throws<CommonContractValidationException>(
        () => CommonContractValidator.Validate(new SemanticVersion { PreRelease = "01" }));
    Assert.Equal(CommonContractValidationError.InvalidVersion, semanticError.Error);

    CommonContractValidationException contractError = Assert.Throws<CommonContractValidationException>(
        () => CommonContractValidator.Validate(new ContractVersion
        {
          Major = 1,
          Lifecycle = ContractLifecycle.Unspecified,
        }));
    Assert.Equal(CommonContractValidationError.InvalidEnum, contractError.Error);
  }
}
