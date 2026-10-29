use app_units::Au;
use layout_api::{LayoutElement, LayoutNode};
use malloc_size_of_derive::MallocSizeOf;
use script::layout_dom::ServoLayoutNode;
use servo_arc::Arc as ServoArc;
use style::context::SharedStyleContext;
use style::properties::ComputedValues;
use web_atoms::local_name;

use super::MathLevelBox;
use crate::cell::ArcRefCell;
use crate::context::LayoutContext;
use crate::dom::WeakLayoutBox;
use crate::dom_traversal::NodeAndStyleInfo;
use crate::fragment_tree::BaseFragmentInfo;
use crate::layout_box_base::LayoutBoxBase;
use crate::mathml::{
    MathBoxMetrics, MathLayoutResult, MathOperatorProperties, MathStretchAxis, MathStretchTarget,
    translate_fragments,
};
use crate::positioned::PositioningContext;
use crate::sizing::{ContentSizes, InlineContentSizesResult};
use crate::style_ext::LayoutStyle;
use crate::{ConstraintSpace, ContainingBlock};
use style::Zero;

#[derive(Debug, MallocSizeOf, PartialEq)]
enum MathRowKind {
    Grouping,
    FirstChildOperator,
    Other,
}

#[derive(Debug, MallocSizeOf)]
pub(crate) struct MathRowBlock {
    pub base: LayoutBoxBase,
    pub children: Vec<ArcRefCell<MathLevelBox>>,
    /// Non-grouping elements currently using row fallback must not accidentally
    /// acquire grouping-element embellished-operator semantics.
    kind: MathRowKind,
}
impl MathRowBlock {
    pub(crate) fn construct(
        node_and_style_info: &NodeAndStyleInfo,
        children: Vec<ArcRefCell<MathLevelBox>>,
    ) -> MathRowBlock {
        let base_fragment_info: BaseFragmentInfo = node_and_style_info.into();
        let base = LayoutBoxBase::new(base_fragment_info, node_and_style_info.style.clone());
        let subtree_size = children
            .iter()
            .map(|c| c.borrow().with_base(|base| base.subtree_size()))
            .sum::<usize>()
            + 1;
        base.set_subtree_size(subtree_size);
        let kind = node_and_style_info
            .node
            .as_element()
            .map_or(MathRowKind::Grouping, |element| {
                match element.local_name() {
                    &local_name!("msqrt") | &local_name!("mroot") => MathRowKind::Other,
                    &local_name!("msub")
                    | &local_name!("msup")
                    | &local_name!("msubsup")
                    | &local_name!("munder")
                    | &local_name!("mover")
                    | &local_name!("munderover")
                    | &local_name!("mmultiscripts")
                    | &local_name!("mfrac") => MathRowKind::FirstChildOperator,
                    _ => MathRowKind::Grouping,
                }
            });
        MathRowBlock {
            base,
            children,
            kind,
        }
    }

