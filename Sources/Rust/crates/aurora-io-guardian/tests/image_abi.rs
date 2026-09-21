//! R3-02 golden ABI, exact mapping closure, and atomic publication acceptance tests.

use std::thread;

use aurora_io_guardian::{
    AggregateQuality, BitOrder, ByteOrder, CapabilityDigest, GapReason, GroupDescriptor,
    GroupDiagnostics, GroupHandle, ImageDirection, ImageError, ImageLayout, ImageMapping,
    ImageSlotHeader, PreparedImage, ProtectionLevel, ProtocolSourceKind, RegionHeader, ScalarType,
    SharedIoRegion, SourceDescriptor, SourceHandle, TimeQualityCode, UpdateMarker, ValueBinding,
    ValueMetadata,
};
use aurora_io_guardian_contracts::{
    ConfigurationDigest, ConfigurationGeneration, GuardianConfiguration, GuardianEpoch,
    ImageSequence, LayoutDigest, LeaseId, LeaseIdentity, LeaseSequence,
};
use aurora_types::{LocalHandle, TagId};

struct Fixture {
    region: RegionHeader,
    sources: [SourceDescriptor; 2],
    groups: [GroupDescriptor; 2],
    values: [ValueBinding; 3],
}

impl Fixture {
    fn new() -> Option<Self> {
        let layout = ImageLayout::new(4, 4, 2, 1, 1, 1, 4_096).ok()?;
        let configuration = GuardianConfiguration::new(
            GuardianEpoch::new(5).ok()?,
            ConfigurationGeneration::new(7).ok()?,
            ConfigurationDigest::from_sha256([0x11; 32]),
            LayoutDigest::from_sha256([0x22; 32]),
        );
        let region = RegionHeader::new(
            layout,
            LeaseIdentity::new(configuration, LeaseId::new([0x44; 16]).ok()?),
            LeaseSequence::new(9).ok()?,
            CapabilityDigest::from_sha256([0x33; 32]),
        );
        let sources = [
            SourceDescriptor::new(
                SourceHandle::new(0),
                ProtocolSourceKind::Ethercat,
                [0x51; 32],
                [0x61; 32],
            ),
            SourceDescriptor::new(
                SourceHandle::new(1),
                ProtocolSourceKind::Can,
                [0x52; 32],
                [0x62; 32],
            ),
        ];
        let groups = [
            GroupDescriptor::new(
                ImageDirection::Input,
                GroupHandle::new(0),
                SourceHandle::new(0),
            ),
            GroupDescriptor::new(
                ImageDirection::Output,
                GroupHandle::new(0),
                SourceHandle::new(1),
            ),
        ];
        let values = [
            ValueBinding::new(
                LocalHandle::new(0).ok()?,
                tag(1)?,
                ImageDirection::Input,
                ScalarType::U16,
                0,
                0,
                ByteOrder::LittleEndian,
                BitOrder::Lsb0,
                SourceHandle::new(0),
                GroupHandle::new(0),
                None,
            ),
            ValueBinding::new(
                LocalHandle::new(1).ok()?,
                tag(2)?,
                ImageDirection::Input,
                ScalarType::U8,
                2,
                0,
                ByteOrder::LittleEndian,
                BitOrder::Lsb0,
                SourceHandle::new(0),
                GroupHandle::new(0),
                None,
            ),
            ValueBinding::new(
                LocalHandle::new(2).ok()?,
                tag(3)?,
                ImageDirection::Output,
                ScalarType::U32,
                0,
                0,
                ByteOrder::BigEndian,
                BitOrder::Lsb0,
                SourceHandle::new(1),
                GroupHandle::new(0),
                Some(ProtectionLevel::DeviceWatchdogProtected),
            ),
        ];
        Some(Self {
            region,
            sources,
            groups,
            values,
        })
    }

