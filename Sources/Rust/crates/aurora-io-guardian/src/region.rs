//! Role-separated ownership of the input and output image channels.

use std::sync::Arc;

use crate::{
    ImageConsumer, ImageDirection, ImageError, ImageMapping, ImageProducer, RegionHeader,
    channel::{SharedImageChannel, channel},
};

/// Fresh per-lease in-memory realization of the shared I/O image core.
///
/// The object can be split exactly once. Guardian receives only input-producer/output-consumer
/// capabilities; Control receives only input-consumer/output-producer capabilities. Neither side
/// receives a device, socket, serial, CAN, or backend handle.
pub struct SharedIoRegion {
    header: RegionHeader,
    guardian_input: ImageProducer,
    control_input: ImageConsumer,
    control_output: ImageProducer,
    guardian_output: ImageConsumer,
    input_shared: Arc<SharedImageChannel>,
    output_shared: Arc<SharedImageChannel>,
}

impl SharedIoRegion {
    /// Allocates all four slots and both consumer staging pairs during initialization.
    ///
    /// # Errors
    ///
    /// Rejects a non-fresh header, a mapping/layout mismatch, or bounded allocation failure. No
    /// partially usable endpoint is returned.
    pub fn new(header: RegionHeader, mapping: ImageMapping<'_>) -> Result<Self, ImageError> {
        if header.input_publish_token() != 0
            || header.output_publish_token() != 0
            || header.input_drop_count() != 0
            || header.output_reject_count() != 0
        {
            return Err(ImageError::HeaderMismatch);
        }
        if mapping.layout() != header.layout()
            || mapping.layout_digest() != header.lease_identity().configuration().layout_digest()
            || mapping.capability_digest() != header.capability_digest()
        {
            return Err(ImageError::StaleOrForeignIdentity);
        }
        let (guardian_input, control_input, input_shared) =
            channel(&header, ImageDirection::Input)?;
        let (control_output, guardian_output, output_shared) =
            channel(&header, ImageDirection::Output)?;
        Ok(Self {
            header,
            guardian_input,
            control_input,
            control_output,
            guardian_output,
            input_shared,
            output_shared,
        })
    }

    /// Consumes the unique region owner and returns the only role-correct endpoints.
    #[must_use]
    pub fn split(self) -> (GuardianImageEndpoint, ControlImageEndpoint) {
        let guardian = GuardianImageEndpoint {
            header: self.header,
            input: self.guardian_input,
            output: self.guardian_output,
            input_shared: Arc::clone(&self.input_shared),
            output_shared: Arc::clone(&self.output_shared),
        };
        let control = ControlImageEndpoint {
            header: self.header,
            input: self.control_input,
            output: self.control_output,
            input_shared: self.input_shared,
            output_shared: self.output_shared,
        };
        (guardian, control)
    }
}

/// Guardian-only image capabilities: publish inputs and consume output commands.
pub struct GuardianImageEndpoint {
    header: RegionHeader,
    input: ImageProducer,
    output: ImageConsumer,
    input_shared: Arc<SharedImageChannel>,
    output_shared: Arc<SharedImageChannel>,
}

impl GuardianImageEndpoint {
    /// Returns the unique input producer.
    #[must_use]
    pub fn input_producer(&mut self) -> &mut ImageProducer {
        &mut self.input
    }

    /// Returns the unique output consumer.
    #[must_use]
    pub fn output_consumer(&mut self) -> &mut ImageConsumer {
        &mut self.output
    }

    /// Encodes an observational header snapshot with live atomic tokens/counters.
    #[must_use]
    pub fn header_snapshot(&self) -> RegionHeader {
        runtime_header(&self.header, &self.input_shared, &self.output_shared)
    }
}

/// Control-only image capabilities: latch inputs and publish output commands.
pub struct ControlImageEndpoint {
    header: RegionHeader,
    input: ImageConsumer,
    output: ImageProducer,
    input_shared: Arc<SharedImageChannel>,
    output_shared: Arc<SharedImageChannel>,
}

impl ControlImageEndpoint {
    /// Returns the unique input consumer.
    #[must_use]
    pub fn input_consumer(&mut self) -> &mut ImageConsumer {
        &mut self.input
    }

    /// Returns the unique output producer.
    #[must_use]
    pub fn output_producer(&mut self) -> &mut ImageProducer {
        &mut self.output
    }

    /// Encodes an observational header snapshot with live atomic tokens/counters.
    #[must_use]
    pub fn header_snapshot(&self) -> RegionHeader {
        runtime_header(&self.header, &self.input_shared, &self.output_shared)
    }
}

fn runtime_header(
    header: &RegionHeader,
    input: &SharedImageChannel,
    output: &SharedImageChannel,
) -> RegionHeader {
    (*header).with_runtime_counters(
        input.publish_token(),
        output.publish_token(),
        input.loss_or_reject_count(),
        output.loss_or_reject_count(),
    )
}