    pub(crate) fn repair_style(
        &mut self,
        context: &SharedStyleContext,
        node: &ServoLayoutNode,
        new_style: &ServoArc<ComputedValues>,
    ) {
        self.base.repair_style(new_style);
        for child in &self.children {
            child.borrow_mut().repair_style(context, node, new_style);
        }
    }
    pub(crate) fn attached_to_tree(&self, layout_box: WeakLayoutBox) {
        for child in &self.children {
            child.borrow().attached_to_tree(layout_box.clone());
        }
    }
    pub(crate) fn inline_content_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        let mut result = self.content_inline_sizes(layout_context, constraint_space);
        let pbm = LayoutStyle::Default(&self.base.style)
            .content_box_sizes_and_padding_border_margin(&constraint_space.into())
            .pbm;
        let extra = pbm.padding_border_sums.inline + pbm.margin.auto_is(Au::zero).inline_sum();
        result.sizes.min_content += extra;
        result.sizes.max_content += extra;
        result
    }

    pub(crate) fn content_inline_sizes(
        &self,
        layout_context: &LayoutContext,
        constraint_space: &ConstraintSpace,
    ) -> InlineContentSizesResult {
        let mut min_content = Au::zero();
        let mut max_content = Au::zero();
        let mut depends_on_block_constraints = false;
        for child in &self.children {
            if !child.borrow().is_in_flow() {
                continue;
            }
            let child_sizes = child
                .borrow()
                .inline_content_sizes(layout_context, constraint_space);
            min_content += child_sizes.sizes.min_content;
            max_content += child_sizes.sizes.max_content;
            depends_on_block_constraints |= child_sizes.depends_on_block_constraints;
        }
        InlineContentSizesResult {
            sizes: ContentSizes {
                min_content,
                max_content,
            },
            depends_on_block_constraints,
        }
    }
    /// Locate the unique embellished child, ignoring only in-flow space-like
    /// siblings. This query does not perform layout or retain a borrowed child.
    pub(crate) fn embellished_operator_index(&self) -> Option<usize> {
        if self.kind == MathRowKind::Other {
            return None;
        }
        if self.kind == MathRowKind::FirstChildOperator {
            return self
                .children
                .iter()
                .enumerate()
                .find_map(|(index, child)| {
                    let child = child.borrow();
                    child
                        .is_in_flow()
                        .then(|| child.operator_properties().map(|_| index))
                })
                .flatten();
        }
        let mut operator = None;
        for (index, child) in self.children.iter().enumerate() {
            let child = child.borrow();
            if !child.is_in_flow() || child.is_space_like() {
                continue;
            }
            if operator.is_some() || child.operator_properties().is_none() {
                return None;
            }
            operator = Some(index);
        }
        operator
    }

    pub(crate) fn operator_properties(&self) -> Option<MathOperatorProperties> {
        let index = self.embellished_operator_index()?;
        self.children[index].borrow().operator_properties()
    }

    pub(crate) fn is_space_like(&self) -> bool {
        self.kind == MathRowKind::Grouping
            && self.children.iter().all(|child| {
                let child = child.borrow();
                !child.is_in_flow() || child.is_space_like()
            })
    }

    pub(crate) fn layout(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let pbm = LayoutStyle::Default(&self.base.style)
            .content_box_sizes_and_padding_border_margin(&containing_block.into())
            .pbm;
        let content = self.layout_contents(
            layout_context,
            positioning_context,
            containing_block,
            target_size,
        );
        MathLayoutResult::from_content(
            &self.base,
            containing_block,
            &pbm,
            content.fragments,
            content.metrics,
            None,
        )
    }

    /// Also used by mpadded, which owns the CSS box around its inferred row.
    /// Apply no padding/border/margin here: the caller decorates the result once.
    pub(crate) fn layout_contents(
        &self,
        layout_context: &LayoutContext,
        positioning_context: &mut PositioningContext,
        containing_block: &ContainingBlock,
        target_size: Option<MathStretchTarget>,
    ) -> MathLayoutResult {
        let inherited_operator = target_size.and_then(|_| self.embellished_operator_index());
        let properties: Vec<_> = self
            .children
            .iter()
            .map(|child| child.borrow().operator_properties())
            .collect();
        let mut outputs: Vec<Option<MathLayoutResult>> =
            (0..self.children.len()).map(|_| None).collect();
        let mut deferred = Vec::new();
        let mut target_extents: Option<(Au, Au)> = None;

        // Sibling coordination stays here. Accepted children append positioning
        // state once; speculative measurements below use temporary contexts.
        for (index, child) in self.children.iter().enumerate() {
            let child = child.borrow();
            if inherited_operator.is_none()
                && child.is_in_flow()
                && properties[index].is_some_and(|properties| {
                    properties.stretchy && properties.stretch_axis == MathStretchAxis::Block
                })
            {
                deferred.push(index);
                continue;
            }
            let target = if inherited_operator == Some(index) {
                target_size
            } else {
                None
            };
            let output = child.layout_for_math_parent(
                layout_context,
                positioning_context,
                containing_block,
                &self.base.style,
                target,
            );
            if child.is_in_flow() {
                let ink = (output.metrics.ink_ascent, output.metrics.ink_descent);
                target_extents = Some(target_extents.map_or(ink, |previous| {
                    (previous.0.max(ink.0), previous.1.max(ink.1))
                }));
            }
            outputs[index] = Some(output);
        }

        // An all-stretchy row first measures with (0, 0), then chooses a shared
        // target from those natural results. Discard speculative positioning
        // state so the final layouts cannot hoist the same descendant twice.
        if target_extents.is_none() {
            for &index in &deferred {
                let mut measurement_positioning = PositioningContext::default();
                let measurement = self.children[index].borrow().layout_for_math_parent(
                    layout_context,
                    &mut measurement_positioning,
                    containing_block,
                    &self.base.style,
                    Some(MathStretchTarget::Block {
                        ascent: Au::zero(),
                        descent: Au::zero(),
                    }),
                );
                let ink = (
                    measurement.metrics.ink_ascent,
                    measurement.metrics.ink_descent,
                );
                target_extents = Some(target_extents.map_or(ink, |previous| {
                    (previous.0.max(ink.0), previous.1.max(ink.1))
                }));
            }
        }
        let (target_ascent, target_descent) = target_extents.unwrap_or((Au::zero(), Au::zero()));
        let target = Some(MathStretchTarget::Block {
            ascent: target_ascent,
            descent: target_descent,
        });
        for index in deferred {
            outputs[index] = Some(self.children[index].borrow().layout_for_math_parent(
                layout_context,
                positioning_context,
                containing_block,
                &self.base.style,
                target,
            ));
        }

        let mut extents: Option<(Au, Au, Au, Au)> = None;
        for (child, output) in self.children.iter().zip(&outputs) {
            if !child.borrow().is_in_flow() {
                continue;
            }
            let metrics = &output.as_ref().unwrap().metrics;
            let child = (
                metrics.ascent(),
                metrics.descent(),
                metrics.ink_ascent,
                metrics.ink_descent,
            );
            extents = Some(extents.map_or(child, |previous| {
                (
                    previous.0.max(child.0),
                    previous.1.max(child.1),
                    previous.2.max(child.2),
                    previous.3.max(child.3),
                )
            }));
        }
        let (ascent, descent, ink_ascent, ink_descent) =
            extents.unwrap_or((Au::zero(), Au::zero(), Au::zero(), Au::zero()));
        let mut fragments = Vec::new();
        let mut inline_size = Au::zero();
        for (child, output) in self.children.iter().zip(outputs) {
            let mut output = output.unwrap();
            if child.borrow().is_in_flow() {
                translate_fragments(
                    &mut output.fragments,
                    inline_size,
                    ascent - output.metrics.ascent(),
                );
                inline_size += output.metrics.size.inline;
            }
            fragments.extend(output.fragments);
        }
        let mut metrics = MathBoxMetrics::new(inline_size, ascent + descent, ascent);
        metrics.ink_ascent = ink_ascent;
        metrics.ink_descent = ink_descent;
        MathLayoutResult { fragments, metrics }
    }
}