    fn mapping(&self) -> Result<ImageMapping<'_>, ImageError> {
        ImageMapping::new(
            self.region.layout(),
            &self.sources,
            &self.groups,
            &self.values,
        )
    }

    fn image(&self, direction: ImageDirection, sequence: u64, payload_byte: u8) -> Option<Vec<u8>> {
        let mapping = self.mapping().ok()?;
        let slot = self.region.layout().slot(direction);
        let source_ns = sequence.checked_mul(10)?;
        let publish_ns = source_ns.checked_add(2)?;
        let validity_ns = match direction {
            ImageDirection::Input => 0,
            ImageDirection::Output => publish_ns.checked_add(8)?,
        };
        let header = ImageSlotHeader::new(
            self.region,
            direction,
            0,
            ImageSequence::new(sequence).ok()?,
            source_ns,
            publish_ns,
            1_700_000_000,
            123_456_789,
            TimeQualityCode::Good,
            AggregateQuality::Good,
            0,
            validity_ns,
        )
        .ok()?;
        let byte_count = usize::try_from(slot.stride_bytes()).ok()?;
        let mut bytes = vec![0; byte_count];
        bytes[..128].copy_from_slice(&header.encode());
        let payload_end =
            128_usize.checked_add(usize::try_from(slot.payload_capacity_bytes()).ok()?)?;
        bytes[128..payload_end].fill(payload_byte);

        let metadata = ValueMetadata::new(
            AggregateQuality::Good,
            GapReason::None,
            UpdateMarker::Updated,
        )
        .ok()?
        .encode();
        let value_count = mapping
            .values()
            .iter()
            .filter(|value| value.direction() == direction)
            .count();
        let metadata_offset = usize::try_from(slot.metadata_offset()).ok()?;
        for index in 0..value_count {
            let start = metadata_offset.checked_add(index.checked_mul(metadata.len())?)?;
            let end = start.checked_add(metadata.len())?;
            bytes[start..end].copy_from_slice(&metadata);
        }

        let group = mapping
            .groups()
            .iter()
            .find(|group| group.direction() == direction)?;
        let diagnostics = GroupDiagnostics::new(
            u32::from(group.handle().get()),
            group.source().get(),
            sequence,
            source_ns,
            publish_ns,
            1_700_000_000,
            123_456_789,
            TimeQualityCode::Good,
            AggregateQuality::Good,
            u32::try_from(value_count).ok()?,
            0,
            0,
            u32::try_from(value_count).ok()?,
        )
        .ok()?
        .encode();
        let diagnostics_offset = usize::try_from(slot.diagnostics_offset()).ok()?;
        let diagnostics_end = diagnostics_offset.checked_add(diagnostics.len())?;
        bytes[diagnostics_offset..diagnostics_end].copy_from_slice(&diagnostics);
        Some(bytes)
    }
}

fn tag(discriminator: u8) -> Option<TagId> {
    let mut bytes = [
        0x01, 0x89, 0x0f, 0x3e, 0x4c, 0x7b, 0x7c, 0xc2, 0x98, 0xc4, 0xdc, 0x0c, 0x0c, 0x07, 0x39,
        0x8f,
    ];
    bytes[15] = discriminator;
    TagId::from_bytes(bytes).ok()
}

#[test]
fn region_and_slot_headers_have_exact_golden_offsets() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let encoded = fixture.region.encode();
        assert_eq!(&encoded[0..8], b"AURIO001");
        assert_eq!(&encoded[8..12], &[1, 0, 0, 0]);
        assert_eq!(&encoded[12..16], &256_u32.to_le_bytes());
        assert_eq!(&encoded[24..32], &5_u64.to_le_bytes());
        assert_eq!(&encoded[32..64], &[0x11; 32]);
        assert_eq!(&encoded[112..144], &[0x33; 32]);
        assert_eq!(&encoded[144..160], &[0x44; 16]);
        assert_eq!(&encoded[192..200], &7_u64.to_le_bytes());
        assert_eq!(&encoded[200..208], &9_u64.to_le_bytes());
        assert_eq!(&encoded[208..240], &[0x22; 32]);
        assert_eq!(&encoded[240..244], &2_u32.to_le_bytes());
        assert_eq!(&encoded[244..248], &1_u32.to_le_bytes());
        assert_eq!(&encoded[248..256], &[0; 8]);
        assert_eq!(RegionHeader::decode(encoded), Ok(fixture.region));

        let image = fixture.image(ImageDirection::Output, 1, 0xa5);
        assert!(image.is_some());
        if let Some(image) = image {
            assert_eq!(&image[16..24], &1_u64.to_le_bytes());
            assert_eq!(&image[56..60], &1_u32.to_le_bytes());
            assert_eq!(&image[60..64], &4_u32.to_le_bytes());
            assert_eq!(&image[64..96], &[0x22; 32]);
            assert_eq!(&image[112..120], &20_u64.to_le_bytes());
            assert_eq!(&image[120..128], &9_u64.to_le_bytes());
            let slot = fixture.region.layout().output();
            let metadata_offset = usize::try_from(slot.metadata_offset()).ok();
            let diagnostics_offset = usize::try_from(slot.diagnostics_offset()).ok();
            assert!(metadata_offset.is_some());
            assert!(diagnostics_offset.is_some());
            if let (Some(metadata_offset), Some(diagnostics_offset)) =
                (metadata_offset, diagnostics_offset)
            {
                assert_eq!(
                    &image[metadata_offset..metadata_offset + 8],
                    &[0, 0, 1, 0, 0, 0, 0, 0]
                );
                assert_eq!(
                    &image[diagnostics_offset..diagnostics_offset + 8],
                    &[0, 0, 0, 0, 1, 0, 0, 0]
                );
            }
        }
    }
}

