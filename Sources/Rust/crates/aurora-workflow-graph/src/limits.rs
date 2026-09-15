use thiserror::Error;

/// Mandatory resource limits for one YAML source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct YamlSourceLimits {
    source_bytes: usize,
    nesting_depth: usize,
    aliases: usize,
    alias_expansion_nodes: usize,
    decoded_scalar_bytes: usize,
}

impl YamlSourceLimits {
    /// Creates non-zero limits whose source offsets fit the frozen `u32` span representation.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowLimitError`] for zero or unrepresentable limits.
    pub fn new(
        max_source_bytes: usize,
        max_nesting_depth: usize,
        max_aliases: usize,
        max_alias_expansion_nodes: usize,
        max_decoded_scalar_bytes: usize,
    ) -> Result<Self, WorkflowLimitError> {
        require_nonzero(max_source_bytes, "max_source_bytes")?;
        if max_source_bytes > u32::MAX as usize {
            return Err(WorkflowLimitError::SourceSpanOverflow);
        }
        require_nonzero(max_nesting_depth, "max_nesting_depth")?;
        require_nonzero(max_aliases, "max_aliases")?;
        require_nonzero(max_alias_expansion_nodes, "max_alias_expansion_nodes")?;
        require_nonzero(max_decoded_scalar_bytes, "max_decoded_scalar_bytes")?;
        Ok(Self {
            source_bytes: max_source_bytes,
            nesting_depth: max_nesting_depth,
            aliases: max_aliases,
            alias_expansion_nodes: max_alias_expansion_nodes,
            decoded_scalar_bytes: max_decoded_scalar_bytes,
        })
    }

    pub(crate) const fn source_bytes(self) -> usize {
        self.source_bytes
    }

    pub(crate) const fn nesting_depth(self) -> usize {
        self.nesting_depth
    }

    pub(crate) const fn aliases(self) -> usize {
        self.aliases
    }

    pub(crate) const fn alias_expansion_nodes(self) -> usize {
        self.alias_expansion_nodes
    }

    pub(crate) const fn decoded_scalar_bytes(self) -> usize {
        self.decoded_scalar_bytes
    }
}

/// Host-only Graph and Layout validation capacities.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkflowValidationLimits {
    graph_yaml: YamlSourceLimits,
    layout_yaml: YamlSourceLimits,
    workflows: usize,
    nodes_per_workflow: usize,
    edges_per_workflow: usize,
    layouts: usize,
    layout_nodes: usize,
    layout_edges: usize,
    layout_groups: usize,
    layout_points: usize,
}

impl WorkflowValidationLimits {
    /// Creates the complete explicit R2-01 limit set.
    ///
    /// Graph and Layout YAML limits are intentionally independent. Equality with every maximum is
    /// accepted; only the first item above a maximum is rejected.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowLimitError`] when a collection limit is zero.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        graph_yaml: YamlSourceLimits,
        layout_yaml: YamlSourceLimits,
        max_workflows: usize,
        max_nodes_per_workflow: usize,
        max_edges_per_workflow: usize,
        max_layouts: usize,
        max_layout_nodes: usize,
        max_layout_edges: usize,
        max_layout_groups: usize,
        max_layout_points: usize,
    ) -> Result<Self, WorkflowLimitError> {
        require_nonzero(max_workflows, "max_workflows")?;
        require_nonzero(max_nodes_per_workflow, "max_nodes_per_workflow")?;
        require_nonzero(max_edges_per_workflow, "max_edges_per_workflow")?;
        require_nonzero(max_layouts, "max_layouts")?;
        require_nonzero(max_layout_nodes, "max_layout_nodes")?;
        require_nonzero(max_layout_edges, "max_layout_edges")?;
        require_nonzero(max_layout_groups, "max_layout_groups")?;
        require_nonzero(max_layout_points, "max_layout_points")?;
        Ok(Self {
            graph_yaml,
            layout_yaml,
            workflows: max_workflows,
            nodes_per_workflow: max_nodes_per_workflow,
            edges_per_workflow: max_edges_per_workflow,
            layouts: max_layouts,
            layout_nodes: max_layout_nodes,
            layout_edges: max_layout_edges,
            layout_groups: max_layout_groups,
            layout_points: max_layout_points,
        })
    }

    pub(crate) const fn graph_yaml(self) -> YamlSourceLimits {
        self.graph_yaml
    }

    pub(crate) const fn layout_yaml(self) -> YamlSourceLimits {
        self.layout_yaml
    }

    pub(crate) const fn workflows(self) -> usize {
        self.workflows
    }

    pub(crate) const fn nodes_per_workflow(self) -> usize {
        self.nodes_per_workflow
    }

    pub(crate) const fn edges_per_workflow(self) -> usize {
        self.edges_per_workflow
    }

    pub(crate) const fn layouts(self) -> usize {
        self.layouts
    }

    pub(crate) const fn layout_nodes(self) -> usize {
        self.layout_nodes
    }

    pub(crate) const fn layout_edges(self) -> usize {
        self.layout_edges
    }

    pub(crate) const fn layout_groups(self) -> usize {
        self.layout_groups
    }

    pub(crate) const fn layout_points(self) -> usize {
        self.layout_points
    }
}

/// Invalid mandatory Workflow validator limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum WorkflowLimitError {
    /// A named limit is zero.
    #[error("Workflow validation limit `{0}` must be non-zero")]
    Zero(&'static str),
    /// Source offsets would not fit the public `u32` span representation.
    #[error("Workflow max_source_bytes must fit a u32 source span")]
    SourceSpanOverflow,
}

fn require_nonzero(value: usize, name: &'static str) -> Result<(), WorkflowLimitError> {
    if value == 0 {
        Err(WorkflowLimitError::Zero(name))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn yaml() -> Option<YamlSourceLimits> {
        YamlSourceLimits::new(64, 8, 8, 32, 128).ok()
    }

    #[test]
    fn yaml_limits_reject_each_zero_and_span_overflow() {
        let cases = [
            (0, 8, 8, 32, 128, "max_source_bytes"),
            (64, 0, 8, 32, 128, "max_nesting_depth"),
            (64, 8, 0, 32, 128, "max_aliases"),
            (64, 8, 8, 0, 128, "max_alias_expansion_nodes"),
            (64, 8, 8, 32, 0, "max_decoded_scalar_bytes"),
        ];
        for (bytes, depth, aliases, expansion, scalars, name) in cases {
            assert_eq!(
                YamlSourceLimits::new(bytes, depth, aliases, expansion, scalars),
                Err(WorkflowLimitError::Zero(name))
            );
        }
        if usize::BITS > 32 {
            assert_eq!(
                YamlSourceLimits::new(u32::MAX as usize + 1, 8, 8, 32, 128),
                Err(WorkflowLimitError::SourceSpanOverflow)
            );
        }
    }

    #[test]
    fn collection_limits_reject_each_zero_field() {
        let yaml = yaml();
        assert!(yaml.is_some());
        let Some(yaml) = yaml else {
            return;
        };
        let names = [
            "max_workflows",
            "max_nodes_per_workflow",
            "max_edges_per_workflow",
            "max_layouts",
            "max_layout_nodes",
            "max_layout_edges",
            "max_layout_groups",
            "max_layout_points",
        ];
        for (index, name) in names.into_iter().enumerate() {
            let mut values = [1_usize; 8];
            values[index] = 0;
            assert_eq!(
                WorkflowValidationLimits::new(
                    yaml, yaml, values[0], values[1], values[2], values[3], values[4], values[5],
                    values[6], values[7],
                ),
                Err(WorkflowLimitError::Zero(name))
            );
        }
    }
}