#[test]
fn headers_reject_unknown_reserved_and_unencodable_tokens() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let mut flags = fixture.region.encode();
        flags[104] = 1;
        assert_eq!(
            RegionHeader::decode(flags),
            Err(ImageError::ReservedNonZero)
        );

        let mut reserved = fixture.region.encode();
        reserved[255] = 1;
        assert_eq!(
            RegionHeader::decode(reserved),
            Err(ImageError::ReservedNonZero)
        );

        let mut token = fixture.region.encode();
        token[160..168].copy_from_slice(&1_u64.to_le_bytes());
        assert_eq!(
            RegionHeader::decode(token),
            Err(ImageError::ImageSequenceViolation)
        );

        let image = fixture.image(ImageDirection::Input, 1, 0x5a);
        assert!(image.is_some());
        if let Some(mut image) = image {
            image[0..8].copy_from_slice(&1_u64.to_le_bytes());
            let mut header = [0; 128];
            header.copy_from_slice(&image[..128]);
            assert_eq!(
                ImageSlotHeader::decode(header, fixture.region, ImageDirection::Input),
                Err(ImageError::StaleOrForeignIdentity)
            );
        }
    }
}

#[test]
fn mapping_rejects_missing_extra_reordered_overlapping_and_out_of_bounds_values() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        assert!(fixture.mapping().is_ok());
        assert!(matches!(
            ImageMapping::new(
                fixture.region.layout(),
                &fixture.sources,
                &fixture.groups,
                &fixture.values[..2]
            ),
            Err(ImageError::MappingClosureMismatch)
        ));

        let mut reordered = fixture.values;
        reordered.swap(0, 1);
        assert!(matches!(
            ImageMapping::new(
                fixture.region.layout(),
                &fixture.sources,
                &fixture.groups,
                &reordered
            ),
            Err(ImageError::NonDenseHandle)
        ));

        let overlapping = [
            fixture.values[0],
            ValueBinding::new(
                fixture.values[1].handle(),
                fixture.values[1].tag_id(),
                ImageDirection::Input,
                ScalarType::U8,
                1,
                0,
                ByteOrder::LittleEndian,
                BitOrder::Lsb0,
                SourceHandle::new(0),
                GroupHandle::new(0),
                None,
            ),
            fixture.values[2],
        ];
        assert!(matches!(
            ImageMapping::new(
                fixture.region.layout(),
                &fixture.sources,
                &fixture.groups,
                &overlapping
            ),
            Err(ImageError::ValueOverlap)
        ));

        let out_of_bounds = [
            fixture.values[0],
            ValueBinding::new(
                fixture.values[1].handle(),
                fixture.values[1].tag_id(),
                ImageDirection::Input,
                ScalarType::U16,
                3,
                0,
                ByteOrder::LittleEndian,
                BitOrder::Lsb0,
                SourceHandle::new(0),
                GroupHandle::new(0),
                None,
            ),
            fixture.values[2],
        ];
        assert!(matches!(
            ImageMapping::new(
                fixture.region.layout(),
                &fixture.sources,
                &fixture.groups,
                &out_of_bounds
            ),
            Err(ImageError::ValueOutOfBounds)
        ));

        let extra = [
            fixture.values[0],
            fixture.values[1],
            fixture.values[2],
            fixture.values[2],
        ];
        assert!(matches!(
            ImageMapping::new(
                fixture.region.layout(),
                &fixture.sources,
                &fixture.groups,
                &extra
            ),
            Err(ImageError::MappingClosureMismatch)
        ));
    }
}

#[test]
fn protocol_handle_catalog_is_exact_and_backend_neutral() {
    let kinds = [
        ProtocolSourceKind::Ethercat,
        ProtocolSourceKind::ModbusTcp,
        ProtocolSourceKind::ModbusRtu,
        ProtocolSourceKind::Serial,
        ProtocolSourceKind::Can,
        ProtocolSourceKind::Lin,
    ];
    assert_eq!(kinds.map(|kind| kind as u8), [0, 1, 2, 3, 4, 5]);
}

#[test]
fn role_split_publishes_whole_images_and_expires_same_token_outputs() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let mapping = fixture.mapping();
        assert!(mapping.is_ok());
        if let Ok(mapping) = mapping {
            let region = SharedIoRegion::new(fixture.region, mapping);
            assert!(region.is_ok());
            if let Ok(region) = region {
                let (mut guardian, mut control) = region.split();
                assert_eq!(
                    control.input_consumer().try_latch(0, mapping).err(),
                    Some(ImageError::NoPublication)
                );

                let input = fixture.image(ImageDirection::Input, 1, 0xa5);
                assert!(input.is_some());
                if let Some(input) = input {
                    let prepared =
                        PreparedImage::new(fixture.region, ImageDirection::Input, mapping, &input);
                    assert!(prepared.is_ok());
                    if let Ok(prepared) = prepared {
                        assert!(guardian.input_producer().try_publish(prepared).is_ok());
                        let latched = control.input_consumer().try_latch(12, mapping);
                        assert!(latched.is_ok());
                        if let Ok(latched) = latched {
                            assert_eq!(&latched.bytes()[128..132], &[0xa5; 4]);
                            assert_eq!(latched.sequence_gap(), 0);
                        }
                    }
                }

                let output = fixture.image(ImageDirection::Output, 1, 0x5a);
                assert!(output.is_some());
                if let Some(output) = output {
                    assert_eq!(
                        control.output_producer().record_input_drop(),
                        Err(ImageError::EndpointOwnership)
                    );
                    let prepared = PreparedImage::new(
                        fixture.region,
                        ImageDirection::Output,
                        mapping,
                        &output,
                    );
                    assert!(prepared.is_ok());
                    if let Ok(prepared) = prepared {
                        assert!(control.output_producer().try_publish(prepared).is_ok());
                        assert!(guardian.output_consumer().try_latch(19, mapping).is_ok());
                        assert_eq!(
                            guardian.output_consumer().try_latch(20, mapping).err(),
                            Some(ImageError::InvalidValidityDeadline)
                        );
                        assert!(guardian.output_consumer().previous_snapshot().is_some());
                    }
                }
                assert_eq!(guardian.header_snapshot().input_publish_token(), 2);
                assert_eq!(control.header_snapshot().output_publish_token(), 2);
                assert_eq!(guardian.header_snapshot().output_reject_count(), 1);
            }
        }
    }
}

#[test]
fn publication_rejects_duplicate_skip_partial_and_hidden_bad_quality() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let mapping = fixture.mapping();
        assert!(mapping.is_ok());
        if let Ok(mapping) = mapping {
            let region = SharedIoRegion::new(fixture.region, mapping);
            assert!(region.is_ok());
            if let Ok(region) = region {
                let (mut guardian, _) = region.split();
                let first = fixture.image(ImageDirection::Input, 1, 0x11);
                assert!(first.is_some());
                if let Some(first) = first {
                    let prepared =
                        PreparedImage::new(fixture.region, ImageDirection::Input, mapping, &first);
                    assert!(prepared.is_ok());
                    if let Ok(prepared) = prepared {
                        assert!(guardian.input_producer().try_publish(prepared).is_ok());
                    }
                    let duplicate =
                        PreparedImage::new(fixture.region, ImageDirection::Input, mapping, &first);
                    assert!(duplicate.is_ok());
                    if let Ok(duplicate) = duplicate {
                        assert_eq!(
                            guardian.input_producer().try_publish(duplicate),
                            Err(ImageError::ImageSequenceViolation)
                        );
                    }
                }

                let skipped = fixture.image(ImageDirection::Input, 3, 0x33);
                assert!(skipped.is_some());
                if let Some(skipped) = skipped {
                    let prepared = PreparedImage::new(
                        fixture.region,
                        ImageDirection::Input,
                        mapping,
                        &skipped,
                    );
                    assert!(prepared.is_ok());
                    if let Ok(prepared) = prepared {
                        assert_eq!(
                            guardian.input_producer().try_publish(prepared),
                            Err(ImageError::ImageSequenceViolation)
                        );
                    }
                }

                let partial = fixture.image(ImageDirection::Input, 2, 0x22);
                assert!(partial.is_some());
                if let Some(mut partial) = partial {
                    let _ = partial.pop();
                    assert!(matches!(
                        PreparedImage::new(
                            fixture.region,
                            ImageDirection::Input,
                            mapping,
                            &partial
                        ),
                        Err(ImageError::SlotSizeMismatch)
                    ));
                }

                let hidden_bad = fixture.image(ImageDirection::Input, 2, 0x22);
                assert!(hidden_bad.is_some());
                if let Some(mut hidden_bad) = hidden_bad {
                    let offset = usize::try_from(fixture.region.layout().input().metadata_offset());
                    assert!(offset.is_ok());
                    if let Ok(offset) = offset {
                        hidden_bad[53] = AggregateQuality::Bad as u8;
                        hidden_bad[offset..offset + 8].copy_from_slice(
                            &ValueMetadata::new(
                                AggregateQuality::Bad,
                                GapReason::Checksum,
                                UpdateMarker::Updated,
                            )
                            .map_or([0; 8], ValueMetadata::encode),
                        );
                        assert!(matches!(
                            PreparedImage::new(
                                fixture.region,
                                ImageDirection::Input,
                                mapping,
                                &hidden_bad
                            ),
                            Err(ImageError::InvalidQualityMetadata)
                        ));
                    }
                }
            }
        }
    }
}

#[test]
fn first_latch_reports_every_sequence_not_observed_by_control() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let mapping = fixture.mapping();
        assert!(mapping.is_ok());
        if let Ok(mapping) = mapping {
            let region = SharedIoRegion::new(fixture.region, mapping);
            assert!(region.is_ok());
            if let Ok(region) = region {
                let (mut guardian, mut control) = region.split();
                for sequence in 1..=2 {
                    let image =
                        fixture.image(ImageDirection::Input, sequence, sequence.to_le_bytes()[0]);
                    assert!(image.is_some());
                    if let Some(image) = image {
                        let prepared = PreparedImage::new(
                            fixture.region,
                            ImageDirection::Input,
                            mapping,
                            &image,
                        );
                        assert!(prepared.is_ok());
                        if let Ok(prepared) = prepared {
                            assert!(guardian.input_producer().try_publish(prepared).is_ok());
                        }
                    }
                }
                let observation = control.input_consumer().try_latch(22, mapping);
                assert!(observation.is_ok());
                if let Ok(observation) = observation {
                    assert_eq!(observation.header().image_sequence().get(), 2);
                    assert_eq!(observation.sequence_gap(), 1);
                }
            }
        }
    }
}

#[test]
fn concurrent_publication_never_accepts_torn_payload() {
    let fixture = Fixture::new();
    assert!(fixture.is_some());
    if let Some(fixture) = fixture {
        let mapping = fixture.mapping();
        assert!(mapping.is_ok());
        if let Ok(mapping) = mapping {
            let region = SharedIoRegion::new(fixture.region, mapping);
            assert!(region.is_ok());
            if let Ok(region) = region {
                let (mut guardian, mut control) = region.split();
                thread::scope(|scope| {
                    let producer = scope.spawn(|| {
                        for sequence in 1..=1_000 {
                            let Some(image) = fixture.image(
                                ImageDirection::Input,
                                sequence,
                                sequence.to_le_bytes()[0],
                            ) else {
                                return false;
                            };
                            let Ok(prepared) = PreparedImage::new(
                                fixture.region,
                                ImageDirection::Input,
                                mapping,
                                &image,
                            ) else {
                                return false;
                            };
                            if guardian.input_producer().try_publish(prepared).is_err() {
                                return false;
                            }
                        }
                        true
                    });
                    let consumer = scope.spawn(|| {
                        for _ in 0..100_000 {
                            match control.input_consumer().try_latch(u64::MAX, mapping) {
                                Ok(observation) => {
                                    let expected =
                                        observation.header().image_sequence().get().to_le_bytes()
                                            [0];
                                    if observation.bytes()[128..132]
                                        .iter()
                                        .any(|value| *value != expected)
                                    {
                                        return false;
                                    }
                                    if observation.header().image_sequence().get() == 1_000 {
                                        return true;
                                    }
                                }
                                Err(ImageError::NoPublication | ImageError::Contended) => {}
                                Err(_) => return false,
                            }
                        }
                        false
                    });
                    assert!(matches!(producer.join(), Ok(true)));
                    assert!(matches!(consumer.join(), Ok(true)));
                });
            }
        }
    }
}
